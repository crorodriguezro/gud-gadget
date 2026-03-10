use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;
use tracing::{debug, warn};

use bytes::BytesMut;
use usb_gadget::function::custom;
use usb_gadget::function::custom::{
    CtrlReceiver, CtrlSender, Endpoint, EndpointDirection, EndpointReceiver,
};
use usb_gadget::Id;

const GUD_DISPLAY_MAGIC: u32 = 0x1d50614d;

const GUD_REQ_GET_STATUS: u8 = 0x00;
const GUD_REQ_GET_DESCRIPTOR: u8 = 0x01;
const GUD_REQ_GET_FORMATS: u8 = 0x40;
const GUD_REQ_GET_PROPERTIES: u8 = 0x41;
const GUD_REQ_GET_CONNECTORS: u8 = 0x50;
const GUD_REQ_GET_CONNECTOR_PROPERTIES: u8 = 0x51;
const GUD_REQ_GET_CONNECTOR_STATUS: u8 = 0x54;
const GUD_REQ_GET_CONNECTOR_MODES: u8 = 0x55;
const GUD_REQ_GET_CONNECTOR_EDID: u8 = 0x56;

const GUD_REQ_SET_CONNECTOR_FORCE_DETECT: u8 = 0x53;
const GUD_REQ_SET_BUFFER: u8 = 0x60;
const GUD_REQ_SET_STATE_CHECK: u8 = 0x61;
const GUD_REQ_SET_STATE_COMMIT: u8 = 0x62;
const GUD_REQ_SET_CONTROLLER_ENABLE: u8 = 0x63;
const GUD_REQ_SET_DISPLAY_ENABLE: u8 = 0x64;

const GUD_DISPLAY_FLAG_STATUS_ON_SET: u32 = 0x01;
const GUD_CONNECTOR_STATUS_CONNECTED: u8 = 0x01;
const GUD_CONNECTOR_STATUS_CHANGED: u8 = 0x80;
pub const GUD_DISPLAY_MODE_FLAG_PREFERRED: u32 = 1 << 10;

pub const GUD_PIXEL_FORMAT_RGB565: u8 = 0x40;
pub const GUD_PIXEL_FORMAT_RGB888: u8 = 0x50;
pub const GUD_PIXEL_FORMAT_XRGB8888: u8 = 0x80;

const GUD_CONNECTOR_TYPE_PANEL: u8 = 0;

const GUD_STATUS_OK: u8 = 0;
const GUD_STATUS_REQUEST_NOT_SUPPORTED: u8 = 0x02;
const GUD_STATUS_INVALID_PARAMETER: u8 = 0x04;

const GUD_COMPRESSION_LZ4: u8 = 0x01;

const GUD_SET_STATE_HEADER_LENGTH: usize = 26;
const GUD_SET_BUFFER_LENGTH: usize = 25;

// https://github.com/openmoko/openmoko-usb-oui/commit/73bdf541b6f9840b70219626b4088d4e3f164904
pub const OPENMOKO_GUD_ID: Id = Id::new(0x1d50, 0x614d);

#[derive(Serialize)]
struct ConnectorDescriptor {
    connector_type: u8,
    flags: u32,
}

pub struct PixelDataEndpoint {
    ep_rx: EndpointReceiver,
    // A collection of the small buffers we've allocated for submission to AIO to read from the endpoint.
    ep_buf: Vec<BytesMut>,
    // The full contents of a transmitted buffer are copied here.
    buf: BytesMut,
    // If compression is enabled, the received buffer is decompressed here.
    compress_buf: BytesMut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DisplayMode {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub flags: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct SetStateHeader {
    mode: DisplayMode,
    format: u8,
    connector: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedState {
    pub mode: DisplayMode,
    pub format: u8,
    pub connector: u8,
}

impl From<SetStateHeader> for CheckedState {
    fn from(value: SetStateHeader) -> Self {
        Self {
            mode: value.mode,
            format: value.format,
            connector: value.connector,
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct SetBuffer {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub length: u32,
    pub compression: u8,
    pub compressed_length: u32,
}

#[derive(Debug)]
pub enum Event<'a> {
    GetDescriptor(GetDescriptor<'a>),
    GetDisplayModes(GetDisplayModes<'a>),
    GetPixelFormats(GetPixelFormats<'a>),
    Buffer(SetBuffer),
}

#[derive(Debug)]
pub struct GetDescriptor<'a> {
    sender: CtrlSender<'a>,
    status: SharedStatus,
}

#[derive(Debug)]
pub struct GetDisplayModes<'a> {
    sender: CtrlSender<'a>,
    status: SharedStatus,
}

#[derive(Debug)]
pub struct GetPixelFormats<'a> {
    sender: CtrlSender<'a>,
    status: SharedStatus,
}

type SharedStatus = Rc<RefCell<StatusTracker>>;

#[derive(Debug, Default)]
struct StatusTracker {
    value: u8,
    clear_on_next_success: bool,
}

impl StatusTracker {
    fn current(&self) -> u8 {
        self.value
    }

    fn latch(&mut self, status: u8) {
        self.value = status;
        self.clear_on_next_success = status != GUD_STATUS_OK;
    }

    fn mark_success(&mut self) {
        if self.clear_on_next_success {
            self.value = GUD_STATUS_OK;
            self.clear_on_next_success = false;
        }
    }
}

#[derive(Debug)]
pub struct ProtocolHandler {
    status: SharedStatus,
    supported_modes: Vec<DisplayMode>,
    supported_formats: Vec<u8>,
    pending_state: Option<CheckedState>,
    committed_state: Option<CheckedState>,
    controller_enabled: bool,
    display_enabled: bool,
    connector_status_changed: bool,
}

impl Default for ProtocolHandler {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new())
    }
}

impl<'a> GetDescriptor<'a> {
    pub fn send_descriptor(
        self,
        min_width: u32,
        min_height: u32,
        max_width: u32,
        max_height: u32,
    ) -> anyhow::Result<()> {
        let descriptor = DisplayDescriptor {
            magic: GUD_DISPLAY_MAGIC,
            version: 1,
            flags: GUD_DISPLAY_FLAG_STATUS_ON_SET,
            compression: GUD_COMPRESSION_LZ4,
            max_height,
            max_width,
            min_height,
            min_width,
            max_buffer_size: max_height * max_width * 4,
        };

        let mut buf: [u8; 30] = [0; 30];
        ssmarshal::serialize(&mut buf, &descriptor).context("serialize display descriptor")?;

        self.sender.send(&buf).context("send display descriptor")?;
        self.status.borrow_mut().mark_success();
        debug!("sent display descriptor {:?}", descriptor);
        Ok(())
    }
}

impl<'a> GetDisplayModes<'a> {
    pub fn send_modes(self, modes: &[DisplayMode]) -> anyhow::Result<()> {
        let size = 24 * modes.len();
        if size > self.sender.len() {
            // TODO: proper Err
            panic!("too many display modes provided");
        }

        let mut buf = vec![0; size];
        let mut pos = 0;
        for mode in modes {
            pos = pos + ssmarshal::serialize(&mut buf[pos..], mode).context("serialize mode")?;
        }

        self.sender.send(&buf).context("send modes")?;
        self.status.borrow_mut().mark_success();

        Ok(())
    }
}

impl<'a> GetPixelFormats<'a> {
    pub fn send_pixel_formats(self, formats: &[u8]) -> anyhow::Result<()> {
        self.sender.send(formats).context("send pixel formats")?;
        self.status.borrow_mut().mark_success();
        debug!("sent pixel formats: {:?}", formats);
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct DisplayDescriptor {
    magic: u32,
    version: u8,
    flags: u32,
    compression: u8,
    max_buffer_size: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

impl ProtocolHandler {
    pub fn new(supported_modes: Vec<DisplayMode>, supported_formats: Vec<u8>) -> Self {
        Self {
            status: Rc::new(RefCell::new(StatusTracker::default())),
            supported_modes,
            supported_formats,
            pending_state: None,
            committed_state: None,
            controller_enabled: false,
            display_enabled: false,
            connector_status_changed: true,
        }
    }

    pub fn can_scanout(&self) -> bool {
        self.controller_enabled && self.display_enabled && self.committed_state.is_some()
    }

    pub fn controller_enabled(&self) -> bool {
        self.controller_enabled
    }

    pub fn display_enabled(&self) -> bool {
        self.display_enabled
    }

    pub fn committed_state(&self) -> Option<&CheckedState> {
        self.committed_state.as_ref()
    }

    pub fn event<'a>(&mut self, event: custom::Event<'a>) -> anyhow::Result<Option<Event<'a>>> {
        match event {
            custom::Event::Enable => {
                self.reset_runtime_state();
                debug!("Enable event received");
            }
            custom::Event::Bind => {
                self.reset_runtime_state();
                debug!("Bind event received");
            }
            custom::Event::SetupDeviceToHost(req) => {
                let ctrl_req = req.ctrl_req();
                match ctrl_req.request {
                    GUD_REQ_GET_STATUS => {
                        let status = self.status.borrow().current();
                        req.send(&[status]).context("send status")?;
                        debug!("sent status {}", status);
                    }
                    GUD_REQ_GET_DESCRIPTOR => {
                        return Ok(Some(Event::GetDescriptor(GetDescriptor {
                            sender: req,
                            status: Rc::clone(&self.status),
                        })));
                    }
                    GUD_REQ_GET_FORMATS => {
                        return Ok(Some(Event::GetPixelFormats(GetPixelFormats {
                            sender: req,
                            status: Rc::clone(&self.status),
                        })));
                    }
                    GUD_REQ_GET_PROPERTIES => {
                        let sent = req.send(&[]).context("send properties")?;
                        self.mark_success();
                        debug!("sent properties {}", sent);
                    }
                    GUD_REQ_GET_CONNECTORS => {
                        let connectors = [ConnectorDescriptor {
                            connector_type: GUD_CONNECTOR_TYPE_PANEL,
                            flags: 0,
                        }];

                        let mut buf: [u8; 5] = [0; 5];
                        ssmarshal::serialize(&mut buf, &connectors)
                            .context("serialize connectors")?;
                        req.send(&buf).context("send connectors")?;
                        self.mark_success();
                        debug!("sent connectors");
                    }
                    GUD_REQ_GET_CONNECTOR_PROPERTIES => {
                        if ctrl_req.value != 0 {
                            return self.reject_sender(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject get connector properties",
                            );
                        }
                        req.send(&[]).context("send connector properties")?;
                        self.mark_success();
                        debug!("sent connector properties");
                    }
                    GUD_REQ_GET_CONNECTOR_MODES => {
                        if ctrl_req.value != 0 {
                            return self.reject_sender(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject get connector modes",
                            );
                        }
                        return Ok(Some(Event::GetDisplayModes(GetDisplayModes {
                            sender: req,
                            status: Rc::clone(&self.status),
                        })));
                    }
                    GUD_REQ_GET_CONNECTOR_EDID => {
                        if ctrl_req.value != 0 {
                            return self.reject_sender(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject get connector EDID",
                            );
                        }
                        req.send(&[]).context("send EDID")?;
                        self.mark_success();
                        debug!("sent empty EDID (no EDID available)");
                    }
                    GUD_REQ_GET_CONNECTOR_STATUS => {
                        if ctrl_req.value != 0 {
                            return self.reject_sender(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject get connector status",
                            );
                        }
                        let status = if self.connector_status_changed {
                            GUD_CONNECTOR_STATUS_CONNECTED | GUD_CONNECTOR_STATUS_CHANGED
                        } else {
                            GUD_CONNECTOR_STATUS_CONNECTED
                        };
                        req.send(&[status])
                            .context("send connector status")?;
                        self.connector_status_changed = false;
                        self.mark_success();
                        debug!("sent connector status 0x{:02x}", status);
                    }
                    request => {
                        warn!("unsupported SetupDeviceToHost request {:x}", request);
                        return self.reject_sender(
                            req,
                            GUD_STATUS_REQUEST_NOT_SUPPORTED,
                            "reject unsupported device-to-host request",
                        );
                    }
                }
            }
            custom::Event::SetupHostToDevice(req) => {
                let ctrl_req = req.ctrl_req();
                match ctrl_req.request {
                    GUD_REQ_SET_CONNECTOR_FORCE_DETECT => {
                        if ctrl_req.value != 0 || ctrl_req.length != 0 {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set connector force detect",
                            );
                        }
                        debug!("connector force detect for connector {}", ctrl_req.value);
                        req.recv_all().context("recv set connector")?;
                        self.connector_status_changed = true;
                        self.mark_success();
                    }
                    GUD_REQ_SET_STATE_CHECK => {
                        if ctrl_req.length as usize != GUD_SET_STATE_HEADER_LENGTH {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set state check",
                            );
                        }
                        let req = req.recv_all().context("recv set state check")?;
                        match self.validate_set_state_check(req.as_slice()) {
                            Ok(state) => {
                                debug!("received valid state check: {:?}", state);
                                self.pending_state = Some(state);
                                self.mark_success();
                            }
                            Err(err) => {
                                warn!("rejecting state check: {}", err);
                                self.latch_status(GUD_STATUS_INVALID_PARAMETER);
                                return Ok(None);
                            }
                        }
                    }
                    GUD_REQ_SET_CONTROLLER_ENABLE => {
                        if ctrl_req.length != 1 {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set controller enable",
                            );
                        }
                        let req = req.recv_all().context("recv set controller enable")?;
                        if !matches!(req.first(), Some(0 | 1)) {
                            warn!("rejecting controller enable payload: {:?}", req);
                            self.latch_status(GUD_STATUS_INVALID_PARAMETER);
                            return Ok(None);
                        }
                        self.controller_enabled = req[0] != 0;
                        if !self.controller_enabled {
                            self.display_enabled = false;
                        }
                        self.mark_success();
                        debug!("received controller enable: {}", self.controller_enabled);
                    }
                    GUD_REQ_SET_DISPLAY_ENABLE => {
                        if ctrl_req.length != 1 {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set display enable",
                            );
                        }
                        let req = req.recv_all().context("recv set display enable")?;
                        if !matches!(req.first(), Some(0 | 1)) {
                            warn!("rejecting display enable payload: {:?}", req);
                            self.latch_status(GUD_STATUS_INVALID_PARAMETER);
                            return Ok(None);
                        }
                        self.display_enabled = req[0] != 0;
                        self.mark_success();
                        debug!("received display enable: {}", self.display_enabled);
                    }
                    GUD_REQ_SET_STATE_COMMIT => {
                        if ctrl_req.length != 0 {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set state commit",
                            );
                        }
                        if self.pending_state.is_none() {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set state commit without pending state",
                            );
                        }
                        req.recv_all().context("recv set state commit")?;
                        self.committed_state = self.pending_state.take();
                        self.mark_success();
                        debug!("received state commit: {:?}", self.committed_state);
                    }
                    GUD_REQ_SET_BUFFER => {
                        if ctrl_req.length != GUD_SET_BUFFER_LENGTH as u16 {
                            return self.reject_receiver(
                                req,
                                GUD_STATUS_INVALID_PARAMETER,
                                "reject set buffer",
                            );
                        }
                        let req = req.recv_all().context("recv set buffer")?;
                        let v: SetBuffer;
                        (v, _) = ssmarshal::deserialize(req.as_slice())
                            .context("deserialize set buffer")?;
                        if v.compression != 0 && v.compression != GUD_COMPRESSION_LZ4 {
                            warn!("rejecting set buffer compression: {}", v.compression);
                            self.latch_status(GUD_STATUS_INVALID_PARAMETER);
                            return Ok(None);
                        }
                        if let Err(err) = self.validate_set_buffer(&v) {
                            warn!("rejecting set buffer: {}", err);
                            self.latch_status(GUD_STATUS_INVALID_PARAMETER);
                            return Ok(None);
                        }
                        self.mark_success();
                        debug!("received set buffer: {:?}", v);
                        return Ok(Some(Event::Buffer(v)));
                    }
                    request => {
                        warn!("unsupported set request {:x}", request);
                        return self.reject_receiver(
                            req,
                            GUD_STATUS_REQUEST_NOT_SUPPORTED,
                            "reject unsupported host-to-device request",
                        );
                    }
                }
            }
            custom::Event::Suspend => {
                self.display_enabled = false;
                debug!("Suspend event received");
            }
            custom::Event::Resume => {
                debug!("Resume event received");
            }
            custom::Event::Disable => {
                self.reset_runtime_state();
                debug!("Disable event received");
            }
            event => {
                warn!("unhandled event {:?}", event);
            }
        }
        Ok(None)
    }

    fn mark_success(&mut self) {
        self.status.borrow_mut().mark_success();
    }

    fn latch_status(&mut self, status: u8) {
        self.status.borrow_mut().latch(status);
    }

    fn reset_runtime_state(&mut self) {
        self.pending_state = None;
        self.committed_state = None;
        self.controller_enabled = false;
        self.display_enabled = false;
        self.connector_status_changed = true;
    }

    fn reject_sender<'a>(
        &mut self,
        req: CtrlSender<'a>,
        status: u8,
        context: &str,
    ) -> anyhow::Result<Option<Event<'a>>> {
        self.latch_status(status);
        req.halt().with_context(|| context.to_owned())?;
        Ok(None)
    }

    fn reject_receiver<'a>(
        &mut self,
        req: CtrlReceiver<'a>,
        status: u8,
        context: &str,
    ) -> anyhow::Result<Option<Event<'a>>> {
        self.latch_status(status);
        req.halt().with_context(|| context.to_owned())?;
        Ok(None)
    }

    fn validate_set_state_check(&self, data: &[u8]) -> anyhow::Result<CheckedState> {
        if data.len() != GUD_SET_STATE_HEADER_LENGTH {
            anyhow::bail!("unexpected state check length: {}", data.len());
        }

        let (state, used): (SetStateHeader, usize) =
            ssmarshal::deserialize(data).context("deserialize set state header")?;
        if used != GUD_SET_STATE_HEADER_LENGTH {
            anyhow::bail!("unexpected state check header length: {}", used);
        }
        if state.connector != 0 {
            anyhow::bail!("unsupported connector index {}", state.connector);
        }
        if !self.supported_formats.contains(&state.format) {
            anyhow::bail!("unsupported pixel format 0x{:02x}", state.format);
        }
        if state.mode.hdisplay == 0 || state.mode.vdisplay == 0 {
            anyhow::bail!("display mode must have non-zero dimensions");
        }
        if !self.supported_modes.contains(&state.mode) {
            anyhow::bail!("unsupported display mode {:?}", state.mode);
        }

        Ok(state.into())
    }

    fn validate_set_buffer(&self, info: &SetBuffer) -> anyhow::Result<()> {
        let state = self
            .committed_state
            .as_ref()
            .context("no committed state for buffer transfer")?;
        let bytes_per_pixel = bytes_per_pixel(state.format)
            .with_context(|| format!("unsupported format 0x{:02x}", state.format))?;
        let max_width = u32::from(state.mode.hdisplay);
        let max_height = u32::from(state.mode.vdisplay);

        if info.width == 0 || info.height == 0 {
            anyhow::bail!("buffer dimensions must be non-zero");
        }
        if info.x.checked_add(info.width).is_none() || info.x + info.width > max_width {
            anyhow::bail!("buffer width out of bounds");
        }
        if info.y.checked_add(info.height).is_none() || info.y + info.height > max_height {
            anyhow::bail!("buffer height out of bounds");
        }

        let expected_length = info
            .width
            .checked_mul(info.height)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel as u32))
            .context("buffer length overflow")?;
        if info.length != expected_length {
            anyhow::bail!(
                "unexpected buffer length {} (expected {})",
                info.length,
                expected_length
            );
        }
        if info.compression == 0 && info.compressed_length != 0 {
            anyhow::bail!("compressed_length must be zero for uncompressed buffers");
        }
        if info.compression == GUD_COMPRESSION_LZ4 && info.compressed_length == 0 {
            anyhow::bail!("compressed buffer length must be non-zero");
        }

        Ok(())
    }
}

fn bytes_per_pixel(format: u8) -> Option<usize> {
    match format {
        GUD_PIXEL_FORMAT_RGB565 => Some(2),
        GUD_PIXEL_FORMAT_RGB888 => Some(3),
        GUD_PIXEL_FORMAT_XRGB8888 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bytes_per_pixel, CheckedState, DisplayMode, ProtocolHandler, SetBuffer, SetStateHeader,
        StatusTracker, GUD_PIXEL_FORMAT_RGB565, GUD_STATUS_OK, GUD_STATUS_REQUEST_NOT_SUPPORTED,
    };

    fn sample_mode() -> DisplayMode {
        DisplayMode {
            clock: 65_000,
            hdisplay: 1024,
            hsync_start: 1040,
            hsync_end: 1184,
            htotal: 1344,
            vdisplay: 768,
            vsync_start: 771,
            vsync_end: 777,
            vtotal: 806,
            flags: 0,
        }
    }

    fn sample_state() -> CheckedState {
        CheckedState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        }
    }

    fn serialize_state_header(header: &SetStateHeader) -> Vec<u8> {
        let mut buf = vec![0; super::GUD_SET_STATE_HEADER_LENGTH];
        let used = ssmarshal::serialize(&mut buf, header).expect("serialize test state header");
        assert_eq!(used, super::GUD_SET_STATE_HEADER_LENGTH);
        buf
    }

    #[test]
    fn error_status_stays_latched_until_success() {
        let mut status = StatusTracker::default();

        status.latch(GUD_STATUS_REQUEST_NOT_SUPPORTED);
        assert_eq!(status.current(), GUD_STATUS_REQUEST_NOT_SUPPORTED);

        status.mark_success();
        assert_eq!(status.current(), GUD_STATUS_OK);
    }

    #[test]
    fn success_without_error_keeps_status_ok() {
        let mut status = StatusTracker::default();

        status.mark_success();
        assert_eq!(status.current(), GUD_STATUS_OK);
    }

    #[test]
    fn validate_state_check_rejects_unknown_mode() {
        let protocol = ProtocolHandler::new(vec![sample_mode()], vec![GUD_PIXEL_FORMAT_RGB565]);
        let invalid_mode = DisplayMode {
            hdisplay: 800,
            vdisplay: 600,
            ..sample_mode()
        };
        let payload = serialize_state_header(&SetStateHeader {
            mode: invalid_mode,
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });

        let err = protocol.validate_set_state_check(&payload).unwrap_err();
        assert!(err.to_string().contains("unsupported display mode"));
    }

    #[test]
    fn validate_set_buffer_requires_committed_state_and_bounds() {
        let mut protocol = ProtocolHandler::new(vec![sample_mode()], vec![GUD_PIXEL_FORMAT_RGB565]);
        let missing_state_err = protocol
            .validate_set_buffer(&SetBuffer {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
                length: 16 * 16 * bytes_per_pixel(GUD_PIXEL_FORMAT_RGB565).unwrap() as u32,
                compression: 0,
                compressed_length: 0,
            })
            .unwrap_err();
        assert!(missing_state_err.to_string().contains("no committed state"));

        protocol.committed_state = Some(sample_state());
        let err = protocol
            .validate_set_buffer(&SetBuffer {
                x: 1000,
                y: 760,
                width: 32,
                height: 32,
                length: 32 * 32 * bytes_per_pixel(GUD_PIXEL_FORMAT_RGB565).unwrap() as u32,
                compression: 0,
                compressed_length: 0,
            })
            .unwrap_err();
        assert!(err.to_string().contains("out of bounds"));
    }
}

impl PixelDataEndpoint {
    pub fn new() -> (Self, Endpoint) {
        let (ep_rx, ep_dir) = EndpointDirection::host_to_device();

        (
            Self {
                ep_rx,
                ep_buf: Vec::new(),
                buf: BytesMut::new(),
                compress_buf: BytesMut::new(),
            },
            Endpoint::bulk(ep_dir),
        )
    }

    pub fn recv_buffer(
        &mut self,
        info: SetBuffer,
        fb: &mut [u8],
        fb_pitch: usize,
        bpp: usize,
    ) -> anyhow::Result<()> {
        let start = Instant::now();
        let max_packet_size = self.ep_rx.max_packet_size().unwrap();
        debug!(
            "recv_buffer: max_packet_size={}, fb_pitch={}, bpp={}, fb_len={}",
            max_packet_size,
            fb_pitch,
            bpp,
            fb.len()
        );

        let len = if info.compression > 0 {
            info.compressed_length
        } else {
            info.length
        } as usize;
        self.buf.clear();

        // Ensure the buffer is large enough to fit all incoming data.
        if self.buf.capacity() < len {
            self.buf.reserve(len - self.buf.capacity());
        }

        // Read the incoming data fully into the buffer.
        let read_start = Instant::now();
        let mut packets = 0usize;
        while self.buf.len() < len {
            let buf = self
                .ep_buf
                .pop()
                .unwrap_or_else(|| BytesMut::with_capacity(max_packet_size));
            let buf = self.ep_rx.recv(buf).context("read bulk ep")?;
            if buf.is_none() {
                continue;
            }
            let mut buf = buf.unwrap();
            self.buf.extend_from_slice(&buf);
            packets += 1;
            buf.clear();
            self.ep_buf.push(buf);
        }
        debug!(
            "read {} bytes in {} packets, took {}ms",
            self.buf.len(),
            packets,
            read_start.elapsed().as_millis()
        );

        if self.buf.len() != len {
            // TODO: proper Err
            panic!("expected buf len {}, got {}", len, self.buf.len());
        }

        let buf = if info.compression > 0 {
            let decompress_start = Instant::now();
            if self.compress_buf.len() < info.length as usize {
                self.compress_buf
                    .resize(info.length as usize - self.compress_buf.capacity(), 0);
            }
            let decompressed = lz4::block::decompress_to_buffer(
                &self.buf,
                Some(info.length as i32),
                &mut self.compress_buf,
            )
            .context("lz4 decompress")?;
            debug!(
                "decompressed {} -> {} bytes, took {}ms",
                self.buf.len(),
                decompressed,
                decompress_start.elapsed().as_millis()
            );
            &self.compress_buf
        } else {
            &self.buf
        };

        let copy_start = Instant::now();
        let mut y = info.y as usize;
        let end_y = (info.y + info.height) as usize;

        let line_len = info.width as usize * bpp;
        let line_start = info.x as usize * bpp;

        let mut buf_pos = 0usize;
        while y < end_y {
            let fb_start = (y * fb_pitch) + line_start;
            let fb_end = fb_start + line_len;
            fb[fb_start..fb_end].copy_from_slice(&buf[buf_pos..buf_pos + line_len]);
            buf_pos += line_len;
            y += 1;
        }
        debug!(
            "copied {} lines ({} bytes), took {}ms",
            end_y - info.y as usize,
            buf_pos,
            copy_start.elapsed().as_millis()
        );

        debug!("recv_buffer total took {}ms", start.elapsed().as_millis());

        Ok(())
    }
}

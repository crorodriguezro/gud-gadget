use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::env::var_os;
use std::fs::{rename, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tracing::{debug, warn};

use bytes::BytesMut;
use usb_gadget::function::custom;
use usb_gadget::function::custom::{CtrlSender, Endpoint, EndpointDirection, EndpointReceiver};
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

pub const GUD_COMPRESSION_LZ4: u8 = 0x01;

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

const GUD_STATE_CHECK_HEADER_LEN: usize = 26;
const GUD_PROPERTY_SIZE: usize = 10;

static CONNECTOR_STATUS_CHANGED_ONCE: AtomicBool = AtomicBool::new(true);
static STATUS_VALUE: AtomicU8 = AtomicU8::new(GUD_STATUS_OK);
static CLEAR_STATUS_ON_NEXT_SUCCESS: AtomicBool = AtomicBool::new(false);
static STATE_CHECK_VALIDATION: OnceLock<Mutex<StateCheckValidation>> = OnceLock::new();
static PROTOCOL_STATE: OnceLock<Mutex<ProtocolState>> = OnceLock::new();

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

fn dump_pixel_buffer_ppm(
    path: &Path,
    buf: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    bpp: usize,
) -> anyhow::Result<()> {
    let width = width as usize;
    let height = height as usize;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gud-rx.ppm");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    let mut writer = BufWriter::new(File::create(&tmp_path)?);
    write!(writer, "P6\n{} {}\n255\n", width, height)?;

    for y in 0..height {
        let row = &buf[(y * pitch)..(y * pitch + width * bpp)];
        for x in 0..width {
            let offset = x * bpp;
            let rgb = match bpp {
                2 => {
                    let pixel = u16::from_le_bytes([row[offset], row[offset + 1]]);
                    let red = ((pixel >> 11) & 0x1f) as u8;
                    let green = ((pixel >> 5) & 0x3f) as u8;
                    let blue = (pixel & 0x1f) as u8;
                    [
                        (red << 3) | (red >> 2),
                        (green << 2) | (green >> 4),
                        (blue << 3) | (blue >> 2),
                    ]
                }
                3 => [row[offset], row[offset + 1], row[offset + 2]],
                4 => [row[offset + 2], row[offset + 1], row[offset]],
                _ => anyhow::bail!("unsupported bytes-per-pixel for dump: {}", bpp),
            };
            writer.write_all(&rgb)?;
        }
    }

    writer.flush()?;
    drop(writer);
    rename(&tmp_path, path)?;
    Ok(())
}

fn dump_raw_buffer(path: &Path, buf: &[u8]) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gud-rx.raw");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    std::fs::write(&tmp_path, buf)?;
    rename(&tmp_path, path)?;
    Ok(())
}

fn dump_pixel_buffer_if_enabled(
    var_name: &str,
    buf: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    bpp: usize,
) {
    let Some(path) = var_os(var_name).map(PathBuf::from) else {
        return;
    };

    if let Err(err) = dump_pixel_buffer_ppm(&path, buf, pitch, width, height, bpp) {
        warn!("Failed to dump pixel data to {}: {}", path.display(), err);
    } else {
        debug!("Wrote pixel dump to {}", path.display());
    }
}

fn dump_raw_buffer_if_enabled(var_name: &str, buf: &[u8]) {
    let Some(path) = var_os(var_name).map(PathBuf::from) else {
        return;
    };

    if let Err(err) = dump_raw_buffer(&path, buf) {
        warn!(
            "Failed to dump raw pixel data to {}: {}",
            path.display(),
            err
        );
    } else {
        debug!("Wrote raw pixel dump to {}", path.display());
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Deserialize)]
struct StateCheckHeader {
    mode: DisplayMode,
    format: u8,
    connector: u8,
}

#[derive(Clone, Debug, Default)]
struct StateCheckValidation {
    connector_count: u8,
    formats: Vec<u8>,
    modes: Vec<DisplayMode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayState {
    mode: DisplayMode,
    format: u8,
    connector: u8,
}

#[derive(Debug, Default)]
struct ProtocolState {
    pending_state: Option<DisplayState>,
    committed_state: Option<DisplayState>,
    controller_enabled: bool,
    display_enabled: bool,
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
}

#[derive(Debug)]
pub struct GetDisplayModes<'a> {
    sender: CtrlSender<'a>,
}

#[derive(Debug)]
pub struct GetPixelFormats<'a> {
    sender: CtrlSender<'a>,
}

impl<'a> GetDescriptor<'a> {
    pub fn send_descriptor(
        self,
        min_width: u32,
        min_height: u32,
        max_width: u32,
        max_height: u32,
        compression: u8,
    ) -> anyhow::Result<()> {
        let descriptor =
            build_display_descriptor(min_width, min_height, max_width, max_height, compression);
        let buf = serialize_display_descriptor(&descriptor)?;

        self.sender.send(&buf).context("send display descriptor")?;
        mark_success();
        debug!("sent display descriptor {:?}", descriptor);
        Ok(())
    }
}

impl<'a> GetDisplayModes<'a> {
    pub fn send_modes(self, modes: &[DisplayMode]) -> anyhow::Result<()> {
        let buf = serialize_display_modes(modes)?;
        if buf.len() > self.sender.len() {
            panic!("too many display modes provided");
        }

        self.sender.send(&buf).context("send modes")?;
        mark_success();

        Ok(())
    }
}

impl<'a> GetPixelFormats<'a> {
    pub fn send_pixel_formats(self, formats: &[u8]) -> anyhow::Result<()> {
        self.sender.send(formats).context("send pixel formats")?;
        mark_success();
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

fn build_display_descriptor(
    min_width: u32,
    min_height: u32,
    max_width: u32,
    max_height: u32,
    compression: u8,
) -> DisplayDescriptor {
    DisplayDescriptor {
        magic: GUD_DISPLAY_MAGIC,
        version: 1,
        flags: 0,
        compression,
        max_height,
        max_width,
        min_height,
        min_width,
        max_buffer_size: max_height * max_width * 4,
    }
}

fn serialize_display_descriptor(descriptor: &DisplayDescriptor) -> anyhow::Result<[u8; 30]> {
    let mut buf = [0_u8; 30];
    ssmarshal::serialize(&mut buf, descriptor).context("serialize display descriptor")?;
    Ok(buf)
}

fn serialize_connector_descriptors(connectors: &[ConnectorDescriptor]) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0_u8; 5 * connectors.len()];
    let mut pos = 0;
    for connector in connectors {
        pos += ssmarshal::serialize(&mut buf[pos..], connector).context("serialize connector")?;
    }
    Ok(buf)
}

fn serialize_display_modes(modes: &[DisplayMode]) -> anyhow::Result<Vec<u8>> {
    let mut buf = vec![0_u8; 24 * modes.len()];
    let mut pos = 0;
    for mode in modes {
        pos += ssmarshal::serialize(&mut buf[pos..], mode).context("serialize mode")?;
    }
    Ok(buf)
}

fn reset_connector_status_changed() {
    CONNECTOR_STATUS_CHANGED_ONCE.store(true, Ordering::SeqCst);
}

fn state_check_validation() -> &'static Mutex<StateCheckValidation> {
    STATE_CHECK_VALIDATION.get_or_init(|| Mutex::new(StateCheckValidation::default()))
}

fn protocol_state() -> &'static Mutex<ProtocolState> {
    PROTOCOL_STATE.get_or_init(|| Mutex::new(ProtocolState::default()))
}

fn current_status() -> u8 {
    STATUS_VALUE.load(Ordering::SeqCst)
}

fn reset_status() {
    STATUS_VALUE.store(GUD_STATUS_OK, Ordering::SeqCst);
    CLEAR_STATUS_ON_NEXT_SUCCESS.store(false, Ordering::SeqCst);
}

fn reset_protocol_state() {
    let mut state = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    state.pending_state = None;
    state.committed_state = None;
    state.controller_enabled = false;
    state.display_enabled = false;
}

fn handle_suspend_transition() {
    reset_protocol_state();
    reset_status();
}

fn handle_resume_transition() {
    reset_connector_status_changed();
    reset_protocol_state();
    reset_status();
}

fn latch_status(status: u8) {
    STATUS_VALUE.store(status, Ordering::SeqCst);
    CLEAR_STATUS_ON_NEXT_SUCCESS.store(status != GUD_STATUS_OK, Ordering::SeqCst);
}

fn mark_success() {
    if CLEAR_STATUS_ON_NEXT_SUCCESS.swap(false, Ordering::SeqCst) {
        STATUS_VALUE.store(GUD_STATUS_OK, Ordering::SeqCst);
    }
}

fn next_connector_status() -> u8 {
    let mut status = GUD_CONNECTOR_STATUS_CONNECTED;
    if CONNECTOR_STATUS_CHANGED_ONCE.swap(false, Ordering::SeqCst) {
        status |= GUD_CONNECTOR_STATUS_CHANGED;
    }
    status
}

pub fn configure_state_check_validation(
    connector_count: u8,
    formats: &[u8],
    modes: &[DisplayMode],
) {
    let mut validation = state_check_validation()
        .lock()
        .expect("state check validation lock poisoned");
    validation.connector_count = connector_count;
    validation.formats = formats.to_vec();
    validation.modes = modes.to_vec();
}

fn bytes_per_pixel(format: u8) -> anyhow::Result<usize> {
    match format {
        GUD_PIXEL_FORMAT_RGB565 => Ok(2),
        GUD_PIXEL_FORMAT_RGB888 => Ok(3),
        GUD_PIXEL_FORMAT_XRGB8888 => Ok(4),
        _ => anyhow::bail!("unsupported pixel format {:#x}", format),
    }
}

fn validate_state_check_payload(payload: &[u8]) -> anyhow::Result<DisplayState> {
    ensure!(
        payload.len() >= GUD_STATE_CHECK_HEADER_LEN,
        "state check payload too short: got {} bytes, expected at least {}",
        payload.len(),
        GUD_STATE_CHECK_HEADER_LEN
    );
    ensure!(
        (payload.len() - GUD_STATE_CHECK_HEADER_LEN).is_multiple_of(GUD_PROPERTY_SIZE),
        "state check property data has invalid length {}",
        payload.len() - GUD_STATE_CHECK_HEADER_LEN
    );

    let (header, consumed): (StateCheckHeader, usize) =
        ssmarshal::deserialize(payload).context("deserialize state check header")?;
    ensure!(
        consumed == GUD_STATE_CHECK_HEADER_LEN,
        "unexpected state check header size {}",
        consumed
    );

    let validation = state_check_validation()
        .lock()
        .expect("state check validation lock poisoned");
    ensure!(
        validation.connector_count > 0,
        "state check validation is not configured"
    );
    ensure!(
        header.connector < validation.connector_count,
        "unsupported connector {}",
        header.connector
    );
    ensure!(
        validation.formats.contains(&header.format),
        "unsupported pixel format {:#x}",
        header.format
    );
    ensure!(
        validation.modes.contains(&header.mode),
        "unsupported mode {}x{}",
        header.mode.hdisplay,
        header.mode.vdisplay
    );

    Ok(DisplayState {
        mode: header.mode,
        format: header.format,
        connector: header.connector,
    })
}

fn store_pending_state(state: DisplayState) {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    protocol.pending_state = Some(state);
}

fn clear_pending_state() {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    protocol.pending_state = None;
}

fn update_controller_enabled(enable: bool) {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    protocol.controller_enabled = enable;
    if !enable {
        protocol.display_enabled = false;
    }
}

fn update_display_enabled(enable: bool) -> anyhow::Result<()> {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    if enable {
        ensure!(
            protocol.controller_enabled,
            "display cannot be enabled while controller is disabled"
        );
        ensure!(
            protocol.committed_state.is_some(),
            "display cannot be enabled before state commit"
        );
    }
    protocol.display_enabled = enable;
    Ok(())
}

fn parse_enable_request(payload: &[u8], request_name: &str) -> anyhow::Result<bool> {
    ensure!(
        payload.len() == 1,
        "{request_name} payload length {} is invalid",
        payload.len()
    );
    match payload[0] {
        0 => Ok(false),
        1 => Ok(true),
        value => anyhow::bail!("{request_name} value {} is invalid", value),
    }
}

fn commit_pending_state() -> anyhow::Result<()> {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    let pending = protocol
        .pending_state
        .take()
        .context("no checked state available to commit")?;
    protocol.committed_state = Some(pending);
    Ok(())
}

fn validate_buffer_request(info: &SetBuffer) -> anyhow::Result<()> {
    let protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    let state = protocol
        .committed_state
        .as_ref()
        .context("no committed state available for buffer upload")?;
    ensure!(protocol.controller_enabled, "controller is disabled");
    ensure!(protocol.display_enabled, "display is disabled");
    let bpp = bytes_per_pixel(state.format)?;
    let width = info.width as usize;
    let height = info.height as usize;

    ensure!(width > 0, "buffer width must be greater than zero");
    ensure!(height > 0, "buffer height must be greater than zero");
    ensure!(
        info.x <= state.mode.hdisplay as u32,
        "buffer x {} exceeds mode width {}",
        info.x,
        state.mode.hdisplay
    );
    ensure!(
        info.y <= state.mode.vdisplay as u32,
        "buffer y {} exceeds mode height {}",
        info.y,
        state.mode.vdisplay
    );
    ensure!(
        info.x + info.width <= state.mode.hdisplay as u32,
        "buffer rect {}x{} at x {} exceeds mode width {}",
        info.width,
        info.height,
        info.x,
        state.mode.hdisplay
    );
    ensure!(
        info.y + info.height <= state.mode.vdisplay as u32,
        "buffer rect {}x{} at y {} exceeds mode height {}",
        info.width,
        info.height,
        info.y,
        state.mode.vdisplay
    );

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bpp))
        .context("buffer dimensions overflowed expected payload length")?;
    ensure!(
        info.length as usize == expected_len,
        "buffer length {} does not match expected {}",
        info.length,
        expected_len
    );
    if info.compression == 0 {
        ensure!(
            info.compressed_length == 0 || info.compressed_length == info.length,
            "uncompressed buffer has unexpected compressed_length {}",
            info.compressed_length
        );
    } else {
        ensure!(
            info.compression == GUD_COMPRESSION_LZ4,
            "unsupported compression {}",
            info.compression
        );
        ensure!(
            info.compressed_length > 0,
            "compressed buffer must set compressed_length"
        );
        ensure!(
            info.compressed_length <= info.length,
            "compressed buffer length {} exceeds uncompressed length {}",
            info.compressed_length,
            info.length
        );
    }

    Ok(())
}

pub fn event(event: custom::Event) -> anyhow::Result<Option<Event>> {
    match event {
        custom::Event::Enable => {
            reset_connector_status_changed();
            reset_status();
            reset_protocol_state();
            debug!("Enable event received");
        }
        custom::Event::Bind => {
            reset_connector_status_changed();
            reset_status();
            reset_protocol_state();
            debug!("Bind event received");
        }
        custom::Event::SetupDeviceToHost(req) => {
            let ctrl_req = req.ctrl_req();
            match ctrl_req.request {
                GUD_REQ_GET_STATUS => {
                    let status = current_status();
                    req.send(&[status]).context("send status")?;
                    debug!("sent status {}", status);
                }
                GUD_REQ_GET_DESCRIPTOR => {
                    return Ok(Some(Event::GetDescriptor(GetDescriptor { sender: req })));
                }
                GUD_REQ_GET_FORMATS => {
                    return Ok(Some(Event::GetPixelFormats(GetPixelFormats {
                        sender: req,
                    })));
                }
                GUD_REQ_GET_PROPERTIES => {
                    let sent = req.send(&[]).context("send properties")?;
                    mark_success();
                    debug!("sent properties {}", sent);
                }
                GUD_REQ_GET_CONNECTORS => {
                    let connectors = [ConnectorDescriptor {
                        connector_type: GUD_CONNECTOR_TYPE_PANEL,
                        flags: 0,
                    }];
                    let buf = serialize_connector_descriptors(&connectors)?;
                    req.send(&buf).context("send connectors")?;
                    mark_success();
                    debug!("sent connectors");
                }
                GUD_REQ_GET_CONNECTOR_PROPERTIES => {
                    req.send(&[]).context("send connector properties")?;
                    mark_success();
                    debug!("sent connector properties");
                }
                GUD_REQ_GET_CONNECTOR_MODES => {
                    return Ok(Some(Event::GetDisplayModes(GetDisplayModes {
                        sender: req,
                    })));
                }
                GUD_REQ_GET_CONNECTOR_EDID => {
                    req.send(&[]).context("send EDID")?;
                    mark_success();
                    debug!("sent empty EDID (no EDID available)");
                }
                GUD_REQ_GET_CONNECTOR_STATUS => {
                    let status = next_connector_status();
                    req.send(&[status]).context("send connector status")?;
                    mark_success();
                    debug!("sent connector status {:#x}", status);
                }
                request => {
                    latch_status(GUD_STATUS_REQUEST_NOT_SUPPORTED);
                    warn!("unhandled SetupDeviceToHost request {:x}", request);
                }
            }
        }
        custom::Event::SetupHostToDevice(req) => {
            let ctrl_req = req.ctrl_req();
            match ctrl_req.request {
                GUD_REQ_SET_CONNECTOR_FORCE_DETECT => {
                    debug!("connector set to {}", ctrl_req.value);
                    req.recv_all().context("recv set connector")?;
                    mark_success();
                }
                GUD_REQ_SET_STATE_CHECK => {
                    let payload = req.recv_all().context("recv set state check")?;
                    match validate_state_check_payload(payload.as_slice()) {
                        Ok(state) => {
                            store_pending_state(state);
                            debug!("received valid state check");
                            mark_success();
                        }
                        Err(err) => {
                            clear_pending_state();
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected state check: {}", err);
                        }
                    }
                }
                GUD_REQ_SET_CONTROLLER_ENABLE => {
                    let payload = req.recv_all().context("recv set controller enable")?;
                    match parse_enable_request(payload.as_slice(), "controller enable") {
                        Ok(enable) => {
                            update_controller_enabled(enable);
                            debug!("received controller enable: {}", enable);
                            mark_success();
                        }
                        Err(err) => {
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected controller enable: {}", err);
                        }
                    }
                }
                GUD_REQ_SET_DISPLAY_ENABLE => {
                    let payload = req.recv_all().context("recv set display enable")?;
                    match parse_enable_request(payload.as_slice(), "display enable") {
                        Ok(enable) => match update_display_enabled(enable) {
                            Ok(()) => {
                                debug!("received display enable: {}", enable);
                                mark_success();
                            }
                            Err(err) => {
                                latch_status(GUD_STATUS_INVALID_PARAMETER);
                                warn!("rejected display enable: {}", err);
                            }
                        },
                        Err(err) => {
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected display enable payload: {}", err);
                        }
                    }
                }
                GUD_REQ_SET_STATE_COMMIT => {
                    req.recv_all().context("recv set state commit")?;
                    match commit_pending_state() {
                        Ok(()) => {
                            debug!("committed checked state");
                            mark_success();
                        }
                        Err(err) => {
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected state commit: {}", err);
                        }
                    }
                }
                GUD_REQ_SET_BUFFER => {
                    let req = req.recv_all().context("recv set buffer")?;
                    let v: SetBuffer;
                    (v, _) =
                        ssmarshal::deserialize(req.as_slice()).context("deserialize set buffer")?;
                    match validate_buffer_request(&v) {
                        Ok(()) => {
                            debug!("validated set buffer: {:?}", v);
                            mark_success();
                            return Ok(Some(Event::Buffer(v)));
                        }
                        Err(err) => {
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected set buffer {:?}: {}", v, err);
                        }
                    }
                }
                value => {
                    latch_status(GUD_STATUS_REQUEST_NOT_SUPPORTED);
                    warn!("unhandled set request {:x}", value);
                }
            }
        }
        custom::Event::Suspend => {
            handle_suspend_transition();
            debug!("Suspend event received");
        }
        custom::Event::Resume => {
            handle_resume_transition();
            debug!("Resume event received");
        }
        custom::Event::Disable => {
            handle_suspend_transition();
            debug!("Disable event received");
        }
        other_event => {
            warn!("unhandled event {:?}", other_event);
        }
    }
    Ok(None)
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

    pub fn recv_payload(&mut self, info: &SetBuffer, bpp: usize) -> anyhow::Result<&[u8]> {
        let start = Instant::now();
        let max_packet_size = self.ep_rx.max_packet_size().unwrap();
        debug!(
            "recv_payload: max_packet_size={}, bpp={}",
            max_packet_size, bpp
        );

        let len = if info.compression > 0 {
            info.compressed_length
        } else {
            info.length
        } as usize;
        self.buf.clear();

        if self.buf.capacity() < len {
            self.buf.reserve(len - self.buf.capacity());
        }

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

        if self.buf.len() < len {
            panic!("expected buf len at least {}, got {}", len, self.buf.len());
        }
        if self.buf.len() > len {
            warn!(
                "bulk read overshot expected payload length: got {} bytes, expected {}. Truncating trailing {} bytes",
                self.buf.len(),
                len,
                self.buf.len() - len
            );
            self.buf.truncate(len);
        }

        let buf = if info.compression > 0 {
            let decompress_start = Instant::now();
            if self.compress_buf.len() < info.length as usize {
                self.compress_buf.resize(info.length as usize, 0);
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

        dump_pixel_buffer_if_enabled(
            "GUD_DUMP_RX_PATH",
            buf,
            info.width as usize * bpp,
            info.width,
            info.height,
            bpp,
        );
        dump_raw_buffer_if_enabled("GUD_DUMP_RX_RAW_PATH", buf);
        debug!("recv_payload total took {}ms", start.elapsed().as_millis());

        Ok(buf)
    }

    pub fn copy_buffer_to_framebuffer(
        info: &SetBuffer,
        buf: &[u8],
        fb: &mut [u8],
        fb_pitch: usize,
        bpp: usize,
    ) -> anyhow::Result<()> {
        let copy_start = Instant::now();
        let mut y = info.y as usize;
        let end_y = (info.y + info.height) as usize;

        let line_len = info.width as usize * bpp;
        let line_start = info.x as usize * bpp;
        let expected_len = info.width as usize * info.height as usize * bpp;
        ensure!(
            buf.len() >= expected_len,
            "payload too short: got {} bytes, expected at least {}",
            buf.len(),
            expected_len
        );

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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_display_descriptor, commit_pending_state, configure_state_check_validation,
        current_status, handle_resume_transition, handle_suspend_transition, latch_status,
        mark_success, next_connector_status, parse_enable_request, reset_connector_status_changed,
        reset_protocol_state, reset_status, serialize_connector_descriptors,
        serialize_display_descriptor, serialize_display_modes, store_pending_state,
        update_controller_enabled, update_display_enabled, validate_buffer_request,
        validate_state_check_payload, ConnectorDescriptor, DisplayMode, DisplayState,
        PixelDataEndpoint, SetBuffer, GUD_COMPRESSION_LZ4, GUD_CONNECTOR_STATUS_CHANGED,
        GUD_CONNECTOR_STATUS_CONNECTED, GUD_CONNECTOR_TYPE_PANEL, GUD_DISPLAY_MAGIC,
        GUD_PIXEL_FORMAT_RGB565, GUD_STATUS_OK, GUD_STATUS_REQUEST_NOT_SUPPORTED,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    struct StateCheckRequest {
        mode: DisplayMode,
        format: u8,
        connector: u8,
    }

    fn sample_mode() -> DisplayMode {
        DisplayMode {
            clock: 174_359,
            hdisplay: 1080,
            hsync_start: 1192,
            hsync_end: 1208,
            htotal: 1244,
            vdisplay: 2280,
            vsync_start: 2316,
            vsync_end: 2324,
            vtotal: 2336,
            flags: 0,
        }
    }

    fn serialize_state_check_request(mode: DisplayMode, format: u8, connector: u8) -> Vec<u8> {
        let req = StateCheckRequest {
            mode,
            format,
            connector,
        };
        let mut buf = vec![0_u8; 26];
        let written = ssmarshal::serialize(&mut buf, &req).unwrap();
        assert_eq!(written, 26);
        buf
    }

    #[test]
    fn build_display_descriptor_sets_expected_fields() {
        let descriptor = build_display_descriptor(640, 480, 1080, 2280, 0);

        assert_eq!(descriptor.magic, GUD_DISPLAY_MAGIC);
        assert_eq!(descriptor.version, 1);
        assert_eq!(descriptor.flags, 0);
        assert_eq!(descriptor.compression, 0);
        assert_eq!(descriptor.min_width, 640);
        assert_eq!(descriptor.min_height, 480);
        assert_eq!(descriptor.max_width, 1080);
        assert_eq!(descriptor.max_height, 2280);
        assert_eq!(descriptor.max_buffer_size, 1080 * 2280 * 4);
    }

    #[test]
    fn build_display_descriptor_preserves_zero_flags_with_compression_enabled() {
        let descriptor = build_display_descriptor(640, 480, 1080, 2280, GUD_COMPRESSION_LZ4);

        assert_eq!(descriptor.flags, 0);
        assert_eq!(descriptor.compression, GUD_COMPRESSION_LZ4);
    }

    #[test]
    fn connector_status_reports_changed_only_once() {
        reset_connector_status_changed();

        let first = next_connector_status();
        let second = next_connector_status();

        assert_eq!(
            first,
            GUD_CONNECTOR_STATUS_CONNECTED | GUD_CONNECTOR_STATUS_CHANGED
        );
        assert_eq!(second, GUD_CONNECTOR_STATUS_CONNECTED);
    }

    #[test]
    fn status_latches_until_next_success() {
        reset_status();

        latch_status(GUD_STATUS_REQUEST_NOT_SUPPORTED);
        assert_eq!(current_status(), GUD_STATUS_REQUEST_NOT_SUPPORTED);
        assert_eq!(current_status(), GUD_STATUS_REQUEST_NOT_SUPPORTED);

        mark_success();
        assert_eq!(current_status(), GUD_STATUS_OK);
    }

    #[test]
    fn serialize_display_descriptor_matches_expected_size_and_header() {
        let descriptor = build_display_descriptor(640, 480, 1080, 2280, 0);

        let buf = serialize_display_descriptor(&descriptor).unwrap();

        assert_eq!(buf.len(), 30);
        assert_eq!(&buf[0..4], &GUD_DISPLAY_MAGIC.to_le_bytes());
        assert_eq!(buf[4], 1);
    }

    #[test]
    fn serialize_single_connector_descriptor_is_five_bytes() {
        let connectors = [ConnectorDescriptor {
            connector_type: GUD_CONNECTOR_TYPE_PANEL,
            flags: 0,
        }];

        let buf = serialize_connector_descriptors(&connectors).unwrap();

        assert_eq!(buf.len(), 5);
        assert_eq!(buf[0], GUD_CONNECTOR_TYPE_PANEL);
        assert_eq!(&buf[1..5], &[0, 0, 0, 0]);
    }

    #[test]
    fn serialize_single_display_mode_is_twenty_four_bytes() {
        let modes = [sample_mode()];

        let buf = serialize_display_modes(&modes).unwrap();

        assert_eq!(buf.len(), 24);
        assert_eq!(&buf[0..4], &174_359_u32.to_le_bytes());
        assert_eq!(&buf[4..6], &1080_u16.to_le_bytes());
        assert_eq!(&buf[12..14], &2280_u16.to_le_bytes());
    }

    #[test]
    fn copy_buffer_to_framebuffer_copies_full_frame() {
        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            length: 8,
            compression: 0,
            compressed_length: 0,
        };
        let src = [1_u8, 2, 3, 4, 5, 6, 7, 8];
        let mut fb = [0_u8; 8];

        PixelDataEndpoint::copy_buffer_to_framebuffer(&info, &src, &mut fb, 4, 2).unwrap();

        assert_eq!(fb, src);
    }

    #[test]
    fn copy_buffer_to_framebuffer_honors_offsets_and_pitch() {
        let info = SetBuffer {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
            length: 8,
            compression: 0,
            compressed_length: 0,
        };
        let src = [10_u8, 11, 12, 13, 20, 21, 22, 23];
        let mut fb = [0xaa_u8; 24];

        PixelDataEndpoint::copy_buffer_to_framebuffer(&info, &src, &mut fb, 8, 2).unwrap();

        assert_eq!(&fb[0..8], &[0xaa; 8]);
        assert_eq!(&fb[8..16], &[0xaa, 0xaa, 10, 11, 12, 13, 0xaa, 0xaa]);
        assert_eq!(&fb[16..24], &[0xaa, 0xaa, 20, 21, 22, 23, 0xaa, 0xaa]);
    }

    #[test]
    fn copy_buffer_to_framebuffer_rejects_short_payload() {
        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
            length: 8,
            compression: 0,
            compressed_length: 0,
        };
        let src = [1_u8, 2, 3, 4, 5, 6];
        let mut fb = [0_u8; 8];

        let err =
            PixelDataEndpoint::copy_buffer_to_framebuffer(&info, &src, &mut fb, 4, 2).unwrap_err();

        assert!(
            err.to_string().contains("payload too short"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_state_check_accepts_advertised_mode_and_format() {
        let mode = sample_mode();
        configure_state_check_validation(1, &[GUD_PIXEL_FORMAT_RGB565], &[mode.clone()]);
        reset_protocol_state();

        let payload = serialize_state_check_request(mode, GUD_PIXEL_FORMAT_RGB565, 0);

        let state = validate_state_check_payload(&payload).unwrap();
        assert_eq!(
            state,
            DisplayState {
                mode: sample_mode(),
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
            }
        );
    }

    #[test]
    fn validate_state_check_rejects_unknown_format() {
        let mode = sample_mode();
        configure_state_check_validation(1, &[GUD_PIXEL_FORMAT_RGB565], &[mode.clone()]);
        reset_protocol_state();

        let payload = serialize_state_check_request(mode, 0x80, 0);
        let err = validate_state_check_payload(&payload).unwrap_err();

        assert!(err.to_string().contains("unsupported pixel format"));
    }

    #[test]
    fn validate_state_check_rejects_unknown_connector() {
        let mode = sample_mode();
        configure_state_check_validation(1, &[GUD_PIXEL_FORMAT_RGB565], &[mode.clone()]);
        reset_protocol_state();

        let payload = serialize_state_check_request(mode, GUD_PIXEL_FORMAT_RGB565, 1);
        let err = validate_state_check_payload(&payload).unwrap_err();

        assert!(err.to_string().contains("unsupported connector"));
    }

    #[test]
    fn validate_state_check_rejects_unknown_mode() {
        let mut mode = sample_mode();
        configure_state_check_validation(1, &[GUD_PIXEL_FORMAT_RGB565], &[mode.clone()]);
        reset_protocol_state();
        mode.vdisplay = 2400;

        let payload = serialize_state_check_request(mode, GUD_PIXEL_FORMAT_RGB565, 0);
        let err = validate_state_check_payload(&payload).unwrap_err();

        assert!(err.to_string().contains("unsupported mode"));
    }

    #[test]
    fn state_commit_requires_pending_state() {
        reset_protocol_state();

        let err = commit_pending_state().unwrap_err();

        assert!(err.to_string().contains("no checked state available"));
    }

    #[test]
    fn validate_buffer_request_accepts_committed_full_frame() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0,
            compressed_length: 0,
        };

        validate_buffer_request(&info).unwrap();
    }

    #[test]
    fn validate_buffer_request_accepts_lz4_metadata() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: GUD_COMPRESSION_LZ4,
            compressed_length: (1080 * 2280 * 2) - 128,
        };

        validate_buffer_request(&info).unwrap();
    }

    #[test]
    fn validate_buffer_request_rejects_without_committed_state() {
        reset_protocol_state();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("no committed state available"));
    }

    #[test]
    fn validate_buffer_request_rejects_out_of_bounds_rect() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 1000,
            y: 2200,
            width: 200,
            height: 100,
            length: 200 * 100 * 2,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("exceeds mode"));
    }

    #[test]
    fn validate_buffer_request_rejects_length_mismatch() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 16,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("buffer length"));
    }

    #[test]
    fn validate_buffer_request_rejects_unknown_compression() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0x7f,
            compressed_length: 1024,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("unsupported compression"));
    }

    #[test]
    fn validate_buffer_request_rejects_compressed_length_larger_than_payload() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: GUD_COMPRESSION_LZ4,
            compressed_length: (1080 * 2280 * 2) + 1,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("exceeds uncompressed length"));
    }

    #[test]
    fn display_enable_requires_controller_and_commit() {
        reset_protocol_state();

        let err = update_display_enabled(true).unwrap_err();
        assert!(err.to_string().contains("controller is disabled"));

        update_controller_enabled(true);
        let err = update_display_enabled(true).unwrap_err();
        assert!(err.to_string().contains("before state commit"));
    }

    #[test]
    fn controller_disable_turns_off_display() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);
        update_display_enabled(true).unwrap();
        update_controller_enabled(false);

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("controller is disabled"));
    }

    #[test]
    fn buffer_rejected_when_display_disabled() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();
        update_controller_enabled(true);

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("display is disabled"));
    }

    #[test]
    fn parse_enable_request_accepts_only_zero_or_one() {
        assert!(!parse_enable_request(&[0], "test").unwrap());
        assert!(parse_enable_request(&[1], "test").unwrap());
        assert!(parse_enable_request(&[], "test").is_err());
        assert!(parse_enable_request(&[2], "test").is_err());
    }

    #[test]
    fn suspend_transition_clears_committed_state() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        });
        commit_pending_state().unwrap();

        handle_suspend_transition();

        let info = SetBuffer {
            x: 0,
            y: 0,
            width: 1080,
            height: 2280,
            length: 1080 * 2280 * 2,
            compression: 0,
            compressed_length: 0,
        };
        let err = validate_buffer_request(&info).unwrap_err();

        assert!(err.to_string().contains("no committed state available"));
        assert_eq!(current_status(), GUD_STATUS_OK);
    }

    #[test]
    fn resume_transition_reports_connector_changed_again() {
        reset_connector_status_changed();
        let _ = next_connector_status();

        handle_resume_transition();

        let status = next_connector_status();
        assert_eq!(
            status,
            GUD_CONNECTOR_STATUS_CONNECTED | GUD_CONNECTOR_STATUS_CHANGED
        );
    }
}

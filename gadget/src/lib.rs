use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use std::env::var_os;
use std::fs::{rename, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

pub const GUD_DISPLAY_FLAG_STATUS_ON_SET: u32 = 1 << 0;
pub const GUD_DISPLAY_MODE_FLAG_PREFERRED: u32 = 1 << 10;
pub const GUD_DISPLAY_MODE_FLAG_USER_MASK: u32 = 0x0000_33ff;
pub const GUD_PIXEL_FORMAT_RGB565: u8 = 0x40;
pub const GUD_PIXEL_FORMAT_RGB888: u8 = 0x50;
pub const GUD_PIXEL_FORMAT_XRGB8888: u8 = 0x80;

const GUD_CONNECTOR_TYPE_PANEL: u8 = 0;

const GUD_STATUS_OK: u8 = 0;
const GUD_STATUS_REQUEST_NOT_SUPPORTED: u8 = 0x02;
const GUD_STATUS_INVALID_PARAMETER: u8 = 0x04;

const GUD_STATE_CHECK_HEADER_LEN: usize = 26;
const GUD_PROPERTY_SIZE: usize = 10;

// The GUD custom function has one interface and one bulk OUT endpoint, which
// usb-gadget exposes as ep1 under its deterministic FunctionFS mount point.
const FUNCTIONFS_BULK_OUT_EP_PATH: &str = "/dev/ffs-usb-gadget0-0/ep1";
// Endpoint::bulk() advertises 512 bytes as its high-speed maximum packet size.
// Do not query EndpointReceiver for this at runtime: that lazily opens ep1
// through usb-gadget's native-AIO path, which is precisely the receive path we
// avoid below.
const FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE: usize = 512;
const FUNCTIONFS_BULK_OUT_MIN_READ_SIZE: usize = 4 * 1024;
// Keep 64 KiB available only for the later diagnostic A/B. The target DWC2
// gadget path segments controller transfers internally, but FunctionFS may
// still need a 64 KiB-class contiguous buffer when scatter-gather is disabled.
const FUNCTIONFS_BULK_OUT_MAX_READ_SIZE: usize = 64 * 1024;
pub const DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE: usize = 16 * 1024;

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
    // Keep the receiver alive so usb-gadget can publish the FunctionFS
    // endpoint, but do not use it: its I/O implementation is native AIO.
    _ep_rx: EndpointReceiver,
    // Retained only for the unrelated viewer demo's historical 512-byte
    // receive behavior.
    legacy_ep_buf: Vec<BytesMut>,
    legacy_512: bool,
    // The FunctionFS endpoint used for ordinary blocking reads. Keeping this
    // separate from EndpointReceiver avoids its Linux AIO receive queue.
    bulk_ep: Option<Arc<File>>,
    // Maximum size of one blocking FunctionFS read. Each read is further
    // limited to the exact number of bytes remaining in the GUD payload.
    read_size: usize,
    payload_seq: u64,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayStateSnapshot {
    pub mode: DisplayMode,
    pub format: u8,
    pub connector: u8,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveScanoutState {
    pub width: u32,
    pub height: u32,
    pub format: u8,
    pub connector: u8,
}

#[derive(Debug, Default)]
struct ProtocolState {
    pending_state: Option<DisplayStateSnapshot>,
    committed_state: Option<DisplayStateSnapshot>,
    next_generation: u64,
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

#[derive(Clone, Copy, Debug, Default)]
pub struct PayloadStats {
    pub payload_seq: u64,
    pub transfer_bytes: usize,
    pub output_bytes: usize,
    /// Estimated USB data packets, excluding retries and protocol overhead.
    pub usb_packets_est: usize,
    pub read_calls: usize,
    pub read_size: usize,
    pub first_request_bytes: usize,
    pub last_request_bytes: usize,
    pub read_ms: u128,
    pub decompress_ms: u128,
    pub total_ms: u128,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CopyStats {
    pub copy_ms: u128,
}

#[derive(Debug)]
pub enum Event<'a> {
    GetDescriptor(GetDescriptor<'a>),
    GetDisplayModes(GetDisplayModes<'a>),
    GetPixelFormats(GetPixelFormats<'a>),
    StateChecked(DisplayStateSnapshot),
    StateCommitted(DisplayStateSnapshot),
    ProtocolStateInvalidated {
        generation: Option<u64>,
        reason: ProtocolInvalidationReason,
    },
    Buffer(SetBuffer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolInvalidationReason {
    InvalidStateCheck,
    CommitWithoutPending,
    Bind,
    Enable,
    Suspend,
    Resume,
    Disconnected,
}

impl ProtocolInvalidationReason {
    pub fn is_disconnect(self) -> bool {
        self == Self::Disconnected
    }

    pub fn is_host_activity(self) -> bool {
        matches!(self, Self::InvalidStateCheck | Self::CommitWithoutPending)
    }
}

impl Event<'_> {
    pub fn is_host_activity(&self) -> bool {
        match self {
            Self::GetDescriptor(_)
            | Self::GetDisplayModes(_)
            | Self::GetPixelFormats(_)
            | Self::StateChecked(_)
            | Self::StateCommitted(_)
            | Self::Buffer(_) => true,
            Self::ProtocolStateInvalidated { reason, .. } => reason.is_host_activity(),
        }
    }
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
        self.send_descriptor_with_max_buffer_size(
            min_width,
            min_height,
            max_width,
            max_height,
            compression,
            None,
        )
    }

    pub fn send_descriptor_with_max_buffer_size(
        self,
        min_width: u32,
        min_height: u32,
        max_width: u32,
        max_height: u32,
        compression: u8,
        max_buffer_size: Option<u32>,
    ) -> anyhow::Result<()> {
        let descriptor = build_display_descriptor(
            min_width,
            min_height,
            max_width,
            max_height,
            compression,
            max_buffer_size,
        )?;
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
    max_buffer_size: Option<u32>,
) -> anyhow::Result<DisplayDescriptor> {
    let natural_max_buffer_size = max_width
        .checked_mul(max_height)
        .and_then(|pixels| pixels.checked_mul(4))
        .context("natural display descriptor maximum buffer size overflowed u32")?;
    let max_buffer_size = max_buffer_size.unwrap_or(natural_max_buffer_size);
    ensure!(
        max_buffer_size > 0,
        "display descriptor maximum buffer size must be greater than zero"
    );
    ensure!(
        max_buffer_size <= natural_max_buffer_size,
        "display descriptor maximum buffer size {} exceeds natural maximum {}",
        max_buffer_size,
        natural_max_buffer_size
    );

    Ok(DisplayDescriptor {
        magic: GUD_DISPLAY_MAGIC,
        version: 1,
        flags: 0,
        compression,
        max_height,
        max_width,
        min_height,
        min_width,
        max_buffer_size,
    })
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
        let mut normalized = mode.clone();
        normalized.flags = (mode.flags & GUD_DISPLAY_MODE_FLAG_USER_MASK)
            | (mode.flags & GUD_DISPLAY_MODE_FLAG_PREFERRED);
        pos += ssmarshal::serialize(&mut buf[pos..], &normalized).context("serialize mode")?;
    }
    Ok(buf)
}

fn modes_have_same_user_timing(left: &DisplayMode, right: &DisplayMode) -> bool {
    left.clock == right.clock
        && left.hdisplay == right.hdisplay
        && left.hsync_start == right.hsync_start
        && left.hsync_end == right.hsync_end
        && left.htotal == right.htotal
        && left.vdisplay == right.vdisplay
        && left.vsync_start == right.vsync_start
        && left.vsync_end == right.vsync_end
        && left.vtotal == right.vtotal
        && (left.flags & GUD_DISPLAY_MODE_FLAG_USER_MASK)
            == (right.flags & GUD_DISPLAY_MODE_FLAG_USER_MASK)
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

fn reset_protocol_state() -> Option<u64> {
    let mut state = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    let invalidated_generation = state
        .pending_state
        .take()
        .map(|snapshot| snapshot.generation);
    state.committed_state = None;
    state.controller_enabled = false;
    state.display_enabled = false;
    invalidated_generation
}

fn handle_suspend_transition() -> Option<u64> {
    let invalidated_generation = reset_protocol_state();
    reset_status();
    invalidated_generation
}

fn handle_resume_transition() -> Option<u64> {
    reset_connector_status_changed();
    let invalidated_generation = reset_protocol_state();
    reset_status();
    invalidated_generation
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
        validation
            .modes
            .iter()
            .any(|mode| modes_have_same_user_timing(mode, &header.mode)),
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

fn store_pending_state(state: DisplayState) -> anyhow::Result<DisplayStateSnapshot> {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    let generation = protocol
        .next_generation
        .checked_add(1)
        .context("pending-state generation exhausted")?;
    protocol.next_generation = generation;
    let snapshot = DisplayStateSnapshot {
        mode: state.mode,
        format: state.format,
        connector: state.connector,
        generation,
    };
    protocol.pending_state = Some(snapshot.clone());
    Ok(snapshot)
}

fn clear_pending_state() -> Option<u64> {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    protocol
        .pending_state
        .take()
        .map(|snapshot| snapshot.generation)
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

fn commit_pending_state() -> anyhow::Result<DisplayStateSnapshot> {
    let mut protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    let pending = protocol
        .pending_state
        .take()
        .context("no checked state available to commit")?;
    protocol.committed_state = Some(pending.clone());
    Ok(pending)
}

pub fn active_scanout_state() -> Option<ActiveScanoutState> {
    let protocol = protocol_state()
        .lock()
        .expect("protocol state lock poisoned");
    protocol
        .committed_state
        .as_ref()
        .map(|state| ActiveScanoutState {
            width: state.mode.hdisplay as u32,
            height: state.mode.vdisplay as u32,
            format: state.format,
            connector: state.connector,
        })
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
            let generation = reset_protocol_state();
            debug!("Enable event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Enable,
            }));
        }
        custom::Event::Bind => {
            reset_connector_status_changed();
            reset_status();
            let generation = reset_protocol_state();
            debug!("Bind event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Bind,
            }));
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
                        Ok(state) => match store_pending_state(state) {
                            Ok(snapshot) => {
                                debug!(
                                    generation = snapshot.generation,
                                    "received valid state check"
                                );
                                mark_success();
                                return Ok(Some(Event::StateChecked(snapshot)));
                            }
                            Err(err) => {
                                let generation = clear_pending_state();
                                latch_status(GUD_STATUS_INVALID_PARAMETER);
                                warn!("rejected state check: {}", err);
                                return Ok(Some(Event::ProtocolStateInvalidated {
                                    generation,
                                    reason: ProtocolInvalidationReason::InvalidStateCheck,
                                }));
                            }
                        },
                        Err(err) => {
                            let generation = clear_pending_state();
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected state check: {}", err);
                            return Ok(Some(Event::ProtocolStateInvalidated {
                                generation,
                                reason: ProtocolInvalidationReason::InvalidStateCheck,
                            }));
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
                        Ok(snapshot) => {
                            debug!(generation = snapshot.generation, "committed checked state");
                            mark_success();
                            return Ok(Some(Event::StateCommitted(snapshot)));
                        }
                        Err(err) => {
                            latch_status(GUD_STATUS_INVALID_PARAMETER);
                            warn!("rejected state commit: {}", err);
                            return Ok(Some(Event::ProtocolStateInvalidated {
                                generation: None,
                                reason: ProtocolInvalidationReason::CommitWithoutPending,
                            }));
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
                            let expected_bulk_bytes = if v.compression > 0 {
                                v.compressed_length
                            } else {
                                v.length
                            };
                            debug!(
                                x = v.x,
                                y = v.y,
                                width = v.width,
                                height = v.height,
                                length = v.length,
                                compressed_length = v.compressed_length,
                                compression = v.compression,
                                expected_bulk_bytes,
                                "received and validated GUD SET_BUFFER; awaiting bulk OUT"
                            );
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
            let generation = handle_suspend_transition();
            debug!("Suspend event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Suspend,
            }));
        }
        custom::Event::Resume => {
            let generation = handle_resume_transition();
            debug!("Resume event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Resume,
            }));
        }
        custom::Event::Disable => {
            let generation = handle_suspend_transition();
            debug!("Disable event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Disconnected,
            }));
        }
        other_event => {
            warn!("unhandled event {:?}", other_event);
        }
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BulkReadStats {
    read_calls: usize,
    first_request_bytes: usize,
    last_request_bytes: usize,
}

fn validate_functionfs_read_size(read_size: usize) -> anyhow::Result<()> {
    ensure!(
        (FUNCTIONFS_BULK_OUT_MIN_READ_SIZE..=FUNCTIONFS_BULK_OUT_MAX_READ_SIZE)
            .contains(&read_size),
        "FunctionFS read size must be between {} and {} bytes, got {}",
        FUNCTIONFS_BULK_OUT_MIN_READ_SIZE,
        FUNCTIONFS_BULK_OUT_MAX_READ_SIZE,
        read_size
    );
    ensure!(
        read_size & (FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE - 1) == 0,
        "FunctionFS read size must be a multiple of {} bytes, got {}",
        FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE,
        read_size
    );
    Ok(())
}

fn usb_packet_estimate(bytes: usize) -> usize {
    bytes.div_ceil(FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE)
}

fn read_functionfs_payload<R: Read>(
    reader: &mut R,
    buf: &mut BytesMut,
    payload_seq: u64,
    payload_len: usize,
    read_size: usize,
) -> anyhow::Result<BulkReadStats> {
    ensure!(
        read_size >= FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE
            && read_size & (FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE - 1) == 0,
        "internal FunctionFS read size must be a non-zero multiple of {} bytes, got {}",
        FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE,
        read_size
    );
    ensure!(
        payload_len > 0,
        "FunctionFS payload length must be non-zero"
    );

    buf.clear();
    buf.resize(payload_len, 0);

    let mut stats = BulkReadStats::default();
    let mut received_bytes = 0usize;
    while received_bytes < payload_len {
        let remaining_before = payload_len - received_bytes;
        // Keep the configured ceiling packet-aligned, but never round the
        // final userspace count past the protocol payload. FunctionFS aligns
        // its kernel-side OUT buffer as needed; padding this count can wait
        // forever when the host ends on a full packet without a ZLP.
        let request_bytes = remaining_before.min(read_size);
        let read_index = stats.read_calls + 1;
        if stats.read_calls == 0 {
            stats.first_request_bytes = request_bytes;
        }

        debug!(
            payload_seq,
            payload_len,
            read_size,
            read_index,
            received_bytes,
            remaining_before,
            request_bytes,
            "starting blocking FunctionFS bulk OUT read"
        );

        let read_start = Instant::now();
        let result = reader.read(&mut buf[received_bytes..received_bytes + request_bytes]);
        let read_us = read_start.elapsed().as_micros();
        let bytes_read = result.with_context(|| {
            format!(
                "read blocking bulk ep: payload_seq={payload_seq}, payload_len={payload_len}, read_size={read_size}, read_index={read_index}, received_bytes={received_bytes}, remaining_before={remaining_before}, request_bytes={request_bytes}"
            )
        })?;
        stats.read_calls += 1;
        stats.last_request_bytes = request_bytes;

        let remaining_after = remaining_before.saturating_sub(bytes_read);
        let short_read = bytes_read != request_bytes;
        debug!(
            payload_seq,
            payload_len,
            read_size,
            read_index,
            remaining_before,
            request_bytes,
            result_bytes = bytes_read,
            remaining_after,
            read_us,
            short_read,
            "completed blocking FunctionFS bulk OUT read"
        );

        if short_read {
            bail!(
                "short FunctionFS bulk OUT read: payload_seq={payload_seq}, payload_len={payload_len}, read_size={read_size}, read_index={read_index}, received_bytes={received_bytes}, request_bytes={request_bytes}, result_bytes={bytes_read}"
            );
        }

        received_bytes += bytes_read;
    }

    Ok(stats)
}

impl PixelDataEndpoint {
    /// Preserve the historical 512-byte receive granularity for the unrelated
    /// viewer demo. The Pi `gud-drm` service must use
    /// `new_with_read_size()` instead.
    pub fn new_legacy_512() -> (Self, Endpoint) {
        Self::build(FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE, true)
    }

    pub fn new_with_read_size(read_size: usize) -> anyhow::Result<(Self, Endpoint)> {
        validate_functionfs_read_size(read_size)?;
        Ok(Self::build(read_size, false))
    }

    fn build(read_size: usize, legacy_512: bool) -> (Self, Endpoint) {
        let (ep_rx, ep_dir) = EndpointDirection::host_to_device();

        (
            Self {
                _ep_rx: ep_rx,
                legacy_ep_buf: Vec::new(),
                legacy_512,
                bulk_ep: None,
                read_size,
                payload_seq: 0,
                buf: BytesMut::new(),
                compress_buf: BytesMut::new(),
            },
            Endpoint::bulk(ep_dir),
        )
    }

    pub fn recv_payload(
        &mut self,
        info: &SetBuffer,
        bpp: usize,
    ) -> anyhow::Result<(&[u8], PayloadStats)> {
        let stats = self.recv_payload_bytes(info)?;
        self.finish_payload(info, bpp, stats)
    }

    pub fn recv_payload_bytes(&mut self, info: &SetBuffer) -> anyhow::Result<PayloadStats> {
        let len = if info.compression > 0 {
            info.compressed_length
        } else {
            info.length
        } as usize;
        self.payload_seq = self.payload_seq.wrapping_add(1);
        let payload_seq = self.payload_seq;
        debug!(
            payload_seq,
            payload_len = len,
            read_size = self.read_size,
            max_packet_size = FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE,
            "receiving FunctionFS bulk OUT payload"
        );

        let read_start = Instant::now();
        let bulk_stats = if self.legacy_512 {
            self.read_legacy_512_payload(len)?
        } else {
            let bulk_ep = self.bulk_endpoint()?;
            let mut bulk_reader = bulk_ep.as_ref();
            debug!(
                payload_seq,
                payload_len = len,
                read_size = self.read_size,
                "FunctionFS bulk OUT endpoint is armed for blocking read"
            );
            read_functionfs_payload(
                &mut bulk_reader,
                &mut self.buf,
                payload_seq,
                len,
                self.read_size,
            )?
        };
        let read_ms = read_start.elapsed().as_millis();
        debug!(
            payload_seq,
            payload_bytes = self.buf.len(),
            read_calls = bulk_stats.read_calls,
            usb_packets_est = usb_packet_estimate(len),
            read_ms,
            "completed FunctionFS bulk OUT payload"
        );

        Ok(PayloadStats {
            payload_seq,
            transfer_bytes: len,
            output_bytes: info.length as usize,
            usb_packets_est: usb_packet_estimate(len),
            read_calls: bulk_stats.read_calls,
            read_size: self.read_size,
            first_request_bytes: bulk_stats.first_request_bytes,
            last_request_bytes: bulk_stats.last_request_bytes,
            read_ms,
            ..PayloadStats::default()
        })
    }

    pub fn finish_payload(
        &mut self,
        info: &SetBuffer,
        bpp: usize,
        mut stats: PayloadStats,
    ) -> anyhow::Result<(&[u8], PayloadStats)> {
        let processing_start = Instant::now();
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
            stats.decompress_ms = decompress_start.elapsed().as_millis();
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
        stats.total_ms = stats.read_ms + processing_start.elapsed().as_millis();
        debug!("recv_payload total took {}ms", stats.total_ms);

        Ok((buf, stats))
    }

    fn read_legacy_512_payload(&mut self, len: usize) -> anyhow::Result<BulkReadStats> {
        let max_packet_size = FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE;
        self.buf.clear();
        if self.buf.capacity() < len {
            self.buf.reserve(len - self.buf.capacity());
        }

        let mut stats = BulkReadStats::default();
        while self.buf.len() < len {
            let mut buf = self
                .legacy_ep_buf
                .pop()
                .unwrap_or_else(|| BytesMut::zeroed(max_packet_size));
            buf.resize(max_packet_size, 0);
            let buffer_capacity = buf.capacity();
            let received_bytes = self.buf.len();
            let bulk_ep = self.bulk_endpoint()?;
            let mut bulk_reader = bulk_ep.as_ref();
            let bytes_read = bulk_reader.read(&mut buf).with_context(|| {
                format!(
                    "read legacy blocking bulk ep: payload_len={len}, \
                     received_bytes={received_bytes}, reads={}, \
                     max_packet_size={max_packet_size}, \
                     buffer_capacity={buffer_capacity}",
                    stats.read_calls
                )
            })?;
            stats.read_calls += 1;
            if stats.read_calls == 1 {
                stats.first_request_bytes = max_packet_size;
            }
            stats.last_request_bytes = max_packet_size;
            if bytes_read == 0 {
                self.legacy_ep_buf.push(buf);
                continue;
            }
            buf.truncate(bytes_read);
            self.buf.extend_from_slice(&buf);
            buf.clear();
            self.legacy_ep_buf.push(buf);
        }

        if self.buf.len() > len {
            warn!(
                "legacy bulk read overshot expected payload length: got {} bytes, expected {}. \
                 Truncating trailing {} bytes",
                self.buf.len(),
                len,
                self.buf.len() - len
            );
            self.buf.truncate(len);
        }

        Ok(stats)
    }

    fn bulk_endpoint(&mut self) -> anyhow::Result<Arc<File>> {
        if self.bulk_ep.is_none() {
            debug!(
                path = FUNCTIONFS_BULK_OUT_EP_PATH,
                "using initialized FunctionFS bulk OUT endpoint for blocking reads"
            );
            self.bulk_ep = Some(
                self._ep_rx
                    .file()
                    .context("get initialized FunctionFS bulk OUT endpoint")?,
            );
        }

        Ok(Arc::clone(
            self.bulk_ep
                .as_ref()
                .expect("bulk endpoint was just initialized"),
        ))
    }

    pub fn copy_buffer_to_framebuffer(
        info: &SetBuffer,
        buf: &[u8],
        fb: &mut [u8],
        fb_pitch: usize,
        bpp: usize,
    ) -> anyhow::Result<()> {
        Self::copy_buffer_to_framebuffer_with_stats(info, buf, fb, fb_pitch, bpp).map(|_| ())
    }

    pub fn copy_buffer_to_framebuffer_with_stats(
        info: &SetBuffer,
        buf: &[u8],
        fb: &mut [u8],
        fb_pitch: usize,
        bpp: usize,
    ) -> anyhow::Result<CopyStats> {
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

        Ok(CopyStats {
            copy_ms: copy_start.elapsed().as_millis(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        active_scanout_state, build_display_descriptor, clear_pending_state, commit_pending_state,
        configure_state_check_validation, current_status, handle_resume_transition,
        handle_suspend_transition, latch_status, mark_success, modes_have_same_user_timing,
        next_connector_status, parse_enable_request, read_functionfs_payload,
        reset_connector_status_changed, reset_protocol_state, reset_status,
        serialize_connector_descriptors, serialize_display_descriptor, serialize_display_modes,
        store_pending_state, update_controller_enabled, update_display_enabled,
        usb_packet_estimate, validate_buffer_request, validate_functionfs_read_size,
        validate_state_check_payload, ActiveScanoutState, ConnectorDescriptor, DisplayMode,
        DisplayState, PixelDataEndpoint, SetBuffer, DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE, GUD_COMPRESSION_LZ4, GUD_CONNECTOR_STATUS_CHANGED,
        GUD_CONNECTOR_STATUS_CONNECTED, GUD_CONNECTOR_TYPE_PANEL, GUD_DISPLAY_FLAG_STATUS_ON_SET,
        GUD_DISPLAY_MAGIC, GUD_DISPLAY_MODE_FLAG_PREFERRED, GUD_DISPLAY_MODE_FLAG_USER_MASK,
        GUD_PIXEL_FORMAT_RGB565, GUD_STATUS_OK, GUD_STATUS_REQUEST_NOT_SUPPORTED,
    };
    use crate::{event, Event, ProtocolInvalidationReason};
    use bytes::BytesMut;
    use serde::Serialize;
    use std::io::{self, Cursor, Read};
    use usb_gadget::function::custom;

    struct RecordingReader {
        data: Cursor<Vec<u8>>,
        max_result: Option<usize>,
        requests: Vec<usize>,
    }

    impl RecordingReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data: Cursor::new(data),
                max_result: None,
                requests: Vec::new(),
            }
        }

        fn with_max_result(data: Vec<u8>, max_result: usize) -> Self {
            Self {
                data: Cursor::new(data),
                max_result: Some(max_result),
                requests: Vec::new(),
            }
        }
    }

    impl Read for RecordingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.requests.push(buf.len());
            let result_len = self.max_result.unwrap_or(buf.len()).min(buf.len());
            self.data.read(&mut buf[..result_len])
        }
    }

    struct ErrorReader;

    impl Read for ErrorReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated endpoint cancellation",
            ))
        }
    }

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
        let descriptor = build_display_descriptor(640, 480, 1080, 2280, 0, None).unwrap();

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
        let descriptor =
            build_display_descriptor(640, 480, 1080, 2280, GUD_COMPRESSION_LZ4, None).unwrap();

        assert_eq!(descriptor.flags, 0);
        assert_eq!(descriptor.flags & GUD_DISPLAY_FLAG_STATUS_ON_SET, 0);
        assert_eq!(descriptor.compression, GUD_COMPRESSION_LZ4);
    }

    #[test]
    fn build_display_descriptor_accepts_smaller_buffer_size_override() {
        let descriptor =
            build_display_descriptor(640, 480, 1920, 1080, GUD_COMPRESSION_LZ4, Some(64_000))
                .unwrap();

        assert_eq!(descriptor.max_buffer_size, 64_000);
        assert_eq!(descriptor.compression, GUD_COMPRESSION_LZ4);
    }

    #[test]
    fn build_display_descriptor_rejects_invalid_buffer_size_override() {
        assert!(build_display_descriptor(640, 480, 1920, 1080, 0, Some(0)).is_err());
        assert!(
            build_display_descriptor(640, 480, 1920, 1080, 0, Some(1920 * 1080 * 4 + 1)).is_err()
        );
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
        let descriptor = build_display_descriptor(640, 480, 1080, 2280, 0, None).unwrap();

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
    fn serialize_display_mode_masks_private_flags_and_preserves_preferred_metadata() {
        let mut mode = sample_mode();
        mode.flags = GUD_DISPLAY_MODE_FLAG_USER_MASK | GUD_DISPLAY_MODE_FLAG_PREFERRED | (1 << 31);

        let buf = serialize_display_modes(&[mode]).unwrap();
        let serialized_flags = u32::from_le_bytes(buf[20..24].try_into().unwrap());

        assert_eq!(
            serialized_flags,
            GUD_DISPLAY_MODE_FLAG_USER_MASK | GUD_DISPLAY_MODE_FLAG_PREFERRED
        );
    }

    #[test]
    fn functionfs_read_size_requires_larger_packet_aligned_requests() {
        assert!(validate_functionfs_read_size(4 * 1024).is_ok());
        assert!(validate_functionfs_read_size(16 * 1024).is_ok());
        assert!(validate_functionfs_read_size(DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE).is_ok());

        assert!(validate_functionfs_read_size(FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE).is_err());
        assert!(validate_functionfs_read_size(65_000).is_err());
        assert!(validate_functionfs_read_size(64 * 1024 + 512).is_err());
    }

    #[test]
    fn functionfs_current_tile_uses_four_staged_reads() {
        let data = vec![0x5a; 64_000];
        let mut reader = RecordingReader::new(data.clone());
        let mut buf = BytesMut::new();

        let stats = read_functionfs_payload(
            &mut reader,
            &mut buf,
            1,
            data.len(),
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap();

        assert_eq!(reader.requests, [16_384, 16_384, 16_384, 14_848]);
        assert_eq!(buf.as_ref(), data.as_slice());
        assert_eq!(stats.read_calls, 4);
        assert_eq!(stats.first_request_bytes, 16_384);
        assert_eq!(stats.last_request_bytes, 14_848);
        assert_eq!(usb_packet_estimate(data.len()), 125);
    }

    #[test]
    fn functionfs_64k_ab_ceiling_uses_one_exact_read() {
        let data = vec![0x3c; 64_000];
        let mut reader = RecordingReader::new(data.clone());
        let mut buf = BytesMut::new();

        let stats =
            read_functionfs_payload(&mut reader, &mut buf, 2, data.len(), 64 * 1024).unwrap();

        assert_eq!(reader.requests, [64_000]);
        assert_eq!(buf.as_ref(), data.as_slice());
        assert_eq!(stats.read_calls, 1);
        assert_eq!(stats.first_request_bytes, 64_000);
        assert_eq!(stats.last_request_bytes, 64_000);
    }

    #[test]
    fn functionfs_read_ceiling_splits_only_after_full_aligned_request() {
        let data = vec![0xa5; DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE + 1];
        let mut reader = RecordingReader::new(data.clone());
        let mut buf = BytesMut::new();

        let stats = read_functionfs_payload(
            &mut reader,
            &mut buf,
            3,
            data.len(),
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap();

        assert_eq!(reader.requests, [16_384, 1]);
        assert_eq!(buf.as_ref(), data.as_slice());
        assert_eq!(stats.read_calls, 2);
        assert_eq!(stats.first_request_bytes, 16_384);
        assert_eq!(stats.last_request_bytes, 1);
    }

    #[test]
    fn functionfs_final_frame_tile_uses_four_staged_reads() {
        let data = vec![0x24; 51_200];
        let mut reader = RecordingReader::new(data.clone());
        let mut buf = BytesMut::new();

        let stats = read_functionfs_payload(
            &mut reader,
            &mut buf,
            4,
            data.len(),
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap();

        assert_eq!(reader.requests, [16_384, 16_384, 16_384, 2_048]);
        assert_eq!(buf.as_ref(), data.as_slice());
        assert_eq!(stats.read_calls, 4);
    }

    #[test]
    fn functionfs_final_unaligned_request_is_not_padded_in_userspace() {
        let data = vec![0x7e; 64_800];
        let mut reader = RecordingReader::new(data.clone());
        let mut buf = BytesMut::new();

        let stats = read_functionfs_payload(
            &mut reader,
            &mut buf,
            5,
            data.len(),
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap();

        assert_eq!(reader.requests, [16_384, 16_384, 16_384, 15_648]);
        assert_eq!(buf.as_ref(), data.as_slice());
        assert_eq!(stats.read_calls, 4);
    }

    #[test]
    fn functionfs_short_read_fails_without_queueing_another_request() {
        let data = vec![0x5a; 64_000];
        let mut reader = RecordingReader::with_max_result(data, 4 * 1024);
        let mut buf = BytesMut::new();

        let err = read_functionfs_payload(
            &mut reader,
            &mut buf,
            6,
            64_000,
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap_err();

        assert_eq!(reader.requests, [16_384]);
        assert!(err.to_string().contains("short FunctionFS bulk OUT read"));
        assert!(err.to_string().contains("result_bytes=4096"));
    }

    #[test]
    fn functionfs_zero_read_fails_without_busy_looping() {
        let mut reader = RecordingReader::new(Vec::new());
        let mut buf = BytesMut::new();

        let err = read_functionfs_payload(
            &mut reader,
            &mut buf,
            7,
            64_000,
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap_err();

        assert_eq!(reader.requests, [16_384]);
        assert!(err.to_string().contains("result_bytes=0"));
    }

    #[test]
    fn functionfs_read_error_preserves_io_error_in_chain() {
        let mut reader = ErrorReader;
        let mut buf = BytesMut::new();

        let err = read_functionfs_payload(
            &mut reader,
            &mut buf,
            8,
            64_000,
            DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        )
        .unwrap_err();
        let io_err = err
            .chain()
            .find_map(|cause| cause.downcast_ref::<io::Error>())
            .expect("I/O error should remain in the error chain");

        assert_eq!(io_err.kind(), io::ErrorKind::Interrupted);
        assert!(err.to_string().contains("read_index=1"));
        assert!(err.to_string().contains("request_bytes=16384"));
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
    fn validate_state_check_uses_normalized_full_timing_membership() {
        let mut advertised = sample_mode();
        advertised.flags =
            GUD_DISPLAY_MODE_FLAG_PREFERRED | GUD_DISPLAY_MODE_FLAG_USER_MASK | (1 << 31);
        configure_state_check_validation(1, &[GUD_PIXEL_FORMAT_RGB565], &[advertised.clone()]);
        reset_protocol_state();

        let mut echoed = advertised.clone();
        echoed.flags = GUD_DISPLAY_MODE_FLAG_USER_MASK | (1 << 30);
        let payload = serialize_state_check_request(echoed, GUD_PIXEL_FORMAT_RGB565, 0);
        validate_state_check_payload(&payload).unwrap();

        let mut different_timing = advertised;
        different_timing.clock += 1;
        assert!(!modes_have_same_user_timing(
            &sample_mode(),
            &different_timing
        ));
        let payload = serialize_state_check_request(different_timing, GUD_PIXEL_FORMAT_RGB565, 0);
        assert!(validate_state_check_payload(&payload).is_err());
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
    fn state_generations_are_monotonic_across_replacement_and_resets() {
        reset_protocol_state();
        let first = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        let replacement = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        assert!(replacement.generation > first.generation);

        assert_eq!(clear_pending_state(), Some(replacement.generation));
        let after_invalid_check = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        assert!(after_invalid_check.generation > replacement.generation);

        assert_eq!(
            handle_suspend_transition(),
            Some(after_invalid_check.generation)
        );
        let after_suspend = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        assert!(after_suspend.generation > after_invalid_check.generation);

        assert_eq!(handle_resume_transition(), Some(after_suspend.generation));
        let after_resume = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        assert!(after_resume.generation > after_suspend.generation);
    }

    #[test]
    fn successful_check_commit_promotes_the_same_generation() {
        reset_protocol_state();
        let checked = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();

        let committed = commit_pending_state().unwrap();

        assert_eq!(committed, checked);
        assert_eq!(clear_pending_state(), None);

        let repeated = store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        assert!(repeated.generation > committed.generation);
        assert_eq!(commit_pending_state().unwrap(), repeated);
    }

    #[test]
    fn every_lifecycle_reset_reports_the_invalidated_generation() {
        for (custom_event, reason) in [
            (custom::Event::Bind, ProtocolInvalidationReason::Bind),
            (custom::Event::Enable, ProtocolInvalidationReason::Enable),
            (custom::Event::Suspend, ProtocolInvalidationReason::Suspend),
            (custom::Event::Resume, ProtocolInvalidationReason::Resume),
            (
                custom::Event::Disable,
                ProtocolInvalidationReason::Disconnected,
            ),
        ] {
            let pending = store_pending_state(DisplayState {
                mode: sample_mode(),
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
            })
            .unwrap();

            let lifecycle_event = event(custom_event).unwrap().unwrap();

            assert!(matches!(
                lifecycle_event,
                Event::ProtocolStateInvalidated {
                    generation: Some(generation),
                    reason: actual_reason,
                } if generation == pending.generation && actual_reason == reason
            ));
        }
    }

    #[test]
    fn active_scanout_state_reports_committed_mode() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
        commit_pending_state().unwrap();

        assert_eq!(
            active_scanout_state(),
            Some(ActiveScanoutState {
                width: 1080,
                height: 2280,
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
            })
        );
    }

    #[test]
    fn validate_buffer_request_accepts_committed_full_frame() {
        reset_protocol_state();
        store_pending_state(DisplayState {
            mode: sample_mode(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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
        })
        .unwrap();
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

    #[test]
    fn disable_event_maps_to_disconnected() {
        let event = event(custom::Event::Disable).unwrap();
        assert!(matches!(
            event,
            Some(Event::ProtocolStateInvalidated {
                reason: ProtocolInvalidationReason::Disconnected,
                ..
            })
        ));
    }

    #[test]
    fn lifecycle_invalidations_do_not_count_as_host_activity() {
        for reason in [
            ProtocolInvalidationReason::Bind,
            ProtocolInvalidationReason::Enable,
            ProtocolInvalidationReason::Suspend,
            ProtocolInvalidationReason::Resume,
            ProtocolInvalidationReason::Disconnected,
        ] {
            let event = Event::ProtocolStateInvalidated {
                generation: None,
                reason,
            };
            assert!(!event.is_host_activity(), "{reason:?}");
        }

        for reason in [
            ProtocolInvalidationReason::InvalidStateCheck,
            ProtocolInvalidationReason::CommitWithoutPending,
        ] {
            let event = Event::ProtocolStateInvalidated {
                generation: None,
                reason,
            };
            assert!(event.is_host_activity(), "{reason:?}");
        }
    }
}

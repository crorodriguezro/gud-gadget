use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use std::env::var_os;
use std::fs::{rename, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Instant;
use tracing::{debug, error, warn};

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
const GUD_STATUS_BUSY: u8 = 0x01;
const GUD_STATUS_REQUEST_NOT_SUPPORTED: u8 = 0x02;
const GUD_STATUS_INVALID_PARAMETER: u8 = 0x04;
const GUD_STATUS_ERROR: u8 = 0x05;

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
// This generation changes only at FunctionFS lifecycle boundaries. It lets the
// receive logs prove whether an ep1 FD was opened before the current lifecycle.
static FUNCTIONFS_LIFECYCLE_GENERATION: AtomicU64 = AtomicU64::new(0);
static FUNCTIONFS_MONOTONIC_EPOCH: OnceLock<Instant> = OnceLock::new();

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
    ep_rx: EndpointReceiver,
    // Retained only for the unrelated viewer demo's historical 512-byte
    // receive behavior.
    legacy_ep_buf: Vec<BytesMut>,
    legacy_512: bool,
    // The FunctionFS endpoint used for ordinary blocking reads. Keeping this
    // separate from EndpointReceiver avoids its Linux AIO receive queue.
    bulk_ep: Option<Arc<File>>,
    bulk_ep_generation: Option<u64>,
    // Test-only blocking read submitted at FunctionFS Enable, before the host
    // can issue SET_BUFFER. This deliberately avoids the native-AIO path.
    prearmed_read: Option<PrearmedBulkRead>,
    exact_aio: ExactAioTransaction,
    // Maximum size of one blocking FunctionFS read. Each read is further
    // limited to the exact number of bytes remaining in the GUD payload.
    read_size: usize,
    payload_seq: u64,
    // The full contents of a transmitted buffer are copied here.
    buf: BytesMut,
    // If compression is enabled, the received buffer is decompressed here.
    compress_buf: BytesMut,
}

const MAX_EXACT_AIO_PAYLOAD_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExactAioState {
    Idle,
    Arming,
    ArmedAwaitStatus,
    Receiving,
    Completed,
    Poisoned,
}

#[derive(Debug)]
struct ExactAioTransaction {
    state: ExactAioState,
    sequence: u64,
    expected_bytes: usize,
    started: Option<Instant>,
}

impl Default for ExactAioTransaction {
    fn default() -> Self {
        Self {
            state: ExactAioState::Idle,
            sequence: 0,
            expected_bytes: 0,
            started: None,
        }
    }
}

impl ExactAioTransaction {
    fn begin_arm(&mut self, expected_bytes: usize) -> anyhow::Result<u64> {
        ensure!(
            self.state == ExactAioState::Idle,
            "cannot arm exact FunctionFS AIO receive while transaction is {:?}",
            self.state
        );
        ensure!(expected_bytes > 0, "exact FunctionFS AIO payload is empty");
        ensure!(
            expected_bytes <= MAX_EXACT_AIO_PAYLOAD_SIZE,
            "exact FunctionFS AIO payload {expected_bytes} exceeds guarded maximum {MAX_EXACT_AIO_PAYLOAD_SIZE}"
        );
        self.sequence = self.sequence.wrapping_add(1);
        self.expected_bytes = expected_bytes;
        self.started = Some(Instant::now());
        self.state = ExactAioState::Arming;
        Ok(self.sequence)
    }

    fn submission_accepted(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.state == ExactAioState::Arming,
            "AIO acceptance outside Arming"
        );
        self.state = ExactAioState::ArmedAwaitStatus;
        Ok(())
    }

    fn submission_rejected(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.state == ExactAioState::Arming,
            "AIO rejection outside Arming"
        );
        self.state = ExactAioState::Idle;
        self.expected_bytes = 0;
        self.started = None;
        Ok(())
    }

    fn status_sent(&mut self, status: u8) -> anyhow::Result<()> {
        if self.state != ExactAioState::ArmedAwaitStatus {
            return Ok(());
        }
        ensure!(
            status == GUD_STATUS_OK,
            "armed SET_BUFFER received non-OK status {status}"
        );
        self.state = ExactAioState::Receiving;
        Ok(())
    }

    fn completion(&mut self, actual_bytes: usize) -> anyhow::Result<()> {
        if self.state != ExactAioState::Receiving {
            let previous = self.state;
            self.state = ExactAioState::Poisoned;
            bail!("exact FunctionFS AIO completion arrived while transaction was {previous:?}");
        }
        if actual_bytes != self.expected_bytes {
            self.state = ExactAioState::Poisoned;
            bail!(
                "exact FunctionFS AIO completion length mismatch: expected={}, actual={actual_bytes}",
                self.expected_bytes
            );
        }
        self.state = ExactAioState::Completed;
        Ok(())
    }

    fn return_idle(&mut self) -> anyhow::Result<()> {
        ensure!(
            self.state == ExactAioState::Completed,
            "return to Idle without completion"
        );
        self.state = ExactAioState::Idle;
        self.expected_bytes = 0;
        self.started = None;
        Ok(())
    }

    fn poison(&mut self) {
        self.state = ExactAioState::Poisoned;
    }
}

struct PrearmedBulkRead {
    request_bytes: usize,
    lifecycle_generation: u64,
    result_rx: mpsc::Receiver<std::io::Result<PrearmedBulkReadResult>>,
    worker: JoinHandle<()>,
}

struct PrearmedBulkReadResult {
    buf: BytesMut,
    bytes_read: usize,
    read_us: u128,
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
    StatusSent(u8),
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
            Self::StatusSent(_) => false,
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
        self.send_descriptor_with_options(
            min_width,
            min_height,
            max_width,
            max_height,
            compression,
            max_buffer_size,
            0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_descriptor_with_options(
        self,
        min_width: u32,
        min_height: u32,
        max_width: u32,
        max_height: u32,
        compression: u8,
        max_buffer_size: Option<u32>,
        flags: u32,
    ) -> anyhow::Result<()> {
        ensure!(
            flags & !GUD_DISPLAY_FLAG_STATUS_ON_SET == 0,
            "unsupported GUD display flags {flags:#x}"
        );
        let descriptor = build_display_descriptor_with_flags(
            min_width,
            min_height,
            max_width,
            max_height,
            compression,
            max_buffer_size,
            flags,
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

#[cfg(test)]
fn build_display_descriptor(
    min_width: u32,
    min_height: u32,
    max_width: u32,
    max_height: u32,
    compression: u8,
    max_buffer_size: Option<u32>,
) -> anyhow::Result<DisplayDescriptor> {
    build_display_descriptor_with_flags(
        min_width,
        min_height,
        max_width,
        max_height,
        compression,
        max_buffer_size,
        0,
    )
}

fn build_display_descriptor_with_flags(
    min_width: u32,
    min_height: u32,
    max_width: u32,
    max_height: u32,
    compression: u8,
    max_buffer_size: Option<u32>,
    flags: u32,
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
        flags,
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

fn functionfs_monotonic_ns() -> u128 {
    FUNCTIONFS_MONOTONIC_EPOCH
        .get_or_init(Instant::now)
        .elapsed()
        .as_nanos()
}

fn functionfs_lifecycle_transition(name: &'static str) -> u64 {
    let generation = FUNCTIONFS_LIFECYCLE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    debug!(
        event = "functionfs_lifecycle",
        lifecycle_event = name,
        lifecycle_generation = generation,
        monotonic_ns = functionfs_monotonic_ns(),
        "FunctionFS lifecycle transition"
    );
    generation
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

pub fn bytes_per_pixel(format: u8) -> anyhow::Result<usize> {
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
            functionfs_lifecycle_transition("enable");
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
            functionfs_lifecycle_transition("bind");
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
                    debug!(
                        event = "status_reply",
                        monotonic_ns = functionfs_monotonic_ns(),
                        status,
                        "sent GUD status"
                    );
                    return Ok(Some(Event::StatusSent(status)));
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
                                event = "set_buffer_validated",
                                monotonic_ns = functionfs_monotonic_ns(),
                                lifecycle_generation =
                                    FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire),
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
            functionfs_lifecycle_transition("suspend");
            let generation = handle_suspend_transition();
            debug!("Suspend event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Suspend,
            }));
        }
        custom::Event::Resume => {
            functionfs_lifecycle_transition("resume");
            let generation = handle_resume_transition();
            debug!("Resume event received");
            return Ok(Some(Event::ProtocolStateInvalidated {
                generation,
                reason: ProtocolInvalidationReason::Resume,
            }));
        }
        custom::Event::Disable => {
            functionfs_lifecycle_transition("disable");
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FunctionFsReadCompletion {
    Exact,
    Short,
    InvalidKernelCompletion,
}

fn classify_functionfs_read_completion(
    result_bytes: usize,
    request_bytes: usize,
    remaining_before: usize,
) -> FunctionFsReadCompletion {
    if result_bytes > request_bytes || result_bytes > remaining_before {
        FunctionFsReadCompletion::InvalidKernelCompletion
    } else if result_bytes < request_bytes {
        FunctionFsReadCompletion::Short
    } else {
        FunctionFsReadCompletion::Exact
    }
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
            event = "exact_read_started",
            monotonic_ns = functionfs_monotonic_ns(),
            lifecycle_generation = FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire),
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
        debug!(
            event = "pi_functionfs_read_returned",
            monotonic_ns = functionfs_monotonic_ns(),
            lifecycle_generation = FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire),
            payload_seq,
            read_index,
            request_bytes,
            result_bytes_raw = bytes_read,
            result_bytes_signed = bytes_read as isize,
            read_us,
            "FunctionFS raw read returned"
        );
        stats.read_calls += 1;
        stats.last_request_bytes = request_bytes;

        let completion =
            classify_functionfs_read_completion(bytes_read, request_bytes, remaining_before);
        if completion == FunctionFsReadCompletion::InvalidKernelCompletion {
            let result_bytes_signed = bytes_read as isize;
            error!(
                classification = "invalid_kernel_read_completion",
                payload_seq,
                payload_len,
                read_size,
                read_index,
                received_bytes_before = received_bytes,
                remaining_before,
                request_bytes,
                result_bytes_raw = bytes_read,
                result_bytes_signed,
                "invalid kernel FunctionFS read completion"
            );
            bail!(
                "invalid kernel FunctionFS read completion: payload_seq={payload_seq}, \
                 payload_len={payload_len}, read_size={read_size}, read_index={read_index}, \
                 received_bytes_before={received_bytes}, remaining_before={remaining_before}, \
                 request_bytes={request_bytes}, result_bytes_raw={bytes_read}, \
                 result_bytes_signed={result_bytes_signed}"
            );
        }

        let remaining_after = remaining_before - bytes_read;
        let short_read = completion == FunctionFsReadCompletion::Short;
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
                ep_rx,
                legacy_ep_buf: Vec::new(),
                legacy_512,
                bulk_ep: None,
                bulk_ep_generation: None,
                prearmed_read: None,
                exact_aio: ExactAioTransaction::default(),
                read_size,
                payload_seq: 0,
                buf: BytesMut::new(),
                compress_buf: BytesMut::new(),
            },
            Endpoint::bulk(ep_dir),
        )
    }

    pub fn exact_aio_state(&self) -> ExactAioState {
        self.exact_aio.state
    }

    pub fn exact_aio_sequence(&self) -> u64 {
        self.exact_aio.sequence
    }

    pub fn exact_aio_elapsed(&self) -> Option<std::time::Duration> {
        self.exact_aio.started.map(|started| started.elapsed())
    }

    pub fn arm_exact_payload_aio(&mut self, info: &SetBuffer) -> anyhow::Result<u64> {
        ensure!(
            !self.legacy_512,
            "exact FunctionFS AIO is unavailable in legacy mode"
        );
        ensure!(
            self.prearmed_read.is_none(),
            "old prearm and STATUS_ON_SET AIO cannot coexist"
        );
        let expected_bytes = if info.compression > 0 {
            info.compressed_length as usize
        } else {
            info.length as usize
        };
        let sequence = self.exact_aio.begin_arm(expected_bytes)?;
        let buf = BytesMut::zeroed(expected_bytes);
        if buf.capacity() != expected_bytes {
            self.exact_aio.submission_rejected()?;
            bail!(
                "exact AIO allocation capacity mismatch: expected={expected_bytes}, capacity={}",
                buf.capacity()
            );
        }
        debug!(
            event = "aio_submit_enter",
            monotonic_ns = functionfs_monotonic_ns(),
            transaction_seq = sequence,
            expected_bytes,
            "submitting exact FunctionFS bulk OUT AIO request"
        );
        if let Err(err) = self.ep_rx.try_recv(buf) {
            self.exact_aio.submission_rejected()?;
            latch_status(GUD_STATUS_ERROR);
            debug!(
                event = "aio_submit_return",
                transaction_seq = sequence,
                expected_bytes,
                accepted = false,
                error = %err,
                "exact FunctionFS AIO request was not accepted"
            );
            return Err(err).context("submit exact FunctionFS bulk OUT AIO request");
        }
        self.exact_aio.submission_accepted()?;
        mark_success();
        debug!(
            event = "aio_submit_return",
            monotonic_ns = functionfs_monotonic_ns(),
            transaction_seq = sequence,
            expected_bytes,
            accepted = true,
            state = ?self.exact_aio.state,
            "exact FunctionFS AIO request accepted before GET_STATUS"
        );
        Ok(sequence)
    }

    pub fn note_status_sent(&mut self, status: u8) -> anyhow::Result<()> {
        self.exact_aio.status_sent(status)?;
        if self.exact_aio.state == ExactAioState::Receiving {
            debug!(
                event = "status_after_arm",
                monotonic_ns = functionfs_monotonic_ns(),
                transaction_seq = self.exact_aio.sequence,
                expected_bytes = self.exact_aio.expected_bytes,
                status,
                "successful status sent after exact request acceptance"
            );
        }
        Ok(())
    }

    pub fn try_complete_exact_payload_aio(
        &mut self,
        output_bytes: usize,
    ) -> anyhow::Result<Option<PayloadStats>> {
        if !matches!(
            self.exact_aio.state,
            ExactAioState::ArmedAwaitStatus | ExactAioState::Receiving
        ) {
            return Ok(None);
        }
        let Some(buf) = self
            .ep_rx
            .try_fetch()
            .context("harvest exact FunctionFS bulk OUT AIO completion")?
        else {
            return Ok(None);
        };
        let actual_bytes = buf.len();
        let sequence = self.exact_aio.sequence;
        let expected_bytes = self.exact_aio.expected_bytes;
        self.exact_aio.completion(actual_bytes)?;
        self.buf = buf;
        let read_ms = self
            .exact_aio
            .started
            .map(|started| started.elapsed().as_millis())
            .unwrap_or_default();
        debug!(
            event = "aio_completion",
            monotonic_ns = functionfs_monotonic_ns(),
            transaction_seq = sequence,
            expected_bytes,
            actual_bytes,
            exact = true,
            "harvested exact FunctionFS AIO completion"
        );
        self.payload_seq = self.payload_seq.wrapping_add(1);
        let stats = PayloadStats {
            payload_seq: self.payload_seq,
            transfer_bytes: expected_bytes,
            output_bytes,
            usb_packets_est: usb_packet_estimate(expected_bytes),
            read_calls: 1,
            read_size: expected_bytes,
            first_request_bytes: expected_bytes,
            last_request_bytes: expected_bytes,
            read_ms,
            ..PayloadStats::default()
        };
        self.exact_aio.return_idle()?;
        debug!(
            event = "exact_aio_state_transition",
            transaction_seq = sequence,
            state = ?self.exact_aio.state,
            "exact FunctionFS AIO transaction returned to Idle"
        );
        Ok(Some(stats))
    }

    pub fn poison_exact_payload_aio(&mut self, reason: &str) {
        self.exact_aio.poison();
        latch_status(GUD_STATUS_BUSY);
        error!(
            event = "exact_aio_poisoned",
            transaction_seq = self.exact_aio.sequence,
            expected_bytes = self.exact_aio.expected_bytes,
            state = ?self.exact_aio.state,
            reason,
            "exact FunctionFS AIO transaction poisoned; preserving accepted request"
        );
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
        let bulk_stats = if self.prearmed_read.is_some() {
            self.finish_prearmed_payload_read(len, payload_seq)?
        } else if self.legacy_512 {
            self.read_legacy_512_payload(len)?
        } else {
            let bulk_ep = self.bulk_endpoint()?;
            let mut bulk_reader = bulk_ep.as_ref();
            debug!(
                event = "functionfs_read_armed",
                monotonic_ns = functionfs_monotonic_ns(),
                lifecycle_generation = FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire),
                endpoint_open_generation = ?self.bulk_ep_generation,
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

    /// Queue one fixed-size blocking read before SET_BUFFER is received.
    ///
    /// This is a guarded diagnostic for the Pi DWC2 late-request-arming
    /// hypothesis. The caller must ensure the advertised GUD maximum buffer
    /// size equals `request_bytes`, and must track the read as in flight before
    /// allowing teardown.
    pub fn prearm_payload_read(&mut self, request_bytes: usize) -> anyhow::Result<()> {
        ensure!(
            !self.legacy_512,
            "prearmed FunctionFS reads are unavailable for the legacy 512-byte path"
        );
        ensure!(
            self.prearmed_read.is_none(),
            "a FunctionFS bulk OUT read is already prearmed"
        );
        validate_functionfs_read_size(request_bytes)?;
        ensure!(
            request_bytes <= self.read_size,
            "prearmed FunctionFS read size {request_bytes} exceeds configured read ceiling {}",
            self.read_size
        );

        let bulk_ep = self.bulk_endpoint()?;
        let lifecycle_generation = FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire);
        let (entered_tx, entered_rx) = mpsc::sync_channel(0);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("gud-ep1-prearm".to_owned())
            .spawn(move || {
                let mut buf = BytesMut::zeroed(request_bytes);
                debug!(
                    event = "functionfs_prearmed_read_enter",
                    monotonic_ns = functionfs_monotonic_ns(),
                    lifecycle_generation,
                    request_bytes,
                    "entering test-only prearmed FunctionFS bulk OUT read"
                );
                if entered_tx.send(()).is_err() {
                    return;
                }
                let read_start = Instant::now();
                let mut reader = bulk_ep.as_ref();
                let result = reader
                    .read(&mut buf[..])
                    .map(|bytes_read| PrearmedBulkReadResult {
                        buf,
                        bytes_read,
                        read_us: read_start.elapsed().as_micros(),
                    });
                let _ = result_tx.send(result);
            })
            .context("spawn prearmed FunctionFS bulk OUT read")?;
        entered_rx
            .recv()
            .context("prearmed FunctionFS read worker exited before syscall entry")?;
        self.prearmed_read = Some(PrearmedBulkRead {
            request_bytes,
            lifecycle_generation,
            result_rx,
            worker,
        });
        debug!(
            event = "functionfs_prearmed_read_submitted",
            monotonic_ns = functionfs_monotonic_ns(),
            lifecycle_generation,
            request_bytes,
            "test-only FunctionFS bulk OUT read submitted before SET_BUFFER"
        );
        Ok(())
    }

    fn finish_prearmed_payload_read(
        &mut self,
        payload_len: usize,
        payload_seq: u64,
    ) -> anyhow::Result<BulkReadStats> {
        let prearmed = self
            .prearmed_read
            .take()
            .context("prearmed FunctionFS bulk OUT read disappeared")?;
        ensure!(
            payload_len <= prearmed.request_bytes,
            "SET_BUFFER payload {payload_len} exceeds prearmed read size {}",
            prearmed.request_bytes
        );
        let result = prearmed
            .result_rx
            .recv()
            .context("prearmed FunctionFS read worker exited without a result")?;
        prearmed
            .worker
            .join()
            .map_err(|_| anyhow::anyhow!("prearmed FunctionFS read worker panicked"))?;
        let mut result = result.context("read prearmed blocking bulk ep")?;
        debug!(
            event = "pi_functionfs_prearmed_read_returned",
            monotonic_ns = functionfs_monotonic_ns(),
            lifecycle_generation = prearmed.lifecycle_generation,
            payload_seq,
            payload_len,
            request_bytes = prearmed.request_bytes,
            result_bytes = result.bytes_read,
            read_us = result.read_us,
            "test-only prearmed FunctionFS bulk OUT read returned"
        );
        ensure!(
            result.bytes_read == payload_len,
            "prearmed FunctionFS read length mismatch: payload_seq={payload_seq}, expected={payload_len}, request_bytes={}, result_bytes={}",
            prearmed.request_bytes,
            result.bytes_read
        );
        result.buf.truncate(result.bytes_read);
        self.buf = result.buf;

        Ok(BulkReadStats {
            read_calls: 1,
            first_request_bytes: prearmed.request_bytes,
            last_request_bytes: prearmed.request_bytes,
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
            let lifecycle_generation = FUNCTIONFS_LIFECYCLE_GENERATION.load(Ordering::Acquire);
            debug!(
                event = "functionfs_endpoint_open",
                monotonic_ns = functionfs_monotonic_ns(),
                lifecycle_generation,
                path = FUNCTIONFS_BULK_OUT_EP_PATH,
                "using initialized FunctionFS bulk OUT endpoint for blocking reads"
            );
            self.bulk_ep = Some(
                self.ep_rx
                    .file()
                    .context("get initialized FunctionFS bulk OUT endpoint")?,
            );
            self.bulk_ep_generation = Some(lifecycle_generation);
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
        ensure!(bpp > 0, "bytes per pixel must be greater than zero");
        let mut y = info.y as usize;
        let end_y = (info.y as usize)
            .checked_add(info.height as usize)
            .context("framebuffer update end row overflow")?;

        let line_len = (info.width as usize)
            .checked_mul(bpp)
            .context("framebuffer update line length overflow")?;
        let line_start = (info.x as usize)
            .checked_mul(bpp)
            .context("framebuffer update line start overflow")?;
        let expected_len = line_len
            .checked_mul(info.height as usize)
            .context("framebuffer update payload length overflow")?;
        ensure!(
            buf.len() >= expected_len,
            "payload too short: got {} bytes, expected at least {}",
            buf.len(),
            expected_len
        );
        ensure!(
            fb_pitch
                >= line_start
                    .checked_add(line_len)
                    .context("framebuffer row length overflow")?,
            "framebuffer pitch {} is too small for update x={} width={} bpp={}",
            fb_pitch,
            info.x,
            info.width,
            bpp
        );
        let required_fb_len = end_y
            .checked_mul(fb_pitch)
            .context("framebuffer update destination length overflow")?;
        ensure!(
            fb.len() >= required_fb_len,
            "framebuffer too short: got {} bytes, need at least {} for update ending at row {}",
            fb.len(),
            required_fb_len,
            end_y
        );

        let mut buf_pos = 0usize;
        while y < end_y {
            let fb_start = y
                .checked_mul(fb_pitch)
                .and_then(|offset| offset.checked_add(line_start))
                .context("framebuffer update destination offset overflow")?;
            let fb_end = fb_start
                .checked_add(line_len)
                .context("framebuffer update destination end overflow")?;
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
        active_scanout_state, build_display_descriptor, build_display_descriptor_with_flags,
        classify_functionfs_read_completion, clear_pending_state, commit_pending_state,
        configure_state_check_validation, current_status, handle_resume_transition,
        handle_suspend_transition, latch_status, mark_success, modes_have_same_user_timing,
        next_connector_status, parse_enable_request, read_functionfs_payload,
        reset_connector_status_changed, reset_protocol_state, reset_status,
        serialize_connector_descriptors, serialize_display_descriptor, serialize_display_modes,
        store_pending_state, update_controller_enabled, update_display_enabled,
        usb_packet_estimate, validate_buffer_request, validate_functionfs_read_size,
        validate_state_check_payload, ActiveScanoutState, ConnectorDescriptor, DisplayMode,
        DisplayState, ExactAioState, ExactAioTransaction, FunctionFsReadCompletion,
        PixelDataEndpoint, SetBuffer, DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE,
        FUNCTIONFS_BULK_OUT_MAX_PACKET_SIZE, GUD_COMPRESSION_LZ4, GUD_CONNECTOR_STATUS_CHANGED,
        GUD_CONNECTOR_STATUS_CONNECTED, GUD_CONNECTOR_TYPE_PANEL, GUD_DISPLAY_FLAG_STATUS_ON_SET,
        GUD_DISPLAY_MAGIC, GUD_DISPLAY_MODE_FLAG_PREFERRED, GUD_DISPLAY_MODE_FLAG_USER_MASK,
        GUD_PIXEL_FORMAT_RGB565, GUD_STATUS_OK, GUD_STATUS_REQUEST_NOT_SUPPORTED,
    };
    use crate::{event, Event, ProtocolInvalidationReason};
    use bytes::BytesMut;
    use serde::Serialize;
    use std::io::{self, Cursor, Read};
    use std::sync::{Mutex, MutexGuard};
    use usb_gadget::function::custom;

    static TEST_GLOBAL_STATE: Mutex<()> = Mutex::new(());

    fn test_global_state() -> MutexGuard<'static, ()> {
        TEST_GLOBAL_STATE.lock().expect("test state lock poisoned")
    }

    struct RecordingReader {
        data: Cursor<Vec<u8>>,
        max_result: Option<usize>,
        reported_result: Option<usize>,
        requests: Vec<usize>,
    }

    impl RecordingReader {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data: Cursor::new(data),
                max_result: None,
                reported_result: None,
                requests: Vec::new(),
            }
        }

        fn with_max_result(data: Vec<u8>, max_result: usize) -> Self {
            Self {
                data: Cursor::new(data),
                max_result: Some(max_result),
                reported_result: None,
                requests: Vec::new(),
            }
        }

        fn with_reported_result(data: Vec<u8>, reported_result: usize) -> Self {
            Self {
                data: Cursor::new(data),
                max_result: None,
                reported_result: Some(reported_result),
                requests: Vec::new(),
            }
        }
    }

    impl Read for RecordingReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.requests.push(buf.len());
            let result_len = self.max_result.unwrap_or(buf.len()).min(buf.len());
            let bytes_read = self.data.read(&mut buf[..result_len])?;
            Ok(self.reported_result.unwrap_or(bytes_read))
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
    fn guarded_descriptor_advertises_status_on_set_only_when_requested() {
        let ordinary = build_display_descriptor(640, 480, 1080, 2280, 0, None).unwrap();
        let guarded = build_display_descriptor_with_flags(
            640,
            480,
            1080,
            2280,
            0,
            Some(12_800),
            GUD_DISPLAY_FLAG_STATUS_ON_SET,
        )
        .unwrap();

        assert_eq!(ordinary.flags, 0);
        assert_eq!(guarded.flags, GUD_DISPLAY_FLAG_STATUS_ON_SET);
        assert_eq!(guarded.max_buffer_size, 12_800);
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SimulatedOperation {
        SetBufferValidated(usize),
        GuardEntered,
        Submit(usize),
        SubmitAccepted,
        StatusOk,
        Bulk(usize),
        Completion(usize),
        Idle,
    }

    fn simulate_status_on_set(
        payload_bytes: usize,
        submit_accepted: bool,
        completion: Option<usize>,
    ) -> (ExactAioTransaction, Vec<SimulatedOperation>) {
        let mut transaction = ExactAioTransaction::default();
        let mut operations = vec![
            SimulatedOperation::SetBufferValidated(payload_bytes),
            SimulatedOperation::GuardEntered,
        ];
        transaction.begin_arm(payload_bytes).unwrap();
        operations.push(SimulatedOperation::Submit(payload_bytes));
        if !submit_accepted {
            transaction.submission_rejected().unwrap();
            return (transaction, operations);
        }
        transaction.submission_accepted().unwrap();
        operations.push(SimulatedOperation::SubmitAccepted);
        transaction.status_sent(GUD_STATUS_OK).unwrap();
        operations.push(SimulatedOperation::StatusOk);
        if let Some(actual) = completion {
            operations.push(SimulatedOperation::Bulk(actual));
            operations.push(SimulatedOperation::Completion(actual));
            if transaction.completion(actual).is_ok() {
                transaction.return_idle().unwrap();
                operations.push(SimulatedOperation::Idle);
            }
        }
        (transaction, operations)
    }

    #[test]
    fn simulated_host_valid_set_arms_before_status_and_completes_exactly() {
        let (transaction, operations) = simulate_status_on_set(12_800, true, Some(12_800));

        assert_eq!(transaction.state, ExactAioState::Idle);
        assert_eq!(
            operations,
            [
                SimulatedOperation::SetBufferValidated(12_800),
                SimulatedOperation::GuardEntered,
                SimulatedOperation::Submit(12_800),
                SimulatedOperation::SubmitAccepted,
                SimulatedOperation::StatusOk,
                SimulatedOperation::Bulk(12_800),
                SimulatedOperation::Completion(12_800),
                SimulatedOperation::Idle,
            ]
        );
    }

    #[test]
    fn rejected_set_buffer_never_enters_transaction_or_submits_bulk() {
        let transaction = ExactAioTransaction::default();
        assert_eq!(transaction.state, ExactAioState::Idle);
        assert_eq!(transaction.sequence, 0);
    }

    #[test]
    fn aio_submission_failure_is_proven_unaccepted_and_returns_idle() {
        let (transaction, operations) = simulate_status_on_set(12_800, false, None);
        assert_eq!(transaction.state, ExactAioState::Idle);
        assert!(!operations.contains(&SimulatedOperation::SubmitAccepted));
        assert!(!operations.contains(&SimulatedOperation::StatusOk));
        assert!(!operations
            .iter()
            .any(|op| matches!(op, SimulatedOperation::Bulk(_))));
    }

    #[test]
    fn short_zero_and_oversized_completions_poison_transaction() {
        for actual in [0, 1, 12_799, 12_801, usize::MAX] {
            let (transaction, _) = simulate_status_on_set(12_800, true, Some(actual));
            assert_eq!(
                transaction.state,
                ExactAioState::Poisoned,
                "actual={actual}"
            );
        }
    }

    #[test]
    fn missing_completion_remains_receiving_until_containment_poisons_it() {
        let (mut transaction, _) = simulate_status_on_set(12_800, true, None);
        assert_eq!(transaction.state, ExactAioState::Receiving);
        transaction.poison();
        assert_eq!(transaction.state, ExactAioState::Poisoned);
    }

    #[test]
    fn completion_before_success_status_is_protocol_poison() {
        let mut transaction = ExactAioTransaction::default();
        transaction.begin_arm(12_800).unwrap();
        transaction.submission_accepted().unwrap();
        assert!(transaction.completion(12_800).is_err());
        assert_eq!(transaction.state, ExactAioState::Poisoned);
    }

    #[test]
    fn overlapping_set_buffer_is_rejected_without_second_sequence() {
        let mut transaction = ExactAioTransaction::default();
        let first_sequence = transaction.begin_arm(12_800).unwrap();
        transaction.submission_accepted().unwrap();
        assert!(transaction.begin_arm(12_800).is_err());
        assert_eq!(transaction.sequence, first_sequence);
        assert_eq!(transaction.state, ExactAioState::ArmedAwaitStatus);
    }

    #[test]
    fn suspend_disconnect_timeout_and_explicit_poison_are_terminal() {
        for _reason in ["suspend", "disconnect", "timeout", "poisoned"] {
            let mut transaction = ExactAioTransaction::default();
            transaction.begin_arm(12_800).unwrap();
            transaction.submission_accepted().unwrap();
            transaction.poison();
            assert_eq!(transaction.state, ExactAioState::Poisoned);
            assert!(transaction.begin_arm(12_800).is_err());
        }
    }

    #[test]
    fn sequential_and_large_payloads_use_exact_independent_transactions() {
        let mut transaction = ExactAioTransaction::default();
        for (index, payload_bytes) in [12_800, 12_801, 65_537, 1_048_576].into_iter().enumerate() {
            assert_eq!(
                transaction.begin_arm(payload_bytes).unwrap(),
                u64::try_from(index + 1).unwrap()
            );
            transaction.submission_accepted().unwrap();
            transaction.status_sent(GUD_STATUS_OK).unwrap();
            transaction.completion(payload_bytes).unwrap();
            transaction.return_idle().unwrap();
            assert_eq!(transaction.state, ExactAioState::Idle);
        }
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
    fn functionfs_read_completion_classifies_exact_short_and_invalid_results() {
        assert_eq!(
            classify_functionfs_read_completion(8_192, 8_192, 12_800),
            FunctionFsReadCompletion::Exact
        );
        assert_eq!(
            classify_functionfs_read_completion(4_096, 8_192, 12_800),
            FunctionFsReadCompletion::Short
        );
        assert_eq!(
            classify_functionfs_read_completion(0, 8_192, 12_800),
            FunctionFsReadCompletion::Short
        );
        assert_eq!(
            classify_functionfs_read_completion(8_193, 8_192, 12_800),
            FunctionFsReadCompletion::InvalidKernelCompletion
        );
        assert_eq!(
            classify_functionfs_read_completion(4_096, 4_096, 2_048),
            FunctionFsReadCompletion::InvalidKernelCompletion
        );
    }

    #[test]
    fn functionfs_read_completion_classifies_observed_wrapped_dwc2_results() {
        for signed in [-514_048_isize, -518_144_isize] {
            let raw = signed as usize;
            assert_eq!(
                classify_functionfs_read_completion(raw, 8_192, 12_800),
                FunctionFsReadCompletion::InvalidKernelCompletion
            );
            assert_eq!(raw as isize, signed);
        }
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
    fn functionfs_invalid_completion_fails_without_advancing_receive_accounting() {
        let wrapped_result = (-514_048_isize) as usize;
        let mut reader = RecordingReader::with_reported_result(vec![0x5a; 8_192], wrapped_result);
        let mut buf = BytesMut::new();

        let err = read_functionfs_payload(&mut reader, &mut buf, 8, 12_800, 8 * 1024).unwrap_err();

        assert_eq!(reader.requests, [8_192]);
        assert!(err
            .to_string()
            .contains("invalid kernel FunctionFS read completion"));
        assert!(err.to_string().contains("received_bytes_before=0"));
        assert!(err.to_string().contains("remaining_before=12800"));
        assert!(err.to_string().contains("result_bytes_signed=-514048"));
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
        let _guard = test_global_state();
        let mode = sample_mode();
        configure_state_check_validation(
            1,
            &[GUD_PIXEL_FORMAT_RGB565],
            std::slice::from_ref(&mode),
        );
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
        let mode = sample_mode();
        configure_state_check_validation(
            1,
            &[GUD_PIXEL_FORMAT_RGB565],
            std::slice::from_ref(&mode),
        );
        reset_protocol_state();

        let payload = serialize_state_check_request(mode, 0x80, 0);
        let err = validate_state_check_payload(&payload).unwrap_err();

        assert!(err.to_string().contains("unsupported pixel format"));
    }

    #[test]
    fn validate_state_check_rejects_unknown_connector() {
        let _guard = test_global_state();
        let mode = sample_mode();
        configure_state_check_validation(
            1,
            &[GUD_PIXEL_FORMAT_RGB565],
            std::slice::from_ref(&mode),
        );
        reset_protocol_state();

        let payload = serialize_state_check_request(mode, GUD_PIXEL_FORMAT_RGB565, 1);
        let err = validate_state_check_payload(&payload).unwrap_err();

        assert!(err.to_string().contains("unsupported connector"));
    }

    #[test]
    fn validate_state_check_rejects_unknown_mode() {
        let _guard = test_global_state();
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
        let _guard = test_global_state();
        reset_protocol_state();

        let err = commit_pending_state().unwrap_err();

        assert!(err.to_string().contains("no checked state available"));
    }

    #[test]
    fn state_generations_are_monotonic_across_replacement_and_resets() {
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
        reset_protocol_state();

        let err = update_display_enabled(true).unwrap_err();
        assert!(err.to_string().contains("controller is disabled"));

        update_controller_enabled(true);
        let err = update_display_enabled(true).unwrap_err();
        assert!(err.to_string().contains("before state commit"));
    }

    #[test]
    fn controller_disable_turns_off_display() {
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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
        let _guard = test_global_state();
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

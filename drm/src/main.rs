mod modes;
mod scanout;

use anyhow::{ensure, Context};
use drm::buffer::Buffer;
use drm::control::{
    connector, crtc, dumbbuffer::DumbBuffer, dumbbuffer::DumbMapping, framebuffer, ClipRect,
    Device, Mode, ModeTypeFlags, PageFlipFlags,
};
use gud_gadget::{DisplayMode, Event, ProtocolInvalidationReason, GUD_COMPRESSION_LZ4};
use modes::{CatalogRoute, ModeKey, PhysicalCatalogMode, RouteCatalog};
use scanout::{
    logical_memory_estimate, FallbackReason, MappedActive, PendingPlan, PendingPlanKind,
    ScanoutAllocation, ScanoutBackend, ScanoutManager, ScanoutRuntime, ScanoutSlot,
};
use std::env::{args, var_os};
use std::ffi::OsStr;
use std::fs::{rename, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use usb_gadget::function::custom::{Custom, Interface};
use usb_gadget::{default_udc, Class, Config, Gadget, RegGadget, Strings, Udc, UdcState};

const CRTC_SET_RETRIES: usize = 20;
const CRTC_SET_RETRY_DELAY: Duration = Duration::from_millis(250);

fn should_restart_after_clean_detach(restart_requested: bool, shutdown_requested: bool) -> bool {
    restart_requested && !shutdown_requested
}

fn record_host_activity(had_host_session: &mut bool, event: &Event<'_>) {
    if event.is_host_activity() && !*had_host_session {
        *had_host_session = true;
        info!(
            had_host_session = true,
            event = ?event,
            "session_activity"
        );
    }
}

const BULK_RECEIVE_DEADLINE: Duration = Duration::from_millis(1_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum BulkReceiveState {
    Idle = 0,
    InFlight = 1,
    Poisoned = 2,
    ShuttingDown = 3,
}

impl BulkReceiveState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Idle,
            1 => Self::InFlight,
            2 => Self::Poisoned,
            3 => Self::ShuttingDown,
            _ => unreachable!("invalid bulk receive state {raw}"),
        }
    }
}

#[derive(Clone)]
struct BulkReceiveSession {
    state: Arc<AtomicU8>,
    deadline: Duration,
}

impl Default for BulkReceiveSession {
    fn default() -> Self {
        Self::with_deadline(BULK_RECEIVE_DEADLINE)
    }
}

impl BulkReceiveSession {
    fn with_deadline(deadline: Duration) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(BulkReceiveState::Idle as u8)),
            deadline,
        }
    }

    fn receive<T, F>(&self, receive: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        self.state
            .compare_exchange(
                BulkReceiveState::Idle as u8,
                BulkReceiveState::InFlight as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|raw| {
                anyhow::anyhow!(
                    "FunctionFS bulk receive session is {:?}; a fresh Pi boot may be required",
                    BulkReceiveState::from_raw(raw)
                )
            })?;
        debug!(
            deadline_ms = self.deadline.as_millis(),
            "FunctionFS bulk receive session entered InFlight"
        );

        let started = std::time::Instant::now();
        let result = receive();
        let elapsed = started.elapsed();
        match result {
            Err(err) => {
                self.state
                    .store(BulkReceiveState::Poisoned as u8, Ordering::Release);
                tracing::error!("FunctionFS bulk receive session entered Poisoned after an error");
                Err(err)
            }
            Ok(_) if elapsed > self.deadline => {
                self.state
                    .store(BulkReceiveState::Poisoned as u8, Ordering::Release);
                tracing::error!(
                    elapsed_ms = elapsed.as_millis(),
                    deadline_ms = self.deadline.as_millis(),
                    "FunctionFS bulk receive session entered Poisoned after a late completion"
                );
                Err(anyhow::anyhow!(
                    "FunctionFS bulk receive exceeded the conservative {:?} safety deadline \
                     (elapsed {:?}); session poisoned",
                    self.deadline,
                    elapsed
                ))
            }
            Ok(value) => {
                self.state
                    .compare_exchange(
                        BulkReceiveState::InFlight as u8,
                        BulkReceiveState::Idle as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .map_err(|raw| {
                        self.state
                            .store(BulkReceiveState::Poisoned as u8, Ordering::Release);
                        anyhow::anyhow!(
                            "FunctionFS bulk receive state changed unexpectedly to {:?}; \
                             session poisoned",
                            BulkReceiveState::from_raw(raw)
                        )
                    })?;
                debug!(
                    elapsed_ms = elapsed.as_millis(),
                    "FunctionFS bulk receive session returned to Idle"
                );
                Ok(value)
            }
        }
    }

    fn begin_shutdown(&self) -> Result<(), BulkReceiveState> {
        loop {
            match self.current_state() {
                BulkReceiveState::Idle => {
                    if self
                        .state
                        .compare_exchange(
                            BulkReceiveState::Idle as u8,
                            BulkReceiveState::ShuttingDown as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                BulkReceiveState::InFlight => {
                    if self
                        .state
                        .compare_exchange(
                            BulkReceiveState::InFlight as u8,
                            BulkReceiveState::Poisoned as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Err(BulkReceiveState::InFlight);
                    }
                }
                state @ (BulkReceiveState::Poisoned | BulkReceiveState::ShuttingDown) => {
                    return Err(state);
                }
            }
        }
    }

    fn current_state(&self) -> BulkReceiveState {
        BulkReceiveState::from_raw(self.state.load(Ordering::Acquire))
    }
}

trait GadgetUnbind: Send + Sync {
    fn unbind(&self) -> io::Result<()>;
}

impl GadgetUnbind for RegGadget {
    fn unbind(&self) -> io::Result<()> {
        self.bind(None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GadgetUnbindOutcome {
    Armed,
    DeferredUntilPublish,
    Unbound,
    AlreadyUnbound,
}

#[derive(Default)]
struct GadgetShutdownState {
    requested: bool,
    unbound: bool,
    target: Option<Weak<dyn GadgetUnbind>>,
}

#[derive(Clone, Default)]
struct GadgetShutdown {
    state: Arc<Mutex<GadgetShutdownState>>,
}

impl GadgetShutdown {
    fn publish(&self, target: &Arc<dyn GadgetUnbind>) -> io::Result<GadgetUnbindOutcome> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.target = Some(Arc::downgrade(target));
        state.unbound = false;

        if state.requested {
            Self::unbind_locked(&mut state)
        } else {
            Ok(GadgetUnbindOutcome::Armed)
        }
    }

    fn request_unbind(&self) -> io::Result<GadgetUnbindOutcome> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.requested = true;
        Self::unbind_locked(&mut state)
    }

    fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.target = None;
    }

    fn unbind_locked(state: &mut GadgetShutdownState) -> io::Result<GadgetUnbindOutcome> {
        if state.unbound {
            return Ok(GadgetUnbindOutcome::AlreadyUnbound);
        }

        let Some(target) = state.target.as_ref().and_then(Weak::upgrade) else {
            return Ok(GadgetUnbindOutcome::DeferredUntilPublish);
        };

        target.unbind()?;
        state.unbound = true;
        Ok(GadgetUnbindOutcome::Unbound)
    }
}

#[derive(Debug)]
pub struct Card(std::fs::File);

impl std::os::unix::io::AsFd for Card {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}

impl Device for Card {}

impl Card {
    pub fn open(path: &str) -> Self {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        options.write(true);
        Card(options.open(path).unwrap())
    }

    pub fn open_global() -> Self {
        Self::open("/dev/dri/card0")
    }
}

struct DrmScanoutBackend {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
}

impl ScanoutBackend for DrmScanoutBackend {
    type Buffer = DumbBuffer;
    type Framebuffer = framebuffer::Handle;
    type Mode = Mode;
    type Mapping<'a> = DumbMapping<'a>;

    fn mode_size(&self, mode: Self::Mode) -> (u32, u32) {
        let (width, height) = mode.size();
        (width.into(), height.into())
    }

    fn create_buffer(&mut self, width: u32, height: u32) -> anyhow::Result<Self::Buffer> {
        self.card
            .create_dumb_buffer((width, height), drm::buffer::DrmFourcc::Rgb565, 16)
            .context("create RGB565 dumb buffer")
    }

    fn buffer_pitch(&self, buffer: &Self::Buffer) -> u32 {
        buffer.pitch()
    }

    fn add_framebuffer(&mut self, buffer: &Self::Buffer) -> anyhow::Result<Self::Framebuffer> {
        self.card
            .add_framebuffer(buffer, 16, 16)
            .context("add RGB565 framebuffer")
    }

    fn map_buffer<'a>(
        &mut self,
        buffer: &'a mut Self::Buffer,
    ) -> anyhow::Result<Self::Mapping<'a>> {
        self.card
            .map_dumb_buffer(buffer)
            .context("map RGB565 dumb buffer")
    }

    fn remove_framebuffer(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
        self.card
            .destroy_framebuffer(framebuffer)
            .context("remove DRM framebuffer")
    }

    fn destroy_buffer(&mut self, buffer: Self::Buffer) -> anyhow::Result<()> {
        self.card
            .destroy_dumb_buffer(buffer)
            .context("destroy DRM dumb buffer")
    }

    fn dirty_framebuffer(
        &mut self,
        framebuffer: Self::Framebuffer,
        x1: u16,
        y1: u16,
        x2: u16,
        y2: u16,
    ) -> anyhow::Result<()> {
        self.card
            .dirty_framebuffer(framebuffer, &[ClipRect::new(x1, y1, x2, y2)])
            .context("dirty DRM framebuffer")
    }

    fn set_crtc(&mut self, framebuffer: Self::Framebuffer, mode: Self::Mode) -> anyhow::Result<()> {
        self.card
            .set_crtc(
                self.crtc,
                Some(framebuffer),
                (0, 0),
                &[self.connector],
                Some(mode),
            )
            .context("set DRM CRTC")
    }

    fn page_flip(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
        self.card
            .page_flip(self.crtc, framebuffer, PageFlipFlags::empty(), None)
            .context("page flip DRM framebuffer")
    }
}

fn dump_pixel_buffer_ppm(
    path: &Path,
    fb: &[u8],
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
        .unwrap_or("gud-framebuffer.ppm");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    let mut writer = BufWriter::new(File::create(&tmp_path)?);
    write!(writer, "P6\n{} {}\n255\n", width, height)?;

    for y in 0..height {
        let row = &fb[(y * pitch)..(y * pitch + width * bpp)];
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

fn dump_raw_buffer(path: &Path, fb: &[u8]) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gud-framebuffer.raw");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    std::fs::write(&tmp_path, fb)?;
    rename(&tmp_path, path)?;
    Ok(())
}

fn dump_framebuffer_if_enabled(
    dump_path: Option<&Path>,
    fb: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    bpp: usize,
) {
    if let Some(path) = dump_path {
        if let Err(err) = dump_pixel_buffer_ppm(path, fb, pitch, width, height, bpp) {
            warn!("Failed to dump framebuffer to {}: {}", path.display(), err);
        } else {
            debug!("Wrote framebuffer dump to {}", path.display());
        }
    }
}

fn dump_framebuffer_raw_if_enabled(dump_path: Option<&Path>, fb: &[u8]) {
    if let Some(path) = dump_path {
        if let Err(err) = dump_raw_buffer(path, fb) {
            warn!(
                "Failed to dump raw framebuffer to {}: {}",
                path.display(),
                err
            );
        } else {
            debug!("Wrote raw framebuffer dump to {}", path.display());
        }
    }
}

fn advertised_preferred_mode_index(mode_types: impl IntoIterator<Item = ModeTypeFlags>) -> usize {
    mode_types
        .into_iter()
        .position(|mode_type| mode_type.contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(0)
}

fn derive_mode_from_native(
    native: &DisplayMode,
    target_width: u16,
    target_height: u16,
) -> DisplayMode {
    let scale_dimension = |delta: u16, native_active: u16, target_active: u16| -> u16 {
        let scaled = (delta as u32 * target_active as u32) / native_active as u32;
        scaled.max(1) as u16
    };

    let h_front = scale_dimension(
        native.hsync_start - native.hdisplay,
        native.hdisplay,
        target_width,
    );
    let h_sync = scale_dimension(
        native.hsync_end - native.hsync_start,
        native.hdisplay,
        target_width,
    );
    let h_back = scale_dimension(
        native.htotal - native.hsync_end,
        native.hdisplay,
        target_width,
    );
    let v_front = scale_dimension(
        native.vsync_start - native.vdisplay,
        native.vdisplay,
        target_height,
    );
    let v_sync = scale_dimension(
        native.vsync_end - native.vsync_start,
        native.vdisplay,
        target_height,
    );
    let v_back = scale_dimension(
        native.vtotal - native.vsync_end,
        native.vdisplay,
        target_height,
    );

    let hsync_start = target_width + h_front;
    let hsync_end = hsync_start + h_sync;
    let htotal = hsync_end + h_back;
    let vsync_start = target_height + v_front;
    let vsync_end = vsync_start + v_sync;
    let vtotal = vsync_end + v_back;
    let clock = (native.clock as u64 * htotal as u64 * vtotal as u64
        / native.htotal as u64
        / native.vtotal as u64) as u32;

    DisplayMode {
        clock,
        hdisplay: target_width,
        hsync_start,
        hsync_end,
        htotal,
        vdisplay: target_height,
        vsync_start,
        vsync_end,
        vtotal,
        flags: 0,
    }
}

const RGB565_BLUE: u16 = 0x001f;
const RGB565_GREEN: u16 = 0x07e0;
const RGB565_CYAN: u16 = 0x07ff;
const RGB565_RED: u16 = 0xf800;
const RGB565_MAGENTA: u16 = 0xf81f;
const RGB565_YELLOW: u16 = 0xffe0;
const RGB565_WHITE: u16 = 0xffff;
const RGB565_DARK_GRAY: u16 = 0x4208;
const RGB565_LIGHT_GRAY: u16 = 0xc618;
const RGB565_ORANGE: u16 = 0xfd20;
const RGB565_BLACK: u16 = 0x0000;

const WAITING_SCREEN_LINES: [&str; 5] = [
    "   _~_        .------------.",
    "  (o o)       |  WAITING   |",
    " /  V  \\      '------------'",
    "/(  _  )\\         |    |",
    "  ^^ ^^           |____|",
];
const WAITING_HIGHLIGHT_LINE: usize = 1;
const WAITING_HIGHLIGHT_TEXT: &str = "WAITING";
const WAITING_GLYPH_WIDTH: usize = 5;
const WAITING_GLYPH_HEIGHT: usize = 7;
const WAITING_GLYPH_SPACING: usize = 1;
const WAITING_LINE_SPACING: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatternMode {
    Off,
    Startup,
    Hold,
    Usb,
}

impl PatternMode {
    fn from_env() -> Self {
        let Some(raw) = var_os("GUD_PATTERN_MODE") else {
            return Self::Off;
        };

        match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "startup" => Self::Startup,
            "hold" => Self::Hold,
            "usb" => Self::Usb,
            other => {
                warn!("Unknown GUD_PATTERN_MODE={other:?}, defaulting to off");
                Self::Off
            }
        }
    }

    fn uses_startup_pattern(self) -> bool {
        matches!(self, Self::Startup | Self::Hold | Self::Usb)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferFormat {
    Rgb565,
    Rgb888,
}

impl TransferFormat {
    fn from_env() -> Self {
        let Some(raw) = var_os("GUD_TRANSFER_FORMAT") else {
            return Self::Rgb565;
        };

        match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
            "rgb565" => Self::Rgb565,
            "rgb888" => Self::Rgb888,
            other => {
                warn!("Unknown GUD_TRANSFER_FORMAT={other:?}, defaulting to rgb565");
                Self::Rgb565
            }
        }
    }

    fn gud_pixel_format(self) -> u8 {
        match self {
            Self::Rgb565 => gud_gadget::GUD_PIXEL_FORMAT_RGB565,
            Self::Rgb888 => gud_gadget::GUD_PIXEL_FORMAT_RGB888,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
        }
    }
}

fn write_rgb565_pixel(fb: &mut [u8], pitch: usize, x: usize, y: usize, color: u16) {
    let offset = y * pitch + x * 2;
    let [lo, hi] = color.to_le_bytes();
    fb[offset] = lo;
    fb[offset + 1] = hi;
}

fn fill_rgb565_solid(fb: &mut [u8], pitch: usize, width: usize, height: usize, color: u16) {
    for y in 0..height {
        for x in 0..width {
            write_rgb565_pixel(fb, pitch, x, y, color);
        }
    }
}

fn fill_rgb565_rect(
    fb: &mut [u8],
    pitch: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: u16,
) {
    for yy in y..(y + height) {
        for xx in x..(x + width) {
            write_rgb565_pixel(fb, pitch, xx, yy, color);
        }
    }
}

fn waiting_scene_glyph(ch: char) -> Option<[u8; WAITING_GLYPH_HEIGHT]> {
    Some(match ch {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '~' => [0, 0b01010, 0b10101, 0, 0, 0, 0],
        '(' => [
            0b00110, 0b01000, 0b10000, 0b10000, 0b10000, 0b01000, 0b00110,
        ],
        ')' => [
            0b01100, 0b00010, 0b00001, 0b00001, 0b00001, 0b00010, 0b01100,
        ],
        'o' => [0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110, 0],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0],
        '\\' => [0b10000, 0b01000, 0b00100, 0b00010, 0b00001, 0, 0],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b01010, 0b00100,
        ],
        '^' => [0b00100, 0b01010, 0b10001, 0, 0, 0, 0],
        '|' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '\'' => [0b00100, 0b00100, 0b00010, 0, 0, 0, 0],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        _ => return None,
    })
}

fn render_bitmap_glyph(
    fb: &mut [u8],
    pitch: usize,
    x: usize,
    y: usize,
    scale: usize,
    rows: [u8; WAITING_GLYPH_HEIGHT],
    color: u16,
) {
    for (row_idx, bits) in rows.iter().enumerate() {
        for col_idx in 0..WAITING_GLYPH_WIDTH {
            if bits & (1 << (WAITING_GLYPH_WIDTH - 1 - col_idx)) == 0 {
                continue;
            }
            fill_rgb565_rect(
                fb,
                pitch,
                x + col_idx * scale,
                y + row_idx * scale,
                scale,
                scale,
                color,
            );
        }
    }
}

fn render_waiting_screen(
    fb: &mut [u8],
    pitch: usize,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    let width = width as usize;
    let height = height as usize;
    fill_rgb565_solid(fb, pitch, width, height, RGB565_BLACK);

    let line_count = WAITING_SCREEN_LINES.len();
    let max_cols = WAITING_SCREEN_LINES
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    ensure!(
        line_count > 0 && max_cols > 0,
        "waiting screen must not be empty"
    );

    let scene_width_units =
        max_cols * WAITING_GLYPH_WIDTH + max_cols.saturating_sub(1) * WAITING_GLYPH_SPACING;
    let scene_height_units =
        line_count * WAITING_GLYPH_HEIGHT + line_count.saturating_sub(1) * WAITING_LINE_SPACING;
    let scale = (width / scene_width_units)
        .min(height / scene_height_units)
        .max(1);
    let scene_width = scene_width_units * scale;
    let scene_height = scene_height_units * scale;
    let origin_x = (width - scene_width) / 2;
    let origin_y = (height - scene_height) / 2;
    let waiting_start = WAITING_SCREEN_LINES[WAITING_HIGHLIGHT_LINE]
        .find(WAITING_HIGHLIGHT_TEXT)
        .context("waiting highlight text missing from scene")?;
    let waiting_end = waiting_start + WAITING_HIGHLIGHT_TEXT.len();

    for (line_idx, line) in WAITING_SCREEN_LINES.iter().enumerate() {
        let baseline_y =
            origin_y + line_idx * (WAITING_GLYPH_HEIGHT + WAITING_LINE_SPACING) * scale;
        for (col_idx, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let glyph = waiting_scene_glyph(ch)
                .with_context(|| format!("unsupported waiting-scene glyph: {ch:?}"))?;
            let color = if line_idx == WAITING_HIGHLIGHT_LINE
                && (waiting_start..waiting_end).contains(&col_idx)
            {
                RGB565_GREEN
            } else {
                RGB565_WHITE
            };
            let glyph_x =
                origin_x + col_idx * (WAITING_GLYPH_WIDTH + WAITING_GLYPH_SPACING) * scale;
            render_bitmap_glyph(fb, pitch, glyph_x, baseline_y, scale, glyph, color);
        }
    }

    Ok(())
}

fn present_waiting_screen<B: ScanoutBackend>(
    backend: &mut B,
    active: &mut MappedActive<'_, B>,
    dump_path: Option<&Path>,
    dump_raw_path: Option<&Path>,
) -> anyhow::Result<()> {
    let (width, height) = active.size();
    let pitch = active.pitch() as usize;
    for index in 0..2 {
        render_waiting_screen(active.buffer_mut(index), pitch, width, height)?;
    }

    match backend.dirty_framebuffer(
        active.front_framebuffer(),
        0,
        0,
        width as u16,
        height as u16,
    ) {
        Ok(()) => tracing::debug!("Waiting screen flushed"),
        Err(err) => tracing::debug!("dirty_framebuffer for waiting screen failed: {}", err),
    }

    dump_framebuffer_if_enabled(
        dump_path,
        active.front_buffer_mut(),
        pitch,
        width,
        height,
        2,
    );
    dump_framebuffer_raw_if_enabled(dump_raw_path, active.front_buffer_mut());

    Ok(())
}

fn udc_is_detached(udc: &Udc) -> bool {
    match udc.state() {
        Ok(UdcState::Configured) => false,
        Ok(_) => true,
        Err(err) => {
            tracing::debug!("Failed to read UDC state: {}", err);
            false
        }
    }
}

fn diagnostic_pattern_color(x: usize, y: usize, width: usize, height: usize) -> u16 {
    const BORDER: usize = 48;
    const CORNER: usize = 128;
    const CENTER_THICKNESS: usize = 24;
    const GRID_X_STEP: usize = 180;
    const GRID_Y_STEP: usize = 240;
    const GRID_THICKNESS: usize = 4;

    let mut color = RGB565_DARK_GRAY;

    if y < BORDER {
        color = RGB565_RED;
    }
    if x >= width.saturating_sub(BORDER) {
        color = RGB565_YELLOW;
    }
    if y >= height.saturating_sub(BORDER) {
        color = RGB565_GREEN;
    }
    if x < BORDER {
        color = RGB565_BLUE;
    }

    if x < CORNER && y < CORNER {
        color = RGB565_WHITE;
    }
    if x >= width.saturating_sub(CORNER) && y < CORNER {
        color = RGB565_CYAN;
    }
    if x < CORNER && y >= height.saturating_sub(CORNER) {
        color = RGB565_MAGENTA;
    }
    if x >= width.saturating_sub(CORNER) && y >= height.saturating_sub(CORNER) {
        color = RGB565_ORANGE;
    }

    let center_x = width / 2;
    let center_y = height / 2;
    if x >= center_x.saturating_sub(CENTER_THICKNESS / 2) && x < center_x + (CENTER_THICKNESS / 2) {
        color = RGB565_WHITE;
    }
    if y >= center_y.saturating_sub(CENTER_THICKNESS / 2) && y < center_y + (CENTER_THICKNESS / 2) {
        color = RGB565_MAGENTA;
    }

    if x % GRID_X_STEP < GRID_THICKNESS || y % GRID_Y_STEP < GRID_THICKNESS {
        color = RGB565_LIGHT_GRAY;
    }

    color
}

fn fill_diagnostic_pattern_rect(
    fb: &mut [u8],
    pitch: usize,
    fb_width: u32,
    fb_height: u32,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
) -> anyhow::Result<()> {
    ensure!(
        rect_x <= fb_width,
        "rect x {} exceeds fb width {}",
        rect_x,
        fb_width
    );
    ensure!(
        rect_y <= fb_height,
        "rect y {} exceeds fb height {}",
        rect_y,
        fb_height
    );
    ensure!(
        rect_x + rect_width <= fb_width,
        "rect width {} at x {} exceeds fb width {}",
        rect_width,
        rect_x,
        fb_width
    );
    ensure!(
        rect_y + rect_height <= fb_height,
        "rect height {} at y {} exceeds fb height {}",
        rect_height,
        rect_y,
        fb_height
    );

    let fb_width = fb_width as usize;
    let fb_height = fb_height as usize;
    let rect_x = rect_x as usize;
    let rect_y = rect_y as usize;
    let rect_width = rect_width as usize;
    let rect_height = rect_height as usize;

    for y in rect_y..(rect_y + rect_height) {
        for x in rect_x..(rect_x + rect_width) {
            let color = diagnostic_pattern_color(x, y, fb_width, fb_height);
            write_rgb565_pixel(fb, pitch, x, y, color);
        }
    }

    Ok(())
}

fn rgb888_to_rgb565(r: u8, g: u8, b: u8) -> u16 {
    (((r as u16) & 0xf8) << 8) | (((g as u16) & 0xfc) << 3) | ((b as u16) >> 3)
}

fn copy_rgb888_to_rgb565_framebuffer(
    info: &gud_gadget::SetBuffer,
    buf: &[u8],
    fb: &mut [u8],
    fb_pitch: usize,
) -> anyhow::Result<()> {
    let width = info.width as usize;
    let height = info.height as usize;
    let expected_len = width * height * 3;
    ensure!(
        buf.len() >= expected_len,
        "RGB888 payload too short: got {} bytes, expected at least {}",
        buf.len(),
        expected_len
    );

    let line_start = info.x as usize * 2;
    let mut buf_pos = 0usize;
    for y in info.y as usize..(info.y + info.height) as usize {
        let fb_row = y * fb_pitch + line_start;
        for x in 0..width {
            let r = buf[buf_pos];
            let g = buf[buf_pos + 1];
            let b = buf[buf_pos + 2];
            let pixel = rgb888_to_rgb565(r, g, b).to_le_bytes();
            let dst = fb_row + x * 2;
            fb[dst] = pixel[0];
            fb[dst + 1] = pixel[1];
            buf_pos += 3;
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScaledLayout {
    dst_x: usize,
    dst_y: usize,
    dst_width: usize,
    dst_height: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShadowRasterIdentity {
    width: u32,
    height: u32,
    format: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShadowActivation {
    Preserved,
    Zeroed,
}

#[derive(Debug, Default)]
struct ShadowFramebuffer {
    width: u32,
    height: u32,
    pitch: usize,
    pixels: Vec<u8>,
    preallocated_capacity: Option<usize>,
    identity: Option<ShadowRasterIdentity>,
    content_valid: bool,
}

impl ShadowFramebuffer {
    fn preallocated(max_capacity: usize) -> anyhow::Result<Self> {
        ensure!(
            max_capacity > 0,
            "maximum source shadow capacity must be non-zero"
        );
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(max_capacity)
            .context("allocate maximum RGB565 source shadow")?;
        pixels.resize(max_capacity, 0);
        Ok(Self {
            pixels,
            preallocated_capacity: Some(max_capacity),
            ..Self::default()
        })
    }

    fn activate_scaled(
        &mut self,
        identity: ShadowRasterIdentity,
    ) -> anyhow::Result<ShadowActivation> {
        let pitch = (identity.width as usize)
            .checked_mul(2)
            .context("source shadow pitch overflow")?;
        let len = pitch
            .checked_mul(identity.height as usize)
            .context("source shadow length overflow")?;

        if let Some(capacity) = self.preallocated_capacity {
            ensure!(
                len <= capacity && self.pixels.len() == capacity,
                "committed shadow raster requires {len} bytes but preallocated capacity is \
                 {capacity}"
            );
        } else if self.pixels.len() != len {
            self.pixels = vec![0; len];
        }
        self.width = identity.width;
        self.height = identity.height;
        self.pitch = pitch;
        let preserve = self.identity == Some(identity) && self.content_valid;
        self.identity = Some(identity);
        self.content_valid = true;
        if preserve {
            Ok(ShadowActivation::Preserved)
        } else {
            self.pixels[..len].fill(0);
            Ok(ShadowActivation::Zeroed)
        }
    }

    fn ensure_size(&mut self, identity: ShadowRasterIdentity) -> anyhow::Result<()> {
        self.activate_scaled(identity).map(|_| ())
    }

    fn invalidate_content(&mut self) {
        self.content_valid = false;
    }
}

fn compute_scaled_layout(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> anyhow::Result<ScaledLayout> {
    ensure!(src_width > 0, "source width must be greater than zero");
    ensure!(src_height > 0, "source height must be greater than zero");
    ensure!(dst_width > 0, "destination width must be greater than zero");
    ensure!(
        dst_height > 0,
        "destination height must be greater than zero"
    );

    let width_limited_height = (dst_width as u64 * src_height as u64) / src_width as u64;
    let (scaled_width, scaled_height) = if width_limited_height <= dst_height as u64 {
        (dst_width as usize, width_limited_height.max(1) as usize)
    } else {
        let width = (dst_height as u64 * src_width as u64) / src_height as u64;
        (width.max(1) as usize, dst_height as usize)
    };

    let dst_width = dst_width as usize;
    let dst_height = dst_height as usize;

    Ok(ScaledLayout {
        dst_x: (dst_width - scaled_width) / 2,
        dst_y: (dst_height - scaled_height) / 2,
        dst_width: scaled_width,
        dst_height: scaled_height,
    })
}

fn scale_rgb565_to_fit(
    src: &[u8],
    src_pitch: usize,
    src_width: u32,
    src_height: u32,
    dst: &mut [u8],
    dst_pitch: usize,
    dst_width: u32,
    dst_height: u32,
) -> anyhow::Result<(ScaledLayout, u128)> {
    let start = std::time::Instant::now();
    let layout = compute_scaled_layout(src_width, src_height, dst_width, dst_height)?;

    fill_rgb565_solid(dst, dst_pitch, dst_width as usize, dst_height as usize, 0);

    let src_width = src_width as usize;
    let src_height = src_height as usize;
    for dst_y_rel in 0..layout.dst_height {
        let src_y = dst_y_rel * src_height / layout.dst_height;
        let dst_y = layout.dst_y + dst_y_rel;
        let src_row = src_y * src_pitch;
        let dst_row = dst_y * dst_pitch;
        for dst_x_rel in 0..layout.dst_width {
            let src_x = dst_x_rel * src_width / layout.dst_width;
            let src_off = src_row + src_x * 2;
            let dst_off = dst_row + (layout.dst_x + dst_x_rel) * 2;
            dst[dst_off] = src[src_off];
            dst[dst_off + 1] = src[src_off + 1];
        }
    }

    Ok((layout, start.elapsed().as_millis()))
}

fn parse_functionfs_read_size(raw: Option<&OsStr>) -> anyhow::Result<usize> {
    let Some(raw) = raw else {
        return Ok(gud_gadget::DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE);
    };
    let raw = raw
        .to_str()
        .context("GUD_FFS_READ_SIZE must be valid UTF-8")?;
    raw.parse::<usize>()
        .with_context(|| format!("invalid GUD_FFS_READ_SIZE={raw:?}"))
}

fn parse_test_compression(raw: Option<&OsStr>) -> anyhow::Result<u8> {
    let Some(raw) = raw else {
        return Ok(GUD_COMPRESSION_LZ4);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_COMPRESSION must be valid UTF-8")?;
    match raw {
        "lz4" => Ok(GUD_COMPRESSION_LZ4),
        "none" => Ok(0),
        _ => anyhow::bail!(
            "invalid GUD_TEST_COMPRESSION={raw:?}; expected exactly \"lz4\" or \"none\""
        ),
    }
}

fn parse_test_max_buffer_size(raw: Option<&OsStr>) -> anyhow::Result<Option<u32>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_MAX_BUFFER_SIZE must be valid UTF-8")?;
    let max_buffer_size = raw
        .parse::<u32>()
        .with_context(|| format!("invalid GUD_TEST_MAX_BUFFER_SIZE={raw:?}"))?;
    ensure!(
        max_buffer_size > 0,
        "GUD_TEST_MAX_BUFFER_SIZE must be greater than zero"
    );
    Ok(Some(max_buffer_size))
}

fn parse_test_dynamic_mode_match(raw: Option<&OsStr>) -> anyhow::Result<bool> {
    let Some(raw) = raw else {
        return Ok(false);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_DYNAMIC_MODE_MATCH must be valid UTF-8")?;
    ensure!(
        raw == "1",
        "invalid GUD_TEST_DYNAMIC_MODE_MATCH={raw:?}; expected exactly \"1\""
    );
    Ok(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TestOutputMode {
    width: u16,
    height: u16,
}

fn parse_test_output_mode(raw: Option<&OsStr>) -> anyhow::Result<Option<TestOutputMode>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_OUTPUT_MODE must be valid UTF-8")?;
    let (width, height) = raw
        .split_once('x')
        .context("GUD_TEST_OUTPUT_MODE must use WIDTHxHEIGHT")?;
    let width = width
        .parse::<u16>()
        .with_context(|| format!("invalid GUD_TEST_OUTPUT_MODE width in {raw:?}"))?;
    let height = height
        .parse::<u16>()
        .with_context(|| format!("invalid GUD_TEST_OUTPUT_MODE height in {raw:?}"))?;
    ensure!(
        width > 0 && height > 0,
        "GUD_TEST_OUTPUT_MODE dimensions must be greater than zero"
    );
    Ok(Some(TestOutputMode { width, height }))
}

fn select_output_mode(
    connector_modes: &[Mode],
    test_output_mode: Option<TestOutputMode>,
) -> anyhow::Result<&Mode> {
    ensure!(
        !connector_modes.is_empty(),
        "connected DRM connector has no modes"
    );
    let Some(requested) = test_output_mode else {
        return Ok(&connector_modes[0]);
    };

    connector_modes
        .iter()
        .find(|mode| mode.size() == (requested.width, requested.height))
        .with_context(|| {
            format!(
                "GUD_TEST_OUTPUT_MODE={}x{} is not supported by the connected DRM connector",
                requested.width, requested.height
            )
        })
}

fn validate_test_mode_policy(
    dynamic_mode_match: bool,
    test_output_mode: Option<TestOutputMode>,
) -> anyhow::Result<()> {
    ensure!(
        !(dynamic_mode_match && test_output_mode.is_some()),
        "GUD_TEST_DYNAMIC_MODE_MATCH and GUD_TEST_OUTPUT_MODE are mutually exclusive"
    );
    Ok(())
}

fn release_dynamic_candidate_off_baseline<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    slot1: &mut ScanoutSlot<B>,
    slot2: &mut ScanoutSlot<B>,
) -> anyhow::Result<()> {
    match runtime.roles().candidate {
        None => Ok(()),
        Some(1) => runtime.release_candidate(backend, 1, slot1),
        Some(2) => runtime.release_candidate(backend, 2, slot2),
        Some(index) => {
            anyhow::bail!("unexpected candidate slot {index} while baseline slot zero is active")
        }
    }
}

fn prepare_dynamic_check<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    slot1: &mut ScanoutSlot<B>,
    slot2: &mut ScanoutSlot<B>,
    catalog: &RouteCatalog<B::Mode>,
    snapshot: gud_gadget::DisplayStateSnapshot,
    state_check_control_ms: u128,
) -> anyhow::Result<()>
where
    B::Mode: std::fmt::Debug,
{
    let prepare_start = std::time::Instant::now();
    let logical_key = ModeKey::from_snapshot(&snapshot);
    let catalog_entry = catalog
        .entry_for_snapshot(&snapshot)
        .context("checked snapshot is absent from route catalog")?;
    let current_physical_key = runtime.dynamic_routes()?.current_physical_key;
    let candidate_key = runtime.dynamic_routes()?.candidate_key;
    let failed_key = runtime.dynamic_routes()?.failed_key(logical_key);
    let failed_cache_hit = runtime
        .dynamic_routes()?
        .failed_routes
        .contains(&failed_key);

    let mut new_candidate_key = None;
    let (kind, requested_physical_key, requested_mode, cache_hit) = match catalog_entry.route {
        CatalogRoute::Exact(physical_mode) if catalog_entry.key == current_physical_key => {
            release_dynamic_candidate_off_baseline(backend, runtime, slot1, slot2)?;
            runtime.counters().no_op();
            (
                PendingPlanKind::ExactActiveNoOp,
                Some(catalog_entry.key),
                Some(physical_mode),
                false,
            )
        }
        CatalogRoute::Exact(physical_mode)
            if candidate_key == Some(catalog_entry.key) && runtime.roles().candidate.is_some() =>
        {
            new_candidate_key = candidate_key;
            (
                PendingPlanKind::ExactCandidate {
                    slot: runtime
                        .roles()
                        .candidate
                        .expect("candidate role was checked"),
                },
                Some(catalog_entry.key),
                Some(physical_mode),
                false,
            )
        }
        CatalogRoute::Exact(physical_mode) if failed_cache_hit => {
            release_dynamic_candidate_off_baseline(backend, runtime, slot1, slot2)?;
            runtime.counters().fallback();
            (
                PendingPlanKind::ScaledCurrentAfterFailure(FallbackReason::FailedCacheHit),
                Some(catalog_entry.key),
                Some(physical_mode),
                true,
            )
        }
        CatalogRoute::Exact(physical_mode) => {
            release_dynamic_candidate_off_baseline(backend, runtime, slot1, slot2)?;
            let prepared_slot = (|| {
                let allocation =
                    ScanoutAllocation::create(backend, physical_mode, &runtime.counters())?;
                if let Err(err) = allocation.flush_black_initialization(backend) {
                    debug!(
                        error = %format_args!("{err:#}"),
                        "Candidate black-buffer dirty flush is unsupported or failed"
                    );
                }
                let (slot_index, slot) = if slot1.is_none() {
                    (1, slot1)
                } else {
                    ensure!(slot2.is_none(), "no empty candidate scanout slot");
                    (2, slot2)
                };
                runtime.stage_candidate(backend, slot_index, slot, allocation)?;
                Ok(slot_index)
            })();
            match prepared_slot {
                Ok(slot_index) => {
                    new_candidate_key = Some(catalog_entry.key);
                    (
                        PendingPlanKind::ExactCandidate { slot: slot_index },
                        Some(catalog_entry.key),
                        Some(physical_mode),
                        false,
                    )
                }
                Err(err) => {
                    runtime
                        .dynamic_routes_mut()?
                        .failed_routes
                        .insert(failed_key);
                    runtime.counters().fallback();
                    warn!(
                        generation = snapshot.generation,
                        logical = ?logical_key,
                        current_physical = ?current_physical_key,
                        error = %format_args!("{err:#}"),
                        "Exact scanout candidate preparation failed; retaining scaled current \
                         scanout"
                    );
                    (
                        PendingPlanKind::ScaledCurrentAfterFailure(FallbackReason::PrepareFailed),
                        Some(catalog_entry.key),
                        Some(physical_mode),
                        false,
                    )
                }
            }
        }
        CatalogRoute::ScaledFallback => {
            release_dynamic_candidate_off_baseline(backend, runtime, slot1, slot2)?;
            runtime.counters().fallback();
            (
                PendingPlanKind::ScaledBaseline {
                    switch_required: runtime.roles().active != runtime.roles().baseline,
                },
                None,
                None,
                false,
            )
        }
    };
    let mode_prepare_ms = prepare_start.elapsed().as_millis();
    if mode_prepare_ms > 1_000 {
        warn!(
            generation = snapshot.generation,
            mode_prepare_ms,
            "Dynamic mode preparation exceeded the provisional 1000 ms acceptance budget"
        );
    }
    {
        let routes = runtime.dynamic_routes_mut()?;
        routes.candidate_key = new_candidate_key;
        routes.pending_plan = Some(PendingPlan {
            snapshot: snapshot.clone(),
            logical_key,
            requested_physical_key,
            requested_mode,
            kind,
            failed_cache_hit: cache_hit,
            mode_prepare_ms,
        });
    }
    info!(
        event = "mode_check_decision",
        generation = snapshot.generation,
        connector = snapshot.connector,
        logical = ?logical_key,
        current_physical = ?current_physical_key,
        requested_physical = ?requested_physical_key,
        ?kind,
        failed_cache_hit = cache_hit,
        state_check_control_ms,
        mode_prepare_ms,
        candidate_slot = ?runtime.roles().candidate,
        counters = ?runtime.counters().snapshot(),
        "Prepared generation-bound dynamic mode plan"
    );
    Ok(())
}

fn invalidate_dynamic_pending_off_baseline<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    slot1: &mut ScanoutSlot<B>,
    slot2: &mut ScanoutSlot<B>,
) -> anyhow::Result<()> {
    release_dynamic_candidate_off_baseline(backend, runtime, slot1, slot2)?;
    runtime.dynamic_routes_mut()?.invalidate_pending();
    Ok(())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    info!("gud-drm starting (XDISP-P0.1 lifecycle and read-size repair)");

    let card_path = args()
        .skip(1)
        .next()
        .expect("specify full path to /dev/dri/cardN as program argument");
    let functionfs_read_size = parse_functionfs_read_size(var_os("GUD_FFS_READ_SIZE").as_deref())?;
    let test_compression_raw = var_os("GUD_TEST_COMPRESSION");
    let test_max_buffer_size_raw = var_os("GUD_TEST_MAX_BUFFER_SIZE");
    let test_output_mode_raw = var_os("GUD_TEST_OUTPUT_MODE");
    let test_dynamic_mode_match_raw = var_os("GUD_TEST_DYNAMIC_MODE_MATCH");
    let descriptor_compression = parse_test_compression(test_compression_raw.as_deref())?;
    let descriptor_max_buffer_size =
        parse_test_max_buffer_size(test_max_buffer_size_raw.as_deref())?;
    let test_output_mode = parse_test_output_mode(test_output_mode_raw.as_deref())?;
    let dynamic_mode_match = parse_test_dynamic_mode_match(test_dynamic_mode_match_raw.as_deref())?;
    validate_test_mode_policy(dynamic_mode_match, test_output_mode)?;
    let (mut gud_data, gud_data_ep) =
        gud_gadget::PixelDataEndpoint::new_with_read_size(functionfs_read_size)
            .context("configure FunctionFS bulk OUT read size")?;
    let dump_path = var_os("GUD_DUMP_FB_PATH").map(PathBuf::from);
    let dump_raw_path = var_os("GUD_DUMP_FB_RAW_PATH").map(PathBuf::from);
    let pattern_mode = PatternMode::from_env();
    let transfer_format = TransferFormat::from_env();
    if let Some(path) = dump_path.as_deref() {
        info!("Framebuffer dumps enabled: {}", path.display());
    }
    if let Some(path) = dump_raw_path.as_deref() {
        info!("Raw framebuffer dumps enabled: {}", path.display());
    }
    info!("Pattern mode: {:?}", pattern_mode);
    info!(
        "Transfer format: {:?} ({} bytes/pixel)",
        transfer_format,
        transfer_format.bytes_per_pixel()
    );
    info!(
        "FunctionFS bulk OUT read ceiling: {} bytes; conservative receive safety deadline: {} ms",
        functionfs_read_size,
        BULK_RECEIVE_DEADLINE.as_millis()
    );
    if test_compression_raw.is_some() || test_max_buffer_size_raw.is_some() {
        warn!(
            compression = if descriptor_compression == 0 {
                "none"
            } else {
                "lz4"
            },
            ?descriptor_max_buffer_size,
            "TEST-ONLY GUD descriptor override enabled; a fresh USB enumeration is required"
        );
    }
    if let Some(output_mode) = test_output_mode {
        warn!(
            width = output_mode.width,
            height = output_mode.height,
            "TEST-ONLY physical DRM output-mode override enabled; normal mode selection is unchanged \
             when GUD_TEST_OUTPUT_MODE is absent"
        );
    }
    if dynamic_mode_match {
        warn!(
            "TEST-ONLY dynamic physical-mode matching enabled; this is not normal/default \
             behavior and requires the XDISP-P2.1 hardware gates before promotion"
        );
    }
    info!("Opening DRM device: {}", card_path);
    let card = Card::open(&card_path);
    let udc = default_udc().expect("no UDC found");
    info!("Using UDC: {:?}", udc);

    let resources = card.resource_handles().expect("load drm resources failed");

    let connector = resources
        .connectors()
        .iter()
        .map(|h| {
            card.get_connector(*h, false)
                .expect("get drm connector failed")
        })
        .find(|c| c.state() == drm::control::connector::State::Connected)
        .expect("no connected connectors found");

    let encoder_handle = connector
        .current_encoder()
        .or_else(|| connector.encoders().first().copied())
        .expect("no encoder found for connected connector");
    let encoder = card
        .get_encoder(encoder_handle)
        .expect("get drm encoder failed");
    let crtc_handle = resources
        .filter_crtcs(encoder.possible_crtcs())
        .into_iter()
        .next()
        .expect("no compatible crtc found for connected connector");
    let crtc = card
        .get_crtc(crtc_handle)
        .expect("get compatible drm crtc failed");

    let connector_handle = connector.handle();
    let connector_modes = connector.modes().to_vec();
    let mode = *select_output_mode(&connector_modes, test_output_mode)?;

    let mut min_width = u32::MAX;
    let mut min_height = u32::MAX;
    let mut max_width = 0;
    let mut max_height = 0;
    for connector_mode in &connector_modes {
        let (width, height) = connector_mode.size();
        let width = width as u32;
        let height = height as u32;
        if width < min_width {
            min_width = width
        }
        if width > max_width {
            max_width = width
        }
        if height < min_height {
            min_height = height
        }
        if height > max_height {
            max_height = height
        }
    }

    min_width = min_width.min(640);
    min_height = min_height.min(480);
    max_width = max_width.max(1920);
    max_height = max_height.max(1080);
    let serialized_max_buffer_size = descriptor_max_buffer_size.unwrap_or(
        max_width
            .checked_mul(max_height)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("serialized descriptor maximum buffer size overflow")?,
    );
    info!(
        event = "descriptor_config",
        magic = "0x1d50614d",
        version = 1,
        flags = 0,
        compression = descriptor_compression,
        max_buffer_size = serialized_max_buffer_size,
        min_width,
        max_width,
        min_height,
        max_height,
        dynamic_mode_match,
        "Serialized GUD display descriptor fields"
    );

    usb_gadget::remove_all().expect("UDC init failed");
    info!("USB gadgets removed");

    info!("Created pixel data endpoint");

    let mut builder = Custom::builder().with_interface(
        Interface::new(Class::vendor_specific(Class::VENDOR_SPECIFIC, 0), "GUD")
            .with_endpoint(gud_data_ep),
    );
    builder.ffs_no_disconnect = true;
    let (mut gud, gud_handle) = builder.build();
    info!("Built USB gadget");

    let running = Arc::new(AtomicBool::new(true));
    let gadget_shutdown = GadgetShutdown::default();
    let bulk_receive_session = BulkReceiveSession::default();

    let r = running.clone();
    let shutdown = gadget_shutdown.clone();
    let signal_bulk_receive_session = bulk_receive_session.clone();
    ctrlc::set_handler(move || {
        if let Err(state) = signal_bulk_receive_session.begin_shutdown() {
            tracing::error!(
                ?state,
                "Ignoring process shutdown signal because the FunctionFS bulk session is not idle; \
                 do not stop, restart, reboot, or shut down this service instance, recover with a \
                 physical/hardware reset"
            );
            return;
        }
        r.store(false, Ordering::SeqCst);
        match shutdown.request_unbind() {
            Ok(GadgetUnbindOutcome::DeferredUntilPublish) => {
                info!("Shutdown requested before USB gadget bind")
            }
            Ok(GadgetUnbindOutcome::Unbound) => {
                info!("USB gadget unbound by shutdown handler")
            }
            Ok(GadgetUnbindOutcome::AlreadyUnbound) => {
                debug!("USB gadget was already unbound")
            }
            Ok(GadgetUnbindOutcome::Armed) => unreachable!(),
            Err(err) => tracing::error!("Failed to unbind USB gadget during shutdown: {}", err),
        }
    })
    .expect("cleanup handler registration failed");

    // The physical test override must not alter which USB mode is advertised
    // as preferred. If DRM supplies no preference, preserve the normal
    // connector-order fallback instead of falling back to the override-selected
    // physical mode.
    let preferred_mode_index =
        advertised_preferred_mode_index(connector_modes.iter().map(Mode::mode_type));
    let physical_catalog_modes: Vec<PhysicalCatalogMode<Mode>> = connector_modes
        .iter()
        .map(|mode| {
            let (hdisplay, vdisplay) = mode.size();
            let (hsync_start, hsync_end, htotal) = mode.hsync();
            let (vsync_start, vsync_end, vtotal) = mode.vsync();
            PhysicalCatalogMode {
                mode: *mode,
                timing: DisplayMode {
                    clock: mode.clock(),
                    hdisplay,
                    htotal,
                    hsync_end,
                    hsync_start,
                    vtotal,
                    vdisplay,
                    vsync_end,
                    vsync_start,
                    flags: mode.flags().bits(),
                },
            }
        })
        .collect();

    let native_mode = physical_catalog_modes[preferred_mode_index].timing.clone();

    let portrait_modes = [
        (900_u16, 1900_u16),
        (810_u16, 1710_u16),
        (720_u16, 1520_u16),
    ];
    let mut synthetic_modes = Vec::new();
    for (w, h) in portrait_modes {
        if !physical_catalog_modes
            .iter()
            .any(|candidate| candidate.timing.hdisplay == w && candidate.timing.vdisplay == h)
        {
            synthetic_modes.push(derive_mode_from_native(&native_mode, w, h));
        }
    }
    let route_catalog = RouteCatalog::build(
        0,
        &physical_catalog_modes,
        preferred_mode_index,
        &synthetic_modes,
    )
    .context("build complete-timing route catalog")?;
    let advertised_modes = route_catalog.advertised_modes();
    info!(
        connector = route_catalog.connector(),
        entries = route_catalog.entries().len(),
        preferred = ?route_catalog.preferred_entry().key,
        exact_routes = route_catalog
            .entries()
            .iter()
            .filter(|entry| matches!(entry.route, CatalogRoute::Exact(_)))
            .count(),
        scaled_routes = route_catalog
            .entries()
            .iter()
            .filter(|entry| entry.route == CatalogRoute::ScaledFallback)
            .count(),
        "Built normalized complete-timing route catalog"
    );
    let logical_memory = logical_memory_estimate(
        connector_modes.iter().map(|mode| {
            let (width, height) = mode.size();
            (width.into(), height.into())
        }),
        advertised_modes
            .iter()
            .map(|mode| (mode.hdisplay.into(), mode.vdisplay.into())),
    )
    .context("calculate catalog-derived logical memory estimate")?;
    info!(
        logical_scanout_estimate_bytes = logical_memory.scanout_bytes,
        max_shadow_bytes = logical_memory.max_shadow_bytes,
        logical_peak_estimate_bytes = logical_memory.peak_bytes,
        "Catalog-derived RGB565 memory estimate; actual DRM pitch/mapping lengths are recorded \
         per allocation"
    );
    let mut shadow = if dynamic_mode_match {
        let shadow = ShadowFramebuffer::preallocated(logical_memory.max_shadow_bytes)
            .context("preallocate dynamic-mode fallback shadow before UDC bind")?;
        info!(
            max_shadow_bytes = logical_memory.max_shadow_bytes,
            "Preallocated maximum RGB565 fallback shadow before UDC bind"
        );
        shadow
    } else {
        ShadowFramebuffer::default()
    };
    gud_gadget::configure_state_check_validation(
        1,
        &[transfer_format.gud_pixel_format()],
        &advertised_modes,
    );

    info!(
        width = mode.size().0,
        height = mode.size().1,
        test_override = test_output_mode.is_some(),
        dynamic_mode_match,
        ?mode,
        "Picked deterministic physical DRM startup/fallback mode"
    );

    let mut backend = DrmScanoutBackend {
        card,
        crtc: crtc.handle(),
        connector: connector_handle,
    };
    let mut scanout_manager =
        ScanoutManager::new_baseline(&mut backend, mode).context("create baseline scanout")?;
    let baseline_timing = physical_catalog_modes
        .iter()
        .find(|physical| physical.mode == mode)
        .context("selected startup mode is absent from physical catalog")?;
    if dynamic_mode_match {
        scanout_manager
            .initialize_dynamic_routes(ModeKey::new(0, &baseline_timing.timing))
            .context("initialize dynamic route state")?;
    }
    let scanout_counters = scanout_manager.counters();
    let (scanout_slots, mut scanout_runtime) = scanout_manager.split_runtime();
    let [slot0, slot1, slot2] = scanout_slots;
    let baseline = slot0
        .as_deref_mut()
        .context("baseline scanout slot is empty")?;
    debug!(
        width = baseline.size().0,
        height = baseline.size().1,
        pitch = baseline.pitch(),
        mapping_lengths = ?baseline.mapping_lengths(),
        live_bytes = baseline.live_bytes(),
        "Created owned baseline double-buffer scanout"
    );
    let mut active = baseline
        .map(&mut backend, &scanout_counters)
        .context("map baseline scanout")?;

    for attempt in 1..=CRTC_SET_RETRIES {
        match backend.set_crtc(active.front_framebuffer(), mode) {
            Ok(()) => break,
            Err(err) if attempt < CRTC_SET_RETRIES => {
                warn!(
                    attempt,
                    retries = CRTC_SET_RETRIES,
                    error = ?err,
                    "set_crtc not ready; retrying before exposing the USB gadget"
                );
                std::thread::sleep(CRTC_SET_RETRY_DELAY);
            }
            Err(err) => return Err(err).context("Could not set CRTC after retries"),
        }
    }
    info!("CRTC set, display should be active");

    let (panel_width, panel_height) = active.size();
    let pitch = active.pitch();
    let width = panel_width;
    let height = panel_height;

    for index in 0..2 {
        let fb_data = active.buffer_mut(index);
        if pattern_mode.uses_startup_pattern() {
            fill_diagnostic_pattern_rect(
                fb_data,
                pitch as usize,
                width,
                height,
                0,
                0,
                width,
                height,
            )?;
        } else {
            render_waiting_screen(fb_data, pitch as usize, width, height)?;
        }
    }
    if pattern_mode.uses_startup_pattern() {
        info!("Filled both framebuffers with diagnostic startup pattern");
    } else {
        info!("Filled both framebuffers with waiting screen");
    }
    let mut waiting_screen_visible = matches!(pattern_mode, PatternMode::Off);

    match backend.dirty_framebuffer(
        active.front_framebuffer(),
        0,
        0,
        width as u16,
        height as u16,
    ) {
        Ok(()) => info!("Test pattern flushed to display"),
        Err(err) => warn!("Failed to flush test pattern: {}", err),
    }
    dump_framebuffer_if_enabled(
        dump_path.as_deref(),
        active.front_buffer_mut(),
        pitch as usize,
        width,
        height,
        2,
    );
    dump_framebuffer_raw_if_enabled(dump_raw_path.as_deref(), active.front_buffer_mut());

    // Keep the USB gadget disconnected until DRM has a working CRTC and the
    // initial framebuffer is ready. Otherwise a host can begin SET_BUFFER
    // while a transient DRM ownership failure tears the gadget down.
    let reg = Arc::new(
        Gadget::new(
            Class::interface_specific(),
            gud_gadget::OPENMOKO_GUD_ID,
            Strings::new("The Internet", "Generic USB Display", ""),
        )
        .with_config(Config::new("gud").with_function(gud_handle))
        .bind(&udc)
        .expect("UDC binding failed"),
    );
    info!("USB gadget bound to UDC");

    tracing::info!("Entering main event loop");
    let mut had_host_session = false;
    let mut restart_requested = false;
    let mut lifecycle_error = None;
    let unbind_target: Arc<dyn GadgetUnbind> = reg.clone();
    match gadget_shutdown.publish(&unbind_target) {
        Ok(GadgetUnbindOutcome::Armed) => {}
        Ok(GadgetUnbindOutcome::Unbound) => {
            info!("USB gadget unbound for shutdown immediately after bind")
        }
        Ok(GadgetUnbindOutcome::DeferredUntilPublish) | Ok(GadgetUnbindOutcome::AlreadyUnbound) => {
            unreachable!()
        }
        Err(err) => {
            running.store(false, Ordering::SeqCst);
            lifecycle_error = Some(
                anyhow::Error::new(err)
                    .context("failed to unbind USB gadget after an early shutdown request"),
            );
        }
    }
    drop(unbind_target);

    'event_loop: while running.load(Ordering::Relaxed) {
        if bulk_receive_session.current_state() == BulkReceiveState::Poisoned {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let event = match gud.event_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(err) => {
                tracing::error!("Failed to read GUD event: {}", err);
                if !running.load(Ordering::Acquire) {
                    tracing::info!("Control endpoint read cancelled for shutdown");
                    break 'event_loop;
                }
                if waiting_screen_visible || !matches!(pattern_mode, PatternMode::Off) {
                    continue;
                }
                if udc_is_detached(&udc) {
                    tracing::info!("Rendering waiting screen after event read failure");
                    if let Err(wait_err) = present_waiting_screen(
                        &mut backend,
                        &mut active,
                        dump_path.as_deref(),
                        dump_raw_path.as_deref(),
                    ) {
                        tracing::error!(
                            "Failed to render waiting screen after event read failure: {}",
                            wait_err
                        );
                    } else {
                        waiting_screen_visible = true;
                        if had_host_session {
                            tracing::info!(
                                "Restarting gadget after event read failure on detached UDC"
                            );
                            restart_requested = true;
                            break 'event_loop;
                        }
                    }
                }
                continue;
            }
        };
        if event.is_none() {
            if !waiting_screen_visible
                && matches!(pattern_mode, PatternMode::Off)
                && udc_is_detached(&udc)
            {
                tracing::info!("Rendering waiting screen after detach");
                if let Err(err) = present_waiting_screen(
                    &mut backend,
                    &mut active,
                    dump_path.as_deref(),
                    dump_raw_path.as_deref(),
                ) {
                    tracing::error!("Failed to render waiting screen after detach: {}", err);
                } else {
                    waiting_screen_visible = true;
                    if had_host_session {
                        tracing::info!("Restarting gadget after detach");
                        restart_requested = true;
                        break 'event_loop;
                    }
                }
            }
            continue;
        }
        let event = event.unwrap();
        tracing::debug!("Received event: {:?}", event);
        if !waiting_screen_visible
            && matches!(pattern_mode, PatternMode::Off)
            && udc_is_detached(&udc)
        {
            tracing::info!("Rendering waiting screen before processing stale queued event");
            if let Err(err) = present_waiting_screen(
                &mut backend,
                &mut active,
                dump_path.as_deref(),
                dump_raw_path.as_deref(),
            ) {
                tracing::error!(
                    "Failed to render waiting screen before processing queued event: {}",
                    err
                );
            } else {
                waiting_screen_visible = true;
                if had_host_session {
                    tracing::info!("Restarting gadget after stale queued event on detached UDC");
                    restart_requested = true;
                    break 'event_loop;
                }
            }
            continue;
        }

        let control_event_start = std::time::Instant::now();
        let parsed_gud_event = gud_gadget::event(event);
        let control_event_ms = control_event_start.elapsed().as_millis();
        match parsed_gud_event {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                record_host_activity(&mut had_host_session, &gud_event);
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(err) = req.send_descriptor_with_max_buffer_size(
                            min_width,
                            min_height,
                            max_width,
                            max_height,
                            descriptor_compression,
                            descriptor_max_buffer_size,
                        ) {
                            tracing::error!("Failed to send descriptor: {}", err);
                        } else {
                            tracing::debug!("Sent descriptor");
                        }
                    }
                    Event::GetPixelFormats(req) => {
                        if let Err(err) =
                            req.send_pixel_formats(&[transfer_format.gud_pixel_format()])
                        {
                            tracing::error!("Failed to send pixel formats: {}", err);
                        } else {
                            tracing::debug!("Sent pixel formats: {:?}", transfer_format);
                        }
                    }
                    Event::GetDisplayModes(req) => {
                        tracing::debug!("Sending {} display modes", advertised_modes.len());
                        if let Err(err) = req.send_modes(&advertised_modes) {
                            tracing::error!("Failed to send modes: {}", err);
                        } else {
                            tracing::debug!("Sent display modes");
                        }
                    }
                    Event::StateChecked(snapshot) => {
                        let catalog_route = route_catalog
                            .entry_for_snapshot(&snapshot)
                            .map(|entry| entry.route);
                        if dynamic_mode_match {
                            if let Err(err) = prepare_dynamic_check(
                                &mut backend,
                                &mut scanout_runtime,
                                slot1,
                                slot2,
                                &route_catalog,
                                snapshot,
                                control_event_ms,
                            ) {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to prepare dynamic state-check plan; optimization gate \
                                     must fail"
                                );
                            }
                        } else {
                            scanout_counters.no_op();
                            tracing::debug!(
                                generation = snapshot.generation,
                                mode = ?snapshot.mode,
                                ?catalog_route,
                                "State check notification ignored while dynamic matching is disabled"
                            );
                        }
                    }
                    Event::StateCommitted(snapshot) => {
                        tracing::debug!(
                            generation = snapshot.generation,
                            mode = ?snapshot.mode,
                            commit_control_ms = control_event_ms,
                            "State commit notification ignored while dynamic matching is disabled"
                        );
                    }
                    Event::ProtocolStateInvalidated {
                        generation,
                        reason:
                            reason @ (ProtocolInvalidationReason::InvalidStateCheck
                            | ProtocolInvalidationReason::CommitWithoutPending
                            | ProtocolInvalidationReason::Bind
                            | ProtocolInvalidationReason::Enable
                            | ProtocolInvalidationReason::Suspend
                            | ProtocolInvalidationReason::Resume),
                    } => {
                        if dynamic_mode_match {
                            if let Err(err) = invalidate_dynamic_pending_off_baseline(
                                &mut backend,
                                &mut scanout_runtime,
                                slot1,
                                slot2,
                            ) {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to release invalidated dynamic candidate"
                                );
                            }
                        }
                        tracing::debug!(
                            ?generation,
                            ?reason,
                            had_host_session,
                            "Protocol state invalidated"
                        );
                    }
                    Event::ProtocolStateInvalidated {
                        generation,
                        reason: ProtocolInvalidationReason::Disconnected,
                    } => {
                        if dynamic_mode_match {
                            if let Err(err) = invalidate_dynamic_pending_off_baseline(
                                &mut backend,
                                &mut scanout_runtime,
                                slot1,
                                slot2,
                            ) {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to release disconnected dynamic candidate"
                                );
                            }
                        }
                        tracing::info!(?generation, had_host_session, "Host disconnected");
                        if matches!(pattern_mode, PatternMode::Off) {
                            if let Err(err) = present_waiting_screen(
                                &mut backend,
                                &mut active,
                                dump_path.as_deref(),
                                dump_raw_path.as_deref(),
                            ) {
                                tracing::error!(
                                    "Failed to render waiting screen after disconnect: {}",
                                    err
                                );
                            } else {
                                waiting_screen_visible = true;
                                if had_host_session {
                                    tracing::info!("Restarting gadget after host disconnect");
                                    restart_requested = true;
                                    break 'event_loop;
                                }
                            }
                        }
                    }
                    Event::Buffer(info) => {
                        let frame_start = std::time::Instant::now();
                        tracing::debug!(
                            "Buffer: x={} y={} {}x{} len={} compression={}",
                            info.x,
                            info.y,
                            info.width,
                            info.height,
                            info.length,
                            info.compression
                        );
                        let clip = ClipRect::new(
                            info.x as u16,
                            info.y as u16,
                            (info.x + info.width) as u16,
                            (info.y + info.height) as u16,
                        );
                        let payload_stats = match bulk_receive_session
                            .receive(|| gud_data.recv_payload_bytes(&info))
                        {
                            Ok(result) => result,
                            Err(err) => {
                                if let Some(io_err) = err
                                    .chain()
                                    .find_map(|cause| cause.downcast_ref::<std::io::Error>())
                                {
                                    tracing::error!(
                                        error = ?err,
                                        error_chain = %format_args!("{err:#}"),
                                        io_error_kind = ?io_err.kind(),
                                        raw_os_error = ?io_err.raw_os_error(),
                                        "Failed to receive buffer payload"
                                    );
                                } else {
                                    tracing::error!(
                                        error = ?err,
                                        error_chain = %format_args!("{err:#}"),
                                        "Failed to receive buffer payload"
                                    );
                                }
                                if !running.load(Ordering::Acquire) {
                                    tracing::info!("Bulk endpoint read cancelled for shutdown");
                                    break 'event_loop;
                                }
                                tracing::error!(
                                    state = ?bulk_receive_session.current_state(),
                                    "FunctionFS bulk receive session is terminal; refusing all \
                                     further USB/control processing and automatic teardown. Do not \
                                     stop, restart, reboot, or shut down this service instance; \
                                     recover with a physical/hardware reset and collect \
                                     previous-boot evidence"
                                );
                                continue;
                            }
                        };
                        let (payload, payload_stats) = match gud_data.finish_payload(
                            &info,
                            transfer_format.bytes_per_pixel(),
                            payload_stats,
                        ) {
                            Ok(result) => result,
                            Err(err) => {
                                tracing::error!(
                                    error = ?err,
                                    error_chain = %format_args!("{err:#}"),
                                    "Failed to post-process received buffer payload; FunctionFS \
                                     receive session remains idle"
                                );
                                continue;
                            }
                        };

                        let active_state = gud_gadget::active_scanout_state();
                        let source_width =
                            active_state.map(|state| state.width).unwrap_or(panel_width);
                        let source_height = active_state
                            .map(|state| state.height)
                            .unwrap_or(panel_height);
                        let scaled_mode =
                            source_width != panel_width || source_height != panel_height;
                        let mut copy_ms = 0u128;
                        let mut scale_ms = 0u128;
                        let framebuffer_changed = match pattern_mode {
                            PatternMode::Off | PatternMode::Startup => {
                                if scaled_mode {
                                    if let Err(err) = shadow.ensure_size(ShadowRasterIdentity {
                                        width: source_width,
                                        height: source_height,
                                        format: transfer_format.gud_pixel_format(),
                                    }) {
                                        tracing::error!(
                                            "Failed to activate source shadow framebuffer: {}",
                                            err
                                        );
                                        continue;
                                    }
                                    let copy_result = match transfer_format {
                                        TransferFormat::Rgb565 => match gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer_with_stats(
                                            &info,
                                            payload,
                                            shadow.pixels.as_mut_slice(),
                                            shadow.pitch,
                                            2,
                                        ) {
                                            Ok(stats) => {
                                                copy_ms = stats.copy_ms;
                                                Ok(())
                                            }
                                            Err(err) => Err(err),
                                        },
                                        TransferFormat::Rgb888 => {
                                            let copy_start = std::time::Instant::now();
                                            let result = copy_rgb888_to_rgb565_framebuffer(
                                                &info,
                                                payload,
                                                shadow.pixels.as_mut_slice(),
                                                shadow.pitch,
                                            );
                                            copy_ms = copy_start.elapsed().as_millis();
                                            result
                                        }
                                    };
                                    match copy_result {
                                        Ok(()) => {}
                                        Err(err) => {
                                            tracing::error!(
                                                "Failed to copy buffer to shadow framebuffer: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }

                                    match scale_rgb565_to_fit(
                                        shadow.pixels.as_slice(),
                                        shadow.pitch,
                                        source_width,
                                        source_height,
                                        active.back_buffer_mut(),
                                        pitch as usize,
                                        panel_width,
                                        panel_height,
                                    ) {
                                        Ok((_layout, elapsed_ms)) => {
                                            scale_ms = elapsed_ms;
                                            true
                                        }
                                        Err(err) => {
                                            tracing::error!(
                                                "Failed to scale shadow framebuffer to panel: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }
                                } else {
                                    let copy_result = match transfer_format {
                                        TransferFormat::Rgb565 => match gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer_with_stats(
                                            &info,
                                            payload,
                                            active.front_buffer_mut(),
                                            pitch as usize,
                                            2,
                                        ) {
                                            Ok(stats) => {
                                                copy_ms = stats.copy_ms;
                                                Ok(())
                                            }
                                            Err(err) => Err(err),
                                        },
                                        TransferFormat::Rgb888 => {
                                            let copy_start = std::time::Instant::now();
                                            let result = copy_rgb888_to_rgb565_framebuffer(
                                                &info,
                                                payload,
                                                active.front_buffer_mut(),
                                                pitch as usize,
                                            );
                                            copy_ms = copy_start.elapsed().as_millis();
                                            result
                                        }
                                    };
                                    match copy_result {
                                        Ok(()) => true,
                                        Err(err) => {
                                            tracing::error!(
                                                "Failed to copy buffer to framebuffer: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }
                                }
                            }
                            PatternMode::Hold => {
                                tracing::debug!("Discarded payload in hold mode");
                                false
                            }
                            PatternMode::Usb => {
                                match fill_diagnostic_pattern_rect(
                                    active.front_buffer_mut(),
                                    pitch as usize,
                                    width,
                                    height,
                                    info.x,
                                    info.y,
                                    info.width,
                                    info.height,
                                ) {
                                    Ok(()) => true,
                                    Err(err) => {
                                        tracing::error!(
                                            "Failed to render diagnostic pattern for update rect: {}",
                                            err
                                        );
                                        continue;
                                    }
                                }
                            }
                        };

                        let mut flush_ms = 0u128;
                        if framebuffer_changed {
                            let flush_start = std::time::Instant::now();
                            if scaled_mode
                                && matches!(pattern_mode, PatternMode::Off | PatternMode::Startup)
                            {
                                match backend.dirty_framebuffer(
                                    active.back_framebuffer(),
                                    0,
                                    0,
                                    panel_width as u16,
                                    panel_height as u16,
                                ) {
                                    Ok(()) => tracing::debug!("Back buffer flushed before flip"),
                                    Err(err) => tracing::debug!(
                                        "dirty_framebuffer on back buffer not supported or failed: {}",
                                        err
                                    ),
                                }

                                let present_result = backend
                                    .page_flip(active.back_framebuffer())
                                    .or_else(|page_flip_err| {
                                        tracing::warn!(
                                            "Page flip failed ({}), falling back to set_crtc",
                                            page_flip_err
                                        );
                                        backend.set_crtc(active.back_framebuffer(), mode)
                                    });

                                match present_result {
                                    Ok(()) => {
                                        let back_buffer_index = active.back_index();
                                        active.set_front_index(back_buffer_index);
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!(
                                            "Scaled framebuffer presented via back-buffer swap"
                                        );
                                    }
                                    Err(err) => {
                                        tracing::error!(
                                            "Failed to present scaled framebuffer: {}",
                                            err
                                        );
                                        continue;
                                    }
                                }
                            } else {
                                match backend.dirty_framebuffer(
                                    active.front_framebuffer(),
                                    clip.x1(),
                                    clip.y1(),
                                    clip.x2(),
                                    clip.y2(),
                                ) {
                                    Ok(()) => {
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!("Framebuffer flushed")
                                    }
                                    Err(err) => {
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!(
                                            "dirty_framebuffer not supported or failed: {}",
                                            err
                                        )
                                    }
                                }
                            }
                        } else {
                            tracing::debug!("Framebuffer unchanged for current buffer event");
                        }

                        let total_ms = frame_start.elapsed().as_millis();
                        let compression_ratio = if payload_stats.transfer_bytes > 0 {
                            payload_stats.output_bytes as f64 / payload_stats.transfer_bytes as f64
                        } else {
                            0.0
                        };
                        let usb_mib_per_s = if payload_stats.read_ms > 0 {
                            (payload_stats.transfer_bytes as f64 / (1024.0 * 1024.0))
                                / (payload_stats.read_ms as f64 / 1000.0)
                        } else {
                            0.0
                        };
                        tracing::info!(
                            "frame_stats payload_seq={} rect={}x{}+{},{} source={}x{} scaled={} transfer_bytes={} output_bytes={} read_size={} read_calls={} first_request_bytes={} last_request_bytes={} usb_packets_est={} compression={} ratio={:.2} recv_ms={} decompress_ms={} copy_ms={} scale_ms={} flush_ms={} total_ms={} usb_mib_s={:.2}",
                            payload_stats.payload_seq,
                            info.width,
                            info.height,
                            info.x,
                            info.y,
                            source_width,
                            source_height,
                            scaled_mode,
                            payload_stats.transfer_bytes,
                            payload_stats.output_bytes,
                            payload_stats.read_size,
                            payload_stats.read_calls,
                            payload_stats.first_request_bytes,
                            payload_stats.last_request_bytes,
                            payload_stats.usb_packets_est,
                            info.compression,
                            compression_ratio,
                            payload_stats.read_ms,
                            payload_stats.decompress_ms,
                            copy_ms,
                            scale_ms,
                            flush_ms,
                            total_ms,
                            usb_mib_per_s
                        );

                        dump_framebuffer_if_enabled(
                            dump_path.as_deref(),
                            active.front_buffer_mut(),
                            pitch as usize,
                            width,
                            height,
                            2,
                        );
                        dump_framebuffer_raw_if_enabled(
                            dump_raw_path.as_deref(),
                            active.front_buffer_mut(),
                        );
                        if matches!(pattern_mode, PatternMode::Off) {
                            waiting_screen_visible = false;
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("Failed to parse GUD event: {}", err);
                if !waiting_screen_visible
                    && matches!(pattern_mode, PatternMode::Off)
                    && udc_is_detached(&udc)
                {
                    tracing::info!("Rendering waiting screen after control-path failure");
                    if let Err(wait_err) = present_waiting_screen(
                        &mut backend,
                        &mut active,
                        dump_path.as_deref(),
                        dump_raw_path.as_deref(),
                    ) {
                        tracing::error!(
                            "Failed to render waiting screen after control-path failure: {}",
                            wait_err
                        );
                    } else {
                        waiting_screen_visible = true;
                        if had_host_session {
                            tracing::info!("Restarting gadget after control-path failure");
                            restart_requested = true;
                            break 'event_loop;
                        }
                    }
                }
            }
        }
    }

    if should_restart_after_clean_detach(restart_requested, !running.load(Ordering::Acquire)) {
        if let Err(state) = bulk_receive_session.begin_shutdown() {
            tracing::error!(
                ?state,
                "Refusing automatic detach teardown because the FunctionFS bulk session is not \
                 idle; do not stop, restart, reboot, or shut down this service instance, recover \
                 with a physical/hardware reset"
            );
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        info!("Idle FunctionFS bulk session claimed for safe detach teardown");
    }

    tracing::info!("Shutting down");

    match gadget_shutdown.request_unbind() {
        Ok(GadgetUnbindOutcome::Unbound) => info!("USB gadget unbound from UDC"),
        Ok(GadgetUnbindOutcome::AlreadyUnbound) => {
            debug!("USB gadget already unbound from UDC")
        }
        Ok(GadgetUnbindOutcome::DeferredUntilPublish) | Ok(GadgetUnbindOutcome::Armed) => {
            unreachable!()
        }
        Err(err) => {
            tracing::error!("Failed to unbind USB gadget from UDC: {}", err);
            if lifecycle_error.is_none() {
                lifecycle_error =
                    Some(anyhow::Error::new(err).context("failed to unbind USB gadget from UDC"));
            }
        }
    }

    gadget_shutdown.clear();
    let remove_result = match Arc::try_unwrap(reg) {
        Ok(reg) => reg
            .remove()
            .map_err(anyhow::Error::new)
            .context("failed to remove USB gadget and close FunctionFS"),
        Err(reg) => {
            drop(reg);
            Err(anyhow::anyhow!(
                "USB gadget still had an unexpected strong owner during shutdown"
            ))
        }
    };
    match remove_result {
        Ok(()) => info!("USB gadget removed and FunctionFS teardown completed"),
        Err(err) => {
            tracing::error!("{:#}", err);
            if lifecycle_error.is_none() {
                lifecycle_error = Some(err);
            }
        }
    }

    drop(gud_data);
    drop(gud);
    info!("FunctionFS endpoint owners dropped; DRM release follows");
    drop(active);
    drop(scanout_runtime);
    if let Err(err) = scanout_manager.release_all(&mut backend) {
        tracing::error!("Failed to explicitly release DRM scanout resources: {err:#}");
        if lifecycle_error.is_none() {
            lifecycle_error = Some(err.context("explicit DRM scanout release failed"));
        }
    } else {
        info!(
            counters = ?scanout_manager.counter_snapshot(),
            "Explicit DRM framebuffer removal and dumb-buffer destruction completed"
        );
    }

    if let Some(err) = lifecycle_error {
        return Err(err);
    }

    let shutdown_requested = !running.load(Ordering::Acquire);
    if restart_requested && shutdown_requested {
        info!("Intentional shutdown supersedes queued detach restart request");
    }
    if should_restart_after_clean_detach(restart_requested, shutdown_requested) {
        info!(
            "USB detached after active host session; exiting successfully for policy-controlled \
             gadget recreation"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        advertised_preferred_mode_index, compute_scaled_layout, derive_mode_from_native,
        parse_functionfs_read_size, parse_test_compression, parse_test_dynamic_mode_match,
        parse_test_max_buffer_size, parse_test_output_mode, record_host_activity,
        render_waiting_screen, should_restart_after_clean_detach, validate_test_mode_policy,
        waiting_scene_glyph, BulkReceiveSession, BulkReceiveState, DisplayMode, GadgetShutdown,
        GadgetUnbind, GadgetUnbindOutcome, ScaledLayout, ShadowActivation, ShadowFramebuffer,
        ShadowRasterIdentity, TestOutputMode, RGB565_BLACK, RGB565_GREEN, RGB565_WHITE,
    };
    use drm::control::ModeTypeFlags;
    use gud_gadget::{
        DisplayStateSnapshot, Event, ProtocolInvalidationReason, GUD_DISPLAY_MODE_FLAG_PREFERRED,
        GUD_PIXEL_FORMAT_RGB565,
    };
    use std::ffi::OsStr;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn functionfs_read_size_parser_uses_default_and_accepts_numeric_override() {
        assert_eq!(
            parse_functionfs_read_size(None).unwrap(),
            gud_gadget::DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE
        );
        assert_eq!(
            parse_functionfs_read_size(Some(OsStr::new("16384"))).unwrap(),
            16_384
        );
        assert!(parse_functionfs_read_size(Some(OsStr::new("not-a-size"))).is_err());
    }

    #[test]
    fn test_compression_parser_preserves_default_and_accepts_named_values() {
        assert_eq!(parse_test_compression(None).unwrap(), 1);
        assert_eq!(parse_test_compression(Some(OsStr::new("lz4"))).unwrap(), 1);
        assert_eq!(parse_test_compression(Some(OsStr::new("none"))).unwrap(), 0);
        assert!(parse_test_compression(Some(OsStr::new("off"))).is_err());
    }

    #[test]
    fn test_max_buffer_size_parser_accepts_positive_u32_only() {
        assert_eq!(parse_test_max_buffer_size(None).unwrap(), None);
        assert_eq!(
            parse_test_max_buffer_size(Some(OsStr::new("64000"))).unwrap(),
            Some(64_000)
        );
        assert!(parse_test_max_buffer_size(Some(OsStr::new("0"))).is_err());
        assert!(parse_test_max_buffer_size(Some(OsStr::new("-1"))).is_err());
        assert!(parse_test_max_buffer_size(Some(OsStr::new("not-a-size"))).is_err());
    }

    #[test]
    fn dynamic_mode_match_parser_accepts_exact_test_value_only() {
        assert!(!parse_test_dynamic_mode_match(None).unwrap());
        assert!(parse_test_dynamic_mode_match(Some(OsStr::new("1"))).unwrap());
        for value in ["", "0", "true", "yes", "01", "1\n"] {
            assert!(
                parse_test_dynamic_mode_match(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn test_output_mode_parser_preserves_default_and_accepts_exact_dimensions() {
        assert_eq!(parse_test_output_mode(None).unwrap(), None);
        assert_eq!(
            parse_test_output_mode(Some(OsStr::new("1280x720"))).unwrap(),
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        );
    }

    #[test]
    fn test_output_mode_parser_rejects_ambiguous_or_invalid_values() {
        for value in ["1280", "1280X720", "1280x720x60", "0x720", "1280x0"] {
            assert!(
                parse_test_output_mode(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn dynamic_and_fixed_output_test_policies_are_mutually_exclusive() {
        assert!(validate_test_mode_policy(false, None).is_ok());
        assert!(validate_test_mode_policy(
            false,
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        )
        .is_ok());
        assert!(validate_test_mode_policy(true, None).is_ok());
        assert!(validate_test_mode_policy(
            true,
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        )
        .is_err());
    }

    #[test]
    fn advertised_preferred_mode_falls_back_to_first_connector_mode() {
        assert_eq!(
            advertised_preferred_mode_index([
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER,
            ]),
            0
        );
    }

    #[test]
    fn advertised_preferred_mode_uses_explicit_drm_preference() {
        assert_eq!(
            advertised_preferred_mode_index([
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER | ModeTypeFlags::PREFERRED,
                ModeTypeFlags::DRIVER,
            ]),
            1
        );
    }

    #[test]
    fn failed_bulk_receive_poison_prevents_a_second_attempt() {
        let session = BulkReceiveSession::default();
        let attempts = AtomicUsize::new(0);

        let first: anyhow::Result<()> = session.receive(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("injected receive failure"))
        });
        assert!(first.is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));

        let second = session.receive(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(second.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(second.unwrap_err().to_string().contains("fresh Pi boot"));
    }

    #[test]
    fn in_flight_bulk_receive_refuses_concurrent_unbind() {
        let session = BulkReceiveSession::default();
        let worker_session = session.clone();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            worker_session.receive(|| {
                worker_entered.wait();
                worker_release.wait();
                Ok(())
            })
        });

        entered.wait();
        assert_eq!(session.current_state(), BulkReceiveState::InFlight);
        let unbind_calls = AtomicUsize::new(0);
        if session.begin_shutdown().is_ok() {
            unbind_calls.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(unbind_calls.load(Ordering::SeqCst), 0);
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));

        release.wait();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn late_bulk_receive_is_poisoned_at_safety_threshold() {
        let session = BulkReceiveSession::with_deadline(Duration::from_millis(1));

        let result = session.receive(|| {
            std::thread::sleep(Duration::from_millis(5));
            Ok(())
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("safety deadline"));
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn slow_failed_post_read_work_does_not_poison_idle_session() {
        let session = BulkReceiveSession::default();
        session.receive(|| Ok(())).unwrap();

        std::thread::sleep(Duration::from_millis(5));
        let post_read_result: anyhow::Result<()> =
            Err(anyhow::anyhow!("injected post-read failure"));

        assert!(post_read_result.is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Idle);
        assert!(session.begin_shutdown().is_ok());
    }

    #[test]
    fn idle_bulk_session_can_be_claimed_for_shutdown_once() {
        let session = BulkReceiveSession::default();

        assert!(session.begin_shutdown().is_ok());
        assert_eq!(session.current_state(), BulkReceiveState::ShuttingDown);
        assert_eq!(
            session.begin_shutdown(),
            Err(BulkReceiveState::ShuttingDown)
        );
    }

    #[test]
    fn intentional_shutdown_suppresses_clean_detach_restart() {
        assert!(!should_restart_after_clean_detach(true, true));
    }

    #[test]
    fn safe_detach_requests_policy_controlled_restart() {
        assert!(should_restart_after_clean_detach(true, false));
        assert!(!should_restart_after_clean_detach(false, false));
    }

    #[test]
    fn lifecycle_only_events_do_not_create_a_host_session() {
        let mut had_host_session = false;
        for reason in [
            ProtocolInvalidationReason::Bind,
            ProtocolInvalidationReason::Enable,
            ProtocolInvalidationReason::Suspend,
            ProtocolInvalidationReason::Resume,
        ] {
            record_host_activity(
                &mut had_host_session,
                &Event::ProtocolStateInvalidated {
                    generation: None,
                    reason,
                },
            );
        }
        assert!(!had_host_session);

        record_host_activity(
            &mut had_host_session,
            &Event::StateChecked(DisplayStateSnapshot {
                mode: native_mode(),
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
                generation: 1,
            }),
        );
        assert!(had_host_session);
    }

    #[derive(Default)]
    struct MockGadget {
        unbind_calls: AtomicUsize,
    }

    impl GadgetUnbind for MockGadget {
        fn unbind(&self) -> io::Result<()> {
            self.unbind_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailOnceGadget {
        unbind_calls: AtomicUsize,
    }

    impl GadgetUnbind for FailOnceGadget {
        fn unbind(&self) -> io::Result<()> {
            if self.unbind_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(io::Error::other("injected first unbind failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn shutdown_unbinds_published_gadget_once() {
        let shutdown = GadgetShutdown::default();
        let gadget = Arc::new(MockGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();

        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Armed
        );
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::AlreadyUnbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_unbind_remains_retryable() {
        let shutdown = GadgetShutdown::default();
        let gadget = Arc::new(FailOnceGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();

        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Armed
        );
        assert!(shutdown.request_unbind().is_err());
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn shutdown_before_publish_unbinds_as_soon_as_gadget_is_available() {
        let shutdown = GadgetShutdown::default();

        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::DeferredUntilPublish
        );

        let gadget = Arc::new(MockGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();
        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 1);
    }

    fn native_mode() -> DisplayMode {
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
            flags: GUD_DISPLAY_MODE_FLAG_PREFERRED,
        }
    }

    #[test]
    fn compute_scaled_layout_preserves_aspect_ratio_for_4_3_mode() {
        assert_eq!(
            compute_scaled_layout(1024, 768, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 0,
                dst_y: 735,
                dst_width: 1080,
                dst_height: 810,
            }
        );
    }

    #[test]
    fn compute_scaled_layout_keeps_native_mode_fullscreen() {
        assert_eq!(
            compute_scaled_layout(1080, 2280, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 0,
                dst_y: 0,
                dst_width: 1080,
                dst_height: 2280,
            }
        );
    }

    #[test]
    fn compute_scaled_layout_centers_tall_source() {
        assert_eq!(
            compute_scaled_layout(720, 2280, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 180,
                dst_y: 0,
                dst_width: 720,
                dst_height: 2280,
            }
        );
    }

    #[test]
    fn derive_mode_from_native_preserves_portrait_shape() {
        let derived = derive_mode_from_native(&native_mode(), 720, 1520);

        assert_eq!(derived.hdisplay, 720);
        assert_eq!(derived.vdisplay, 1520);
        assert!(derived.hsync_start > derived.hdisplay);
        assert!(derived.hsync_end > derived.hsync_start);
        assert!(derived.htotal > derived.hsync_end);
        assert!(derived.vsync_start > derived.vdisplay);
        assert!(derived.vsync_end > derived.vsync_start);
        assert!(derived.vtotal > derived.vsync_end);
        assert_eq!(derived.flags, 0);
    }

    #[test]
    fn preallocated_shadow_preserves_identity_and_zeros_only_when_required() {
        let mut shadow = ShadowFramebuffer::preallocated(32).unwrap();
        let identity = ShadowRasterIdentity {
            width: 4,
            height: 4,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Zeroed
        );
        shadow.pixels[0..4].copy_from_slice(&[1, 2, 3, 4]);

        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Preserved
        );
        assert_eq!(&shadow.pixels[0..4], &[1, 2, 3, 4]);

        shadow.invalidate_content();
        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Zeroed
        );
        assert_eq!(&shadow.pixels[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn shadow_format_or_geometry_change_zeros_once_without_reallocation() {
        let mut shadow = ShadowFramebuffer::preallocated(64).unwrap();
        let original_ptr = shadow.pixels.as_ptr();
        let rgb565 = ShadowRasterIdentity {
            width: 4,
            height: 4,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        shadow.activate_scaled(rgb565).unwrap();
        shadow.pixels[0] = 0xff;

        let format_change = ShadowRasterIdentity {
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB888,
            ..rgb565
        };
        assert_eq!(
            shadow.activate_scaled(format_change).unwrap(),
            ShadowActivation::Zeroed
        );
        shadow.pixels[0] = 0xaa;

        let geometry_change = ShadowRasterIdentity {
            width: 2,
            height: 2,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        assert_eq!(
            shadow.activate_scaled(geometry_change).unwrap(),
            ShadowActivation::Zeroed
        );
        assert_eq!(shadow.pixels[0], 0);
        assert_eq!(shadow.pixels.as_ptr(), original_ptr);
    }

    #[test]
    fn maximum_shadow_allocation_and_capacity_fail_before_use() {
        assert!(ShadowFramebuffer::preallocated(usize::MAX).is_err());
        let mut shadow = ShadowFramebuffer::preallocated(8).unwrap();
        assert!(shadow
            .activate_scaled(ShadowRasterIdentity {
                width: 3,
                height: 2,
                format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
            })
            .is_err());
    }

    #[test]
    fn waiting_scene_supports_all_glyphs() {
        for line in super::WAITING_SCREEN_LINES {
            for ch in line.chars() {
                assert!(
                    waiting_scene_glyph(ch).is_some(),
                    "missing glyph for {:?}",
                    ch
                );
            }
        }
    }

    #[test]
    fn waiting_screen_contains_black_white_and_green_pixels() {
        let width = 320usize;
        let height = 240usize;
        let pitch = width * 2;
        let mut fb = vec![0u8; pitch * height];

        render_waiting_screen(&mut fb, pitch, width as u32, height as u32).unwrap();

        let mut has_black = false;
        let mut has_white = false;
        let mut has_green = false;
        for chunk in fb.chunks_exact(2) {
            let pixel = u16::from_le_bytes([chunk[0], chunk[1]]);
            has_black |= pixel == RGB565_BLACK;
            has_white |= pixel == RGB565_WHITE;
            has_green |= pixel == RGB565_GREEN;
        }

        assert!(has_black);
        assert!(has_white);
        assert!(has_green);
    }
}

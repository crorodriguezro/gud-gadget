use anyhow::{ensure, Context};
use drm::buffer::Buffer;
use drm::control::{
    dumbbuffer::DumbMapping, framebuffer, ClipRect, Device, Mode, ModeTypeFlags, PageFlipFlags,
};
use gud_gadget::{DisplayMode, Event, GUD_COMPRESSION_LZ4};
use std::env::{args, var_os};
use std::fs::{rename, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use usb_gadget::function::custom::{Custom, Interface};
use usb_gadget::{default_udc, Class, Config, Gadget, Strings, Udc, UdcState};

const CRTC_SET_RETRIES: usize = 20;
const CRTC_SET_RETRY_DELAY: Duration = Duration::from_millis(250);

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

fn gud_flags_for_mode(mode: &Mode, preferred_mode_name: &std::ffi::CStr) -> u32 {
    let mut flags = mode.flags().bits();
    if mode.mode_type().contains(ModeTypeFlags::PREFERRED) || mode.name() == preferred_mode_name {
        flags |= gud_gadget::GUD_DISPLAY_MODE_FLAG_PREFERRED;
    }
    flags
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

fn present_waiting_screen(
    card: &mut Card,
    mappings: &mut [DumbMapping<'_>; 2],
    pitch: usize,
    width: u32,
    height: u32,
    fb_handles: &[framebuffer::Handle; 2],
    front_buffer_index: usize,
    dump_path: Option<&Path>,
    dump_raw_path: Option<&Path>,
) -> anyhow::Result<()> {
    for mapping in mappings.iter_mut() {
        render_waiting_screen(mapping.as_mut(), pitch, width, height)?;
    }

    let full_panel = ClipRect::new(0, 0, width as u16, height as u16);
    match card.dirty_framebuffer(fb_handles[front_buffer_index], &[full_panel]) {
        Ok(()) => tracing::debug!("Waiting screen flushed"),
        Err(err) => tracing::debug!("dirty_framebuffer for waiting screen failed: {}", err),
    }

    dump_framebuffer_if_enabled(
        dump_path,
        mappings[front_buffer_index].as_mut(),
        pitch,
        width,
        height,
        2,
    );
    dump_framebuffer_raw_if_enabled(dump_raw_path, mappings[front_buffer_index].as_mut());

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

#[derive(Debug, Default)]
struct ShadowFramebuffer {
    width: u32,
    height: u32,
    pitch: usize,
    pixels: Vec<u8>,
}

impl ShadowFramebuffer {
    fn ensure_size(&mut self, width: u32, height: u32) {
        let pitch = width as usize * 2;
        let len = pitch * height as usize;
        if self.width != width || self.height != height || self.pixels.len() != len {
            self.width = width;
            self.height = height;
            self.pitch = pitch;
            self.pixels = vec![0; len];
        }
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

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    info!("gud-drm starting (working baseline)");

    let card_path = args()
        .skip(1)
        .next()
        .expect("specify full path to /dev/dri/cardN as program argument");
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
    info!("Opening DRM device: {}", card_path);
    let mut card = Card::open(&card_path);
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

    let mut min_width = u32::MAX;
    let mut min_height = u32::MAX;
    let mut max_width = 0;
    let mut max_height = 0;
    for mode in connector.modes() {
        let (width, height) = mode.size();
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

    usb_gadget::remove_all().expect("UDC init failed");
    info!("USB gadgets removed");

    let (mut gud_data, gud_data_ep) = gud_gadget::PixelDataEndpoint::new();
    info!("Created pixel data endpoint");

    let mut builder = Custom::builder().with_interface(
        Interface::new(Class::vendor_specific(Class::VENDOR_SPECIFIC, 0), "GUD")
            .with_endpoint(gud_data_ep),
    );
    builder.ffs_no_disconnect = true;
    let (mut gud, gud_handle) = builder.build();
    info!("Built USB gadget");

    let running = Arc::new(AtomicBool::new(true));

    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .expect("cleanup handler registration failed");

    let connector_modes = connector.modes();
    let mode = connector_modes.first().unwrap();
    let preferred_mode_name = connector_modes
        .iter()
        .find(|candidate| candidate.mode_type().contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(mode)
        .name()
        .to_owned();
    let mut advertised_modes: Vec<DisplayMode> = connector_modes
        .iter()
        .map(|mode| {
            let (hdisplay, vdisplay) = mode.size();
            let (hsync_start, hsync_end, htotal) = mode.hsync();
            let (vsync_start, vsync_end, vtotal) = mode.vsync();
            DisplayMode {
                clock: mode.clock(),
                hdisplay,
                htotal,
                hsync_end,
                hsync_start,
                vtotal,
                vdisplay,
                vsync_end,
                vsync_start,
                flags: gud_flags_for_mode(mode, preferred_mode_name.as_c_str()),
            }
        })
        .collect();

    let native_mode = advertised_modes
        .iter()
        .find(|candidate| candidate.flags & gud_gadget::GUD_DISPLAY_MODE_FLAG_PREFERRED != 0)
        .cloned()
        .unwrap_or_else(|| advertised_modes[0].clone());

    let portrait_modes = [
        (900_u16, 1900_u16),
        (810_u16, 1710_u16),
        (720_u16, 1520_u16),
    ];
    for (w, h) in portrait_modes {
        if !advertised_modes
            .iter()
            .any(|candidate| candidate.hdisplay == w && candidate.vdisplay == h)
        {
            advertised_modes.push(derive_mode_from_native(&native_mode, w, h));
        }
    }
    gud_gadget::configure_state_check_validation(
        1,
        &[transfer_format.gud_pixel_format()],
        &advertised_modes,
    );

    info!("picked mode {:?}", mode);

    let (width, height) = mode.size();
    debug!("Creating double dumb buffers {}x{} (RGB565)", width, height);
    let mut dumb_buffers = [
        card.create_dumb_buffer(
            (width.into(), height.into()),
            drm::buffer::DrmFourcc::Rgb565,
            16,
        )
        .expect("Could not create primary dumb buffer"),
        card.create_dumb_buffer(
            (width.into(), height.into()),
            drm::buffer::DrmFourcc::Rgb565,
            16,
        )
        .expect("Could not create secondary dumb buffer"),
    ];

    let fb_handles = [
        card.add_framebuffer(&dumb_buffers[0], 16, 16)
            .expect("Could not create primary FB"),
        card.add_framebuffer(&dumb_buffers[1], 16, 16)
            .expect("Could not create secondary FB"),
    ];
    debug!("Framebuffers created (RGB565, 16bpp)");

    for attempt in 1..=CRTC_SET_RETRIES {
        match card.set_crtc(
            crtc.handle(),
            Some(fb_handles[0]),
            (0, 0),
            &[connector.handle()],
            Some(*mode),
        ) {
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

    let pitch = dumb_buffers[0].pitch();
    ensure!(
        dumb_buffers[1].pitch() == pitch,
        "dumb buffer pitches differ: {} vs {}",
        pitch,
        dumb_buffers[1].pitch()
    );

    let (primary_buffer, secondary_buffer) = dumb_buffers.split_at_mut(1);
    let mut mappings = [
        card.map_dumb_buffer(&mut primary_buffer[0])
            .expect("map_dumb_buffer for primary failed"),
        card.map_dumb_buffer(&mut secondary_buffer[0])
            .expect("map_dumb_buffer for secondary failed"),
    ];
    debug!("Dumb buffer mapped, pitch={}", pitch);
    let panel_width = width as u32;
    let panel_height = height as u32;
    let mut shadow = ShadowFramebuffer::default();
    let mut front_buffer_index = 0usize;

    for mapping in mappings.iter_mut() {
        let fb_data = mapping.as_mut();
        if pattern_mode.uses_startup_pattern() {
            fill_diagnostic_pattern_rect(
                fb_data,
                pitch as usize,
                width.into(),
                height.into(),
                0,
                0,
                width.into(),
                height.into(),
            )?;
        } else {
            render_waiting_screen(fb_data, pitch as usize, width.into(), height.into())?;
        }
    }
    if pattern_mode.uses_startup_pattern() {
        info!("Filled both framebuffers with diagnostic startup pattern");
    } else {
        info!("Filled both framebuffers with waiting screen");
    }
    let mut waiting_screen_visible = matches!(pattern_mode, PatternMode::Off);

    let test_clip = ClipRect::new(0, 0, width as u16, height as u16);
    match card.dirty_framebuffer(fb_handles[front_buffer_index], &[test_clip]) {
        Ok(()) => info!("Test pattern flushed to display"),
        Err(err) => warn!("Failed to flush test pattern: {}", err),
    }
    dump_framebuffer_if_enabled(
        dump_path.as_deref(),
        mappings[front_buffer_index].as_mut(),
        pitch as usize,
        width.into(),
        height.into(),
        2,
    );
    dump_framebuffer_raw_if_enabled(
        dump_raw_path.as_deref(),
        mappings[front_buffer_index].as_mut(),
    );

    // Keep the USB gadget disconnected until DRM has a working CRTC and the
    // initial framebuffer is ready. Otherwise a host can begin SET_BUFFER
    // while a transient DRM ownership failure tears the gadget down.
    let _reg = Gadget::new(
        Class::interface_specific(),
        gud_gadget::OPENMOKO_GUD_ID,
        Strings::new("The Internet", "Generic USB Display", ""),
    )
    .with_config(Config::new("gud").with_function(gud_handle))
    .bind(&udc)
    .expect("UDC binding failed");
    if let Err(err) = udc.set_soft_connect(true) {
        warn!("Failed to assert USB soft-connect: {}", err);
    }
    info!("USB gadget bound to UDC");

    tracing::info!("Entering main event loop");
    let mut had_host_session = false;
    let mut restart_requested = false;

    'event_loop: while running.load(Ordering::Relaxed) {
        let event = match gud.event_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(err) => {
                tracing::error!("Failed to read GUD event: {}", err);
                if waiting_screen_visible || !matches!(pattern_mode, PatternMode::Off) {
                    continue;
                }
                if udc_is_detached(&udc) {
                    tracing::info!("Rendering waiting screen after event read failure");
                    if let Err(wait_err) = present_waiting_screen(
                        &mut card,
                        &mut mappings,
                        pitch as usize,
                        width.into(),
                        height.into(),
                        &fb_handles,
                        front_buffer_index,
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
                    &mut card,
                    &mut mappings,
                    pitch as usize,
                    width.into(),
                    height.into(),
                    &fb_handles,
                    front_buffer_index,
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
                &mut card,
                &mut mappings,
                pitch as usize,
                width.into(),
                height.into(),
                &fb_handles,
                front_buffer_index,
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

        match gud_gadget::event(event) {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                if !matches!(gud_event, Event::Disconnected) {
                    had_host_session = true;
                }
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(err) = req.send_descriptor(
                            min_width,
                            min_height,
                            max_width,
                            max_height,
                            GUD_COMPRESSION_LZ4,
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
                    Event::Disconnected => {
                        tracing::info!("Host disconnected");
                        if matches!(pattern_mode, PatternMode::Off) {
                            if let Err(err) = present_waiting_screen(
                                &mut card,
                                &mut mappings,
                                pitch as usize,
                                width.into(),
                                height.into(),
                                &fb_handles,
                                front_buffer_index,
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
                        let (payload, payload_stats) = match gud_data
                            .recv_payload(&info, transfer_format.bytes_per_pixel())
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
                                if !waiting_screen_visible
                                    && matches!(pattern_mode, PatternMode::Off)
                                    && udc_is_detached(&udc)
                                {
                                    tracing::info!(
                                        "Rendering waiting screen after bulk receive failure"
                                    );
                                    if let Err(wait_err) = present_waiting_screen(
                                        &mut card,
                                        &mut mappings,
                                        pitch as usize,
                                        width.into(),
                                        height.into(),
                                        &fb_handles,
                                        front_buffer_index,
                                        dump_path.as_deref(),
                                        dump_raw_path.as_deref(),
                                    ) {
                                        tracing::error!(
                                                "Failed to render waiting screen after bulk receive failure: {}",
                                                wait_err
                                            );
                                    } else {
                                        waiting_screen_visible = true;
                                        if had_host_session {
                                            tracing::info!(
                                                "Restarting gadget after bulk receive failure on detached UDC"
                                            );
                                            restart_requested = true;
                                            break 'event_loop;
                                        }
                                    }
                                }
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
                        let back_buffer_index = front_buffer_index ^ 1;
                        let mut copy_ms = 0u128;
                        let mut scale_ms = 0u128;
                        let framebuffer_changed = match pattern_mode {
                            PatternMode::Off | PatternMode::Startup => {
                                if scaled_mode {
                                    shadow.ensure_size(source_width, source_height);
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
                                        mappings[back_buffer_index].as_mut(),
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
                                            mappings[front_buffer_index].as_mut(),
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
                                                mappings[front_buffer_index].as_mut(),
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
                                    mappings[front_buffer_index].as_mut(),
                                    pitch as usize,
                                    width.into(),
                                    height.into(),
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
                                let full_panel =
                                    ClipRect::new(0, 0, panel_width as u16, panel_height as u16);
                                match card
                                    .dirty_framebuffer(fb_handles[back_buffer_index], &[full_panel])
                                {
                                    Ok(()) => tracing::debug!("Back buffer flushed before flip"),
                                    Err(err) => tracing::debug!(
                                        "dirty_framebuffer on back buffer not supported or failed: {}",
                                        err
                                    ),
                                }

                                let present_result = card
                                    .page_flip(
                                        crtc.handle(),
                                        fb_handles[back_buffer_index],
                                        PageFlipFlags::empty(),
                                        None,
                                    )
                                    .or_else(|page_flip_err| {
                                        tracing::warn!(
                                            "Page flip failed ({}), falling back to set_crtc",
                                            page_flip_err
                                        );
                                        card.set_crtc(
                                            crtc.handle(),
                                            Some(fb_handles[back_buffer_index]),
                                            (0, 0),
                                            &[connector.handle()],
                                            Some(*mode),
                                        )
                                    });

                                match present_result {
                                    Ok(()) => {
                                        front_buffer_index = back_buffer_index;
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
                                match card
                                    .dirty_framebuffer(fb_handles[front_buffer_index], &[clip])
                                {
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
                            "frame_stats rect={}x{}+{},{} source={}x{} scaled={} transfer_bytes={} output_bytes={} packets={} compression={} ratio={:.2} recv_ms={} decompress_ms={} copy_ms={} scale_ms={} flush_ms={} total_ms={} usb_mib_s={:.2}",
                            info.width,
                            info.height,
                            info.x,
                            info.y,
                            source_width,
                            source_height,
                            scaled_mode,
                            payload_stats.transfer_bytes,
                            payload_stats.output_bytes,
                            payload_stats.packets,
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
                            mappings[front_buffer_index].as_mut(),
                            pitch as usize,
                            width.into(),
                            height.into(),
                            2,
                        );
                        dump_framebuffer_raw_if_enabled(
                            dump_raw_path.as_deref(),
                            mappings[front_buffer_index].as_mut(),
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
                        &mut card,
                        &mut mappings,
                        pitch as usize,
                        width.into(),
                        height.into(),
                        &fb_handles,
                        front_buffer_index,
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

    tracing::info!("Shutting down");

    if restart_requested {
        return Err(anyhow::anyhow!(
            "USB detached after active host session; restart to recreate gadget"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        compute_scaled_layout, derive_mode_from_native, render_waiting_screen, waiting_scene_glyph,
        DisplayMode, ScaledLayout, RGB565_BLACK, RGB565_GREEN, RGB565_WHITE,
    };
    use gud_gadget::GUD_DISPLAY_MODE_FLAG_PREFERRED;

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

use anyhow::ensure;
use drm::buffer::Buffer;
use drm::control::{ClipRect, Device, Mode, ModeTypeFlags};
use gud_gadget::{DisplayMode, Event};
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
use usb_gadget::{default_udc, Class, Config, Gadget, Strings};

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
        !matches!(self, Self::Off)
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

    let crtc = resources
        .crtcs()
        .iter()
        .flat_map(|crtc| card.get_crtc(*crtc))
        .next()
        .expect("no crtc found");

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

    let (mut gud, gud_handle) = Custom::builder()
        .with_interface(
            Interface::new(Class::vendor_specific(Class::VENDOR_SPECIFIC, 0), "GUD")
                .with_endpoint(gud_data_ep),
        )
        .build();
    info!("Built USB gadget");

    let _reg = Gadget::new(
        Class::interface_specific(),
        gud_gadget::OPENMOKO_GUD_ID,
        Strings::new("The Internet", "Generic USB Display", ""),
    )
    .with_config(Config::new("gud").with_function(gud_handle))
    .bind(&udc)
    .expect("UDC binding failed");
    info!("USB gadget bound to UDC");

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

    info!("picked mode {:?}", mode);

    let (width, height) = mode.size();
    debug!("Creating dumb buffer {}x{} (RGB565)", width, height);
    let mut db = card
        .create_dumb_buffer(
            (width.into(), height.into()),
            drm::buffer::DrmFourcc::Rgb565,
            16,
        )
        .expect("Could not create dumb buffer");

    let fb_handle = card
        .add_framebuffer(&db, 16, 16)
        .expect("Could not create FB");
    debug!("Framebuffer created (RGB565, 16bpp)");

    card.set_crtc(
        crtc.handle(),
        Some(fb_handle),
        (0, 0),
        &[connector.handle()],
        Some(*mode),
    )
    .expect("Could not set CRTC");
    info!("CRTC set, display should be active");

    let pitch = db.pitch();

    let mut mapping = card
        .map_dumb_buffer(&mut db)
        .expect("map_dumb_buffer failed");
    debug!("Dumb buffer mapped, pitch={}", pitch);

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
        info!("Filled framebuffer with diagnostic startup pattern");
    } else {
        fill_rgb565_solid(
            fb_data,
            pitch as usize,
            width.into(),
            height.into(),
            RGB565_GREEN,
        );
        info!("Filled framebuffer with green test pattern");
    }

    let test_clip = ClipRect::new(0, 0, width as u16, height as u16);
    match card.dirty_framebuffer(fb_handle, &[test_clip]) {
        Ok(()) => info!("Test pattern flushed to display"),
        Err(err) => warn!("Failed to flush test pattern: {}", err),
    }
    dump_framebuffer_if_enabled(
        dump_path.as_deref(),
        mapping.as_mut(),
        pitch as usize,
        width.into(),
        height.into(),
        2,
    );
    dump_framebuffer_raw_if_enabled(dump_raw_path.as_deref(), mapping.as_mut());

    tracing::info!("Entering main event loop");

    while running.load(Ordering::Relaxed) {
        let event = match gud.event_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(err) => {
                tracing::error!("Failed to read GUD event: {}", err);
                continue;
            }
        };
        if event.is_none() {
            continue;
        }
        let event = event.unwrap();
        tracing::debug!("Received event: {:?}", event);

        match gud_gadget::event(event) {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(err) =
                            req.send_descriptor(min_width, min_height, max_width, max_height)
                        {
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
                        let mut modes: Vec<DisplayMode> = connector_modes
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

                        let standard_modes = [
                            (1024, 768, 65000, 1040, 1184, 1344, 771, 777, 806),
                            (800, 600, 40000, 832, 960, 1056, 601, 604, 628),
                            (640, 480, 25175, 656, 752, 800, 490, 492, 525),
                        ];
                        for (w, h, clock, hss, hse, ht, vss, vse, vt) in standard_modes {
                            if !modes
                                .iter()
                                .any(|candidate| candidate.hdisplay == w && candidate.vdisplay == h)
                            {
                                modes.push(DisplayMode {
                                    clock,
                                    hdisplay: w,
                                    htotal: ht,
                                    hsync_end: hse,
                                    hsync_start: hss,
                                    vtotal: vt,
                                    vdisplay: h,
                                    vsync_end: vse,
                                    vsync_start: vss,
                                    flags: 0,
                                });
                            }
                        }

                        tracing::debug!("Sending {} display modes", modes.len());
                        if let Err(err) = req.send_modes(&modes) {
                            tracing::error!("Failed to send modes: {}", err);
                        } else {
                            tracing::debug!("Sent display modes");
                        }
                    }
                    Event::Buffer(info) => {
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
                        let payload =
                            match gud_data.recv_payload(&info, transfer_format.bytes_per_pixel()) {
                                Ok(payload) => payload,
                                Err(err) => {
                                    tracing::error!("Failed to receive buffer payload: {}", err);
                                    continue;
                                }
                            };

                        let framebuffer_changed = match pattern_mode {
                            PatternMode::Off | PatternMode::Startup => {
                                let copy_result = match transfer_format {
                                    TransferFormat::Rgb565 => {
                                        gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer(
                                            &info,
                                            payload,
                                            mapping.as_mut(),
                                            pitch as usize,
                                            2,
                                        )
                                    }
                                    TransferFormat::Rgb888 => copy_rgb888_to_rgb565_framebuffer(
                                        &info,
                                        payload,
                                        mapping.as_mut(),
                                        pitch as usize,
                                    ),
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
                            PatternMode::Hold => {
                                tracing::debug!("Discarded payload in hold mode");
                                false
                            }
                            PatternMode::Usb => {
                                match fill_diagnostic_pattern_rect(
                                    mapping.as_mut(),
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

                        if framebuffer_changed {
                            match card.dirty_framebuffer(fb_handle, &[clip]) {
                                Ok(()) => tracing::debug!("Framebuffer flushed"),
                                Err(err) => {
                                    tracing::debug!(
                                        "dirty_framebuffer not supported or failed: {}",
                                        err
                                    )
                                }
                            }
                        } else {
                            tracing::debug!("Framebuffer unchanged for current buffer event");
                        }

                        dump_framebuffer_if_enabled(
                            dump_path.as_deref(),
                            mapping.as_mut(),
                            pitch as usize,
                            width.into(),
                            height.into(),
                            2,
                        );
                        dump_framebuffer_raw_if_enabled(dump_raw_path.as_deref(), mapping.as_mut());
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("Failed to parse GUD event: {}", err);
            }
        }
    }

    tracing::info!("Shutting down");

    Ok(())
}

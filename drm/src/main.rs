use drm::buffer::Buffer;
use drm::control::{ClipRect, Device, Mode, ModeTypeFlags};
use gud_gadget::{DisplayMode, Event};
use std::env::{args, var_os};
use std::fs::File;
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
/// A simple wrapper for a device node.
pub struct Card(std::fs::File);

/// Implementing `AsFd` is a prerequisite to implementing the traits found
/// in this crate. Here, we are just calling `as_fd()` on the inner File.
impl std::os::unix::io::AsFd for Card {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// With `AsFd` implemented, we can now implement `drm::Device`.
impl drm::Device for Card {}

impl Device for Card {}

/// Simple helper methods for opening a `Card`.
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

fn sync_scanout(
    card: &Card,
    crtc: drm::control::crtc::Handle,
    connector: drm::control::connector::Handle,
    fb_handle: drm::control::framebuffer::Handle,
    mode: Mode,
    scanout_active: &mut bool,
    should_be_active: bool,
) {
    if *scanout_active == should_be_active {
        return;
    }

    if let Err(err) = drm::Device::acquire_master_lock(card) {
        warn!("Failed to acquire DRM master lock: {}", err);
        return;
    }

    let result = if should_be_active {
        card.set_crtc(crtc, Some(fb_handle), (0, 0), &[connector], Some(mode))
    } else {
        card.set_crtc(crtc, None, (0, 0), &[], None)
    };

    match result {
        Ok(()) => {
            *scanout_active = should_be_active;
            info!(
                "Local scanout {}",
                if should_be_active {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }
        Err(err) => {
            warn!(
                "Failed to {} local scanout: {}",
                if should_be_active {
                    "enable"
                } else {
                    "disable"
                },
                err
            );
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

fn dump_rgb565_ppm(
    path: &Path,
    fb: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
) -> anyhow::Result<()> {
    let width = width as usize;
    let height = height as usize;
    let mut writer = BufWriter::new(File::create(path)?);
    write!(writer, "P6\n{} {}\n255\n", width, height)?;

    for y in 0..height {
        let row = &fb[(y * pitch)..(y * pitch + width * 2)];
        for x in 0..width {
            let offset = x * 2;
            let pixel = u16::from_le_bytes([row[offset], row[offset + 1]]);
            let red = ((pixel >> 11) & 0x1f) as u8;
            let green = ((pixel >> 5) & 0x3f) as u8;
            let blue = (pixel & 0x1f) as u8;
            let rgb = [
                (red << 3) | (red >> 2),
                (green << 2) | (green >> 4),
                (blue << 3) | (blue >> 2),
            ];
            writer.write_all(&rgb)?;
        }
    }

    writer.flush()?;
    Ok(())
}

fn dump_framebuffer_if_enabled(
    dump_path: Option<&Path>,
    fb: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
) {
    if let Some(path) = dump_path {
        if let Err(err) = dump_rgb565_ppm(path, fb, pitch, width, height) {
            warn!("Failed to dump framebuffer to {}: {}", path.display(), err);
        } else {
            debug!("Wrote framebuffer dump to {}", path.display());
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    info!("gud-drm starting (op6 branch)");

    let card_path = args()
        .skip(1)
        .next()
        .expect("specify full path to /dev/dri/cardN as program argument");
    let dump_path = var_os("GUD_DUMP_FB_PATH").map(PathBuf::from);
    if let Some(path) = dump_path.as_deref() {
        info!("Framebuffer dumps enabled: {}", path.display());
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

    // Include standard mode ranges for descriptor
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
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(mode)
        .name()
        .to_owned();

    info!("picked mode {:?}", mode);

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

    // Add standard modes if not present for host compatibility, but keep one source of truth.
    let standard_modes = [
        (1024, 768, 65000, 1040, 1184, 1344, 771, 777, 806),
        (800, 600, 40000, 832, 960, 1056, 601, 604, 628),
        (640, 480, 25175, 656, 752, 800, 490, 492, 525),
    ];
    for (w, h, clock, hss, hse, ht, vss, vse, vt) in standard_modes {
        if !advertised_modes
            .iter()
            .any(|advertised_mode| advertised_mode.hdisplay == w && advertised_mode.vdisplay == h)
        {
            advertised_modes.push(DisplayMode {
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

    let mut protocol = gud_gadget::ProtocolHandler::new(
        advertised_modes.clone(),
        vec![gud_gadget::GUD_PIXEL_FORMAT_RGB565],
    );

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

    let pitch = db.pitch();

    let mut mapping = card
        .map_dumb_buffer(&mut db)
        .expect("map_dumb_buffer failed");
    debug!("Dumb buffer mapped, pitch={}", pitch);

    let fb_data = mapping.as_mut();
    fb_data.fill(0);
    dump_framebuffer_if_enabled(
        dump_path.as_deref(),
        fb_data,
        pitch as usize,
        width.into(),
        height.into(),
    );
    info!("Initialized framebuffer to black");

    let mut scanout_active = false;
    sync_scanout(
        &card,
        crtc.handle(),
        connector.handle(),
        fb_handle,
        *mode,
        &mut scanout_active,
        false,
    );

    tracing::info!("Entering main event loop");

    while running.load(Ordering::Relaxed) {
        let event = match gud.event_timeout(Duration::from_millis(100)) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("Failed to read GUD event: {}", e);
                continue;
            }
        };
        if event.is_none() {
            continue;
        }
        let event = event.unwrap();
        tracing::debug!("Received event: {:?}", event);

        match protocol.event(event) {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(e) =
                            req.send_descriptor(min_width, min_height, max_width, max_height)
                        {
                            tracing::error!("Failed to send descriptor: {}", e);
                        } else {
                            tracing::debug!("Sent descriptor");
                        }
                    }
                    Event::GetPixelFormats(req) => {
                        if let Err(e) =
                            req.send_pixel_formats(&[gud_gadget::GUD_PIXEL_FORMAT_RGB565])
                        {
                            tracing::error!("Failed to send pixel formats: {}", e);
                        } else {
                            tracing::debug!("Sent pixel formats: RGB565");
                        }
                    }
                    Event::GetDisplayModes(req) => {
                        tracing::debug!("Sending {} display modes", advertised_modes.len());
                        if let Err(e) = req.send_modes(&advertised_modes) {
                            tracing::error!("Failed to send modes: {}", e);
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
                        if let Err(e) =
                            gud_data.recv_buffer(info, mapping.as_mut(), pitch as usize, 2)
                        {
                            tracing::error!("Failed to receive buffer: {}", e);
                        } else {
                            if protocol.can_scanout() {
                                match card.dirty_framebuffer(fb_handle, &[clip]) {
                                    Ok(()) => tracing::debug!("Framebuffer flushed"),
                                    Err(e) => tracing::debug!(
                                        "dirty_framebuffer not supported or failed: {}",
                                        e
                                    ),
                                }
                            } else {
                                tracing::debug!(
                                    "Skipping framebuffer flush: controller_enabled={} display_enabled={} committed_state={}",
                                    protocol.controller_enabled(),
                                    protocol.display_enabled(),
                                    protocol.committed_state().is_some(),
                                );
                            }
                            dump_framebuffer_if_enabled(
                                dump_path.as_deref(),
                                mapping.as_mut(),
                                pitch as usize,
                                width.into(),
                                height.into(),
                            );
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Failed to parse GUD event: {}", e);
            }
        }

        sync_scanout(
            &card,
            crtc.handle(),
            connector.handle(),
            fb_handle,
            *mode,
            &mut scanout_active,
            protocol.can_scanout(),
        );
    }

    tracing::info!("Shutting down");

    Ok(())
}

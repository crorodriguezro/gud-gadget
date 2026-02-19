use drm::buffer::Buffer;
use drm::control::{ClipRect, Device};
use gud_gadget::{DisplayMode, Event};
use std::env::args;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};
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

    let mode = connector.modes().first().unwrap();

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
    
    // Fill with green test pattern to verify display works
    let fb_data = mapping.as_mut();
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = y * pitch as usize + x * 2;
            // RGB565 green: 0x07E0
            fb_data[offset] = 0xE0;
            fb_data[offset + 1] = 0x07;
        }
    }
    // Flush the test pattern
    let test_clip = ClipRect::new(0, 0, width as u16, height as u16);
    match card.dirty_framebuffer(fb_handle, &[test_clip]) {
        Ok(()) => info!("Test pattern flushed to display"),
        Err(e) => warn!("Failed to flush test pattern: {}", e),
    }
    info!("Filled framebuffer with green test pattern");

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

        match gud_gadget::event(event) {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(e) = req.send_descriptor(min_width, min_height, max_width, max_height) {
                            tracing::error!("Failed to send descriptor: {}", e);
                        } else {
                            tracing::debug!("Sent descriptor");
                        }
                    }
                    Event::GetPixelFormats(req) => {
                        if let Err(e) = req.send_pixel_formats(&[gud_gadget::GUD_PIXEL_FORMAT_RGB565]) {
                            tracing::error!("Failed to send pixel formats: {}", e);
                        } else {
                            tracing::debug!("Sent pixel formats: RGB565");
                        }
                    }
                    Event::GetDisplayModes(req) => {
                        let modes = card
                            .get_modes(connector.handle())
                            .unwrap()
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
                                    flags: 0,
                                }
                            })
                            .collect::<Vec<DisplayMode>>();
                        tracing::debug!("Sending {} display modes", modes.len());
                        if let Err(e) = req.send_modes(&modes) {
                            tracing::error!("Failed to send modes: {}", e);
                        } else {
                            tracing::debug!("Sent display modes");
                        }
                    }
                    Event::Buffer(info) => {
                        tracing::debug!("Buffer: x={} y={} {}x{} len={} compression={}", 
                            info.x, info.y, info.width, info.height, info.length, info.compression);
                        let clip = ClipRect::new(
                            info.x as u16,
                            info.y as u16,
                            (info.x + info.width) as u16,
                            (info.y + info.height) as u16,
                        );
                        if let Err(e) = gud_data.recv_buffer(info, mapping.as_mut(), pitch as usize, 2) {
                            tracing::error!("Failed to receive buffer: {}", e);
                        } else {
                            // Flush the framebuffer to notify the display controller
                            match card.dirty_framebuffer(fb_handle, &[clip]) {
                                Ok(()) => tracing::debug!("Framebuffer flushed"),
                                Err(e) => tracing::debug!("dirty_framebuffer not supported or failed: {}", e),
                            }
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Failed to parse GUD event: {}", e);
            }
        }
    }

    tracing::info!("Shutting down");

    Ok(())
}

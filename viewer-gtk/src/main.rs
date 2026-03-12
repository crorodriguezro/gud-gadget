use std::env::var_os;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use adw::prelude::*;
use anyhow::{Context, Result};
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;
use gud_viewer_gtk::rgb565_to_rgba;
use libadwaita as adw;
use memmap2::Mmap;
use tracing::{info, warn};
use viewer_ipc::{default_socket_path, PixelFormat, ServerMessage};

#[derive(Debug)]
enum UiEvent {
    Frame {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Waiting,
    Shutdown,
}

#[derive(Debug)]
struct SharedFramebuffer {
    width: u32,
    height: u32,
    stride: usize,
    map: Mmap,
}

fn viewer_socket_path() -> PathBuf {
    if let Some(path) = var_os("GUD_VIEWER_SOCKET_PATH").map(PathBuf::from) {
        return path;
    }

    let uid = unsafe {
        // SAFETY: `geteuid` has no preconditions and simply returns the effective uid.
        libc::geteuid()
    };
    default_socket_path(uid)
}

fn open_shared_framebuffer(
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: PixelFormat,
    shm_path: &str,
) -> Result<SharedFramebuffer> {
    anyhow::ensure!(
        matches!(pixel_format, PixelFormat::Rgb565),
        "unsupported shared pixel format"
    );
    let file =
        File::open(shm_path).with_context(|| format!("open shared framebuffer {shm_path}"))?;
    let map = unsafe {
        // SAFETY: The file remains open for the duration of the mapping creation, and the mapping
        // is used read-only in this process.
        Mmap::map(&file)
    }
    .with_context(|| format!("map shared framebuffer {shm_path}"))?;

    Ok(SharedFramebuffer {
        width,
        height,
        stride: stride as usize,
        map,
    })
}

fn start_socket_thread(sender: mpsc::Sender<UiEvent>, socket_path: PathBuf) {
    thread::spawn(move || {
        let mut waiting_sent = false;
        loop {
            match UnixStream::connect(&socket_path) {
                Ok(stream) => {
                    info!("Connected to viewer daemon at {}", socket_path.display());
                    waiting_sent = false;
                    let mut reader = BufReader::new(stream);
                    let mut shared: Option<SharedFramebuffer> = None;

                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) => break,
                            Ok(_) => {}
                            Err(err) => {
                                warn!("Failed to read viewer socket: {}", err);
                                break;
                            }
                        }

                        let line = line.trim_end();
                        if line.is_empty() {
                            continue;
                        }

                        let message: ServerMessage = match serde_json::from_str(line) {
                            Ok(message) => message,
                            Err(err) => {
                                warn!("Failed to parse viewer message: {}", err);
                                continue;
                            }
                        };

                        match message {
                            ServerMessage::Hello { .. } => {}
                            ServerMessage::SessionStart {
                                width,
                                height,
                                stride,
                                pixel_format,
                                shm_path,
                            } => match open_shared_framebuffer(
                                width,
                                height,
                                stride,
                                pixel_format,
                                &shm_path,
                            ) {
                                Ok(mapping) => shared = Some(mapping),
                                Err(err) => warn!("Failed to open shared framebuffer: {}", err),
                            },
                            ServerMessage::FrameReady { .. } => {
                                let Some(shared) = shared.as_ref() else {
                                    warn!("Ignoring frame-ready without shared framebuffer");
                                    continue;
                                };
                                match rgb565_to_rgba(
                                    shared.width,
                                    shared.height,
                                    shared.stride,
                                    &shared.map,
                                ) {
                                    Ok(rgba) => {
                                        let _ = sender.send(UiEvent::Frame {
                                            width: shared.width,
                                            height: shared.height,
                                            rgba,
                                        });
                                    }
                                    Err(err) => warn!("Failed to convert RGB565 frame: {}", err),
                                }
                            }
                            ServerMessage::Disconnected => {
                                shared = None;
                                waiting_sent = true;
                                let _ = sender.send(UiEvent::Waiting);
                            }
                            ServerMessage::Shutdown => {
                                let _ = sender.send(UiEvent::Shutdown);
                                return;
                            }
                        }
                    }
                }
                Err(err) => {
                    if !waiting_sent {
                        let _ = sender.send(UiEvent::Waiting);
                        waiting_sent = true;
                    }
                    warn!(
                        "Waiting for viewer daemon socket {}: {}",
                        socket_path.display(),
                        err
                    );
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}

fn configure_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("GUD Viewer")
        .default_width(900)
        .default_height(1800)
        .build();
    window.maximize();

    let picture = gtk::Picture::new();
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_can_shrink(true);
    picture.set_keep_aspect_ratio(true);

    let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 12);
    placeholder.set_valign(gtk::Align::Center);
    placeholder.set_halign(gtk::Align::Center);
    let title = gtk::Label::new(Some("Waiting for PC"));
    title.add_css_class("title-1");
    let subtitle = gtk::Label::new(Some(
        "Connect a host and start the GUD session to mirror it here.",
    ));
    subtitle.add_css_class("dim-label");
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk::Justification::Center);
    placeholder.append(&title);
    placeholder.append(&subtitle);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.add_named(&placeholder, Some("waiting"));
    stack.add_named(&picture, Some("viewer"));
    stack.set_visible_child_name("waiting");

    let toolbar = adw::HeaderBar::new();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&toolbar);
    layout.append(&stack);
    window.set_content(Some(&layout));

    let socket_path = viewer_socket_path();
    let (sender, receiver) = mpsc::channel();
    start_socket_thread(sender, socket_path);

    glib::timeout_add_local(
        Duration::from_millis(16),
        glib::clone!(
            #[weak]
            picture,
            #[weak]
            stack,
            #[weak]
            window,
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || {
                while let Ok(event) = receiver.try_recv() {
                    match event {
                        UiEvent::Frame {
                            width,
                            height,
                            rgba,
                        } => {
                            let stride = width as usize * 4;
                            let bytes = glib::Bytes::from_owned(rgba);
                            let texture = gdk::MemoryTexture::new(
                                width as i32,
                                height as i32,
                                gdk::MemoryFormat::R8g8b8a8,
                                &bytes,
                                stride,
                            );
                            picture.set_paintable(Some(&texture));
                            stack.set_visible_child_name("viewer");
                        }
                        UiEvent::Waiting => stack.set_visible_child_name("waiting"),
                        UiEvent::Shutdown => {
                            window.close();
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    window.present();
}

fn main() {
    configure_tracing();
    adw::init().expect("initialize libadwaita");

    let app = adw::Application::builder()
        .application_id("com.theinternet.GudViewer")
        .build();
    app.connect_activate(build_ui);
    app.run();
}

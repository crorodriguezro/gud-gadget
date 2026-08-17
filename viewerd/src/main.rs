use std::env::{var, var_os};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use gud_gadget::{
    ActiveScanoutState, DisplayMode, Event, PixelDataEndpoint, ProtocolInvalidationReason,
    GUD_COMPRESSION_LZ4,
};
use memmap2::{MmapMut, MmapOptions};
use nix::unistd::{chown, Gid, Uid};
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use usb_gadget::function::custom::{Custom, Interface};
use usb_gadget::{default_udc, Class, Config, Gadget, Strings, Udc, UdcState};
use viewer_ipc::{default_framebuffer_path, default_socket_path, PixelFormat, ServerMessage};

const DEFAULT_UID: u32 = 10000;
const DEFAULT_GID: u32 = 10000;
const DEFAULT_NATIVE_MODE: DisplayMode = DisplayMode {
    clock: 174_359,
    hdisplay: 1080,
    hsync_start: 1192,
    hsync_end: 1208,
    htotal: 1244,
    vdisplay: 2280,
    vsync_start: 2316,
    vsync_end: 2324,
    vtotal: 2336,
    flags: gud_gadget::GUD_DISPLAY_MODE_FLAG_PREFERRED,
};
const PORTRAIT_FALLBACKS: &[(u16, u16)] = &[(900, 1900), (810, 1710), (720, 1520)];

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

#[derive(Debug)]
struct RuntimePaths {
    socket_path: PathBuf,
    framebuffer_path: PathBuf,
}

#[derive(Debug)]
struct PublishedSession {
    width: u32,
    height: u32,
    stride: usize,
    generation: u64,
    map: MmapMut,
    shm_path: PathBuf,
}

impl PublishedSession {
    fn new(width: u32, height: u32, shm_path: PathBuf, uid: u32, gid: u32) -> anyhow::Result<Self> {
        let stride = width as usize * 2;
        let len = stride
            .checked_mul(height as usize)
            .context("framebuffer size overflow")?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&shm_path)
            .with_context(|| format!("open shared framebuffer {}", shm_path.display()))?;
        file.set_len(len as u64)
            .with_context(|| format!("resize shared framebuffer {}", shm_path.display()))?;
        fs::set_permissions(&shm_path, fs::Permissions::from_mode(0o660))
            .with_context(|| format!("chmod shared framebuffer {}", shm_path.display()))?;
        chown(
            &shm_path,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .with_context(|| format!("chown shared framebuffer {}", shm_path.display()))?;
        let map = unsafe {
            // SAFETY: The file length is set to `len` immediately above and remains open for the
            // lifetime of the mapping. The mapping is only mutated through this `MmapMut`.
            MmapOptions::new().len(len).map_mut(&file)
        }
        .with_context(|| format!("mmap shared framebuffer {}", shm_path.display()))?;

        Ok(Self {
            width,
            height,
            stride,
            generation: 0,
            map,
            shm_path,
        })
    }

    fn metadata_message(&self) -> ServerMessage {
        ServerMessage::SessionStart {
            width: self.width,
            height: self.height,
            stride: self.stride as u32,
            pixel_format: PixelFormat::Rgb565,
            shm_path: self.shm_path.display().to_string(),
        }
    }

    fn frame_ready_message(&self) -> ServerMessage {
        ServerMessage::FrameReady {
            width: self.width,
            height: self.height,
            stride: self.stride as u32,
            generation: self.generation,
            full_frame: true,
        }
    }
}

#[derive(Debug)]
struct ViewerPublisher {
    listener: UnixListener,
    clients: Vec<UnixStream>,
    current_session: Option<PublishedSession>,
}

impl ViewerPublisher {
    fn new(paths: &RuntimePaths, uid: u32, gid: u32) -> anyhow::Result<Self> {
        prepare_runtime_path(&paths.socket_path, uid, gid)?;
        prepare_runtime_path(&paths.framebuffer_path, uid, gid)?;
        let _ = fs::remove_file(&paths.socket_path);
        let listener = UnixListener::bind(&paths.socket_path).with_context(|| {
            format!("bind viewer control socket {}", paths.socket_path.display())
        })?;
        listener
            .set_nonblocking(true)
            .context("set control socket nonblocking")?;
        fs::set_permissions(&paths.socket_path, fs::Permissions::from_mode(0o660))
            .with_context(|| format!("chmod control socket {}", paths.socket_path.display()))?;
        chown(
            &paths.socket_path,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .with_context(|| format!("chown control socket {}", paths.socket_path.display()))?;

        Ok(Self {
            listener,
            clients: Vec::new(),
            current_session: None,
        })
    }

    fn accept_new_clients(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _addr)) => {
                    if let Err(err) = send_message(
                        &mut stream,
                        &ServerMessage::Hello {
                            version: viewer_ipc::PROTOCOL_VERSION,
                        },
                    ) {
                        warn!("Failed to greet viewer client: {}", err);
                        continue;
                    }
                    if let Some(session) = &self.current_session {
                        if let Err(err) = send_message(&mut stream, &session.metadata_message()) {
                            warn!("Failed to send session metadata to new client: {}", err);
                            continue;
                        }
                        if let Err(err) = send_message(&mut stream, &session.frame_ready_message())
                        {
                            warn!("Failed to send session frame notice to new client: {}", err);
                            continue;
                        }
                    } else if let Err(err) = send_message(&mut stream, &ServerMessage::Disconnected)
                    {
                        warn!("Failed to send disconnected state to new client: {}", err);
                        continue;
                    }
                    self.clients.push(stream);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) => {
                    warn!("Failed to accept viewer client: {}", err);
                    break;
                }
            }
        }
    }

    fn ensure_session(
        &mut self,
        state: ActiveScanoutState,
        paths: &RuntimePaths,
        uid: u32,
        gid: u32,
    ) -> anyhow::Result<&mut PublishedSession> {
        let needs_new = self
            .current_session
            .as_ref()
            .map(|session| session.width != state.width || session.height != state.height)
            .unwrap_or(true);

        if needs_new {
            let session = PublishedSession::new(
                state.width,
                state.height,
                paths.framebuffer_path.clone(),
                uid,
                gid,
            )?;
            self.current_session = Some(session);
            let message = self
                .current_session
                .as_ref()
                .expect("session just created")
                .metadata_message();
            self.broadcast(&message);
        }

        self.current_session
            .as_mut()
            .context("shared framebuffer session unavailable")
    }

    fn frame_ready(&mut self) {
        let Some(session) = self.current_session.as_mut() else {
            return;
        };
        session.generation += 1;
        let message = session.frame_ready_message();
        self.broadcast(&message);
    }

    fn disconnected(&mut self) {
        self.current_session = None;
        self.broadcast(&ServerMessage::Disconnected);
    }

    fn shutdown(&mut self) {
        self.broadcast(&ServerMessage::Shutdown);
    }

    fn broadcast(&mut self, message: &ServerMessage) {
        let mut idx = 0usize;
        while idx < self.clients.len() {
            if let Err(err) = send_message(&mut self.clients[idx], message) {
                warn!("Dropping viewer client after send failure: {}", err);
                self.clients.remove(idx);
            } else {
                idx += 1;
            }
        }
    }
}

fn send_message(stream: &mut UnixStream, message: &ServerMessage) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *stream, message).context("serialize viewer message")?;
    stream
        .write_all(b"\n")
        .context("write viewer message newline")?;
    Ok(())
}

fn prepare_runtime_path(path: &Path, uid: u32, gid: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create runtime directory {}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o770))
        .with_context(|| format!("chmod runtime directory {}", parent.display()))?;
    chown(parent, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
        .with_context(|| format!("chown runtime directory {}", parent.display()))?;
    Ok(())
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

fn build_advertised_modes() -> Vec<DisplayMode> {
    let mut modes = vec![DEFAULT_NATIVE_MODE.clone()];
    for &(width, height) in PORTRAIT_FALLBACKS {
        modes.push(derive_mode_from_native(&DEFAULT_NATIVE_MODE, width, height));
    }
    modes
}

fn copy_rgb888_to_rgb565_framebuffer(
    info: &gud_gadget::SetBuffer,
    buf: &[u8],
    fb: &mut [u8],
    fb_pitch: usize,
) -> anyhow::Result<()> {
    let width = info.width as usize;
    let height = info.height as usize;
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .context("rgb888 buffer length overflow")?;
    ensure!(
        buf.len() >= expected_len,
        "rgb888 payload too short: got {} bytes, expected at least {}",
        buf.len(),
        expected_len
    );

    for row in 0..height {
        let src_row = &buf[row * width * 3..(row + 1) * width * 3];
        let dst_start = (info.y as usize + row) * fb_pitch + info.x as usize * 2;
        let dst_row = &mut fb[dst_start..dst_start + width * 2];
        for col in 0..width {
            let src_idx = col * 3;
            let r = src_row[src_idx];
            let g = src_row[src_idx + 1];
            let b = src_row[src_idx + 2];
            let pixel = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
            let [lo, hi] = pixel.to_le_bytes();
            dst_row[col * 2] = lo;
            dst_row[col * 2 + 1] = hi;
        }
    }

    Ok(())
}

fn udc_is_detached(udc: &Udc) -> bool {
    match udc.state() {
        Ok(UdcState::Configured) => false,
        Ok(_) => true,
        Err(err) => {
            debug!("Failed to read UDC state: {}", err);
            false
        }
    }
}

fn parse_env_u32(name: &str, default: u32) -> u32 {
    match var(name) {
        Ok(value) => value.parse().unwrap_or_else(|_| {
            warn!("Invalid {name} value {value:?}, using default {default}");
            default
        }),
        Err(_) => default,
    }
}

fn build_runtime_paths(uid: u32) -> RuntimePaths {
    let socket_path = var_os("GUD_VIEWER_SOCKET_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_socket_path(uid));
    let framebuffer_path = var_os("GUD_VIEWER_SHM_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_framebuffer_path(uid));

    RuntimePaths {
        socket_path,
        framebuffer_path,
    }
}

fn configure_tracing() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();
}

fn main() -> anyhow::Result<()> {
    configure_tracing();

    let uid = parse_env_u32("GUD_VIEWER_UID", DEFAULT_UID);
    let gid = parse_env_u32("GUD_VIEWER_GID", DEFAULT_GID);
    let runtime_paths = build_runtime_paths(uid);
    let transfer_format = TransferFormat::from_env();
    let udc = default_udc().expect("no UDC found");
    let advertised_modes = build_advertised_modes();
    gud_gadget::configure_state_check_validation(
        1,
        &[transfer_format.gud_pixel_format()],
        &advertised_modes,
    );

    info!(
        "Starting viewer daemon with socket {} and framebuffer {}",
        runtime_paths.socket_path.display(),
        runtime_paths.framebuffer_path.display()
    );

    usb_gadget::remove_all().context("remove existing USB gadgets")?;
    // The XDISP-P0.1 larger-read experiment is scoped to the Pi gud-drm
    // service. Keep this unrelated viewer demo at its historical granularity.
    let (mut gud_data, gud_data_ep) = PixelDataEndpoint::new_legacy_512();
    let mut builder = Custom::builder().with_interface(
        Interface::new(Class::vendor_specific(Class::VENDOR_SPECIFIC, 0), "GUD")
            .with_endpoint(gud_data_ep),
    );
    builder.ffs_no_disconnect = true;
    let (mut gud, gud_handle) = builder.build();
    let _reg = Gadget::new(
        Class::interface_specific(),
        gud_gadget::OPENMOKO_GUD_ID,
        Strings::new("The Internet", "Generic USB Display", ""),
    )
    .with_config(Config::new("gud-viewer").with_function(gud_handle))
    .bind(&udc)
    .context("bind GUD gadget to UDC")?;
    if let Err(err) = udc.set_soft_connect(true) {
        warn!("Failed to assert USB soft-connect: {}", err);
    }

    let running = Arc::new(AtomicBool::new(true));
    let ctrlc_running = running.clone();
    ctrlc::set_handler(move || {
        ctrlc_running.store(false, Ordering::SeqCst);
    })
    .context("register ctrl-c handler")?;

    let mut publisher = ViewerPublisher::new(&runtime_paths, uid, gid)?;
    let mut had_host_session = false;

    info!(
        "Transfer format: {:?} ({} bytes/pixel)",
        transfer_format,
        transfer_format.bytes_per_pixel()
    );
    info!("Using UDC: {:?}", udc);

    while running.load(Ordering::Relaxed) {
        publisher.accept_new_clients();
        let event = match gud.event_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(err) => {
                warn!("Failed to read GUD event: {}", err);
                if had_host_session && udc_is_detached(&udc) {
                    publisher.disconnected();
                    return Err(anyhow::anyhow!(
                        "USB detached after active host session; restart to recreate gadget"
                    ));
                }
                continue;
            }
        };

        let Some(event) = event else {
            if had_host_session && udc_is_detached(&udc) {
                publisher.disconnected();
                return Err(anyhow::anyhow!(
                    "USB detached after active host session; restart to recreate gadget"
                ));
            }
            continue;
        };

        match gud_gadget::event(event) {
            Ok(Some(gud_event)) => {
                record_host_activity(&mut had_host_session, &gud_event);
                match gud_event {
                    Event::StatusSent(_) => {}
                    Event::GetDescriptor(req) => {
                        req.send_descriptor(
                            640,
                            480,
                            DEFAULT_NATIVE_MODE.hdisplay as u32,
                            DEFAULT_NATIVE_MODE.vdisplay as u32,
                            GUD_COMPRESSION_LZ4,
                        )
                        .context("send display descriptor")?;
                    }
                    Event::GetPixelFormats(req) => {
                        req.send_pixel_formats(&[transfer_format.gud_pixel_format()])
                            .context("send pixel formats")?;
                    }
                    Event::GetDisplayModes(req) => {
                        req.send_modes(&advertised_modes)
                            .context("send advertised modes")?;
                    }
                    Event::StateChecked(snapshot) => {
                        debug!(
                            generation = snapshot.generation,
                            mode = ?snapshot.mode,
                            "State check notification ignored by viewer"
                        );
                    }
                    Event::StateCommitted(snapshot) => {
                        debug!(
                            generation = snapshot.generation,
                            mode = ?snapshot.mode,
                            "State commit notification ignored by viewer"
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
                        debug!(
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
                        info!(?generation, had_host_session, "Host disconnected");
                        publisher.disconnected();
                        if had_host_session {
                            return Err(anyhow::anyhow!(
                                "USB detached after active host session; restart to recreate gadget"
                            ));
                        }
                    }
                    Event::Buffer(info) => {
                        let active_state = gud_gadget::active_scanout_state()
                            .context("buffer upload arrived without committed scanout state")?;
                        let session =
                            publisher.ensure_session(active_state, &runtime_paths, uid, gid)?;
                        let (payload, _stats) = gud_data
                            .recv_payload(&info, transfer_format.bytes_per_pixel())
                            .context("receive bulk payload")?;
                        match transfer_format {
                            TransferFormat::Rgb565 => {
                                PixelDataEndpoint::copy_buffer_to_framebuffer(
                                    &info,
                                    payload,
                                    session.map.as_mut(),
                                    session.stride,
                                    2,
                                )
                                .context("copy rgb565 payload into shared framebuffer")?
                            }
                            TransferFormat::Rgb888 => copy_rgb888_to_rgb565_framebuffer(
                                &info,
                                payload,
                                session.map.as_mut(),
                                session.stride,
                            )
                            .context("convert rgb888 payload into shared framebuffer")?,
                        }
                        session
                            .map
                            .flush_async()
                            .context("flush shared framebuffer mapping")?;
                        publisher.frame_ready();
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                warn!("Failed to parse GUD event: {}", err);
                if had_host_session && udc_is_detached(&udc) {
                    publisher.disconnected();
                    return Err(anyhow::anyhow!(
                        "USB detached after active host session; restart to recreate gadget"
                    ));
                }
            }
        }
    }

    publisher.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{record_host_activity, DisplayMode};
    use gud_gadget::{
        DisplayStateSnapshot, Event, ProtocolInvalidationReason, GUD_PIXEL_FORMAT_RGB565,
    };

    fn mode() -> DisplayMode {
        super::DEFAULT_NATIVE_MODE
    }

    #[test]
    fn lifecycle_only_events_do_not_create_a_viewer_host_session() {
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
            &Event::StateCommitted(DisplayStateSnapshot {
                mode: mode(),
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
                generation: 1,
            }),
        );
        assert!(had_host_session);
    }
}

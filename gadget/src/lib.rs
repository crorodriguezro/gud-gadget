use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::env::var_os;
use std::fs::{rename, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

const GUD_DISPLAY_FLAG_FULL_UPDATE: u32 = 0x02;

const GUD_CONNECTOR_STATUS_CONNECTED: u8 = 0x01;
const GUD_CONNECTOR_STATUS_CHANGED: u8 = 0x80;

pub const GUD_DISPLAY_MODE_FLAG_PREFERRED: u32 = 1 << 10;
pub const GUD_PIXEL_FORMAT_RGB565: u8 = 0x40;
pub const GUD_PIXEL_FORMAT_RGB888: u8 = 0x50;
pub const GUD_PIXEL_FORMAT_XRGB8888: u8 = 0x80;

const GUD_CONNECTOR_TYPE_PANEL: u8 = 0;

const GUD_STATUS_OK: u8 = 0;

static CONNECTOR_STATUS_CHANGED_ONCE: AtomicBool = AtomicBool::new(true);

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

#[derive(Debug, Serialize)]
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
    ) -> anyhow::Result<()> {
        let descriptor = build_display_descriptor(min_width, min_height, max_width, max_height);
        let buf = serialize_display_descriptor(&descriptor)?;

        self.sender.send(&buf).context("send display descriptor")?;
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

        Ok(())
    }
}

impl<'a> GetPixelFormats<'a> {
    pub fn send_pixel_formats(self, formats: &[u8]) -> anyhow::Result<()> {
        self.sender.send(formats).context("send pixel formats")?;
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
) -> DisplayDescriptor {
    DisplayDescriptor {
        magic: GUD_DISPLAY_MAGIC,
        version: 1,
        flags: GUD_DISPLAY_FLAG_FULL_UPDATE,
        compression: 0,
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

fn serialize_connector_descriptors(
    connectors: &[ConnectorDescriptor],
) -> anyhow::Result<Vec<u8>> {
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

fn next_connector_status() -> u8 {
    let mut status = GUD_CONNECTOR_STATUS_CONNECTED;
    if CONNECTOR_STATUS_CHANGED_ONCE.swap(false, Ordering::SeqCst) {
        status |= GUD_CONNECTOR_STATUS_CHANGED;
    }
    status
}

pub fn event(event: custom::Event) -> anyhow::Result<Option<Event>> {
    match event {
        custom::Event::Enable => {
            reset_connector_status_changed();
            debug!("Enable event received");
        }
        custom::Event::Bind => {
            reset_connector_status_changed();
            debug!("Bind event received");
        }
        custom::Event::SetupDeviceToHost(req) => {
            let ctrl_req = req.ctrl_req();
            match ctrl_req.request {
                GUD_REQ_GET_STATUS => {
                    req.send(&[GUD_STATUS_OK]).context("send status")?;
                    debug!("sent status");
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
                    debug!("sent properties {}", sent);
                }
                GUD_REQ_GET_CONNECTORS => {
                    let connectors = [ConnectorDescriptor {
                        connector_type: GUD_CONNECTOR_TYPE_PANEL,
                        flags: 0,
                    }];
                    let buf = serialize_connector_descriptors(&connectors)?;
                    req.send(&buf).context("send connectors")?;
                    debug!("sent connectors");
                }
                GUD_REQ_GET_CONNECTOR_PROPERTIES => {
                    req.send(&[]).context("send connector properties")?;
                    debug!("sent connector properties");
                }
                GUD_REQ_GET_CONNECTOR_MODES => {
                    return Ok(Some(Event::GetDisplayModes(GetDisplayModes {
                        sender: req,
                    })));
                }
                GUD_REQ_GET_CONNECTOR_EDID => {
                    req.send(&[]).context("send EDID")?;
                    debug!("sent empty EDID (no EDID available)");
                }
                GUD_REQ_GET_CONNECTOR_STATUS => {
                    let status = next_connector_status();
                    req.send(&[status]).context("send connector status")?;
                    debug!("sent connector status {:#x}", status);
                }
                request => {
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
                    reset_connector_status_changed();
                }
                GUD_REQ_SET_STATE_CHECK => {
                    debug!("received state check");
                    req.recv_all().context("recv set state check")?;
                }
                GUD_REQ_SET_CONTROLLER_ENABLE => {
                    let req = req.recv_all().context("recv set controller enable")?;
                    debug!("received controller enable: {:?}", req);
                }
                GUD_REQ_SET_DISPLAY_ENABLE => {
                    let req = req.recv_all().context("recv set display enable")?;
                    debug!("received display enable: {:?}", req);
                }
                GUD_REQ_SET_STATE_COMMIT => {
                    req.recv_all().context("recv set state commit")?;
                    debug!("received state commit");
                }
                GUD_REQ_SET_BUFFER => {
                    let req = req.recv_all().context("recv set buffer")?;
                    let v: SetBuffer;
                    (v, _) =
                        ssmarshal::deserialize(req.as_slice()).context("deserialize set buffer")?;
                    debug!("received set buffer: {:?}", v);
                    return Ok(Some(Event::Buffer(v)));
                }
                value => {
                    warn!("unhandled set request {:x}", value);
                }
            }
        }
        custom::Event::Suspend => {
            debug!("Suspend event received");
        }
        custom::Event::Resume => {
            reset_connector_status_changed();
            debug!("Resume event received");
        }
        custom::Event::Disable => {
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

        if self.buf.len() != len {
            panic!("expected buf len {}, got {}", len, self.buf.len());
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
        build_display_descriptor, next_connector_status, reset_connector_status_changed,
        serialize_connector_descriptors, serialize_display_descriptor, serialize_display_modes,
        ConnectorDescriptor, DisplayMode, PixelDataEndpoint, SetBuffer,
        GUD_CONNECTOR_STATUS_CHANGED, GUD_CONNECTOR_STATUS_CONNECTED,
        GUD_CONNECTOR_TYPE_PANEL, GUD_DISPLAY_FLAG_FULL_UPDATE, GUD_DISPLAY_MAGIC,
    };

    #[test]
    fn build_display_descriptor_sets_expected_fields() {
        let descriptor = build_display_descriptor(640, 480, 1080, 2280);

        assert_eq!(descriptor.magic, GUD_DISPLAY_MAGIC);
        assert_eq!(descriptor.version, 1);
        assert_eq!(descriptor.flags, GUD_DISPLAY_FLAG_FULL_UPDATE);
        assert_eq!(descriptor.compression, 0);
        assert_eq!(descriptor.min_width, 640);
        assert_eq!(descriptor.min_height, 480);
        assert_eq!(descriptor.max_width, 1080);
        assert_eq!(descriptor.max_height, 2280);
        assert_eq!(descriptor.max_buffer_size, 1080 * 2280 * 4);
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
    fn serialize_display_descriptor_matches_expected_size_and_header() {
        let descriptor = build_display_descriptor(640, 480, 1080, 2280);

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
        let modes = [DisplayMode {
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
        }];

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
}

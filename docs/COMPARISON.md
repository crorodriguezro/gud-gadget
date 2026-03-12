# Kernel Module vs Userspace Implementation Comparison

This document compares the official [Linux Gadget Driver](https://github.com/notro/gud/wiki/Linux-Gadget-Driver) kernel module with this userspace Rust implementation.

## Key Differences

| Aspect | Kernel Module (notro/gud) | Userspace (this project) |
|--------|---------------------------|--------------------------|
| **Location** | In-kernel (`gud_gadget.c` + `f_gud.c`) | Userspace Rust via FunctionFS |
| **Kernel Version** | Out-of-tree, doesn't build on 6.x | Kernel-agnostic (uses stable FunctionFS API) |
| **Buffer Transfer** | `memcpy` to DRM framebuffer | `memcpy` (main), DMA-BUF (branch, WIP) |
| **EDID** | Full support via `connector->edid_blob_ptr` | Minimal/stub on main; added in dma-buf branch |
| **Connector Properties** | Full TV properties (margins, mode, contrast, etc.) | Empty properties response |
| **Backlight** | Integrated via `backlight_device` | Not implemented |
| **Async Flush** | Worker thread for SPI panels | Not implemented |
| **Modeset** | Full `drm_client_modeset_*` API | Direct CRTC setup, single mode |
| **Hotplug** | DRM client hotplug callback | None |
| **Partial Updates** | Rectangle tracking + flush | Rectangle copying |
| **Multi-connector** | Mask-based connector selection | Single hardcoded panel |
| **Endpoint Handling** | Kernel USB gadget API | FunctionFS + manual ep0 handling (branch) |
| **Compression** | LZ4 in kernel | LZ4 via userspace crate |

## Zero Copy Status

### Kernel Module

**No zero copy support.** The kernel module uses `memcpy` to copy received USB buffers into the DRM framebuffer.

Evidence from `gud_gadget.c`:

```c
static size_t gud_gadget_write_buffer_memcpy(struct drm_client_buffer *buffer,
                                             const void *src, size_t len,
                                             struct drm_rect *rect)
{
    ...
    for (y = 0; y < drm_rect_height(rect) && len; y++) {
        memcpy(dst, src, src_pitch);
        ...
    }
}
```

The Performance wiki explicitly notes: *"RAM speed: the received buffer is memcpy'ed into the framebuffer"*

### Userspace

The `dma-buf-overhaul` branch is attempting to add true zero-copy via DMA-BUF, which would eliminate the memcpy overhead present in the kernel module version. This would allow the USB controller to write directly to a buffer shared with the DRM device.

---

## Kernel Module Advantages

### 1. Full DRM Client Integration

**What it is**: The kernel module uses the `drm_client` API which provides a complete display pipeline abstraction. It can:

- Query and set connector properties (TV margins, underscan, color settings)
- Handle rotation via plane properties
- Do proper modeset check before commit (atomic-style validation)
- Manage multiple framebuffers and plane assignments
- React to DRM driver events (hotplug, DPMS changes)

**Can userspace add it?**: **Partially**

- Userspace can use libdrm or direct ioctls for most DRM operations
- Property setting is doable via `DRM_IOCTL_MODE_OBJ_SETPROPERTY`
- The current code does basic modeset but skips property negotiation
- The `dma-buf-overhaul` branch already adds EDID reading
- **Limitation**: No in-kernel event callbacks - userspace must poll or use fd monitoring

---

### 2. Async Flushing (Worker Thread for SPI Panels)

**What it is**: SPI displays with onboard memory need explicit flush commands (50ms+ for full frame). The kernel queues the flush to a worker thread, allowing the next USB frame to be received in parallel.

```c
static void gud_gadget_flush_worker(struct work_struct *work)
{
    ret = drm_client_framebuffer_flush(buffer, &gdg->flush_rect);
}
```

Without this, USB transfer and SPI flush are serialized, halving throughput on slow displays.

**Can userspace add it?**: **Yes**

- Spawn a thread to handle the DRM dirty framebuffer ioctl
- The current code writes directly to the mmap'd buffer synchronously
- Implementation approach:
  ```rust
  std::thread::spawn(move || {
      // Issue DRM_IOCTL_MODE_DIRTYFB for the rect
  });
  ```
- Need to double-buffer or synchronize to avoid tearing

---

### 3. Proper Hotplug Support

**What it is**: The kernel module registers a `drm_client_funcs.hotplug` callback. When a display is (dis)connected, the DRM core calls this, and the gadget can re-probe connectors and notify the host via status changes.

**Can userspace add it?**: **Yes**

- Monitor `/sys/class/drm/card0-HDMI-A-1/status` via inotify
- Or use `udev` monitoring for drm subsystem events
- Then set the `GUD_CONNECTOR_STATUS_CHANGED` flag in the next status response
- The gadget lib already has `GUD_REQ_GET_CONNECTOR_STATUS` handling

---

### 4. Backlight Support

**What it is**: Direct kernel API access for brightness control:

```c
gdg->backlight = backlight_device_get_by_name(backlight);
backlight_device_set_brightness(gdg->backlight, val);
```

The brightness is exposed as a GUD property and scaled to 0-100%.

**Can userspace add it?**: **Yes**

- Write to `/sys/class/backlight/<device>/brightness`
- Read from `max_brightness` for scaling
- Add `GUD_PROPERTY_BACKLIGHT_BRIGHTNESS` to connector properties response

---

### 5. No Context Switching Overhead

**What it is**:

- **Kernel module**: USB interrupt → gadget driver → DRM memcpy all in kernel context. No syscall boundaries between receiving USB data and writing to framebuffer.
- **Userspace**: USB interrupt → kernel → userspace read syscall → memcpy → (optional) DRM ioctl. Each boundary adds overhead.

**Can userspace add it?**: **Partially**

- The DMA-BUF approach in the branch reduces this: USB controller writes directly to DMA-BUF, which is shared with DRM. The transfer still crosses kernel/userspace for setup/ioctls, but the data path is zero-copy.
- io_uring could batch multiple operations (read + DRM flush) with single submission
- **Fundamental limit**: Some syscall overhead unavoidable; kernel module will always have lower latency
- **Practical reality**: For display use cases (30-60 fps), this overhead is typically negligible compared to USB transfer time and decompression

---

## Summary: Feasibility of Adding Kernel Features to Userspace

| Advantage | Feasibility | Effort | Notes |
|-----------|-------------|--------|-------|
| Full DRM client | Partial | Medium | Use libdrm/ioctls for property negotiation |
| Async flushing | Yes | Low | Thread pool for DRM dirty fb ioctl |
| Hotplug | Yes | Medium | udev/inotify on sysfs |
| Backlight | Yes | Low | sysfs writes |
| Zero context switch | Partial | High | DMA-BUF reduces overhead; io_uring helps |

The `dma-buf-overhaul` branch is addressing the most impactful one (zero-copy transfer). Async flushing and backlight are straightforward additions.

## Userspace Advantages

While the kernel module has the advantages listed above, the userspace implementation has its own benefits:

- **Kernel version agnostic**: Works on any kernel with FunctionFS support (mainline since 2.6.37)
- **Easier development**: No kernel builds, reboots, or crash debugging
- **Potential for true zero-copy**: DMA-BUF integration could outperform the kernel module's memcpy approach
- **Portable**: Could potentially run on non-Linux systems with USB gadget support
- **Flexible**: Can integrate with any userspace graphics stack (not just DRM)

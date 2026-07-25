# Known Issues

## Permanent Black Screen / Repeated USB Disconnects on the Phone

### Symptom

The phone stays black, or briefly shows the host display and then falls back to black while the host repeatedly disconnects and re-detects the USB device.

Typical signs:

- the host briefly sees `1d50:614d`
- the host may also see the phone flip to NCM or mass-storage USB identities
- the phone UDC falls back from `configured` to `not attached`
- the userspace GUD service itself may still look healthy for a moment, but the USB session collapses underneath it

### Root Cause

The main cause we identified was `usb-moded.service` on the phone fighting the userspace GUD gadget.

While `gud-drm` was actively programming a FunctionFS gadget, `usb-moded` tried to switch the phone back to its normal USB modes. That produced a tug-of-war over configfs gadget ownership and led to black-screen/disconnect behavior that looked like a rendering failure, but was actually a USB-mode conflict.

### Fix / Workaround

Before starting the userspace GUD driver on the phone:

```sh
doas systemctl stop usb-moded.service || true
doas systemctl mask --runtime usb-moded.service || true
```

For the current OnePlus setup, also free the panel from the phone UI:

```sh
doas systemctl stop greetd || true
doas systemctl stop getty@tty1.service || true
doas systemctl mask --runtime getty@tty1.service || true
doas pkill agetty || true
doas sh -lc 'echo 0 > /sys/class/vtconsole/vtcon1/bind || true'
doas sh -lc 'echo 0 > /sys/class/graphics/fb0/blank || true'
```

### Status

This is a runtime environment conflict, not a protocol bug in the GUD userspace implementation itself.

The current deployment flow should always stop and runtime-mask `usb-moded` before launching `gud-drm`.

## FunctionFS first bulk-transfer timeout after rebind (`XDISP-P0.1`)

### Symptom

After the Pi gadget is rebound or the phone is rebooted/reconnected, USB
enumeration can look healthy: the OnePlus sees `1d50:614d`, loads `gud.ko`,
and the Pi UDC reports `configured`. The first framebuffer upload can still
hang. The host then reports a GUD bulk-transfer or atomic-update timeout
(`-110`), followed by timed-out control requests. Pi logs show that the
FunctionFS bulk reader entered its read loop but did not report a userspace
read completion.

This is different from the `usb-moded` conflict above: the gadget remains
configured and no competing phone USB identity is required to reproduce it.

### What is known

- A previous clean session completed 64,000-byte RGB565 tiles in 125
  512-byte packets, typically in 5--11 ms, so the protocol and data path are
  capable of working.
- A 2026-07-25 host diagnostic proved that the OnePlus submitted and completed
  the first 64,000-byte bulk URB (`status=0`, `actual=64000`) while Pi
  `gud-drm` stayed blocked in its matching FunctionFS `ep1` read. The next
  `SET_BUFFER` control request then timed out.
- Therefore the current first failing boundary is Pi FunctionFS/kernel delivery
  from a completed endpoint request into the userspace reader, not host bulk
  submission. The Pi became unstable after capture; inspect pstore and its
  persistent journal after recovery before attempting another repair.
- The old AIO receive path was already replaced with a synchronous read of the
  endpoint opened during FunctionFS initialization. That avoided earlier
  invalid AIO completions and endpoint-reopen hangs, but it has not yet proved
  rebind-safe over repeated fresh cycles.
- The userspace teardown repair is implementation-complete: `gud-drm`
  explicitly unbinds the UDC to cancel an active endpoint read, removes the
  gadget, and drops FunctionFS endpoint owners before DRM cleanup. The
  vendored removal path unbinds before closing FunctionFS, and systemd
  `SIGTERM` is handled through the same idempotent path. Focused tests cover
  shutdown timing and retry after an unbind failure.
- The base lifecycle-repair binary completed a controlled no-payload restart
  and one controlled post-payload restart without DWC2 timeout or Oops. The
  isolated 1280x720 RGB565 submission completed all 29 tiles, the host returned
  `PAYLOAD_RC=0`, shutdown released FunctionFS before DRM, and the OnePlus
  re-enumerated `1d50:614d` without `-110`.
- A small follow-up now makes an intentional shutdown take precedence over a
  queued detach restart request, while preserving the error result for an
  unexpected detach. Eleven focused `gud-drm` tests pass, including both exit
  decisions. Its first hardware attempt could not exercise SIGTERM: the first
  payload after activation reproduced host `-110` before shutdown and Pi SSH
  became unreachable, so containment correctly prevented a service stop.
  The timeout occurred before the changed return path; the exit-status fix is
  locally verified but still needs one safe hardware payload/restart. Artifact
  `7053d5b1cf3f7cc94da776a97f64d3471382df9b8f40b00b511eb9ef3bcf1e12`
  is installed on the Pi, with the prior base retained
  at `/home/cristian/gud-drm.pre-xdisp-p0.1-exit0-5aae726`.
- Persistent journal recovery after two Pi restarts captured the failed
  payload boot. `gud-drm` validated the first 64,000-byte `SET_BUFFER` and
  blocked in its first FunctionFS bulk read. Fifteen seconds later the kernel
  Oopsed in `__kmalloc_noprof` while `sshd-session` loaded an ELF binary, with
  `f81ff81ff81ff81f` in allocator state and a subsequent bad RSS-counter
  report. No SIGTERM, DWC2 endpoint-stop timeout, FunctionFS teardown, or DRM
  release occurred. The lifecycle repair therefore makes cleanup safer but
  does not remove the corruption that can occur during the active blocked
  payload. Do not retry the existing 512-byte read loop before changing the
  receive strategy or isolating DWC2 DMA.
- A separate boot-time DRM race exhausted `set_crtc` retries once before a
  manual start succeeded. This is not the previous kernel-cleanup crash, but
  should remain visible as a follow-up lifecycle issue.

### Priority and acceptance

This is `XDISP-P0.1`, tracked canonically in
`../../gud/PROJECT-STATUS.md`. Step 2 has one positive post-payload runtime
result, but the item is still **blocked**, not verified.

Do not mark it resolved from one successful frame. The acceptance test is ten
fresh gadget rebind/phone reconnect cycles in which the first 64 KiB payload
completes without a host `-110` timeout. Retain both host kernel logs and Pi
service logs for each failed or successful run. Follow
`XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md` exactly so the evidence is comparable.

### Safe investigation boundary

Keep the test independent from the experimental Mir plugin. First validate
with the host `gud-kms-fill`/smoke path, then test compositor integration only
after this gate passes. The local 1280x720 HDMI-mode preference change is an
unverified scaling experiment; it must not be presented as a confirmed fix for
this transport failure.

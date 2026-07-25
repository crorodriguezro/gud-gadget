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
  entered the 512-byte FunctionFS loop without an aggregate completion. The
  old logging cannot identify which of its 125 reads stalled. Fifteen seconds
  later the kernel Oopsed in `__kmalloc_noprof` while `sshd-session` loaded an
  ELF binary, with `f81ff81ff81ff81f` in allocator state and a subsequent bad
  RSS-counter report. No SIGTERM, DWC2 endpoint-stop timeout, FunctionFS
  teardown, or DRM release occurred. The lifecycle repair therefore makes
  cleanup safer but does not remove the corruption that can occur during the
  active blocked payload. Do not retry the existing 512-byte read loop before
  changing the receive strategy or isolating DWC2 DMA.
- Step 5 was deployed and its first 16,384-byte hardware payload failed on the
  first read. The Pi requested 16,384 bytes, but FunctionFS returned
  `18446744073709045760` (signed `-505856`) after 476 microseconds. DWC2
  debugfs showed `g_dma=1`, `g_dma_desc=0`, and ep1 OUT
  `DOEPTSIZ=0x0007f800`, or 522,240 bytes remaining. With 16,384 bytes loaded,
  `0x4000 - 0x7f800` underflows to `0xfff84800`, exactly the returned value.
  This localizes the immediate failure to DWC2 buffer-DMA completion
  accounting propagated by FunctionFS; no tile completed.
- Individual read start/completion logs now include the payload sequence,
  request size, result size, remaining bytes, and duration. A short or
  zero-byte completion fails that payload immediately instead of queueing a
  request for bytes the host has not declared. The caller marks the session
  `InFlight` before blocking, so a hung read refuses concurrent teardown. Any
  receive error or completion beyond the conservative one-second safety
  threshold permanently poisons the process when it returns; a hung read
  remains `InFlight`. Decompression and optional dumps run after the session
  returns to `Idle` and cannot falsely poison the endpoint. In-flight and
  poisoned states refuse further USB/control work and ignore `SIGTERM`;
  recovery requires a physical power cycle, hardware reset, or watchdog reset.
  That containment worked in hardware: the process entered `Poisoned`, parked,
  kept the UDC configured, and did not trigger teardown. The Pi remained
  reachable with no new Oops, allocator warning, DWC2 stop timeout, or pstore
  record. All 39 `gud-gadget` and 17 `gud-drm` tests pass; the deployed
  artifact is
  `0c5961daf65a543101bb1727c2c6909a19b5a48ae398cadd94c4c6ebd044b662`.
- The OnePlus utility returned `PAYLOAD_RC=0`, but its kernel logged fresh GUD
  bulk and atomic `-110`, then request `0x64` `-110`. Tool return status alone
  is not transfer proof. Do not run 65,536 bytes, fall back to 512 bytes, or
  stop/restart that poisoned boot. It was subsequently physically recovered.
  Evidence is under
  `../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-step5-16k-first-hardware-2026-07-25T2214BST/`.
- The later modern-laptop control passed with the same 16 KiB reads and
  `g_dma=1`: 953 compressed payloads, 432.6 MB, and 27,053 of 27,053 exact
  FunctionFS reads completed without a transport or kernel error. This proves
  the OnePlus failure is conditional rather than a universal 16 KiB/DMA fault.
  Before compiling a `g_dma=0` kernel, run the planned userspace-only modern
  host controls at a 64,000-byte advertised maximum: first with LZ4 to isolate
  tiling/control cadence, then after a clean result and fresh enumeration with
  compression disabled to reproduce the OnePlus bulk lengths. The user
  observed no noticeable read-size performance difference; formal
  512-byte-versus-16 KiB benchmarking is deferred to `XDISP-P2.1`.
  Evidence is under
  `../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-16k-live-2026-07-25T2256BST/`.
- A separate boot-time DRM race exhausted `set_crtc` retries once before a
  manual start succeeded. This is not the previous kernel-cleanup crash, but
  should remain visible as a follow-up lifecycle issue.

### Priority and acceptance

This is `XDISP-P0.1`, tracked canonically in
`../../gud/PROJECT-STATUS.md`. Step 2 has one positive post-payload runtime
result, but the item is still **blocked**, not verified.

Do not mark it resolved from one successful frame. The acceptance test is ten
fresh gadget rebind/phone reconnect cycles in which all 29 tiles of the
1,843,200-byte frame, including the first 64,000-byte tile, complete without a
host `-110` timeout. Retain both host kernel logs and Pi service logs for each
failed or successful run. Follow
`XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md` exactly so the evidence is comparable.

### Safe investigation boundary

Keep the test independent from the experimental Mir plugin. First validate
with the host `gud-kms-fill`/smoke path, then test compositor integration only
after this gate passes. The local 1280x720 HDMI-mode preference change is an
unverified scaling experiment; it must not be presented as a confirmed fix for
this transport failure.

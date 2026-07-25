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
(`-110`), followed by timed-out control requests. Pi logs normally show that
the FunctionFS bulk reader entered its read loop but never received the first
payload.

This is different from the `usb-moded` conflict above: the gadget remains
configured and no competing phone USB identity is required to reproduce it.

### What is known

- A previous clean session completed 64,000-byte RGB565 tiles in 125
  512-byte packets, typically in 5--11 ms, so the protocol and data path are
  capable of working.
- The same first payload can stall after a rebind even with the UDC configured
  and FunctionFS descriptors accepted.
- The old AIO receive path was already replaced with a synchronous read of the
  endpoint opened during FunctionFS initialization. That avoided earlier
  invalid AIO completions and endpoint-reopen hangs, but it has not yet proved
  rebind-safe over repeated fresh cycles.

### Priority and acceptance

This is `XDISP-P0.1`, tracked canonically in
`../../gud/PROJECT-STATUS.md`. It is **blocked**, not fixed.

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

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

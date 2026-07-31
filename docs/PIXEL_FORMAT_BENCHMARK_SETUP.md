# Pixel format benchmark setup

`gud-drm` advertises exactly one GUD transfer format per process. The selected
format is set before the service starts with `GUD_TRANSFER_FORMAT`; it cannot be
changed in an active USB session.

RGB888 remains supported end-to-end. Its DRM native memory order is B, G, R;
XRGB8888 is B, G, R, X on little-endian Linux DRM targets. PPM dumps decode both
to logical R, G, B pixels.

## Safety boundary

Do not stop, restart, reboot, unbind the gadget, or retry a payload if the
FunctionFS receive state is `InFlight` or `Poisoned`. Preserve journals and
follow the physical recovery procedure in
`XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md`. The following switching steps apply
only after a clean, idle receiver state and permitted host detach.

Changing only the sender command, including `mirgud --pixel-format`, is
insufficient. The Pi's advertised GUD format, DRM scanout format, and host
session must all be recreated together.

## RGB565 switch

1. Confirm the prior session is detached and the receiver is `Idle`.
2. Stop the safe service only when that state permits it. Do not use `restart`.
3. Set the service environment to `GUD_TRANSFER_FORMAT=rgb565` and run
   `systemctl daemon-reload` after changing a drop-in.
4. Start the service once and perform a fresh USB gadget enumeration. Wait for
   phone host rediscovery before transmitting data.
5. Verify the Pi startup log contains:

```text
transfer_format=Rgb565 gud_format=0x40 drm_fourcc=Rgb565 depth=16 bpp=16 bytes_per_pixel=2
```

## XRGB8888 switch

1. Confirm the prior session is detached and the receiver is `Idle`.
2. Stop the safe service only when that state permits it. Do not use `restart`.
3. Set the service environment to `GUD_TRANSFER_FORMAT=xrgb8888` and run
   `systemctl daemon-reload` after changing a drop-in.
4. Start the service once and perform a fresh USB gadget enumeration. Wait for
   phone host rediscovery before transmitting data.
5. Verify the Pi startup log contains:

```text
transfer_format=Xrgb8888 gud_format=0x80 drm_fourcc=Xrgb8888 depth=24 bpp=32 bytes_per_pixel=4
```

## Verification checklist

- The Pi startup log records the intended `transfer_format`, GUD format,
  FourCC, depth, bpp, and bytes-per-pixel.
- The descriptor exposes only the expected GUD format: `0x40` for RGB565 or
  `0x80` for XRGB8888.
- The phone's GUD DRM framebuffer uses the expected FourCC.
- The phone has rediscovered a freshly enumerated USB gadget; no prior-format
  session remains.
- Before any timed benchmark, run only the bounded static diagnostic pattern
  and capture a PPM dump for channel-order inspection.

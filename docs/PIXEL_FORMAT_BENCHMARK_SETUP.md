# Pixel format benchmark setup

`gud-drm` advertises exactly one GUD transfer format per process. The selected
format is set before the service starts with `GUD_TRANSFER_FORMAT`; it cannot be
changed in an active USB session.

RGB888 remains supported by the Pi implementation for legacy or local use, but
it is not supported end to end by the current mirgud + OnePlus GUD benchmark
stack and is outside this benchmark. It advertises GUD format `0x50`, uses DRM
`Rgb888`, and has three bytes per pixel. Its DRM native memory order is B, G, R.
XRGB8888 is B, G, R, X on little-endian Linux DRM targets; the X byte is ignored
when rendering or producing PPM output. PPM dumps decode visible pixels as
R, G, B.

## Safety boundary

Do not stop, restart, reboot, unbind the gadget, or retry a payload if the
FunctionFS receive state is `InFlight` or `Poisoned`. Preserve journals and
follow the physical recovery procedure in
`XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md`. The following switching steps apply
only after a clean, idle receiver state and permitted host detach.

Changing only the sender command, including `mirgud --pixel-format`, is
insufficient. The Pi's advertised GUD format, DRM scanout format, and host
session must all be recreated together.

## Fresh format switch

Every format selection requires a fresh gadget process and USB enumeration. Do
not change `GUD_TRANSFER_FORMAT` in an attached session: stop only from `Idle`,
set the environment, start once, then detach and reattach so the host reads the
new single-format descriptor.

For controlled XDISP benchmarks, install exactly one complete drop-in from
`systemd/test-only/70-xdisp-benchmark-rgb565.conf` or
`systemd/test-only/71-xdisp-benchmark-xrgb8888.conf`. They keep dumping
disabled, retain the 16 KiB FunctionFS read size, and force native 1280x720
scanout. Do not reconstruct this environment from partial format-only drop-ins.

## RGB565

1. Confirm the prior session is detached and the receiver is `Idle`.
2. Stop the safe service only when that state permits it. Do not use `restart`.
3. Set the service environment to `GUD_TRANSFER_FORMAT=rgb565` and run
   `systemctl daemon-reload` after changing a drop-in.
4. Start the service once and perform a fresh USB gadget enumeration. Wait for
   phone host rediscovery before transmitting data.
5. Verify the Pi startup log contains:

```text
transfer_format=rgb565 gud_format=0x40 drm_fourcc=Rgb565 depth=16 bpp=16 bytes_per_pixel=2
```

## RGB888

RGB888 is Pi-only for this benchmark and must not be selected for a mirgud +
OnePlus GUD comparison run.

1. Confirm the prior session is detached and the receiver is `Idle`.
2. Stop the safe service only when that state permits it. Do not use `restart`.
3. Set the service environment to `GUD_TRANSFER_FORMAT=rgb888` and run
   `systemctl daemon-reload` after changing a drop-in.
4. Start the service once, then detach and reattach for a fresh USB gadget
   enumeration. Wait for phone host rediscovery before transmitting data.
5. Verify the Pi startup log contains:

```text
transfer_format=rgb888 gud_format=0x50 drm_fourcc=Rgb888 depth=24 bpp=24 bytes_per_pixel=3
```

## XRGB8888

1. Confirm the prior session is detached and the receiver is `Idle`.
2. Stop the safe service only when that state permits it. Do not use `restart`.
3. Set the service environment to `GUD_TRANSFER_FORMAT=xrgb8888` and run
   `systemctl daemon-reload` after changing a drop-in.
4. Start the service once and perform a fresh USB gadget enumeration. Wait for
   phone host rediscovery before transmitting data.
5. Verify the Pi startup log contains:

```text
transfer_format=xrgb8888 gud_format=0x80 drm_fourcc=Xrgb8888 depth=24 bpp=32 bytes_per_pixel=4
```

## Verification checklist

- The Pi startup log records the intended `transfer_format`, GUD format,
  FourCC, depth, bpp, and bytes-per-pixel.
- The descriptor exposes only the expected GUD format: `0x40` for RGB565,
  `0x50` for RGB888, or `0x80` for XRGB8888.
- The phone's GUD DRM framebuffer uses the expected FourCC.
- The phone has rediscovered a freshly enumerated USB gadget; no prior-format
  session remains.
- Every `frame_stats` line records the selected lowercase `transfer_format`, its
  `gud_format`, and `bytes_per_pixel` exactly once.
- Before any timed benchmark, run only the bounded static diagnostic pattern
  and capture a PPM dump for channel-order inspection. For XRGB8888, vary the
  X padding byte and confirm the visible PPM pixels are unchanged.
- `GUD_FRAME_DUMP_MODE` defaults to `disabled`; timed benchmarks must retain
  that setting. `single-frame` permits only an untimed startup dump when one or
  both legacy dump paths are supplied. The receiver never writes framebuffer
  dumps after a payload, so no diagnostic disk I/O occurs between `SET_BUFFER`
  requests during a timed run.

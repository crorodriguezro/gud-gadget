# XDISP-P2.1 Native 1280x720 Scanout Gate

## Goal

Measure whether the poor OnePlus-to-Pi video cadence comes from the current
scaled presentation path rather than USB receive bandwidth.

The baseline 1280x720 raw-video run used a 1920x1080 physical HDMI scanout.
Each GUD frame averaged 11.3 rectangles. The service software-scaled the full
frame and swapped the back buffer after every rectangle, producing about
39--40 ms of Pi presentation work per rectangle and only 2.015 synchronous
updates per second.

The upstream C gadget does not perform a new scaled presentation for each
rectangle. It copies each update into the framebuffer being scanned out. This
gate reproduces that shape using the existing native path in `gud-drm`.

## Test-only control

```text
GUD_TEST_OUTPUT_MODE=WIDTHxHEIGHT
```

When absent, physical DRM mode selection is unchanged: `gud-drm` selects the
connector's first mode. When present, the value must be valid UTF-8, use exact
lowercase `WIDTHxHEIGHT` syntax, contain positive `u16` dimensions, and match
an actual mode reported by the connected DRM connector.

Invalid syntax fails before DRM or UDC setup. A well-formed but unsupported
mode fails after connector discovery and before `usb_gadget::remove_all()`.
The startup journal emits an explicit test-only warning and records the
selected physical mode and timing. If the connector exposes multiple timings
at the requested resolution, the override selects the first exact-resolution
match. It does not force the USB host to choose the same GUD mode.

The controlled 1280x720 drop-in is:

```text
systemd/test-only/40-xdisp-p2.1-native-1280x720.conf
```

It changes neither the GUD descriptor's compression/max-buffer defaults nor
the FunctionFS read ceiling. It must not be installed as normal service
configuration.

## Safety boundary

Keep the OnePlus diagnostic module's 12,800-byte actual payload cap and the
Pi's 16 KiB FunctionFS read ceiling unchanged.

Do not stop, restart, reboot, signal, or replace the active Pi service while
USB is connected or its receive state is `InFlight` or `Poisoned`.

Before activation:

1. Physically detach the Pi-OnePlus data cable.
2. Prove the phone logged GUD disconnect and has no GUD DRM card.
3. Prove the Pi service's most recent receive returned to `Idle`, followed by
   `Suspend`, with no timeout or kernel fault.
4. Prove no stale `systemd/test-only/30-*` descriptor/read gate is installed;
   retain only the normal 16 KiB read-size and containment drop-ins.
5. Only then stop the service, install the staged binary/drop-in, reload
   systemd, and start it.
6. Reconnect physically and perform a fresh `1d50:614d` enumeration.

The normal OnePlus `/home/phablet/gud.ko` and Mir remain unchanged.

## Gate procedure

Use the same host module, runner, clip, mode, and pacing as the scaled
baseline:

```text
/home/phablet/gud-kms-animate-raw \
  /dev/dri/card1 raw 10 5 \
  /home/phablet/gud-testsrc2-1280x720-5fps-10f.rgb565le
```

Before playback, require:

- Pi startup warning reports `GUD_TEST_OUTPUT_MODE=1280x720`;
- Pi selected physical DRM output is 1280x720;
- host committed a 1280x720 GUD state;
- the Pi service and phone diagnostic driver are healthy.

After playback, require:

- ten requested video frames completed;
- every Pi receive has a matching `InFlight` and `Idle`;
- every actual host payload remains at or below 12,800 bytes;
- every Pi `frame_stats` line reports `source=1280x720 scaled=false`;
- `scale_ms=0` for every payload;
- no `Scaled framebuffer presented via back-buffer swap` during the gate;
- no short read, poison, host `-110`, DWC2/vc4 fault, Oops, pstore record, or
  reboot;
- visual output is recognizable and any progressive-update tearing is
  recorded separately from throughput.

Compare synchronous update FPS and commit latency directly with the preserved
scaled baseline:

```text
update_fps=2.015
commit_avg_ms=489.394
rectangles=113
```

## Interpretation

- Large improvement with the same rectangle count: full-frame scaling and
  per-rectangle back-buffer presentation were the primary bottleneck.
- Similar performance with `scaled=false`: investigate host compression CPU,
  USB/control cadence, and Pi copy/dirty behavior next.
- Native path is fast but visibly tears: retain the transport result and
  design asynchronous burst/vblank presentation separately. GUD has no
  explicit end-of-frame message, so batching requires a defined timing policy.

If the gate passes, the product direction is dynamic physical modesetting on
the GUD `SET_STATE_COMMIT`, not a permanent 1280x720 environment override.
If it fails, remove the drop-in while physically detached and restore the
previous staged service binary without changing either kernel.

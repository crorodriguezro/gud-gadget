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

## First Activation Was Invalid at State Negotiation

The first hardware attempt on 2026-07-26 did not exercise native pixel
transfer and is not a performance result. The Pi correctly selected the
physical 1280x720 timing, but the monitor's DRM mode list marked every mode
only as `DRIVER`; it supplied no `PREFERRED` type. The initial override
implementation therefore used the override-selected 1280x720 mode as its
fallback USB preferred mode and advertised its flags as `0x405`.

The separately preserved OnePlus diagnostic backport submitted the identical
74250 kHz 720p timing with flags `0x005`. The only difference was the GUD
advertisement-only preferred bit `0x400`. Exact state validation rejected
`SET_STATE_CHECK`, so no state was committed and `SET_BUFFER` was also
rejected. The host nevertheless submitted a 6,227-byte bulk request and
reported `-110` because the Pi had correctly posted no bulk receive for the
invalid buffer request.

There was no `Event::Buffer`, `InFlight`, `Poisoned`, `payload_seq`, or
`frame_stats` entry. This was neither a 12,800-byte transport failure nor an
adaptive-LZ4 result.

The correction keeps advertised preference independent of the physical
test override: use an explicit DRM preferred mode when one exists, otherwise
retain connector mode index zero, which is the normal no-override fallback.
Unit tests cover both cases. The retry must prove a successful state check and
commit before interpreting any bulk or performance result.

## Corrected Hardware Result

The corrected 2026-07-26 gate passed with source commit `4556800` and Pi
binary SHA-256
`05bbc2284f38bcd0152bcf462cecd009fa94b342e42e399204347a3fb53a8984`.

The unchanged host command completed at the requested pacing limit:

```text
modeset_ms=34.049
update_elapsed_s=1.800
update_fps=4.999
commit_avg_ms=50.097
commit_min_ms=34.142
commit_max_ms=70.695
```

All ten state checks and commits succeeded. The ten frames produced 113 Pi
payloads, and all 113 entered `InFlight` and returned to `Idle`. Every
`frame_stats` entry reported `source=1280x720`, `scaled=false`, and
`scale_ms=0`; there was no scaled back-buffer swap. The largest payload was
12,793 bytes. The mean of the logged whole-millisecond Pi `total_ms` values
was 0.504 ms per payload and the logged maximum was 6 ms. Neither host nor Pi
recorded a new transport or kernel fault.

The final physical cable removal was also safe. The OnePlus logged USB/GUD
disconnect and removed its GUD DRM card. The Pi stayed on the same boot and
service PID, and FunctionFS delivered `Suspend` after the final receive had
returned to `Idle`; all 113 `InFlight` entries still had matching `Idle`
entries and no receive was poisoned. No Pi kernel fault or pstore record
appeared. FunctionFS did not deliver `Disable`, however: the service remained
active and UDC sysfs retained `configured`. This is a quiescent physical
detach, not proof of automatic gadget teardown/restart. A manual stop is not
part of this performance gate; it is required only before removing the
test-only drop-in or changing the staged service artifact, using the safety
preconditions above.

The scaled baseline and corrected native run used the same clip, host module,
113 rectangles, roughly 0.919 MB of payload, and a 12,793-byte maximum.
Average commit latency fell from 489.394 ms to 50.097 ms, a 9.77-times
improvement, while update rate rose from 2.015 to 4.999 fps and reached the
configured 5-fps pacing ceiling. This causally identifies per-rectangle Pi
scaling/presentation as the baseline bottleneck; it does not establish maximum
native throughput.

The next userspace design step is dynamic physical mode selection on a
successful GUD state commit: select an exact connector mode, create and map
matching scanout buffers, modeset once, then receive through the native copy
path. Preserve the current scaled path when no exact physical mode exists and
keep USB advertised preference independent of physical selection. An unpaced
native benchmark should follow before optimizing the remaining host-side
compression and synchronous submission cost.

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

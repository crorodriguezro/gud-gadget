# GUD and H.264 display-path findings

Date: 2026-08-28
Hardware: OnePlus 6, Fedora Asahi KDE laptop, Raspberry Pi Zero 2 W, 1920x1080 HDMI display

This document consolidates the implemented approaches, tests, benchmarks,
failures, and current conclusions for displaying either the Lomiri desktop or
the laptop desktop through the Pi. It covers both USB GUD and H.264 transport.

The most important comparison rule is that results from different workloads
must not be treated as a controlled A/B. A static coding desktop, a synthetic
moving pattern, and incompressible noise have radically different LZ4 and H.264
costs. Values below retain their workload and evidence status.

## Executive summary

- GUD is the lossless desktop path. It preserves RGB565 text and color edges,
  uses the standard Linux GUD host driver, and carries display updates over
  USB2. The qualified Lomiri production path reached 19-20 presentation
  submits/s at representative 1080p and passed exact-mode, pixel-correctness,
  soak, and lifecycle gates.
- Optimized Lomiri H.264 reaches 29.975-29.982 source/encode/decode FPS and
  29.039-29.821 DRM presentation submits/s. Transport retained every access
  unit. Its tradeoff is irreversible 4:2:0 chroma loss on small colored text.
- Laptop GUD works as a normal KDE extended monitor through the in-tree Linux
  GUD driver. The recent interactive session observed approximately
  13.6-16.3 updates/s and felt responsive for coding, but that short session is
  an observation, not a controlled benchmark bundle.
- Laptop H.264 now creates a real KDE extended output and streams continuously
  at approximately 30 FPS over Wi-Fi. A settled live sample received 1,230,
  decoded 1,228, and presented 1,202 frames; the sender held 29.9-30.2 FPS and
  presentation was approximately 29.3 FPS with a bounded 1-2-frame decoder
  queue.
- No path has a calibrated input-to-photon result. SETPLANE return,
  transaction completion, and independent-clock relative-age values are not
  physical scanout timestamps.
- For coding, choose GUD when lossless text is the priority and the USB host
  topology is convenient. Choose H.264 when smoother updates, much lower wire
  bandwidth, or independence from the USB host connection is more important.

## Repository/topology evolution

The repository's March checkpoint used the OnePlus itself as the GUD gadget
and the laptop as host. That work established protocol state handling, live
LZ4, lower-resolution portrait scaling, double-buffered presentation, and the
fact that `usb-moded` could fight the userspace configfs gadget. It also found
that the old synchronous `STATUS_ON_SET` userspace sequencing deadlocked.

The later July/August display project moved `gud-gadget` to the Pi Zero 2 W.
The OnePlus and laptop are now alternative Linux GUD hosts, and the Pi drives
the 1080p HDMI display. FunctionFS AIO and accepted-I/O ownership were
redesigned in that topology, so the later status-on-set and payload results
supersede the original phone-gadget limitations. Early phone-panel performance
figures must not be mixed with the Pi/HDMI benchmarks below.

Historical context:
[March checkpoint](docs/CHECKPOINT_2026-03-10.md).

## The four pipelines

### Lomiri over GUD/USB

```text
Lomiri/Mir external output
  -> direct CPU-mappable RGB565 screencast
  -> xdisp/mirgud latest-frame presenter
  -> OnePlus host DRM GUD driver
  -> LZ4-or-raw GUD full update over USB2
  -> Pi FunctionFS gud-gadget
  -> RGB565 VC4 DRM scanout
  -> HDMI
```

The OnePlus is the USB host and the Pi is the GUD gadget/display device. The
Pi service is `gud-userspace.service`; the phone host exposes the gadget as a
secondary DRM node such as `/dev/dri/card1`.

### Laptop over GUD/USB

```text
KDE/KWin extended output
  -> upstream Linux GUD DRM driver
  -> LZ4-or-raw GUD update over USB2
  -> Pi FunctionFS gud-gadget
  -> RGB565 VC4 DRM scanout
  -> HDMI
```

KWin supports the GUD device as a secondary GPU/output. GNOME/Mutter Wayland
has historically detected the DRM device but not rendered to it; see
[docs/GNOME_ISSUE.md](docs/GNOME_ISSUE.md).

### Lomiri over H.264/Wi-Fi

```text
Lomiri/Mir ABGR8888
  -> NEON BT.709-limited NV12 conversion
  -> Qualcomm Venus H.264, four bounded buffers
  -> MH264FRM access units over TCP/Wi-Fi
  -> Pi /dev/video10 hardware decoder
  -> NV12 DMABUF/DRM PRIME
  -> bounded latest-frame VC4 plane presenter
  -> HDMI
```

### Laptop over H.264/Wi-Fi

```text
KWin virtual extended output
  -> krfb-virtualmonitor loopback RFB
  -> GStreamer videorate at 30 FPS
  -> x264 ultrafast/zerolatency
  -> MH264FRM access units over TCP/Wi-Fi
  -> Pi hardware decoder
  -> NV12 DMABUF/DRM PRIME
  -> bounded latest-frame VC4 plane presenter
  -> HDMI
```

RFB stays on `127.0.0.1`; only H.264 crosses Wi-Fi. The laptop currently has
no usable Apple-GPU H.264 encoder exposed to FFmpeg/GStreamer, so x264 is a
software encoder.

## Why the visible desktop may not match the attached USB host

GUD/USB and H.264/TCP are independent pixel paths. The H.264 receiver uses the
active `gud-userspace` process as a stable DRM-fd owner, but it imports its own
NV12 decoder buffers and selects its own VC4 plane. It does not receive pixels
through GUD.

Consequently:

- attaching the laptop to the USB hub can make KDE discover a GUD extended
  monitor without changing the HDMI picture if H.264 still owns the scanout;
- a Lomiri desktop can remain on HDMI over Wi-Fi while the laptop is the USB
  host;
- stopping an H.264 receiver can disable its plane without automatically
  restoring a GUD framebuffer; and
- only one intended scanout owner should be qualified at a time.

This explains the observed case where the Pi displayed Lomiri even though the
laptop was connected to the hub.

## GUD implementation findings

### Mir formats and selected transport

The deployed Mir 1.8.3 Android2/libhybris backend was audited through the
public screencast API.

| Requested format | API/result | 1280x720 stride/bytes | Source-only rate | End-to-end suitability |
|---|---|---:|---:|---|
| XRGB8888 | Not advertised; rejected in this audit | N/A | N/A | Current code can reorder ABGR to XRGB, but not direct here |
| RGB888 | Accepted, true packed delivery | 3,840 / 2,764,800 | 32 FPS | No direct GUD transport variant |
| RGB565 | Accepted, true packed delivery | 2,560 / 1,843,200 | 61 FPS | Selected direct transport |

The backend advertised ABGR8888, XBGR8888, RGB888, and RGB565. Direct RGB565
halves source and wire bytes relative to 32-bit RGB and avoids the older
ABGR-to-XRGB conversion. The qualification proves exact returned layout and
CPU mapping; it does not distinguish compositor-internal rendering from an
internal format conversion.

Primary evidence:
[Mir format capability](evidence/xdisp-mir-format-capability-20260824T003458Z/summary.md).

### GUD update architecture

The selected production design is a synchronous negotiated-capacity full
update:

- one logical RGB565 rectangle per full frame;
- one LZ4 attempt for that rectangle;
- same-rectangle raw fallback when compression is not beneficial;
- one serialized accepted payload at a time;
- backpressure and freshness coalescing in the userspace
  `LatestFramePresenter`; and
- at most one in-flight plus one newest pending source frame.

An asynchronous host flush prototype also passed bounded-work and detach
containment tests, including a controlled 3.5-second worker stall, but it was
not selected as production. Synchronous full update was simpler and already
met the live target.

Primary evidence:
[upstream full-update qualification](evidence/xdisp-e4-t07-upstream-full-update-async-20260826T232713Z/final-report.md).

### FunctionFS and USB findings

The large-transfer work established that the original payload ceiling was an
implementation limit, not a GUD or USB2 protocol limit.

1. A 327,680-byte FunctionFS AIO initially failed with an order-7 contiguous
   allocation request. The Pi DWC2 controller cannot advertise the scatter/
   gather capability needed by the normal upstream path.
2. The fix retained one logical FunctionFS/GUD payload but backed it with
   vmalloc memory and issued sequential 16 KiB DMA chunks internally.
3. After the fix, 327,680 bytes passed 100/100. Larger rungs through
   1,843,200 bytes passed, and a 3,686,400-byte XRGB8888 logical frame later
   passed 100/100 with exact completion.
4. Full-frame raw XRGB8888 measured approximately 36.5 MB/s and about 9 FPS.
   Pi processing was only about 4 ms p50, making USB bulk the limiting stage.
5. Controlled raw RGB565 measurements place the practical bulk-only ceiling
   around 40 MB/s, below USB2's 480 Mbit/s signalling rate as expected after
   protocol/controller overhead.

The old 12,800-byte planning requirement was therefore removed. The 16 KiB
value remains an internal USB chunk/read granularity, not the logical frame
limit.

`STATUS_ON_SET` also evolved. The original blocking userspace receiver could
deadlock by waiting for bulk data before the host was allowed to send it. The
later `status-on-set-aio` design first admits the exact native AIO request,
then returns status so the host can submit bulk. It passed a correlated
physical transaction and a clean-source 100/100 ordered gate with exact
12,800-byte completions, zero BUSY, timeout, poison, or processing failure.
The lesson is not that `STATUS_ON_SET` is inherently unusable; it requires the
admission ordering and ownership model implemented by the later receiver.

Primary evidence:
[large-AIO fix](evidence/xdisp-e1-functionfs-large-aio-20260823T200646Z/summary.md),
[full-frame scaling](evidence/xdisp-e1-fullframe-managed-scaling-20260823T223143Z/summary.md),
[definitive performance](evidence/xdisp-definitive-performance-20260827T140000Z/definitive-summary.md),
[physical status-on-set](evidence/functionfs-status-on-set-hs-20260817T165632Z/FINAL_REPORT.md),
and [100-frame status-on-set gate](evidence/functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/FINAL_REPORT.md).

### Native damage and compression

The current public Mir screencast/buffer-stream consumer does not expose
native damage, and the deployed GUD backport has no `FB_DAMAGE_CLIPS` path.
No full-frame software diff was added. Bandwidth reduction therefore comes
from RGB565 plus LZ4, not rectangle damage tracking.

Measured managed XRGB8888 LZ4 work before direct RGB565 showed:

- payload 679,244 B p50 and 2,846,081 B p95;
- host compression 22.045/62.719 ms p50/p95;
- USB wait 16.691/70.269 ms p50/p95;
- Pi decompression 7/18 ms p50/p95; and
- 13.27 presented FPS over 60.795 seconds.

Direct Mir RGB565 substantially improved the path by reducing both source and
uncompressed frame size.

Primary evidence:
[bandwidth/damage audit](evidence/xdisp-e2-bandwidth-damage-20260823T232018Z/summary.md).

### Mode routing, scaling, and pixel correctness

The Pi enumerated 25 real HDMI modes. GUD advertises complete timing, not only
width and height. Real 1280x720 and 1920x1080 modes route through `DirectExact`
to the matching VC4 mode; synthetic modes use `ScaledFallback`.

Qualification results:

- exact 1280x720 RGB565: pitch 2,560, mapping 1,843,200 B;
- exact 1920x1080 RGB565: pitch 3,840, mapping 4,147,200 B;
- five complete 720p/1080p round trips plus same-mode idempotence: zero
  failures and no unnecessary same-mode allocations/modesets;
- synthetic 720x1520 fallback: passed, but the CPU scaler costs approximately
  152 ms p50 and is not suitable for high frame rates; and
- deterministic checkerboard/color/edge test: 9/9 presented, no crop,
  squeeze, shift, or channel-order defect observed.

The exact-mode implementation passed 174 Rust tests, all retained C contract
tests, the AArch64 release build, and live mode/cycle gates.

Primary evidence:
[physical mode alignment](evidence/xdisp-e4-t03-physical-mode-alignment-20260827T053221Z/final-report.md)
and [pixel correctness](evidence/xdisp-e4-t02-pixel-correctness-20260827T010445Z/qualification-summary.md).

## GUD benchmarks

### Representative Lomiri production runs

Each row below used a roughly 60-second interval after warmup, direct RGB565,
LZ4, and exact physical mode routing. `Presented FPS` means accepted
presentation submits; no vblank completion event was available.

The source reports classify the overall requested matrix as **PARTIAL** because
they did not retain every requested controlled desktop animation, repetition,
per-frame distribution, and immediate post-run journal export. The completed
rows below are valid; the broader matrix must not be called complete.

| Mode/run | Mir source FPS | Presented FPS | LZ4 hit | Mean payload | LZ4 p50/p95 | Bulk p50/p95 |
|---|---:|---:|---:|---:|---:|---:|
| 1280x720 r1 | 61 | 28 | 100% | 521,128 B | 18.6/25.7 ms | 13.7/21.7 ms |
| 1280x720 r2 | 61 | 21 | 100% | 633,319 B | 25.3/30.3 ms | 15.5/15.9 ms |
| 1280x720 r3 | 61 | 21 | 100% | 633,319 B | 26.5/30.8 ms | 15.5/15.9 ms |
| 1920x1080 r1 | 61 | 20 | 100% | 645,375 B | 27.6/32.8 ms | 15.8/16.2 ms |
| 1920x1080 r2 | 61 | 20 | 100% | 645,375 B | 27.7/33.5 ms | 15.8/16.1 ms |
| 1920x1080 r3 | 61 | 19 | 100% | 645,375 B | 28.1/33.7 ms | 15.8/16.2 ms |

The 1080p representative path was compression/presenter bound, not at the raw
USB ceiling. Host GUD present time was approximately 45-46 ms, consistent with
about 20 FPS.

Primary evidence:
[720p/1080p report](evidence/xdisp-720p-1080p-performance-20260827T150000Z/final-report.md)
and [raw results](evidence/xdisp-720p-1080p-performance-20260827T150000Z/results.csv).

### Controlled workload limits

| Workload | Result | Interpretation |
|---|---:|---|
| 720p incompressible noise, 3 x 60 s | 17/17/17 FPS | Raw USB-bound; did not meet 20 FPS |
| 720p moving/compressible, 3 runs | 29/30/29 FPS | Approximately 7.3 KB LZ4 payloads |
| 1080p moving/compressible control | 15 FPS | Requested rate met; approximately 16.3 KB payloads |
| 1080p representative stable desktop | 19-20 FPS | Approximately 645 KB payloads |
| 720x1520 scaled | approximately 5 FPS in older runs | CPU scaler around 152 ms p50 |

These results demonstrate why a single “GUD FPS” number is misleading: the
content's compressibility and whether scaling is required determine the
dominant stage.

### Thirty-minute Lomiri soak

The direct Mir RGB565 + LZ4 1280x720 path completed a 1,800-second managed
soak:

- overall presented rate: 24.474 FPS;
- early/middle/late rates: 25.603/25.428/25.367 FPS;
- transaction E2E: 48.976 ms p50, 62.728 ms p95, 131.012 ms max;
- `mirgud` RSS: 21,220-22,232 KiB, approximately zero slope;
- xdispd RSS, fds, and threads: flat;
- no unexplained short completions, timeouts, poison, OOM, kernel fault, or
  transport failure; and
- presenter bounds remained one pending and one in-flight.

The large difference between Mir's source count and presented count is the
intentional latest-frame policy, not a leaked FIFO.

Primary evidence:
[RGB565/LZ4 soak](evidence/xdisp-e2-t04-rgb565-lz4-soak-20260824T033632Z/final-report.md).

### Laptop GUD observations

The Fedora Asahi KDE laptop bound the in-tree `gud` driver and exposed the Pi
as connector `USB-1`. After reseating the USB hub, KDE showed a genuine
extended desktop on the external monitor. A short live sample was interpreted
as approximately 13.6-16.3 presentation updates/s; the user reported that it
felt responsive and sufficiently low-latency for coding.

This session did not retain the controlled workload, warmup, repetitions,
host CPU, or physical latency needed for a benchmark claim. Treat it as a
functional and subjective usability result only. Earlier laptop controls also
proved sustained compressed 1920x1080 content and established KDE as the
correct multi-GPU acceptance compositor.

## GUD lifecycle and failure findings

### Bounded freshness and teardown

- The producer/presenter uses one in-flight and one newest pending frame.
- Physical USB detach can return `-71`; xdispd contains the known Mir release
  stall with a roughly 3-second deadline and reaps the child.
- Absence of a GUD device does not spawn a child or create a respawn loop.
- Pi FunctionFS ownership distinguishes Idle, accepted/in-flight, resolved
  error, and Poisoned states. An accepted ambiguous transfer is not blindly
  retried.

The B/C/E lifecycle qualification completed 3/3 pre-bulk fault runs and two
real detach runs with zero stale children, zombies, uncontrolled respawns,
unsafe receiver restarts, or transport-timeout changes.

Primary evidence:
[lifecycle completion](evidence/xdisp-e2-t05-bce-completion-20260824T135743Z/final-report.md).

### OnePlus USB-host reconnect

The original reconnect failure was not a Pi display or GUD-format failure.
After a physical detach, the OnePlus could remain logically in host mode with
a stale xHCI root port. Charger-present was not a trustworthy reattach signal.

The implemented recovery is event-driven:

- the host GUD backport observes a proven stable physical root-port reattach;
- it emits one tagged event;
- a userspace helper performs one bounded `host -> device -> host` controller
  rebuild; and
- no periodic or blind role flipping is used.

Qualification passed three inactive cycles and one active RGB565+LZ4 cycle
without manual role flips, phone reboot, Pi restart, or recovery loop. Tagged
reattach to recovered GUD took about 31 seconds in that run.

Primary evidence:
[OnePlus USB-role recovery](evidence/xdisp-e3-b02-oneplus-usb-role-recovery-20260825T144602Z/final-report.md).

### Last frame after disconnect

A physical disconnect initially produces FunctionFS `SUSPEND`, not an
immediate terminal `DISABLE`. The final AIO safely drains to Idle, but VC4
continues scanning out the persistent last framebuffer. A delayed `DISABLE`
on reconnect restores the waiting screen. This is a display-cleanup/UX issue,
not proof of stale USB ownership and not the cause of the OnePlus reconnect
failure.

Primary evidence:
[Pi disconnect scanout lifecycle](evidence/xdisp-e3-b04-pi-disconnect-scanout-lifecycle-20260825T164000Z/final-report.md).

## H.264 development findings

### Initial physical POC

The first end-to-end physical proof used Mir, Qualcomm Venus, MPEG-TS over a
temporary USB ECM link, Pi `bcm2835-codec`, DRM_PRIME, and VLC `drm_vout`.

- local Venus encode: 900/900 at 29.97 FPS;
- final physical live run: 900/900 at 29.88 FPS and 6.49 Mbit/s;
- Pi decoder capacity test: approximately 64 FPS;
- largest observed access unit: 1,645,040 B in the final live run and up to
  approximately 1.79 MiB in exploration;
- Pi FFmpeg's default 1,632,256-byte compressed buffer dropped large IDRs;
  requesting 2 MiB fixed the overflow; and
- USB ECM MTU 1500 stalled on this DWC2 path, while MTU 512 sustained
  68.8 Mbit/s and was still far above the video rate.

This proved hardware feasibility, not acceptable latency. Verbose VLC logging
itself reduced throughput to approximately 26.6 FPS.

Primary evidence:
[H.264 USB POC](../mir-android2-platform-gud/evidence/h264-usb-poc-20260827T183841/REPORT.md).

### Color and receiver architecture

The correct source conversion is progressive BT.709 limited-range NV12 with
2x2 chroma averaging. Venus accepts V4L2 color controls but does not emit useful
H.264 VUI color metadata. Explicit BT.709-limited decoding produced expected
black, white, gray, red, green, blue, cyan, magenta, and yellow samples.

A diagnostic Pi path that decoded to CPU RGB565 ran at only 9.8 FPS and built
latency. That result was a software colorspace-conversion bottleneck, not a Pi
H.264 hardware limit. The final receiver therefore exports native decoder
NV12 buffers, imports them through DRM PRIME, and scans them out without
FFmpeg, VLC, or CPU YUV-to-RGB conversion.

H.264 4:2:0 necessarily averages one-pixel alternating chroma. Raising bitrate
cannot restore tiny red/blue text edges. GUD RGB565 is therefore visually
sharper for colored terminal/editor text.

Primary evidence:
[quality/latency report](../mir-android2-platform-gud/evidence/h264-quality-latency-20260827T020000Z/REPORT.md).

### Baseline Lomiri H.264 bottleneck characterization

Three 3,900-frame runs established the pre-optimization baseline:

| Metric | Run 1 | Run 2 | Run 3 |
|---|---:|---:|---:|
| Source/encode/receive FPS | 24.82 | 25.05 | 25.68 |
| Presentation-submit FPS | 18.70 | 18.65 | 22.05 |
| RGB->NV12 p50/p95 | 32.692/33.932 ms | 32.566/33.883 ms | 32.163/32.506 ms |
| Venus p50/p95 | 85.879/90.109 ms | 85.140/89.562 ms | 83.337/85.166 ms |
| TCP send p50 | 0.254 ms | 0.243 ms | 0.249 ms |
| Pi decode p50/p95 | 46.925/163.564 ms | 47.762/149.517 ms | 41.204/85.026 ms |
| Pi DRM submit p50/p95 | 8.744/16.322 ms | 8.877/16.423 ms | 8.346/16.031 ms |
| Decoded frames replaced | 962 | 996 | 552 |

Every run sent, received, and decoded all 3,900 access units. The first rate
limit was phone capture/conversion/completion serialization. The lower visible
update rate came from overly aggressive receiver freshness coalescing plus
decoder/DRM scheduling. TCP/Wi-Fi was not a bottleneck.

Venus's approximately 85 ms per-frame latency did not imply an 11-12 FPS
throughput limit because multiple buffers were in flight.

Primary evidence:
[live bottleneck report](../mir-android2-platform-gud/evidence/h264-live-bottleneck-20260828/final-report.md).

### Mir and Venus format audits

The 1080p public Mir audit found that Mir could produce ABGR8888, XBGR8888,
ARGB8888, XRGB8888, RGB888, and RGB565 for this H.264 experiment; BGR888 was
rejected. The difference from the earlier GUD audit is real: the audits were
performed at different resolutions/code revisions and exercised more request
variants, so each claim is scoped to its evidence.

Venus accepted NV12, Q128/NV12-UBWC, RGB4, NV21, Q12A/TP10-UBWC, and
QP10/P10-Venus. It rejected the relevant alternative packed RGB/RGBA,
RGB888/BGR888, RGB565, and NV12M requests.

RGB4 was the only plausible direct overlap. It accepted `S_FMT` and produced
H.264, but live Mir and deterministic ABGR, XBGR, ARGB, and XRGB layouts all
decoded with the same corruption. Direct Mir RGB to Venus was therefore
rejected on correctness, not merely performance.

### Retained Lomiri H.264 optimizations

Three production changes were implemented:

1. AArch64 NEON ABGR-to-NV12 with a byte-exact scalar fallback.
2. Independent Venus-completion/TCP draining with four preallocated bounded
   buffers, so capture/conversion/QBUF no longer waits for completion/send.
3. Event-driven Pi decoder draining plus exactly one in-flight and one newest
   pending presentation buffer.

Converter microbenchmark:

| Converter | p50 | p95 | Speedup |
|---|---:|---:|---:|
| Scalar reference | 12.758 ms | 17.622 ms | 1.00x |
| NEON | 3.957 ms | 8.730 ms | 3.22x p50 / 2.02x p95 |

The live conversion interval remained 26.911-28.335 ms p50 because public-Mir
mapped-buffer reads and scheduling dominate beyond the arithmetic kernel.

Combined 2,100-frame runs:

| Metric | Run 1 | Run 2 | Run 3 |
|---|---:|---:|---:|
| Source/encode/receive/decode FPS | 29.982 | 29.978 | 29.975 |
| Presentation-submit FPS | 29.039 | 29.821 | 29.461 |
| Venus p50/p95 | 46.489/68.895 ms | 46.799/67.951 ms | 46.773/65.317 ms |
| Pi decode p50/p95 | 29.731/124.761 ms | 25.968/73.613 ms | 28.387/113.713 ms |
| Decoded replacement | 3.143% | 0.524% | 1.714% |

All 6,300 combined access units arrived and decoded. Maximum phone occupancy
was 3-4, with zero encoder-buffer waits. Phone sender CPU was 71.70% of one
logical core in the standalone CPU run; Pi receiver CPU was 2.15-2.27% of one
core. Phone temperature was 44.6 C, Pi 45.1-46.2 C, and neither throttled.

Primary evidence:
[optimization report](../mir-android2-platform-gud/evidence/h264-optimization-20260828T132426Z/final-report.md),
[results CSV](../mir-android2-platform-gud/evidence/h264-optimization-20260828T132426Z/results.csv),
[correctness](../mir-android2-platform-gud/evidence/h264-optimization-20260828T132426Z/correctness.txt),
and [safety invariants](../mir-android2-platform-gud/evidence/h264-optimization-20260828T132426Z/safety.txt).

## Laptop H.264 implementation and tests

### Capture approach

The laptop is Fedora KDE Wayland. FFmpeg had `kmsgrab` but no PipeWire input;
GStreamer had `pipewiresrc`, x264, OpenH264, and RFB source support. A portal
capture would require interactive session/fd handling, while KDE already
shipped `krfb-virtualmonitor` as its supported virtual-output provider.

The implemented sender therefore:

1. launches `krfb-virtualmonitor` at 1920x1080;
2. consumes the password-protected loopback RFB stream with GStreamer;
3. retains at most one capture buffer;
4. converts to I420 and encodes constrained-baseline Annex-B H.264 using x264
   ultrafast/zerolatency, no B-frames, and a one-second keyframe interval;
5. receives one complete access unit per appsink buffer;
6. prefixes the 32-byte `MH264FRM` sequence/timestamp/length header; and
7. sends it with `TCP_NODELAY` to the persistent Pi receiver on port 5505.

The KDE output was confirmed as `Virtual-H264-to-Pi`, 1920x1080@60, positioned
at `1512,0` beside the scaled internal panel. The encoder timeline is capped at
30 FPS.

Implementation:
[sender](../mir-android2-platform-gud/src/utils/laptop_h264_sender.py),
[wire protocol](../mir-android2-platform-gud/src/utils/h264_frame_protocol.py),
[laptop service](../mir-android2-platform-gud/systemd/laptop-h264-to-pi.service),
and [Pi service](../mir-android2-platform-gud/systemd/pi-h264-live-receiver.service).

### Laptop H.264 failures and fixes

- With damage-driven RFB and `videorate drop-only=true`, a mostly static KDE
  desktop produced approximately 9.4-10.0 encoded updates/s. This was source
  damage cadence, not encoder, network, or decoder saturation.
- Normal `videorate` now repeats the most recent image to create a continuous
  30 FPS stream. Repetition does not invent motion that the RFB server failed
  to capture; it only makes encode/transport/decode cadence continuous.
- x264's otherwise valid timing VUI caused the Pi `/dev/video10` decoder to
  return `EPIPE` after two frames. `insert-vui=false` fixed the hardware path.
  The receiver already forces limited-range BT.709 on the DRM plane.
- The first service start can race the Pi listener restarting after an old
  sender disconnects. Both persistent units use bounded restart delays and
  reconnect automatically.

### Laptop H.264 validation results

Isolated qualification:

- protocol unit tests: 3 passed;
- Python syntax/import checks: passed;
- local framed-stream validator: 90/90 access units, 9.59 damage-driven FPS,
  3 keyframes, valid Annex-B, access-unit sizes 120-265,738 B; and
- offline FFmpeg decode: passed; constrained baseline, yuv420p, level 4.0,
  1920x1080.

Damage-driven live sample before continuous mode:

- 419/420 decoded/presented at the Pi;
- decoder queue held at one frame;
- relative pipeline proxy settled around 32-38 ms;
- Pi receiver CPU approximately 1.3%; and
- Pi approximately 43.5 C, unthrottled.

Continuous-mode live sample:

- sender 29.9-30.2 FPS;
- 1,230 received, 1,228 decoded, 1,202 presented in the retained settled
  observation;
- presentation approximately 29.3 FPS;
- decoder input/metadata queue bounded at 1-2;
- Pi receiver CPU approximately 3.3%; and
- Pi 45.1 C, `get_throttled=0x0`.

The presenter coalesced repeated/late decoded buffers instead of allowing a
FIFO to grow. A startup burst briefly produced a 233 ms relative-age proxy,
then the queue recovered and remained bounded. This is not input-to-photon
latency.

These laptop numbers came from the interactive implementation session's live
service journals and protocol validator. They have not yet been packaged as a
timestamped, checksummed evidence bundle or repeated with a controlled motion
workload, so they are a functional performance observation rather than a final
comparative benchmark.

Commits in `mir-android2-platform-gud`:

- `0fb1c85` — KDE virtual-display H.264 sender and persistent services;
- `e08c095` — continuous 30 FPS emission.

Operational documentation:
[laptop H.264 runbook](../mir-android2-platform-gud/doc/laptop-h264-to-pi.md).

## Cross-path comparison

| Property | GUD | H.264 |
|---|---|---|
| Pixel representation | Lossless RGB565 | Lossy 4:2:0 NV12/H.264 |
| Best use | Text, terminals, exact colored edges | Motion, photos, low bandwidth |
| Main transport tested | USB2 bulk | TCP over Wi-Fi; early POC also USB ECM |
| Lomiri 1080p representative | 19-20 submits/s | 29.039-29.821 submits/s optimized |
| Lomiri source cadence | Mir produces approximately 61 FPS; presenter coalesces | 29.975-29.982 FPS combined path |
| Laptop result | Functional KDE extended display; informal 13.6-16.3 updates/s | Continuous 29.9-30.2 sender, approximately 29.3 presentation FPS |
| Content sensitivity | Very high through LZ4 ratio | Bitrate/AU size sensitive, usually much smaller |
| Raw bandwidth ceiling | Approximately 40 MB/s | Transport not limiting in qualified runs |
| Known visual limitation | RGB565 quantization, but no chroma subsampling | Small colored text softened by 4:2:0 |
| Known source bottleneck | GUD compression/host present at representative 1080p | Mir map/conversion on phone; loopback RFB capture on laptop |
| Known Pi bottleneck | CPU scaler for non-native modes | Decoder jitter and SETPLANE scheduling |
| Hotplug maturity | Substantial FunctionFS/USB-role qualification | Persistent TCP services; unplug/replug recovery not fully qualified |

The laptop rows are not a controlled USB-versus-Wi-Fi A/B. That comparison
still requires the same KDE scene, motion script, duration, warmup, CPU/thermal
sampling, and physical latency method on both paths.

## What is and is not a bottleneck

### Lomiri GUD

- Direct Mir RGB565 acquisition is not the 1080p presentation limiter; it can
  produce approximately 61 FPS.
- Representative 1080p is dominated by LZ4 plus synchronous host GUD present,
  approximately 45-46 ms combined.
- Incompressible content reaches the approximately 40 MB/s raw USB ceiling.
- Synthetic non-native scaling is Pi CPU-bound around 152 ms p50.

### Lomiri H.264 bottlenecks

- The old approximately 25 FPS limit was phone-side serialization and
  conversion, not Wi-Fi.
- The optimized phone path reaches the 30 FPS Mir cadence target.
- Venus latency is pipelined and must not be inverted into a throughput number.
- Pi decode is the largest variable receiver stage, but every submitted frame
  completed in the sustained qualifications.
- Latest-frame presentation deliberately replaces stale pending decoded
  buffers; this is bounded freshness, not transport loss.

### Laptop GUD bottlenecks

- The short 13.6-16.3 updates/s session did not retain enough stage timing to
  attribute a hard bottleneck.
- Based on the qualified Lomiri GUD path, content-dependent LZ4 and synchronous
  GUD present/USB time are the leading candidates, but that remains an
  inference until a controlled laptop run records both host and Pi timings.
- The user's low-latency coding impression is compatible with latest-frame
  coalescing: a lower update count does not necessarily imply a long FIFO.

### Laptop H.264 bottlenecks

- The initial approximately 10 FPS was damage-driven RFB output on a mostly
  static desktop.
- Continuous duplication makes the transport 30 FPS, but cannot recover motion
  never emitted by RFB.
- The settled Pi can decode essentially 30 FPS, while presentation coalesces a
  small fraction and remains around 29 FPS.
- A direct KWin/PipeWire capture path would be the next architectural
  improvement because it removes the loopback RFB stage.

## Rejected or superseded approaches

| Approach | Result | Reason/status |
|---|---|---|
| Old small-payload GUD planner | Superseded | Logical limit was caused by FunctionFS allocation behavior, not protocol |
| Single large `kmalloc` FunctionFS AIO | Failed at 327,680 B | Order-7 contiguous allocation pressure |
| Multiple GUD payload ownership to work around USB | Rejected | Would weaken unambiguous accepted-I/O semantics |
| CPU software framebuffer damage diff | Not implemented | Extra full-frame memory work; no native damage source |
| Pi CPU scaling for synthetic modes | Correct but slow | Approximately 152 ms p50 |
| GNOME Wayland as laptop GUD host | Not viable in tested setup | Mutter secondary-DRM rendering limitation |
| Direct Mir RGB into Venus RGB4 | Rejected | All plausible 32-bit layouts decoded corruptly |
| Pi H.264 decode followed by CPU RGB565 conversion | Rejected | Approximately 9.8 FPS and stale-frame accumulation |
| FFmpeg/VLC queued media receiver | Superseded | Probe/logging/buffering behavior obscured low-latency display semantics |
| x264 timing VUI on Pi | Rejected | `/dev/video10` returned `EPIPE`; explicit DRM color configuration retained |
| Damage-only laptop H.264 cadence | Superseded as default | Approximately 10 FPS on static desktop; continuous 30 FPS requested |
| Interpret relative age as input-to-photon | Invalid | Phone/laptop and Pi clocks are unsynchronized; no physical scanout stamp |

## Test inventory

### GUD

- Rust unit suite: 174 tests passed at the exact-mode qualification point.
- C contract suites: LZ4, LZ4 contract, and KMS mode-sequence checks passed.
- Cross-compiled AArch64 release builds passed.
- Payload ladder: single/repeat gates through 1,843,200 B, plus 3,686,400 B
  full-frame 100/100 in the full-frame qualification.
- Controlled raw, motion, representative-desktop, exact-mode, scaled-mode,
  and deterministic pixel-pattern tests.
- Thirty-minute resource soak with 120 samples.
- Physical detach, absent-device, injected pre-bulk error, reconnect, mode
  cycle, and same-mode idempotence tests.
- Kernel/user ownership telemetry checked short completions, ambiguous accepted
  I/O, timeout, poison, unsafe teardown, and epoch retirement.

### Lomiri H.264 tests

- Public Mir format audit and actual returned-layout verification.
- Venus `ENUM_FMT`, `TRY_FMT`, and `S_FMT` audit.
- Four deterministic direct-RGB layout rejection trials.
- Scalar-versus-NEON byte-exact tests including stride, tail, flip, and guard
  bytes.
- Presenter state-machine unit test for idle, in-flight, pending replacement,
  completion, shutdown, and decoder-buffer lifetime.
- AddressSanitizer/UndefinedBehaviorSanitizer runs.
- Three 3,900-frame baseline and three 2,100-frame combined hardware runs.
- Clean 30-frame receiver resource-teardown regression.

### Laptop H.264 tests

- Three wire-protocol unit tests.
- Python compile/import checks.
- Ninety-access-unit local TCP framing validator and offline FFmpeg decode.
- Initial hardware rejection test with VUI, corrected no-VUI hardware test,
  persistent service replacement/reconnect test, damage-driven sample, and
  continuous 30 FPS sample.

## Current runtime at documentation time

At the end of the 2026-08-28 laptop qualification:

- laptop `laptop-h264-to-pi.service`: enabled and active;
- KDE output `Virtual-H264-to-Pi`: active at 1920x1080;
- Pi `h264-live-receiver.service`: enabled, active, and connected on TCP 5505;
- Pi `gud-userspace.service`: active as the DRM owner and available for GUD,
  but GUD pixels are not the selected H.264 plane;
- phone transient H.264 sender: inactive; and
- Pi: approximately 45 C and unthrottled.

Runtime state is inherently temporary; use the commands below rather than
assuming this snapshot is still current.

## Operational checks

Laptop H.264:

```sh
systemctl --user status laptop-h264-to-pi.service
journalctl --user -u laptop-h264-to-pi.service -f
kscreen-doctor -o
```

Pi receiver and gadget:

```sh
systemctl status h264-live-receiver.service gud-userspace.service
journalctl -u h264-live-receiver.service -f
cat /sys/class/udc/3f980000.usb/state
vcgencmd get_throttled
```

Stop laptop H.264 and remove its virtual monitor:

```sh
systemctl --user stop laptop-h264-to-pi.service
```

Start it again after the Pi listener is ready:

```sh
systemctl --user start laptop-h264-to-pi.service
```

GUD deployment and status procedures remain in
[docs/DEPLOY.md](docs/DEPLOY.md),
[docs/PERFORMANCE_INSTRUMENTATION.md](docs/PERFORMANCE_INSTRUMENTATION.md),
and the scripts under [`scripts/`](scripts/).

## Open work

1. Run a controlled laptop GUD/USB versus laptop H.264/Wi-Fi A/B using the
   same deterministic coding and motion scenes.
2. Add synchronized physical input-to-photon measurement; do not promote
   relative monotonic offsets as latency.
3. Replace laptop loopback RFB with direct KWin/PipeWire capture and measure
   actual new-frame cadence separately from repeated-frame cadence.
4. Determine whether a usable hardware H.264 encoder becomes available on the
   Asahi laptop stack; retain x264 as the compatibility path.
5. Improve Lomiri H.264 source-memory access/conversion without sacrificing
   BT.709 correctness; GPU/hardware NV12 conversion is the main candidate.
6. Fix the known Lomiri H.264 crop/orientation defect independently of
   performance work.
7. Add an explicit Pi scanout-arbitration service so GUD and H.264 ownership
   changes restore the intended framebuffer deterministically.
8. Add a display-safe waiting-screen transition for GUD `SUSPEND` without
   weakening FunctionFS ownership/epoch rules.
9. Optimize or avoid the Pi CPU scaler for synthetic non-native GUD modes.
10. Complete H.264 disconnect/reconnect and long-soak qualification with the
    same rigor as the GUD lifecycle suite.

## Evidence index and status language

- **Qualified benchmark:** controlled workload and retained evidence with
  explicit duration/counters.
- **Qualification:** bounded functional/correctness/safety gate, not
  necessarily a throughput benchmark.
- **Observation:** useful live behavior without enough controls for a general
  performance claim.
- **Proxy:** software timestamp that does not measure physical scanout.

The authoritative raw evidence remains in this repository's `evidence/` tree
and the sibling `mir-android2-platform-gud/evidence/` tree. This document is a
cross-project index and interpretation; if a number conflicts with a retained
CSV or final report, the scoped source artifact takes precedence.

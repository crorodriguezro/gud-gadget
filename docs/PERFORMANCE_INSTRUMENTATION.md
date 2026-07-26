# Performance Instrumentation

This document describes the frame-timing instrumentation currently present in the userspace GUD driver.

## Purpose

The instrumentation identifies which stage of the userspace GUD gadget
pipeline is the bottleneck for each payload. In the `XDISP-P0.1` setup this
code runs on the Raspberry Pi; the OnePlus is the USB host.

1. USB receive
2. LZ4 decompression
3. framebuffer copy
4. DRM dirty flush
5. total frame handling time

This is useful when deciding whether the real limit is:

- USB transfer bandwidth
- decompression CPU cost
- framebuffer copy cost
- panel / DRM flush cost

## Where It Is Implemented

- [`gadget/src/lib.rs`](../gadget/src/lib.rs)
- [`drm/src/main.rs`](../drm/src/main.rs)

## Instrumented Data

The gadget-side receive path tracks:

- `transfer_bytes`
  - actual number of bytes transferred over USB for the frame payload
- `output_bytes`
  - expected uncompressed pixel payload size
- `payload_seq`
  - process-local sequence number for correlating per-read and frame logs
- `read_size`
  - configured maximum FunctionFS read size
- `read_calls`
  - number of completed userspace FunctionFS reads
- `first_request_bytes` / `last_request_bytes`
  - requested size of the first and final read for this payload
- `usb_packets_est`
  - estimated high-speed USB data packets, rounded up from the payload size
    and the 512-byte high-speed maximum packet size
  - this excludes bus retries and protocol overhead
  - it is meaningful only when the retained UDC evidence reports
    `current_speed=high-speed`
- `recv_ms`
  - time spent receiving USB payload data
- `decompress_ms`
  - time spent in LZ4 decompression
- `copy_ms`
  - time spent copying the received pixel payload into the mapped framebuffer
- `flush_ms`
  - time spent calling `dirty_framebuffer()`
- `total_ms`
  - total time from the start of buffer handling to the end of flush
- `usb_mib_s`
  - effective receive throughput based on `transfer_bytes / recv_ms`
- `ratio`
  - `output_bytes / transfer_bytes`
  - this is the effective compression ratio when compression is in use

## Log Format

Each completed frame logs a single line like this:

```text
frame_stats payload_seq=1 rect=1280x25+0,0 source=1280x720 scaled=false transfer_bytes=64000 output_bytes=64000 read_size=16384 read_calls=4 first_request_bytes=16384 last_request_bytes=14848 usb_packets_est=125 compression=0 ratio=1.00 recv_ms=8 decompress_ms=0 copy_ms=0 scale_ms=0 flush_ms=0 total_ms=8 usb_mib_s=7.63
```

Field meanings:

- `rect`
  - updated rectangle in `width x height + x,y` form
- `compression`
  - `0` means uncompressed
  - `1` means `GUD_COMPRESSION_LZ4`

Each FunctionFS read also emits structured start/completion records with its
`payload_seq`, read index, remaining bytes, requested/result bytes, and
duration in microseconds. A start without a matching completion identifies the
exact request that blocked.

## How To Read It

### If `recv_ms` dominates

The likely bottleneck is USB transfer or host-side send pacing.

### If `decompress_ms` dominates

The likely bottleneck is LZ4 decompression CPU time.

### If `copy_ms` dominates

The likely bottleneck is the userspace framebuffer copy path.

### If `flush_ms` dominates

The likely bottleneck is DRM dirty flushing or panel/display update behavior.

## Current Observations

The 2026-07-26 OnePlus raw-video baseline exposed a separate Pi presentation
bottleneck. A 1280x720 GUD source was software-scaled into a 1920x1080 physical
HDMI framebuffer. Ten frames became 113 rectangles; every rectangle triggered
a full-frame scale and back-buffer swap at roughly 39--40 ms, producing only
2.015 synchronous updates per second. USB receive and framebuffer copy did not
explain that cadence.

The next controlled gate selects an actual 1280x720 physical HDMI mode through
the test-only `GUD_TEST_OUTPUT_MODE=1280x720` override and repeats the identical
clip. The acceptance shape is `scaled=false`, `scale_ms=0`, no scaled
back-buffer swap, and exact matching `InFlight`/`Idle` counts. See
`XDISP-P2.1-NATIVE-SCANOUT-GATE.md`.

The corrected native gate passed. It retained the same 113 rectangles and
roughly 0.919 MB of compressed payload, but all 113 Pi entries reported
`scaled=false` and `scale_ms=0`. The mean of the logged whole-millisecond Pi
`total_ms` values was 0.504 ms per payload; host average commit latency fell
from 489.394 ms to 50.097 ms, and the workload reached its configured 5-fps
ceiling at 4.999 fps. This isolates per-rectangle full-frame
scaling/presentation as the earlier bottleneck. It does not measure maximum
unpaced throughput, Mir/Lomiri presentation, CPU utilization, or tearing.

Historical full-frame `1080x2280 RGB565` compressed gadget runs showed:

- `compression=1`
- `ratio` around `5.65`
- `recv_ms` around `47..65`
- `decompress_ms` around `13..16`
- `copy_ms` around `1..2`
- `flush_ms` around `0..1`
- `total_ms` around `66..84`

Interpretation:

- gadget-side framebuffer copy was not the bottleneck
- gadget-side DRM flush was not the bottleneck
- decompression is not free, but it is smaller than receive time
- the dominant cost is the transfer/receive stage

## Read-granularity performance decision

The user reported no noticeable visible-performance difference between the
historical 512-byte FunctionFS reads and the 16 KiB read ceiling during normal
extended-monitor use. The successful 1920x1080 laptop control proves
correctness and records service timing, but it was not a controlled A/B:
content, host, compression, update cadence, and instrumentation were not held
constant. Do not claim a frame-rate improvement from the read-size change.

Keep the 16 KiB setting for the active reliability investigation because it
reduces request churn and provides bounded, exact-length diagnostics.
Benchmarking belongs to `XDISP-P2.1`, after `XDISP-P0.1` is stable. That future
benchmark should:

- compare 512-byte and 16 KiB userspace reads with the same host, mode,
  compression setting, content, and run duration;
- include warm-up and repeated runs rather than subjective observation alone;
- record end-to-end presented FPS and frame drops, not only payload count;
- retain Pi `recv_ms`, `total_ms`, CPU, memory, and read-call distributions;
- retain host CPU, GUD errors, usbmon throughput, and USB negotiated speed; and
- report median and tail latency before selecting a performance default.

The 512-byte path is a future test-only comparison implemented with the
current poison/teardown containment; do not redeploy the old artifact. It is
not an authorized fallback or reliability fix during `XDISP-P0.1`.

## How To Collect Logs

On the Pi:

```bash
journalctl -u gud-userspace.service --no-pager -f | grep frame_stats
```

Over SSH:

```bash
ssh cristian@192.168.1.110 "journalctl -u gud-userspace.service --no-pager -f | grep frame_stats"
```

To capture a fixed sample:

```bash
ssh cristian@192.168.1.110 "journalctl -u gud-userspace.service --no-pager -n 300 | grep -E 'FunctionFS bulk OUT read|frame_stats'"
```

## Notes

- The instrumentation is gadget-side only; for `XDISP-P0.1` that means the Pi.
- It does not measure OnePlus compositor time or host `gud` driver scheduling.
- If visible FPS is lower than what `total_ms` suggests, the missing bottleneck is likely on the host side.

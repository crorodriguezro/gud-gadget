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

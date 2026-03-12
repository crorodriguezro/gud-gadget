# Performance Instrumentation

This document describes the frame-timing instrumentation currently present in the userspace GUD driver.

## Purpose

The instrumentation is meant to identify which stage of the phone-side display pipeline is the bottleneck for each frame:

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

- [gadget/src/lib.rs](/home/cristian/Projects/linux-driver/gud-gadget/gadget/src/lib.rs)
- [drm/src/main.rs](/home/cristian/Projects/linux-driver/gud-gadget/drm/src/main.rs)

## Instrumented Data

The gadget-side receive path tracks:

- `transfer_bytes`
  - actual number of bytes transferred over USB for the frame payload
- `output_bytes`
  - expected uncompressed pixel payload size
- `packets`
  - number of bulk packets consumed
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
frame_stats rect=1080x2280+0,0 transfer_bytes=870965 output_bytes=4924800 packets=1702 compression=1 ratio=5.65 recv_ms=54 decompress_ms=13 copy_ms=1 flush_ms=0 total_ms=71 usb_mib_s=15.38
```

Field meanings:

- `rect`
  - updated rectangle in `width x height + x,y` form
- `compression`
  - `0` means uncompressed
  - `1` means `GUD_COMPRESSION_LZ4`

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

In recent full-frame `1080x2280 RGB565` compressed runs, the logs showed:

- `compression=1`
- `ratio` around `5.65`
- `recv_ms` around `47..65`
- `decompress_ms` around `13..16`
- `copy_ms` around `1..2`
- `flush_ms` around `0..1`
- `total_ms` around `66..84`

Interpretation:

- phone-side framebuffer copy is not the bottleneck
- phone-side DRM flush is not the bottleneck
- decompression is not free, but it is smaller than receive time
- the dominant cost is the transfer/receive stage

## How To Collect Logs

On the phone:

```bash
journalctl -u gud-userspace.service --no-pager -f | grep frame_stats
```

Over SSH:

```bash
ssh cristian@192.168.1.106 "journalctl -u gud-userspace.service --no-pager -f | grep frame_stats"
```

To capture a fixed sample:

```bash
ssh cristian@192.168.1.106 "journalctl -u gud-userspace.service --no-pager -n 200 | grep frame_stats"
```

## Notes

- The instrumentation is phone-side only.
- It does not measure host compositor time or host `gud` driver scheduling.
- If visible FPS is lower than what `total_ms` suggests, the missing bottleneck is likely on the host side.

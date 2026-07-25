# FunctionFS bulk-transfer incident: AIO completion failure

## Scope

This document records the failure observed when a OnePlus 6 (Linux 4.9 GUD
host driver) sent framebuffer data to the Raspberry Pi GUD gadget. It covers
the two separate FunctionFS issues found during diagnosis, the startup race
that made testing unreliable, and the resulting fixes.

## User-visible symptom

The GUD device enumerated successfully on the OnePlus, including a completed
probe for USB ID `1d50:614d`. The first atomic framebuffer update then timed
out on the host:

```
gud ... GUD atomic update failed: -110
```

The Pi had accepted the `GUD_REQ_SET_BUFFER` control request, but the bulk OUT
payload never completed through the previous receive path.

## Original issue: native AIO on a FunctionFS bulk endpoint

`usb-gadget`'s `EndpointReceiver` uses its native asynchronous-I/O driver for
endpoint reads. On the Pi's DWC2 FunctionFS setup, the completion result was
invalid (reported as raw OS error `521728`) even though kernel tracing showed
the controller receiving and completing 512-byte bulk packets. In effect, the
USB controller and FunctionFS endpoint were moving data, but the userspace AIO
abstraction did not deliver a usable completion to `gud-drm`.

This is why enumeration worked: descriptor and control transfers use ep0. The
failure appeared only when the host started sending framebuffer bytes to the
bulk OUT endpoint.

## Next issue: reopening an enabled endpoint blocks

The first workaround changed the GUD receiver to use a blocking `File::read`.
It initially opened `/dev/ffs-usb-gadget0-0/ep1` for every receive path.
That did not work either: on this FunctionFS/DWC2 combination, opening a
second file handle for an enabled endpoint can block indefinitely. The host
then again timed out with `-110`, before the first blocking read could begin.

The endpoint had already been opened during FunctionFS initialization by
`usb-gadget`; the correct fix was to use that exact file handle rather than
opening the endpoint path again.

## Fixes

1. Vendor the pinned `usb-gadget` dependency and add
   `EndpointReceiver::file()`. It returns an `Arc<File>` for the initialized
   FunctionFS endpoint. Keeping the receiver alive also keeps the endpoint
   state and file lifetime valid.
2. In `PixelDataEndpoint`, retain that initialized endpoint file and, in the
   original repair, perform synchronous, bounded 512-byte `read()` calls until
   the GUD payload length has been received. This bypassed the unreliable
   native-AIO completion path and avoided the second-open deadlock.
3. Bind the USB gadget only after DRM has successfully acquired the CRTC,
   allocated/mapped framebuffers, and rendered the waiting screen. A temporary
   DRM permission/ownership failure previously exposed a half-started gadget
   and then tore it down while the host could be enumerating it. CRTC setup is
   retried before USB is exposed.
4. Keep detailed diagnostics around FunctionFS reads and frame processing so
   future regressions distinguish control, bulk, copy, scale, and flush work.

## Hardware verification

After rebooting the Pi, deploying the rebuilt binary, and forcing a fresh
OnePlus USB-device-to-host transition:

- the OnePlus freshly probed the GUD device;
- the Pi logged CRTC setup before USB binding;
- the Pi received every framebuffer tile as 512-byte bulk packets;
- representative 64,000-byte tiles completed in 5--11 ms;
- the Pi rendered the full 1280x720 update; and
- the OnePlus produced no new `GUD atomic update failed: -110` message.

The Pi's `frame_stats` entries are the authoritative evidence that payloads
were received, copied/scaled, and presented rather than merely enumerated.

## Step 5: larger aligned reads

Later rebind testing exposed a separate failure in the original blocking path.
The OnePlus completed a 64,000-byte host URB while the Pi entered its 512-byte
FunctionFS loop without an aggregate completion, followed by allocator
corruption during the active payload. The old logging does not identify which
of the 125 reads stalled. This made another hardware retry of that loop unsafe.

The Step 5 implementation keeps the same initialized endpoint file and
blocking API, but changes request granularity:

- `GUD_FFS_READ_SIZE` is validated as a 512-byte-aligned value from 4,096
  through 65,536 bytes;
- the default and tracked first-test value is 16,384 bytes;
- each syscall requests exactly `min(remaining_payload, read_size)`;
- the normal 64,000-byte tile is therefore four requests:
  `[16384, 16384, 16384, 14848]`;
- no userspace request is padded past the declared payload; and
- a short or zero-byte completion fails immediately instead of starting
  another blocking read.

The read logs now distinguish FunctionFS `read_calls` from the estimated USB
packet count and record each requested/result byte count. Unit tests cover the
16 KiB first-test strategy, a later 64 KiB A/B ceiling, the 51,200-byte final
tile, ceiling splits, an unaligned exact tail, short/zero reads, and endpoint
I/O errors. A caller-level test also proves that one receive error permanently
poisons the process before a second read attempt. An atomic
`Idle -> InFlight -> Idle/Poisoned` state machine also prevents `SIGTERM` from
unbinding DWC2 while a receive is blocked. A completion taking more than the
conservative one-second safety threshold is poisoned when it returns; a hung
read remains `InFlight`. Decompression and optional dumps run only after the
session returns to `Idle`, so slow/failed post-read work does not falsely
poison the endpoint. In-flight and poisoned processes refuse further
USB/control processing and automatic teardown and require
physical/hardware-reset recovery.

The 16 KiB first setting lowers contiguous-allocation pressure relative to a
one-request 64,000-byte read on this non-scatter-gather path; it does not prove
that allocation pressure caused or fixes the corruption.

The first 16 KiB hardware test failed on the first read. FunctionFS returned
`18446744073709045760` (signed `-505856`) for a 16,384-byte request. The
poisoned DWC2 state showed buffer DMA enabled, descriptor DMA disabled, and
ep1 OUT residual `0x7f800` (522,240) against a loaded length of `0x4000`
(16,384). The buffer-DMA calculation therefore underflowed exactly to
`0xfff84800`, the observed result. The service entered `Poisoned` and safely
parked without teardown; the Pi remained reachable and kernel-clean, while
the host logged bulk/atomic `-110`.

This rules out read-call count alone as the fix. `PAYLOAD_RC=0` also proved
insufficient as standalone evidence because the asynchronous host failure
appeared in the kernel after the utility reported success.

A later modern-laptop control completed 953 compressed payloads, 432.6 MB, and
27,053 of 27,053 exact 16 KiB-capped FunctionFS reads with `g_dma=1`. The
immediate failure is therefore conditional on host, transfer shape, timing, or
their interaction—not a universal DWC2 DMA or 16 KiB failure. Before compiling
a `g_dma=0` kernel, two test-only modern-host gates will use
`max_buffer_size=64000` at RGB565 1280x720: first with LZ4 retained to isolate
25-row tiling/control cadence, then after a clean result and fresh enumeration
with compression disabled to reproduce the OnePlus bulk lengths. If both pass,
focus on separately named OnePlus chunk-size/DMA-mapping diagnostic modules
while preserving the normal module.

The user reported no noticeable visible-performance difference from the read
size change. This is not a controlled benchmark; defer 512-byte-versus-16 KiB
performance testing to `XDISP-P2.1` and do not return to 512 bytes as a P0.1
fallback.

## Future improvements

- Upstream a minimal endpoint-file accessor or a supported synchronous receive
  mode to `usb-gadget`, replacing this local vendor copy when accepted.
- Add a hardware-in-the-loop regression test that sends a small known frame
  through FunctionFS and asserts both GUD completion and Pi frame statistics.
- Add an explicit FunctionFS/DWC2 capability probe or configuration flag so
  platforms with working AIO can opt into it deliberately.
- Add a bounded read timeout/cancellation strategy. While `XDISP-P0.1` remains
  blocked, a receive anomaly poisons the process and requires fresh-boot
  recovery rather than an automatic gadget restart.
- Investigate DRM dirty-framebuffer support on the Pi. It currently falls back
  to back-buffer swaps after `Function not implemented`, which is correct but
  less efficient than an available damage/flush path.
- Record transfer latency and error counters as metrics, not only debug logs,
  to make field diagnosis easier.

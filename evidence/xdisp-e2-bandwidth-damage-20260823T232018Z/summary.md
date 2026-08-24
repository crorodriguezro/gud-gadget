# Summary

1. The exact qualified FunctionFS change is packaged at
   `gud-gadget/docs/test-only/functionfs-large-aio-vmalloc.patch`; it applies
   cleanly to the qualified baseline and is reproducible from the captured Pi
   config. The clean rebuild was run from `.xfercompltrace-source` into a new
   output directory.
2. The half-frame correction is 113 actual USB requests per 1,843,200-byte
   payload and 226 requests per two-payload frame. The full frame remains 225
   requests. Raw logs were not rewritten.
3. The managed LZ4 sample produced 808 host/Pi logical payloads. Every host
   frame record had `source=3686400`, `rectangles=1`, and one compression
   decision; each Pi record had `output_bytes=3686400`, one read, and matching
   compressed or RAW transfer length. No BUSY, short completion, timeout,
   poison, or ownership ambiguity occurred.
4. Managed LZ4 actual payload bytes were 679,244 p50, 2,846,081 p95, and
   2,889,993 worst. Logical/actual ratio was 5.427210 p50, 254.814405 p95,
   and 1.275574 worst. The high-entropy control safely fell back to RAW.
5. Managed LZ4 host compression was 22.045 ms p50 / 62.719 ms p95; Pi
   decompression was 7 / 18 ms; USB bulk wait was 16.691 / 70.269 ms; E2E
   source-ready to worker-end was 71.788 / 165.117 / 203.668 ms p50/p95/max.
   The managed sample presented 807 frames in 60.795 s: 13.27 fps.
6. Native Mir damage is not exposed to this screencast/buffer-stream consumer,
   and the current GUD backport has no `FB_DAMAGE_CLIPS` path. No software
   framebuffer diff was added.

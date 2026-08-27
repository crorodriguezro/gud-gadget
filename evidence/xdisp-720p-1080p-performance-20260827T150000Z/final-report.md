# Final 720p / 1080p production benchmark

Result: **PARTIAL**. Six valid production-path runs were completed: three at
1280x720 and three at 1920x1080, each with 10 s warmup and approximately 60 s
measurement. The source was Mir at approximately 61 FPS, RGB565, and both
runs used DirectExact with scaler invocation count zero.

| Mode | Source FPS | Presenter FPS | USB/presentation FPS | LZ4 hit | Payload mean/p95 | LZ4 p50/p95 | Bulk p50/p95 | Sustained wire |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 1280x720 representative (r1–r3) | 61 | 28/21/21 | 28/21/21 | 100% | 633,319/633,319 B* | 25.3/30.8 ms* | 15.5/15.9 ms* | 13.6 MB/s* |
| 1920x1080 representative (r1–r3) | 61 | 20/20/19 | 20/20/19 | 100% | 645,375/645,375 B | 27.7/33.5 ms | 15.8/16.2 ms | 12.9 MB/s |

`*` 720p r2/r3 complete host windows; r1 host records were journal-tail
limited. Successful page-flip completion/vblank events are not available, so
these are presentation-submit FPS, not physically displayed FPS.

The 1080p representative path is not primarily limited by the raw USB ceiling:
payloads are about 645 KB, bulk transfer is about 16 ms, and LZ4 is about
28–34 ms. The measured host GUD present total is about 45–46 ms, matching the
approximately 20 FPS output; compression and presenter/GUD blocking are the
dominant measured stages. A controlled desktop-like animation and separate
60-FPS-target compressible Mir workload were not available in the deployed
tool (its deterministic generator is synthetic/direct-KMS and forbidden by
this task), so the four-configuration PASS criteria are not met.

Maximum demonstrated production-path rates here are 28 FPS at 720p and 20 FPS
at 1080p. These are stable-desktop observations, not deterministic workload
ceilings.

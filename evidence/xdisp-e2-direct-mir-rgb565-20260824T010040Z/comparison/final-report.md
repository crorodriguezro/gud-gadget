# Final report

1. Task: **PASS** — direct Mir RGB565 capability and bounded managed transport qualified.
2. `gud` HEAD before: `4a2eb91b0ea1a3404b38af75821250a14040ccbf`
3. `gud-gadget` HEAD before: `0af1ae3ace9452f12db5621812b3c89860ba55c7`
4. `mir` HEAD before: `13818a0ded045042cc9f955809fccfcfa896fb2e`
5. `gud` HEAD after: `4a2eb91b0ea1a3404b38af75821250a14040ccbf`
6. `gud-gadget` HEAD after: `0af1ae3ace9452f12db5621812b3c89860ba55c7`
7. `mir` HEAD after: `13818a0ded045042cc9f955809fccfcfa896fb2e`
8. Pixel-format capability documentation committed: **no commit created**; working-tree path [`mir-android2-platform-gud/docs/pixel-format-capabilities.md`](../../../../mir-android2-platform-gud/docs/pixel-format-capabilities.md). The roadmap entry is [`gud/PROJECT-ROADMAP.md`](../../../../gud/PROJECT-ROADMAP.md).
9. Mir version: `1.8.3`
10. Lomiri version: `0.5.0`
11. Graphics backend: Android2 / libhybris / Android HAL gralloc
12. Mir modified: **NO**
13. Lomiri modified: **NO**
14. Managed source format requested: `rgb565`
15. Managed source format actually delivered: `rgb565`, Mir enum 7, 2 Bpp, stride 2560 at 1280 px
16. Mir RGB565 packed bytes/pixel: **2**
17. Mir RGB565 stride: **2560 bytes** for 1280×720
18. mirgud performs RGB565 color conversion: **NO**; direct `memcpy` path, 1837/1837 RAW and 1838/1838 LZ4 direct-copy source frames, zero pack/expand/reorder frames.
19. Direct RGB565 source-only result: **PASS** — 1208 received/submitted/presented, 0 dropped, 19.867132 s, 60/60/61 FPS fields, `accounting_ok=true`.
20. Direct RGB565 physical GUD result: **PASS** — exact 1,843,200-byte RGB565 frame, one admission, complete KMS control path.
21. Physical color/orientation result: transport and scanout control passed; HDMI operator visual confirmation was **NOT OBSERVED** in this session.
22. RGB565 RAW logical bytes/frame: **1,843,200**
23. RGB565 RAW GUD admissions/frame: **1**
24. FunctionFS internal requests per full RGB565 frame: **113** = 112 × 16,384 + 1 × 8,192
25. RGB565 RAW actual USB bytes/frame: **1,843,200**
26. RGB565 RAW bulk p50/p95/max: **50/52/53 ms** `recv_ms`, 84 captured Pi frame records
27. RGB565 RAW E2E p50/p95/max: **61.616/72.026/72.026 ms**, 14 matched phone trace samples
28. RGB565 RAW presented FPS: **18.16** (542 / 29.834571 s)
29. RGB565 RAW dropped frames: **1,294**
30. RGB565 RAW failures: **0** capture, conversion, release, dump, GUD-submit, lifecycle, or transport failures
31. RGB565 LZ4 logical bytes/frame: **1,843,200**
32. RGB565 LZ4 GUD admissions/frame: **1**
33. RGB565 LZ4 compressed bytes/frame: **498,448 / 903,333 / 940,082 bytes** p50/p95/worst observed, 467 captured Pi frame records
34. RGB565 LZ4 compression ratio: **3.70 / 3.70 / 1.96** p50/p95/worst observed; maximum sparse-frame ratio was 95.03
35. RGB565 host compression cost: **not separately instrumented**; combined `submit_us` was 50/100/100 ms p50/p95/max over 677 submits and includes compression, USB submission, and completion.
36. RGB565 Pi decompression cost: **4/13/14 ms** p50/p95/max, 467 captured Pi frame records
37. RGB565 LZ4 bulk p50/p95/max: **16/28/37 ms** `recv_ms`, 467 captured Pi frame records
38. RGB565 LZ4 E2E p50/p95/max: **48.491/56.900/58.605 ms**, 23 matched phone trace samples
39. RGB565 LZ4 presented FPS: **22.62** (677 / 29.927901 s)
40. RGB565 LZ4 dropped frames: **1,160**
41. RGB565 LZ4 failures: **0** capture, conversion, release, dump, GUD-submit, lifecycle, or transport failures
42. Reference XRGB8888 RAW FPS: **~9**; existing E1 full-frame baseline
43. Reference XRGB8888 LZ4 FPS: **13.27**; existing full-frame managed baseline
44. Four-path comparison table: see [`comparison/summary.md`](summary.md)
45. Physical RGB565 quality: **NOT ASSESSED**; no HDMI operator observation was available
46. Observed RGB565 quality defects: **none recorded**; this is not a visual-quality claim
47. >=20 fps achieved: **yes**
48. Which mode achieved it: **RGB565 LZ4**
49. Bounded continuity run: **PASS** — real managed xdispd activation/deactivation, 30-second bounded run, clean lifecycle
50. Bounded continuity duration: **29.927901 s LZ4**; RAW was **29.834571 s**
51. Sustained FPS: **22.62 presented FPS** for RGB565 LZ4; RAW 18.16
52. Phone remained responsive: **yes** — SSH, D-Bus deactivation, service cleanup, and final state checks completed
53. Ambiguous accepted I/O count: **0**
54. Short completion count: **0**
55. Poisoned transition count: **0**
56. Timeout count: **0**
57. Final receiver: `aggregate_state=Idle`, `aio_state=Idle`, `currently_owned=false`
58. Dominant bottleneck in RGB565 RAW: serialized USB bulk transfer / downstream submission; 1.8432 MB frames took about 50–53 ms to receive
59. Dominant bottleneck in RGB565 LZ4: serialized submit/transport path; Pi decompression was only 4/13/14 ms p50/p95/max, while combined submit timing remained 50/100/100 ms
60. Recommended transport candidate: **DIRECT MIR RGB565 LZ4**
61. Is RGB888 the next experiment: **no**; direct RGB565 is already accepted, packed, direct-copied, and reaches the target with LZ4, while RGB888 lacks current transport plumbing
62. Exact next project milestone recommendation: E5-T03 apples-to-apples RGB565-vs-XRGB8888 quality/workload qualification, using direct Mir RGB565 LZ4 as the candidate; record the visual result before selecting a release default
63. E2-T04: **remain paused**; do not start it in this task
64. Commits created: **0**
65. Commits pushed: **NO**
66. Final phone configuration: production `/home/phablet/gud.ko`, SHA256 `b7d48055ff1a20bc9b409c7713a8e0e2f9c346c28bd1220cee25aaa14e64912e`; xdisp service active/enabled via `/home/phablet/xdispd-throughput-wrapper`; XDisp available, `ActivationRequested=false`, `ChildPid=0`, `/dev/dri/card1` present
67. Final Pi configuration: `gud-userspace.service` active/enabled, UDC configured, XRGB8888, RAW (`GUD_TEST_COMPRESSION=none`), 12,800-byte cap, 16,384-byte read size, 1280×720
68. Production-safe settings restored: **yes**
69. Evidence path: `gud-gadget/evidence/xdisp-e2-direct-mir-rgb565-20260824T010040Z/`

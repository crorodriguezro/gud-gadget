# E4-T07 final report

1. E4-T07 result: PASS-B
2. gud HEAD: `639a57e75bcdca666ac9610fa4ff9d057dc4bc2f` (uncommitted task changes present)
3. gud-gadget HEAD: `001a5da9e39ecdd3200bbe3a68ed313ff26adb2f`
4. mir HEAD: `1fbc01d7c8ea5e5da61efb1e11f8e874e23e49c1` (uncommitted task changes present)
5. current upstream Linux GUD source URL: `https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git`
6. upstream GUD source SHA/tag: `502d45774af09f1c681c754c4b7cdfb5d7f72fd9`
7. current Pi advertised max buffer size: 1,843,200 bytes
8. RGB565 1280x720 logical size: 1,843,200 bytes
9. XRGB8888 1280x720 logical size: 3,686,400 bytes
10. largest previously qualified logical GUD payload: 1,843,200 bytes in the current runtime/evidence; the task's provisional 3,686,400-byte statement is not advertised by the current gadget
11. old 12,800-byte requirement origin: empirical safe envelope for the old Pi DWC2/FunctionFS receive implementation, not GUD or xHCI protocol
12. current 12,800 requirement still necessary: NO
13. obsolete planner functions removed: bounded/ratio/predictive row-fit planner family, `gud_xdisp_plan_chunk*`, row hints, and ratio cache
14. obsolete module parameters removed: `xdisp_payload_limit`, `xdisp_target_policy`, `xdisp_ratio_cache`, `xdisp_bounded_discovery`, `xdisp_predictive_bounded`
15. host GUD additions/deletions before cleanup: task-start production update layer was 1,999 LOC; a textual diff against current upstream is not meaningful across DRM API generations
16. host GUD additions/deletions after cleanup: 398 insertions, 876 deletions versus task-start HEAD across the five production update files; 1,514 LOC, net -485 LOC
17. OnePlus-specific USB/DMA compatibility retained: coherent bounce buffer, explicit URB, `URB_NO_TRANSFER_DMA_MAP`, explicit completion/timeout, exact actual-length check
18. E1 safety-specific custom code retained: one serialized logical payload, bounded pre-bulk SET_BUFFER BUSY retry, no retry of ambiguous accepted I/O, disconnect/error containment
19. configuration A result: previously qualified at 22--24 FPS on the Mir RGB565+LZ4 path, but depended on obsolete 12,800-byte host planning
20. configuration B build result: PASS (exact 4.9 target module, symbols, sanitizer, LZ4/source/PM contracts)
21. configuration B hardware result: PASS on OnePlus 6 host and Pi Zero 2 W gadget
22. configuration B SET_BUFFER count per full frame: 1
23. configuration B logical bulk payload count per full frame: 1
24. configuration B RGB565 RAW result: PASS, exact 1,843,200-byte raw fallback
25. configuration B RGB565 LZ4 result: PASS, one attempt; representative compressed payload 50,696 bytes
26. configuration B FPS: direct-KMS incompressible 11.438 FPS; bounded Mir/LatestFramePresenter smoke 22 presented FPS
27. configuration B phone responsiveness: PASS; Mir capture held ~61 FPS with one pending frame while presentation coalesced at 22 FPS
28. configuration B Poisoned observed: NO
29. configuration B kernel fault observed: NO
30. async_flush implemented: YES, optional and default-off
31. configuration C build result: kernel PASS; local direct-presenter component PASS; established AArch64 Mir build not authorized
32. configuration C hardware result: kernel async PASS; direct Mir configuration C not deployed, so no PASS-C claim
33. configuration C producer submit p95: 0.842 ms (final 100-frame direct-KMS noise run)
34. configuration C producer submit max: 1.245 ms in the percentile run; 1.423 ms across recorded normal/detach runs
35. configuration C longest worker/USB stall tested: 3.727 seconds worker elapsed under a controlled 3.500-second pre-bulk pause
36. configuration C queue/pending-state bound: one pending framebuffer/damage state and one work item
37. configuration C stale-frame backlog observed: NO; intermediate states coalesced into the shared upstream-style shadow image
38. configuration C detach containment: PASS at kernel boundary during active 1,000-frame stream; explicit work quiescing, no UAF or deadlock
39. configuration C kernel fault observed: NO
40. LatestFramePresenter retained: YES
41. selected production architecture: B
42. selected GUD update model: synchronous negotiated-capacity full update
43. selected compression model: one exact LZ4 attempt per negotiated rectangle, same-rectangle raw fallback
44. selected backpressure boundary: userspace `LatestFramePresenter`
45. physical smoke-check result: transport/KMS RGB565 smoke PASS with no obvious corruption or fault; full visual geometry/channel qualification remains E4-T02
46. E4-T02 claimed complete: NO
47. roadmap E4-T07 added: YES
48. roadmap execution order updated: YES
49. E5 12,800 assumptions updated: YES
50. commits created: 0
51. pushed: NO
52. evidence path: `gud-gadget/evidence/xdisp-e4-t07-upstream-full-update-async-20260826T232713Z/`
53. SHA256 verification: `SHA256SUMS` generated and verified with `sha256sum -c`
54. exact next ticket: E4-T02 — Fix full-width content and channel correctness

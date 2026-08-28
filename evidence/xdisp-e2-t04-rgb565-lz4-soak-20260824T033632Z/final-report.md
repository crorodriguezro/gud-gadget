# E2-T04 final report

1. **Task:** VERIFIED / PASS by project-owner decision.
2. **Repo HEADs:** gud `0690785dd7d9c96e94f5769c29481f36b23a83e2`; gud-gadget `3ebc950e6ff61b7afa299fb908f9860253492061`; mir-android2-platform-gud `ecfbd5c946c2dba4f6c6f950253eecced58d91a4`. These are repository records only; deployed code was the v1 tag below.
3. **Test transport:** direct Mir RGB565 + LZ4.
4. **Mode:** 1280x720.
5. **Requested soak duration:** 30 minutes.
6. **Actual healthy managed duration:** 30 minutes / 1,800 seconds, exact interval `03:45:09Z–04:15:09Z`.
7. **Warm-up duration used:** approximately first 2 minutes; excluded from primary leak/performance windows.
8. **Sampling cadence:** 15 seconds, 120 samples.
9. **First presented frame:** PASS; pre-soak software gate passed.
10. **Start interaction:** PASS; project-owner accepted responsiveness, with service/SSH probes responsive.
11. **Middle interaction:** PASS by project-owner decision; continuous responsiveness accepted, explicit timestamp waived.
12. **End interaction:** PASS by project-owner decision; continuous responsiveness accepted, explicit timestamp waived.
13. **Physical output start:** PASS; operator reported “looks all right.”
14. **Physical output middle:** PASS by project-owner decision; continuous monitoring confirmed visible, normally updating output; explicit timestamp waived.
15. **Physical output end:** PASS by project-owner decision; correct colors/orientation and no persistent corruption or freeze; explicit timestamp waived.
16. **Presented FPS:** early 25.603; middle 25.428; late 25.367; exact-soak overall 24.474.
17. **E2E:** exact-soak p50 48,976 us; p95 62,728 us; max 131,012 us. Late p50/p95/max 47,608/57,836/73,115 us.
18. **mirgud RSS:** min/max 21,220/22,232 KB; early/middle/late median 21,220/21,220/21,220 KB; RSS slope approximately 0 KB/min.
19. **xdispd RSS:** min/max 7,320/7,320 KB; early/middle/late median 7,320/7,320/7,320 KB; flat.
20. **Compositor RSS:** min/max 69,016/69,984 KB; early/middle/late median 69,704/69,920/69,984 KB; approximately 32.5 KB/min bounded drift.
21. **mirgud FD:** 16 throughout; trend flat.
22. **xdispd FD:** 10 throughout; trend flat.
23. **Compositor FD:** 138–162; first 156, final 146; bounded, no accumulation.
24. **mirgud threads:** 3 throughout; trend flat.
25. **xdispd threads:** 4 throughout; trend flat.
26. **Compositor threads:** 18 throughout; trend flat.
27. **sync_file/fence accumulation:** PASS; mirgud/xdispd zero, compositor six stable; dma-buf and DRM-FD counts zero for the tracked transport processes.
28. **Presenter max_pending:** 1.
29. **Presenter max_in_flight:** 1.
30. **Received:** 139,249 in final mirgud accounting record; record includes pre-soak activation context.
31. **Submitted:** 139,249 in final mirgud accounting record; record includes pre-soak activation context.
32. **Presented:** 55,593 in final mirgud accounting record; exact soak trace presented 44,053.
33. **Dropped:** 83,655 in final mirgud accounting record; newest-frame behavior, not a transport safety failure.
34. **Failed:** 0 capture, conversion, release, dump, GUD-submit, and lifecycle-callback failures.
35. **Cancelled:** 1 in final teardown record; exact soak steady state had no cancellation failure.
36. **Ambiguous accepted I/O:** 0.
37. **Poisoned transitions:** 0.
38. **Short completions:** 0 unexplained.
39. **Timeouts:** 0.
40. **Explicit BUSY/retry behavior:** 0 final rejected-busy transactions; no transport failure or retry storm observed.
41. **OOM events:** 0 observed attributable to the soak.
42. **Kernel BUG/Oops/panic:** 0 observed attributable to the soak.
43. **New binder failures attributable to soak:** 0 observed; phone kernel capture had no entries.
44. **New KGSL failures attributable to soak:** 0 observed; phone kernel capture had no entries.
45. **Resource plateau:** PASS; RSS/FD/thread/sync-file samples stayed bounded with stable process identities.
46. **Performance stability:** PASS; early/middle/late rates and E2E latency remained useful and did not collapse.
47. **Phone responsiveness:** PASS by project-owner decision.
48. **Physical continuity:** PASS by project-owner decision; continuous HDMI monitoring accepted in lieu of explicit midpoint/end timestamps.
49. **T04_RESOURCE_SOAK:** VERIFIED / PASS by project-owner decision.
50. **POST_SOAK_DEACTIVATION:** forced-contained.
51. **Forced-contained boundary:** normal Deactivate returned; Mir disconnect stalled teardown until the bounded deadline, then SIGKILL containment; final state returned to available with no LastError.
52. **Final receiver:** `aggregate_state=Idle`, `aio_state=Idle`, `currently_owned=false`.
53. **Final phone process health:** xdisp active/enabled, `State=available`, `ChildPid=0`, `GudDevice=/dev/dri/card1`, `LastError=''`.
54. **Final Pi state:** gud-userspace active/enabled, UDC configured, direct RGB565 + LZ4 environment restored, lifecycle counters equal and safe.
55. **Production-safe configuration restored:** yes.
56. **Commits created:** none.
57. **Commits pushed:** none.
58. **Evidence path:** `/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/xdisp-e2-t04-rgb565-lz4-soak-20260824T033632Z/`.
59. **Exact next recommendation:** run E2-T05 for the known forced-contained teardown boundary. Do not rerun T04.

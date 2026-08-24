# E2-T05 final report

1. **Task:** NEEDS PROJECT-OWNER DISPOSITION.
2. **gud HEAD before:** `0690785dd7d9c96e94f5769c29481f36b23a83e2`.
3. **gud-gadget HEAD before:** `3ebc950e6ff61b7afa299fb908f9860253492061`.
4. **mir HEAD before:** `ecfbd5c946c2dba4f6c6f950253eecced58d91a4`.
5. **Current xdispd graceful-stop deadline:** 3 seconds (`g_timeout_add_seconds(3)`).
6. **Force-kill policy:** SIGTERM immediately, then SIGKILL after the single deadline; child-watch reaps.
7. **Timeout values changed:** NO. The test hook did not alter a timeout.
8. **Presenter destruction order:** stop marks stopping, drops pending, wakes worker, joins worker; then KMS is reset.
9. **Mir release order:** KMS/DRM teardown, screencast release, then Mir connection release.
10. **T05-A healthy cycles:** 10/10 bounded.
11. **Healthy graceful exits:** 0.
12. **Healthy forced-contained exits:** 10.
13. **Healthy stop duration:** p50 3269 ms / p95 3733 ms / max 3778 ms.
14. **Healthy stalled boundary:** `MIR_CONNECTION_RELEASE_BEGIN` with no completion.
15. **T05-B absent cycles:** not run; Pi endpoint refused authentication.
16. **Absent child-spawn loops:** 0 observed in the available phone state; absent-device qualification not claimed.
17. **T05-C detach runs:** 0/2 current interpretable runs.
18. **Detach result:** not run; historical July evidence is not counted as this T05 run.
19. **Detach phone responsiveness:** not applicable to a current T05-C run.
20. **Detach final xdispd state:** not applicable.
21. **T05-D mechanism:** test-only `MIRGUD_T05_SAFE_STALL=1`, after the first frame and before `drmModeAtomicCommit`.
22. **Safe-stall ownership proof:** hook emits `T05_SAFE_STALL_ENTER` before commit; no GUD request is admitted there.
23. **Safe-stall runs:** 3/3.
24. **Safe-stall containment:** 0 graceful / 3 forced-contained.
25. **Safe-stall duration:** p50 3149 ms / p95 3572 ms / max 3572 ms.
26. **T05-E mechanism:** no current narrowly controlled pre-bulk failure hook was exposed; not run.
27. **Pre-bulk failure runs:** 0/3.
28. **Pre-bulk failure containment:** not applicable; deterministic lifecycle tests cover recoverable child failure.
29. **T05-F Mir-disconnect stall:** REPRODUCED naturally.
30. **Final marker before stall:** `SCREENCAST_RELEASE_COMPLETE`.
31. **`mir_connection_release` completed:** no.
32. **xdispd forced containment:** yes.
33. **Force-containment duration:** 2792–3778 ms in T05-A; 2853–3572 ms in T05-D.
34. **Child reaped:** yes for every exercised run.
35. **Stale mirgud children:** 0 observed after exercised runs.
36. **Zombie children:** 0 observed.
37. **Respawn loops:** 0 observed.
38. **Presenter max_pending invariant:** PASS; existing tests and prior qualified runtime report max 1.
39. **Presenter max_in_flight invariant:** PASS; existing tests and prior qualified runtime report max 1.
40. **Ambiguous accepted I/O:** 0 observed in this T05 host-side qualification; Pi-side fresh counters unavailable.
41. **Poisoned transitions:** 0 observed.
42. **Short completions:** 0 observed in available evidence; fresh Pi counters unavailable.
43. **Transport timeouts:** 0 observed in available evidence; fresh Pi counters unavailable.
44. **Unsafe receiver restarts/unbinds:** 0.
45. **Phone compositor survived interpretable cases:** yes.
46. **Phone remained responsive:** yes by SSH/D-Bus health probes; no UI freeze observed.
47. **OOM events:** 0 observed in phone evidence collected.
48. **Kernel BUG/Oops/panic:** 0 observed in phone evidence collected.
49. **Resource/lifetime leaks:** none observed in exercised lifecycle; all child PIDs converged to zero.
50. **Healthy shutdown verdict:** bounded/safe, but graceful-path requirement not met.
51. **Degraded shutdown verdict:** PASS for the safe pre-ownership stall case.
52. **Forced-containment policy verdict:** SAFE for exercised cases; healthy-path acceptance requires owner disposition.
53. **E2-T05 final verdict:** NEEDS PROJECT-OWNER DISPOSITION.
54. **Owner acceptance required:** yes, because healthy deactivation consistently uses forced containment.
55. **Remaining lifecycle caveat:** Mir 1.8.3 connection disconnect can block after safe external-resource cleanup.
56. **Source changes:** one test-only safe-stall hook in `src/utils/gud_screencast.cpp`.
57. **Focused tests:** 50/50 xdisp tests passed; Android2 binary could not load its bundled Boost library outside its container; Python tests could not start because pytest is unavailable.
58. **Commits created:** mir `eedf520a247eb1baf73472fa6e86488b5ea8be4c` (`xdisp: add safe pre-ownership shutdown stall hook`); evidence commit pending.
59. **Commits pushed:** NO.
60. **Final gud HEAD:** unchanged from before.
61. **Final gud-gadget HEAD:** unchanged from before.
62. **Final mir HEAD:** `eedf520a247eb1baf73472fa6e86488b5ea8be4c`.
63. **Final xdispd status:** active, enabled, `State=available`.
64. **Final ChildPid:** 0.
65. **Final compositor/LightDM health:** compositor and LightDM PIDs remained alive in the final phone probe.
66. **Final Pi service/UDC state:** not reverified because Pi SSH authentication was unavailable; no Pi mutation was performed.
67. **Final receiver:** not reverified; last accepted project evidence was `aggregate_state=Idle`, `aio_state=Idle`, `currently_owned=false`.
68. **Production-safe configuration restored:** phone yes; temporary safe-stall environment and binary were restored. Pi configuration was not changed.
69. **Evidence path:** `/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/xdisp-e2-t05-shutdown-containment-20260824T133342Z/`.
70. **Next recommendation:** restore Pi SSH/physical operator access, run T05-B/C/E, then obtain owner disposition on the repeatable healthy Mir-disconnect containment boundary before declaring E2 complete.

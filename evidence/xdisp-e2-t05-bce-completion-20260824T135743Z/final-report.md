# Final report

1. **Task:** PASS
2. **gud HEAD:** `0690785dd7d9c96e94f5769c29481f36b23a83e2`
3. **gud-gadget HEAD at test snapshot:** `b57e460a10da6b59f081436891d138764bbdd0a8`
4. **mir HEAD:** `0cd8f5bf8add9ad515ffbeedf43d2db019dc9153`
5. **Source changes required:** yes; one default-off T05 pre-bulk hook
6. **Timeouts changed:** no
7. **Mir modified:** no Mir sources modified
8. **Lomiri modified:** no

## T05-B

9. **Cycles:** 3/3 passed
10. **GUD genuinely absent:** yes; no `/dev/dri/card1`, `GudDevice=""`
11. **Activation behavior:** public Activate returned without a child; state stayed `unavailable`
12. **Child spawn count:** 0
13. **Child respawn-loop count:** 0
14. **Deactivate idempotent:** yes; two public Deactivate calls per cycle
15. **Final ChildPid:** 0 after every cycle
16. **Verdict:** PASS

## T05-C

17. **Physical USB detach performed:** yes; two real USB DATA detach runs
18. **Interpretable runs:** 2/2
19. **External display working before detach:** yes; managed presentation was active with frame traffic
20. **Phone responsive after detach:** yes; xdispd D-Bus, compositor, and session remained usable
21. **Compositor survived:** yes
22. **LightDM/session survived:** yes
23. **First transport result:** GUD request/bulk failure `-71`, followed by USB disconnect
24. **Ownership classification:** `CLEAN_RESOLVED_ERROR` in both runs
25. **Poisoned transition:** no; Pi telemetry remained `poisoned_transactions=0`
26. **Ambiguous accepted I/O:** 0
27. **xdispd containment:** existing deadline and SIGKILL path; stop durations 2962 ms and 3386 ms
28. **Mir disconnect stall reproduced:** yes; known release stall after safe cleanup
29. **Child reaped:** yes
30. **Final ChildPid:** 0 after both runs
31. **Respawn loop:** no
32. **Final xdispd state:** `unavailable` after detach
33. **Unsafe receiver restart/unbind:** no
34. **Verdict:** PASS

## T05-E

35. **Pre-bulk fault mechanism:** source hook enabled by `MIRGUD_T05_PREBULK_FAIL=1`; throws EIO after the first frame and before `drmModeAtomicCommit`
36. **Before accepted bulk ownership:** yes; no GUD SET_BUFFER or bulk request admitted for the injected failure
37. **Runs:** 3/3
38. **Explicit -EBUSY used:** no
39. **Retry count:** 0
40. **Ownership ambiguity:** 0
41. **Poisoned:** 0; all Pi checks were Idle/unowned
42. **Worker failure propagated:** yes; user-space worker recorded failed errno 5 and accounting remained valid
43. **xdispd containment:** public Deactivate plus existing deadline/SIGKILL path; stop durations 3111 ms, 3489 ms, and 2953 ms
44. **Child reaped:** yes, all three runs
45. **Phone responsive:** yes; compositor and LightDM/session survived
46. **Verdict:** PASS

## Global

47. **Stale mirgud children:** 0 after each case
48. **Zombie children:** 0 observed
49. **Uncontrolled respawn loops:** 0 observed
50. **Attributable OOM events:** 0 observed
51. **Attributable kernel BUG/Oops/panic:** 0 observed
52. **Transport timeouts changed:** 0
53. **Unsafe transport-rule violations:** 0
54. **Previous T05-A owner disposition valid:** yes
55. **Previous T05-D result valid:** yes
56. **Previous T05-F result valid:** yes
57. **Overall T05-B:** PASS
58. **Overall T05-C:** PASS
59. **Overall T05-E:** PASS
60. **E2-T05 final verdict:** PASS
61. **E2 overall:** complete for this requested B/C/E continuation
62. **Remaining caveat:** the known Mir disconnect-release stall remains, but existing xdispd deadline containment is effective
63. **Source commits created:** `0cd8f5bf8add9ad515ffbeedf43d2db019dc9153`
64. **Evidence commit created:** pending local evidence-only commit
65. **Commits pushed:** no
66. **Final phone xdispd:** live, D-Bus responsive, `State=available`, `GudDevice=/dev/dri/card1`, `LastError=""`
67. **Final ChildPid:** 0
68. **Final compositor/LightDM:** compositor and LightDM/session processes alive
69. **Final Pi service:** `gud-userspace.service` active
70. **Final UDC:** `3f980000.usb` configured and bound
71. **Final receiver:** Idle, AIO Idle, `currently_owned=false`, `poisoned_transactions=0`
72. **Production-safe configuration restored:** yes; production mirgud restored, T05 environment unset, temporary test files removed
73. **Evidence path:** `gud-gadget/evidence/xdisp-e2-t05-bce-completion-20260824T135743Z/`
74. **Next recommendation:** proceed to E3 hotplug/reconnect qualification; do not rerun T05-A/D/F

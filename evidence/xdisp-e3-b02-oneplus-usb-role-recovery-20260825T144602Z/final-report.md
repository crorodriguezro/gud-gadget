# E3-B02 Final Report

1. Task: **PASS**
2. initial gud HEAD: `276312b56ddef5e8cf581d5fa378fb133d7874bd`
3. initial gud-gadget HEAD: `1cd654fd345d4ec773e5c47414a86ae19b3cecc3`
4. initial mir HEAD: `991a8cdeb537ccd8ee1ef04804e216a4accbf8c0`
5. location/status of 991a8cd: `mir-android2-platform-gud`, current HEAD on `pixel-format-benchmark`; retained E4-T01 evidence commit
6. location/status of 562922f: `gud-gadget`, reachable historical commit `562922f` (`evidence: record E3-B01 physical reconnect result`), not current HEAD
7. OnePlus kernel: `4.9.112-g6b190d86b #1 SMP PREEMPT Thu Mar 12 00:01:48 UTC 2026 aarch64`
8. USB controller driver: Qualcomm glue `msm-dwc3` for `qcom,dwc-usb3-msm`; core `dwc3`; host child `xhci-hcd`
9. USB role/OTG driver: downstream Qualcomm `msm-dwc3` mode state machine
10. Type-C/extcon implementation: no `/sys/class/typec` or `/sys/class/usb_role` entries; extcon providers include `vendor:extcon_usb1`, PMI8998 SMB2/USB-PD PHY, MSM EUD, and display extcon
11. relevant sysfs role path: `/sys/bus/platform/devices/a600000.ssusb/mode`
12. historical `device -> host`: synchronously remove xHCI/root hubs through `mode=device`, wait for controller teardown/settle, then recreate host/xHCI/root hubs through `mode=host`
13. role owner before fix: downstream Qualcomm kernel OTG/extcon policy; no product userspace owner repaired a host-role controller that remained logically `host` but stale after reconnect
14. healthy initial attach: OTG host request -> msm-dwc3 host -> dwc3/xHCI child -> root hubs -> external hub -> Pi `1d50:614d` -> GUD probe -> DRM
15. active detach: GUD disconnect -> xdispd bounded containment -> root-port monitor armed once; controller remained logically host
16. failed reconnect: charger `PRESENT` became true while physically detached, the userspace-only helper consumed its transaction early, and the later real reconnect received no recovery action
17. earliest divergence: unreliable charger-PRESENT/generic-topology event was accepted before physical root-port reattach
18. manual recovery transition: `host -> device` removed stale xHCI/buses; bounded teardown+settle; `device -> host` recreated xHCI/root hubs and allowed GUD enumeration
19. root cause classification: **USERSPACE_POLICY_REQUIREMENT** (with a missing platform reattach/retrigger policy signal)
20. exact root cause: the platform can remain `mode=host` with a stale/non-enumerating xHCI root port after physical downstream reconnect, while charger PRESENT is not a reliable cable-reconnect signal; no existing owner performs one bounded controller rebuild after proven port detach/reattach
21. architectural owner: GUD kernel backport for bounded physical root-port observation; phone `mir-android2-platform-gud` helper for the one-shot role transaction
22. source changes: added `gud_reconnect.c/.h` and variant wrappers/Kbuild integration; arm on GUD disconnect, cancel on probe, emit one tagged stable-reattach uevent; helper/policy now accepts only that tag; packaging and 12 policy tests added
23. kernel changes: **yes**, GUD backport module only
24. userspace role-policy changes: **yes**
25. xdispd changes: **no**
26. Mir modified: **NO**
27. Lomiri modified: **NO**
28. FunctionFS epoch safety weakened: **NO**
29. transport/timeouts modified: **NO**
30. blind periodic role flip introduced: **NO**
31. role recovery event-driven: **yes**
32. duplicate event handling: **PASS**; one consumed removal, one tagged signal, one transaction, no retry
33. device-mode/charging safety considered: **PASS**; no startup action, non-host modes do not write, charger PRESENT ignored, bounded rollback only after owned device write
34. focused tests: **12/12 passed**, plus Python compile and diff checks
35. build validation: LZ4 12800 variant built; SHA-256 `a2d93ca07ae4d25552e2bc0c837fc1cdac969469f043726363c217ddb7abf94f`; srcversion `2E765CCD9E93DFD432A30C4`; exact 4.9.112 vermagic; no unresolved/global LZ4 symbols; LZ4, contract, and transfer-retry tests passed
36. inactive reconnect cycles: **3/3 passed** (target 3)
37. manual role flips: **0**
38. phone reboots: **0 during counted cycles**; one excluded pre-qualification reboot is documented
39. Pi service restarts: **0**
40. GUD VID/PID after each reconnect: **PASS**
41. GUD probe after each reconnect: **PASS**
42. GUD DRM node after each reconnect: **PASS**
43. role oscillations: **0 unplanned**; each cycle had exactly one bounded planned device/host transaction
44. active RGB565+LZ4 before detach: **PASS**, visually confirmed
45. physical detach: **PASS**
46. xdispd reached unavailable: **yes**
47. ChildPid after detach: **0**
48. old Pi epoch retired: **yes**, final ownership `aggregate_state=Idle`, `aio_state=Idle`, `currently_owned=false`; no unsafe teardown
49. physical reconnect: **yes**
50. autonomous host-role recovery: **PASS**
51. manual device/host cycle required: **NO**
52. VID/PID after reconnect: **PASS**, `1d50:614d` at 480 Mbit/s
53. GUD probe: **PASS**
54. GUD probe `-110`: **no** in corrected qualification
55. DRM node created: **yes**, `/dev/dri/card1`
56. xdispd became available: **yes**
57. ChildPid before explicit Activate: **0 after normal Deactivate cleared the retained request**; existing sticky-request behavior had first spawned transient child 10879 and is documented
58. explicit Activate: **PASS**
59. fresh mirgud child PID: `11187`
60. external desktop restored: **yes**, operator confirmed
61. phone remained responsive: **yes**
62. xdispd restart required: **NO**
63. Pi restart/service restart required: **NO**
64. phone role-reset debug command required: **NO**
65. reconnect -> available latency: 29.599 s helper transaction; about 31 s from tagged reattach to recovered GUD
66. Activate -> restored first frame latency: active/fresh child observed within 5 s; exact first-frame latency was not separately timestamped
67. resource/role-loop problems: **0 recovery loops or service leaks**; stable helper/xdispd/Pi PIDs during qualification. Separate follow-ups: variable 4-15 presented FPS and one non-deterministic stale Virtual Trackpad session after forced Mir disconnect containment
68. final USB role: `host`
69. final xdispd State: `available`
70. final ChildPid: `0`
71. final compositor/LightDM health: active/running; controlled A/B returned normal phone UI with both old and new modules
72. final Pi service/UDC: PID 5623 active/running, NRestarts=0, UDC `configured`; this post-A/B restoration restart is excluded from qualification
73. final receiver: `aggregate_state=Idle`, `aio_state=Idle`, `currently_owned=false`, `processing_failed=0`, `unsafe_teardown_suppressed=0`
74. commits created: gud `3e3361d` (`usb: signal stable GUD root-port reattach`); mir-android2-platform-gud `1fbc01d` (`usb-role: recover GUD host after tagged reattach`); this evidence bundle is committed separately in gud-gadget
75. commits pushed: **NO**
76. evidence path: `/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/xdisp-e3-b02-oneplus-usb-role-recovery-20260825T144602Z/`
77. E3-B02 verdict: **PASS**; autonomous role recovery and the required 3 inactive + 1 active hardware qualification passed
78. E3-T02 status: **READY TO RERUN**; it was not run in this task
79. remaining E3-B02 blocker: **none**; controlled clean-boot A/B with identical userspace showed normal phone UI return for old module `780A954B1169DD84C8B65E7` (~10 s) and new module `2E765CCD9E93DFD432A30C4` (~5 s), excluding the root-port monitor as a deterministic trackpad regressor
80. next recommendation: review/commit the E3-B02 changes, then run E3-T02 while recording phone-UI return time and presented FPS across repeated cycles; treat any recurrent stale Lomiri session as a separate Mir/Lomiri lifecycle investigation

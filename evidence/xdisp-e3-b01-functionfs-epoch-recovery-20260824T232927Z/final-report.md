# E3-B01 final report

1. Task: **BLOCKED**

2. initial gud HEAD: `276312b56ddef5e8cf581d5fa378fb133d7874bd`

3. initial gud-gadget HEAD: `5145620a9ae166853d82bbe33364c9308edce1fc`

4. initial mir HEAD: `7eec63eada784ac8e4dc05a6aa2302cb3232b3a1`

5. exact Pi kernel/version audited: `Linux 6.12.47`, deployed as `6.12.47+rpt-rpi-v8-ffs-xfercompltrace #2 SMP PREEMPT Sun Aug 16 23:57:59 -05 2026 aarch64`. The phone, not the Pi, runs 4.9.112.

6. exact FunctionFS source corresponding to deployed kernel found: **yes** — local `.xfercompltrace-source`, matched by release/config and deployed trace-provider marker.

7. existing userspace ownership-gate implementation summary: shutdown required aggregate Idle, exact-AIO Idle, no accepted operation identity, empty AIO queue, no pending/ready receive, no cleanup, and no diagnostic admission. The old event loop stopped progressing those prerequisites after Poisoned.

8. exact reason service shutdown previously hung: SIGTERM only set `shutdown_requested`; Poisoned prevented the old loop from harvesting the terminal Linux-AIO completion and lifecycle boundary, while the shutdown gate required ownership to disappear. The circular wait lasted until systemd's 90-second timeout and forced kill.

9. original poison event reproduced: **no** — historical logs/source were analyzed; deliberately reproducing it required unavailable physical access.

10. original poison root cause: an EP0 reconnect/teardown error was handled by the generic GUD-event error branch while an exact bulk AIO remained accepted. The old branch labeled this as protocol parse failure, poisoned the receiver, and then stopped lifecycle/AIO progress.

11. meaning of `GUD protocol parse failure while exact AIO accepted`: the generic `gud_gadget::event(raw EP0)` error path fired while a bulk transaction identity existed. The historical log omitted the underlying error chain, so the wording describes control-path classification plus concurrent ownership, not proven malformed bulk bytes.

12. was that truly malformed protocol data: **no evidence of malformed data; no** for purposes of the diagnosis. The retained evidence cannot identify malformed SET_BUFFER or payload bytes.

13. role of FUNCTIONFS_SUSPEND: it reports USB bus suspension and preserves the configured endpoint generation and accepted ownership. The new code preserves the live epoch across it.

14. can SUSPEND resume same session: **yes**; `ffs_func_resume()` queues RESUME for that same configuration.

15. is SUSPEND safe epoch terminal boundary: **NO**. The deployed source only queues SUSPEND; it neither disables endpoints nor drains requests, and RESUME can follow.

16. exact definitive USB-session terminal boundary identified: DISABLE or UNBIND after FunctionFS endpoint disable; because lifecycle events coalesce, a subsequent BIND or ENABLE also proves the previous endpoint generation was destroyed. Retirement additionally waits for the old logical AIO completion to be harvested and its queue to be empty. Only ENABLE creates the new data epoch.

17. kernel/source proof for terminal boundary: `ffs_func_disable()` calls `ffs_func_eps_disable()` before DISABLE; `ffs_func_set_alt()` disables the old set before enabling the new one; `ffs_func_unbind()` disables and drains `io_completion_wq` before UNBIND. DWC2 disconnect/endpoint-disable paths synchronously call `kill_all_requests(..., -ESHUTDOWN)`, whose giveback removes and completes every queued request.

18. what happens to outstanding FunctionFS AIO on endpoint disable/disconnect: DWC2 unmaps/removes each queued USB request and invokes its completion with shutdown; FunctionFS queues its AIO userspace completion worker. The userspace completion can be observed after the lifecycle event even though the UDC callback has already been given back.

19. guaranteed completion/cancellation status: DWC2 disconnect and endpoint disable use `-ESHUTDOWN` for queued requests. A racing already-completed request can retain its real completion status; in all cases retirement waits for the sole AIO result and empty queue rather than assuming a status.

20. can old completion arrive after terminal boundary: **yes, at the userspace Linux-AIO observation layer**, because FunctionFS queues `ffs_user_copy_worker`; **no after the implemented retirement gate**, because retirement requires that completion harvested and the queue empty before a new epoch.

21. USB epoch/session model added: **yes**.

22. epoch identifier mechanism: shared monotonically increasing `AtomicU64` outer epoch plus exact-AIO epoch metadata; only ENABLE begins a strictly greater epoch ID.

23. old unresolved transaction terminal disposition name/meaning: `ABORTED_BY_SESSION_DESTRUCTION` — historical failure caused by proven destruction of its endpoint generation, never success and never same-epoch Idle.

24. arbitrary Poisoned -> Idle reset added: **NO**.

25. stale old-epoch callback isolation: **PASS (source/unit proof)**. New-epoch admission requires terminal boundary, old AIO harvest, no operation identity, and empty queue, preventing a stale callback rather than accepting and filtering it.

26. shutdown gate after dead epoch: **PASS (unit proof)**; `shutdown_after_destroyed_outer_epoch_is_bounded` passes. Physical service-stop proof was not possible.

27. shutdown gate for still-live ambiguous epoch remains conservative: **PASS**; `live_ambiguous_outer_epoch_still_blocks_shutdown` passes.

28. exact GUD host probe request that previously returned -110: vendor/interface IN `GUD_REQ_GET_DESCRIPTOR` (`0x01`), expected `struct gud_display_descriptor_req`, called by `gud_probe -> gud_get_display_descriptor -> usb_control_msg`, with `USB_CTRL_GET_TIMEOUT` observed as about 5 seconds.

29. Pi saw that request: **no corresponding EP0 request marker was captured**.

30. exact reason host probe timed out: path A — the Pi event loop was parked behind the old Poisoned ownership state and did not read/answer fresh EP0, so the phone timed out awaiting the display descriptor. This is an evidence-backed inference from the synchronized host timeout and Pi state; response-transmission evidence does not exist.

31. source changes: `gadget/src/lib.rs` adds exact-AIO epoch/disposition/history/admission; `drm/src/main.rs` adds outer epochs, lifecycle classification, boundary/harvest retirement, continued Poisoned polling, fresh-ENABLE admission, detailed error logging, and shutdown gating. Key implementation locations are `gadget/src/lib.rs:190`, `gadget/src/lib.rs:393`, `gadget/src/lib.rs:2062`, `drm/src/main.rs:308`, `drm/src/main.rs:840`, `drm/src/main.rs:969`, and `drm/src/main.rs:3369`.

32. kernel changes: **no**.

33. Mir modified: **NO**.

34. Lomiri modified: **NO**.

35. transport format changed: **NO**.

36. ownership safety rules weakened: **NO**.

37. focused tests: **161 / 161 passed** (`gud-drm` 83/83, `gud-gadget` 78/78).

38. build validation: format PASS; check PASS; strict clippy PASS; AArch64 release build PASS; full offline/unit corpus 167 passed. Four vendor configfs integration tests were environment-only failures because the x86 build host has no USB gadget configfs. Final binary SHA-256: `7e22188ab8eb0e6a31eefacc9f88b6196cc3604e83730bfe2625ab8f16654a00`.

39. one clean active detach -> reconnect cycle attempted: **no** — operator away from physical hardware.

40. physical active detach: **NOT ATTEMPTED**.

41. old epoch retired safely: **not demonstrated on hardware**; source/unit model passes.

42. service restart required: **not applicable; success run not attempted**. No restart is designed into recovery.

43. phone USB role forcing required: **not applicable; none performed for validation**.

44. phone reboot required: **no reboot performed; success run not attempted**.

45. Pi reboot required: **no reboot performed; success run not attempted**.

46. xdispd restart required: **no restart performed; success run not attempted**.

47. reconnect VID/PID: **not observed**; expected project device is `1d50:614d`.

48. GUD probe after reconnect: **NOT ATTEMPTED**.

49. GUD DRM node created: **no**; current detached phone has only `/dev/dri/card0`.

50. xdispd after reconnect: **not observed**. Current detached state at 2026-08-24T23:38:14Z is `unavailable`.

51. ChildPid before explicit Activate: **0** in current detached baseline; no reconnect activation sequence occurred.

52. explicit Activate: **NOT ATTEMPTED**.

53. new managed child PID: **none**.

54. external desktop restored: **no test performed**.

55. selected transport after restore: **not exercised**; implementation remains direct Mir RGB565 + LZ4.

56. inactive reprobe cycles: **0 / 0**, gated on the missing active reconnect proof.

57. Poisoned transitions during successful validation: **not applicable; no successful hardware validation run**.

58. ambiguous accepted I/O during successful validation: **not applicable; no successful hardware validation run**.

59. unsafe receiver reset/unbind: **0**.

60. service stop after definitively dead epoch: **NOT ATTEMPTED**. Deployment-time restart while already detached is excluded.

61. final Pi receiver epoch/state: last confirmed after deployment while detached: `usb_epoch=0`, `WaitingForHost`, aggregate Idle, exact-AIO Idle, `currently_owned=false`; later SSH refresh was rejected.

62. final xdispd state: `unavailable` at 2026-08-24T23:38:14Z.

63. final ChildPid: `0`.

64. final compositor/LightDM health: PASS — lomiri-system-compositor PID 72473 and LightDM PID 72466 alive; phone responsive over SSH.

65. final Pi service/UDC state: last confirmed service active with `/home/cristian/gud-drm-e3-b01-functionfs-epoch` (SHA above), UDC configured and `not attached`; later SSH authentication failed, so no newer assertion is made.

66. commits created: `b9fc4effec076594badad3508b5a006909256bb7 functionfs: retire dead USB epochs safely`; evidence commit recorded by final handoff.

67. commits pushed: **NO**.

68. evidence path: `/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/xdisp-e3-b01-functionfs-epoch-recovery-20260824T232927Z/`

69. E3-B01 verdict: **BLOCKED**.

70. E3-T02 status after this work: **STILL BLOCKED**.

71. exact remaining blocker if any: physical access is required to perform the mandatory one clean active USB detach -> reconnect -> fresh GUD probe -> explicit Activate cycle and the follow-on dead-epoch service-stop check. Offline tests cannot prove DWC2/FunctionFS runtime ordering on the actual cable path.

72. exact next recommendation: when physically present, leave the deployed Pi service and xdispd running, connect to establish a fresh Idle epoch, activate direct RGB565+LZ4, perform exactly one physical active detach/reconnect without any administrative recovery, capture the prescribed phone/Pi timeline, explicitly Activate, and require display restoration. If it passes, run the small inactive reprobe set and dead-epoch service-stop test; only then mark E3-B01 PASS and make E3-T02 ready to rerun.

## Physical extension — 2026-08-25

73. active pre-detach display: **PASS**, operator-confirmed, direct Mir RGB565 + LZ4 at 1280x720.

74. physical detach containment: **PASS**. Transaction 1791 completed before FunctionFS SUSPEND; ownership and exact AIO were Idle, the AIO queue was empty, and poisoned/timed-out/failed transaction counts were zero.

75. definitive terminal boundary: **PASS**, `functionfs-disable-disconnect` at `2026-08-25T05:16:40.367147Z`.

76. service continuity: **PASS**, Pi `gud-userspace.service` retained PID 1462.

77. autonomous reconnect: **FAIL**. The phone remained in `host` mode but exposed only root hubs throughout the complete 30-second VID/PID poll; Pi UDC was `not attached`.

78. administrative recovery required: **YES**. The runbook-prescribed OnePlus `device -> host` role cycle was applied only after recording the strict failure.

79. post-recovery enumeration: **PASS**, `1d50:614d` at dynamic path `1-1.3`, 480 Mbit/s.

80. post-recovery GUD/DRM: **PASS**, fresh probe and `/dev/dri/card1`.

81. epoch recovery: **PASS**, same Pi service advanced `usb_epoch=1 -> 2` and completed new RGB565+LZ4 frames with zero poison, timeout, or processing failure.

82. managed child/display restoration: **PASS**, MirGUD PID 88186 and operator-confirmed visible external desktop.

83. final strict E3-B01 verdict: **FAIL**, because USB-role forcing was required after reconnect. This does not indicate a failure of the implemented FunctionFS epoch retirement; it identifies the independently documented stale OnePlus host-controller state.

84. extension evidence: `physical-extension-20260825T051154Z/README.md`.

## Physical retry — 2026-08-25T053600Z

85. B01 retry strict active detach/reconnect: **FAIL**; the phone exposed only
root hubs through the bounded 30-second poll after reconnect.

86. B01 failure-path epoch retirement: **PASS**; transaction 1021 was retired
as `ABORTED_BY_SESSION_DESTRUCTION` after FunctionFS DISABLE and AIO harvest.

87. dead-epoch service-stop check: **PASS**; the proven-Idle gate completed,
the gadget unbound, and the service stopped without forced kill.

88. recovery after strict failure: the documented phone `device -> host` role
cycle restored `1d50:614d` at `1-1.3`, 480 Mbit/s, `/dev/dri/card1`, and active
RGB565+LZ4 streaming. This recovery is excluded from the strict B01 verdict.

89. retry evidence: `physical-retry-20260825T053600Z/README.md`.

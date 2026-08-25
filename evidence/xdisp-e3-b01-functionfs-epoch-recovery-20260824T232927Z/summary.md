# Summary

The deployed FunctionFS/DWC2 kernel is the Pi's
`6.12.47+rpt-rpi-v8-ffs-xfercompltrace`, not the phone's 4.9 host kernel.
Source audit proves SUSPEND is resumable and non-terminal. FunctionFS DISABLE,
or a later ENABLE that coalesced an unread DISABLE, occurs only after the old
endpoint set was synchronously disabled. DWC2 gives every queued request back
with `-ESHUTDOWN` during that disable.

The implementation preserves ownership until both a terminal endpoint-generation
boundary is observed and the sole Linux-AIO completion is harvested. The old
transaction is then recorded as `ABORTED_BY_SESSION_DESTRUCTION`; a fresh epoch
cannot start until the AIO queue is empty.

Focused tests passed 161/161; format, check, clippy, and the AArch64 release build
passed. Binary SHA-256 is
`7e22188ab8eb0e6a31eefacc9f88b6196cc3604e83730bfe2625ab8f16654a00`.
The final binary was selected by `gud-userspace.service` on the Pi while detached.

No physical detach/reconnect was possible: the operator was away from the house.
At 2026-08-24T23:38:14Z the phone was healthy but disconnected, with xdispd
`unavailable`, `ChildPid=0`, only `/dev/dri/card0`, LightDM PID 72466, and
lomiri-system-compositor PID 72473. The task is therefore BLOCKED on the required
one-cycle hardware proof; E3-T02 remains blocked.

## Physical extension — 2026-08-25

Physical access later became available. Direct RGB565+LZ4 was active and
visually confirmed before detach. The detach was contained from proven Idle
ownership with transaction 1791 complete, an empty exact-AIO queue, and no
poison/timeouts. The same Pi service process (PID 1462) survived.

The strict reconnect gate failed: after reconnect the OnePlus controller
reported `host` for the complete 30-second poll but exposed only its two root
hubs. Applying the documented `device -> host` controller recovery immediately
restored `1d50:614d` at 480 Mbit/s. GUD reprobed, `/dev/dri/card1` returned, the
Pi advanced from USB epoch 1 to 2, RGB565+LZ4 streaming resumed, and the
operator confirmed the display was visible.

The implementation's epoch recovery therefore passed on hardware after fresh
enumeration, but E3-B01 remains a strict **FAIL** because administrative USB
role recovery was required. See `physical-extension-20260825T051154Z/README.md`.

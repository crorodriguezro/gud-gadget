# Backlog

The cross-repository product scope, epic hierarchy, and priority order live in
`../gud/PROJECT-ROADMAP.md`. This file lists Pi gadget work only.

## P0 — Productionize protocol-native FunctionFS receives

The original generic `usb-gadget` Linux-AIO queue accepted 16 speculative
requests and returned the invalid value `0x7f600` before payload bytes arrived.
That design remains unsafe and must not be restored.

The newer STATUS_ON_SET path is materially different: it creates one exact
request only after a valid SET_BUFFER, proves `io_submit()` acceptance before
GET_STATUS=OK, keeps EP0 nonblocking, and harvests one exact completion. On
2026-08-17, the OnePlus/Pi hardware gates progressed from one exact
12,800-byte XRGB8888 transaction to two guarded, ordered, uncompressed
12,800-byte transactions. Each passed through host URB, DWC2, FunctionFS AIO,
userspace, and framebuffer processing, and both receive guards returned to
Idle before the next transaction. Sequential evidence is under
`evidence/functionfs-status-on-set-e1-t02-hs-corrected-20260817T192939Z/`.

This is a bounded two-transaction diagnostic result, not production
multi-frame qualification. Remaining P0 work is tracked by the roadmap:

- `E1-T01`: **verified**. Commit `cea9942` completed one exact 12,800-byte
  transaction, returned both guards to Idle, drained the host's chained
  display-disable and controller-disable statuses, and detached with no
  post-success `-71`. Evidence is under
  `evidence/functionfs-status-on-set-e1-t01-hs-20260817T185502Z/`;
- `E1-T02`: **verified**. Commits `93a6364` and `fcde453` completed two exact,
  uncompressed 12,800-byte transactions in order, returned both receive guards
  to Idle between them, drained both cleanup statuses for each transaction,
  and detached after transaction 2 without retry, overlap, `-71`, poison, or
  unsafe teardown. Evidence is under
  `evidence/functionfs-status-on-set-e1-t02-hs-corrected-20260817T192939Z/`;
- `E1-T03`: **verified**. The accepted decision selects protocol-gated exact
  native AIO as the production default, serializes one depth-one transaction
  through `Idle -> Arming -> InFlight -> Processing -> Idle`, and retains the
  qualified blocking receiver as a separate detached/Idle rollback through
  E1-T06. Spec:
  `../gud/docs/superpowers/specs/2026-08-17-e1-t03-production-receive-architecture-design.md`;
- `E1-T04`: **verified**. The production candidate uses `GUD_RECEIVE_MODE`
  with `status-on-set-aio` as its default, aggregate
  `Idle -> Arming -> InFlight -> Processing -> Idle` ownership, queue depth
  one, operation-identity correlation, and the semantic audit/runbook in
  `docs/e1-t04-production-aio-runbook.md`. The OnePlus 6/Pi Zero 2 W gate
  completed 100 ordered exact 12,800-byte transactions with final aggregate
  Idle and no poison, timeout, host failure, DWC2 anomaly, or kernel fault.
  Canonical clean-source evidence:
  `evidence/functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/`.
  The retained clean-source diagnostic attempt under
  `evidence/functionfs-status-on-set-e1-t04-clean-source-rerun-20260818T025417Z/`
   identified and led to the host XRGB8888 stage-format fix;
- `E1-T05`: **blocked**. The lifecycle ownership matrix and offline containment
  coverage are defined in `docs/e1-t05-lifecycle-matrix-runbook.md`. The
  clean-source normal reconnect run retained at
  `evidence/functionfs-status-on-set-e1-t05-normal-reconnect-20260818T033000Z/`
  failed at cycle 1 of 10: a completed transaction reached Idle and FunctionFS
  Suspend was safely observed from Idle, but reattach did not restore the Pi
  session and the OnePlus later logged descriptor `-110`. A fresh-baseline
  retry at `evidence/functionfs-status-on-set-e1-t05-normal-reconnect-retry-20260818T034100Z/`
  reproduced the failure: `1d50:614d` did not return within 45 seconds after
  safe Idle Suspend. Isolated evidence at
  `evidence/functionfs-status-on-set-e1-t05-idle-role-power-diagnosis-20260818T041030Z/`
  shows why: after a successful exact transaction returned to Idle, the phone
  `host -> device -> host` cycle changed the Pi boot ID, proving the Pi reset
  rather than preserving a reconnectable FunctionFS/DWC2 session. The new boot
  then failed `set_crtc` with `Permission denied`, leaving the service failed
  and UDC `not attached`; phone enumeration consequently ended in descriptor
  `-110`/address `-62`. The reset occurred during the role/re-enumeration
  sequence; whether supply was lost or host VBUS/backfeed caused a transient is
  not yet measured. Isolate Pi power and OTG VBUS, use data-only detach, and
  prove boot-ID continuity before rerunning N1. The live Pi also booted
  `6.12.47+rpt-rpi-v8` rather than the pinned `-ffs-xfercompltrace` kernel, so
  restore and verify the pinned kernel first. Do not run controlled fault cases
  until normal reconnect passes 10/10. Follow-up evidence at
  `evidence/functionfs-status-on-set-e1-t05-topology-validation-20260818T042317Z/`
   restored the pinned kernel and found a second blocker in the superseded
   restart-on-disconnect design: a proven-Idle detach exited zero after safe
   teardown, but deployed production/diagnostic drop-ins overrode containment
   with effective `Restart=no`, preventing fresh process creation. E1-T05 now
   replaces that model with same-PID persistent FunctionFS reconnect; normal
   idle `DISABLE -> ENABLE` retains the UDC, FunctionFS files, DRM, and
   exact-AIO sequence with `NRestarts` unchanged. A corrected process completed
   a new exact transaction to Idle, but the next cable/re-role sequence again
  hard-reset the Pi before a post-reconnect transaction. Electrically verify
  Pi supply and OTG VBUS isolation before another attempt;
- `E1-T06`: pass the sustained transport soak.

Keep the 12,800-byte actual-payload operating constraint through these gates.
Do not represent simulation or the bounded two-transaction diagnostic as
sustained DWC2 validation.

## P2 — Explain the larger-payload DWC2/FunctionFS boundary

The observed boundary remains 12,800 bytes qualified versus larger aligned
transfers that have stalled or produced impossible completion state. This is
`E1-T07`. It is not a v1 blocker unless the qualified envelope fails the
product performance SLO.

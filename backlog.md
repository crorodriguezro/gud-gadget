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
- `E1-T03`: **in progress**. The proposed decision selects protocol-gated exact
  native AIO as the production default and retains the qualified blocking
  receiver as an explicit detached/Idle rollback through E1-T06. Spec:
  `../gud/docs/superpowers/specs/2026-08-17-e1-t03-production-receive-architecture-design.md`;
- `E1-T04`: implement the chosen long-lived receive path;
- `E1-T05`: pass disconnect, suspend, timeout, and failure lifecycle gates;
- `E1-T06`: pass the sustained transport soak.

Keep the 12,800-byte actual-payload operating constraint through these gates.
Do not represent simulation or the bounded two-transaction diagnostic as
sustained DWC2 validation.

## P2 — Explain the larger-payload DWC2/FunctionFS boundary

The observed boundary remains 12,800 bytes qualified versus larger aligned
transfers that have stalled or produced impossible completion state. This is
`E1-T07`. It is not a v1 blocker unless the qualified envelope fails the
product performance SLO.

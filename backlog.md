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
2026-08-17, the OnePlus/Pi hardware gate completed one 12,800-byte XRGB8888
transaction through host URB, DWC2, FunctionFS AIO, userspace, and framebuffer
processing. Both the exact transaction and outer guard returned to Idle.
Evidence is under
`evidence/functionfs-status-on-set-hs-20260817T165632Z/`.

This is a one-transaction diagnostic result, not production multi-frame
qualification. Remaining P0 work is tracked by the roadmap:

- `E1-T01`: drain the trailing status before diagnostic detach;
- `E1-T02`: prove two sequential exact transactions;
- `E1-T03`: select native AIO or the qualified blocking path for production;
- `E1-T04`: implement the chosen long-lived receive path;
- `E1-T05`: pass disconnect, suspend, timeout, and failure lifecycle gates;
- `E1-T06`: pass the sustained transport soak.

Keep the 12,800-byte actual-payload operating constraint through these gates.
Do not represent simulation or the single diagnostic transaction as sustained
DWC2 validation.

## P2 — Explain the larger-payload DWC2/FunctionFS boundary

The observed boundary remains 12,800 bytes qualified versus larger aligned
transfers that have stalled or produced impossible completion state. This is
`E1-T07`. It is not a v1 blocker unless the qualified envelope fails the
product performance SLO.

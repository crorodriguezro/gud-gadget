# E2-T05 shutdown and containment qualification

This evidence package records the 2026-08-24 qualification of managed xdispd
shutdown for the direct Mir RGB565 + LZ4 v1 path at 1280x720.

The healthy and safe-stall cases were run through the public D-Bus lifecycle
interface. No operator signal was sent to mirgud. The Pi SSH endpoint refused
the available credentials during this session, so fresh absent-device,
physical-detach, and pre-bulk-failure runs were not claimed.

Verdict: NEEDS PROJECT-OWNER DISPOSITION. The current implementation is
bounded and safe in the exercised cases, but every healthy stop required the
existing forced-containment deadline; normal graceful shutdown was not
demonstrated. See `final-report.md`.

# E2-T03 nonblocking qualification - blocked preflight

**Status: BLOCKED; not verified.**

This evidence root records the source/test gate and the hardware safety
preflight performed on 2026-08-20. It is not a healthy/slow/absent/failing
qualification result and does not authorize an E2-T03 roadmap update.

The Pi receiver's recent journal contains an accepted FunctionFS transaction
that entered `Poisoned`. In accordance with the E1 safety boundary, no command
in this session stopped, restarted, unbound, rebooted, or otherwise tore down
the Pi service. No xdisp activation, slow injection, absence test, failing
presentation test, or manual phone-responsiveness interaction was attempted.

## Source architecture audit

- `LatestFramePresenter::submit()` copies/replaces the single pending frame
  while holding only its local mutex and then wakes the worker.
- The worker removes that frame, releases the mutex, and calls
  `GudKms::present()` outside the producer path.
- The presenter permits at most one pending and one in-flight frame.
- `xdispd` dynamically reopens the discovered GUD device and passes it to the
  managed `mirgud` child; it does not select a fixed DRM card.
- Worker presentation remains measured by legacy `submit_*` fields. This
  qualification adds producer-visible `enqueue_*` fields without changing the
  legacy names used by telemetry consumers.

## Required recovery before resumption

Recover the Pi physically using the already-approved E1 procedure until the
receiver is known Idle and no Poisoned accepted-I/O state remains. Then begin a
new clean E2-T03 hardware root and run all H/S/A/F cases. Do not resume this
root by forcing FunctionFS or service teardown.

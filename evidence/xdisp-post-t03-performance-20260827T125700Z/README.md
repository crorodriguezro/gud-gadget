# Post-T03 performance characterization

This bundle records the E4 housekeeping work and nine live post-T03 RGB565
benchmark repetitions run on 2026-08-27. The primary path was
`Mir -> mirgud -> LatestFramePresenter -> GUD -> USB -> Pi gadget -> VC4`.

Each repetition used a 10 s warmup and approximately 60 s measured interval,
with one negotiated logical update and the unchanged production routing.
Phone-side final reports are complete for all nine repetitions. Pi-side
`frame_stats` is retained only for the journal window still available on the
device; the oldest 1280x720 repetition is no longer retained and is marked
accordingly in the report.

The source was the live extended desktop continuously sampled by Mir. It was
not an intentionally generated moving/noise workload; therefore compression
and bandwidth results characterize this desktop workload, not a worst-case
incompressible stress pattern. No production routing, presenter depth,
scaler, or async behavior was changed.

See `final-report.md`, the CSV summaries, and `SHA256SUMS`.

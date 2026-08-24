E2-T04 was started with direct Mir RGB565 + LZ4 at 1280x720 and stopped before
the 30-minute acceptance interval because the operator observed an undersized
external desktop (about one third of the monitor).

Evidence captured during the run:

- Phone source: true packed Mir RGB565, direct-copy path, 1280x720, stride
  2560, logical frame 1,843,200 bytes.
- Pi transport: RGB565 + LZ4, 16,384-byte reads, one logical admission and
  one exact AIO receive per transaction.
- Abort point: Pi sequence 13,920; accepted/completed 13,920; poisoned,
  timed-out, processing-failed, and ambiguous-accepted counts all zero in the
  captured telemetry.
- Geometry: Pi HDMI connector preferred mode 1920x1080, but the run forced the
  test-only physical output mode to 1280x720.
- Native-mode check: 1920x1080 selected successfully with `scaled=true`, but
  one-row damage updates measured about 203 ms of scaling work.

Verdict: **BLOCKED**, not pass. A corrected full-screen geometry/source-size
configuration is required before repeating the 30-minute soak.

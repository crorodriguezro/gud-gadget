# E2-T04 RGB565 + LZ4 managed soak

Status: **BLOCKED — run aborted on physical geometry failure**

The selected direct Mir RGB565 + LZ4 path was activated at 1280x720, with one
logical accepted/in-flight GUD payload and the existing status-on-set AIO
containment. The operator reported that the desktop occupied only about one
third of the monitor. The 30-minute soak was stopped immediately; this bundle
does not claim a soak pass.

The receiver logs show 13,920 accepted and completed transactions before the
abort, with zero poisoned transactions, timed-out transactions, processing
failures, or ambiguous accepted I/O in the captured state. The Pi connector
advertises 1920x1080 as its preferred mode, while the run forced the test-only
physical output mode to 1280x720. A short native-mode diagnostic confirmed that
1920x1080 is selectable and scales the 1280x720 source, but its observed
~203 ms per one-row update makes it unsuitable as a sustained qualification
configuration without further geometry/source work.

Verdict: **BLOCKED pending a corrected full-screen geometry configuration and
a fresh T04 run**. The selected transport is not promoted to the release
default by this aborted run.

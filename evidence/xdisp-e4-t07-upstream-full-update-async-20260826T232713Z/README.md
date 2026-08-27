# E4-T07 retained evidence index

This bundle records the final decision and reproducible measurements for the
upstream-style full-update and async-flush audit. Raw stage logs remain in the
host's ignored `gud/backport-4.9/env/local/evidence/e4-t07-*` directories; the
canonical source analysis is
`gud/docs/e4-t07-upstream-full-update-audit.md`.

Result: **PASS-B**. Production keeps `LatestFramePresenter` and selects the
synchronous, negotiated-capacity, one-attempt-LZ4 GUD path. The default-off
kernel async path is qualified for continued configuration-C evaluation.

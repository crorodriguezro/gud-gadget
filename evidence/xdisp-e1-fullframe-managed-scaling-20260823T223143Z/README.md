# Evidence package

This package records the direct 1,843,200-byte control, 3,686,400-byte direct
single and 100/100 repeat qualification, and matched managed RAW measurements.

The authoritative raw artifacts are:

- `direct/` phone probe logs
- `phone-kernel-dmesg.log` host XDISP timing trace
- `managed-1843200/` and `managed-3686400/` matched phone/Pi journals
- `summary.md`, `scaling-table.csv`, and `bottleneck-analysis.txt`

The separate `managed-1843200/xdisp-journal.log` is the earlier intentionally
invalid descriptor/host-cap mismatch capture. It is retained for audit and is
not used by the matched metrics.

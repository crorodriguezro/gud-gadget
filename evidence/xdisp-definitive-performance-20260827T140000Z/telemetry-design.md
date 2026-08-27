# Telemetry and workload design

The host used the existing low-overhead `xdisp_payload_timing` module
parameter. It records monotonic timestamps for compression, copy,
SET_BUFFER, bulk submission, URB completion, actual length, and status. The
host frame aggregate records raw bytes, selected LZ4/RAW payload, and timing
totals. Pi `frame_stats` records one logical transaction per line; internal
FunctionFS reads are not counted as frames. Phone presenter reports use
`LatestFramePresenter` counters and monotonic timestamps.

Workload A was the deterministic pre-generated moving pattern. Workload C was
the deterministic pre-generated xorshift PRNG noise pattern. The previous live
desktop runs remain the representative observation, but the current binary has
no Mir-path workload injection option; they are therefore not relabelled as a
controlled representative animation. The controlled pattern mode is a
direct-KMS/GUD/USB/Pi control and does not include Mir capture.

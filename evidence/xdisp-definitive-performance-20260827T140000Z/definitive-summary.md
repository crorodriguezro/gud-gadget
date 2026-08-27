# Definitive benchmark checklist

The controlled runs demonstrate a practical raw RGB565 USB payload ceiling of
about 40 MB/s bulk-only (approximately 320 Mbit/s), below the 480 Mbit/s USB2
High-Speed signalling rate and consistent with protocol, scheduling, and
controller overhead. 1080p noise is therefore USB-bulk-bound, not Pi scaling-
bound or LZ4-bound. The compressed 1080p moving control reaches the requested
15 FPS with 100% LZ4 wins and ~16.3 KB payloads, showing that compression cost
alone is not the limiting stage. The live-desktop 1080p observation remains
about 8 FPS, but it is not a controlled representative animation.

For 720p noise, three 60-second measured intervals produced 17/17/17 FPS,
so the >=20 FPS target is not met for incompressible content. For 720p moving,
three repetitions produced 29/30/29 FPS with ~7.3 KB LZ4 payloads. Scaled
720x1520 remains CPU-scaler-bound: the controlled scaled run measured roughly
152 ms scale p50 and ~200 ms presenter submit p50.

The requested full matrix was not completed: controlled representative Mir
animation, 1080p A/C repetitions beyond the captured controls, Pi journal
export immediately after every run, per-frame SET_BUFFER/bulk distributions
for every workload, and separate black-clear versus scale-copy timings remain
open. Accordingly the task result is PARTIAL, not PASS.

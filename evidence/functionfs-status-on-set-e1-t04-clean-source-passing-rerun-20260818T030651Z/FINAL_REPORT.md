# E1-T04 Clean-Source Reproducibility Result

The clean-source 100-frame T04 gate passed on the OnePlus 6 and Pi Zero 2 W.
It used a clean detached gadget worktree at
`6aea5e73865f3a4cb449c0f817cadf752d721f65` and a clean detached host worktree
at `bde330da2c5133c80d8be36fdf3e88c5a5386bb8`.

The host-stage tool was rebuilt from source after commit `bde330d` corrected
its framebuffer format from RGB565 to XRGB8888, matching the production
gadget's only advertised format. The preceding clean rerun at `6468a43` is
retained under `../functionfs-status-on-set-e1-t04-clean-source-rerun-20260818T025417Z/`;
it identified the pre-transport state-check rejection and did not exercise
accepted I/O.

The unchanged T04 runner completed 100 ordered uncompressed 12,800-byte
XRGB8888 transactions. Host output contains 100 successful results and
`XDISP_T04_END result=0`. Pi telemetry records exactly 100 SET_BUFFERs,
submission attempts, accepted requests, exact completions, processing starts,
and processing completions. Sequence IDs are 1 through 100. Busy, processing
failure, poison, and timeout counts are zero. Final ownership is aggregate
Idle, AIO Idle, and `currently_owned=false`.

Phone and Pi checks found no `-71`, kernel fault, DWC2 anomaly, pstore record,
or unsafe software teardown. E1-T05 was not started.

`SHA256SUMS` covers all retained reports and raw logs.

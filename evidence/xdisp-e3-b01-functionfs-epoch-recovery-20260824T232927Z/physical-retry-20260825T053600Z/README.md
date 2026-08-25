# E3-B01 physical retry — 2026-08-25

This is the second physical B01 attempt, using the staged B01 binary
`/home/cristian/gud-drm-e3-b01-functionfs-epoch` (SHA-256
`7e22188ab8eb0e6a31eefacc9f88b6196cc3604e83730bfe2625ab8f16654a00`).

The strict active detach → reconnect gate **FAILED**: after the operator
reconnected the cable, the phone remained with only root hubs for the bounded
30-second poll. No phone role change, Pi restart, or xdispd restart was used
before declaring that failure.

The B01 implementation itself passed the failure-path recovery checks. Pi PID
2737 stayed alive with `NRestarts=0`; FunctionFS SUSPEND was preserved, the
accepted transaction timed out, DISABLE established the terminal boundary, and
epoch 1 was retired as `ABORTED_BY_SESSION_DESTRUCTION`. The required dead-epoch
service stop then reached the proven-Idle shutdown gate and stopped cleanly.

After evidence capture, the lab was restored using the troubleshooting guide's
permitted `device -> host` controller cycle. The phone found `1d50:614d` at
dynamic path `1-1.3` and 480 Mbit/s; `/dev/dri/card1` returned, xdispd was
active with child PID 89868, and the restarted B01 Pi service streamed clean
RGB565+LZ4 frames.

The role-cycle recovery is explicitly excluded from the strict B01 result.

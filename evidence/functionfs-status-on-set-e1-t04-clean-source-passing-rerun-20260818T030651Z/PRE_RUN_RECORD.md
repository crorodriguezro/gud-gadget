# Clean-Source Pre-Run Record

The run used detached clean source worktrees. The target kernel manifest was
copied only as external local kernel-build configuration and is recorded by
hash in `VALIDATION.txt`.

Before installing the clean gadget artifact, the prior Pi process reported
aggregate/AIO Idle with no owned transaction. It was stopped only at that
proven Idle state; the service was then inactive, UDC `not attached`, and no
`gud-drm` process was present. The new artifact startup reported
`status-on-set-aio`, `STATUS_ON_SET=1`, FunctionFS EP1 AIO queue depth one,
12,800-byte cap, XRGB8888, and no compression.

# Clean-Source Pre-Run Record

The reproducibility gate used detached clean worktrees, not the shared
worktrees that contain unrelated untracked investigation artifacts.

- `gud-gadget` commit: `6aea5e73865f3a4cb449c0f817cadf752d721f65`
- `gud` commit: `6468a4301af8ccd8b1c423283697d3889c126ab1`
- Both detached worktree status files are empty before build.
- The external target manifest was copied solely to supply the local OnePlus
  kernel build path; SHA-256:
  `c05195e219417be587d682dfd857258322e268f7e84b6ed8dfaf7a3b8a41587a`.
- The prior Pi service reported `active`, UDC `configured`, and final
  telemetry `aggregate_state=Idle`, `aio_state=Idle`,
  `currently_owned=false`, sequence 100 before it was stopped.
- It was stopped only after that Idle proof. The UDC then reported `not
  attached` and no `gud-drm` process existed before the clean artifact was
  installed.
- The clean service startup reported `receive_mode=status-on-set-aio`,
  descriptor `STATUS_ON_SET=1`, FunctionFS EP1 queue length one, 12,800-byte
  cap, XRGB8888, and uncompressed configuration before the host runner began.

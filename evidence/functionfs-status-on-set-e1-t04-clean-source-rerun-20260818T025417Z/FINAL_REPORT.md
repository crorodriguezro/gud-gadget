# E1-T04 clean-source reproducibility rerun

This rerun did not pass. It was run from clean detached worktrees at gadget
commit `6aea5e73865f3a4cb449c0f817cadf752d721f65` and host commit
`6468a4301af8ccd8b1c423283697d3889c126ab1`. Both production source trees
were clean before building. The machine-local target manifest was copied only
as external kernel-build configuration and is identified in `VALIDATION.txt`.

The clean rebuilt Pi `gud-drm` and host `gud.ko` hashes match the artifacts
used by the earlier passing gate. The initial conclusion that the host stage
binary differed semantically was incorrect: its source was identical, and the
ELF difference was build metadata. Phone and Pi logs instead showed the
committed stage source creating an RGB565 framebuffer and issuing
`SET_STATE_CHECK` format `0x40`, while the production gadget advertises only
XRGB8888 (`0x80`).

The first host transaction therefore failed at `drmModeAtomicCommit(commit):
Invalid argument`; the runner stopped with `XDISP_T04_END result=1`. No GUD
`SET_BUFFER` reached the Pi, so no native AIO request was submitted or accepted
and no transaction entered InFlight, Processing, or Poisoned. The Pi service
remained active/configured; no unsafe teardown or software recovery occurred.

Host commit `bde330d` aligned the stage tool to XRGB8888. The unchanged clean
gate then passed and is retained under
`../functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/`.

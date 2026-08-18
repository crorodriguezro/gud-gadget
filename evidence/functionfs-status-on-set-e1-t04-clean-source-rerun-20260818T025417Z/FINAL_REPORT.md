# E1-T04 clean-source reproducibility rerun

This rerun did not pass. It was run from clean detached worktrees at gadget
commit `6aea5e73865f3a4cb449c0f817cadf752d721f65` and host commit
`6468a4301af8ccd8b1c423283697d3889c126ab1`. Both production source trees
were clean before building. The machine-local target manifest was copied only
as external kernel-build configuration and is identified in `VALIDATION.txt`.

The clean rebuilt Pi `gud-drm` and host `gud.ko` hashes match the artifacts
used by the earlier passing gate. The clean rebuilt `gud-kms-stage` is a
different artifact from the previously used untracked host binary:

| Artifact | SHA-256 | ELF Build ID |
| --- | --- | --- |
| Prior stage binary | `30f95070703b625d235d5b1d02062cd40b63714a0ffc69cb6715aad54bd825d5` | `d73dda1c47ddb8fdd34b856a6703dcc9b294200f` |
| Clean rebuilt stage binary | `c69f24c4a7051b7c3bbde41f8c9c13eb30a599c8e1109341a9f1b01c889735bf` | `5027d109461590772e5915a22e93e440bdcb1c6a` |

The first host transaction failed at `drmModeAtomicCommit(commit): Invalid
argument`; the runner stopped with `XDISP_T04_END result=1`. No GUD
`SET_BUFFER` reached the Pi, so no native AIO request was submitted or accepted
and no transaction entered InFlight, Processing, or Poisoned. The Pi service
remained active/configured; no unsafe teardown or software recovery occurred.

The earlier T04 rerun under
`../functionfs-status-on-set-e1-t04-hs-rerun-20260818T022645Z/` remains valid
transport evidence, but it cannot be called cleanly reproducible against the
final committed host build until the stage-binary difference is explained and
the unchanged gate passes from exact identified artifacts. E1-T05 must not
start.

# Summary

| Case | Result | Evidence |
|---|---|---|
| T05-B absent device | PASS, 3/3 | Genuine absence; Activate stayed `unavailable`; Deactivate idempotent; `ChildPid=0`; no mirgud child or respawn storm |
| T05-C real USB detach | PASS, 2/2 | Physical USB DATA removal; GUD transport failed; xdispd contained the known Mir release stall and reaped the child |
| T05-E pre-bulk failure | PASS, 3/3 | Default-off source hook failed before `drmModeAtomicCommit`; no GUD bulk ownership, retries, EBUSY, or poisoned receiver state |

E2-T05 is PASS and complete for the requested B/C/E continuation. The phone
remained responsive, with compositor and LightDM/session processes alive.
Production mirgud and the xdispd environment were restored after E. The
previous T05-A owner disposition, T05-D result, and T05-F verification remain
unchanged and were not rerun.

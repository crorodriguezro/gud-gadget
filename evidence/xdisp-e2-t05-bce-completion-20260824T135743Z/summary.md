# Summary

| Case | Result | Evidence |
|---|---|---|
| T05-B absent device | PASS, 3/3 | Activate remained `unavailable`; Deactivate was idempotent; `ChildPid=0`; no mirgud child or respawn storm |
| T05-C real USB detach | BLOCKED | Pi SSH authentication failed; no active external display was available |
| T05-E pre-bulk failure | BLOCKED | Pi/GUD unavailable; no controlled pre-bulk mechanism was exposed |

The phone remained healthy after B: xdispd D-Bus stayed responsive, the
Lomiri compositor was present, and LightDM/session processes remained alive.
The previous T05-A owner disposition, T05-D result, and T05-F verification
remain unchanged.


# Summary

| Case | Result |
| --- | --- |
| T05-A healthy normal deactivation | 10/10 bounded; 0 graceful, 10 forced-contained |
| T05-B GUD absent | Not run; Pi endpoint unavailable |
| T05-C active physical detach | Not run; no safe physical-detach operator path available |
| T05-D safe pre-ownership stall | 3/3 forced-contained |
| T05-E explicit pre-bulk failure | Not run; no narrowly controlled current mechanism exposed |
| T05-F Mir disconnect boundary | Reproduced naturally on T05-A; also seen in T05-D |
| T05-G duplicate Deactivate | Pass; idempotent convergence observed |

T05-A stop durations were 2792–3778 ms, p50 3269 ms, p95 3733 ms. T05-D
durations were 2853–3572 ms, p50 3149 ms. All exercised cases returned
`State=available`, `ChildPid=0`, and no immediate respawn was observed.

The stable boundary is after presenter/KMS and screencast cleanup, at
`MIR_CONNECTION_RELEASE_BEGIN`; no completion marker appeared before xdispd's
current three-second escalation deadline.

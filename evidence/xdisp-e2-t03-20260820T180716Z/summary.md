# E2-T03 preflight summary

| Condition | External path | enqueue p95 | enqueue max | worker present max | max pending | max inflight | phone responsive | compositor stable | result |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| healthy | not run | n/a | n/a | n/a | n/a | n/a | not observed | preflight alive | BLOCKED |
| slow | not run | n/a | n/a | n/a | n/a | n/a | not observed | not observed | BLOCKED |
| absent | not run | n/a | n/a | n/a | n/a | n/a | not observed | not observed | BLOCKED |
| failing | not run | n/a | n/a | n/a | n/a | n/a | not observed | not observed | BLOCKED |

Producer enqueue latency has source-level and focused-unit-test coverage, but
has not been measured against a real slow or failing GUD worker. The hardware
matrix is blocked until the Poisoned receiver is physically recovered.

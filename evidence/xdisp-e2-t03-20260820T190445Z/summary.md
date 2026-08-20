# E2-T03 partial hardware summary

| Condition | External path | enqueue p95 | enqueue max | worker present max | max pending | max inflight | phone responsive | compositor stable | result |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| healthy | presenting | 10 us | 177 us | 3,143,113 us | 1 | 1 | observed currently; full sequence not observed | yes | PARTIAL |
| slow | no safe controlled injection | n/a | n/a | observed 3,143,113 us only | 1 | 1 | n/a | n/a | BLOCKED |
| absent | unavailable | n/a | n/a | n/a | n/a | n/a | yes | yes | PASS |
| failing | physical data detach contained | pre-failure p95 10 us | pre-failure max 324 us | pre-failure max 46,644 us | 1 | 1 | yes | yes | PASS |

Producer enqueue latency remained decoupled from the observed healthy-run
worker stall. E2-T03 remains BLOCKED because the slow row was not a controlled
test. The next action is an engineering decision on a T03-safe, existing or
newly approved slow-GUD injection method; it is not E2-T04 or E2-T05.

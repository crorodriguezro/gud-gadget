# Final report

## Result

**PARTIAL characterization; housekeeping complete.** All requested modes were
run three times for roughly 60 s after warmup, with zero phone-side capture,
conversion, release, submit, or lifecycle failures. The primary limitations
are retained-journal loss for the oldest Pi run, disabled host
`xdisp_payload_timing`, and the absence of an intentionally changing/noise
workload.

## Configurations

| Config | Route | Logical RGB565 bytes | Phone received/submitted | Phone presented | Phone drop % | presenter `submit_us` p50/p95 |
|---|---|---:|---:|---:|---:|---:|
| A | 1280x720 DirectExact | 1,843,200 | 3970/3970, 3975/3975, 3988/3988 | 2842, 2763, 2737 | 28.4, 30.5, 31.3 | 50/50 ms |
| B | 1920x1080 DirectExact | 4,147,200 | 3981/3981, 3984/3984, 3981/3981 | 554, 557, 555 | 86.1, 86.0, 86.0 | 200/200 ms |
| C | 720x1520 ScaledFallback | 2,188,800 | 3977/3977, 3975/3975, 3967/3967 | 335, 335, 334 | 91.6, 91.5, 91.6 | 200/200 ms |

Phone source/submitted rates were 61/61 fps in every run. Presenter depth was
bounded at one pending and one in-flight frame. Page-flip completion was not
available: the current Pi backend submits page flips without a completion
event, so successful Pi `frame_stats`/presentation operations are the
available presentation-submit measure.

## Pi-side telemetry

Available rows (journal retention permitting) were:

| Config/run | Rows | Receive p50/p95 ms | Processing p50/p95 ms | Scale p50/p95 ms | Wire MiB/s |
|---|---:|---:|---:|---:|---:|
| A/r1 | unavailable | unavailable | unavailable | unavailable | unavailable |
| A/r2 | 2169 retained | 10/11 | 5/6 | 0/0 | 12.94 |
| A/r3 | 2205 | 10/10 | 5/6 | 0/0 | 9.87 |
| B/r1 | 547 | 63/63 | 20/22 | 0/0 | 16.53 |
| B/r2 | 563 | 63/63 | 20/22 | 0/0 | 13.07 |
| B/r3 | 555 | 63/63 | 20/22 | 0/0 | 13.18 |
| C/r1 | 263 retained | 31/33 | 161/161 | 152/152 | 4.38 |
| C/r2 | 270 | 31/33 | 161/162 | 152/153 | 3.46 |
| C/r3 | 272 | 31/33 | 161/161 | 152/152 | 3.46 |

Pi compression was reported as active on all retained rows. The host-side USB
completion timing parameter was `N`; Pi `recv_ms` is therefore the closest
available receive/USB interval and must not be read as a host completion
timestamp.

## Interpretation and safety

The exact 1280x720 path is presenter-bound around a 50 ms GUD submit. Exact
1920x1080 is slower at about 200 ms per successful submit, producing a much
larger drop fraction. ScaledFallback is dominated by Pi processing/scaling,
with approximately 152 ms scaling and 161 ms total processing p50. The
single-slot presenter prevents unbounded queue growth in all runs.

Pi telemetry ended with zero processing failures, poisoned transactions,
timeouts, and unsafe teardown suppressions. Final checks retained host mode,
GUD card presence, active Pi service, configured UDC, no throttling, and no
matching kernel fault signatures.

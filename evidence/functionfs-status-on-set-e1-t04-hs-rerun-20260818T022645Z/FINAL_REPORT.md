# E1-T04 long-lived production exact-AIO result

The corrected T04 hardware gate passed on the OnePlus 6 and Raspberry Pi Zero
2 W. The production `status-on-set-aio` mode advertised `STATUS_ON_SET=1` and
created the FunctionFS EP1 native-AIO driver with queue length one.

The dedicated host runner completed 100 ordered XRGB8888 `SET_BUFFER`
transactions. Every payload was uncompressed and exactly 12,800 bytes. The
host logged 100 successful atomic commits and `XDISP_T04_END result=0`.

The Pi's final cumulative telemetry reports exactly 100 SET_BUFFERs, AIO
submission attempts, accepted requests, exact completions, processing starts,
and processing completions. Sequence IDs are ordered from 1 through 100. Busy,
processing failures, poisoned transactions, and timeouts are all zero. The
final owner is `aggregate_state=Idle`, `aio_state=Idle`, and
`currently_owned=false`.

The full logs show each transaction's `Arming -> InFlight -> Processing ->
Idle` order. AIO acceptance precedes the status-ready marker, and no subsequent
admission occurs before processing finalizes the preceding transaction.

Phone and Pi kernel checks found no `-71`, kernel Oops/WARNING/panic, pstore
record, or traced DWC2 anomaly. The service remains running only because its
aggregate transaction is safely Idle; no unsafe teardown occurred.

The first 100-frame attempt is retained separately under
`../functionfs-status-on-set-e1-t04-hs-20260818T022519Z/`. It proved transport
behavior but is not acceptance evidence because its `set_buffer_seen` telemetry
double-counted the synthetic post-completion dispatch. The accepted rerun uses
the corrected counter.

Raw host, phone, Pi userspace, and Pi kernel logs are retained alongside
`VALIDATION.txt`. `SHA256SUMS` covers immutable retained artifacts.

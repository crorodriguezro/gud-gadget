# E1-T04 Production Exact-AIO Receive

## Configuration

Select the mode before binding the UDC:

- `GUD_RECEIVE_MODE=status-on-set-aio` is the production default. It advertises
  `STATUS_ON_SET=1`, configures the FunctionFS OUT endpoint with AIO queue
  depth one, and submits one exact native-AIO receive after each valid
  `SET_BUFFER`.
- `GUD_RECEIVE_MODE=blocking` is the detached/Idle rollback mode. It advertises
  `STATUS_ON_SET=0` and retains the existing qualified blocking receiver.

There is no `auto` mode, no in-session mode switch, and no blocking retry for
possibly accepted AIO.

## Ownership Audit

The production primitive is:

```text
PixelDataEndpoint::arm_exact_payload_aio()
  -> EndpointReceiver::try_recv_identified()
  -> aio::Driver::submit(IOCB_CMD_PREAD / opcode::PREAD)
  -> SYS_io_submit on initialized FunctionFS ep1 OUT fd
  -> pinned IOCB plus driver-owned BytesMut
  -> accepted-count == 1
  -> SYS_io_getevents / try_fetch_identified()
  -> FunctionFS ffs_epfile_read_iter() / ffs_epfile_io()
```

`EndpointIo` owns the initialized ep1 fd and Linux AIO context. The vendored
driver owns the pinned IOCB and receive `BytesMut` until the matching
completion. `EndpointOperation` preserves the AIO `OpHandle` identity; the
GUD transaction correlates it with a process-lifetime sequence ID and the
cloned validated `SET_BUFFER` metadata.

| State | Ownership |
| --- | --- |
| Idle | No accepted I/O, buffer, operation, metadata, or transaction owner. |
| Arming | Validated metadata and exact buffer are owned while `io_submit()` is attempted. |
| InFlight | One accepted operation, buffer, metadata, sequence, endpoint fd, and IOCB are owned. |
| Processing | The matching completion transferred the buffer to the transaction; metadata and sequence remain exclusively owned through framebuffer work. |
| Poisoned | Accepted-I/O or processing ownership is unsafe; no automatic teardown, cancellation, close/drop, fallback, or return to Idle is allowed. |

`io_submit()` returning accepted-count one proves the userspace request was
accepted by Linux AIO for the initialized FunctionFS ep1 fd. It does not
directly prove physical DWC2 hardware state. E1-T01/T02 correlate that
userspace barrier with the target's FunctionFS/DWC2 queue-before-host-send
trace, which is why it is the readiness barrier for this appliance.

Suspend, disable, disconnect, EP0 failure, timeout, invalid completion, or
processing failure after acceptance poisons ownership. Driver close/drop and
Linux AIO cancellation are not accepted-I/O recovery mechanisms on this target:
they cannot prove whether FunctionFS/DWC2 still owns or can complete the
request. Preserve evidence and use the physical containment procedure.

## T04 Hardware Gate

Do not run this gate from an unsafe state. Use the OnePlus 6 and Pi Zero 2 W,
the <=12,800-byte uncompressed XRGB8888 envelope, and the production mode.

Run 100 sequential valid `SET_BUFFER` transactions. This is intentionally a
bounded multi-frame gate, not the E1-T06 30-minute/10,000-payload soak.
Require logs/counters to show for every sequence: one arm attempt, accepted
count one before `GET_STATUS=OK`, matching operation completion, Processing,
and successful Processing-to-Idle before the next admission. Verify EP0 status
responses continue while a receive is pending, sequences are unique/in order,
and there are no unexpected BUSY, overlap, timeout, poison, host timeout,
DWC2 anomaly, kernel Oops/WARNING/pstore fault, or software teardown.

Retain immutable evidence under `evidence/functionfs-status-on-set-e1-t04-*`:
source commits and hashes, artifact hashes, exact environment, phone logs, Pi
userspace and kernel logs, sequence/state/counter summary, and final Idle
state. Stop and physically contain on any stop condition.

## Result

The clean-source bounded gate passed on the OnePlus 6 and Pi Zero 2 W with 100
ordered, uncompressed 12,800-byte XRGB8888 transactions. Canonical evidence:
`evidence/functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/`.
It records exact source commits and rebuilt artifact hashes, 100 AIO
acceptances, exact completions, Processing finalizations, sequence IDs 1
through 100, final aggregate/AIO Idle, and zero Busy, timeout, processing
failure, poison, `-71`, DWC2 anomaly, kernel fault, or pstore record.

The first clean rerun remains historical diagnostic evidence:
`evidence/functionfs-status-on-set-e1-t04-clean-source-rerun-20260818T025417Z/`.
It found that the committed host stage tool requested RGB565 while the
production gadget advertises XRGB8888. Commit `bde330d` aligned the stage tool
format; the unchanged gate then passed from clean source. E1-T05 is still
planned and has not started.

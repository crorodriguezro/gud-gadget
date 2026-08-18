# E1-T05 Lifecycle Matrix

## Status and prerequisite

Status: blocked for hardware qualification. E1-T04 is a satisfied prerequisite: its canonical
clean-source 100-frame result is
`evidence/functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/`.
It used gadget commit `6aea5e73865f3a4cb449c0f817cadf752d721f65` and host
commit `bde330da2c5133c80d8be36fdf3e88c5a5386bb8`.

The 2026-08-18 hardware preflight reached both targets but did not start a
matrix case: T05 source changes were uncommitted, both worktrees had changes,
and the OnePlus sudo credential required for the host gate was unavailable.
Do not deploy, mutate the service, or inject a fault until exact committed
artifacts, clean-worktree proof, and host privilege are available.

This ticket qualifies ownership and containment, not transparent recovery from
accepted I/O. Run one matrix case per fresh known-safe session. Do not start
E1-T06 until this document's hardware rows have passed.

## Invariants

Production configuration is fixed before UDC bind:

- `GUD_RECEIVE_MODE=status-on-set-aio`
- `STATUS_ON_SET=1`
- one FunctionFS EP1 exact native-AIO request, effective depth one
- uncompressed XRGB8888 actual payload no larger than 12,800 bytes
- no speculative receive, overlap, fallback, descriptor change, or in-session
  mode change

The aggregate and AIO state are correlated as
`Idle -> Arming -> InFlight -> Processing -> Idle`. Sequence IDs are unique
for a process lifetime; a deliberately recreated process starts a new sequence
space and its evidence must name the new PID/process instance. An accepted
operation identity, buffer, and `SET_BUFFER` metadata remain owned through
Processing. Only a proven `Idle` state may close endpoints, drop the AIO
driver, unbind/recreate the gadget, or restart normally.

`Arming` is safe to roll back only when `io_submit()` returned zero accepted
requests. A lifecycle observation in Arming is ambiguous and is contained.
`InFlight`, `Processing`, and `Poisoned` are never ordinary cleanup states.

The service emits `e1_t05_lifecycle_observed`,
`e1_t05_lifecycle_containment`, and `e1_t05_teardown_suppressed`. Retain these
with `production_receive_telemetry`, which includes aggregate/AIO state,
sequence, accepted operation identity, ownership, accepted/completed/
processing/timeout/poison counters, lifecycle count, cleanup/restart attempts,
and suppressed unsafe teardowns.

## Lifecycle audit

| Path | Idle | Arming | InFlight | Processing | Poisoned |
| --- | --- | --- | --- | --- | --- |
| SIGTERM/process shutdown | Claim shutdown, then unbind/remove | Poison and suppress | Poison and suppress | Poison and suppress | Suppress |
| FunctionFS Disable/UDC detach | Normal lifecycle cleanup/recreate | Contain | Contain | Contain | Suppress |
| FunctionFS Suspend/Resume | Record; new session policy is allowed | Contain | Contain | Contain | Suppress |
| EP0 read/error | Normal detached handling only | Contain | Contain | Contain | Suppress |
| AIO timeout/completion error | N/A | Contain if acceptance is ambiguous | Contain | Contain | Suppress |

No row may prove safety by `io_cancel`, endpoint close/drop, UDC unbind,
service restart, blocking retry, or descriptor/mode switch. A contained case
must preserve evidence and use the physical containment/reset procedure before
the next case.

## Matrix

| ID | Injection and precondition | Expected result | Allowed action | Forbidden action | Acceptance/evidence |
| --- | --- | --- | --- | --- | --- |
| N1 | Ten normal cycles: completed transaction is Idle, then physical detach and reconnect | New FunctionFS/UDC session transfers successfully and returns Idle | Idle cleanup/rebind | Manual repair; stale fd/operation/metadata | Per-cycle descriptor/mode, sequence/PID, success, final Idle; no timeout, poison, `-71`, DWC2/kernel/pstore fault |
| N2 | Disable/detach while aggregate and AIO are Idle | Normal cleanup and next session succeed | Close/unbind/recreate | Containment recovery masquerading as normal | Idle markers before/after and fresh successful transaction |
| F1 | Offline only: lifecycle observation immediately after entering Arming, before proven `io_submit` result | Ambiguous Arming becomes Poisoned | Evidence preservation | Arming-to-Idle unless zero acceptance is proved | Focused unit result and rationale: hardware cannot distinguish this boundary safely |
| F2 | Valid SET_BUFFER, accepted exact request, aggregate InFlight; perform one predeclared physical disconnect/disable | Poisoned containment; no framebuffer processing | Physical containment/reset after capture | Cancel, close, unbind, restart, fallback | State/sequence/operation markers, host result, Pi and phone kernel logs |
| F3 | Exact completion then Processing ownership; inject a deterministic test-only processing barrier before finalization, then lifecycle event | Poisoned containment; metadata stays owned; no new admission | Physical containment/reset after capture | Finalize or teardown | Barrier configuration/log, Processing state, sequence/operation, no second SET_BUFFER |
| F4 | Accepted request exceeds production completion deadline | Timeout increments once and becomes Poisoned | Physical containment/reset after capture | Assume Idle, cancel, fallback, unbind/restart | Timeout marker, active identity, host/Pi result |
| F5 | Offline deterministic completion/processing errors: short, zero, wrong identity, duplicate/cross association, completion error, processing error | Poisoned; invalid bytes never reach framebuffer processing | Offline unit qualification | USB corruption experiments | Test names/results and state assertions |
| F6 | FunctionFS gadget `Suspend` while Idle | Record concrete FunctionFS suspend; resume must establish a fresh normal session unless proven safe | Idle cleanup/new session | Generic "suspend" claims | Event markers and reconnect transfer |
| F7 | FunctionFS gadget `Suspend`, `Resume`, or `Disable` after accepted ownership | Poisoned containment | Physical containment/reset after capture | Continue old session or forget request | Lifecycle event/state and ownership markers |

`Suspend` in this matrix specifically means the FunctionFS `Suspend` event
delivered by the Pi gadget USB controller. It is not Pi system suspend and is
not an assumed host/phone power-management event. The implementation observes
FunctionFS `Suspend`, `Resume`, and `Disable` (mapped to disconnected); Pi
system suspend is outside E1-T05 unless it demonstrably produces one of those
events.

## Test-only processing barrier

F3 uses the debug-build-only `GUD_TEST_E1_T05_PROCESSING_BARRIER` hook. Set it
to `processing-before-idle` together with
`GUD_TEST_E1_T05_PROCESSING_BARRIER_DIR=<new evidence case directory>/barrier`.
It creates `processing-ready-seq-<sequence>`, waits at most 30 seconds for the
operator-created `processing-release-seq-<sequence>`, then deterministically
injects a FunctionFS-disconnect lifecycle observation while Processing owns the
payload. It is disabled by default, unavailable in release builds, logs its
configuration and sequence, and permits only one injection. The wait is
bounded and does not cancel I/O. Do not use it in normal reconnect cases.

## Offline qualification

Current focused coverage establishes:

- Idle lifecycle/shutdown cleanup is allowed.
- rejected zero-accepted submission returns Idle.
- Arming lifecycle ambiguity, InFlight shutdown, Processing lifecycle/shutdown,
  timeout, short/zero/wrong completion, processing error, and ordinary cleanup
  after poison are contained.
- overlapping `SET_BUFFER`, sequence reuse, fallback, and descriptor-mode
  switching are rejected by the existing production architecture tests.

Run before hardware:

```sh
cargo test -p gud-gadget
cargo test -p gud-drm
git diff --check
```

The vendored `usb-gadget` integration suite needs a configfs/UDC host and is
not a substitute for these focused offline tests.

## Hardware evidence procedure

Before each case create a new immutable directory under
`evidence/functionfs-status-on-set-e1-t05-normal-reconnect-<timestamp>/` or
`evidence/functionfs-status-on-set-e1-t05-fault-<case>-<timestamp>/`. Record
both commits, clean-worktree proof, artifact SHA256 values, this runbook
version, receive configuration, initial Idle state, and prior kernel/pstore
checks. Capture host logs, Pi userspace/kernel logs, phone kernel logs,
pstore results, validation summary, and `SHA256SUMS`.

For every fault case, immediately preserve evidence after the declared
containment marker. Do not invoke normal service stop, unbind, restart, or
reboot. Use the established physical containment/reset procedure, then prove a
fresh baseline before another case. A case passes only if its ownership rule is
preserved; it need not reconnect.

E1-T05 is verified only after N1 is 10/10 and every declared applicable fault
case has retained reproducible evidence with no unexplained FunctionFS/DWC2,
kernel, or pstore fault.

# E1-T05 Lifecycle Matrix

## Status and prerequisite

Status: E1-T05 is verified for v1: N1, F1-F5, and F7 are qualified. F6 is deferred P2
as a target-platform PM limitation. E1-T04 is a satisfied prerequisite: its canonical
clean-source 100-frame result is
`evidence/functionfs-status-on-set-e1-t04-clean-source-passing-rerun-20260818T030651Z/`.
It used gadget commit `6aea5e73865f3a4cb449c0f817cadf752d721f65` and host
commit `bde330da2c5133c80d8be36fdf3e88c5a5386bb8`.

The 2026-08-18 normal-reconnect run is retained at
`evidence/functionfs-status-on-set-e1-t05-normal-reconnect-20260818T033000Z/`.
It failed at cycle 1 of 10 after a completed transaction reached Idle: the
declared Idle detach produced FunctionFS Suspend, but reattach did not restore
a usable Pi session and the OnePlus later logged USB descriptor `-110` errors.
The fresh-baseline retry at
`evidence/functionfs-status-on-set-e1-t05-normal-reconnect-retry-20260818T034100Z/`
reproduced the same gate failure: after safe Idle suspend, `1d50:614d` did not
return within 45 seconds. The isolated diagnosis at
`evidence/functionfs-status-on-set-e1-t05-idle-role-power-diagnosis-20260818T041030Z/`
identified the concrete test-topology failure: after a successful 12,800-byte
transaction returned to Idle, the OnePlus `host -> device -> host` role cycle
changed the Pi boot ID from `11a762a5-8094-49f5-aad0-0c63f8a7a84b` to
`35ecfcf3-33ff-4a18-a80f-76133763104a`. The Pi therefore did not survive as a
FunctionFS/DWC2 session. Its new boot then lost the DRM-master startup race,
exited after repeated `set_crtc` `Permission denied`, and left the UDC
`not attached`, while the OnePlus logged descriptor `-110` and address `-62`.
No orderly Pi shutdown, kernel fault, DWC2 fault, or pstore record was found.
The rebooted Pi also reported `6.12.47+rpt-rpi-v8`, not the pinned
`6.12.47+rpt-rpi-v8-ffs-xfercompltrace`; this does not affect the reset
diagnosis, but it independently disqualifies the attempt as N1 qualification.

Treat OnePlus controller role switching as a topology reset operation, not an
N1 detach mechanism, until Pi power and the OTG VBUS/backfeed path are isolated
and boot-ID continuity is demonstrated. The evidence does not distinguish loss
of Pi supply from a transient when host VBUS and enumeration return. No fault
case may run until a valid N1 method is established and normal reconnect passes
10/10.

The follow-up topology and restart-policy validation is retained at
`evidence/functionfs-status-on-set-e1-t05-topology-validation-20260818T042317Z/`.
It restored the pinned kernel and proved a second normal-reconnect blocker:
after a successful exact transaction and Idle Suspend, PID 582 performed safe
Idle teardown and exited zero, but deployed production/diagnostic drop-ins made
the effective policy `Restart=no`, so no fresh process was created. The then-
deployed restart-on-success policy is superseded by the persistent same-PID
implementation: nonzero exits still remain down, and a normal idle reconnect
does not exit. The persistent same-PID implementation supersedes that restart
policy: a proven-idle cable reconnect retains the UDC binding, FunctionFS
objects, endpoint files, DRM resources, and exact-AIO sequence. A corrected
PID 3848 completed another exact transaction to Idle,
but the following cable/re-role sequence hard-reset the Pi before a fresh
session or post-reconnect transaction. Therefore the policy correction is not
hardware-qualified and N1 remains 0/10. Electrically verify supply continuity
and OTG VBUS isolation before another hardware attempt.

The 2026-08-19 independent-power/data-only run qualified N1 and F2. The release
artifact from gadget commit `828deb3` (SHA-256
`2ac10f2458bec2608d86c38c274de95edc38c14abdb4bf0943bc8ef2115e1687`)
passed N1 10/10 with PID 974, unchanged boot ID
`ccd1fa95-f335-4f2b-bb4a-8c58d34862fb`, `NRestarts=0`, activation generation
1 through 11, exact-AIO sequence 1 through 11, and final Idle. Per-transfer
host evidence is retained outside the worktree at
`/tmp/opencode/e1-t05-cycle-{01..10}-20260819T*.`, with the baseline at
`/tmp/opencode/e1-t05-baseline-after-shutdown-20260819T033440Z`.

F2 used the test-only host pre-bulk pause (`gud` commit `7e75222`) and the
debug-only Pi deadline override (`gadget` commits `259253f` through `3e8f6dd`).
The accepted sequence 2 was InFlight when the verified physical data detach
caused phone device absence and FunctionFS Suspend. The Pi contained it as
Poisoned without `payload_completed` or `frame_stats` for sequence 2; the
phone's later bulk submit returned `-19` because the device was absent. Evidence
is at `/tmp/opencode/e1-t05-f2-debug90-inflight-final-20260819T042101Z`.
The release artifact was restored and the temporary debug drop-in removed after
the contained case. pstore was empty and no Pi kernel/DWC2 fault was observed.

The final F6 diagnostic used the isolated PM-test host artifact from `gud`
commit `db8ee75` (SHA-256 `2c195d5bdfead8ba225d18e82113453e239d4a379736cbd35944871773f4c44b`).
It proved real Idle FunctionFS Suspend and Resume, but target runtime-PM resume
then physically re-enumerated the GUD USB device: stable topology/sysfs path
`1-1.2` and bus `1` remained, while host device number changed `5 -> 6`.
The Pi consequently observed `Suspend -> Resume -> Suspend -> Disable -> Enable`
and activation `2 -> 3`. The host did not log `GUD_PM_TEST resume result=0` or
`reset_resume`; it logged a fresh PM-test probe. Same-activation persistent
resume is therefore deferred P2, not required for the v1 qualified envelope.
Evidence is `/tmp/opencode/e1-t05-f6-final-pm-diagnostic-20260819T051300Z`.

This ticket qualifies ownership and containment, not transparent recovery from
accepted I/O. Run one matrix case per fresh known-safe session. E1-T06 may use
the N1-qualified reconnect and exact-AIO envelope; it must not claim F6 resume
support.

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
for the persistent process lifetime and continue across normal reconnects. An accepted
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
| FunctionFS Disable | Reset host state, render waiting screen, retain all objects | Contain | Contain | Contain | Suppress |
| FunctionFS Suspend/Resume | Retain objects; Idle Resume returns to Active | Contain | Contain | Contain | Suppress |
| FunctionFS Unbind | Controlled fatal lifecycle result | Contain | Contain | Contain | Suppress |
| EP0 read/error | Retain session; FunctionFS lifecycle events decide state | Contain | Contain | Contain | Suppress |
| AIO timeout/completion error | N/A | Contain if acceptance is ambiguous | Contain | Contain | Suppress |

No row may prove safety by `io_cancel`, endpoint close/drop, UDC unbind,
service restart, blocking retry, or descriptor/mode switch. A contained case
must preserve evidence and use the physical containment/reset procedure before
the next case.

## Matrix

| ID | Injection and precondition | Expected result | Allowed action | Forbidden action | Acceptance/evidence |
| --- | --- | --- | --- | --- | --- |
| N1 | Passed 2026-08-19: ten completed-Idle physical data detach/reconnect cycles | Same PID/UDC/FunctionFS objects observed `SUSPEND -> DISABLE -> WaitingForHost -> ENABLE -> Active`, transferred successfully, and returned Idle | Persistent idle transition | Manual repair, restart, unbind, FunctionFS recreation, stale fd/operation/metadata | PID 974, boot ID and `NRestarts=0` unchanged; activation 1..11 and sequence 1..11; no timeout, poison, `-71`, DWC2/kernel/pstore fault |
| N2 | Disable/detach while aggregate and AIO are Idle | Same persistent session renders waiting state; next Enable creates a new host activation | Retain objects | Close/unbind/recreate or containment recovery masquerading as normal | Idle markers before/after, unchanged PID/UDC/FunctionFS, and fresh successful transaction |
| F1 | Offline only: lifecycle observation immediately after entering Arming, before proven `io_submit` result | Ambiguous Arming becomes Poisoned | Evidence preservation | Arming-to-Idle unless zero acceptance is proved | Focused unit result and rationale: hardware cannot distinguish this boundary safely |
| F2 | Passed 2026-08-19: valid SET_BUFFER/accepted exact sequence 2 held InFlight by test-only host pause, then verified data-path detach | FunctionFS Suspend contained the request as Poisoned; no payload completion or framebuffer processing | Physical containment/reset after capture | Cancel, close, unbind, restart, fallback | Phone absence marker, `-19` post-detach bulk submit, Pi InFlight/operation markers, `e1_t05_lifecycle_containment`, empty pstore, and no Pi kernel fault |
| F3 | Exact completion then Processing ownership; inject a deterministic test-only processing barrier before finalization, then lifecycle event | Poisoned containment; metadata stays owned; no new admission | Physical containment/reset after capture | Finalize or teardown | Barrier configuration/log, Processing state, sequence/operation, no second SET_BUFFER |
| F4 | Accepted request exceeds production completion deadline | Timeout increments once and becomes Poisoned | Physical containment/reset after capture | Assume Idle, cancel, fallback, unbind/restart | Timeout marker, active identity, host/Pi result |
| F5 | Offline deterministic completion/processing errors: short, zero, wrong identity, duplicate/cross association, completion error, processing error | Poisoned; invalid bytes never reach framebuffer processing | Offline unit qualification | USB corruption experiments | Test names/results and state assertions |
| F6 | Deferred P2 2026-08-19: target PM-test runtime resume delivered real Idle Suspend/Resume, then USB device re-enumeration (`devnum 5 -> 6`) caused Disable -> Enable and activation `2 -> 3` | Same-activation persistent resume is not qualified for v1 | Retain objects | Generic "suspend" claims or re-enumeration as resume | Retained PM sysfs/host/Pi evidence classifies the platform limitation; no production PM behavior is enabled |
| F7 | Passed 2026-08-19 by retained F2 evidence: real FunctionFS Suspend after accepted InFlight ownership | Poisoned containment | Physical containment/reset after capture | Continue old session or forget request | Sequence 2 InFlight/operation markers, FunctionFS Suspend, terminal Poisoned containment, and no payload/frame processing |

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

- Idle lifecycle reuse is allowed; actual shutdown alone performs UDC-first cleanup.
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

For N1, verify stable independent Pi power, isolate unintended OTG VBUS/backfeed,
and detach only the data path. Record Pi boot ID and monotonic uptime before
detach and after reconnect; either a changed boot ID or reset uptime invalidates
the case as a power/topology reset. Do not use OnePlus
`host -> device -> host` as the detach while that operation resets the Pi.
Verify the pinned Pi kernel before each case; a fallback boot is not acceptable.
Re-enumeration alone is insufficient:
each cycle requires a FunctionFS Enable/new activation generation in the same
process and FunctionFS instance, one successful exact 12,800-byte transaction,
and final aggregate/AIO Idle ownership.

For every fault case, immediately preserve evidence after the declared
containment marker. Do not invoke normal service stop, unbind, restart, or
reboot. Use the established physical containment/reset procedure, then prove a
fresh baseline before another case. A case passes only if its ownership rule is
preserved; it need not reconnect.

E1-T05 is verified only after N1 is 10/10 and every declared applicable fault
case has retained reproducible evidence with no unexplained FunctionFS/DWC2,
kernel, or pstore fault.

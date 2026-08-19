# E1-T05 Persistent FunctionFS Reconnect Handoff

## Mission

Refactor the Rust GUD gadget so a normal USB disconnect is a host-session
transition inside one long-lived process, not the lifetime boundary of the
process or FunctionFS gadget.

For a disconnect observed with proven-idle receive ownership, preserve:

```text
same gud-drm PID
same registered configfs gadget and UDC binding
same FunctionFS mount, ep0, and ep1 file descriptors
same DRM resources
same exact-AIO driver and monotonically increasing transaction sequence
```

On reconnect, consume the next FunctionFS `ENABLE`, reset per-host protocol
state, and service enumeration and exact `STATUS_ON_SET` transfers normally.
Do not unbind the UDC, remove FunctionFS, exit, or rely on systemd for a normal
idle disconnect.

This is a small lifecycle refactor, not a rollback. Preserve the production
exact-AIO architecture introduced for E1-T03/T04. Do not start E1-T06.

## Workspace and baseline

Repository:

```text
/home/cristianr/Projects/linux-mobile/gud-gadget
branch: pixel-format-benchmark
baseline HEAD: ea052ec4706ac1e101a33247061cbd41244fd235
host repository: /home/cristianr/Projects/linux-mobile/gud
qualified host HEAD: f5eb86be0b3844d126143cfccb2da6cb49d37dab
```

Baseline source hashes on 2026-08-18:

```text
c12e25f99fbcd13e7d2bc15e807cf89fe8271f445e8b9589c9dda03dfc6d7b94  gadget/src/lib.rs
11a15f3a99a34cf28db9013b84f318f67020be3844bc8eab4773c94da988c0df  drm/src/main.rs
ff4e63e1523fd305b256eaedad14470fa5bd92bd30e6b6529ee1cd355fae863c  vendor/usb-gadget/src/function/custom/mod.rs
8598f8d41373f3d55949b58fa9fd0e0b8eec3a03ddc6950d366fe0156f3d152a  vendor/usb-gadget/src/function/custom/aio/mod.rs
1336d54de17f2c4136387639623dab66ddd79312004e34a701c13e24f21f803d  docs/e1-t05-lifecycle-matrix-runbook.md
```

These hashes identify the handoff baseline. They are not instructions to
overwrite newer user or agent changes.

The worktree is intentionally dirty with tracking edits, test-only kernel
artifacts, and evidence. Read `AGENTS.md` and `git status` first. Never reset,
clean, stash, restore, or modify unrelated work. In particular, preserve the
current edits to `backlog.md` and `docs/e1-t05-lifecycle-matrix-runbook.md`.

## Why this pivot is justified

The normal reconnect path added by commit
`646d714421f9187be79648576929ba70d4b7c0a9` changed the architecture from a
persistent FunctionFS process to:

```text
disconnect
-> set restart_requested
-> leave event loop
-> unbind UDC
-> remove FunctionFS
-> close endpoint owners
-> exit
-> systemd starts a fresh PID
-> create FunctionFS and bind UDC
-> immediate host enumeration
```

The current E1-T05 retry successfully recreated the process and completed a
post-reconnect 12,800-byte transfer, but the host's first descriptor attempt
reported transient `-71` (`EPROTO`) before a later enumeration succeeded. That
run does not satisfy N1. Its new retry logs were not yet preserved in a
dedicated evidence directory at handoff time.

The last Rust revision before restart-on-disconnect,
`c636dd07e924f250a0f6ec1e07ba117bd9cc941f`, kept the same process, FunctionFS
files, endpoint objects, and UDC binding across `DISABLE` and `ENABLE`. Commit
`646d714` deliberately introduced process recreation; FunctionFS did not force
it.

Linux 6.12 FunctionFS supports the persistent object lifetime:

- `ffs_func_disable()` disables endpoints and emits `FUNCTIONFS_DISABLE`.
- `ffs_func_eps_disable()` clears each live endpoint mapping but does not close
  userspace endpoint file descriptors.
- Pending gadget requests are completed with `-ESHUTDOWN` on endpoint disable.
- A later `ffs_func_set_alt()` enables/remaps the existing endpoint files and
  emits `FUNCTIONFS_ENABLE`.
- An ordinary cable reconnect normally gives `DISABLE -> ENABLE`, not a new
  `BIND`. Explicit UDC/configfs removal gives `UNBIND -> BIND -> ENABLE`.
- FunctionFS lifecycle events can be coalesced, so code must not require every
  intermediate event to be observed.

Relevant Linux 6.12 sources:

- <https://docs.kernel.org/6.12/usb/functionfs.html>
- `drivers/usb/gadget/function/f_fs.c`: `ffs_func_eps_disable()`,
  `ffs_func_eps_enable()`, `ffs_func_set_alt()`, `ffs_func_disable()`, and
  `__ffs_event_add()`
- `drivers/usb/gadget/udc/core.c`: `usb_ep_disable()` completion contract
- `drivers/usb/dwc2/gadget.c`: disconnect and endpoint-disable request giveback

The Linux `tools/usb/ffs-aio-example` programs also keep their process and
endpoint files alive across `DISABLE`/`ENABLE`, but they are examples, not a
safety model for this implementation. They do not enforce exact operation
identity, exact byte count, status ordering, or ownership through processing.
The reference C GUD function is kernel code, not a userspace process.

Persistent idle reconnect is therefore supported and historically exercised,
but accepted AIO recovery is not established. This task must retain terminal
containment for all non-idle lifecycle ambiguity.

## Required production invariants

Do not weaken any of these:

- `GUD_RECEIVE_MODE=status-on-set-aio`
- `GUD_DISPLAY_FLAG_STATUS_ON_SET` ordering
- one exact EP1 OUT native-AIO request with queue depth one
- exact accepted `EndpointOperation` identity
- exact completion byte count
- accepted operation, buffer, and `SET_BUFFER` metadata owned through
  framebuffer presentation
- no speculative receive, fallback, overlap, descriptor change, or in-session
  transfer-mode change
- `ExactAioState: Idle -> Arming -> InFlight -> Processing -> Idle`
- `Arming` returns to `Idle` only when `io_submit()` is proven to have accepted
  zero requests
- `Arming`, `InFlight`, `Processing`, and `Poisoned` lifecycle ambiguity is
  contained; it is not reset, canceled, closed, unbound, or restarted
- actual process/service shutdown may tear down only from proven `Idle`

Do not use the old Rust implementation or the Linux C examples as permission
to pipeline reads or discard `-ESHUTDOWN` completions.

## USB session state machine

Add a small state machine separate from exact-AIO ownership:

```rust
enum UsbSessionState {
    WaitingForHost,
    Active,
    Suspended,
    Contained,
}
```

The exact names may change if a smaller representation is clearer, but these
states and transitions must remain explicit and testable.

| Input | Precondition | Required result |
| --- | --- | --- |
| Startup or `BIND` | no active host | `WaitingForHost`; do not infer transfer readiness |
| `ENABLE` | not `Contained` | reset per-host protocol/status state, increment activation generation, enter `Active` |
| repeated/coalesced `ENABLE` | not `Contained`, proven Idle | treat as a new activation; reset host state and remain/enter `Active` |
| `SUSPEND` | proven Idle | enter `Suspended`; retain all FunctionFS/DRM resources |
| `RESUME` | `Suspended`, proven Idle | enter `Active` after existing protocol invalidation handling |
| `DISABLE` | proven Idle | reset per-host state, render waiting screen, enter `WaitingForHost` |
| lifecycle input | `Arming`, `InFlight`, `Processing`, or `Poisoned` | enter terminal `Contained`; preserve existing receive ownership and suppress teardown |
| unexpected `UNBIND` | proven Idle | controlled fatal lifecycle result; do not treat it as ordinary cable disconnect |
| unexpected `UNBIND` | non-Idle | terminal `Contained` |
| SIGTERM/service shutdown | proven Idle | existing UDC-first teardown, FunctionFS removal, DRM release, and exit |
| SIGTERM/service shutdown | non-Idle | existing terminal containment; no close/unbind/SIGKILL |

`Contained` is terminal for the current process. Do not attempt automatic
accepted-AIO recovery in this patch.

## Proven-idle reuse gate

Normal reconnect reuse is allowed only after one centralized predicate proves
all relevant ownership layers are idle:

```text
BulkReceiveSession == Idle
ExactAioTransaction == Idle
ExactAioTransaction accepted operation == None
EndpointReceiver AIO queue is empty
pending_exact_receive == None
ready_exact_payload == None
status_on_set_cleanup_drain == None
STATUS_ON_SET admission/diagnostic guard is inactive
no prearmed diagnostic receive is active
```

Do not rely only on `BulkReceiveSession::current_state()`. Expose the minimum
read-only state needed from `PixelDataEndpoint`/`EndpointReceiver`, and produce
one structured log describing every gate component when a lifecycle event is
classified.

The bounded test-only `STATUS_ON_SET` diagnostic currently treats lifecycle
events after its configured transaction count starts as containment. Preserve
that behavior. Production unsets the diagnostic environment variables.

## Lifecycle generation

Use a distinct USB activation generation, incremented on every processed
`ENABLE`. Do not equate it with the existing display-state generation.

Capture the activation generation in `ExactAioTransaction` when arming and
include it in submission, completion, lifecycle, and containment telemetry.
When harvesting a completion, a generation mismatch is stale/ambiguous and
must poison/contain the transaction.

Generation equality is not proof that an operation did not cross a physical
lifecycle boundary:

- EP0 lifecycle events and AIO completions have no common ordering guarantee.
- A completion may be published before or after userspace reads `DISABLE`.
- FunctionFS lifecycle events may be coalesced.
- Endpoints are enabled before the userspace `ENABLE` event is consumed.

Generation is therefore an additional rejection rule and diagnostic identity,
not a replacement for exact operation identity, byte-count validation, the
proven-idle gate, or terminal containment.

## Current code hazards to address

### Restart paths

`drm/src/main.rs` currently sets `restart_requested` in five places around:

- EP0 event-read failure: lines 3087-3130
- timeout plus detached UDC: lines 3147-3170
- a read event plus detached UDC: lines 3172-3197
- parsed FunctionFS `DISABLE`: lines 4022-4118
- control-path parse failure: lines 4703-4737

The third path discards an already-read FunctionFS event before passing it to
`gud_gadget::event()`. A persistent implementation must process that event or
explicitly classify it; it must not silently lose protocol/lifecycle reset.

Remove normal-disconnect dependence on `restart_requested`. Remove obsolete
restart-policy helpers/tests if they no longer express real behavior.

### UDC state is not the lifecycle state machine

`udc_is_detached()` currently treats every state except `Configured` as
detached. That includes `Suspended`, `Reconnecting`, `Addressed`, and `Powered`.
Do not use this predicate to exit the event loop, discard an event, or tear down
FunctionFS.

FunctionFS lifecycle events are authoritative. UDC state may be logged and may
help decide when to render the waiting screen, but it must not independently
destroy or restart the session.

### FunctionFS event handling

`gadget/src/lib.rs::event()` already resets protocol/status state for `BIND`,
`ENABLE`, `SUSPEND`, `RESUME`, and `DISABLE`. Preserve that behavior while
making session state explicit.

Raw `custom::Event::Unbind` currently falls through the wildcard arm. Handle it
explicitly so an administrative unbind cannot masquerade as an idle cable
cycle.

The vendored `Custom::event_timeout()` does not call `clear_prev_event()`, while
`event()` and `try_event()` do. Inspect whether a failed/stalled setup transfer
can leave `setup_event` set across persistent reconnect. Apply the smallest
safe fix and add a focused unit test where practical; do not broadly rewrite
the vendored event API.

### AIO queue state

`EndpointReceiver::is_empty()` exists, but `PixelDataEndpoint` does not expose
it to the main lifecycle classifier. Add a narrow query rather than exposing
the AIO driver. Queue space becomes available only when a completion is
harvested, so an `Idle` assertion must not hide an unharvested completion.

Do not use `EndpointReceiver::cancel()` for reconnect. Its cancellation is not
a proven quiescence barrier, failed `io_cancel()` operations remain active, and
automatic non-idle recovery is out of scope.

### Shutdown boundary

The teardown at `drm/src/main.rs:4741-4844` correctly claims idle ownership,
unbinds the UDC before FunctionFS removal, drops endpoint owners, then releases
DRM. Keep this order for actual shutdown.

Normal `DISABLE` must no longer reach this block. The main event loop continues
in `WaitingForHost` and consumes the next `ENABLE` with the same objects.

### Service policy and documentation

`systemd/gud-userspace.service.d/98-e1-t04-production-aio.conf` and
`10-xdisp-p0.1-containment.conf` still describe normal proven-idle process
restart. Update their comments and explicitly preserve the nonzero-failure and
`SendSIGKILL=no` containment policy. Normal reconnect acceptance requires
`NRestarts` to remain unchanged.

Update `docs/e1-t05-lifecycle-matrix-runbook.md`; its N1/N2 and lifecycle audit
currently permit or require cleanup/recreation. The new expected result is a
same-PID, same-FunctionFS `DISABLE -> WaitingForHost -> ENABLE -> Active` cycle.
Do not rewrite or discard existing failure evidence.

## Implementation plan

1. Inspect current source, tests, worktree changes, production service
   drop-ins, and the E1-T05 runbook. Confirm no concurrent edit conflicts.
2. Add the explicit USB session state and pure transition/proven-idle
   classification helpers with unit tests before changing the event loop.
3. Expose exact operation absence and endpoint AIO queue emptiness through the
   narrowest read-only `PixelDataEndpoint` API.
4. Add USB activation generation, capture it when exact AIO arms, validate it
   when completion is harvested, and cover match/mismatch behavior in tests.
5. Handle raw FunctionFS `UNBIND` explicitly and preserve the existing
   protocol/status reset semantics of all lifecycle events.
6. Refactor the five restart/detach paths. Never discard an already-read event;
   never break solely because UDC state is not `Configured`.
7. Make proven-idle `DISABLE` render waiting state and continue the same event
   loop. Make the next `ENABLE` activate the new host session without
   recreating endpoint, gadget, or DRM objects.
8. Keep every non-idle lifecycle observation on the current poison/containment
   path, including shutdown suppression and no further EP0 admission.
9. Ensure actual signal/fatal shutdown can reach UDC-first teardown only after
   the centralized idle gate passes.
10. Update focused tests, service comments, E1-T05 runbook rows, and backlog
    state. Do not mark hardware qualification complete from offline results.
11. Run formatting, tests, checks, and lint. Review the final diff for accidental
    changes and stale restart terminology.
12. Build/deploy only after offline qualification, then run the same-PID N1
    hardware gate. Ask the user for only one physical action at a time.

Prefer the smallest correct change. Do not create a generic framework, recreate
the gadget inside an outer process loop, or retain dead process-restart paths
for hypothetical compatibility.

## Required offline tests

At minimum, add focused tests for:

- startup/`BIND` to `WaitingForHost`
- idle `ENABLE` to `Active`
- repeated/coalesced idle `ENABLE` creates a new activation generation
- idle `SUSPEND -> RESUME`
- proven-idle `DISABLE -> WaitingForHost`
- next `ENABLE -> Active` without shutdown/restart intent
- `DISABLE`, `SUSPEND`, `RESUME`, and `UNBIND` during `Arming`, `InFlight`, and
  `Processing` produce terminal containment
- `Poisoned` remains contained
- proven-idle gate rejects every individual non-idle component
- AIO completion with matching operation and activation generation follows the
  existing exact-length/status checks
- generation mismatch poisons without releasing metadata or operation identity
- an already-read lifecycle event is not discarded because UDC is no longer
  `Configured`
- explicit idle shutdown remains allowed
- non-idle shutdown remains suppressed

Run:

```sh
cargo fmt -- --check
cargo test -p gud-gadget
cargo test -p gud-drm
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
git diff --check
```

If an existing unrelated worktree change prevents one command, report the exact
blocker. Do not modify unrelated files to make checks pass.

## Hardware acceptance gate

Do not run fault injection until normal reconnect passes 10/10.

Use the production configuration:

```text
GUD_RECEIVE_MODE=status-on-set-aio
STATUS_ON_SET advertised
queue depth 1
uncompressed XRGB8888
exact payload 12,800 bytes
```

Before the run, verify independent Pi power/data-only detach topology, pinned
kernel `6.12.47+rpt-rpi-v8-ffs-xfercompltrace`, unchanged boot ID, and qualified
host artifact hashes. Do not use a OnePlus role switch that resets Pi power.

For each of ten cycles require:

```text
successful baseline transaction returns aggregate and exact AIO to Idle
physical data detach
FunctionFS DISABLE is observed from proven Idle
USB session becomes WaitingForHost
PID unchanged
systemd NRestarts unchanged
no UDC unbind
no FunctionFS removal or endpoint-owner drop
physical data reconnect
FunctionFS ENABLE is observed
activation generation increases
USB session becomes Active
first descriptor request succeeds
one exact 12,800-byte transfer completes and is presented
exact AIO sequence continues monotonically
aggregate and exact AIO return to Idle
Pi boot ID unchanged and uptime monotonic
```

Whole-run rejection conditions:

```text
any -71, -110, -62, or descriptor retry/failure
any Poisoned or Contained transition
any DWC2/kernel/pstore fault
any PID, boot ID, UDC binding, FunctionFS instance, or systemd restart change
any manual repair between cycles
```

Preserve immutable evidence in a new timestamped directory containing both
repository commits, source/artifact hashes, effective service configuration,
per-cycle lifecycle/AIO telemetry, Pi and phone kernel logs, host logs, boot and
uptime continuity, pstore checks, a results summary, and verified
`SHA256SUMS`.

## Explicitly out of scope

- Automatic recovery after a disconnect during accepted AIO ownership
- Treating `-ESHUTDOWN` as permission to return a poisoned transaction to Idle
- AIO cancellation or stale-completion draining as reconnect recovery
- Gadget/configfs recreation inside the same PID
- Host driver changes
- Descriptor, mode, compression, queue-depth, or payload-size changes
- General DWC2 fixes
- E1-T06 or any T05 fault-injection hardware row before N1 passes 10/10
- Claiming that persistent sessions caused or fixed `-71` without clean
  comparative evidence

## Completion report

At the end, report separately:

1. Source and state-machine changes with file references.
2. Preserved exact-AIO and containment invariants.
3. Offline command results.
4. Hardware observations and evidence paths, if hardware was run.
5. Remaining inferences or unqualified risks.

Do not call E1-T05 complete unless the same-PID normal reconnect gate passes
10/10 with zero USB protocol errors and all evidence checksums verify.

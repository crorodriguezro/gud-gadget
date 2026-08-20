# E2-T02 Current Modeset Timeout Investigation

## Result

**FAIL** -- E2-T02 remains in progress.

## Source and artifacts

- Mir: `e7250c83c4408b3b657035764e623a9919b700e8`
- gud: `69250a0722ece271776b6a93a42cc4fd7f38211c`
- gud-gadget: `e1c038d9707acecee151732f648a550ec23cf849`
- Built and deployed xdispd SHA-256:
  `fae478a57dbb1f2976c283f0d7202f083c64d71b6616f495812c6d006ecedf44`
- Built and deployed mirgud SHA-256:
  `752c2d505d66992ad01468f8126b9902b5f540b2eae7d34edfc052a260598762`

`build-hashes-committed-head.txt` and `deployed-hashes.txt` prove exact
equality after rebuilding from the committed Mir HEAD.

## Lifetime fix and tests

Mir commit `e7250c8` installs an idempotent presenter/KMS resource boundary
before the Mir connection is created. Both Mir connection and screencast
deleters invoke it before releasing their Mir object, so early Mir-era
exceptions stop/join the presenter and release KMS first.

The committed-head build passed:

- `GudPresentationWorker.*:GudHwcBoundary.*`: 6/6
- xdisp/mirgud suite: 49/49

## Timeout classification

The first production wrapper-free managed activation used xrgb8888 on
`/dev/dri/card1`, connector 25, and failed its initial atomic modeset with
`ETIMEDOUT`. It emitted `KMS_TEARDOWN_BEGIN` and `KMS_TEARDOWN_COMPLETE`.

The immediately following known direct control also failed:

```text
stage=caps complete
drmModeAtomicCommit(commit): Connection timed out
```

This is Case A: a GUD/USB/Pi/device-state failure, not a mirgud KMS atomic
request regression. Pi telemetry records a poisoned exact-AIO transaction.

The existing production `gud-userspace.service` was restored after its stale
worker was contained. The gadget re-enumerated at `1-1.2`; the recovered direct
control then passed both `caps` and `atomic-commit`. A subsequent production
DBus `Enable -> Activate` run passed initial modeset and reached `ACTIVE` after
the first real GUD frame.

## Successful presentation observation

The recovered managed run recorded:

- `MODESET_COMPLETE` before the first `ACTIVE` milestone
- 62 presented frames after 63 submitted frames at the two-second report
- `max_pending_observed=1`
- `max_in_flight_observed=1`
- zero reported GUD submit failures through the captured periodic reports

## Blocking shutdown observation

The normal DBus `Deactivate` did not produce `CAPTURE_LOOP_EXIT`,
`PRESENTER_STOP_*`, `KMS_TEARDOWN_*`, `SCREENCAST_RELEASE_*`, or
`MIR_CONNECTION_RELEASE_BEGIN` before the xdispd three-second containment
deadline. xdispd therefore recorded:

```text
LastChildExit=forced-sigkill:signal:9
LastStopForced=true
ForcedStopCount=1
```

The process was contained with no leftover mirgud and the phone boot, LightDM,
Lomiri compositor, and xdispd remained stable. However, KMS teardown before
Mir/screencast release and final zero-pending accounting were not observable.
Those unmet E2-T02 gates prevent verification and creation of a final
current-binary qualification bundle.

## Evidence map

- `managed-smoke.log`: initial timeout, KMS teardown, and poison transition
- `direct-kms-control.log`: matching direct timeout
- `pi-production-aio-telemetry.log`: poisoned exact-AIO receiver evidence
- `pi-restart-failure.log`: stale worker containment diagnosis
- `direct-kms-control-after-e1-recovery.log`: recovered direct control PASS
- `managed-smoke-after-e1-recovery.log`: recovered production activation
- `shutdown-markers.log`: failed normal shutdown ordering
- `phone-after.txt` and `pi-after.txt`: post-run continuity and diagnostics

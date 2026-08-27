# XDISP-P2.1 Dynamic Mode Matching Test

## Status

This is a historical P2.1 gate procedure. E4-T03 supersedes its transport and
deployment assumptions: exact complete-timing physical routing is normal
runtime behavior and no longer requires `GUD_TEST_DYNAMIC_MODE_MATCH=1`.
The historical commands and recorded evidence below remain snapshots; do not
use their 12,800-byte descriptor cap or 16 KiB FunctionFS request size as a
logical GUD transaction limit.

Canonical design and plan:

- `../../gud/docs/superpowers/specs/2026-07-26-xdisp-p2-1-dynamic-mode-matching-design.md`
- `../../gud/docs/superpowers/plans/2026-07-26-xdisp-p2-1-dynamic-mode-matching.md`

This procedure does not verify `XDISP-P0.1`, start `XDISP-P0.2`, or complete
all of `XDISP-P2.1`.

## Intended behavior

In current normal runtime:

- an exact full-timing connector match uses a matching physical DRM mode and
  direct rectangle copy;
- a non-exact or synthetic mode keeps the existing aspect-fit scaled
  presentation;
- repeated commits of the same mode do not reallocate or modeset; and
- USB advertised mode ordering/preference never changes when physical output
  changes.

The obsolete `GUD_TEST_DYNAMIC_MODE_MATCH=1` value is accepted for deployment
compatibility but has no enabling effect. `GUD_TEST_OUTPUT_MODE` remains a
test-only startup/fallback selection override and does not alter the advertised
physical catalog.

## Standing safety boundary

Keep unchanged:

- separately preserved OnePlus adaptive-LZ4 diagnostic module;
- one negotiated logical full-frame update per production transaction;
- current 1280x720 RGB565 logical payload: 1,843,200 bytes;
- Pi/DWC2/FunctionFS request chunking is an internal implementation detail,
  not a logical GUD payload limit;
- normal OnePlus `/home/phablet/gud.ko`;
- both installed kernels; and
- Mir/Lomiri.

Never stop, restart, reboot, signal, replace, or roll back the Pi service while
USB is connected or the receive state is `InFlight` or `Poisoned`.

Before every hardware connection, verify the intended Pi drop-in inventory
and structured startup descriptor log while detached. Start a
snaplen-sufficient host usbmon capture before attachment. After fresh
enumeration, run:

```text
python3 gud/backport-4.9/tests/decode-gud-usbmon-descriptor.py HOST_USBMON_LOG
```

Require the actually served descriptor to report
`GUD_DISPLAY_FLAG_STATUS_ON_SET == 0` before any KMS state or `SET_BUFFER`
runner is allowed. Never run a deliberately invalid connector, format, or
timing request against the Pi. Negative protocol cases belong only in
deterministic unit/mock tests because this single-threaded control/bulk design
cannot safely service the follow-up status handshake.

The planned preflight command shape is:

```text
# Pi, while detached
journalctl -u gud-userspace.service -b --no-pager | grep descriptor_config

# Host, started before attachment; stop with SIGINT after enumeration
sudo sh -c 'cat /sys/kernel/debug/usb/usbmon/0u > /tmp/xdisp-descriptor-usbmon.log'

# Analysis workstation, before running KMS traffic
python3 gud/backport-4.9/tests/decode-gud-usbmon-descriptor.py \
    /tmp/xdisp-descriptor-usbmon.log

# Host, start only after descriptor approval and keep through the payload gate
sudo sh -c 'cat /sys/kernel/debug/usb/usbmon/0u > /tmp/xdisp-full-usbmon.log'
```

Retain the structured Pi log line, descriptor capture, decoded JSON, and full
payload capture. The 30-byte descriptor response must be present in the first
capture; a truncated capture is a failed preflight. The second capture starts
before the KMS runner and stops only after its last bulk completion, so actual
URB lengths and correlations are not lost while the descriptor snapshot is
decoded.

Before every service mutation:

1. Physically remove the Pi-host data cable while keeping Pi power connected.
2. Prove the host logged GUD disconnect and removed its GUD DRM card.
3. Prove the last Pi receive returned to `Idle`.
4. Require later `Suspend`/safe detach evidence and no DWC2/vc4/Oops fault.
5. Only then perform one controlled stop.

There is one explicit never-attached branch for a service freshly started
while the data cable has remained physically absent. A controlled stop may
proceed without host-card-removal or `Suspend` evidence only when all of these
are recorded:

- the current service instance logs `had_host_session=false`;
- receive state is `Idle`, never `InFlight` or `Poisoned`;
- the UDC is unattached and has never reached configured state in this
  instance;
- no host data cable is present; and
- service/kernel logs contain no DWC2, vc4, Oops, watchdog, or reboot fault.

Use this branch to install the first laptop cap after a detached start or a
fresh boot. Once any host has enumerated, the normal
detach/card-removal/`Idle`/`Suspend` boundary is mandatory.

If any receive is missing `Idle`, remains blocked, or becomes `Poisoned`, do
not attempt graceful teardown. Collect read-only evidence, detach physically,
and use the existing containment hardware-recovery path.

## Controlled laptop boundary

KDE previously claimed the gadget immediately as an extended monitor. Before
Gates 0, C, or F, place the laptop in a headless/quiesced graphics session
before attachment. After enumeration:

1. dynamically discover the new GUD DRM node;
2. require the display manager/compositor to remain stopped or otherwise
   prevented from auto-modesetting that node;
3. run `sudo fuser -v GUD_DRM_NODE` (and the corresponding render node, if
   present) and require no compositor/display-manager opener; and
4. do not release the controlled runner until descriptor decoding passes.

Any unsolicited `SET_STATE`, `SET_BUFFER`, or GUD-node opener fails the gate
and requires clean physical detach. Do not race KDE with the test runner.

## Offline gate

Required before staging:

```text
cargo fmt --all -- --check
cargo test -p gud-gadget -p gud-drm -- --test-threads=1
```

Use the known working Fedora AArch64 overrides for the release build:

```text
env CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-redhat-linux-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='-C link-arg=-static-libgcc' \
    'CC_aarch64-unknown-linux-gnu=aarch64-redhat-linux-gcc' \
    'CFLAGS_aarch64-unknown-linux-gnu=' \
    cargo build --release -p gud-drm
```

Build the planned laptop runner on the controlled x86-64 laptop:

```text
cc -Wall -Wextra -Werror -O2 $(pkg-config --cflags libdrm) \
    -o gud/backport-4.9/tests/gud-kms-mode-sequence \
    gud/backport-4.9/tests/gud-kms-mode-sequence.c \
    $(pkg-config --libs libdrm)
sha256sum gud/backport-4.9/tests/gud-kms-mode-sequence
```

Record:

- source commit;
- binary SHA-256;
- unit-test log;
- cross-build log;
- dynamic `50-*` drop-in SHA-256;
- max-only laptop `60-*` drop-in SHA-256;
- descriptor-decoder contract log; and
- the known rollback binary/configuration hashes.

### Recorded offline implementation evidence (2026-07-26)

This record covers Tasks 1--7 only. No phone/Pi deployment, service change,
USB attachment, or hardware-gate action was performed.

- `gud-gadget` source commit:
  `d2fa1d4119b7deb308f5effc3550adc14af96fa6`
  (`XDISP-P2.1 switch exact modes on committed state`).
- `gud` descriptor-tool commit:
  `97fc774` (`XDISP-P2.1 decode served GUD descriptors`).
- `gud` mode-runner/source commit:
  `96bdcb5f4e98d421919078143630484db7e7a1ba`
  (`XDISP-P2.1 add generic KMS mode-sequence runner`).
- AArch64 `target/release/gud-drm` SHA-256:
  `55631c1c73701a7fd54693ab967567e5d2d16629857896a7c46a6991588e8b67`.
- Dynamic `50-*` drop-in SHA-256:
  `648835c21ae4069a3348f7ce8cee823ac54e1ebac6c528786f9df4e0e1b4b4fc`.
- Max-only laptop `60-*` drop-in SHA-256:
  `8b28512fa118c147294d7c816b5b5213a229f1dd011b51abf0e9fcb0383fef74`.
- Build host/toolchain: AArch64 Fedora; `rustc 1.93.1`, `cargo 1.93.1`,
  `aarch64-redhat-linux-gcc 16.1.1`, `cc 16.1.1`, and `libdrm 2.4.134`.
- The exact serialized test command above passed with 56 `gud-drm` tests
  and 47 `gud-gadget` tests. The `gud-drm` ownership/failure suite also
  passed 25 consecutive repetitions.
- `gud-drm` and `gud-viewerd` dependent checks passed together. Focused
  clippy completed successfully with dependency linting excluded. A strict
  `-D warnings` run is not a gate in this tree because existing vendored
  `usb-gadget` warnings and pre-existing style lints fail it.
- All eight `gud/backport-4.9/tests/test-*-contract.sh` suites passed,
  including the descriptor decoder and mode-sequence runner contracts.
- A current-host AArch64 build of `gud-kms-mode-sequence` passed its offline
  self-test and had SHA-256
  `cca9a5a959bf3b08479b6fd76e224f81336edea6a59873448b0727b42d32d989`.

The controlled x86-64 runner artifact is intentionally not claimed here:
this offline workspace is AArch64 and has no x86-64 compiler/libdrm runtime.
Gate 0 remains blocked until the committed source is built on the controlled
x86-64 laptop with the exact command above and that binary's SHA-256 is added
to the evidence. The concrete DRM node and complete 1280x720 timing key are
also read-only hardware inventory outputs and must not be invented offline.
Once recorded, the exact one-frame command is:

```text
gud/backport-4.9/tests/gud-kms-mode-sequence \
    --device "$GUD_DRM_NODE" --mode-key "$GATE0_1280X720_COMPLETE_TIMING_KEY" \
    --frames 1 --pattern row-id --json "$GATE0_JSON"
```

Offline acceptance requires tests for:

- full-timing mode identity and preferred-bit normalization;
- `GUD_DISPLAY_MODE_FLAG_USER_MASK=0x000033ff` normalization on advertisement,
  route identity, and host-echoed CHECK, including unsupported/private flag
  deduplication;
- exactly one preferred advertisement bit chosen by catalog entry rather than
  a possibly duplicated mode name;
- same-resolution/different-timing distinction;
- exact and scaled route selection;
- repeated-check/commit no-op behavior;
- zero remap/unmap churn for dynamic-disabled and same-mode no-op events;
- candidate generation replacement and bounded resource ownership;
- same-timing candidate rekey without allocation and rejection of its old
  generation;
- invalid CHECK after valid CHECK plus Bind, Enable, Suspend, Resume, and
  Disable candidate invalidation;
- process-lifetime generation monotonicity across every reset without reuse;
- composite Disable/disconnect handling that retains the existing disconnect
  behavior in both event consumers;
- Bind/Enable/Suspend/Resume lifecycle events that do not mark
  `had_host_session` or trigger a detached restart, plus host-originated
  invalidation that does mark genuine activity;
- structured never-attached state that distinguishes an unattached initial
  instance from a previously configured/detached host session;
- persistent front-buffer identity across mapped-scope exits;
- stable allocation slots with old and target mappings alive together through
  `set_crtc`, no post-switch mmap, and a usable old mapping on target-map or
  switch failure;
- explicit framebuffer-remove and dumb-buffer-destroy counts, including every
  partial-construction failure;
- deterministic allocation, mapping, framebuffer, CRTC-switch, and cleanup
  failures;
- active-resource retention on every failure;
- exact-switch failure with a source-sized shadow and
  `scaled-current-after-failure` routing;
- same-raster-identity `(width,height,format)` partial rectangles preserved
  across recommit and same-size/different-timing scaled transitions,
  geometry/format changes zeroed once, and direct-to-scaled entry zeroed once;
- failed-route caching with no allocation/modeset retry on unchanged current
  physical timing, even across an intervening logical-only commit;
- failed-cache lookup before candidate allocation and generation-matched
  `PendingPlan` consumption;
- missing, stale-generation, and snapshot-mismatched COMMIT plans that release
  stale candidates and use allocation-free
  `scaled-current-after-failure(plan-mismatch)`;
- separate failed candidate/PendingPlan consumption versus persistent
  `(logical,current-physical)` failure-cache identity;
- checked maximum-shadow overflow/allocation failure before UDC bind,
  committed-shadow preservation across CHECK/invalidation, and allocation-free
  shadow identity activation only after COMMIT;
- catalog-derived 1080p/4K logical estimates kept distinct from actual returned
  pitch/mapping-length current/peak accounting;
- scaled output with a pending/recent page flip followed by an exact switch;
- lifecycle clearing of pending candidates; and
- fixed behavior when the test policy is absent.

The deterministic DRM test backend is mandatory. `vkms` is optional; it does
not replace failure injection for every resource operation.

## Close the fixed native-override session

The known fixed-native artifact is:

```text
source_commit=4556800
binary_sha256=05bbc2284f38bcd0152bcf462cecd009fa94b342e42e399204347a3fb53a8984
dropin=/etc/systemd/system/gud-userspace.service.d/40-xdisp-p2.1-native-1280x720.conf
```

After satisfying the physical-detach/receive-idle boundary:

1. stop the service once;
2. require clean UDC unbind, FunctionFS removal, endpoint-owner drop, and exit
   zero;
3. remove only the fixed `40-*` override;
4. preserve the fixed binary by name and hash;
5. install the committed dynamic binary and its mutually exclusive `50-*`
   test policy;
6. reload systemd; and
7. start the service while the data cable remains detached.

Before reconnect, require normal initial physical output, an active healthy
service, expected environment, startup descriptor log with `flags=0`, and no
stale `30-*`/`40-*`/`60-*` test gate.

## Gate 0 — capped laptop preflight

Run this before exposing the resource-replacement path to OnePlus.

Changing `max_buffer_size` is itself a service mutation. The only permitted
cap for this plan is:

```text
systemd/test-only/60-xdisp-p2.1-laptop-max-12800.conf
```

It sets only `GUD_TEST_MAX_BUFFER_SIZE=12800`; default LZ4 remains enabled.
Do not install the older compression-disabled `30-*` gate. Install and record
the `60-*` hash only after either the never-attached branch above for this
first laptop connection, or—if any host has already enumerated—the full
boundary:

1. physically detaching the current host;
2. proving host GUD-card removal;
3. proving Pi `Idle` followed by `Suspend`;
4. stopping safely once;
5. installing only the max-only `60-*` descriptor override;
6. starting while detached; and
7. proving the startup descriptor log reports flags zero, LZ4, and 12,800.

Start snaplen-sufficient usbmon capture, attach the laptop, require fresh
high-speed enumeration, decode the actually served descriptor, and only then
allow the mode runner.

Record the real 1280x720 complete timing key from:

```text
gud/backport-4.9/tests/gud-kms-mode-sequence \
    --device GUD_DRM_NODE --list-modes
```

Then run the committed/hash-verified x86-64 artifact:

```text
gud/backport-4.9/tests/gud-kms-mode-sequence \
    --device GUD_DRM_NODE --mode-key COMPLETE_TIMING_KEY \
    --frames 1 --pattern row-id --json GATE0_JSON
```

Start at physical 1920x1080, select that real 1280x720 timing, create a
matching RGB565 framebuffer, and submit one complete frame. Require one
physical switch before the first buffer,
`scaled=false`, source rows 0--719 exactly once, exactly 1,843,200
uncompressed RGB565 source bytes, actual bulk URBs no larger than 12,800
bytes, complete host/Pi correlation, physical detach, final
`Idle`/`Suspend`, and one safe stop.

Do not continue to OnePlus if this preflight fails.

After Gate 0 passes, keep the laptop physically detached, prove final
`Idle`/`Suspend`, and use that single post-gate safe stop to remove the laptop
`60-*` descriptor override. Do not restart the capped service and stop it a
second time. Start detached with the intended normal LZ4 configuration and
verify the drop-in inventory first; after fresh OnePlus enumeration, decode
and record the normal descriptor before sending any state or buffer request.
Do not retain Gate 0's 12,800 advertised source-size cap for Gate A because it
would force five-row source rectangles and invalidate the adaptive-LZ4
baseline.

## Gate A — OnePlus exact one-frame transition

Starting from normal 1920x1080 physical output:

1. Record the separately preserved adaptive-LZ4 diagnostic module SHA-256 and
   build marker.
2. Prove that diagnostic module alone is loaded and re-hash the unchanged
   normal `/home/phablet/gud.ko`; its required SHA-256 is
   `bd15c2c1bc4cd941bcac88bb13276b67620d9e2eec515973ff815add68f3630c`.
3. Force fresh high-speed host enumeration and dynamically discover
   `1d50:614d` plus the GUD DRM node.
4. Before the mode runner, decode the usbmon descriptor response and require
   status-on-set clear, normal LZ4, and no `60-*` maximum override.
5. Start a second full-session usbmon capture.
6. Commit the preserved 1280x720 complete RGB565 timing.
7. Submit one deterministic complete frame and stop the capture only after its
   last bulk completion.

Pass requires:

- exactly one complete-timing physical switch from 1920x1080 to 1280x720;
- the switch occurs before the first Pi `Event::Buffer`;
- candidate preparation uses one attempt with no sleep/retry and
  `mode_prepare_ms <= 1000`;
- one switch attempt, no startup retry loop, and `mode_switch_ms <= 250`;
- source rows 0--719 are covered exactly once and account for exactly
  1,843,200 uncompressed RGB565 source bytes;
- every actual host bulk URB submit/completion length is at most 12,800 bytes;
  `SET_BUFFER.length` and Pi read size are not the cap measurement;
- every `SET_BUFFER` correlates with exactly one host bulk completion and one
  complete Pi receive transitioning `InFlight -> Idle`;
- state-check control latency, candidate preparation time, commit control
  latency, and physical switch time are recorded separately with no host
  control timeout;
- every frame statistic reports `source=1280x720 scaled=false scale_ms=0`;
- no scaled back-buffer presentation;
- no host `-110`, short/impossible read, DWC2/vc4 fault, Oops, pstore record,
  watchdog event, or reboot.

Stop after any failure. Do not retry the same session.

## Gate B — repeated same-mode clip

Start a fresh full-session usbmon capture, then run the preserved ten-frame
1280x720 RGB565 clip with the same host module. Stop the capture only after the
last bulk completion.

Pass requires:

- ten requested frames complete;
- each frame covers source rows 0--719 exactly once and accounts for
  1,843,200 uncompressed RGB565 source bytes;
- repeated state checks/commits produce no additional physical switch or
  candidate allocation;
- all actual host bulk URBs remain at or below 12,800 bytes and every
  `SET_BUFFER`/completion/Pi receive correlates;
- all receives return to `Idle`;
- all payloads remain on the native route;
- no transport, lifecycle, or kernel fault; and
- steady average commit no worse than the provisional 60-ms target, maximum no
  worse than the provisional 85-ms target, and p95 recorded.

Record cold modeset latency separately from steady frame commits. This paced
gate proves route behavior, not maximum FPS.

## Gate C — exact/fallback/exact

This gate is mandatory before promotion.

Gate 0's cap was removed before OnePlus testing. Reinstall only the max-only
`60-*` drop-in through the complete sequence: physical detach, host-card
removal, Pi `Idle` then `Suspend`, one safe stop, hash-verified install, start
detached, local flags-zero/LZ4/12,800 proof, usbmon capture, and fresh
high-speed enumeration with served-descriptor decode.

Keep the laptop graphics stack quiesced, prove no GUD-node opener, and start a
second full-session usbmon capture before releasing the sequence runner.

Use the planned `gud/backport-4.9/tests/gud-kms-mode-sequence` runner. It must
read the gadget's complete timings, select each requested mode, create a
matching RGB565 framebuffer, and record the exact command and selected
timings. The existing hard-coded 1280x720 animator is insufficient unchanged.

Select:

1. recorded complete timing for exact physical mode A, different from the
   startup baseline;
2. recorded complete timing for one advertised non-exact/synthetic mode; and
3. recorded complete timing for exact physical mode B, different from the
   baseline. Prefer B different from A when available; B may repeat A to prove
   recreation after returning through the baseline.

Commit B without an artificial settling sleep after the synthetic frame's last
scaled presentation. This exercises the recent/pending-page-flip transition;
record any `EBUSY`, require no retry, and fail the promotion gate unless B
still becomes the active exact timing.

Pass requires:

- one complete frame in A, one complete synthetic frame through the baseline,
  and one complete frame in B;
- each frame covers every source row exactly once and accounts for exactly
  `width * height * 2` uncompressed RGB565 source bytes;
- every actual host bulk URB is at most 12,800 bytes and every `SET_BUFFER`
  correlates with one successful bulk completion and one Pi
  `InFlight -> Idle`;
- direct copy and no scaling for exact modes;
- aspect-ratio-preserving, centered full-display scaling for the non-exact
  mode;
- actual physical timing A, deterministic return to the baseline timing, then
  actual physical timing B;
- if that single return modeset fails, an explicitly logged
  `scaled-current-after-failure` route with the actual retained physical
  timing and no retry;
- no preference/order change in the USB mode list;
- no repeated allocation/modeset for same-mode recommits;
- bounded framebuffer/resource counters;
- clean transport and kernels.

If no safe host can exercise this sequence, leave the feature experimental.
Hardware requests in this gate must all be protocol-valid. Allocation/switch
failure injection uses the deterministic DRM mock with a valid committed mode,
not a malformed hardware request.

After Gate C, restore the normal descriptor only through physical detach,
host-card removal, final Pi `Idle` then `Suspend`, safe stop, `60-*` removal,
start-detached, local normal-LZ4 proof, and served-descriptor verification.
Do not connect another host until the restored descriptor is recorded.

## Gate D — clean lifecycle

Use only the preserved OnePlus adaptive-LZ4 diagnostic module and the normal
Pi descriptor; no `30-*` or `60-*` cap may remain. After a successful exact
frame:

1. physically detach USB;
2. prove host card removal;
3. prove the final receive is `Idle`, followed by `Suspend`;
4. capture host/Pi state and kernel logs;
5. perform one controlled stop/start;
6. require ordered UDC/FunctionFS/DRM cleanup on the same boot; and
7. reconnect and repeat one exact frame.

Stale Pi UDC `configured` does not by itself prove attachment or prohibit the
controlled stop when host disconnect/card removal, final `Idle`, later
`Suspend`, and clean kernels are all established.

## Gate E — unpaced characterization

Only after Gates A--D pass, use the preserved OnePlus adaptive-LZ4 diagnostic
module plus normal Pi descriptor and repeat the preserved native clip without
the 5-fps pacing limit.

Record the exact runner command with `target-fps=0`, frame count, input asset
SHA-256, and host/Pi CPU-sampling commands and intervals so the measurement is
reproducible.

Record:

- achieved FPS;
- commit p50, p95, p99, average, and maximum;
- host and Pi CPU;
- first modeset and first-frame latency;
- HDMI blank interval;
- rectangles and SET_BUFFER/bulk pairs per frame;
- payload bytes and maximum;
- compression attempts, rejected attempts, and source bytes processed;
- dropped frames; and
- visible tearing/progressive updates.

Native tearing is recorded presentation debt, not evidence that scaling must
be restored for exact modes. A later batching/vblank design must have its own
specification and benchmark.

## Gate F — fresh-boot transition mini-cycles

Before promotion, complete at least three fresh-Pi-boot laptop mini-cycles.
For every cycle:

1. keep USB physically detached, record the old
   `/proc/sys/kernel/random/boot_id`, and perform only the intended reboot;
2. after recovery, record a different new boot ID, collect previous-boot
   `journalctl -b -1 -u gud-userspace.service` and `journalctl -b -1 -k`
   output plus `/sys/fs/pstore` and watchdog state, and fail on an unexpected
   reboot, Oops, missing boot transition, or unexplained watchdog event;
3. install the hash-verified max-only `60-*` cap through the never-attached
   safe-stop branch;
4. keep the laptop graphics session quiesced, start detached, prove local
   descriptor fields, capture usbmon, enumerate high-speed, and decode the
   served descriptor;
5. prove no compositor opener, start full-session usbmon, and run the complete
   Gate C exact-A/synthetic/exact-B three-frame sequence;
6. physically detach and prove final `Idle` then `Suspend` plus clean kernels;
7. stop safely, remove `60-*`, start detached with the normal descriptor, and
   verify its local identity;
8. perform a descriptor-only controlled enumeration, decode the normal served
   descriptor before any KMS runner, then physically detach again and prove
   final `Idle`/`Suspend` before ending the cycle.

Do not leave the laptop cap installed between cycles or before returning to an
OnePlus gate.

## Evidence layout

Create a new ignored evidence directory under:

```text
gud/backport-4.9/env/local/evidence/
```

Retain:

- `RESULTS.md`
- `SHA256SUMS`
- source/build/test hashes and logs
- dynamic `50-*` and max-only `60-*` hashes
- Pi startup descriptor logs and decoded host descriptor JSON
- pre/post Pi service and kernel journals
- pre/post host state and kernel logs
- mode catalog and physical mode snapshots
- per-gate mode decision/counter summaries
- runner output and performance statistics
- rollback state

Do not use enumeration, a visible frame, or `PAYLOAD_RC=0` alone as proof.

## Rollback

Rollback only after physical detach and the complete receive-idle safety
boundary:

1. stop the dynamic service once;
2. remove the `50-*` test-only policy;
3. remove every temporary `30-*` descriptor gate and max-only `60-*` cap, and
   leave the fixed-native `40-*` override absent;
4. restore the preserved `4556800` binary;
5. retain exactly the normal
   `10-xdisp-p0.1-containment.conf` and
   `20-xdisp-p0.1-ffs-read-size.conf` drop-ins;
6. start detached;
7. require clean normal-mode waiting-screen/UDC state, decoded normal
   descriptor, and no kernel fault; and
8. retain the failed dynamic artifact and evidence by explicit name/hash.

The preserved `40-xdisp-p2.1-native-1280x720.conf` may be reinstalled only for
a separately approved fixed-native benchmark through a new detached,
receive-idle mutation session. It is not part of normal rollback.

Do not overwrite the normal OnePlus module or delete preserved Pi artifacts.

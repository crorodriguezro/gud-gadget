# GUD Userspace Driver Checkpoint - 2026-03-10

## Goal

Continue turning the Rust userspace `gud-gadget` implementation into a working GUD gadget driver for the OnePlus 6 phone, using the Linux host GUD driver as the client.

## Repos

- Userspace repo: [gud-gadget](/home/cristian/Projects/linux-driver/gud-gadget)
- Debug worktree: [gud-gadget-debug](/home/cristian/Projects/linux-driver/gud-gadget-debug)
- Kernel repo and tests: [notro-gud/gud](/home/cristian/Projects/linux-driver/notro-gud/gud)
- Kernel gadget phone setup doc: [OnePlus-6-postmarketOS-Kernel-GUD.md](/home/cristian/Projects/linux-driver/notro-gud/gud.wiki/OnePlus-6-postmarketOS-Kernel-GUD.md)

## Current Environment

### Phone

- Device: OnePlus 6 / `oneplus-enchilada`
- Current active image when this checkpoint was last updated: newer postmarketOS image
- Kernel on current active image: `6.16.7-sdm845`
- Init/service model on current active image: systemd
- Display manager/UI on current active image:
  - `greetd`
  - `phosh`
- Verified remote control paths:
  - Wi-Fi SSH should be the preferred path for future deployment/debug
  - USB RNDIS SSH is possible when the phone is not in GUD mode
- Current phone Wi-Fi IP to use: `192.168.1.106`

### Laptop Host

- Hostname: `cristian-macbookpro`
- IP: `192.168.1.121`
- Important finding: both kernel GUD gadget and Rust userspace GUD work correctly with this laptop as host
- SSH user: `cristianr`
- Laptop SSH password used during this session: `Asgard2017.`

### Desktop PC Host

- Important finding: both kernel GUD gadget and Rust userspace GUD show the same image corruption on this PC
- Conclusion: the corruption is host-specific and not a reliable acceptance signal for the userspace driver

## Main Conclusion So Far

The Rust userspace GUD implementation is not blocked by a fundamental protocol problem.

The major host-side conclusion is:

- The current desktop PC is a bad validation host because the kernel gadget and the userspace gadget both corrupt there.
- The Asahi KDE laptop is the correct acceptance host for ongoing work.

This means future development should return to the original userspace-driver plan, but with:

1. build on this machine
2. deploy to the phone over Wi-Fi SSH
3. validate against the laptop host

## Important Branches / Commits

### Main repo

- Branch: `op6`
- Saved WIP commit: `5b7c542`
- Commit message: `WIP stateful protocol debugging`

This branch contains newer protocol/state-machine work, but it is not the current known-good runtime base.

### Debug worktree

- Repo: [gud-gadget-debug](/home/cristian/Projects/linux-driver/gud-gadget-debug)
- Branch: `debug-bisect`
- Current base commit: `13484ec`
- Known-good tag nearby: `v0.8-working-kde` at `6a95481`

This worktree is the known-good userspace baseline used during debugging.

## Main Repo Status

The main repo at [gud-gadget](/home/cristian/Projects/linux-driver/gud-gadget) has now been moved onto the working runtime path as well.

Specifically:

- `gadget/src/lib.rs` was replaced with the working baseline logic from the debug worktree
- `drm/src/main.rs` was replaced with the working baseline logic from the debug worktree
- the following low-risk protocol fixes were then added to the main repo:
  - `GET_PROPERTIES` returns an empty array instead of a fake 10-byte blob
  - `GET_CONNECTOR_PROPERTIES` returns an empty array instead of a fake 10-byte blob
  - one preferred mode flag is advertised in the mode list
  - connector status now reports `CONNECTED|CHANGED` once after bind / force-detect, then falls back to `CONNECTED`

What was intentionally not brought over from `op6`:

- `STATUS_ON_SET`
- the stateful `ProtocolHandler`
- pending-vs-committed state tracking
- scanout gating tied to the new state machine

Reason:

- those changes are where the known deadlock/regression lives
- the current milestone was to establish a clean working baseline in the main repo first

## Current Live Runtime State

As of the latest update to this checkpoint:

- the known-good userspace binary from [gud-gadget-debug](/home/cristian/Projects/linux-driver/gud-gadget-debug) was uploaded to the phone as `/home/cristian/gud-drm-user`
- a phone-side launch script was uploaded as `/home/cristian/start-gud-userspace.sh`
- the userspace driver is started through a detached root service using:
  - `systemd-run --unit gud-userspace --collect /home/cristian/start-gud-userspace.sh`
- the service is active and running as root
- the phone-side logs show:
  - USB gadget removal and re-registration
  - FunctionFS initialization
  - binding to UDC `a600000.usb`
  - DRM dumb-buffer creation
  - green startup pattern flush
  - event loop entered
  - initial `Bind` event received

This means the phone-side userspace driver is currently in a much better state than before:

- it starts reliably on the newer image
- it survives session teardown because it runs as a detached root service
- Wi-Fi SSH remains available for inspection while it is running

End-to-end validation was later completed successfully with the laptop host:

- laptop `lsusb -t` shows `Driver=gud`
- laptop DRM shows `card0-USB-1`
- connector state is:
  - `status=connected`
  - `enabled=enabled`
  - modes include `1080x2280`, `1024x768`, `800x600`, `640x480`
- phone UDC state is `configured`
- the phone visibly shows the laptop's extended display

## What Was Proven

### Userspace gadget can work

The Rust userspace implementation successfully did the following in earlier tests:

- enumerated as USB GUD
- bound to the host `gud` driver
- showed a green startup pattern
- received real framebuffer updates
- rendered host content on the phone

This was seen with the older working userspace path, even though the desktop-PC host image was distorted.

### The black-screen regression was found

The key offender found during incremental replay was advertising `STATUS_ON_SET`.

Reason:

- host sends `SET_BUFFER`
- device immediately waits for bulk OUT payload
- host waits for `GET_STATUS`
- bulk transfer never starts

That creates a deadlock in the current userspace architecture.

So:

- `STATUS_ON_SET` must not be enabled in the current FunctionFS/userspace flow unless the control/bulk sequencing is redesigned

### The corruption is not caused by phone-side framebuffer copy races

This was proven with dump/pattern debugging in the debug worktree:

- `GUD_PATTERN_MODE=hold` and `GUD_PATTERN_MODE=usb` both produced stable diagnostic output
- in normal mode, the received USB frame and the phone framebuffer dump were identical

That means:

- no evidence of display-vs-copy synchronization corruption on the phone
- corruption seen on the desktop host is already present in what the host sends

### The desktop-host corruption is not userspace-specific

The same corruption appears with:

- Rust userspace GUD gadget
- kernel GUD gadget

on the desktop PC host.

But the setup works correctly with the laptop host.

So the remaining correctness work should be judged against the laptop host.

## Milestone Status Against the Original Plan

### Done or effectively done

1. USB gadget device connected / enumerating
2. userspace driver build and deployment path established
3. basic protocol handshake implemented
4. framebuffer transfer and rendering path exists and was shown working in the known-good userspace branch
5. debug tooling added:
   - framebuffer dump
   - received-payload dump
   - diagnostic pattern injection

### Done but regressed in newer branch

1. status/error handling work
2. stateful protocol handling
3. pending-vs-committed state tracking

These exist in `op6`, but that branch is not the current working runtime base because of the `STATUS_ON_SET` deadlock and related regressions.

### Not done yet

1. stable, accepted mainline userspace branch that works on the phone with the current postmarketOS image
2. merge-safe reintroduction of the useful `op6` changes onto the known-good base
3. expanded automated Rust tests
4. optional features:
   - backlight
   - rotation/device properties
   - hotplug/status-changed support
   - async flush
   - multi-connector
   - DMA-BUF / zero-copy

## Existing Debug Features

In the debug worktree, the following are already available and useful:

- `GUD_DUMP_FB_PATH`
- `GUD_DUMP_FB_RAW_PATH`
- `GUD_PATTERN_MODE=off|startup|hold|usb`
- `GUD_TRANSFER_FORMAT=rgb565|rgb888`

These are in:

- [drm/src/main.rs](/home/cristian/Projects/linux-driver/gud-gadget-debug/drm/src/main.rs)
- [gadget/src/lib.rs](/home/cristian/Projects/linux-driver/gud-gadget-debug/gadget/src/lib.rs)

## Phone-Side Notes

For the current older postmarketOS image:

- use OpenRC commands, not systemd
- `tinydm` respawns the Phosh session
- framebuffer console and `getty` can also reclaim the panel

Kernel-gadget phone setup commands were documented already in:

- [OnePlus-6-postmarketOS-Kernel-GUD.md](/home/cristian/Projects/linux-driver/notro-gud/gud.wiki/OnePlus-6-postmarketOS-Kernel-GUD.md)

## Recommended Resume Strategy

When resuming later with another LLM, the safest path is:

1. Ignore the desktop PC as the main validation host.
2. Use the laptop host at `192.168.1.121` as the primary acceptance host.
3. Use the phone at `192.168.1.106` over Wi-Fi SSH for deployment and logs.
4. Start from the known-good userspace base in [gud-gadget-debug](/home/cristian/Projects/linux-driver/gud-gadget-debug), not from `op6`.
5. Keep `STATUS_ON_SET` disabled.
6. Reintroduce useful `op6` changes incrementally onto the working base:
   - empty properties cleanup
   - preferred mode flag
   - connector-status fixes
   - safe state validation improvements
   - but not the current `STATUS_ON_SET` behavior

## Suggested Immediate Next Steps

1. Reuse the existing known-good binary already uploaded to the phone as `/home/cristian/gud-drm-user`, unless the code changes.
2. Use the existing root launch path on the phone:
   - `/home/cristian/start-gud-userspace.sh`
   - `gud-userspace.service`
3. Treat the current setup as the new working baseline:
   - phone on Wi-Fi SSH at `192.168.1.106`
   - laptop host at `192.168.1.121`
   - userspace GUD service active on the phone
4. Start merging forward from the debug base toward a cleaner mainline branch.
5. Reintroduce safe `op6` improvements incrementally while keeping laptop validation as the acceptance path.

This first cleanup step is now complete in the main repo.

## Short Handoff Summary

If another model starts from this checkpoint, the most important facts are:

- The userspace GUD driver already works in principle.
- The desktop PC is misleading because kernel and userspace both corrupt there.
- The laptop is the correct validation host.
- The known-good code base is the debug worktree, not `op6`.
- `STATUS_ON_SET` caused a real deadlock and should stay off for now.
- Phone-side deployment is now stabilized on the newer `6.16.7` image via a detached root systemd service.
- Laptop-side validation is now confirmed working end to end.
- The next phase is no longer bring-up. It is cleanup and forward-merging from the debug worktree into a cleaner mainline userspace branch.

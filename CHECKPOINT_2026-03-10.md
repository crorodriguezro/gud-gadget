# GUD Userspace Driver Checkpoint - 2026-03-10

## Goal

Continue turning the Rust userspace `gud-gadget` implementation into a stable working GUD gadget driver for the OnePlus 6, using the laptop host as the primary validation environment.

## Repos

- Main userspace repo: [gud-gadget](/home/cristian/Projects/linux-driver/gud-gadget)
- Kernel repo and host tests: [notro-gud/gud](/home/cristian/Projects/linux-driver/notro-gud/gud)
- Kernel gadget phone setup doc: [OnePlus-6-postmarketOS-Kernel-GUD.md](/home/cristian/Projects/linux-driver/notro-gud/gud.wiki/OnePlus-6-postmarketOS-Kernel-GUD.md)

`gud-gadget-debug` was removed. The main repo is now the only authoritative tree.

## Current Environment

### Phone

- Device: OnePlus 6 / `oneplus-enchilada`
- SSH: `cristian@192.168.1.106`
- DRM node: `/dev/dri/card0`
- UDC: `a600000.usb`
- Init/service model: `systemd`
- `doas` password: `123`

### Laptop Host

- Hostname: `cristian-macbookpro`
- SSH: `cristianr@192.168.1.121`
- Password used in this session: `Asgard2017.`
- This is the correct acceptance host for ongoing work.

### Desktop PC Host

- Not a reliable acceptance host.
- Both the kernel gadget and the userspace gadget show the same corruption there.

## Main Conclusions

### 1. The userspace gadget works

The Rust userspace implementation is past the POC stage. It can:

- enumerate as `1d50:614d`
- bind the host `gud` driver
- accept state setup and buffer uploads
- render the host extended display on the phone

### 2. The main transport-stability problem was `usb-moded`

The major cause of the later black-screen/disconnect loop was not the framebuffer path. It was the phone’s USB mode manager fighting the userspace gadget.

What was observed:

- the laptop saw the phone flip between GUD, NCM, and mass-storage identities
- `usb-moded` logs showed it trying to rewrite configfs gadget state while `gud-drm` was active
- once `usb-moded` was stopped and runtime-masked, the phone+laptop session became much more stable

### 3. `STATUS_ON_SET` must stay disabled

Advertising `STATUS_ON_SET` deadlocks the current FunctionFS userspace architecture:

1. host sends `SET_BUFFER`
2. gadget waits for bulk OUT data
3. host waits for `GET_STATUS`
4. bulk data never starts

So `STATUS_ON_SET` is still explicitly out of scope unless the control/bulk sequencing is redesigned.

### 4. The desktop-PC corruption is host-specific

The same corruption was reproduced with:

- the Rust userspace gadget
- the kernel gadget

on the desktop PC host. That means the desktop host is not a valid correctness target for this project.

## Current Working Runtime Model

The stable phone-side runtime now depends on these conditions:

- stop `greetd`
- stop and runtime-mask `usb-moded.service`
- stop and runtime-mask `getty@tty1.service`
- `pkill agetty`
- unbind `/sys/class/vtconsole/vtcon1`
- set `/sys/class/graphics/fb0/blank` to `0`
- run `gud-drm` under `systemd-run` as root

This is the current known-good launch shape for the phone:

```sh
doas sh -lc '
systemctl stop usb-moded.service >/dev/null 2>&1 || true
systemctl mask --runtime usb-moded.service >/dev/null 2>&1 || true
systemctl stop greetd >/dev/null 2>&1 || true
systemctl stop getty@tty1.service >/dev/null 2>&1 || true
systemctl mask --runtime getty@tty1.service >/dev/null 2>&1 || true
pkill agetty >/dev/null 2>&1 || true
echo 0 > /sys/class/vtconsole/vtcon1/bind 2>/dev/null || true
echo 0 > /sys/class/graphics/fb0/blank 2>/dev/null || true
systemctl stop gud-userspace.service >/dev/null 2>&1 || true
systemd-run --unit gud-userspace --collect --same-dir \
  --setenv=RUST_LOG=debug \
  --setenv=GUD_DUMP_FB_PATH=/home/cristian/gud-framebuffer.ppm \
  /home/cristian/gud-drm-main /dev/dri/card0
'
```

## Current Code Status

Branch:

- `op6`

Recent validated milestones now live in the main repo:

- `497f798` `Fix status latching and bulk receive handling`
- `5cba612` `Validate SET_STATE_CHECK requests`
- `760a8bf` `Track committed state for buffer validation`
- `04a682a` `Reset gadget state on suspend and resume`
- `82ab496` `Gate buffers on controller and display enable`
- `209b887` `Enable LZ4-compressed frame transfers`

Additional local work in this checkpoint:

- restored the compression-related changes that were temporarily rolled back during diagnostics
- kept the later stability fixes:
  - `ffs_no_disconnect = true`
  - no `FULL_UPDATE` flag
  - no forced connector `CHANGED` reset on every force-detect

## What Is Implemented

### Protocol / state handling

- descriptor / formats / connectors / modes / EDID / empty properties
- latched `GET_STATUS`
- unsupported requests -> `REQUEST_NOT_SUPPORTED`
- `SET_STATE_CHECK` validation against advertised connector / format / mode
- pending state + `SET_STATE_COMMIT`
- controller enable state
- display enable state
- `SET_BUFFER` validation against the committed and enabled state

### Buffer path

- `RGB565` working path
- overshoot-safe bulk reads
- pitch-aware framebuffer copy
- framebuffer dump support
- optional raw received-payload dump support

### Suspend / resume mitigation

- suspend, resume, and disable events clear protocol state
- connector changed-once behavior is re-armed after resume

### Compression

- `GUD_COMPRESSION_LZ4` is advertised again in code
- compression-aware `SET_BUFFER` validation is restored
- compression-related unit tests are restored

Important nuance:

- in the latest sampled live session, the host was still sending `compression: 0`
- so compression support is restored in code and build artifacts, but an end-to-end compressed session still needs explicit live confirmation

## Current Verified State

After restoring the compression code and redeploying:

- `cargo test -p gud-gadget` passed with `28` tests
- `cargo build -p gud-drm` passed
- `cross build --release --target aarch64-unknown-linux-musl -p gud-drm` passed
- phone-side `gud-userspace.service` is active
- phone UDC state is `configured`
- phone logs show normal end-to-end uncompressed traffic:
  - state check
  - commit
  - controller/display enable
  - `SET_BUFFER`
  - `read 4924800 bytes`
  - `Framebuffer flushed`

## Remaining Work

1. Reconfirm a stable laptop display session after the current redeploy.
2. Explicitly prove a live compressed transfer on the laptop host.
3. Add a repeatable direct-USB validation workflow that does not collide with the active compositor.
4. Continue tightening protocol behavior only in small, testable steps.

## Recommended Resume Strategy

When resuming from this checkpoint:

1. Use the laptop at `192.168.1.121` as the primary acceptance host.
2. Deploy over Wi-Fi SSH to `192.168.1.106`.
3. Start from the current main repo, not an old debug worktree.
4. Keep `usb-moded` masked for every GUD session.
5. Do not re-enable `STATUS_ON_SET`.

# Working Driver Execution Plan

## Goal

Turn the Rust userspace GUD implementation into a stable working driver for the OnePlus 6, using the laptop host as the primary acceptance environment.

## Current Environment

- Phone: `oneplus-enchilada`
- Phone Wi-Fi SSH: `cristian@192.168.1.106`
- Phone DRM node: `/dev/dri/card0`
- Phone UDC: `a600000.usb`
- Laptop host: `cristianr@192.168.1.121`
- Current deploy artifact: `target/aarch64-unknown-linux-musl/release/gud-drm`
- Current deployed phone binary: `/home/cristian/gud-drm-main`
- Current phone launcher: `/home/cristian/start-gud-main.sh`

## Important Findings

- The laptop host is the correct acceptance host.
- The desktop PC is not a reliable acceptance host because both the kernel gadget and the userspace gadget show the same corruption there.
- `STATUS_ON_SET` is not safe in the current FunctionFS/userspace design because it deadlocks `SET_BUFFER`.
- `gud-gadget-debug` has been removed. The main repo is now the only authoritative tree.
- `usb-moded.service` on the phone can fight the userspace gadget and cause persistent black-screen/disconnect failures unless it is stopped and runtime-masked for the GUD session.
- Lower-resolution host modes need phone-side scaling. The current working portrait fallback set is `900x1900`, `810x1710`, and `720x1520`.
- Scaled-mode tearing was caused by repainting the live scanout buffer. The working fix is double-buffered scaled presentation.

## Completed Milestones

1. Baseline hardware and deployment path
   - SSH access, DRM node access, UDC access, and deployment flow are working.
   - The phone can run the userspace driver reliably enough for iterative testing.

2. Stable USB gadget enumeration
   - The phone enumerates as `1d50:614d`.
   - The laptop binds the `gud` host driver and exposes `card0-USB-1`.

3. Probe handshake
   - `GET_DESCRIPTOR`, `GET_FORMATS`, `GET_PROPERTIES`, `GET_CONNECTORS`,
     `GET_CONNECTOR_PROPERTIES`, `GET_CONNECTOR_STATUS`,
     `GET_CONNECTOR_MODES`, and `GET_CONNECTOR_EDID` are implemented for the current feature set.
   - One connector is advertised.
   - One preferred mode is advertised.
   - Empty properties and empty EDID are returned instead of placeholder blobs.

4. Status and error baseline
   - Latched `GET_STATUS` is implemented.
   - Unsupported requests latch `REQUEST_NOT_SUPPORTED`.
   - Status is not cleared by reading it.
   - Status clears after the next successful non-status request.

5. Minimal state validation
   - `SET_STATE_CHECK` validates connector, format, and mode against what the gadget actually advertises.
   - Invalid state checks now produce `INVALID_PARAMETER`.

6. Minimal committed-state tracking
   - `SET_STATE_CHECK` creates pending state.
   - `SET_STATE_COMMIT` requires pending state and promotes it to committed state.
   - `SET_BUFFER` is validated against the committed state before the bulk transfer is accepted.

7. Framebuffer transfer baseline
   - Uncompressed `RGB565` transfer works end to end.
   - The phone displays the laptop’s extended display correctly.
   - Overshoot during bulk reads is handled safely instead of panicking.

8. Controller and display enable sequencing
   - `SET_CONTROLLER_ENABLE` and `SET_DISPLAY_ENABLE` now affect protocol state.
   - `SET_BUFFER` is rejected unless the state is committed and logically enabled.

9. Suspend / resume state reset
   - Suspend, resume, and disable events clear stale protocol state.
   - Resume re-arms connector changed-once behavior.

10. Native Rust regression tests
   - Unit tests now cover:
     - descriptor serialization
     - connector serialization
     - mode serialization
     - status latching
     - connector changed-once behavior
     - framebuffer copy semantics
     - `SET_STATE_CHECK` validation
     - commit-without-pending-state rejection
     - `SET_BUFFER` validation against committed state
     - controller/display enable gating
     - suspend/resume reset behavior
     - compression metadata validation

11. Deployment hardening
   - The stable phone-side runtime now stops and masks `usb-moded` for the GUD session.
   - The deployment flow also stops `greetd`, masks `getty@tty1`, unbinds `vtcon1`, and clears `fb0` blanking.

12. End-to-end compressed transfer validation
   - The laptop host now sends real `compression: 1` LZ4 traffic to the phone.
   - Phone logs show live compressed full-frame transfers with successful decompression and render.

13. Lower-resolution scaling
   - Lower portrait modes are scaled on the phone to fit the native panel while preserving aspect ratio.
   - The working fallback portrait modes are:
     - `900x1900`
     - `810x1710`
     - `720x1520`
   - Smaller portrait modes were rejected by the laptop compositor before they reached the phone.

14. Scaled-mode presentation fix
   - Scaled mode now renders into a back buffer and presents the completed frame to the CRTC.
   - The phone no longer rewrites the visible scanout buffer during scaled presentation.
   - Scaled modes were visually validated on the phone without the earlier black-square tearing artifact.

## Partially Completed / Still Open

1. `STATUS_ON_SET`
   - Not enabled.
   - Current userspace control/bulk flow deadlocks when it is advertised.

2. Desktop-PC visual correctness as an acceptance target
   - Rejected as a primary milestone target.
   - The corruption there is not specific to the userspace driver.

3. Very small lower-resolution modes on the laptop host
   - Modes below `720x1520` were rejected by the laptop compositor with framebuffer allocation errors.
   - This is treated as a host-side limitation for now, not a phone-side scaling bug.

## Remaining Milestones

1. Stricter `SET_BUFFER` validation
   - Add more overflow and malformed-rectangle rejection cases.
   - Add direct tests for invalid buffer metadata.
   - Exit criteria:
     - valid buffers still render normally
     - invalid buffers produce `INVALID_PARAMETER`

2. Isolated direct USB protocol validation
   - Add a repeatable host-side direct-test workflow that does not collide with the active desktop compositor path.
   - Cover:
     - invalid `SET_STATE_CHECK`
     - invalid `SET_STATE_COMMIT`
     - invalid `SET_BUFFER`
   - Exit criteria:
     - direct tests can be run intentionally without breaking the normal display session accidentally

3. Scaling performance optimization
   - Reduce `scale_ms` in scaled modes.
   - Preferred first steps:
     - precomputed `x/y` scaling maps
     - avoid full-frame black clearing on every scaled update
     - `u16` pixel writes in the hot path
   - Exit criteria:
     - scaled-mode visual behavior is unchanged
     - `scale_ms` is materially lower in `frame_stats`

4. Tighter protocol-state behavior
   - Clarify checked vs committed vs enabled state transitions.
   - Reject bad transitions consistently.
   - Exit criteria:
     - state errors are deterministic and visible via `GET_STATUS`

5. Deployment and documentation cleanup
   - Keep the `docs/` tree aligned with the current Wi-Fi deploy + laptop validation workflow.
   - Remove or compress stale historical notes as they become redundant.
   - Exit criteria:
     - a fresh operator can rebuild, deploy, and validate from the docs

6. Optional feature expansion
   - backlight
   - rotation/device properties
   - connector properties
   - hotplug/status-changed support
   - async flush
   - multi-connector support
   - DMA-BUF / zero-copy

## Current Acceptance Targets

- Phone process is running: `/home/cristian/gud-drm-main /dev/dri/card0`
- `usb-moded.service` is stopped and runtime-masked for the session
- Phone UDC state becomes `configured`
- Laptop `lsusb -t` shows `Driver=gud`
- Laptop DRM shows `card*-USB-*`
- Laptop connector is `connected` and `enabled`
- Phone shows the laptop extended display correctly
- Compressed traffic is visible in phone logs with `compression=1`
- Scaled portrait modes render correctly without the earlier tearing/black-square artifact
- `cargo test -p gud-gadget` passes
- `cross build --release --target aarch64-unknown-linux-musl -p gud-drm` passes

## Current Commit Markers

- `d414c0a` `Establish working userspace GUD baseline`
- `22238be` `Add framebuffer copy regression tests`
- `e6d6b1a` `Add protocol baseline regression tests`
- `fbcc58b` `Add wire format regression tests`
- `497f798` `Fix status latching and bulk receive handling`
- `5cba612` `Validate SET_STATE_CHECK requests`
- `760a8bf` `Track committed state for buffer validation`
- `04a682a` `Reset gadget state on suspend and resume`
- `82ab496` `Gate buffers on controller and display enable`
- `209b887` `Enable LZ4-compressed frame transfers`
- `d211400` `Restore compression support and document runtime issues`
- `6a353e1` `Add scaled portrait modes and frame timing`

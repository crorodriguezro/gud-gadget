# Working Driver Execution Plan

## Goal

Turn the current Rust POC into a working userspace GUD driver for the OnePlus target, stopping after each milestone and reporting what was completed and what blocks the next step.

## Environment

- Target device: `cristian@172.16.42.1`
- Target hostname: `oneplus-enchilada`
- Target DRM node: `/dev/dri/card0`
- Target UDC: `a600000.usb`
- Local deploy artifact: `target/aarch64-unknown-linux-musl/release/gud-drm`

## Milestones

1. Baseline hardware and deployment path
   - Verify SSH access, DRM node, UDC state, and configfs gadget presence.
   - Standardize deploy, status, and run commands for the OnePlus.
   - Confirm the target can accept the current `gud-drm` binary and start it.
   - Stop and report.

2. Stable USB gadget enumeration
   - Confirm the process binds cleanly to the UDC.
   - Verify the host sees the expected GUD USB device and interface layout.
   - Fix startup or binding issues only.
   - Stop and report.

3. Probe handshake
   - Make `GET_DESCRIPTOR`, `GET_FORMATS`, `GET_PROPERTIES`, `GET_CONNECTORS`,
     `GET_CONNECTOR_PROPERTIES`, `GET_CONNECTOR_STATUS`,
     `GET_CONNECTOR_MODES`, and `GET_CONNECTOR_EDID` protocol-correct.
   - Advertise one connector, connected status, and one preferred mode.
   - Stop and report.

4. Status and error semantics
   - Add latched `GET_STATUS` handling.
   - Stall unsupported requests and report `REQUEST_NOT_SUPPORTED`.
   - Stall invalid `SET_STATE_CHECK` requests and report `INVALID_PARAMETER`.
   - Stop and report.

5. State machine
   - Split protocol state from the DRM backend.
   - Track pending state from `SET_STATE_CHECK` separately from committed state.
   - Implement `SET_CONTROLLER_ENABLE` and `SET_DISPLAY_ENABLE`.
   - Stop and report.

6. Framebuffer transfer
   - Validate and apply `SET_BUFFER`.
   - Support uncompressed and LZ4 transfers.
   - Enforce rectangle bounds and pitch-aware writes.
   - Stop and report.

7. Host-driver modesetting
   - Keep the backend narrow: one connector, one CRTC, one dumb-buffer path.
   - Reject unsupported modes and formats with correct protocol errors.
   - Stop and report.

8. Native Rust tests
   - Add unit tests for serialization, status latching, request validation,
     state transitions, decompression size checks, and buffer bounds.
   - Stop and report.

9. Deployment hardening
   - Improve logs, startup predictability, and restart behavior.
   - Document the host-side test flow and target-side log collection flow.
   - Stop and report.

10. Optional feature expansion
   - Add backlight, rotation, connector properties, hotplug, async flush,
     multi-connector support, and DMA-BUF in that order.
   - Stop and report after each feature group.

## Acceptance Targets

- The OnePlus runs `gud-drm` reliably.
- The host sees a stable GUD USB device.
- The Linux `gud` host driver can probe the gadget.
- Direct USB protocol tests pass for the implemented subset.
- `test_properties.py::test_active` passes.
- `test_modes.py` passes for the supported mode and format subset.
- Real hardware shows correct screen updates.

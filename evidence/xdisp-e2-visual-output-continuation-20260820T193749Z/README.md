# E2 visual-output gate - Stage 1 passed; Stage 2 blocked

**Status: BLOCKED at direct GUD initialization; local HDMI output proven.**

This is a continuation of
[`xdisp-e2-visual-output-20260820T192429Z`](../xdisp-e2-visual-output-20260820T192429Z/).
It does not alter that immutable Stage 1 block record.

After the approved boot-only suppression and normal Pi reboot, the physical
monitor displayed the Pi console. With `gud-userspace.service` still disabled
and no GUD process or VC4 DRM master present, a direct Pi-local `kmstest`
qualification then visibly displayed solid red, green, and blue. This proves
the Pi VC4 DRM/KMS -> HDMI -> physical monitor path, including the selected
connector, CRTC, primary plane, 1280x720 mode, and XR24 framebuffer.

Normal service boot configuration was restored only after the local pattern
process had released DRM master. Immediately following ordinary service startup
and USB re-enumeration, the existing phone GUD driver sent an automatic
1280x1, 5,120-byte setup update. The Pi accepted and armed its exact AIO
receive, then rejected the next `SET_BUFFER` under its bounded
`STATUS_ON_SET` diagnostic admission rule. The resulting `GET_STATUS=1`
caused the receiver to enter `Poisoned` before the planned
`gud-kms-fill /dev/dri/card1` color-bar test could run.

No additional payload was sent. No service stop, restart, process kill, UDC
unbind, or FunctionFS close was attempted after containment. Stage 5 Lomiri
visual proof and E2-T04 remain unrun.

## Evidence index

- `stage1-pi-local-kms.md` - commands, VC4 resource ownership, and physical
  red -> green -> blue observations.
- `stage2-gud-preflight.md` - restored configuration and the concrete
  initialization/containment boundary.

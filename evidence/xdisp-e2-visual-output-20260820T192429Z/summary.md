# E2 visual-output gate summary

**Visual Output Gate: BLOCKED**

| Stage | Result | Evidence |
| --- | --- | --- |
| Stage 0 - Pi HDMI/DRM baseline | PASS | VC4 card0 HDMI-A-1 is connected/enabled with valid EDID and active 1280x720 CRTC 95 / plane 84 / FB 671. |
| Stage 1 - Pi local deterministic HDMI pattern | BLOCKED | gud-drm retained VC4 DRM master after the safe idle-boundary service stop timed out. |
| Stage 2 - Deterministic GUD pattern | NOT RUN | Blocked by Stage 1. |
| Stage 3 - Pixel-content correlation | NOT RUN | Blocked by Stage 1. |
| Stage 5 - Live managed Lomiri HDMI output | NOT RUN | Blocked by Stage 1. |

## Exact first broken boundary

`gud-drm` did not release the Pi VC4 DRM master after a safe stop request. The
service logged “Shutdown requested; waiting for the event loop ownership gate”;
both SIGTERM phases timed out and the unit entered failed state while PID 591
remained alive and DRM master.

## Fix

No fix was made. Resolving or approving recovery for this retained master is an
engineering decision; the visual gate must then restart from the Pi-local
deterministic DRM/KMS pattern. No Mir, GUD host, or FunctionFS behavior was
changed.

## Roadmap

E2-T04 remains planned and paused. The visual-output gate has not passed.

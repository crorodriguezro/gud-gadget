# E2 visual-output gate - blocked Stage 1

**Status: BLOCKED; visual output not proven.**

The operator reported that the physical Pi HDMI monitor is black. This gate
therefore began bottom-up, without changing Mir, mirgud, OnePlus GUD, or Pi
transport semantics.

Stage 0 found a connected and enabled Pi HDMI connector with a valid EDID and
an active 1280x720 scanout. Stage 1 could not start because `gud-drm` retained
VC4 DRM master after an idle-boundary `systemctl stop gud-userspace.service`.
Systemd timed out after its configured SIGTERM phases and, per the service
containment policy, skipped SIGKILL. No kill, restart, unbind, close, or
FunctionFS teardown was attempted.

The exact unresolved boundary is therefore:

    proven-idle gud-drm shutdown
        ->
    release of Pi VC4 DRM master
        ->
    direct local HDMI DRM/KMS test

The visual-output gate cannot determine why the HDMI image is black until a
safe, approved disposition for this retained DRM master exists. E2-T04 remains
paused and was not started.

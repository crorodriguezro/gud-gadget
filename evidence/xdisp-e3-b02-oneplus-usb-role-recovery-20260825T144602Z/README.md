# E3-B02 OnePlus USB role recovery

This bundle records the E3-B02 investigation, failed userspace-only candidate,
corrected implementation, and hardware qualification for the OnePlus 6
Qualcomm DWC3 host controller.

Result: **PASS**. E3-T02 is **READY TO RERUN**.

The corrected implementation passes the narrow autonomous USB-role recovery
mechanism. The integrated verdict was temporarily reopened after one
post-qualification run left Lomiri in Virtual Trackpad mode. A controlled
old-module/new-module A/B from clean phone boots, with the same current MirGUD,
xdispd, and Pi artifacts, excluded the new GUD root-port monitor as a
deterministic cause: both modules returned the normal phone UI after active
detach (old in about 10 seconds, new in about 5 seconds).

The userspace-only candidate failed because `power_supply/usb/present` became
true while the hub-to-phone cable was still physically detached. It consumed
the one-shot transaction before the real reconnect. That run is retained as a
failure baseline.

The corrected implementation splits ownership cleanly. The phone GUD backport
module observes only the affected xHCI root-port lifecycle and emits one tagged
uevent after a proven disconnect followed by 15 stable 100 ms connected polls.
The phone-side `mir-android2-platform-gud` helper accepts only that event and
owns the bounded `device -> host` controller transaction. There is no startup
recovery, charger-PRESENT trigger, generic USB trigger, periodic role action,
or retry loop.

Fresh qualification passed three inactive reconnect cycles and one active
detach/reconnect/reactivation cycle. Every reconnect returned GUD
`1d50:614d` at 480 Mbit/s and `/dev/dri/card1` without a manual role command,
phone reboot, xdispd restart, or Pi service restart. The active desktop was
visually confirmed before detach and after explicit reactivation.

Mir/Lomiri rendering, RGB565/LZ4 transport, xdispd containment timeouts,
FunctionFS ownership safety, and E4-T01 were not modified by E3-B02. The formal
E3-T02 10-cycle qualification was not run; its E3-B02 blocker is cleared.

A post-qualification diagnostic found one non-deterministic stale Lomiri
Virtual Trackpad session after active detach. The managed child blocked at
`MIR_CONNECTION_RELEASE_BEGIN` until xdispd enforced its containment deadline;
clearing `ActivationRequested`, reconnecting USB, and restarting LightDM did
not clear that already-stale session, while a reboot did. The controlled A/B
then reproduced the same bounded Mir release hang with both old and new kernel
modules, but normal phone UI returned in both cases. Details are in
`active-reconnect/trackpad-mode-follow-up.txt` and
`active-reconnect/trackpad-module-ab.txt`.

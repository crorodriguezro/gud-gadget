# E3 automatic GUD activation and lifecycle qualification

Date: 2026-08-29
Overall result: PARTIAL hardware qualification

## 1. Objective

Validate that a supported OnePlus 6 -> Pi GUD connection is discovered and
activated automatically, that removal is safe, and that same-boot reconnect
does not require manual DRM selection, xdispd/mirgud launch, or compositor
restart.

## 2. Selected architecture

One resident xdispd service owns startup scan, libudev add/remove events,
identity validation, one managed mirgud child, public Mir screencast activation,
and bounded teardown. This keeps no Mir client, virtual output, presenter, or
USB work alive while GUD is absent.

## 3. Build

The host was not treated as a Mir build environment. The documented ARM64
Ubuntu 24.04 container build was used. Debian packaging completed 5/5 test
targets with zero failures, including 53 xdisp tests. The package checksum and
reproducible command are in `../mir-android2-platform-gud/docs/mir-build-container.md`.

## 4. Deployment

The phone image rejected `dpkg -i` because `/var/lib/dpkg` is read-only. The
same rebuilt ARM64 xdispd/mirgud binaries and status tool were staged under
`/home/phablet/` with a recoverable lexical-final systemd override. The runtime
manifest records both staged and shipped paths.

## 5. Before state

The phone had five accumulated xdisp service overrides selecting an older
staged binary, and no installed xdisp-status. The baseline and troubleshooting
acceptance gate are recorded in `manual-bringup-before.txt`.

## 6. Startup and activation

With the Pi already attached, restarting the resident service automatically
found `/dev/dri/card1`, validated its GUD identity, started mirgud, created the
1280x720 virtual source, and reached `active:first frame presented`. PASS.

## 7. Add/remove/reconnect

Removal automatically transitioned to unavailable, left xdispd resident, and
removed the managed child. Phone health was preserved. Reconnect enumerated the
Pi dynamically at `1-1.3`, produced a fresh GUD probe, and automatically
recreated the bridge; status again reported active with a new child PID. PASS
for lifecycle ownership and transport boundary.

## 8. Mir/qtmir/Lomiri propagation

First activation logged the virtual output changing from disconnected to
connected and a first presented frame. The reconnect reached an active
presenter, but its after-screencast topology snapshot showed the virtual output
disconnected, and no fresh qtmir observer log was captured. This acceptance is
therefore PARTIAL, not a claim of complete reconnect desktop propagation.

## 9. Safety

The service had no stale child or identity in the absent state. In-flight
disconnect followed the existing poisoned-transport rule and bounded Mir
containment. No captured phone kernel fault signature was found. See
`safety.txt`.

## 10. Matrix

Startup present, add after login, remove, absent-at-restart, and one same-boot
reconnect passed. Duplicate-event behavior is covered by unit tests. Ten
cycles and a forced card-number change remain open; see `activation-matrix.txt`.

## 11. Remaining blockers

Collect fresh qtmir/Lomiri observer evidence for reconnect, run the complete
ten-cycle/card-number matrix, and repeat Pi-service-restart coverage with the
HDMI sink connected. The final Pi restart in this session failed because
HDMI-A-1 was physically disconnected, not because of the phone lifecycle.

## 12. Handoff

The implementation and build procedure are documented in the gud and Mir
repositories. E3-T02/T03 can be treated as hardware-verified for the tested
paths; E3-T04/T05 remain partial. After closing those evidence gaps, proceed to
E6 product packaging/integration.

# Automatic GUD Plug-and-Display Lifecycle

## Result

PARTIAL — the automatic userspace lifecycle is implemented and statically
validated. New-binary hardware qualification is gated because the workstation
cannot configure the full Mir build and the current live devices are below the
OnePlus VID/PID attachment gate (`1d50:614d` absent; Pi UDC `not attached`).

## Architecture

- Selected architecture: standalone `xdispd` + managed `mirgud` + public Mir
  screencast/virtual-output path.
- Mir modifications: NO.
- Android2 modifications: NO for this lifecycle change.
- HWC modifications: NO.
- Lifecycle owner: `xdispd`.
- Activation: always-running systemd service, bounded startup scan, then udev
  DRM event monitor; one managed child per live GUD instance.
- Discovery: DRM driver identity `gud`, connected 1280x720 mode, udev sysfs
  identity, device-number validation, and pre-spawn revalidation.

## Before and after

Observed prior manual path: establish USB/DRM, start or restart `xdisp.service`,
then issue D-Bus `Enable` and `Activate`; the old lifecycle required explicit
reactivation after a fresh add.

New path:

```text
GUD DRM add or startup scan
  -> xdispd exact-instance discovery
  -> managed mirgud child
  -> Mir public extend screencast / virtual output
  -> Mir DisplayConfiguration update
  -> qtmir ScreensModel update
  -> Lomiri additional screen

GUD DRM remove
  -> exact instance invalidation
  -> bounded presenter/KMS child teardown
  -> Mir screencast/virtual-output release
  -> Mir DisplayConfiguration update
  -> qtmir screen removal
```

Manual work remaining in the qualified deployment: none for DRM enumeration,
xdispd launch, mirgud launch, or Lomiri rescan. The new artifact still needs a
real-phone deployment and the complete hardware matrix.

## Lomiri event path

Existing retained Mir/qtmir captures show virtual output 3 at 1280x720, Mir
configuration delivery, `MirDisplayConfigurationObserver::configuration_applied`,
qtmir `ScreensModel::updateInternal()`, and screen creation/removal. The new
implementation relies on that path and adds no restart/rescan mechanism.

## Resource state while absent

`xdispd` remains resident but sleeps in the GLib/udev event loop. It does not
run a periodic scan, create a Mir object, start `mirgud`, capture, compress, or
send GUD traffic. Numeric CPU/RSS values are intentionally not claimed because
the new binary was not deployed; see `idle-resources.txt`.

## Hardware matrix and reconnect

No-GUD baseline: gated below the GUD attachment acceptance check.
Connect-after-login, present-before-session, active disconnect, and automatic
same-boot reconnect with the new artifact: pending deployment. Existing E3-B02
evidence establishes the underlying device lifecycle and an active
detach/reconnect path, but used the prior explicit activation semantics.
Same-boot reconnect remains platform-blockable if the OnePlus host controller
does not recreate `1d50:614d`; the troubleshooting guide requires a fresh
dynamic VID/PID result before KMS diagnosis.

## Packaging

Phone manifest: `gud.ko`, `modules-load.d/gud.conf`, `xdispd`, `mirgud`,
`xdisp-status`, `xdisp.service`, D-Bus service/policy files, state directory,
and ABI-matched Mir platform libraries.

Pi manifest: `gud-userspace.service`, FunctionFS configfs gadget `1d50:614d`,
UDC binding `3f980000.usb`, and the pinned VC4/HDMI GUD runtime.

## Repository status

Changes are uncommitted at capture time. No push was performed. The evidence
bundle is intentionally a partial qualification record, not a hardware PASS.

# XDISP-P0.1: FunctionFS rebind reliability test specification

## Goal

Prove that the Pi GUD gadget accepts the **first** bulk payload after a fresh
gadget rebind or OnePlus reconnect. This is a transport gate for the external
display work; it does not test Mir or Lomiri.

## Scope

The system under test is:

```text
OnePlus `gud.ko` + `gud-kms-fill` -> USB -> Pi FunctionFS GUD service
```

Use the normal GUD RGB565 1280x720 KMS smoke/fill path. Do not run the
experimental Mir graphics plugin during this test. The test begins only after
the OnePlus sees `1d50:614d`, `gud.ko` has probed, and the active GUD DRM node
is present.

## Failure being guarded

After a gadget rebind or phone reboot/reconnect, the Pi can report a configured
UDC and FunctionFS can accept descriptors, yet its bulk reader never receives
the first framebuffer payload. The OnePlus then reports a GUD bulk or atomic
update timeout (`-110`) and subsequent control requests can time out.

The result is a **failure** if any of the following occur during a cycle:

- the first 64 KiB bulk payload does not complete;
- the host logs `GUD bulk transfer failed` or `GUD atomic update failed: -110`;
- Pi service logs show the bulk reader waiting indefinitely for that first
  payload; or
- recovery requires a manual device-node symlink or modification of the Mir
  POC.

## Test matrix

Perform ten numbered cycles. At least five must start with a Pi gadget rebind;
at least five must start with a OnePlus USB reconnect/reboot. They may overlap,
but record which reset caused each cycle.

For every cycle:

1. Record the cycle number, reset type, Pi boot/service identity, and OnePlus
   kernel time.
2. Perform exactly one defined reset action: Pi gadget rebind, cable/USB-host
   reconnect, or phone reboot followed by its normal host-mode setup.
3. Wait for the OnePlus enumeration gate: `1d50:614d`, successful `gud.ko`
   probe, and a live GUD `/dev/dri/cardX` node. Record the actual node; it is
   not necessarily `card1`.
4. Run the isolated KMS fill/smoke tool once. Its first transfer must include
   at least 64 KiB of RGB565 payload.
5. Capture the Pi GUD service log and the focused OnePlus kernel log from just
   before enumeration through the end of the first transfer.
6. Mark the cycle pass or fail before attempting recovery. If it fails, retain
   the logs, state the first failing operation, and begin the next cycle from a
   new reset.

## Acceptance

`XDISP-P0.1` becomes **verified** only if all ten cycles pass, each first
64 KiB payload completes, and none of the retained host logs contains a GUD
`-110` bulk/atomic timeout. A single passing frame, or a test that succeeds
only after retrying the first transfer, is diagnostic evidence and does not
pass this specification.

## Evidence record

Store a short table with one row per cycle and preserve the referenced raw
logs. At minimum record:

| Cycle | Reset type | GUD DRM node | First payload size/time | Host result | Pi result | Log paths |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Pi rebind | `cardX` | 64 KiB, `N` ms | pass/fail | pass/fail | host + Pi |

When this gate passes, update the canonical entry in
`../../gud/PROJECT-STATUS.md` from **blocked** to **verified** with the
evidence location. Only then resume experimental Mir output testing.

# XDISP-P0.1: FunctionFS rebind reliability test specification

## Goal

Prove that the Pi GUD gadget accepts the **first** bulk payload after a fresh
gadget rebind or OnePlus reconnect. This is a transport gate for the external
display work; it does not test Mir or Lomiri.

The cross-repository design and implementation plan are maintained in the
host project at `../../gud/docs/superpowers/specs/2026-07-25-xdisp-p0-1-functionfs-rebind-design.md`
and `../../gud/docs/superpowers/plans/2026-07-25-xdisp-p0-1-functionfs-rebind.md`.
This file is the single executable hardware test procedure.

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

- the first 64,000-byte RGB565 tile does not complete;
- the host logs `GUD bulk transfer failed` or `GUD atomic update failed: -110`;
- Pi service logs show the bulk reader waiting indefinitely for that first
  payload or show no matching read completion;
- the Pi logs an Oops/BUG, allocator or slab corruption, or a DWC2 endpoint
  stop timeout;
- the Pi loses SSH or otherwise becomes unresponsive during the payload or
  teardown;
- the service exits unexpectedly or the Pi boot ID changes; or
- recovery requires a manual device-node symlink or modification of the Mir
  POC.

The process marks the receive state `InFlight` before entering FunctionFS. An
absent completion therefore refuses teardown while it remains blocked; a
short, zero, `ENOMEM`, late, or other failed completion changes the state to
`Poisoned`. Either state is terminal for the hardware instance, just like a Pi
kernel/allocator/DWC2 failure or loss of responsiveness. Do not run
`systemctl stop`, `systemctl restart`, `reboot`, or `shutdown` on the affected
instance. Preserve any still-accessible read-only evidence, use a physical
power cycle, hardware reset, or watchdog reset, and immediately collect the
previous-boot service/kernel journals, `/sys/fs/pstore`, and watchdog state
before another gadget start.

## Step 5 pre-matrix gate

Do not begin the ten-cycle matrix with the historical 512-byte loop or with
the optional 64 KiB A/B setting. Before the first hardware payload:

1. Disable auto-start without signaling the current process:

   ```bash
   sudo systemctl disable gud-userspace.service
   systemctl is-active gud-userspace.service
   ```

   If it reports `inactive` or `failed` and the boot has no payload anomaly, a
   clean reboot is allowed. If it reports `active`, stop it separately only
   after proving it is idle and the current boot has had no payload anomaly.
   Otherwise use the non-graceful physical/hardware reset path above. After
   recovery, require `systemctl is-active gud-userspace.service` to report
   `inactive` or `failed`; do not issue another stop.
2. Build/stage the AArch64 Step 5 artifact and both tracked drop-ins. The
   expected deployed paths and hashes are:

   ```text
   /home/cristian/gud-drm
     0c5961daf65a543101bb1727c2c6909a19b5a48ae398cadd94c4c6ebd044b662
   /etc/systemd/system/gud-userspace.service.d/10-xdisp-p0.1-containment.conf
     5da095b75ac6797077e31d88edad25d6a2aea334e01430040e0a147a7707ca5c
   /etc/systemd/system/gud-userspace.service.d/20-xdisp-p0.1-ffs-read-size.conf
     4b074555a5aca73d494aa824c9499577e0334d4160bc6a0055d6e9086d842c49
   ```

   From the `gud-gadget` repository, stage the files under `/home/cristian/`:

   ```bash
   scp target/release/gud-drm cristian@192.168.1.110:/home/cristian/gud-drm.step5
   scp systemd/gud-userspace.service.d/10-xdisp-p0.1-containment.conf cristian@192.168.1.110:/home/cristian/
   scp systemd/gud-userspace.service.d/20-xdisp-p0.1-ffs-read-size.conf cristian@192.168.1.110:/home/cristian/
   ```

   On the Pi, first hash and preserve the currently deployed
   `7053d5b1...` binary, then install while the service is inactive:

   ```bash
   sha256sum /home/cristian/gud-drm
   sudo cp --preserve=all /home/cristian/gud-drm /home/cristian/gud-drm.pre-xdisp-p0.1-step5-7053d5b
   sha256sum /home/cristian/gud-drm.pre-xdisp-p0.1-step5-7053d5b
   sudo install -m 0755 /home/cristian/gud-drm.step5 /home/cristian/gud-drm
   sudo install -D -m 0644 /home/cristian/10-xdisp-p0.1-containment.conf /etc/systemd/system/gud-userspace.service.d/10-xdisp-p0.1-containment.conf
   sudo install -D -m 0644 /home/cristian/20-xdisp-p0.1-ffs-read-size.conf /etc/systemd/system/gud-userspace.service.d/20-xdisp-p0.1-ffs-read-size.conf
   sudo systemctl daemon-reload
   ```

   Require both pre-install hashes to equal
   `7053d5b1cf3f7cc94da776a97f64d3471382df9b8f40b00b511eb9ef3bcf1e12`;
   if the deployed input differs, stop and record it instead of overwriting an
   unidentified binary.
3. Verify all three deployed hashes. Also retain:

   ```bash
   sha256sum /home/cristian/gud-drm
   sha256sum /etc/systemd/system/gud-userspace.service.d/10-xdisp-p0.1-containment.conf
   sha256sum /etc/systemd/system/gud-userspace.service.d/20-xdisp-p0.1-ffs-read-size.conf
   systemctl show gud-userspace.service -p FragmentPath -p DropInPaths -p Restart -p SendSIGKILL -p Environment
   cat /proc/sys/kernel/random/boot_id
   cat /sys/module/dwc2/parameters/g_dma
   cat /sys/module/dwc2/parameters/g_dma_desc
   cat /sys/class/udc/3f980000.usb/state
   cat /proc/meminfo
   cat /proc/buddyinfo
   find /sys/fs/pstore -maxdepth 1 -type f -print -exec sha256sum {} \;
   cat /proc/sys/kernel/watchdog
   cat /proc/sys/kernel/watchdog_thresh
   ```

   Require both drop-ins in `DropInPaths`, `Restart=no`, `SendSIGKILL=no`,
   `GUD_FFS_READ_SIZE=16384`, and `RUST_LOG=debug`. Also retain any readable
   `/sys/class/watchdog/watchdog0/{bootstatus,identity,state,status}` values.
4. While the service remains inactive, restore and verify the normal OnePlus
   module and prepare host mode. Start the Pi service exactly once, then wait
   for the mandatory dynamic gate: OnePlus enumeration of `1d50:614d`,
   successful `gud.ko` probe, and a live GUD DRM node. Require
   `/sys/class/udc/3f980000.usb/current_speed` to report `high-speed`.

   ```bash
   sudo systemctl start gud-userspace.service
   ```
5. Only after that gate passes, send one isolated 1280x720 RGB565 frame. Before
   teardown, require all 29 contiguous `frame_stats` records: 28 64,000-byte
   tiles plus one 51,200-byte tile, totaling 1,843,200 bytes. Every tile must
   report `read_size=16384` and `read_calls=4`. Each normal tile must show
   requests `[16384, 16384, 16384, 14848]`; the final tile must show
   `[16384, 16384, 16384, 2048]`. Require host `PAYLOAD_RC=0`, no host `-110`,
   and no Pi failure condition.
6. Capture host usbmon for the first URB when available and require
   `status=0, actual_length=64000`. If usbmon is unavailable, record that fact;
   `PAYLOAD_RC=0` is the fallback proof because the unchanged host transfer
   logic in `gud_pipe.c` returns an error when URB `actual_length` differs from
   the submitted tile length.
7. Immediately after the complete frame, retain the Pi service/kernel journal,
   boot ID, UDC state/speed, `/proc/meminfo`, `/proc/buddyinfo`, allocator
   warnings, host result, and host kernel log. Only if every completion and
   baseline is clean may the controlled UDC-first Step 2 stop/start run.
8. Run three fresh 16,384-byte payload/teardown/rebind mini-cycles. The
   16,384-byte setting may advance to the official matrix after all three pass.
   A 65,536-byte ceiling is an optional later A/B comparison, not a prerequisite
   and not the first hardware value. Keep boot auto-start disabled throughout
   the mini-cycles and matrix. For a proven-clean Pi rebind, issue separate
   `systemctl stop` and `systemctl start` commands; never substitute
   `systemctl restart`. Re-enable auto-start only after full acceptance and
   explicit authorization.

   ```bash
   sudo systemctl stop gud-userspace.service
   sudo systemctl start gud-userspace.service
   ```

Any Step 5 read anomaly is terminal for that boot. Do not retry it at 16 KiB,
do not switch sizes in the same boot, never fall back to 512 bytes, and recover
only with a physical power cycle, hardware reset, or watchdog reset.

## Test matrix

Perform ten numbered cycles. At least five must start with a Pi gadget rebind;
at least five must start with a OnePlus USB reconnect/reboot. Keep the reset
categories isolated and record the one reset action that caused each cycle.

For every cycle:

1. Record the cycle number, reset type, Pi boot/service identity, and OnePlus
   kernel time.
2. Perform exactly one defined reset action: Pi gadget rebind, cable/USB-host
   reconnect, or phone reboot followed by its normal host-mode setup.
3. Wait for the OnePlus enumeration gate: `1d50:614d`, successful `gud.ko`
   probe, and a live GUD `/dev/dri/cardX` node. Record the actual node; it is
   not necessarily `card1`.
4. Run the isolated KMS fill/smoke tool once. Its first transfer must be the
   normal 64,000-byte RGB565 tile.
5. Capture the Pi GUD service log and the focused OnePlus kernel log from just
   before enumeration through the end of the complete 29-tile frame.
6. For every tile retain the selected `read_size`, matching `payload_seq`,
   request/result byte counts, and `read_calls`; also retain the negotiated UDC
   speed.
7. Mark the cycle pass or fail before attempting recovery. If it fails, retain
   the logs, state the first failing operation, and recover with a physical
   power cycle, hardware reset, or watchdog reset under the containment rule
   above. Never stop/restart or gracefully reboot/shut down the failed
   instance.

## Acceptance

`XDISP-P0.1` becomes **verified** only if all ten cycles pass at the selected
16,384-byte setting, each complete 1,843,200-byte frame and its first
64,000-byte tile complete, and none of the retained host logs contains a GUD
`-110` bulk/atomic timeout. A single passing frame, an optional 65,536-byte A/B
result, or a test that succeeds only after retrying the first transfer is
diagnostic evidence and does not pass this specification.

## Evidence record

Store a short table with one row per cycle and preserve the referenced raw
logs. At minimum record:

| Cycle | Reset type | GUD DRM node | Read size | First tile requests/results/calls | Frame bytes/tiles | UDC speed | Host result | Pi result | Log paths |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Pi rebind | `cardX` | 16384 | 4 matching reads/4 | 1843200/29 | high-speed | pass/fail | pass/fail | host + Pi |

When this gate passes, update the canonical entry in
`../../gud/PROJECT-STATUS.md` from **blocked** to **verified** with the
evidence location. Only then resume experimental Mir output testing.

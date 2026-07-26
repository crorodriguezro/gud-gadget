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

## Step 5 read strategy and rationale

The Step 5 change replaces the Pi service's historical 512-byte userspace read
loop with a configurable, packet-aligned FunctionFS read ceiling. The first
hardware setting is 16,384 bytes. A normal 64,000-byte tile is therefore read
as `16384 + 16384 + 16384 + 14848`, reducing 125 blocking FunctionFS read
operations to four. The last 51,200-byte tile is read as
`16384 + 16384 + 16384 + 2048`.

This changes the number and size of userspace/FunctionFS requests, not the
number of 512-byte USB high-speed packets on the wire. Its goal is to reduce
FunctionFS request churn and the timing surface around the DWC2 endpoint while
retaining a bounded allocation size. Starting at 16 KiB is deliberately more
conservative than immediately requesting one 64 KiB kernel buffer because the
reproduced failure also showed allocator corruption on a DWC2 configuration
without scatter-gather support.

The final read always requests the exact remaining protocol payload. It is not
padded to 16 KiB or even to 512 bytes, because the host sends exactly the
declared tile length and does not promise extra padding or a terminating
zero-length packet. Padding the userspace request could create a new indefinite
wait after a complete host transfer.

This is a diagnostic stability change, not a proven fix. A clean complete
frame followed by a safe stop/start establishes only the first Step 5 gate.
Three clean mini-cycles are required before the official ten-cycle matrix, and
only all ten matrix passes can verify `XDISP-P0.1`.

## First 16 KiB hardware result: failed

The first controlled Step 5 hardware run on 2026-07-25 deployed and verified
the documented binary and drop-ins, manually started the service from an
inactive state, and reached a fresh high-speed `1d50:614d` probe after the
documented OnePlus `device -> host` role reset.

The first 64,000-byte tile failed on read 1 of 4. The Pi requested 16,384 bytes
and FunctionFS returned `18446744073709045760`, the unsigned representation of
signed `-505856`, after 476 microseconds. DWC2 debugfs simultaneously showed
buffer DMA enabled (`g_dma=1`, `g_dma_desc=0`) and ep1 OUT `DOEPTSIZ=0x7f800`,
or 522,240 bytes remaining, despite only 16,384 bytes being loaded. The
resulting unsigned arithmetic is:

```text
0x4000 - 0x7f800 = 0xfff84800 = -505856 signed
```

That result matches the DWC2 buffer-DMA completion path that derives
`req->actual` from loaded bytes minus the endpoint's residual count. FunctionFS
propagated the impossible completion, and the Step 5 service correctly entered
`Poisoned` without another read or teardown. The Pi stayed reachable and
showed no new kernel Oops, allocator warning, endpoint-stop timeout, or pstore
record.

The OnePlus utility printed `PAYLOAD_RC=0`, but its fresh kernel log recorded
bulk and atomic-update `-110`, followed by control request `0x64` `-110`.
`PAYLOAD_RC=0` is therefore not proof of successful transport for this
asynchronous update path; host kernel status plus a complete Pi `frame_stats`
record are mandatory.

Conclusion: larger reads improved fault localization and containment, but did
not fix transport. Do not run a 64 KiB A/B, return to 512 bytes, start the
mini-cycles, or attempt service stop/start. The poisoned instance requires
physical/hardware/watchdog reset. At that point the next proposed diagnostic
was a fresh-boot DWC2 gadget test with `g_dma=0`; the later laptop control and
revised ordering below supersede that ordering.

Evidence:
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-step5-16k-first-hardware-2026-07-25T2214BST/`.

## Initial read-count and `g_dma=0` decision

**Initial decision (2026-07-25, before the laptop control):** retain the
configurable 16 KiB implementation and its four exact-length FunctionFS reads
as the candidate receive strategy. Do not restore the historical 125-read loop
as a fix: it had one
clean run but later stalled during an active payload and was followed by
allocator corruption. The four-read run also failed with `g_dma=1`, but it
localized the failure to the first DWC2 buffer-DMA completion and the new
containment prevented teardown and a new Oops.

The next comparison changes only DWC2 gadget DMA:

```text
OnePlus host URB:       unchanged (one 64,000-byte bulk transfer per normal tile)
USB packets on wire:    unchanged (125 high-speed 512-byte packets)
Pi FunctionFS requests: unchanged Step 5 sequence (16,384 + 16,384 + 16,384 + 14,848)
Pi DWC2 data movement:  g_dma=1 buffer DMA -> g_dma=0 slave/PIO FIFO handling
```

Do not compare 125 reads against four reads in the same boot. The original
next action was the same four-read binary with `g_dma=0`; retain the procedure
below as a fallback, but do not execute it before the revised userspace-only
controls later in this document.

### Why this Pi needs a separate test kernel

Read-only inspection on 2026-07-25 established that the installed
`6.12.47+rpt-rpi-v8` kernel has `CONFIG_USB_DWC2=y`, so DWC2 is built in rather
than unloadable. It has no `/sys/module/dwc2/parameters/g_dma` file. The
installed `dwc2` overlay exposes only `dr_mode`, `g-rx-fifo-size`, and
`g-np-tx-fifo-size`; `/sys/kernel/debug/usb/3f980000.usb/params` is mode `0444`
and reports state rather than accepting changes.

The matching Raspberry Pi DWC2 source auto-enables `g_dma` on a DMA-capable
gadget controller, does not read a device property for it, and leaves the
Broadcom parameter callback at that default:

- [DWC2 parameter initialization](https://github.com/raspberrypi/linux/blob/rpi-6.12.y/drivers/usb/dwc2/params.c)
- [DWC2 gadget DMA/slave paths](https://github.com/raspberrypi/linux/blob/rpi-6.12.y/drivers/usb/dwc2/gadget.c)
- [Raspberry Pi `dwc2` overlay parameters](https://github.com/raspberrypi/firmware/blob/master/boot/overlays/README)

Consequently, none of these changes enables the required test on this kernel:

```text
dwc2.g_dma=0 in cmdline.txt          unsupported: no such module parameter
dtoverlay=dwc2,g_dma=0               unsupported: no such overlay/property
write 0 to the debugfs params file   impossible: the file is read-only
unload/reload dwc2                   impossible: CONFIG_USB_DWC2=y
```

Do not clear the controller DMA bit with `devmem` and do not impersonate
another SoC with a Device Tree `compatible` string. Either would leave the
driver's software state inconsistent with the hardware or apply unrelated
SoC parameters.

The controlled implementation is a one-line Broadcom-specific test change in
the matching Raspberry Pi kernel source:

```diff
 static void dwc2_set_bcm_params(struct dwc2_hsotg *hsotg)
 {
     struct dwc2_core_params *p = &hsotg->params;

+    p->g_dma = false;
     p->host_rx_fifo_size = 774;
```

Build and install it as a separately named test kernel with its matching
modules and initramfs; do not overwrite the stock kernel or its modules. Keep
the stock boot selection as the rollback path. The poisoned instance that
produced the Step 5 failure was subsequently physically recovered. Any future
kernel test must likewise start from a clean boot with service auto-start
disabled, never by transitioning a poisoned instance.

On the first fresh boot, do not start the service or send a payload until all
of these gates pass:

```bash
uname -r
grep -E '^CONFIG_USB_DWC2(=|_)' /boot/config-$(uname -r)
systemctl is-active gud-userspace.service
grep -E 'g_dma|g_dma_desc' /sys/kernel/debug/usb/3f980000.usb/params
cat /proc/sys/kernel/random/boot_id
find /sys/fs/pstore -maxdepth 1 -type f -print
```

Require a distinct test-kernel release, `CONFIG_USB_DWC2=y`, an inactive or
failed service, `g_dma: 0`, and `g_dma_desc: 0`. Also collect the previous-boot
service/kernel journals, pstore, and watchdog state before the one isolated
four-read payload. A kernel that still reports `g_dma: 1` is not the intended
test and must receive no payload.

### Modern-laptop control with `g_dma=1`: passed

After physical recovery, the same Pi binary and 16 KiB setting were started
once from an inactive service and connected to a modern Linux 7.0 Asahi/Fedora
laptop using its upstream in-tree GUD driver and xHCI. KDE recognized the
gadget as an extended monitor at 1920x1080. The Pi remained in buffer-DMA mode
(`g_dma=1`, `g_dma_desc=0`) at high speed.

The live aggregate reached 953 completed compressed payloads, 432,606,856
transfer bytes, and 27,053 FunctionFS read calls. All 27,053 reads completed;
there were zero short reads, poisoned transitions, or impossible completion
lengths. Receive time averaged 14.86 ms and total Pi processing averaged
32.48 ms. A representative 420,584-byte compressed 1920x1080 update used 26
reads (`25 * 16384 + 10984`) and completed receive/decompress/copy processing
in 27 ms. Neither host nor Pi logged a transport/kernel failure.

This control proves that neither 16 KiB reads nor DWC2 buffer DMA is
universally broken on this Pi. The OnePlus failure is conditional on host,
transfer shape, timing, or their interaction. Important differences include
the laptop's modern upstream GUD/xHCI path and compressed full-screen payloads
versus the OnePlus Linux 4.9 backport and uncompressed 64,000-byte tiles.

Keep the four-read candidate and do not restore the 125-read loop. This laptop
control does not satisfy any OnePlus mini-cycle or acceptance-matrix entry, so
`XDISP-P0.1` remains blocked. The later Gate B result below supersedes the
initial conclusion that `g_dma=0` would be useful only for an OnePlus-specific
A/B: the same impossible DWC2 residual was reproduced with the laptop's
upstream host driver.

Evidence:
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-16k-live-2026-07-25T2256BST/`.

The final form of this control reached 4,786 complete compressed payloads.
After physical USB detach, the last receive was `Idle`; a controlled stop
exited zero, unbound the UDC, completed FunctionFS teardown before DRM release,
and produced no DWC2 stop timeout or Pi kernel fault. Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-16k-final-detach-2026-07-25T2345BST/`.

The user saw no noticeable performance difference between the historical
512-byte userspace reads and the 16 KiB implementation. Treat that as a useful
subjective observation, not a benchmark or a performance claim. The 16 KiB
setting remains selected for request-count reduction, diagnostics, and
containment—not because it has demonstrated higher visible frame rate.
Controlled read-granularity benchmarking is deferred to `XDISP-P2.1`, after
the reliability gate. Do not re-enable 512-byte reads during `XDISP-P0.1` to
measure performance.

## Laptop transfer-shape gates: Gate A passed; Gates B and C failed

The test-only descriptor controls are implemented as
`GUD_TEST_COMPRESSION=lz4|none` and
`GUD_TEST_MAX_BUFFER_SIZE=<positive u32>`. With both absent, the normal LZ4 and
natural maximum-buffer defaults remain unchanged. Invalid values fail before
UDC setup. Install only one temporary drop-in at a time; any descriptor change
requires a fresh Pi boot and USB enumeration.

Gate A used LZ4, `max_buffer_size=64000`, 16 KiB FunctionFS reads, and
`g_dma=1`. Usbmon captured 5,671 matched SET_BUFFER/bulk pairs with no control
or bulk error, including 15 complete RGB565 1280x720 frames. All actual
compressed bulk URBs were 131--12,600 bytes; none reached 16 KiB. After
physical detach, the controlled service stop exited zero with no DWC2 timeout,
Oops, or pstore record. Gate A passed. Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-a-2026-07-25T1754COT/`.

Gate B used no compression with the same advertised 64,000-byte maximum,
16 KiB FunctionFS reads, and `g_dma=1`. KDE restored 1920x1080 before a
1280x720 mode change could be committed, so the first complete-row rectangle
was 1920x16, or 61,440 bytes, rather than 1280x25/64,000 bytes. This does not
invalidate the isolation result: the first SET_BUFFER completed, and usbmon
proves that the upstream GUD/xHCI host submitted one 61,440-byte bulk URB. It
was cancelled 3.033326 seconds later with status `-104` after 18,432 bytes,
while the host logged framebuffer flush `-110`.

The Pi's matching first 16,384-byte read returned
`18446744073709045760`, signed `-505856`. The preserved ep1 OUT register was
`DOEPTSIZ=0x0007f800`, whose transfer residual is 522,240 bytes:

```text
16384 - 522240 = -505856
```

The receive session entered `Poisoned` and parked without teardown. Physical
recovery retained the prior boot journal; pre-reset and post-reset pstore were
empty, watchdog bootstatus was zero, and neither boot contained a DWC2 stop
timeout or kernel Oops. Gate B failed safely. Its drop-in is quarantined and
must not be installed again for an unchanged 64,000-byte test. Evidence is
under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-b-2026-07-25T1810COT/`.

Gate C kept compression disabled but reduced the advertised maximum to 15,360
bytes. The first SET_BUFFER was 1920x4/15,360 bytes. Usbmon proves that its
single bulk URB completed in full with status zero after 774 microseconds, but
the matching one-call FunctionFS read never returned. Live DWC2 state retained
the request in flight with zero bytes done and `DOEPTSIZ=0x00200000`, even
after physical detach. Containment prevented teardown and physical recovery
found no endpoint-stop timeout, Oops, watchdog reset, or pstore record.
Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-c-2026-07-25T1834COT/`.

Gate C disproves a simple large-transfer threshold. Gate A's 5,671 successful
actual bulk lengths were all nonmultiples of 512, whereas both failed
uncompressed lengths—61,440 and 15,360—are exact multiples of the high-speed
512-byte maxpacket. Gate C additionally shows successful delivery of all host
bytes without completion of the Pi read. The leading hypothesis is therefore
DWC2 buffer-DMA/FunctionFS OUT completion when a transfer ends on a full
maxpacket without a terminating short packet or ZLP. This is a strong
correlation, not sole-cause proof, because Gate A also used compression.

## Revised no-kernel packet-termination isolation

Do not repeat Gates B or C and do not compile a Pi kernel yet:

1. Run Gate D from a fresh, kernel-clean boot with the service inactive.
   Keep `GUD_FFS_READ_SIZE=16384`, `g_dma=1`, and compression disabled.
   Advertise `max_buffer_size=11520`.
2. Gate D is valid only when the first SET_BUFFER is 1920x3/11,520 bytes. That
   host bulk transfer ends with 22 full 512-byte packets and a 256-byte short
   packet. If the cached mode is not 1920 wide, stop before interpreting the
   result and select a maximum that produces a non-512-aligned complete-row
   tile in the actual mode.
3. Capture usbmon and full host/Pi journals. Require at least one complete
   frame, exact successful URB/read matching, an `Idle` receive session, and a
   safe post-detach controlled stop. Any impossible/short read, host timeout,
   late completion, or kernel anomaly poisons that boot and requires physical
   recovery.
4. If Gate D passes repeatedly, treat full-maxpacket termination as confirmed
   strongly enough to test a separately named OnePlus diagnostic `gud.ko`
   that requests `URB_ZERO_PACKET` only for aligned bulk writes. Preserve
   `/home/phablet/gud.ko` unchanged. This builds one module, not the Pi kernel.
5. If Gate D fails, alignment alone is insufficient. The next isolation is one
   Pi test kernel with `g_dma=0`; do not vary the host module at the same time.
6. Only after one clean OnePlus frame and a safe restart may the three
   mini-cycles and then the ten-cycle matrix begin.

This packet-termination isolation is a correctness diagnostic, not the
deferred performance benchmark. `XDISP-P0.1` remains blocked.

## Post-isolation pre-matrix gate

Do not execute this section yet. It is gated on the transfer-shape control
above and then one evidence-backed OnePlus changed-variable test completing
the full frame plus a safe controlled stop/start. Do not begin the ten-cycle
matrix with the historical 512-byte loop or with the optional 64 KiB A/B
setting. After the gate is satisfied and before the first mini-cycle payload:

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
   grep -E 'g_dma|g_dma_desc' /sys/kernel/debug/usb/3f980000.usb/params
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
   `status=0, actual_length=64000`. If usbmon is unavailable, record that fact
   and require both a host kernel log without GUD failure and the complete
   matching Pi `frame_stats` sequence. `PAYLOAD_RC=0` records only that the
   userspace invocation returned; the first Step 5 hardware test proved that
   it can coexist with a later asynchronous host bulk/atomic `-110`.
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

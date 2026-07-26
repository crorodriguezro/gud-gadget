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

Gate D passed one complete 1920x1080 frame as 360 uncompressed
1920x3/11,520-byte transfers, each ending in a 256-byte short packet. KDE then
selected 1280x720 and the same session passed six complete frames as 1,080
uncompressed 1280x4/10,240-byte transfers. Those later transfers are exact
20-packet multiples. Usbmon recorded 1,440 matched successful bulk transfers,
all with transfer flags zero and no zero-length bulk URB. The Pi recorded
1,440 returns to `Idle`, and post-detach controlled shutdown exited zero with
clean gadget/DRM teardown and no kernel anomaly. Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-d-2026-07-25T1905COT/`.

Gate D rejects exact maxpacket termination as a sufficient trigger. Do not
implement the proposed OnePlus `URB_ZERO_PACKET` diagnostic on this evidence.
The useful boundary is now an aligned 10,240-byte pass versus aligned
15,360-byte failure; size and/or intermittent DWC2 state still matters.

## Revised no-kernel aligned-size isolation

Do not repeat Gates B or C and do not compile a Pi kernel yet:

1. Run Gate E from a fresh, kernel-clean boot with the service inactive.
   Keep `GUD_FFS_READ_SIZE=16384`, `g_dma=1`, and compression disabled.
   Advertise `max_buffer_size=12800`.
2. Interpret Gate E only after observing 1280x5/12,800-byte SET_BUFFERs. They
   are exactly 25 high-speed 512-byte packets, between the proven-clean
   10,240-byte/20-packet value and failed 15,360-byte/30-packet value. If the
   host starts at 1920, its temporary 1920x3/11,520-byte transfers repeat an
   already clean shape; wait for the cached 1280 mode without treating those
   transfers as Gate E.
3. Capture usbmon and full host/Pi journals. Require at least one complete
   1280x720 frame, exact successful URB/read matching, an `Idle` receive
   session, and a safe post-detach controlled stop. Any impossible/short read,
   host timeout, late completion, or kernel anomaly poisons that boot and
   requires physical recovery.
4. If Gate E fails, retain 10,240 bytes as the current userspace ceiling
   candidate and verify that candidate repeatedly before OnePlus use. Keep
   one `g_dma=0` Pi test kernel as later root-cause isolation.
5. If Gate E passes, the clean/failing aligned boundary narrows to
   12,800--15,360 bytes. Repeat the clean candidate on separate fresh boots
   because the historical 64,000-byte behavior was intermittent.
6. Only after a repeated laptop candidate, one clean OnePlus frame, and a safe
   restart may the three mini-cycles and then the ten-cycle matrix begin.

This aligned-size isolation is a correctness diagnostic, not the deferred
performance benchmark. `XDISP-P0.1` remains blocked.

### Gate E qualified across three fresh laptop boots

The first Gate E boot completed one known-clean 1920 frame and then six
complete target 1280x720 frames: 864 aligned 12,800-byte transfers. All 1,224
session transfers matched in usbmon with status zero and full length, and all
1,224 Pi reads returned to `Idle`. Physical detach and controlled stop were
clean. Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-e-2026-07-25T1919COT/`.

Two additional identical fresh-boot repeats also passed. Each run completed
six target 1280x720 frames, 864 aligned 12,800-byte transfers, and an exit-zero
safe stop. Across the three boots, all 2,592 target transfers completed
without a host URB error, length mismatch, Pi read anomaly, poisoned session,
or kernel fault. The qualification table is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-e-qualification-2026-07-25.md`.

Gate E qualifies 12,800 bytes only as a complete-transfer correctness
boundary. It is not an acceptable normal configuration: at 1280x720 the
advertised ceiling forces 144 SET_BUFFER operations per frame, and the user
observed roughly one visible frame every five seconds. Do not confuse this
descriptor-induced control cadence with the internal read-call optimization.

### Gate F failed: internal 12,800-byte reads cannot preserve large tiles

Gate F restored the normal LZ4 and natural maximum-buffer descriptor while
changing only `GUD_FFS_READ_SIZE=12800`. Its purpose was to keep one/few large
host rectangles while consuming their compressed bulk stream in smaller
reads.

The first fresh-boot payload was one full-screen 1920x1080 LZ4 rectangle:
4,147,200 bytes uncompressed and 16,274 bytes on the wire. The first
12,800-byte FunctionFS read returned after 395 microseconds with unsigned
`18446744073709042176`, signed `-509440`. The live ep1 OUT residual was again
`DOEPTSIZ=0x0007f800`, or 522,240 bytes:

```text
12800 - 522240 = -509440
```

Usbmon recorded the 16,274-byte submit and its cancellation 3.052803 seconds
later with status `-104` and 14,848 bytes actual; the laptop logged framebuffer
flush `-110`. Containment changed the Pi session to `Poisoned` and prevented
teardown. Physical recovery retained matching previous-boot journals; pstore
was empty and neither boot contained a DWC2 stop timeout or kernel Oops.
Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-laptop-gate-f-2026-07-25T2039COT/`.

Gate E passed when the complete host transfer and userspace read were both
12,800 bytes. Gate F proves that the same read size does not safely consume
the prefix of a larger host transfer with DWC2 buffer DMA enabled. Therefore
the advertised-buffer and internal-read ceilings cannot be decoupled as a
`g_dma=1` performance workaround. Gate F is quarantined and must not be
reinstalled unchanged.

Do not proceed to the OnePlus gate, mini-cycles, or matrix with Gate F. The
next root-cause isolation at that point was the separately named Pi `g_dma=0`
test kernel described above. That test does not modify the OnePlus kernel,
module, or Mir, but this installed Pi kernel cannot enable it through a boot
argument or overlay. Its completed result follows.

### `g_dma=0` laptop isolation failed on the first payload

The separately named one-shot kernel
`6.12.47+rpt-rpi-v8-xdisp-gdma0` booted with `CONFIG_USB_DWC2=y`,
`g_dma=0`, and `g_dma_desc=0`. The stock kernel, modules, `config.txt`,
`kernel8.img`, and `initramfs8` remained unchanged. The service started once
with the normal LZ4/natural-size descriptor and the 16 KiB blocking
FunctionFS read ceiling; neither a Gate E/F descriptor override nor a
OnePlus/Mir change was present.

The first normal laptop update was one 1920x1080 LZ4 rectangle:
4,147,200 bytes uncompressed and 16,274 bytes on the wire. Usbmon proves the
upstream GUD/xHCI host submitted and successfully completed that entire bulk
URB:

```text
bulk submit:   2026-07-26 00:23:06.039858, length=16274, status=-115
bulk complete: 2026-07-26 00:23:06.040681, actual=16274, status=0
elapsed:       823 microseconds
```

The Pi requested exactly 16,274 bytes from FunctionFS, but the slave/PIO path
returned only 3,986 bytes after 302 microseconds. The live ep1 state reported
`total_data=3986` and `DOEPTSIZ=0x00d83000`; its 12,288-byte transfer-size
residual exactly matches the missing part:

```text
16274 - 3986 = 12288
```

Containment changed the service to `Poisoned` and refused teardown. Later host
control retries triggered 17 instances of the DWC2 ep0-state warning at
`drivers/usb/dwc2/gadget.c:2534`; they are secondary to the first short bulk
read, not its cause. No stop/restart, OnePlus test, mini-cycle, or matrix was
attempted on the poisoned boot.

After physical USB detach and a physical reset, the Pi returned through the
cleared one-shot flag to stock `6.12.47+rpt-rpi-v8` with `g_dma=1`. The
service is disabled/inactive and the UDC is `not attached`. The retained
previous-boot journals match the final live captures; they contain no DWC2
endpoint-stop timeout, Oops, panic, or paging fault. Pstore is empty, watchdog
bootstatus is zero, and the recovery boot has no kernel error.

This is a failed diagnostic, not a workaround. Disabling buffer DMA changes
the observed failure from the `g_dma=1` impossible residual/hang to a
slave/PIO short completion even though the host completed the full URB. It
strengthens the localization to the Pi DWC2/FunctionFS receive path, but does
not identify a safe kernel fix. Do not ship or repeat this `g_dma=0` build
unchanged. The reproducible patch, one-shot boot configuration, and
stock-preserving installer are under `docs/test-only/`; evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-pi-gdma0-laptop-normal-2026-07-25T2140COT/`.

`XDISP-P0.1` remains blocked. Gate E remains the only laptop-qualified
userspace configuration, but its roughly one-frame-per-five-seconds cadence
is not a usable product setting. Normal-performance verification remains
blocked on a DWC2/FunctionFS receive fix; do not advance to the OnePlus gate
or verification matrix.

## Adaptive-LZ4 OnePlus pre-matrix result

The separately preserved OnePlus diagnostic module changed the host transfer
shape by compressing before splitting and enforcing a final actual-payload
cap of 12,800 bytes. Its first hardware gate passed on 2026-07-26:

- one 1280x720 RGB565 frame covered all 720 rows with four contiguous LZ4
  rectangles;
- actual payloads were 11,260, 12,144, 12,380, and 10,204 bytes;
- all four payloads completed in one 16 KiB FunctionFS read and returned the
  receive session to `Idle`;
- the host logged no `-110`, and neither kernel logged a warning or Oops;
- after physical detach, a controlled post-payload restart exited zero,
  unbound the UDC before FunctionFS/DRM release, kept the same Pi boot alive,
  and returned the new service instance to `not attached`.

The four Pi processing totals sum to 208 ms, approximately 4.8 full frames/s
for the highly compressible one-shot color-bar pattern. This is not a
sustained-cadence benchmark and does not establish product performance.

Evidence is under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-oneplus-adaptive-frame-2026-07-26T1154COT/`.
The result satisfies the one-frame and first safe-lifecycle prerequisites for
three fresh mini-cycles. Those mini-cycles subsequently passed 3/3 under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-oneplus-adaptive-mini-cycles-2026-07-26T1236COT/`.
They used 4, 3, and 4 compressed complete-row rectangles, with maximum actual
payloads of 12,728, 12,718, and 12,347 bytes. Every transfer returned to
`Idle`, and every proven-detached separate stop/start completed without host
`-110`, DWC2 stop timeout, vc4 fault, or Pi Oops. This authorizes the
ten-cycle matrix from cycle 1 but does not verify `XDISP-P0.1`.

The first matrix attempt passed cycle 1, then failed cycle 2 before payload
during a reconnect-only reset. The detached service safely removed its gadget
and FunctionFS resources, then exited status 1 to request recreation.
Containment's `Restart=no` prevented it, so the phone could not complete
enumeration. This was not a receive or kernel failure.

Do not continue that matrix. The replacement userspace policy is now
qualified: only a proven-idle safe-detach path exits successfully under
`Restart=on-success`; nonzero failures, poisoned receives, and crashes remain
non-restarting. In the dedicated reconnect gate, PID 2739 exited zero after
UDC-first teardown, systemd created PID 2836 on the same Pi boot, and the
OnePlus re-enumerated automatically. A fresh frame under PID 2836 covered all
720 rows with five one-read compressed payloads; the 12,735-byte maximum was
below the 12,800-byte cap, every receive returned to `Idle`, and the Pi kernel
remained clean. Start all ten matrix cycles again from cycle 1. Evidence is
under
`../../gud/backport-4.9/env/local/evidence/xdisp-p0.1-detach-restart-repair-2026-07-26T1446COT/`.

## Post-isolation pre-matrix gate

The adaptive-LZ4 result above satisfies this section's original entry
prerequisite. The detailed 64,000-byte normal-module frame expectations in
steps 4--7 below are retained as historical procedure and must not be used for
the adaptive mini-cycles: the original 29-tile transfer shape is unsafe on the
Pi and is deliberately absent from the diagnostic.

For each adaptive mini-cycle, require the diagnostic OnePlus module version
`xdisp-p0.1-adaptive-12800-v1`, contiguous complete-row coverage of the full
RGB565 frame, every actual payload at or below 12,800 bytes, matching complete
Pi `frame_stats`, and no host or kernel error. After the sender exits,
physically detach USB and prove every receive session returned to `Idle`.
Then use separate `systemctl stop` and `systemctl start` operations; do not
substitute `restart`.

This gate passed 3/3 on 2026-07-26. Use the same adaptive contract for the
official matrix below. Do not substitute the historical 64,000-byte
normal-module transfer shape.

Do not begin the ten-cycle matrix with the historical 512-byte loop or with
the optional 64 KiB A/B setting. The original procedure follows for
provenance:

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
3. Wait for the OnePlus enumeration gate: `1d50:614d`, successful
   `gud_xdisp_lz4_12800` probe, diagnostic version
   `xdisp-p0.1-adaptive-12800-v1`, and a live GUD `/dev/dri/cardX` node.
   Record the actual node; it is not necessarily `card1`.
4. Run the isolated KMS fill/smoke tool once at 1280x720 RGB565. Require the
   adaptive module to cover the full frame with contiguous, non-overlapping
   complete-row rectangles and to report every actual payload at or below
   12,800 bytes.
5. Capture the Pi GUD service log and the focused OnePlus kernel log from just
   before enumeration through the end of the complete adaptive frame.
6. For every rectangle retain `SetBuffer`, the selected `read_size`, matching
   `payload_seq`, request/result byte counts, `read_calls`, `frame_stats`, and
   the transition from `InFlight` back to `Idle`; also retain the negotiated
   UDC speed.
7. Mark the cycle pass or fail before attempting recovery. If it fails, retain
   the logs, state the first failing operation, and recover with a physical
   power cycle, hardware reset, or watchdog reset under the containment rule
   above. Never stop/restart or gracefully reboot/shut down the failed
   instance.

## Acceptance

`XDISP-P0.1` becomes **verified** only if all ten cycles pass with the Pi
16,384-byte read ceiling and the separately preserved adaptive diagnostic
module. Each cycle must cover the complete 1,843,200-byte RGB565 frame with
contiguous complete-row rectangles, keep every actual payload at or below
12,800 bytes, match every receive with completion and return to `Idle`, and
contain no host GUD `-110`, short/impossible read, poisoned receive, DWC2
endpoint-stop timeout, vc4 fault, or Pi Oops. A single passing frame, the
three passing mini-cycles, an optional 65,536-byte A/B result, or a test that
succeeds only after retrying the first transfer does not pass this
specification.

## Evidence record

Store a short table with one row per cycle and preserve the referenced raw
logs. At minimum record:

| Cycle | Reset type | GUD DRM node | Read ceiling | Rectangle payloads/read calls | Row coverage / source bytes | UDC speed | Host result | Pi result | Log paths |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Pi rebind | `cardX` | 16384 | all `<=12800`, matching one-read completions | 720 contiguous / 1843200 | high-speed | pass/fail | pass/fail | host + Pi |

When this gate passes, update the canonical entry in
`../../gud/PROJECT-STATUS.md` from **blocked** to **verified** with the
evidence location. Only then resume experimental Mir output testing.

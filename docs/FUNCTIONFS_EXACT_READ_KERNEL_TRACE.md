# FunctionFS Exact-Read Kernel Trace

This diagnostic kernel patch identifies the request boundary where a valid
12,800-byte FunctionFS bulk OUT read stops progressing. It is observability
only: it does not change endpoint ownership, request sizes, queueing, waits,
timeouts, retries, endpoint lifetime, or gadget lifecycle.

## Trace2 IRQ Boundary

The second diagnostic release is `6.12.47+rpt-rpi-v8-ffs-trace2`. It retains
the trace1 records and adds no transport behavior changes. Its DWC2 additions
are reads only: `dwc2_doeptsiz_program`, `dwc2_doeptsiz_readback`,
`dwc2_irq_enter`, `dwc2_irq_oepint_check`, `dwc2_irq_daint`,
`dwc2_epint_dispatch`, `dwc2_epint_enter`,
`dwc2_epint_xfercompl_check`, and one
`dwc2_request_delayed_snapshot` scheduled about 5 ms after a tracked request
starts. The delayed work first verifies that the exact request is still active;
it does not clear or write registers, complete/cancel a request, wake
FunctionFS, retry, or recover the gadget. It can add a small delayed-work
scheduling cost only.

Trace2 FunctionFS emits `ffs_trace_provider_active` as each FunctionFS
instance is allocated, and `ffs_io_observed` for every OUT read before the
endpoint-1/12,800-byte detailed filter. The latter records the requested and
queued lengths, endpoint, request pointer, direction, and timestamp.

For the supplied v8 configuration, `usb_f_fs` is a module and DWC2 is built
in. Before a payload test, verify the trace2 release; verify
`usb_f_fs.ko` has trace2 vermagic and contains `ffs_trace_provider_active` and
`ffs_io_observed`; verify `vmlinux` contains `dwc2_irq_enter` and
`dwc2_request_delayed_snapshot`. Do not treat module provenance as proof of
the built-in DWC2 implementation.

## Source Provenance

The Pi runs the Raspberry Pi Debian package release `6.12.47-1+rpt1`, with
the installed kernel reporting `6.12.47+rpt-rpi-v8`.

The exact source inputs are available from the Raspberry Pi Debian archive:

```text
linux_6.12.47.orig.tar.xz
sha256 e82fe40871743048226987bd349ef107168b15aab90140e872ca4ed470922e25

linux_6.12.47-1+rpt1.debian.tar.xz
sha256 d028f5cfce9e8220fbdeae9d83d5a3c31bce7ec9fa4baafa805fb2fd9b1fde88
```

The Debian source package uses quilt and applies `debian/patches/rpi/rpi.patch`.
The prior diagnostic build used the saved configuration at
`../../.xdisp-kernel-build-6.12.47-gdma0/.config`, whose local version was
`+rpt-rpi-v8-xdisp-gdma0` and compiler was GCC 16.1.1.

## Rebuild

Use a clean work area, apply the Debian package patch series, apply
`test-only/functionfs-exact-read-kernel-boundary.patch`, and use a new output
directory. Do not overwrite the existing diagnostic build directory.

```bash
WORK=/tmp/linux-6.12.47-rpt
OUT=$PWD/.functionfs-kernel-build-6.12.47

sha256sum linux_6.12.47.orig.tar.xz linux_6.12.47-1+rpt1.debian.tar.xz
tar -xf linux_6.12.47.orig.tar.xz -C /tmp
tar -xf linux_6.12.47-1+rpt1.debian.tar.xz -C "$WORK"
cd "$WORK"
for patch_file in $(grep -v '^#\|^$' debian/patches/series); do
    patch -p1 --forward --batch -i "debian/patches/$patch_file"
done
patch -p1 --forward --batch \
    -i /path/to/gud-gadget/docs/test-only/functionfs-exact-read-kernel-boundary.patch

cp /path/to/.xdisp-kernel-build-6.12.47-gdma0/.config "$OUT/.config"
make O="$OUT" olddefconfig
make -j"$(nproc)" O="$OUT" Image modules
make O="$OUT" modules_install INSTALL_MOD_PATH=/path/to/stage
```

Before deployment, run `git diff --check` in application worktrees, hash the
Image, `vmlinux`, System.map, `.config`, and module archive, and retain the
build log.

## Trace Events

The patch emits `gud_ffs_trace` records only for bulk OUT endpoint 1 requests
whose length is 12,800 bytes. Pointer values correlate one request within one
boot; they are not stable across boots due to allocation and address
randomization.

FunctionFS events:

```text
ffs_ep_queue_enter
ffs_ep_queue_return
ffs_wait_enter
ffs_wait_return
ffs_read_kernel_return
ffs_complete_enter
ffs_complete_wake
ffs_complete_exit
```

DWC2 events:

```text
dwc2_queue_enter
dwc2_queue_added
dwc2_start_req_enter
dwc2_start_req_return
dwc2_out_irq
dwc2_complete_enter
dwc2_giveback_enter
dwc2_giveback_return
```

All timestamps use `ktime_get_ns()`. DWC2 register snapshots are read-only and
include `DOEPTSIZ`, `DOEPCTL`, `DOEPINT`, `DOEPDMA`, `DAINT`, `DAINTMSK`,
`GINTSTS`, and `GINTMSK`.

## Deployment Gate

Do not install or activate this kernel while the receiver is `InFlight`,
`Poisoned`, or unknown. After qualified physical recovery, preserve prior
boot logs, confirm the receiver is `Idle`, then use a separately named kernel
image and one-shot `tryboot` configuration. The existing
`test-only/install-xdisp-p0.1-gdma0.sh` illustrates the required inactive
service and unattached-UDC install gate. A normal physical reboot rolls back
to the unchanged stock boot configuration.

# FunctionFS/DWC2 Trace2 Artifacts

Release: `6.12.47+rpt-rpi-v8-ffs-trace2`

Trace2 is observability only. It adds FunctionFS startup and pre-filter I/O
records, DWC2 `DOEPTSIZ` program/readback records, IRQ masking/dispatch
records, and one read-only delayed snapshot. It does not modify queueing,
waiting, completion, retry, recovery, or endpoint ownership.

## Verified Artifacts

```text
ecff1fac58bbc4ad84301679b1568afb0c04336c172d85b2bcbbe43a3f85b75a  Image
bee9603563554d826efb9091f69ff584249e568c9e555c7ca10e040ba27015d1  vmlinux
d6f3b40247d22a0c55fed7ade440d86b1a92f7f1afdb7c22b9ab40aa88293fad  System.map
4c7b0af931e3e257f40368a7cff87af43693f55b5079dc2564235a362c02880a  .config
1182c3270245b646a3d562d39f41ed32d5c958f1017d21284d2ca05f21729d51  functionfs-ffs-trace2-modules.tar.gz
```

The image and `vmlinux` are ARM64. The configuration has
`CONFIG_USB_DWC2=y`, `CONFIG_USB_CONFIGFS_F_FS=y`, and `CONFIG_USB_F_FS=m`.
`vmlinux` contains the DWC2 trace2 strings. The staged `usb_f_fs.ko` contains
`ffs_trace_provider_active` and `ffs_io_observed` and has trace2 vermagic.

## Deployment Gate

1. Confirm the receiver is `Idle`, no payload is active, and UDC is safe to
   detach.
2. Stop the service only from that state and confirm UDC is detached.
3. Install Image, modules, config, and System.map under the trace2 release.
4. Build the trace2 initramfs and use a one-shot boot selection.
5. After boot, verify `uname -r`, artifact hashes, and both built-in/module
   instrumentation before starting the receiver.
6. Send exactly one 12,800-byte payload. If it hangs, preserve evidence and
   require physical recovery; do not send a second request or teardown action.

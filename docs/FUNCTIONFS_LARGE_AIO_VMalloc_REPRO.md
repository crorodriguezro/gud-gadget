# FunctionFS large-AIO vmalloc/chunk qualification

This record packages the already-qualified Raspberry Pi FunctionFS transport
change. It is a test/qualification artifact, not a new admission design.

## Exact source delta

Apply `test-only/functionfs-large-aio-vmalloc.patch` to the source tree whose
`drivers/usb/gadget/function/f_fs.c` hash is:

```text
baseline (xfer size trace): 9a365e11c0941712295561bd5e3770a43ab71c9e06fadee879e3d192baef0342
qualified result:            9d9d0dc7dcc05206123c9ad9acaf32fb78356fdf206bdf2d0faf101d954c7cad
```

The patch keeps the SG path unchanged. When SG is unavailable and the logical
request exceeds 16 KiB, it allocates one vmalloc-backed logical buffer and one
sequential contiguous kmalloc DMA chunk. The completion callback copies the
completed chunk, queues the next chunk on the same FunctionFS I/O, and wakes
the original waiter only after the logical request completes. Small requests
and SG-capable requests retain their existing paths.

## Rebuild recipe

Use the captured Pi configuration, a fresh output directory, and the exact
kernel source tree used for the qualified module:

```bash
SRC=$PWD/.xfercompltrace-source
OUT=$PWD/.functionfs-kernel-build-6.12.47-repro
STAGE=$PWD/.functionfs-kernel-stage-6.12.47-repro
CONFIG=/tmp/pi-active.config

sha256sum "$SRC/drivers/usb/gadget/function/f_fs.c" "$CONFIG"
install -m 0644 "$CONFIG" "$OUT/.config"
make -C "$SRC" O="$OUT" ARCH=arm64 olddefconfig
make -C "$SRC" O="$OUT" ARCH=arm64 -j"$(nproc)" Image modules
make -C "$SRC" O="$OUT" ARCH=arm64 modules_install INSTALL_MOD_PATH="$STAGE"
sha256sum "$OUT/arch/arm64/boot/Image" \
  "$OUT/drivers/usb/gadget/function/usb_f_fs.ko" \
  "$OUT/.config"
```

The qualified configuration hash is
`468663f1cf02031078f90e1fd89c2a7b4b044b2e75fb180fdeb746483b3bf55e`.
The target kernel release is `6.12.47+rpt-rpi-v8-ffs-xfercompltrace`.
Before any deployment, confirm the built module's vermagic and hash against
the staged compressed module, and require the receiver to be `Idle` with the
UDC detached. Do not install while the receiver is `InFlight`, `Poisoned`, or
unknown.

## Accounting rule

Chunk counts are request counts, so use ceiling division. A 1,843,200-byte
payload is 112 full 16 KiB requests plus one 8,192-byte tail: 113 requests.
A full 3,686,400-byte frame is exactly 225 requests. The logical FunctionFS
completion remains one completion per AIO in both cases.

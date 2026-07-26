# XDISP-P0.1 `g_dma=0` diagnostic artifacts

These files reproduce the completed Raspberry Pi DWC2 slave/PIO isolation.
They are retained for auditability and are not a product kernel or deployment
recommendation.

- `xdisp-p0.1-gdma0.patch` is the one-line Broadcom DWC2 source change plus
  exact source/build hashes.
- `xdisp-p0.1-gdma0-tryboot.txt` selects separately named diagnostic kernel
  and initramfs files through Raspberry Pi's one-shot `tryboot` mechanism.
- `install-xdisp-p0.1-gdma0.sh` verifies upload hashes and installs the test
  artifacts without replacing the stock kernel, modules, `config.txt`,
  `kernel8.img`, or `initramfs8`.

The test booted as `6.12.47+rpt-rpi-v8-xdisp-gdma0` with `g_dma=0` and
`g_dma_desc=0`. Its first normal 16,274-byte LZ4 laptop payload failed:
usbmon proved the host completed the full URB, but FunctionFS returned 3,986
bytes and DWC2 retained the exact 12,288-byte remainder. The service entered
`Poisoned` and was physically recovered without teardown.

The one-shot rollback succeeded. The Pi returned to stock
`6.12.47+rpt-rpi-v8`, `g_dma=1`, the service disabled/inactive, and the UDC
detached. Do not reinstall or repeat this kernel unchanged. See
`../XDISP-P0.1-FUNCTIONFS-REBIND-TEST.md` for the complete result and safety
procedure.

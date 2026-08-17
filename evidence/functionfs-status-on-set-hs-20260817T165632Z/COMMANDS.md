# Material controlled-run actions

- Started Pi kernel/userspace journal followers and phone `dmesg -w` before
  USB exposure. Phone usbmon was unavailable because `CONFIG_USB_MON` is not
  set in the installed phone kernel.
- Forced the documented phone platform controller node to `host`.
- Started the guarded Pi service while USB data was disconnected.
- After the user connected USB data, performed enumeration-only checks.
- The first poll saw only phone root hubs. With no receive active, performed
  the single documented controller recovery cycle `device -> host`.
- Verified `1d50:614d`, 480 Mbps, EP1 OUT bulk, and 512-byte max packet.
- Loaded only `/home/phablet/xdisp-artifacts/gud-qualified-6bcf3a98.ko`
  with `bulk_timeout_ms=3000 bulk_trace_limit=1 xdisp_payload_timing=Y
  xdisp_probe_payload_length=12800 xdisp_probe_zero_packet=N`.
- Restored the qualified volatile tool `/tmp/gud-kms-stage` after the phone
  reboot and verified its hash.
- Ran exactly once, without warm-up or retry:

  `timeout 10s /tmp/gud-kms-stage atomic-commit /dev/dri/card1`

  Result: `stage=atomic-commit complete`, `PAYLOAD_RC=0`.
- Captured state without teardown. Both receive guards had returned to Idle;
  the guarded service then completed its designed one-shot clean shutdown.
- After the user physically disconnected USB data, unloaded the unbound
  qualified phone module and stopped only the temporary capture processes.

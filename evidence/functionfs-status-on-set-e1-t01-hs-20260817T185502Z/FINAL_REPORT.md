# E1-T01 trailing-status drain hardware result

## Result

E1-T01 passed its OnePlus 6 / Raspberry Pi Zero 2 W hardware gate with the
commit-qualified `gud-gadget` source at
`cea99422ebd5323f35f0343d01eee82a8f91648e`.

The host issued one protocol-valid compressed SET_BUFFER for a 640x6 XRGB8888
rectangle: 15,360 uncompressed bytes and exactly 12,800 bulk bytes. The Pi
accepted one exact native FunctionFS AIO request before returning the initial
STATUS_ON_SET status. The host URB, DWC2 request, FunctionFS completion,
decompression, framebuffer copy, and processing all completed exactly once.

The exact-AIO state and outer receive guard both returned to Idle before
teardown. The service then remained attached long enough to answer the two
host cleanup statuses: display-disable and controller-disable. Both returned
OK. Only after the second cleanup status did the service emit
`functionfs_status_on_set_trailing_status_drained`, unbind the UDC, remove
FunctionFS, and exit successfully.

The phone's marked acceptance window contains no status-request failure,
post-success `-71`, bulk failure, atomic-update failure, kernel warning, Oops,
or panic. The Pi pstore is empty and the current-boot kernel log contains no
DWC2 stop timeout or kernel fault.

## Qualified inputs

- Source commit: `cea99422ebd5323f35f0343d01eee82a8f91648e`
- Pi binary SHA-256:
  `0fe550c16c9501afb8818df22d78197b635de0575a6d84eede969501e3ab3fc4`
- Pi kernel: `6.12.47+rpt-rpi-v8-ffs-xfercompltrace`
- Phone kernel: `4.9.112-g6b190d86b`
- Host module parameter: `xdisp_probe_payload_length=12800`
- Dynamic USB path: `1-1.3`, high-speed / 480 Mbit/s
- Pi service restart policy: `Restart=no`

## Exact acceptance counts

- valid SET_BUFFER: 1
- accepted exact AIO submission: 1
- initial STATUS_ON_SET success: 1
- host bulk submission/completion: 1 / 1, actual 12,800
- exact userspace completion: 1
- payload completion: 1
- framebuffer-processing completion: 1
- post-payload cleanup statuses: 2, both OK
- trailing-status-drained marker: 1
- second SET_BUFFER accepted: 0
- poison transitions: 0
- drain timeouts: 0

## Final state

- Pi service inactive, MainPID 0, Result=success, ExecMainStatus=0
- Pi UDC `not attached`; configfs gadget removed
- Pi pstore empty
- Phone controller remains in host mode
- GUD DRM card removed after clean gadget detach
- Diagnostic host module remains loaded but has no live GUD device

The earlier attempt using `xdisp_probe_payload_length=0` is not acceptance
evidence; it intentionally remains in the historical logs. It exposed the
need to drain both chained disable statuses and led to commit `cea9942`.

See `VALIDATION.txt` for the machine-readable assertions and the unmodified
full logs for raw evidence.

# FunctionFS STATUS_ON_SET high-speed hardware result

## Result

The first physical STATUS_ON_SET transaction passed the safety-critical and
transport acceptance gates on the Raspberry Pi Zero 2 W DWC2 gadget and the
OnePlus 6 Linux 4.9 host.

The host checked and committed a 1280x720 state with GUD format `128`
(XRGB8888). It then issued exactly one valid compressed SET_BUFFER for a
640x6 rectangle: 15,360 uncompressed bytes and exactly 12,800 bulk bytes.
The Pi entered InFlight, submitted the exact native FunctionFS AIO request,
and received kernel acceptance before returning GET_STATUS=OK.

The host submitted and completed one 12,800-byte bulk URB with status 0 and
actual length 12,800. DWC2 programmed 12,800 bytes as 25 512-byte packets,
reported EP1 XFERCOMPL, zero remaining bytes and packets, and gave back status
0 / actual 12,800. FunctionFS woke and returned exactly 12,800. The native AIO
transaction and outer containment guard both returned to Idle in about 2 ms;
normal decompression, framebuffer copy, `payload_completed`, and
`framebuffer_processing_completed` markers followed.

The guarded one-shot service then exited before a second SET_BUFFER, cleanly
unbound and removed FunctionFS, and left the UDC `not attached`. After the
user physically disconnected USB, the qualified phone module was unloaded.

## Exact counts

- one valid SET_BUFFER;
- one accepted 12,800-byte AIO submission;
- one STATUS_ON_SET success after that acceptance;
- one host bulk submission and one successful host completion;
- one DWC2/FunctionFS completion chain;
- one exact 12,800-byte userspace AIO completion;
- one payload and framebuffer-processing completion;
- zero second SET_BUFFERs and zero Poisoned transitions.

## Health and final state

- No transaction-window kernel Oops, panic, allocator warning, or DWC2
  timeout was found.
- Pi pstore contained zero files.
- Pi: trace kernel still booted, service inactive, MainPID 0, Restart=no, UDC
  `not attached`, configfs gadget absent.
- Phone: USB physically disconnected, GUD module unloaded, `card1` absent.

## Observed limitations and remaining risks

- The phone kernel has no usbmon support (`CONFIG_USB_MON` unset). Host URB
  submission/completion evidence therefore comes from the qualified module's
  deterministic trace, correlated with the Pi DWC2 and FunctionFS trace.
- Immediately after the one-shot Pi intentionally detached, the host logged
  `GUD status request failed: -71` and then `GUD disconnected`. This occurred
  after the successful bulk and complete Pi processing, caused no retry or
  second payload, and is consistent with the deliberately abrupt one-shot
  boundary. A production multi-frame lifecycle must not use that boundary.
- The known Pi `dirty_framebuffer not supported or failed` debug marker
  remained, but framebuffer processing completed and no kernel failure
  followed.
- Enumeration required one documented OnePlus `device -> host` controller
  cycle. The initial connected state exposed only root hubs.
- This validates one exact 12,800-byte high-speed physical transaction. It
  does not yet validate sequential physical payloads, payloads above 12,800,
  cancellation/dequeue, production multi-frame lifecycle, or recovery from a
  physically stalled DWC2 request. Those cases remain covered only by offline
  tests or remain explicitly unsafe for this hardware.

See `VALIDATION.txt` for the direct assertions and the unmodified `*.log`
files for raw evidence.

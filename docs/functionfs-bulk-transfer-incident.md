# FunctionFS bulk-transfer incident: AIO completion failure

## Scope

This document records the failure observed when a OnePlus 6 (Linux 4.9 GUD
host driver) sent framebuffer data to the Raspberry Pi GUD gadget. It covers
the two separate FunctionFS issues found during diagnosis, the startup race
that made testing unreliable, and the resulting fixes.

## User-visible symptom

The GUD device enumerated successfully on the OnePlus, including a completed
probe for USB ID `1d50:614d`. The first atomic framebuffer update then timed
out on the host:

```
gud ... GUD atomic update failed: -110
```

The Pi had accepted the `GUD_REQ_SET_BUFFER` control request, but the bulk OUT
payload never completed through the previous receive path.

## Original issue: native AIO on a FunctionFS bulk endpoint

`usb-gadget`'s `EndpointReceiver` uses its native asynchronous-I/O driver for
endpoint reads. On the Pi's DWC2 FunctionFS setup, the completion result was
invalid (reported as raw OS error `521728`) even though kernel tracing showed
the controller receiving and completing 512-byte bulk packets. In effect, the
USB controller and FunctionFS endpoint were moving data, but the userspace AIO
abstraction did not deliver a usable completion to `gud-drm`.

This is why enumeration worked: descriptor and control transfers use ep0. The
failure appeared only when the host started sending framebuffer bytes to the
bulk OUT endpoint.

## Next issue: reopening an enabled endpoint blocks

The first workaround changed the GUD receiver to use a blocking `File::read`.
It initially opened `/dev/ffs-usb-gadget0-0/ep1` for every receive path.
That did not work either: on this FunctionFS/DWC2 combination, opening a
second file handle for an enabled endpoint can block indefinitely. The host
then again timed out with `-110`, before the first blocking read could begin.

The endpoint had already been opened during FunctionFS initialization by
`usb-gadget`; the correct fix was to use that exact file handle rather than
opening the endpoint path again.

## Fixes

1. Vendor the pinned `usb-gadget` dependency and add
   `EndpointReceiver::file()`. It returns an `Arc<File>` for the initialized
   FunctionFS endpoint. Keeping the receiver alive also keeps the endpoint
   state and file lifetime valid.
2. In `PixelDataEndpoint`, retain that initialized endpoint file and perform
   synchronous, bounded 512-byte `read()` calls until the GUD payload length
   has been received. This bypasses the unreliable native-AIO completion path
   and avoids the second-open deadlock.
3. Bind the USB gadget only after DRM has successfully acquired the CRTC,
   allocated/mapped framebuffers, and rendered the waiting screen. A temporary
   DRM permission/ownership failure previously exposed a half-started gadget
   and then tore it down while the host could be enumerating it. CRTC setup is
   retried before USB is exposed.
4. Keep detailed diagnostics around FunctionFS reads and frame processing so
   future regressions distinguish control, bulk, copy, scale, and flush work.

## Hardware verification

After rebooting the Pi, deploying the rebuilt binary, and forcing a fresh
OnePlus USB-device-to-host transition:

- the OnePlus freshly probed the GUD device;
- the Pi logged CRTC setup before USB binding;
- the Pi received every framebuffer tile as 512-byte bulk packets;
- representative 64,000-byte tiles completed in 5--11 ms;
- the Pi rendered the full 1280x720 update; and
- the OnePlus produced no new `GUD atomic update failed: -110` message.

The Pi's `frame_stats` entries are the authoritative evidence that payloads
were received, copied/scaled, and presented rather than merely enumerated.

## Future improvements

- Upstream a minimal endpoint-file accessor or a supported synchronous receive
  mode to `usb-gadget`, replacing this local vendor copy when accepted.
- Add a hardware-in-the-loop regression test that sends a small known frame
  through FunctionFS and asserts both GUD completion and Pi frame statistics.
- Add an explicit FunctionFS/DWC2 capability probe or configuration flag so
  platforms with working AIO can opt into it deliberately.
- Add a bounded read timeout/cancellation strategy. The current blocking read
  is reliable on the tested Pi, but an unplug during a payload should be
  surfaced promptly and trigger a controlled gadget restart.
- Investigate DRM dirty-framebuffer support on the Pi. It currently falls back
  to back-buffer swaps after `Function not implemented`, which is correct but
  less efficient than an available damage/flush path.
- Record transfer latency and error counters as metrics, not only debug logs,
  to make field diagnosis easier.

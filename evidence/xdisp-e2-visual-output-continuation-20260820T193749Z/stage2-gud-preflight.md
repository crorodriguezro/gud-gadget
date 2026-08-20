# Stage 2 - direct GUD pattern preflight and containment

After Stage 1 released DRM master, normal boot configuration was restored:

```sh
sudo systemctl enable gud-userspace.service
sudo systemctl start gud-userspace.service
```

The service was `enabled` and `active`; PID `1241` (`gud-drm`) correctly owned
VC4 DRM master, and the UDC was bound to `3f980000.usb`. Its normal production
configuration remained unchanged:

```text
GUD_RECEIVE_MODE=status-on-set-aio
GUD_TEST_COMPRESSION=none
GUD_TEST_MAX_BUFFER_SIZE=12800
GUD_TEST_OUTPUT_MODE=1280x720
GUD_TRANSFER_FORMAT=xrgb8888
GUD_FRAME_DUMP_MODE=disabled
GUD_PATTERN_MODE=off
```

The intended direct host test was the pre-existing phone-side
`/home/phablet/gud-kms-fill /dev/dri/card1`. Its source paints deterministic
eight-band RGB565 color bars, modesets the direct GUD connector at 1280x720,
and holds the image for 30 seconds. It was **not run** because the receiver had
already contained itself during automatic host initialization:

```text
19:35:29.117646 received and validated GUD SET_BUFFER:
  x=0 y=0 width=1280 height=1 length=5120 expected_bulk_bytes=5120
19:35:29.117741 receiver_state=Arming
19:35:29.117963 exact AIO accepted, state=InFlight
19:35:29.118066 accepted valid SET_BUFFER and armed exactly one AIO request
19:35:29.118282 successful status sent after exact request acceptance, status=0
19:35:29.118690 rejected SET_BUFFER outside the bounded STATUS_ON_SET
  diagnostic admission window (transaction_limit=0)
19:35:29.119068 sent GUD status, status=1
19:35:29.119121 exact FunctionFS AIO transaction poisoned:
  expected_bytes=5120, reason="invalid GET_STATUS result after arm"
19:35:29.119156 receiver_state=Poisoned
19:35:29.119183 Invalid status transition
```

This happened before any intentional Stage 2 payload. The service remained
active and the UDC remained bound, but the receiver's explicit terminal
`Poisoned` state prevents retry. Containment policy therefore prohibited a
service stop/restart, GUD payload retry, UDC unbind, FunctionFS manipulation,
or a higher-level Lomiri test.

The exact unresolved boundary is:

```text
normal GUD USB re-enumeration
  -> automatic 1280x1 host setup
  -> valid exact AIO arm
  -> extra SET_BUFFER / GET_STATUS=1
  -> receiver containment before a direct color-bar payload
```

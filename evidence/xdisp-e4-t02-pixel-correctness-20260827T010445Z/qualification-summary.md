# E4-T02 hardware qualification (2026-08-27)

## Deterministic reference

The canonical source is `reference/reference-pattern.rgb565`: 1280x720,
RGB565, little-endian, pitch 2560, 1,843,200 bytes.  Its SHA-256 is recorded
in `reference/reference-sha256.txt`; `reference/reference-pattern.png` is the
decoded visual reference.  The generator and unit tests are copied into the
reference directory.

## Live path exercised

After HDMI reconnection and the documented OnePlus `device -> host` role
cycle, the dynamic gadget identity was found at `1-1.3` (`1d50:614d`).  The
phone exposed `/dev/dri/card1` and the Pi reported UDC `configured` with HDMI
`connected`.  The normal managed path reached `active` at the E4-T01 mode
contract `e4c1-2c5242af7c0e3ebe`.

An isolated checkerboard qualification then ran through the deployed
`mirgud-direct-rgb565-13818a0.bin` using `--size 1280 720 --pixel-format
rgb565`, one generated frame per second for eight seconds.  This is the real
DRM framebuffer -> GUD -> USB FunctionFS -> Pi VC4/HDMI path (not a Pi-local
pattern).  The final report records 9 submitted and 9 presented frames,
`max_pending_observed=1`, `max_in_flight_observed=1`, zero drops, zero
conversion failures, zero GUD submit failures, and `accounting_ok=true`.

The Pi accepted 1,103 full-frame SET_BUFFER transactions in the surrounding
clean session; the sampled checkerboard interval has no processing failures,
timeouts, short/invalid reads, or poisoned transactions.  Its telemetry stayed
`aggregate_state=Idle`, `aio_state=Idle` after every completion.

Raw command and journal captures are in `pattern/phone-checkerboard.log` and
`pattern/pi-checkerboard.log`.  The operator observed the live HDMI output:
the blue/white checker pattern and the external-monitor content were both
visibly correct, with no reported crop, squeeze, shift, or channel-order
defect.  Physical marker appearance is therefore PASS for this session.

## Diagnostic note

The earlier per-row-INFO run was intentionally retained under `pi/` and
`gud/`: it overloaded the receiver diagnostic path and produced `-110` after
successful frames.  It is not used as a healthy-path qualification result.

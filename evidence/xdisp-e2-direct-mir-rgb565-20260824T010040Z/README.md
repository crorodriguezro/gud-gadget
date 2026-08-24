# Direct Mir RGB565 qualification

This evidence package qualifies the direct Mir RGB565 path at 1280x720 and
compares RAW with LZ4 against the existing XRGB8888 full-frame baselines.

The experiment is test-only. Mir, Lomiri, and production defaults are not
modified. The managed daemon is the current `xdispd`; its child arguments are
rewritten only by a uniquely staged test wrapper so the managed path can
request `--source-pixel-format rgb565 --pixel-format rgb565`.

## Result

Task: **PASS for direct RGB565 capability and bounded transport qualification.**
The deployed, unmodified Mir 1.8.3 Android2 screencast returned true packed
RGB565 buffers. The source-only run and the physical RGB565 control passed.
The real managed RGB565+LZ4 run presented 677 frames in 29.927901 seconds
(22.62 fps), with zero transport failures, poison transitions, timeouts, or
ownership ambiguity. RAW presented 542 frames in 29.834571 seconds (18.16
fps), so RAW RGB565 alone did not reach the 20 fps target.

The managed daemon was the real xdispd path. Because the checked-in production
child currently fixes its arguments to XRGB8888, a uniquely staged private
wrapper rewrote only those child arguments to `--source-pixel-format rgb565
--pixel-format rgb565`; Mir, Lomiri, and production source files were not
modified.

## Required result fields

The final report records source format/stride, logical frame size, direct-copy
status, one-admission accounting, RAW/LZ4 payload metrics, E1 safety state,
physical inspection, and restoration status. Raw command output and hashes are
kept in the subdirectories below.

## Baselines

- XRGB8888 RAW: 1280x720, 3,686,400 logical bytes, one admission/frame;
  historical E1 result approximately 9 fps.
- XRGB8888 LZ4: 1280x720, 3,686,400 logical bytes, one logical frame update;
  prior full-frame result 13.27 presented fps with zero transport safety
  failures.

The comparison uses the existing full-frame evidence, not the earlier
small-tile LZ4 evidence.

## Expected RGB565 accounting

RGB565 at 1280x720 is 1,843,200 logical bytes. With a 16,384-byte internal
FunctionFS read size this is 112 full chunks plus one 8,192-byte tail, or 113
internal USB requests inside one logical GUD ownership unit.

## Evidence status

Runtime cleanup is complete. The Pi and phone are restored to production-safe
settings. `SHA256SUMS` is generated after all evidence files are written.

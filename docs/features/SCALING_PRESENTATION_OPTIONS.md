# Scaled-Mode Presentation Options

## Summary

Lower-resolution modes currently use a software scaling path on the phone. The source frame is copied into a shadow buffer, scaled to the native panel size, and written directly into the live scanout buffer. That works functionally, but it causes visible tearing and short black flashes when the phone is showing a scaled mode.

This document describes the issue, the measured cost, the implementation options, and the recommended fix.

## Current Behavior

In native mode, the driver writes directly into the panel-sized framebuffer and flushes the updated rectangle.

In scaled mode, the driver:

1. copies the host frame into a source-resolution shadow buffer
2. clears the native framebuffer to black
3. rescales the shadow buffer into the native framebuffer
4. flushes the native framebuffer

The problem is that step 2 and step 3 happen on the same dumb buffer that the panel is currently scanning out. If the display reads the buffer while the CPU is still repainting it, the user sees:

- tearing
- black squares
- short black flashes

## Root Cause

The issue is a presentation/synchronization problem, not a USB transport problem.

- The phone is rewriting the front buffer while it is visible.
- The scaler redraws the full native frame in scaled mode.
- The code black-fills the destination before drawing the scaled image, so partially rendered frames show black regions.

This is separate from the `dma-buf-overhaul` branch. That branch focuses on USB receive efficiency. It does not add a proper back-buffer or page-flip presentation path.

## Measured Cost

Recent live numbers at `900x1900` scaled mode:

- `recv_ms`: `12..15`
- `decompress_ms`: `3..5`
- `copy_ms`: `1..2`
- `scale_ms`: `54..66`
- `total_ms`: `79..88`

Implications:

- in scaled mode, the software scaler is the dominant phone-side cost
- the current path is roughly limited to about `11..12 fps` even before host-side pacing
- copy and flush are small compared with scaling

Approximate memory usage:

- one native `1080x2280 RGB565` scanout buffer: about `4.70 MiB`
- one shadow buffer:
  - `900x1900`: about `3.26 MiB`
  - `810x1710`: about `2.64 MiB`
  - `720x1520`: about `2.09 MiB`

Adding a second native scanout buffer for proper double-buffering would cost about `+4.70 MiB`.

## Options

### 1. Quick Mitigation

Stop black-clearing the live framebuffer before each scaled redraw, or reduce how much of the panel is explicitly cleared.

Pros:

- very small code change
- likely reduces the black flashes immediately
- negligible extra memory cost

Cons:

- does not really fix tearing
- panel can still scan partially rendered frames
- only hides the most visible symptom

Use when:

- a temporary improvement is needed before a real presentation fix

### 2. Double-Buffered Scaled Scanout with Page Flip

Keep two native panel-sized dumb buffers:

- front buffer: currently scanned out
- back buffer: CPU writes the fully scaled frame here

After the scaled frame is complete, flip the CRTC to the back buffer and swap roles.

Pros:

- fixes tearing and black flashes at the actual cause
- does not materially increase scaling CPU cost
- keeps the existing shadow-buffer scaling architecture
- visually correct solution

Cons:

- adds about `+4.70 MiB` of memory
- requires buffer lifecycle and flip handling changes
- needs careful fallback behavior if page-flip or atomic swap is not available on the current DRM path

Use when:

- correctness is the priority
- current scaling behavior should remain, but presentation must be stable

### 3. Back Buffer Plus Final Copy Into the Front Buffer

Scale into a second temporary native buffer, then memcpy the whole native frame into the live scanout buffer.

Pros:

- simpler than a true page-flip design in some DRM setups
- avoids black-clear flashes during scaling itself

Cons:

- still writes the visible buffer while scanout is active
- adds one more full native-frame copy per update
- higher CPU cost than today
- weaker than a real flip-based solution

Use when:

- page-flip integration is temporarily blocked
- a short-term intermediate step is acceptable

### 4. Full Presentation Refactor

Refactor scaled-mode presentation around an explicit front/back scanout model, likely with more formal DRM atomic/page-flip handling and cleaner state ownership.

Pros:

- strongest long-term architecture
- best base for future scaling improvements
- best base if more advanced composition or asynchronous presentation is added later

Cons:

- biggest code change
- highest testing cost
- slower path to a practical fix

Use when:

- broader DRM presentation work is already planned

## Recommendation

The recommended fix is **Option 2: double-buffered scaled scanout with page flip**.

Reason:

- it fixes the real bug instead of hiding it
- it should not materially increase CPU cost versus the current scaled path
- the extra cost is mainly memory, and the added memory is modest for this device

This is the best tradeoff between correctness, complexity, and performance.

## Expected Performance Impact

If Option 2 is implemented correctly:

- scaling CPU cost should stay roughly where it is today
- visible tearing/black flashes should disappear
- perceived smoothness should improve
- memory usage should increase by about one extra native framebuffer (`+4.70 MiB`)

What should be avoided:

- scaling into a temporary buffer and then copying the full frame into the front buffer

That design adds extra CPU work without fixing presentation as cleanly.

## Acceptance Criteria

The feature is complete when all of these are true:

- scaled modes no longer show black squares or black flashes
- scaled modes no longer visibly tear during normal use
- native mode still behaves as before
- `frame_stats` still work in scaled mode
- phone memory growth is consistent with one additional native buffer
- laptop host still binds and drives the display normally

## Notes

- This issue is about phone-side presentation, not host-side USB transfer.
- `dma-buf-overhaul` may still be useful later for transport efficiency, but it is not the right first fix for this bug.

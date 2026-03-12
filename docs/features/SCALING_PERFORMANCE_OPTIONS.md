# Scaled-Mode CPU Cost and Optimization Options

## Summary

Lower-resolution modes currently work by scaling the host frame in software on the phone. That path is correct functionally, but it is CPU-heavy. This document records why the current scaler is expensive, what parts of the implementation are responsible, and which optimizations are worth doing next.

## Current Cost

Recent live numbers at `900x1900` scaled mode:

- `recv_ms`: `12..15`
- `decompress_ms`: `3..5`
- `copy_ms`: `1..2`
- `scale_ms`: `54..66`
- `total_ms`: `79..88`

Interpretation:

- scaling is the dominant phone-side cost
- the scaler is roughly `65..80%` of total frame handling time in scaled mode
- the current phone-side path is effectively capped near `11..12 fps` before additional host-side pacing is considered

## Why It Is Expensive

The current scaler in `drm/src/main.rs` does all of the following on the CPU for each scaled frame:

1. clears the whole native panel framebuffer to black
2. loops over every destination pixel
3. computes source coordinates with integer division inside the inner loop
4. copies each `RGB565` pixel as two separate byte stores
5. writes directly into the visible scanout buffer

That means the current path is doing a full-panel memory pass just for clearing, then another full-panel pass for scaling, while also paying per-pixel arithmetic overhead.

At native `1080x2280`, the destination surface has `2,462,400` pixels. Even when the source mode is lower, the destination work still covers almost the full panel.

## Main Hot Spots

### 1. Per-pixel integer division

The current inner loop computes:

- `src_x = dst_x_rel * src_width / layout.dst_width`
- `src_y = dst_y_rel * src_height / layout.dst_height`

Division in the inner loop is expensive and repeated for every destination pixel.

### 2. Full-frame black clear

The scaler clears the full native framebuffer before every redraw. That is one extra full native-frame write per scaled frame, even when the black borders do not change.

### 3. Byte-wise pixel stores

Each `RGB565` pixel is written as two separate `u8` stores instead of one `u16` write.

### 4. Full-frame rescale on every update

Even if the host only updates part of the source frame, the current scaled path redraws the full native destination image.

## Improvement Options

### 1. Precompute coordinate maps

Cache:

- destination `x -> source x`
- destination `y -> source y`

Pros:

- removes division from the hot path
- straightforward implementation
- large CPU win for low complexity

Cons:

- mode/layout change handling must rebuild the maps

### 2. Stop clearing the whole framebuffer every frame

Only clear the border areas when the scaled layout changes, or preserve the black bars once they are established.

Pros:

- removes one full native-frame memory pass
- low implementation complexity

Cons:

- requires slightly more layout state tracking

### 3. Use `u16` pixel writes

Treat `RGB565` pixels as 16-bit values in the scaling loop instead of two byte stores.

Pros:

- simpler and cheaper inner loop
- low risk

Cons:

- requires careful alignment-safe handling

### 4. Scale only the affected destination region

Map a dirty source rect into destination space and redraw only the needed destination area.

Pros:

- biggest CPU reduction for partial updates
- better fit for real desktop workloads

Cons:

- more complicated math and clipping
- more complicated border handling
- more testing required

### 5. SIMD / NEON acceleration

Use ARM NEON to accelerate row expansion and pixel copies.

Pros:

- strongest CPU win without changing behavior
- good long-term path if scaling stays in software

Cons:

- higher implementation complexity
- architecture-specific code
- more testing burden

### 6. Hardware-assisted scaling

Move scaling out of the CPU and into a hardware composition or rendering path.

Pros:

- best long-term performance potential
- reduces CPU pressure significantly

Cons:

- architectural change
- much larger implementation effort

## Recommended Optimization Order

1. Precompute `x/y` coordinate maps
2. Stop full-frame black clearing on every scaled frame
3. Switch the inner loop to `u16` writes
4. Add dirty-rect-aware scaling if CPU cost is still too high
5. Revisit NEON or hardware scaling only if software scaling remains a bottleneck

This order gives the best ratio of benefit to implementation complexity.

## Relation to Tearing Fix

Scaling performance and tearing are related but separate:

- the tearing/black-square bug is a presentation problem
- the high CPU cost is a scaling hot-path problem

The tearing fix should use a back buffer and page flip. That should not materially increase scaling CPU cost. The expensive part is already the scaler itself.

## Acceptance Criteria

Optimization work is successful when:

- scaled-mode visual behavior is unchanged
- `scale_ms` decreases materially in `frame_stats`
- native-mode behavior is unchanged
- laptop host compatibility is unchanged

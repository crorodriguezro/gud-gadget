# XDISP damage audit

Audit result for the current native Mir screencast -> GUD path:

```text
native damage status: NO_NATIVE_DAMAGE_FOR_THIS_PATH
DRM FB_DAMAGE_CLIPS status: NOT_PRESENT_IN_GUD_BACKPORT
software framebuffer diffing: NOT IMPLEMENTED
```

`mir-android2-platform-gud/src/utils/gud_screencast.cpp` consumes a complete
Mir screencast buffer through `mir_buffer_stream_get_graphics_region()`, copies
and converts the complete configured frame, then releases the buffer with
`mir_buffer_stream_swap_buffers_sync()`. The Mir C screencast/buffer-stream
interface used by this build supplies no damage rectangles to this consumer.
The presenter therefore submits complete frames; it does not infer damage by
comparing framebuffer contents.

The current `gud/backport-4.9` path constructs XDISP rectangles from its own
transport planner and has no `FB_DAMAGE_CLIPS`, `DRM_MODE_FB_DAMAGE_CLIPS`,
damage property blob, or atomic damage-property programming. There is no
native damage rectangle available to forward.

No damage implementation is included in this task. If native damage becomes
available later, dropped-frame correctness must accumulate pending damage (or
force a full-frame fallback) before forwarding rectangles; the current
newest-frame presenter must not silently treat a dropped frame's partial damage
as a complete framebuffer.

E2-T04 is intentionally not started.

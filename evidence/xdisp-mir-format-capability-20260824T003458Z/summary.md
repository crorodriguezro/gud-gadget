# Audit result

1. Task: PASS
2. Mir version: 1.8.3 runtime libraries
3. Lomiri version: 0.5.0
4. Graphics backend: UBports Android2/libhybris/Android HAL gralloc
5. Defined Mir formats: invalid=0; abgr8888=1; xbgr8888=2; argb8888=3;
   xrgb8888=4; bgr888=5; rgb888=6; rgb565=7; rgba5551=8; rgba4444=9;
   sentinel mir_pixel_formats=10
   The local Android2 conversion map also references
   `mir_pixel_format_rgba_10101002`, but that symbol was not present in the
   deployed Mir enum listing used here and was not advertised at runtime; it
   remains an unresolved compatibility reference, not a verified format/value.
6. Current mirgud requested source format: `auto`; current production transport:
   XRGB8888. Auto selects advertised ABGR8888 (enum 1).
7. API: `mir_connection_get_available_surface_formats()` ->
   `mir_screencast_spec_set_pixel_format()` -> `mir_screencast_create_sync()`
   -> `mir_screencast_get_buffer_stream()` ->
   `mir_buffer_stream_get_graphics_region()`.

## Candidate results

| Requested | Enum | API | Frames | CPU map | Actual | Stride | Logical / mapped bytes | Result |
|---|---:|---|---:|---|---|---:|---:|---|
| XRGB8888 | 4 | rejected: not advertised | 0 | no | — | — | — | CLASS C |
| RGB888 | 6 | accepted | 255 | yes | RGB888 | 3840 | 2,764,800 / 2,764,800 | true packed delivery |
| RGB565 | 7 | accepted | 2434 | yes | RGB565 | 2560 | 1,843,200 / 1,843,200 | direct delivery |

8. XRGB8888: enum yes; API no; acquisition no; CPU mapping no; no returned
   format/stride; source-only frame benchmark is not applicable because this
   backend rejects the request. Current production receives ABGR8888 stride
   5120 and reorders to XRGB8888.
9. RGB888: enum yes; API yes; acquisition yes; CPU map yes; actual enum 6;
   stride 3840; logical bpp 3; backing bpp 3; changing content yes; source-only
   32 fps. Mirgud currently expands it to XRGB8888, so delivery is direct at
   the Mir buffer boundary but not direct through current mirgud transport.
10. RGB565: enum yes; API yes; acquisition yes; CPU map yes; actual enum 7;
    stride 2560; logical bpp 2; backing bpp 2; changing content yes;
    source-only 61 fps. Mirgud direct-copied all 2434 frames when transport
    RGB565 was selected.
11. Other useful formats: backend advertises ABGR8888, XBGR8888, RGB888, RGB565;
    it does not advertise ARGB8888, XRGB8888, BGR888, RGBA5551, or RGBA4444.
12. Lomiri itself restricts tested formats: no. It owns the Virtual output and
    server; format choice is Mir API/backend behavior.
13. Android/gralloc restricts tested formats: yes, by the backend's advertised
    list and HAL mappings. It accepts RGB888 -> HAL_PIXEL_FORMAT_RGB_888 and
    RGB565 -> HAL_PIXEL_FORMAT_RGB_565; XRGB8888 is not advertised.
14. Can Mir deliver RGB565 directly to mirgud: YES.
15. Can Mir deliver true packed RGB888 directly to mirgud: YES.
16. Does RGB888 save 25% mapped bytes: YES (2,764,800 vs 3,686,400).
17. Does RGB565 save 50% mapped bytes: YES (1,843,200 vs 3,686,400).
18. Does current mirgud support direct RGB565: YES, when transport is selected
    as RGB565; current production XRGB8888 still needs conversion.
19. Does current mirgud support direct RGB888: NO for transport; source-side
    plumbing exists, but current transport PixelFormat has no RGB888 variant.
20. If direct RGB565 delivery were unavailable, conversion would have to occur
    in mirgud after `MirGraphicsRegion` acquisition, before GUD submission.
21. If direct RGB888 transport is required, conversion/repacking would have to
    be addressed in mirgud and the host/GUD KMS path; the current mirgud path
    expands RGB888 to XRGB8888.
22. Best next transport experiment: DIRECT MIR RGB565.
23. Mir modified: NO.
24. Lomiri modified: NO.
25. Production transport changed: NO.
26. E2-T04: remains paused; not started.
27. Commits created: none.
28. Commits pushed: NO.
29. Final phone configuration: original deployed Mir/Lomiri/Android2 stack;
    kernel remains 4.9.112-g6b190d86b; temporary diagnostic process cleaned up.
30. Final Pi configuration: unchanged; no transport benchmark or Pi change.
31. Evidence path: this directory.

## Format decision matrix

RGB565 is CLASS A at the Mir buffer boundary and is the strongest candidate for
the next transport test. RGB888 is also true packed delivery (not a fake
32-bit allocation), but is CLASS E for current end-to-end mirgud transport
because mirgud/host transport lacks a direct RGB888 path. The current managed
ABGR8888 -> XRGB8888 path remains the production baseline.

Internal Mir compositor conversion versus direct rendering into the returned
buffer was not instrumented; classify that internal distinction as UNKNOWN.
The proven result is that Mir's deployed Android2 allocator accepted each
requested format and returned a CPU-mappable buffer of that exact packed size.

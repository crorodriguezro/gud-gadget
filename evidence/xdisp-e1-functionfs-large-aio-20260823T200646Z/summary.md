# E1 FunctionFS large-AIO allocation fix

1. Task:
   COMPLETE for E1 large-payload qualification. Source fix, physical recovery, enumeration, the 163,840 control, the 327,680 single/repeat gate, and the prescribed larger RAW ladder all passed.

2. Physical recovery performed:
   User completed the runbook topology: phone disconnected, hub and Pi depowered, hub powered first, Pi powered separately, Pi data port connected to the BENFEI fixed port, and hub upstream connected to the OnePlus OTG adapter. Enumeration then found Pi GUD `1d50:614d`.

3. Original 327,680 failure root cause:
   `ffs_epfile_read_iter()` -> `ffs_epfile_io()` -> `ffs_alloc_buffer()` selected `kmalloc(data_len, GFP_KERNEL)` because `gadget->sg_supported` was false.

4. Exact allocation request size:
   327,680 bytes.

5. Exact reported allocation order:
   order 7.

6. Why order:7 was required:
   327,680 / 4,096 = 80 pages; the buddy allocator rounds to 128 contiguous pages, which is order 7 (524,288 bytes).

7. DWC2 SG capability:
   UNAVAILABLE — runtime `using_dma=1`, `using_desc_dma=0`; Pi evidence decodes `GHWCFG4=0x1ff00020` with descriptor DMA absent.

8. FunctionFS SG capability before fix:
   yes — the vmalloc + `sg_table` implementation is present, but gated by the unavailable UDC descriptor-DMA capability.

9. Modern upstream behavior:
   Large FunctionFS buffers use vmalloc-backed pages and an `sg_table` when the UDC advertises SG support; this Pi cannot use that path.

10. Chosen fix:
    For large SG-unavailable I/O, retain one logical FunctionFS AIO but use one vmalloc backing store and sequential 16 KiB contiguous DMA chunks.

11. Files changed:
    `.xfercompltrace-source/drivers/usb/gadget/function/f_fs.c`; evidence files under this package. Existing GUD worktree changes were preserved.

12. Does fix preserve one logical GUD ownership:
    yes.

13. Does fix introduce multiple queued GUD payloads:
    NO. It introduces only internal USB request chunks under one FunctionFS/GUD logical payload.

14. 163,840 control:
    PASS — one atomic commit; phone bulk result 0, actual 163,840 bytes, elapsed 4,118 us.

15. 327,680 single transfer:
    PASS — one atomic commit; phone bulk result 0, actual 327,680 bytes, elapsed 8,240 us.

16. 327,680 actual completion bytes:
    327,680 after fix. Prior failed attempt had host-side actual 2,048 bytes with an ambiguous timeout.

17. 327,680 repeated qualification:
    PASS — 100/100 transactions returned `command_result=0`; remote runner result 0.

17a. Prescribed larger RAW ladder:
    PASS — 491,520, 655,360, 983,040, 1,310,720, and 1,843,200-byte direct transfers all completed exactly. Each larger rung also passed a bounded 10-transfer repeat gate; the previously completed 655,360 rung additionally passed 100/100.

18. short completions:
    0 in the post-fix phone evidence; all 100 repeat records report the configured payload length.

19. timeouts:
    0 in post-fix phone and Pi failure-marker checks.

20. Poisoned transitions:
    0 post-fix; Pi production telemetry reports `poisoned_transactions=0` and aggregate state `Idle`.

21. ambiguous accepted I/O:
    0 post-fix; Pi telemetry reports `aio_accepted=100`, `aio_completed=100`, and `currently_owned=false` for the repeat gate.

22. largest newly successful payload:
    1,843,200 bytes.

23. largest newly repeatedly-qualified payload:
    1,843,200 bytes, 10/10 bounded repeat.

24. first failing larger rung if any:
    None in the prescribed ladder through 1,843,200 bytes; full-frame 3,686,400-byte testing was intentionally not started.

25. managed RAW tested:
    no.

26. managed tx/frame:
    N/A.

27. managed FPS:
    N/A.

28. improvement vs 163,840 / 6.56 fps:
    N/A.

29. dominant bottleneck after fix:
    The prior order-7 contiguous allocation bottleneck is removed. The fixed path emitted `ffs_chunked_io` for every larger logical length with 16,384-byte chunks; final Pi checks found zero allocation-failure, timeout, or poison markers.

30. is previous 163,840 ceiling confirmed false:
    yes — 327,680 passed singly and 100x repeatedly.

31. exact next recommendation:
    CONTINUE PAYLOAD LADDER only if full-frame characterization is explicitly desired; otherwise retain 1,843,200 bytes as the qualified E1 ceiling for this run. Do not start E2-T04.

32. E2-T04:
    must remain paused; it was not started.

33. evidence path:
    `gud-gadget/evidence/xdisp-e1-functionfs-large-aio-20260823T200646Z/`

34. commits created/pushed:
    none.

35. final phone configuration:
    Dynamic GUD `1d50:614d`, experimental host module loaded with `xdisp_payload_limit=327680`, `/dev/dri/card1` present during probes, and 100 repeat transactions completed.

36. final Pi configuration:
    Unchanged from prior handoff: `gud-userspace.service` active, UDC configured on `3f980000.usb`, production environment with `GUD_TEST_MAX_BUFFER_SIZE=12800`, `GUD_TEST_COMPRESSION=none`, `GUD_RECEIVE_MODE=status-on-set-aio`, `GUD_TRANSFER_FORMAT=xrgb8888`, `GUD_FFS_READ_SIZE=16384`, 1280x720.

37. final Pi receiver state:
    Production telemetry ended with `set_buffer_seen=247`, `accepted_transactions=247`, `aio_accepted=247`, `aio_completed=247`, `processing_completed=247`, `processing_failed=0`, `poisoned_transactions=0`, `timed_out_transactions=0`, aggregate state `Idle`, `aio_state=Idle`, and `currently_owned=false`; `gud-userspace.service` is active, MainPID 1295, and UDC `3f980000.usb` is configured.

## Larger-rung qualification

| RAW XRGB8888 payload | Direct transfer | Bounded repeat | Evidence |
| ---: | :---: | :---: | :--- |
| 491,520 B | PASS, exact | PASS, 10/10 | `runtime-491520/`, `runtime-repeat-491520x10/` |
| 655,360 B | PASS, exact | PASS, 100/100 | `runtime-655360/`, `runtime-repeat-655360x100/` |
| 983,040 B | PASS, exact | PASS, 10/10 | `runtime-983040/`, `runtime-repeat-983040x10/` |
| 1,310,720 B | PASS, exact | PASS, 10/10 | `runtime-1310720/`, `runtime-repeat-1310720x10/` |
| 1,843,200 B | PASS, exact | PASS, 10/10 | `runtime-1843200/`, `runtime-repeat-1843200x10/` |

No short completion, timeout, Poisoned transition, or ambiguous accepted-I/O marker appeared in the new ladder runs. The full-frame rung was not attempted.

## Local validation

- arm64 out-of-tree `f_fs.o` build with the captured Pi configuration: PASS.
- `W=1` FunctionFS object build: PASS; only the kernel-doc script’s unrelated precedence warning appeared.
- GUD LZ4 tests: 0 failures.
- GUD LZ4 contract tests: 0 failures.
- shell syntax checks: PASS.
- repository diff checks: PASS.

See `source/` for the forensic call path and capability analysis, `build/validation.txt` for hashes, and `source/chosen-fix-rationale.txt` for the invariant-preserving design.

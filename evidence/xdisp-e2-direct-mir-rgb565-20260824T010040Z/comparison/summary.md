# Direct Mir RGB565 comparison

## Decision

Direct Mir RGB565 is usable on the deployed OnePlus 6 stack without an
intermediate XRGB8888 conversion. The source-only run delivered 1208/1208
frames with `source_mir_format=7`, `transport_format=rgb565`,
`conversion_path_current=direct-copy`, and zero RGB565 pack/expand or channel
reorder frames.

The managed RAW run passed transport safety but presented 542 frames in
29.834571 s (18.16 fps). The managed LZ4 run presented 677 frames in
29.927901 s (22.62 fps), clearing the 20 fps bounded target. Both runs used
one logical GUD admission per 1,843,200-byte RGB565 frame.

| Path | Logical bytes/frame | GUD admissions/frame | Presented FPS | Drops | Transport result |
| --- | ---: | ---: | ---: | ---: | --- |
| XRGB8888 RAW reference | 3,686,400 | 1 | ~9 | — | historical E1 full-frame baseline |
| XRGB8888 LZ4 reference | 3,686,400 | 1 | 13.27 | — | historical full-frame baseline |
| Direct Mir RGB565 RAW | 1,843,200 | 1 | 18.16 | 1,294 | safe, below target |
| Direct Mir RGB565 LZ4 | 1,843,200 | 1 | 22.62 | 1,160 | safe, meets target |

### Measured RGB565 transport metrics

The Pi `frame_stats` records captured 84 RAW frames and 467 LZ4 frames from
the bounded runs. RAW wire size was exactly 1,843,200 bytes/frame; `recv_ms`
was 50/52/53 ms p50/p95/max. LZ4 compressed wire size was 498,448/903,333/940,082
bytes p50/p95/worst observed, with compression ratios 3.70/3.70/1.96 p50/p95/
worst. The observed LZ4 Pi decompression cost was 4/13/14 ms p50/p95/max and
bulk receive time was 16/28/37 ms p50/p95/max.

The phone trace provided 14 matched RAW and 23 matched LZ4
`source_ready -> worker_end` samples. Their E2E latency was:

- RAW: 61.616/72.026/72.026 ms p50/p95/max.
- LZ4: 48.491/56.900/58.605 ms p50/p95/max.

mirgud does not expose a separate host LZ4-compression timer in this build.
Its available `submit_us` timing includes host compression, USB submission,
and downstream completion: RAW 100/100/100 ms p50/p95/max over 542 submits;
LZ4 50/100/100 ms p50/p95/max over 677 submits. Therefore a standalone host
compression p50/p95/max is **not instrumented**, and is not inferred from the
combined submit timing.

## Accounting and safety

At 1280x720 RGB565, one frame is 1,843,200 logical bytes. With the 16,384-byte
FunctionFS read size, the internal request shape is 112 × 16,384 plus one ×
8,192, or 113 requests inside one logical GUD admission. The Pi logs one
logical read per frame because userspace presents the full request to the
FunctionFS receiver; the 113 value is the internal USB-side chunk count.

Both managed runs ended with `aggregate_state=Idle`, `aio_state=Idle`, and
`currently_owned=false`. Ambiguous accepted I/O, short completions, poisoned
transitions, timeouts, and transport failures were all zero. The physical
RGB565 KMS-fill control also passed with exact 1,843,200-byte raw payloads.
HDMI operator visual confirmation was not observed in this session, so image
quality is recorded as unassessed rather than claimed.

## Recommendation

Use **DIRECT MIR RGB565 LZ4** as the next transport candidate and carry the
decision into E5-T03 apples-to-apples quality/workload qualification. Do not
add XRGB8888-to-RGB565 conversion. RGB888 is a true packed Mir source format,
but it has no direct current mirgud/GUD transport plumbing and is not the next
experiment. E2-T04 remains paused.

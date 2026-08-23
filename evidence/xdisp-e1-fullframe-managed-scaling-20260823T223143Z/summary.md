# E1 full-frame RAW and managed scaling

Task result: PASS.

The 1,843,200-byte control passed. One 3,686,400-byte 1280x720 XRGB8888
frame passed as one logical GUD payload, and the exact logical payload passed
100/100 repeat qualification. Host and Pi both completed exactly 3,686,400
bytes. The expected and derived internal FunctionFS chunk count is 225; zero
short/failed chunks, timeouts, poisoned transitions, and ambiguous accepted
I/O were observed.

Matched managed RAW runs proved 2 admissions/frame at 1,843,200 bytes and 1
admission/frame at 3,686,400 bytes. Both presented about 9 fps. The full-frame
E2E source-ready-to-worker-end distribution was 119.826/133.736/160.031 ms
(p50/p95/max), with about 36.5 MB/s effective decimal RAW USB throughput.
USB bulk transfer is therefore the dominant bottleneck; Pi processing was only
4 ms p50. GET_STATUS 5-second stall was not reproduced. LZ4 was not run.

E2-T04 remains paused. The Pi and phone were restored to matched production-safe
12,800-byte settings, with the receiver active/configured/Idle and no ownership
or timeout failures. No commits or pushes were created.

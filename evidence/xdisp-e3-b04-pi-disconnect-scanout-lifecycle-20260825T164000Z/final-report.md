# E3-B04 Final Report

1. Task: **PASS**
2. initial gud HEAD: `276312b56ddef5e8cf581d5fa378fb133d7874bd`
3. initial gud-gadget HEAD: `1cd654fd345d4ec773e5c47414a86ae19b3cecc3`
4. initial mir HEAD: `991a8cdeb537ccd8ee1ef04804e216a4accbf8c0`
5. Pi kernel version: `6.12.47+rpt-rpi-v8-ffs-xfercompltrace #2 SMP PREEMPT aarch64`
6. current gud-userspace implementation commit: `b9fc4effec076594badad3508b5a006909256bb7` (`functionfs: retire dead USB epochs safely`), deployed SHA-256 `7e22188ab8eb0e6a31eefacc9f88b6196cc3604e83730bfe2625ab8f16654a00`
7. waiting-screen owner: `gud-gadget/drm/src/main.rs`, `render_waiting_screen()` / `present_waiting_screen()`, Pi `gud-drm`
8. waiting-screen DRM architecture: persistent two-buffer RGB565 dumb-buffer allocation on `/dev/dri/card0`; HDMI-A-1; CRTC `pixelvalve-2`; plane-3; startup `set_crtc()`; restore rewrites both buffers and dirties the current front framebuffer
9. event currently intended to restore waiting screen: `ProtocolInvalidationReason::Disconnected` from FunctionFS `DISABLE`
10. does that event actually fire on physical phone disconnect: **no**; only `SUSPEND` fired while disconnected, and `DISABLE` arrived after reconnect began
11. external monitor after active disconnect: **LAST FRAME**
12. reason last frame persists: non-terminal SUSPEND has no display callback, so VC4 scans persistent fb 671 containing the last GUD pixels
13. is persistent last frame normal KMS behavior: **yes**; the delayed waiting-screen transition is a separate cleanup bug
14. FunctionFS events observed on disconnect: `SUSPEND`; delayed `DISABLE` during reconnect; no `UNBIND`; no endpoint error
15. terminal FunctionFS event observed: **yes**, exact event `DISABLE`, but only during reconnect; none while physically disconnected
16. old USB epoch retired: **no explicit retirement required/observed**; epoch 3 drained to Idle before delayed DISABLE, and no fresh ENABLE occurred
17. aggregate_state after disconnect: `Idle` after the final in-flight transfer completed
18. aio_state after disconnect: `Idle`
19. currently_owned after disconnect: `false`
20. Poisoned entered: **no**
21. gud-userspace PID survived: **yes**, PID `4488`, `NRestarts=0`
22. UDC remained bound: **yes**, `3f980000.usb`
23. is that expected: **yes** for persistent configfs reuse; `configured` while detached is unusual but not sufficient stale-state proof
24. DRM scanout after disconnect: plane-3, fb `671`, `RG16`, 1280x720, pitch 2560, CRTC `pixelvalve-2` enabled/active
25. was old GUD framebuffer logically still owned: **no** as FunctionFS/AIO ownership; the Pi process independently retained its DRM allocation
26. natural reconnect attempted: **yes**
27. phone reached external hub: **no**
28. Pi received descriptor/setup requests: **no**; Pi saw full-speed electrical attempts only
29. descriptor failures: **0 descriptor transactions reached Pi**; phone root port logged repeated `Cannot enable` and `unable to enumerate USB device`
30. GUD VID:PID returned: **no**
31. GUD probe passed: **no**
32. GUD DRM node returned: **no**
33. reconnect failure layer: **PHONE HOST**
34. frozen-frame classification: **DISPLAY_CLEANUP_MISSING**
35. display-only waiting-screen experiment performed: **no**; natural delayed DISABLE exercised the existing DRM-only restore
36. did waiting screen restore correctly: **yes**, immediately when reconnect delivered delayed DISABLE
37. did display-only restore modify FunctionFS/UDC state: **no**; PID, binding, and ownership remained unchanged
38. reconnect after display-only restore: **FAIL**
39. did waiting-screen restoration change reconnect behavior: **no**
40. if yes, causal mechanism: **not applicable**
41. is frozen frame causally related to reconnect blocker: **NO**
42. Pi DWC2 stale-state evidence: **no new conclusive evidence**; Pi saw repeated full-speed signaling but no EP0 setup. Prior `-110`/`-62` descriptor-error runs remain independent evidence
43. code changes made: evidence files only; no application/runtime source changes
44. permanent waiting-screen fix implemented: **no**
45. E1 safety weakened: **NO**
46. UDC reset introduced as normal behavior: **NO**
47. service restart introduced: **NO**
48. Mir modified: **NO**
49. Lomiri modified: **NO**
50. transport changed: **NO**
51. commits created: **none**
52. commits pushed: **NO**
53. evidence path: `/home/cristianr/Projects/linux-mobile/gud-gadget/evidence/xdisp-e3-b04-pi-disconnect-scanout-lifecycle-20260825T164000Z/`
54. final diagnosis: physical disconnect yields only FunctionFS SUSPEND; final AIO drains safely, but no waiting-screen callback runs, so normal VC4 KMS persistence displays the last frame. Delayed DISABLE on reconnect restores waiting pixels without touching USB state, while phone root-port enumeration still fails
55. E3 reconnect blocker after this task: **NARROWED**
56. exact next recommendation: track a separate Pi UX fix that restores waiting pixels at a proven display-safe disconnect boundary without changing FunctionFS ownership; continue E3 at the OnePlus root-port/HS-PHY failure, retaining Pi DWC2 investigation for runs where the phone reaches the hub and Pi descriptors

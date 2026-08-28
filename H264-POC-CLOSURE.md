# H.264 display transport POC — closure

## Status

**CLOSED / DEFERRED — 2026-08-28**

Active project direction: **GUD**.

This is a prioritization decision. H.264 was not technically disproven, and
the retained implementation remains a viable experimental transport.

## Why it was investigated

The POC compared video-codec transport with lossless GUD RGB565/LZ4 for a
phone or laptop extended display driven through a Raspberry Pi Zero 2 W.
The goal was to determine whether hardware codecs could reduce USB/framebuffer
bandwidth while retaining useful 1080p desktop behavior.

## What was proven

- Qualcomm Venus hardware encoding and Pi `bcm2835-codec` hardware decoding
  work in a real OnePlus/Lomiri-to-Pi display path.
- A real KDE laptop extended output can be captured, encoded, transported over
  TCP/Wi-Fi, hardware-decoded by the Pi, and presented over DRM/VC4/HDMI.
- Native 1920x1080 operation was demonstrated on both paths.
- The optimized Lomiri run sustained 29.975-29.982 source/encode/decode FPS
  and 29.039-29.821 DRM presentation submits/s.
- A settled laptop sample received 1,230 frames, decoded 1,228, and presented
  1,202; sender cadence was 29.9-30.2 FPS and presentation about 29.3 FPS.
- H.264 reduces wire bandwidth dramatically relative to full framebuffer
  transport. Pi receiver architecture matters: direct NV12 DRM/PRIME scanout
  is preferable to the measured CPU YUV-to-RGB565 conversion path, which ran
  at about 9.8 FPS and accumulated stale frames.
- No calibrated input-to-photon latency result was produced. Software submit,
  completion, and independent-clock age measurements are not physical scanout
  timestamps.

Detailed methods, workload qualifications, failures, and the GUD comparison
are in [DISPLAY_PATH_FINDINGS.md](DISPLAY_PATH_FINDINGS.md).

## Why H.264 is not being pursued now

H.264 has clear bandwidth and motion-cadence advantages, but it also adds
capture, codec, framing, color, receiver, scanout-ownership, and latency
complexity. Its 4:2:0 chroma subsampling is inherently less suitable for small
colored desktop text. The project already has substantial qualified GUD
infrastructure and its current goal is to finish that implementation rather
than continue transport research. This does not claim that GUD is universally
superior.

## When H.264 should be revisited

Reopen the POC only when a concrete requirement justifies the cost, such as
high-motion/video use, unacceptable GUD USB2 bandwidth, a hybrid transport,
resumed direct low-latency DRM/PRIME work, or hardware that enables a more
desktop-suitable codec/chroma format. The ten-item technical list at the end
of `DISPLAY_PATH_FINDINGS.md` is deferred context, not an active queue.

## Final selected direction

- **ACTIVE:** GUD implementation.
- **DEFERRED:** H.264 transport research.

## Important commits

| Repository | Branch | Commit | Purpose |
|---|---|---|---|
| `gud` | `poc/h264-usb-display` | `e739c60` | Prove the Venus H.264 USB display path. |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `11ce0a3` | Optimize capture, encode, and presentation. |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `d212304` | Qualify the optimized phone path. |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `0fb1c85` | Add the KDE virtual-display sender and persistent services. |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `e08c095` | Emit a continuous laptop frame timeline. |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `c83e8f6` | Retain final archival diagnostics and generated-artifact policy. |
| `gud-gadget` | `development` | `d8af510` | Consolidate GUD and H.264 findings. |

The exact closure-document commit is recorded by the repository history and
in the final operator report rather than self-referencing an unknown SHA.

## Evidence

Git retains source, methodology, final reports, and small summaries. The
closure evidence index is
[`evidence/h264-poc-closure-20260828T180110Z/README.md`](evidence/h264-poc-closure-20260828T180110Z/README.md).

The complete raw archive is intentionally outside Git at the workspace-level
layout `archives/h264-display-poc-closure-20260828T180110Z/`, with
`raw-evidence/<repository>/<repository-relative-path>`, `MANIFEST.txt`, and
`SHA256SUMS`. Its compressed `.tar.zst` sibling must not be pushed to GitHub.

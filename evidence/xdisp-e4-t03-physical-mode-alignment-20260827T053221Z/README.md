# E4-T03 physical-mode alignment

UTC evidence root: `2026-08-27T05:32:21Z`.

Result: PASS. The Rust gadget now enables complete-timing physical routing in
normal runtime. The OnePlus host driver was corrected to request and expose the
gadget's full 28-mode catalog instead of a local single-mode table. Live gates
proved exact 1280x720 and 1920x1080 VC4 modesets, five round trips, repeated
same-mode idempotence, stable advertisement hashes, and scaled fallback for a
synthetic 720x1520 timing.

The existing E4-T02 operator-observed 720p visual gate was not repeated. The
1080p waiting screen was previously observed on this same preferred VC4 mode;
this run added exact timing, framebuffer, full-frame, and clean fault evidence.
No physical USB or HDMI reconnect is part of this qualification.

See `final-report.md` for the gate summary and `SHA256SUMS` for integrity.

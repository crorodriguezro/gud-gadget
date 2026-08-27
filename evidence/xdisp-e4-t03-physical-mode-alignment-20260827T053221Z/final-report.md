1. E4-T03 result: PASS
2. gud HEAD: cde24e9a60c8233262cbd508c16418a4e44b1e7b
3. gud-gadget HEAD: d3cb0248e4c819902770b5a88ea680fda322bf6f
4. mir HEAD: eb7d70cbaae8570124f3db569f05b9adcbc68428
5. all canonical branches are development: YES
6. upstream C gadget reference repo/SHA: notro/gud gadget patches carried by crorodriguezro/gud at cde24e9a60c8233262cbd508c16418a4e44b1e7b; patch-source SHA-256 0004=ca40897ed9ba7b4ffaac2f530a7922f9bd3c9bdb, 0005=6ad4a6efe0b01be10de9d9f2d5f0a68df0d0c692
7. C connector probe behavior: DRM connector fill_modes enumerates the real connector and exposes EDID/modes
8. C mode advertisement behavior: gud_from_display_mode serializes every physical DRM mode with complete timing; preferred metadata is preserved
9. C SET_STATE modeset behavior: gud_to_display_mode reconstructs the selected timing, allocates the selected geometry/format, checks client modeset state, then commits it
10. Rust physical connector: /dev/dri/card0 HDMI-A-1, discovered connector index 0
11. Rust physical mode count: 25
12. Rust preferred physical mode: no DRM PREFERRED bit; deterministic connector-index-0 fallback = exact 1920x1080@148500 timing
13. physical 1280x720 available: YES
14. physical 1920x1080 available: YES
15. GUD advertised real mode count: 25
16. GUD advertised synthetic mode count: 3
17. complete timing identity method: normalized ModeKey(connector, clock, hdisplay, hsync_start, hsync_end, htotal, vdisplay, vsync_start, vsync_end, vtotal, flags), masking only documented GUD-private/preferred bits
18. width/height-only production matching present: NO
19. hardcoded 1920x1080 physical policy present: NO
20. hardcoded 1280x720 physical policy present: NO
21. GUD_TEST_DYNAMIC_MODE_MATCH behavior before: gated shadow preparation, exact CHECK planning/commit routing, and lifecycle invalidation; did not change connector, advertisement, preference, validation, ownership, or transport
22. GUD_TEST_DYNAMIC_MODE_MATCH required after: NO
23. exact matching normal-runtime enabled: YES
24. 720p selected GUD timing: 74250:1280:1390:1430:1650:720:725:730:750:0x5
25. 720p selected route: DirectExact
26. 720p physical HDMI timing after selection: 74250:1280:1390:1430:1650:720:725:730:750:0x5
27. 720p exact physical match: YES
28. 720p framebuffer dimensions/format: 1280x720 packed RGB565, pitch 2560, 1,843,200 bytes/mapping
29. 1080p selected GUD timing: 148500:1920:2008:2052:2200:1080:1084:1089:1125:0x5
30. 1080p selected route: DirectExact
31. 1080p physical HDMI timing after selection: 148500:1920:2008:2052:2200:1080:1084:1089:1125:0x5
32. 1080p exact physical match: YES
33. 1080p framebuffer dimensions/format: 1920x1080 packed RGB565, pitch 3840, 4,147,200 bytes/mapping
34. 720p->1080p->720p cycles run: 5 full 720p<->1080p round trips, 10 transitions, plus initial and final smoke gates
35. mode-cycle failures: 0
36. same-mode commits tested: 720p -> 720p -> 720p
37. unnecessary same-mode modesets observed: 0; modeset_commits remained 13 on both repeats
38. unnecessary same-mode framebuffer allocations observed: 0; framebuffer_allocations remained 42 on both repeats
39. physical real mode route: CatalogRoute::Exact -> DirectExact -> original libdrm mode/set_crtc
40. synthetic mode route: CatalogRoute::ScaledFallback -> ScaledBaseline
41. synthetic fallback result: PASS; 720x1520 scaled=true into preferred exact 1920x1080 baseline; complete 2,188,800-byte frame
42. advertised mode ordering stable: YES; before/after list SHA-256 both d169c3d69a598ee991c1fbfeef5d169fa2f970ec5f5df63828150f85a03b3cc7
43. preferred flag stable: YES; exactly the same index-0 1080p advertisement remained preferred
44. stale 12,800 active assumption removed: YES
45. stale 16KiB logical-limit wording removed: YES
46. visual 720p result: PASS; E4-T02 operator-observed full-screen checkerboard/color/edge gate retained, and T03 direct path was scaled=false with identical full-frame geometry
47. visual 1080p result: PASS; previously observed preferred-mode waiting display plus T03 exact CRTC/direct full-frame evidence; no visual regression signal
48. phone responsiveness: PASS; SSH/uptime responsive, xdisp.service restored active, USB 480 Mbit/s
49. Poisoned observed: NO
50. healthy-path timeout observed: NO
51. phone kernel fault observed: NO
52. Pi DWC2 fault observed: NO
53. Pi VC4 fault observed: NO
54. production code files modified: gud-gadget/drm/src/main.rs, drm/src/modes.rs, drm/src/scanout.rs; gud/backport-4.9/gud_connector.c, gud_protocol.h
55. test files modified: focused inline Rust tests in drm/src/main.rs, drm/src/modes.rs, drm/src/scanout.rs
56. documentation files modified: gud-gadget/docs/DEPLOY.md, docs/XDISP-P2.1-DYNAMIC-MODE-MATCHING-TEST.md, systemd/test-only/50-xdisp-p2.1-dynamic-mode-match.conf; gud/PROJECT-ROADMAP.md; this evidence bundle
57. tests run: cargo fmt --all -- --check; cargo test --locked; AArch64 release build; test-xdisp-lz4.sh; test-xdisp-lz4-contract.sh; test-gud-kms-mode-sequence-contract.sh; live exact 720p/1080p/cycle/idempotence/synthetic gates
58. tests result: PASS; Rust 174 tests passed, 0 failed; all C contract tests 0 failures; final AArch64 build and live smoke passed
59. E4-T03 roadmap marked verified: YES
60. E4-T04 marked complete: MUST BE NO
61. commits created: 0
62. pushed: MUST BE NO
63. evidence path: gud-gadget/evidence/xdisp-e4-t03-physical-mode-alignment-20260827T053221Z/
64. SHA256 verification: SHA256SUMS generated for every retained evidence file; sha256sum -c reports all OK
65. exact selected production mode-routing policy: real physical GUD mode -> exact complete-timing lookup -> corresponding VC4 physical mode -> physical modeset/direct scanout; synthetic nonphysical mode -> ScaledFallback
66. exact next ticket: E4-T04 — Persist Lomiri external-display placement

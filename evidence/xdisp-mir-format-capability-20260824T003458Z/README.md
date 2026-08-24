# Mir pixel-format capability audit

This bundle audits the unmodified Mir/Lomiri runtime used on the OnePlus 6,
specifically the Virtual/extended-output screencast path consumed by mirgud.

Result: the deployed stack delivered both `mir_pixel_format_rgb_565` and
`mir_pixel_format_rgb_888` as CPU-mappable buffers. RGB565 was delivered with
stride 2560 at 1280 pixels (2 bytes/pixel); RGB888 was delivered with stride
3840 (3 bytes/pixel). No Mir, Lomiri, production transport, or E2-T04 change
was made for this audit.

The RGB888 run used the existing diagnostic-only `--source-pixel-format`
selector in mirgud and kept GUD presentation disabled. The older diagnostic
binary did not exit cleanly after SIGTERM; the audit process was terminated
after its bounded capture and its temporary frame was removed.

See `summary.md` for the decision matrix and `source/`, `runtime/`,
`pixels/`, and `benchmark/` for evidence.

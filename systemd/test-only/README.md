# Historical/test-only deployment overrides

The files in this directory are retained for reproducible diagnostics and
historical qualification. They are not normal production configuration.

In particular, the P2.1 mode-matching overrides and the
`60-xdisp-p2.1-laptop-max-12800.conf` file must not be interpreted as current
logical GUD transaction limits. The 12,800-byte and 16-KiB values in the
historical procedures describe old test environments; current production uses
one negotiated logical update, while Pi FunctionFS/DWC2 request chunking is an
internal implementation detail.

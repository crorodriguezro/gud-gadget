# E1-T02 corrected hardware result

The corrected E1-T02 gate passed on the OnePlus 6 and Raspberry Pi Zero 2 W.
It exercised gadget commit `93a6364` and host-probe commit `fcde453`. The
descriptor and both SET_BUFFER transactions use no compression: each is a
640x5 XRGB8888 rectangle with `length=12800`, `compression=0`, and
`compressed_length=0`.

The single runner returned zero after two ordered commits. Both host URBs were
submitted and completed successfully with exactly 12,800 actual bytes. The Pi
accepted AIO before each initial status, returned both guards to Idle between
transactions, drained both cleanup statuses after each transaction, and
detached cleanly after transaction 2.

The retained full phone kernel log also contains older experiments. Acceptance
is bounded by the final `XDISP_T02_BEGIN` through its matching
`XDISP_T02_END result=0`; that window contains exactly two uncompressed probe
starts, two successful completions, and no `-71`, timeout, Oops, or retry.

Offline verification passed with 140 Rust tests, `cargo fmt -- --check`,
warning-clean Clippy, and `bash -n` for the host runner. The runner preserves
the remote result while collecting logs and exits nonzero if the remote gate
fails.

Raw logs are retained in `host-run/`, `phone-kernel-full.log`,
`pi-userspace-full.log`, and `pi-kernel-full.log`. `SHA256SUMS` covers the
durable summaries and raw logs used for this result.

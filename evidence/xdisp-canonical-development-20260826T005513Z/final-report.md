# Final report

Branch reconciliation: PASS (default GitHub branch metadata not changed).

`gud/development` is `cde24e9` (PASS-B tip `76afa07` plus branch policy
documentation) and `gud-gadget/development` is `ffc0293`. Both are pushed and
track `origin/development`.

Mir `pixel-format-benchmark` was `1d2fc98`; it is preserved exactly at
`legacy/pre-t06-android2-synthetic-gud-fork` and remains undeleted. The
recovered E4-T06 worktree was
`/home/cristianr/Projects/linux-mobile/.worktrees/e4-t06-minimal-mir-integration`,
based on UBports `72f96d2f69adcd1856295c36ee55ac03ad2860c5`. Its reconstruction
was committed as `eb7d70c` and published as Mir `development`.

Mir `development` has an empty diff under `src/platforms/android/server/` vs
the upstream base and contains no `gud_output.cpp` or synthetic Android2
controls. It includes standalone `mirgud`, `xdispd`, RGB565 support, and
LatestFramePresenter. The T07 direct-presentation commit was not imported.

The local CMake configure could not run focused tests because this environment
lacks Boost headers and `libandroid-properties`; no source changes were made
to compensate. Gud PASS-B and gadget large-logical-transfer behavior remain on
their canonical tips by ancestry.

Active roadmap documentation now names `development` as canonical and records
the legacy Mir branch. Historical evidence was not rewritten. The next ticket
is **E4-T02 — Fix full-width content and channel correctness**, using
`development` in all three repositories.

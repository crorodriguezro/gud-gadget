# Post-qualification reconciliation

This addendum preserves the repository state recorded by the E4-T03 raw
qualification evidence while documenting the subsequent production commits.

## E4-T03 qualification state to production history

The qualification bundle recorded the following dirty-tree state and correctly
reported `commits created: 0` and `pushed: MUST BE NO`:

| Repository | Qualification-time HEAD | Subsequent production commit |
| --- | --- | --- |
| `gud` | `cde24e9a60c8233262cbd508c16418a4e44b1e7b` | `c51c5bda4c0b3c4af2ff89c04dcea37a880306cf` — `gud: expose gadget timing catalog on 4.9 host` |
| `gud-gadget` | `d3cb0248e4c819902770b5a88ea680fda322bf6f` | `619ad8a8e65315757851fcef996f1c24e194ca3e` — `gud-drm: promote exact physical mode routing` |

The relationship is:

```text
qualification state
        ↓
same reviewed working-tree implementation
        ↓
subsequent production commits
```

The commits above are the reviewed implementation changes; no historical raw
qualification statement is being rewritten.

## Hash terminology correction

The values `ca40897ed9ba7b4ffaac2f530a7922f9bd3c9bdb` and
`6ad4a6efe0b01be10de9d9f2d5f0a68df0d0c692` are 40-character Git commit/object
identifiers. They are not SHA-256 checksums. The immutable E4-T03 evidence
contains the historical mislabel; this addendum is the correction for future
reference. Actual file hashes remain identified as SHA-256 only when they are
64-hex file digests in `SHA256SUMS` or equivalent files.

## Current qualified logical payload examples

- 1280x720 packed RGB565: 1,843,200 bytes.
- 1920x1080 packed RGB565: 4,147,200 bytes.

These are logical GUD transactions. Internal Pi FunctionFS/DWC2 request
chunking is below that boundary and is not a logical payload limit.

# H.264 POC closure evidence index

This small Git record provides traceability to the complete local archive
without committing repetitive raw logs. The archive was assembled before
cleanup and independently extracted and checksum-verified.

## Inventory

| Repository | Initial branch | Initial HEAD | Untracked files | Bytes archived |
|---|---|---:|---:|---:|
| `gud` | `poc/h264-usb-display` | `e739c60` | 5 | 348,010 |
| `gud-gadget` | `development` | `d8af510` | 1,870 | 1,656,948,106 |
| `mir-android2-platform-gud` | `poc/h264-usb-display` | `e08c095` | 376 | 74,211,152 |

An ignored-file follow-up also archived 701 historical GUD evidence, capability
capture, and motion-asset files (740,780,822 bytes) that ordinary `git status`
did not list. Rebuildable kernel and Cargo/build outputs were category D.

The final directory archive contains 2,959 files, including README, inventory,
manifest, and checksum metadata. Classification recorded 2 retained source
files, 2,928 raw-evidence files, and 22 generated files. The compressed archive
is 156,351,574 bytes and has SHA-256
`21dd89e43114eb7c75940d0352c547d8c361ed0c8e2f919ee99ed08a6da833e1`.

## Oversized raw artifacts

These files exceed GitHub's 100 MiB limit and remain only under
`raw-evidence/gud-gadget/` in the local archive:

| Repository-relative path | Bytes | SHA-256 | Supports |
|---|---:|---|---|
| `evidence/xdisp-e2-t04-rgb565-lz4-native-soak-20260824T020019Z/logs/pi-kernel.log` | 311,349,146 | `175f49807dc2cecf9c69364dde913f088c823ec1f7b7d11319a9cb7c852951f3` | Native-soak kernel safety audit. |
| `evidence/xdisp-e2-t04-rgb565-lz4-native-soak-20260824T020019Z/logs/pi-journal.log` | 285,885,996 | `a5d6c6069aee0a862736da7774adad1b2166b0c80f660aad734537bdfe2eaf02` | Native-soak receiver behavior. |
| `evidence/xdisp-e2-current-rgb565-controlled-20260821T135153Z/direct-bars/pi-journal-after-all.txt` | 226,532,887 | `a63fb3af1b2cb8951d75459c508ddffc926b7600e3ff4fb1b63b4a887020ffde` | Controlled RGB565 presentation diagnosis. |
| `evidence/xdisp-e2-t04-rgb565-lz4-soak-20260824T033632Z/logs/xdispd.log` | 115,316,686 | `ced8e731d9e890956d967eceeacfad9109501062bf57252b12173ecb2c712708` | Managed 30-minute soak producer accounting. |

## Verification

- `sha256sum -c SHA256SUMS` passed on the assembled directory.
- `tar --zstd -tf` listed the compressed archive successfully.
- The compressed archive was extracted into a fresh temporary directory.
- `sha256sum -c --quiet SHA256SUMS` returned status 0 in that extraction.

No Git LFS objects were created. Conclusions and retained small reports take
precedence for normal development; raw files are forensic history.

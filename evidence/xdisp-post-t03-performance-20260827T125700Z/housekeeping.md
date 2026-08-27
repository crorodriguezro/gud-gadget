# E4 housekeeping record

- E4-T02 is marked `verified` in `gud/PROJECT-ROADMAP.md`; E4-T04 remains
  `planned`.
- The exact T02 tooling is retained in Mir:
  `tools/e4-t02-reference.py` and `tests/test_e4_t02_reference.py`.
- T02 validation: two unittest cases passed; generated RGB565 payload length
  was 1,843,200 bytes and its recorded digest verified.
- Qualification dirty heads are mapped to production commits in
  `docs/POST-QUALIFICATION.md` without rewriting raw evidence.
- P2.1 design/plan carry prominent supersession banners for historical
  12,800-byte/16-KiB assumptions.
- The two 40-character Git refs are corrected as Git object identifiers, not
  SHA-256 digests. Raw historical evidence is unchanged.
- No fresh 1080 visual was repeated; the prior qualified visual evidence is
  retained and this limitation is explicit.

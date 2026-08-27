# Rust physical-mode architecture audit

Audit baseline: `gud-gadget` HEAD
`d3cb0248e4c819902770b5a88ea680fda322bf6f`.

## Discovery and advertisement

`main()` selects the first connected connector returned by DRM resources; it
does not hard-code `HDMI-A-1`. It copies `connector.modes()` into
`connector_modes`. `advertised_preferred_mode_index()` selects the first DRM
mode carrying `ModeTypeFlags::PREFERRED`, or connector index 0 if none does.

Every connector mode becomes a `PhysicalCatalogMode<Mode>`. The stored
`DisplayMode` copies clock, all horizontal and vertical timing fields, and DRM
flags; the stored `Mode` is the exact libdrm value later passed to `set_crtc()`.
`RouteCatalog::build()` preserves connector order, de-duplicates by complete
`ModeKey`, and creates `CatalogRoute::Exact(original_mode)` entries.

The three product portrait timings (900x1900, 810x1710, and 720x1520) are
derived from the preferred physical timing. They support phone-shaped logical
outputs. They are appended after physical entries and route to
`ScaledFallback`. A dimension pre-check avoids generating common duplicates,
and the catalog's complete-key de-duplication guarantees a real physical entry
wins if a synthetic timing is identical.

The GUD GET_MODES handler serves `route_catalog.advertised_modes()` directly.
The preferred/private bits are normalized for USB advertisement without
altering `ModeKey` identity; exactly one entry receives the GUD-private
preferred bit.

## SET_STATE and scanout

Gadget protocol validation accepts only the advertised connector, RGB565
format, and modes. `Event::StateChecked` contains the wire-visible selected
snapshot. `ModeKey::from_snapshot()` uses connector, clock, every horizontal
and vertical timing field, and user-visible flags. There is no resolution-only
production lookup.

Before T03 promotion, `GUD_TEST_DYNAMIC_MODE_MATCH=1` controlled all routing
actions after validation:

- exact active timing: stage `ExactActiveNoOp`;
- exact inactive timing: allocate a two-buffer scanout at the stored physical
  mode dimensions and stage `ExactCandidate`;
- synthetic timing: retain/return to baseline and activate the scaled shadow;
- failed exact allocation/modeset: retain the current physical scanout and use
  a generation-bound scaled failure route.

At matching `StateCommitted`, an exact candidate receives one `set_crtc()`
with its stored original libdrm mode. A successful transition becomes
`DirectExact`; synthetic routes become `ScaledBaseline`. Direct updates copy
to the physical framebuffer; scaled updates use the shadow and aspect-fit
composition path.

`ScanoutCounters` currently records allocations, releases, framebuffer and
dumb-buffer teardown, mappings, physical switches, no-ops, and fallbacks.
Candidate reuse prevents reallocation across repeated CHECKs. Once the exact
mode is active, repeated identical states produce `ExactActiveNoOp`, avoiding
both framebuffer allocation and `set_crtc()`.

## Test-flag difference before T03

| Behavior | Flag unset | `GUD_TEST_DYNAMIC_MODE_MATCH=1` | Desired production behavior |
| --- | --- | --- | --- |
| Advertised list/order | Physical exact entries then synthetic entries | Same | Same |
| Preferred mode | DRM preferred/index-0 fallback | Same | Same |
| State validation | Exact advertised timing/format/connector | Same | Same |
| State CHECK routing | Notification ignored | Exact/synthetic plan prepared | Always prepare |
| Framebuffer allocation | Fixed startup scanout only | Exact candidate allocated at selected dimensions | Allocate/reuse exact candidate |
| Physical modeset | Startup mode remains fixed | Exact route commits selected physical mode | Exact route commits selected physical mode |
| Scaling | All logical modes presented against fixed startup output | Synthetic/failure only | Synthetic/failure only |
| Connector ownership | Same connector/CRTC | Same | Same |
| Error handling | No route preparation error | Failed exact route retains current scanout with scaled failure route | Same safe failure behavior |
| USB transport | Same | Same | Same |

The flag does not change descriptors, mode order, preference, protocol
validation, connector ownership, or transport. It gates only shadow
preallocation, dynamic scanout state initialization, CHECK planning, COMMIT
routing, and lifecycle candidate invalidation.

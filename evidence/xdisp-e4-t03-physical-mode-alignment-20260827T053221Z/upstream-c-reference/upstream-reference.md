# Upstream C GUD gadget mode semantics

Reference source: `notro/gud` historical gadget patches carried by
`crorodriguezro/gud`:

- `0004-drm-gud-Add-functionality-for-the-USB-gadget-side.patch`, patch source
  commit `ca40897ed9ba7b4ffaac2f530a7922f9bd3c9bdb`
- `0005-usb-gadget-function-Add-GUD-USB-Display-support.patch`, patch source commit
  `6ad4a6efe0b01be10de9d9f2d5f0a68df0d0c692`

The audit used the patches at `gud` HEAD
`cde24e9a60c8233262cbd508c16418a4e44b1e7b`.

| Behavior | C reference symbol/function | Rust equivalent | Equivalent today? | Action needed |
| --- | --- | --- | --- | --- |
| Probe connector | `gud_gadget_probe_connector()` calls `connector->funcs->fill_modes()` under `mode_config.mutex` | `main()` calls libdrm `get_connector(..., false)` | PARTIAL | Startup enumeration is equivalent; live reprobe is absent. |
| Enumerate modes | `list_for_each_entry(..., &connector->modes, head)` | `connector.modes().to_vec()` | YES | None for startup catalog. |
| Expose EDID | Copies `connector->edid_blob_ptr` and serves `GUD_REQ_GET_CONNECTOR_EDID` | No physical EDID response | NO | Track separately; not required to make exact timing routing correct. |
| Preferred mode | Preserved through `gud_from_display_mode()` conversion of DRM mode type | `advertised_preferred_mode_index()` selects `ModeTypeFlags::PREFERRED`, falling back to connector index 0, then normalization marks exactly one mode | YES | Retain deterministic fallback and test it. |
| DRM to GUD conversion | `gud_from_display_mode()` converts every connector mode | `PhysicalCatalogMode` copies clock, horizontal/vertical timing, and flags from every libdrm `Mode` | YES | Retain complete timing conversion. |
| SET_STATE conversion/validation | `gud_to_display_mode()`, format/connector checks, property handling | Gadget request validation plus `ModeKey::from_snapshot()` lookup in `RouteCatalog` | PARTIAL | Exact catalog lookup is stronger than accepting an arbitrary converted mode, but was runtime-gated. Enable it normally. |
| Framebuffer allocation | `drm_client_framebuffer_create(mode.hdisplay, mode.vdisplay, format)` when the existing buffer does not match | `ScanoutAllocation::create()` uses selected physical mode dimensions; backend format is negotiated production RGB565 | YES | Retain cached active/candidate allocations and expose counters. |
| Modeset configuration | `drm_client_modeset_set(client, connector, &mode, buffer->fb)` | `DrmScanoutBackend::set_crtc(framebuffer, exact libdrm Mode)` | PARTIAL | Exact path exists but was test-only. Promote it. |
| Modeset validation | `drm_client_modeset_check()` | No separate libdrm test-only commit; candidate allocation is prepared and the real modeset occurs once at COMMIT | PARTIAL | Behavioral validation occurs through catalog identity/allocation; do not add a destructive pre-commit modeset. |
| Modeset commit | `drm_client_modeset_commit()` | One `set_crtc()` in `attempt_mapped_switch()` | PARTIAL | Equivalent only with dynamic path enabled; promote it. |
| Same-mode commit | Buffer is reused when dimensions/format match, but C still calls modeset set/check/commit | `ExactActiveNoOp` avoids allocation and modeset | YES (project requirement) | Rust intentionally provides stricter idempotence than the C reference. |
| Frame updates | Copies/decompresses received rectangle into the active client framebuffer and flushes | Direct copy for `DirectExact`; scaling/composition shadow for fallback | YES | Retain full negotiated update transport. |
| Hotplug | DRM client `.hotplug = gud_gadget_client_hotplug`; reprobes modes, EDID, status, and marks changed | Connector/catalog is captured only at process startup; protocol lifecycle can report changed but does not reprobe VC4 | NO | Document as a remaining T03 boundary unless hardware qualification requires service lifecycle reprobe. |

## Exact semantic sequence

The C reference probes the connector, serializes each connector timing, converts
the selected GUD timing back to a DRM mode, creates or reuses a matching
framebuffer, configures the selected connector/mode, checks the modeset, and
commits it. It does not choose a physical mode by width and height.

The Rust behavioral equivalent is therefore a `ModeKey` lookup of the exact
host-selected timing followed by `set_crtc()` with the original libdrm `Mode`
stored in that catalog entry. Synthetic timings have no original libdrm mode
and must remain on the scaled route.

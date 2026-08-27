use std::collections::HashMap;
use std::fmt::Debug;

use anyhow::{ensure, Context};
use gud_gadget::{
    DisplayMode, DisplayStateSnapshot, GUD_DISPLAY_MODE_FLAG_PREFERRED,
    GUD_DISPLAY_MODE_FLAG_USER_MASK,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ModeKey {
    pub(crate) connector: u8,
    pub(crate) clock: u32,
    pub(crate) hdisplay: u16,
    pub(crate) hsync_start: u16,
    pub(crate) hsync_end: u16,
    pub(crate) htotal: u16,
    pub(crate) vdisplay: u16,
    pub(crate) vsync_start: u16,
    pub(crate) vsync_end: u16,
    pub(crate) vtotal: u16,
    pub(crate) flags: u32,
}

impl ModeKey {
    pub(crate) fn new(connector: u8, mode: &DisplayMode) -> Self {
        Self {
            connector,
            clock: mode.clock,
            hdisplay: mode.hdisplay,
            hsync_start: mode.hsync_start,
            hsync_end: mode.hsync_end,
            htotal: mode.htotal,
            vdisplay: mode.vdisplay,
            vsync_start: mode.vsync_start,
            vsync_end: mode.vsync_end,
            vtotal: mode.vtotal,
            flags: mode.flags & GUD_DISPLAY_MODE_FLAG_USER_MASK,
        }
    }

    pub(crate) fn from_snapshot(snapshot: &DisplayStateSnapshot) -> Self {
        Self::new(snapshot.connector, &snapshot.mode)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogRoute<M> {
    Exact(M),
    ScaledFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CatalogEntry<M> {
    pub(crate) key: ModeKey,
    pub(crate) advertised_mode: DisplayMode,
    pub(crate) route: CatalogRoute<M>,
}

#[derive(Clone, Debug)]
pub(crate) struct PhysicalCatalogMode<M> {
    pub(crate) mode: M,
    pub(crate) timing: DisplayMode,
}

#[derive(Clone, Debug)]
pub(crate) struct RouteCatalog<M> {
    connector: u8,
    entries: Vec<CatalogEntry<M>>,
    by_key: HashMap<ModeKey, usize>,
    preferred_index: usize,
}

impl<M: Copy + Debug + Eq> RouteCatalog<M> {
    pub(crate) fn build(
        connector: u8,
        physical_modes: &[PhysicalCatalogMode<M>],
        preferred_physical_index: usize,
        synthetic_modes: &[DisplayMode],
    ) -> anyhow::Result<Self> {
        ensure!(!physical_modes.is_empty(), "physical mode catalog is empty");
        let preferred_physical = physical_modes
            .get(preferred_physical_index)
            .context("preferred physical mode index is out of range")?;
        let preferred_key = ModeKey::new(connector, &preferred_physical.timing);

        let mut entries = Vec::new();
        let mut by_key = HashMap::new();
        for physical in physical_modes {
            let key = ModeKey::new(connector, &physical.timing);
            if by_key.contains_key(&key) {
                continue;
            }
            let index = entries.len();
            by_key.insert(key, index);
            entries.push(CatalogEntry {
                key,
                advertised_mode: normalize_advertised_mode(&physical.timing, false),
                route: CatalogRoute::Exact(physical.mode),
            });
        }

        let preferred_index = *by_key
            .get(&preferred_key)
            .context("preferred physical timing was not cataloged")?;

        for synthetic in synthetic_modes {
            let key = ModeKey::new(connector, synthetic);
            if by_key.contains_key(&key) {
                continue;
            }
            let index = entries.len();
            by_key.insert(key, index);
            entries.push(CatalogEntry {
                key,
                advertised_mode: normalize_advertised_mode(synthetic, false),
                route: CatalogRoute::ScaledFallback,
            });
        }

        for (index, entry) in entries.iter_mut().enumerate() {
            entry.advertised_mode =
                normalize_advertised_mode(&entry.advertised_mode, index == preferred_index);
        }
        ensure!(
            entries
                .iter()
                .filter(|entry| {
                    entry.advertised_mode.flags & GUD_DISPLAY_MODE_FLAG_PREFERRED != 0
                })
                .count()
                == 1,
            "route catalog must advertise exactly one preferred timing"
        );

        Ok(Self {
            connector,
            entries,
            by_key,
            preferred_index,
        })
    }

    pub(crate) fn connector(&self) -> u8 {
        self.connector
    }

    pub(crate) fn entries(&self) -> &[CatalogEntry<M>] {
        &self.entries
    }

    pub(crate) fn advertised_modes(&self) -> Vec<DisplayMode> {
        self.entries
            .iter()
            .map(|entry| entry.advertised_mode.clone())
            .collect()
    }

    pub(crate) fn preferred_entry(&self) -> &CatalogEntry<M> {
        &self.entries[self.preferred_index]
    }

    pub(crate) fn entry_for_snapshot(
        &self,
        snapshot: &DisplayStateSnapshot,
    ) -> Option<&CatalogEntry<M>> {
        self.entry_for_key(ModeKey::from_snapshot(snapshot))
    }

    #[cfg(test)]
    pub(crate) fn entry_for_mode(&self, mode: &DisplayMode) -> Option<&CatalogEntry<M>> {
        self.entry_for_key(ModeKey::new(self.connector, mode))
    }

    pub(crate) fn entry_for_key(&self, key: ModeKey) -> Option<&CatalogEntry<M>> {
        self.by_key.get(&key).map(|index| &self.entries[*index])
    }
}

fn normalize_advertised_mode(mode: &DisplayMode, preferred: bool) -> DisplayMode {
    let mut normalized = mode.clone();
    normalized.flags &= GUD_DISPLAY_MODE_FLAG_USER_MASK;
    if preferred {
        normalized.flags |= GUD_DISPLAY_MODE_FLAG_PREFERRED;
    }
    normalized
}

#[cfg(test)]
mod tests {
    use gud_gadget::{
        DisplayMode, DisplayStateSnapshot, GUD_DISPLAY_MODE_FLAG_PREFERRED,
        GUD_DISPLAY_MODE_FLAG_USER_MASK, GUD_PIXEL_FORMAT_RGB565,
    };

    use super::{
        normalize_advertised_mode, CatalogRoute, ModeKey, PhysicalCatalogMode, RouteCatalog,
    };

    fn mode(clock: u32, width: u16, height: u16, flags: u32) -> DisplayMode {
        DisplayMode {
            clock,
            hdisplay: width,
            hsync_start: width + 40,
            hsync_end: width + 80,
            htotal: width + 160,
            vdisplay: height,
            vsync_start: height + 5,
            vsync_end: height + 10,
            vtotal: height + 30,
            flags,
        }
    }

    fn physical<M>(raw: M, timing: DisplayMode) -> PhysicalCatalogMode<M> {
        PhysicalCatalogMode { mode: raw, timing }
    }

    #[test]
    fn full_timing_identity_distinguishes_clock_totals_and_polarity() {
        let base = mode(74_250, 1280, 720, 0);
        let mut clock = base.clone();
        clock.clock += 1;
        let mut total = base.clone();
        total.htotal += 1;
        let mut polarity = base.clone();
        polarity.flags = 1;

        assert_ne!(ModeKey::new(0, &base), ModeKey::new(0, &clock));
        assert_ne!(ModeKey::new(0, &base), ModeKey::new(0, &total));
        assert_ne!(ModeKey::new(0, &base), ModeKey::new(0, &polarity));
        assert_ne!(ModeKey::new(0, &base), ModeKey::new(1, &base));
    }

    #[test]
    fn preferred_and_private_flags_do_not_change_timing_identity() {
        let base = mode(74_250, 1280, 720, 0x21);
        let mut decorated = base.clone();
        decorated.flags |= GUD_DISPLAY_MODE_FLAG_PREFERRED | (1 << 31) | (1 << 20);

        assert_eq!(ModeKey::new(0, &base), ModeKey::new(0, &decorated));
        assert_eq!(
            ModeKey::new(0, &decorated).flags,
            base.flags & GUD_DISPLAY_MODE_FLAG_USER_MASK
        );
    }

    #[test]
    fn unsupported_flag_duplicates_keep_the_first_real_mode() {
        let first = mode(74_250, 1280, 720, 0);
        let mut duplicate = first.clone();
        duplicate.flags = (1 << 31) | GUD_DISPLAY_MODE_FLAG_PREFERRED;
        let catalog =
            RouteCatalog::build(0, &[physical(11, first), physical(22, duplicate)], 1, &[])
                .unwrap();

        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(catalog.entries()[0].route, CatalogRoute::Exact(11));
        assert_eq!(
            catalog.entries()[0].advertised_mode.flags,
            GUD_DISPLAY_MODE_FLAG_PREFERRED
        );
    }

    #[test]
    fn preferred_selection_uses_physical_index_and_marks_exactly_one_entry() {
        let first = mode(148_500, 1920, 1080, 0);
        let second = mode(74_250, 1280, 720, 0);
        let catalog = RouteCatalog::build(
            0,
            &[physical(1, first), physical(2, second.clone())],
            1,
            &[],
        )
        .unwrap();

        assert_eq!(catalog.preferred_entry().key, ModeKey::new(0, &second));
        assert_eq!(
            catalog
                .advertised_modes()
                .iter()
                .filter(|mode| mode.flags & GUD_DISPLAY_MODE_FLAG_PREFERRED != 0)
                .count(),
            1
        );
    }

    #[test]
    fn exact_physical_and_synthetic_scaled_routes_are_classified() {
        let physical_mode = mode(148_500, 1920, 1080, 0);
        let synthetic_mode = mode(103_000, 900, 1900, 0);
        let catalog = RouteCatalog::build(
            0,
            &[physical(7, physical_mode.clone())],
            0,
            std::slice::from_ref(&synthetic_mode),
        )
        .unwrap();

        assert_eq!(
            catalog.entry_for_mode(&physical_mode).unwrap().route,
            CatalogRoute::Exact(7)
        );
        assert_eq!(
            catalog.entry_for_mode(&synthetic_mode).unwrap().route,
            CatalogRoute::ScaledFallback
        );
    }

    #[test]
    fn physical_mode_beats_an_identical_synthetic_timing() {
        let timing = mode(74_250, 1280, 720, 0x5);
        let catalog = RouteCatalog::build(
            0,
            &[physical(17, timing.clone())],
            0,
            std::slice::from_ref(&timing),
        )
        .unwrap();

        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(
            catalog.entry_for_mode(&timing).unwrap().route,
            CatalogRoute::Exact(17)
        );
    }

    #[test]
    fn route_lookups_do_not_change_advertised_mode_order_or_preference() {
        let preferred = mode(148_500, 1920, 1080, 0x5);
        let second = mode(74_250, 1280, 720, 0x5);
        let synthetic = mode(103_000, 900, 1900, 0);
        let catalog = RouteCatalog::build(
            0,
            &[physical(1, preferred.clone()), physical(2, second.clone())],
            0,
            std::slice::from_ref(&synthetic),
        )
        .unwrap();
        let before = catalog.advertised_modes();

        assert!(catalog.entry_for_mode(&second).is_some());
        assert!(catalog.entry_for_mode(&synthetic).is_some());
        assert!(catalog.entry_for_mode(&preferred).is_some());

        assert_eq!(catalog.advertised_modes(), before);
        assert_eq!(catalog.preferred_entry().advertised_mode, before[0]);
    }

    #[test]
    fn host_masked_echo_matches_the_catalog_entry() {
        let mut physical_mode = mode(74_250, 1280, 720, 0x21);
        physical_mode.flags |= (1 << 31) | GUD_DISPLAY_MODE_FLAG_PREFERRED;
        let catalog =
            RouteCatalog::build(0, &[physical(5, physical_mode.clone())], 0, &[]).unwrap();
        let mut echoed = physical_mode;
        echoed.flags &= GUD_DISPLAY_MODE_FLAG_USER_MASK;
        let snapshot = DisplayStateSnapshot {
            mode: echoed,
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
            generation: 9,
        };

        assert_eq!(
            catalog.entry_for_snapshot(&snapshot).unwrap().route,
            CatalogRoute::Exact(5)
        );
    }

    #[test]
    fn normalization_masks_private_flags_before_adding_preferred() {
        let raw = mode(
            74_250,
            1280,
            720,
            GUD_DISPLAY_MODE_FLAG_USER_MASK | (1 << 31),
        );

        assert_eq!(
            normalize_advertised_mode(&raw, true).flags,
            GUD_DISPLAY_MODE_FLAG_USER_MASK | GUD_DISPLAY_MODE_FLAG_PREFERRED
        );
        assert_eq!(
            normalize_advertised_mode(&raw, false).flags,
            GUD_DISPLAY_MODE_FLAG_USER_MASK
        );
    }
}

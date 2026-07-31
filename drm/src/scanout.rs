use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Debug;
use std::rc::Rc;

use anyhow::{ensure, Context};
use gud_gadget::DisplayStateSnapshot;

use crate::modes::ModeKey;

pub(crate) const SCANOUT_SLOT_COUNT: usize = 3;
pub(crate) const BUFFERS_PER_SCANOUT: usize = 2;

pub(crate) type ScanoutSlot<B> = Option<Box<ScanoutAllocation<B>>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FallbackReason {
    PrepareFailed,
    SwitchFailed,
    BaselineSwitchFailed,
    PlanMismatch,
    FailedCacheHit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingPlanKind {
    ExactActiveNoOp,
    ExactCandidate { slot: usize },
    ScaledBaseline { switch_required: bool },
    ScaledCurrentAfterFailure(FallbackReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingPlan<M> {
    pub(crate) snapshot: DisplayStateSnapshot,
    pub(crate) logical_key: ModeKey,
    pub(crate) requested_physical_key: Option<ModeKey>,
    pub(crate) requested_mode: Option<M>,
    pub(crate) kind: PendingPlanKind,
    pub(crate) failed_cache_hit: bool,
    pub(crate) mode_prepare_ms: u128,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FailedRouteKey {
    pub(crate) logical: ModeKey,
    pub(crate) current_physical: ModeKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PresentationRoute {
    DirectExact {
        logical: ModeKey,
        physical: ModeKey,
    },
    ScaledBaseline {
        logical: ModeKey,
    },
    ScaledCurrentAfterFailure {
        logical: ModeKey,
        reason: FallbackReason,
    },
}

#[derive(Debug)]
pub(crate) struct DynamicRouteState<M> {
    pub(crate) pending_plan: Option<PendingPlan<M>>,
    pub(crate) failed_routes: HashSet<FailedRouteKey>,
    pub(crate) committed_snapshot: Option<DisplayStateSnapshot>,
    pub(crate) presentation_route: Option<PresentationRoute>,
    pub(crate) current_physical_key: ModeKey,
    pub(crate) candidate_key: Option<ModeKey>,
}

impl<M> DynamicRouteState<M> {
    fn new(baseline_physical_key: ModeKey) -> Self {
        Self {
            pending_plan: None,
            failed_routes: HashSet::new(),
            committed_snapshot: None,
            presentation_route: None,
            current_physical_key: baseline_physical_key,
            candidate_key: None,
        }
    }

    pub(crate) fn failed_key(&self, logical: ModeKey) -> FailedRouteKey {
        FailedRouteKey {
            logical,
            current_physical: self.current_physical_key,
        }
    }

    pub(crate) fn invalidate_pending(&mut self) {
        self.pending_plan = None;
        self.candidate_key = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LogicalMemoryEstimate {
    pub(crate) max_shadow_bytes: usize,
    pub(crate) scanout_bytes: usize,
    pub(crate) peak_bytes: usize,
}

pub(crate) fn logical_memory_estimate(
    physical_sizes: impl IntoIterator<Item = (u32, u32)>,
    advertised_sizes: impl IntoIterator<Item = (u32, u32)>,
    bytes_per_pixel: usize,
) -> anyhow::Result<LogicalMemoryEstimate> {
    let largest_physical = physical_sizes
        .into_iter()
        .map(|(width, height)| checked_bytes(width, height, bytes_per_pixel))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("physical mode catalog is empty")?;
    let max_shadow_bytes = advertised_sizes
        .into_iter()
        .map(|(width, height)| checked_bytes(width, height, bytes_per_pixel))
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .max()
        .context("advertised mode catalog is empty")?;
    let scanout_bytes = largest_physical
        .checked_mul(BUFFERS_PER_SCANOUT)
        .and_then(|bytes| bytes.checked_mul(SCANOUT_SLOT_COUNT))
        .context("logical scanout memory estimate overflow")?;
    let peak_bytes = scanout_bytes
        .checked_add(max_shadow_bytes)
        .context("logical peak memory estimate overflow")?;
    Ok(LogicalMemoryEstimate {
        max_shadow_bytes,
        scanout_bytes,
        peak_bytes,
    })
}

fn checked_bytes(width: u32, height: u32, bytes_per_pixel: usize) -> anyhow::Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
        .context("mode byte calculation overflow")
}

fn checked_add_live_bytes(current: usize, addition: usize) -> anyhow::Result<usize> {
    current
        .checked_add(addition)
        .context("actual scanout live-byte accounting overflow")
}

pub(crate) trait ScanoutBackend {
    type Buffer;
    type Framebuffer: Copy + Debug + Eq;
    type Mode: Copy + Debug + Eq;
    type Mapping<'a>: AsMut<[u8]>
    where
        Self: 'a;

    fn mode_size(&self, mode: Self::Mode) -> (u32, u32);
    fn create_buffer(&mut self, width: u32, height: u32) -> anyhow::Result<Self::Buffer>;
    fn buffer_pitch(&self, buffer: &Self::Buffer) -> u32;
    fn add_framebuffer(&mut self, buffer: &Self::Buffer) -> anyhow::Result<Self::Framebuffer>;
    fn map_buffer<'a>(&mut self, buffer: &'a mut Self::Buffer)
        -> anyhow::Result<Self::Mapping<'a>>;
    fn remove_framebuffer(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()>;
    fn destroy_buffer(&mut self, buffer: Self::Buffer) -> anyhow::Result<()>;
    fn dirty_framebuffer(
        &mut self,
        framebuffer: Self::Framebuffer,
        x1: u16,
        y1: u16,
        x2: u16,
        y2: u16,
    ) -> anyhow::Result<()>;
    fn set_crtc(&mut self, framebuffer: Self::Framebuffer, mode: Self::Mode) -> anyhow::Result<()>;
    fn page_flip(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ScanoutCounterSnapshot {
    pub(crate) allocations: u64,
    pub(crate) releases: u64,
    pub(crate) framebuffer_removes: u64,
    pub(crate) dumb_buffer_destroys: u64,
    pub(crate) maps: u64,
    pub(crate) unmaps: u64,
    pub(crate) switches: u64,
    pub(crate) no_ops: u64,
    pub(crate) fallbacks: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ScanoutCounters(Rc<RefCell<ScanoutCounterSnapshot>>);

impl ScanoutCounters {
    pub(crate) fn snapshot(&self) -> ScanoutCounterSnapshot {
        *self.0.borrow()
    }

    fn allocation(&self) {
        self.0.borrow_mut().allocations += 1;
    }

    fn release(&self) {
        self.0.borrow_mut().releases += 1;
    }

    fn framebuffer_remove(&self) {
        self.0.borrow_mut().framebuffer_removes += 1;
    }

    fn dumb_buffer_destroy(&self) {
        self.0.borrow_mut().dumb_buffer_destroys += 1;
    }

    fn map(&self) {
        self.0.borrow_mut().maps += 1;
    }

    fn unmap(&self) {
        self.0.borrow_mut().unmaps += 1;
    }

    pub(crate) fn switched(&self) {
        self.0.borrow_mut().switches += 1;
    }

    pub(crate) fn no_op(&self) {
        self.0.borrow_mut().no_ops += 1;
    }

    pub(crate) fn fallback(&self) {
        self.0.borrow_mut().fallbacks += 1;
    }
}

struct TrackedMapping<'a, B: ScanoutBackend + 'a> {
    mapping: B::Mapping<'a>,
    counters: ScanoutCounters,
}

impl<'a, B: ScanoutBackend + 'a> AsMut<[u8]> for TrackedMapping<'a, B> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.mapping.as_mut()
    }
}

impl<'a, B: ScanoutBackend + 'a> Drop for TrackedMapping<'a, B> {
    fn drop(&mut self) {
        self.counters.unmap();
    }
}

struct ScanoutBuffer<B: ScanoutBackend> {
    buffer: Option<B::Buffer>,
    framebuffer: Option<B::Framebuffer>,
    pitch: u32,
    mapping_len: usize,
}

impl<B: ScanoutBackend> Default for ScanoutBuffer<B> {
    fn default() -> Self {
        Self {
            buffer: None,
            framebuffer: None,
            pitch: 0,
            mapping_len: 0,
        }
    }
}

pub(crate) struct ScanoutAllocation<B: ScanoutBackend> {
    mode: B::Mode,
    width: u32,
    height: u32,
    buffers: [ScanoutBuffer<B>; BUFFERS_PER_SCANOUT],
    pitch: u32,
    front_buffer_index: usize,
    live_bytes: usize,
    released: bool,
}

impl<B: ScanoutBackend> ScanoutAllocation<B> {
    pub(crate) fn create(
        backend: &mut B,
        mode: B::Mode,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<Self> {
        let (width, height) = backend.mode_size(mode);
        ensure!(
            width > 0 && height > 0,
            "scanout dimensions must be non-zero"
        );

        let mut allocation = Self {
            mode,
            width,
            height,
            buffers: std::array::from_fn(|_| ScanoutBuffer::default()),
            pitch: 0,
            front_buffer_index: 0,
            live_bytes: 0,
            released: false,
        };

        let result = (|| {
            for index in 0..BUFFERS_PER_SCANOUT {
                let buffer = backend
                    .create_buffer(width, height)
                    .with_context(|| format!("create dumb buffer {index}"))?;
                let pitch = backend.buffer_pitch(&buffer);
                allocation.buffers[index].pitch = pitch;
                allocation.buffers[index].buffer = Some(buffer);
                let framebuffer = backend
                    .add_framebuffer(
                        allocation.buffers[index]
                            .buffer
                            .as_ref()
                            .expect("buffer was just inserted"),
                    )
                    .with_context(|| format!("create framebuffer {index}"))?;
                allocation.buffers[index].framebuffer = Some(framebuffer);
            }

            ensure!(
                allocation.buffers[0].pitch == allocation.buffers[1].pitch,
                "dumb buffer pitches differ: {} vs {}",
                allocation.buffers[0].pitch,
                allocation.buffers[1].pitch
            );
            allocation.pitch = allocation.buffers[0].pitch;

            let mapping_lengths = {
                let mut mapped = allocation
                    .map(backend, counters)
                    .context("probe scanout mappings")?;
                for index in 0..BUFFERS_PER_SCANOUT {
                    let mapping = mapped.buffer_mut(index);
                    mapping.fill(0);
                }
                mapped.mapping_lengths()
            };
            allocation.live_bytes =
                mapping_lengths
                    .into_iter()
                    .try_fold(0usize, |total, length| {
                        total
                            .checked_add(length)
                            .context("scanout mapping byte accounting overflow")
                    })?;
            allocation.buffers[0].mapping_len = mapping_lengths[0];
            allocation.buffers[1].mapping_len = mapping_lengths[1];

            Ok(())
        })();

        if let Err(err) = result {
            let cleanup = allocation.release(backend, counters);
            return match cleanup {
                Ok(()) => Err(err),
                Err(cleanup_err) => Err(err.context(format!(
                    "partial scanout cleanup also failed: {cleanup_err:#}"
                ))),
            };
        }

        counters.allocation();
        Ok(allocation)
    }

    #[allow(dead_code)]
    pub(crate) fn mode(&self) -> B::Mode {
        self.mode
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn pitch(&self) -> u32 {
        self.pitch
    }

    pub(crate) fn mapping_lengths(&self) -> [usize; BUFFERS_PER_SCANOUT] {
        [self.buffers[0].mapping_len, self.buffers[1].mapping_len]
    }

    pub(crate) fn live_bytes(&self) -> usize {
        self.live_bytes
    }

    pub(crate) fn flush_black_initialization(&self, backend: &mut B) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        for (index, buffer) in self.buffers.iter().enumerate() {
            let Some(framebuffer) = buffer.framebuffer else {
                continue;
            };
            if let Err(err) =
                backend.dirty_framebuffer(framebuffer, 0, 0, self.width as u16, self.height as u16)
            {
                errors.push(format!("flush candidate framebuffer {index}: {err:#}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }

    pub(crate) fn map<'a>(
        &'a mut self,
        backend: &mut B,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<MappedActive<'a, B>> {
        ensure!(!self.released, "cannot map a released scanout allocation");
        let [first, second] = &mut self.buffers;
        let first_mapping = backend
            .map_buffer(
                first
                    .buffer
                    .as_mut()
                    .context("first dumb buffer is not available")?,
            )
            .context("map first dumb buffer")?;
        counters.map();
        let mut first_mapping = TrackedMapping {
            mapping: first_mapping,
            counters: counters.clone(),
        };
        let second_mapping = backend
            .map_buffer(
                second
                    .buffer
                    .as_mut()
                    .context("second dumb buffer is not available")?,
            )
            .context("map second dumb buffer")?;
        counters.map();
        let mut second_mapping = TrackedMapping {
            mapping: second_mapping,
            counters: counters.clone(),
        };

        let framebuffers = [
            first
                .framebuffer
                .context("first framebuffer is not available")?,
            second
                .framebuffer
                .context("second framebuffer is not available")?,
        ];
        let mapping_lengths = [first_mapping.as_mut().len(), second_mapping.as_mut().len()];

        Ok(MappedActive {
            mappings: [first_mapping, second_mapping],
            framebuffers,
            mapping_lengths,
            pitch: self.pitch,
            width: self.width,
            height: self.height,
            mode: self.mode,
            front_buffer_index: &mut self.front_buffer_index,
        })
    }

    pub(crate) fn release(
        &mut self,
        backend: &mut B,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<()> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let mut errors = Vec::new();

        for index in (0..BUFFERS_PER_SCANOUT).rev() {
            if let Some(framebuffer) = self.buffers[index].framebuffer.take() {
                match backend.remove_framebuffer(framebuffer) {
                    Ok(()) => counters.framebuffer_remove(),
                    Err(err) => errors.push(format!("remove framebuffer {index}: {err:#}")),
                }
            }
        }
        for index in (0..BUFFERS_PER_SCANOUT).rev() {
            if let Some(buffer) = self.buffers[index].buffer.take() {
                match backend.destroy_buffer(buffer) {
                    Ok(()) => counters.dumb_buffer_destroy(),
                    Err(err) => errors.push(format!("destroy dumb buffer {index}: {err:#}")),
                }
            }
        }
        counters.release();
        self.live_bytes = 0;

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }
}

pub(crate) struct MappedActive<'a, B: ScanoutBackend + 'a> {
    mappings: [TrackedMapping<'a, B>; BUFFERS_PER_SCANOUT],
    framebuffers: [B::Framebuffer; BUFFERS_PER_SCANOUT],
    mapping_lengths: [usize; BUFFERS_PER_SCANOUT],
    pitch: u32,
    width: u32,
    height: u32,
    mode: B::Mode,
    front_buffer_index: &'a mut usize,
}

impl<'a, B: ScanoutBackend + 'a> MappedActive<'a, B> {
    pub(crate) fn mode(&self) -> B::Mode {
        self.mode
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(crate) fn pitch(&self) -> u32 {
        self.pitch
    }

    pub(crate) fn mapping_lengths(&self) -> [usize; BUFFERS_PER_SCANOUT] {
        self.mapping_lengths
    }

    #[allow(dead_code)]
    pub(crate) fn front_index(&self) -> usize {
        *self.front_buffer_index
    }

    pub(crate) fn back_index(&self) -> usize {
        self.front_index() ^ 1
    }

    pub(crate) fn set_front_index(&mut self, index: usize) {
        assert!(index < BUFFERS_PER_SCANOUT);
        *self.front_buffer_index = index;
    }

    pub(crate) fn framebuffer(&self, index: usize) -> B::Framebuffer {
        self.framebuffers[index]
    }

    pub(crate) fn front_framebuffer(&self) -> B::Framebuffer {
        self.framebuffer(self.front_index())
    }

    pub(crate) fn back_framebuffer(&self) -> B::Framebuffer {
        self.framebuffer(self.back_index())
    }

    pub(crate) fn buffer_mut(&mut self, index: usize) -> &mut [u8] {
        self.mappings[index].as_mut()
    }

    pub(crate) fn front_buffer_mut(&mut self) -> &mut [u8] {
        let index = self.front_index();
        self.buffer_mut(index)
    }

    pub(crate) fn back_buffer_mut(&mut self) -> &mut [u8] {
        let index = self.back_index();
        self.buffer_mut(index)
    }
}

pub(crate) enum StableMappedActive<'a, B: ScanoutBackend + 'a> {
    Slot0(MappedActive<'a, B>),
    Slot1(MappedActive<'a, B>),
    Slot2(MappedActive<'a, B>),
}

impl<'a, B: ScanoutBackend + 'a> StableMappedActive<'a, B> {
    pub(crate) fn index(&self) -> usize {
        match self {
            Self::Slot0(_) => 0,
            Self::Slot1(_) => 1,
            Self::Slot2(_) => 2,
        }
    }

    fn mapped(&self) -> &MappedActive<'a, B> {
        match self {
            Self::Slot0(mapped) | Self::Slot1(mapped) | Self::Slot2(mapped) => mapped,
        }
    }

    fn mapped_mut(&mut self) -> &mut MappedActive<'a, B> {
        match self {
            Self::Slot0(mapped) | Self::Slot1(mapped) | Self::Slot2(mapped) => mapped,
        }
    }

    pub(crate) fn mode(&self) -> B::Mode {
        self.mapped().mode()
    }

    pub(crate) fn size(&self) -> (u32, u32) {
        self.mapped().size()
    }

    pub(crate) fn pitch(&self) -> u32 {
        self.mapped().pitch()
    }

    pub(crate) fn mapping_lengths(&self) -> [usize; BUFFERS_PER_SCANOUT] {
        self.mapped().mapping_lengths()
    }

    #[allow(dead_code)]
    pub(crate) fn front_index(&self) -> usize {
        self.mapped().front_index()
    }

    pub(crate) fn back_index(&self) -> usize {
        self.mapped().back_index()
    }

    pub(crate) fn set_front_index(&mut self, index: usize) {
        self.mapped_mut().set_front_index(index);
    }

    pub(crate) fn front_framebuffer(&self) -> B::Framebuffer {
        self.mapped().front_framebuffer()
    }

    pub(crate) fn back_framebuffer(&self) -> B::Framebuffer {
        self.mapped().back_framebuffer()
    }

    pub(crate) fn buffer_mut(&mut self, index: usize) -> &mut [u8] {
        self.mapped_mut().buffer_mut(index)
    }

    pub(crate) fn front_buffer_mut(&mut self) -> &mut [u8] {
        self.mapped_mut().front_buffer_mut()
    }

    pub(crate) fn back_buffer_mut(&mut self) -> &mut [u8] {
        self.mapped_mut().back_buffer_mut()
    }
}

#[cfg(test)]
pub(crate) struct MappedTransition<'old, 'target, B: ScanoutBackend + 'old + 'target> {
    pub(crate) old: MappedActive<'old, B>,
    pub(crate) target: MappedActive<'target, B>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScanoutRoles {
    pub(crate) active: usize,
    pub(crate) baseline: usize,
    pub(crate) candidate: Option<usize>,
}

pub(crate) struct ScanoutPool<B: ScanoutBackend> {
    slots: [ScanoutSlot<B>; SCANOUT_SLOT_COUNT],
    roles: ScanoutRoles,
}

impl<B: ScanoutBackend> ScanoutPool<B> {
    fn with_baseline(allocation: ScanoutAllocation<B>) -> Self {
        let mut slots = std::array::from_fn(|_| None);
        slots[0] = Some(Box::new(allocation));
        Self {
            slots,
            roles: ScanoutRoles {
                active: 0,
                baseline: 0,
                candidate: None,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn roles(&self) -> ScanoutRoles {
        self.roles
    }

    #[cfg(test)]
    pub(crate) fn active_mut(&mut self) -> &mut ScanoutAllocation<B> {
        self.slots[self.roles.active]
            .as_deref_mut()
            .expect("active scanout slot is empty")
    }

    #[cfg(test)]
    pub(crate) fn candidate(&self) -> Option<&ScanoutAllocation<B>> {
        self.roles.candidate.map(|index| {
            self.slots[index]
                .as_deref()
                .expect("candidate scanout slot is empty")
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_candidate(
        &mut self,
        backend: &mut B,
        allocation: ScanoutAllocation<B>,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<usize> {
        self.release_candidate(backend, counters)?;
        let index = (0..SCANOUT_SLOT_COUNT)
            .find(|index| *index != self.roles.active && self.slots[*index].is_none())
            .context("no stable scanout slot is available for a candidate")?;
        self.slots[index] = Some(Box::new(allocation));
        self.roles.candidate = Some(index);
        Ok(index)
    }

    #[cfg(test)]
    pub(crate) fn release_candidate(
        &mut self,
        backend: &mut B,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<()> {
        let Some(index) = self.roles.candidate.take() else {
            return Ok(());
        };
        ensure!(
            index != self.roles.active,
            "candidate role aliases active scanout"
        );
        let mut allocation = self.slots[index]
            .take()
            .context("candidate scanout slot is empty")?;
        allocation.release(backend, counters)
    }

    #[allow(dead_code)]
    pub(crate) fn promote_candidate(&mut self) -> anyhow::Result<(usize, usize)> {
        let candidate = self
            .roles
            .candidate
            .take()
            .context("no candidate scanout to promote")?;
        let old_active = self.roles.active;
        self.roles.active = candidate;
        Ok((old_active, candidate))
    }

    #[cfg(test)]
    pub(crate) fn release_inactive(
        &mut self,
        backend: &mut B,
        index: usize,
        counters: &ScanoutCounters,
    ) -> anyhow::Result<()> {
        ensure!(index != self.roles.active, "cannot release active scanout");
        if index == self.roles.baseline {
            return Ok(());
        }
        let Some(mut allocation) = self.slots[index].take() else {
            return Ok(());
        };
        allocation.release(backend, counters)
    }

    fn release_all(&mut self, backend: &mut B, counters: &ScanoutCounters) -> anyhow::Result<()> {
        self.roles.candidate = None;
        let mut errors = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate().rev() {
            if let Some(mut allocation) = slot.take() {
                if let Err(err) = allocation.release(backend, counters) {
                    errors.push(format!("release scanout slot {index}: {err:#}"));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("; "))
        }
    }
}

pub(crate) struct ScanoutManager<B: ScanoutBackend> {
    pool: ScanoutPool<B>,
    counters: ScanoutCounters,
    current_live_bytes: usize,
    observed_peak_live_bytes: usize,
    dynamic_routes: Option<DynamicRouteState<B::Mode>>,
}

impl<B: ScanoutBackend> ScanoutManager<B> {
    pub(crate) fn new_baseline(
        backend: &mut B,
        mode: B::Mode,
    ) -> anyhow::Result<ScanoutManager<B>> {
        let counters = ScanoutCounters::default();
        let mut allocation = ScanoutAllocation::create(backend, mode, &counters)?;
        let live_bytes = allocation.live_bytes();
        if live_bytes.checked_add(0).is_none() {
            allocation.release(backend, &counters)?;
            anyhow::bail!("baseline live-byte accounting overflow");
        }
        Ok(Self {
            pool: ScanoutPool::with_baseline(allocation),
            counters,
            current_live_bytes: live_bytes,
            observed_peak_live_bytes: live_bytes,
            dynamic_routes: None,
        })
    }

    pub(crate) fn initialize_dynamic_routes(
        &mut self,
        baseline_physical_key: ModeKey,
    ) -> anyhow::Result<()> {
        ensure!(
            self.dynamic_routes.is_none(),
            "dynamic route state is already initialized"
        );
        self.dynamic_routes = Some(DynamicRouteState::new(baseline_physical_key));
        Ok(())
    }

    pub(crate) fn split_runtime(
        &mut self,
    ) -> (
        &mut [ScanoutSlot<B>; SCANOUT_SLOT_COUNT],
        ScanoutRuntime<'_, B>,
    ) {
        let Self {
            pool,
            counters,
            current_live_bytes,
            observed_peak_live_bytes,
            dynamic_routes,
        } = self;
        let ScanoutPool { slots, roles } = pool;
        (
            slots,
            ScanoutRuntime {
                roles,
                counters: counters.clone(),
                current_live_bytes,
                observed_peak_live_bytes,
                dynamic_routes,
            },
        )
    }

    pub(crate) fn counters(&self) -> ScanoutCounters {
        self.counters.clone()
    }

    pub(crate) fn counter_snapshot(&self) -> ScanoutCounterSnapshot {
        self.counters.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn roles(&self) -> ScanoutRoles {
        self.pool.roles()
    }

    #[cfg(test)]
    pub(crate) fn active_mut(&mut self) -> &mut ScanoutAllocation<B> {
        self.pool.active_mut()
    }

    #[cfg(test)]
    pub(crate) fn current_live_bytes(&self) -> usize {
        self.current_live_bytes
    }

    #[cfg(test)]
    pub(crate) fn observed_peak_live_bytes(&self) -> usize {
        self.observed_peak_live_bytes
    }

    #[cfg(test)]
    pub(crate) fn stage_candidate(
        &mut self,
        backend: &mut B,
        mode: B::Mode,
    ) -> anyhow::Result<usize> {
        let allocation = ScanoutAllocation::create(backend, mode, &self.counters)?;
        let candidate_bytes = allocation.live_bytes();
        let previous_candidate_bytes = self
            .pool
            .candidate()
            .map_or(0, |candidate| candidate.live_bytes());
        let retained_live_bytes = self
            .current_live_bytes
            .checked_sub(previous_candidate_bytes)
            .context("candidate live-byte accounting underflow")?;
        let new_live_bytes = match checked_add_live_bytes(retained_live_bytes, candidate_bytes) {
            Ok(bytes) => bytes,
            Err(err) => {
                let mut allocation = allocation;
                allocation.release(backend, &self.counters)?;
                return Err(err);
            }
        };
        let index = self
            .pool
            .replace_candidate(backend, allocation, &self.counters)?;
        self.current_live_bytes = new_live_bytes;
        self.observed_peak_live_bytes = self.observed_peak_live_bytes.max(new_live_bytes);
        Ok(index)
    }

    #[cfg(test)]
    pub(crate) fn release_candidate(&mut self, backend: &mut B) -> anyhow::Result<()> {
        if let Some(candidate) = self.pool.candidate() {
            self.current_live_bytes = self
                .current_live_bytes
                .checked_sub(candidate.live_bytes())
                .context("candidate live-byte accounting underflow")?;
        }
        self.pool.release_candidate(backend, &self.counters)
    }

    #[allow(dead_code)]
    pub(crate) fn promote_candidate(&mut self) -> anyhow::Result<(usize, usize)> {
        self.pool.promote_candidate()
    }

    #[cfg(test)]
    pub(crate) fn release_replaced_active(
        &mut self,
        backend: &mut B,
        index: usize,
    ) -> anyhow::Result<()> {
        if index == self.pool.roles.baseline {
            return Ok(());
        }
        let released_bytes = self.pool.slots[index]
            .as_deref()
            .map(ScanoutAllocation::live_bytes)
            .unwrap_or(0);
        self.pool.release_inactive(backend, index, &self.counters)?;
        self.current_live_bytes = self
            .current_live_bytes
            .checked_sub(released_bytes)
            .context("released scanout live-byte accounting underflow")?;
        Ok(())
    }

    pub(crate) fn release_all(&mut self, backend: &mut B) -> anyhow::Result<()> {
        let result = self.pool.release_all(backend, &self.counters);
        self.current_live_bytes = 0;
        result
    }
}

pub(crate) struct ScanoutRuntime<'a, B: ScanoutBackend> {
    roles: &'a mut ScanoutRoles,
    counters: ScanoutCounters,
    current_live_bytes: &'a mut usize,
    observed_peak_live_bytes: &'a mut usize,
    dynamic_routes: &'a mut Option<DynamicRouteState<B::Mode>>,
}

impl<B: ScanoutBackend> ScanoutRuntime<'_, B> {
    pub(crate) fn roles(&self) -> ScanoutRoles {
        *self.roles
    }

    pub(crate) fn counters(&self) -> ScanoutCounters {
        self.counters.clone()
    }

    pub(crate) fn current_live_bytes(&self) -> usize {
        *self.current_live_bytes
    }

    pub(crate) fn observed_peak_live_bytes(&self) -> usize {
        *self.observed_peak_live_bytes
    }

    pub(crate) fn dynamic_routes(&self) -> anyhow::Result<&DynamicRouteState<B::Mode>> {
        self.dynamic_routes
            .as_ref()
            .context("dynamic route state is not initialized")
    }

    pub(crate) fn dynamic_routes_mut(&mut self) -> anyhow::Result<&mut DynamicRouteState<B::Mode>> {
        self.dynamic_routes
            .as_mut()
            .context("dynamic route state is not initialized")
    }

    pub(crate) fn stage_candidate(
        &mut self,
        backend: &mut B,
        slot_index: usize,
        slot: &mut ScanoutSlot<B>,
        mut allocation: ScanoutAllocation<B>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.roles.candidate.is_none(),
            "candidate role is already occupied"
        );
        ensure!(
            slot_index != self.roles.active,
            "candidate slot aliases active scanout"
        );
        ensure!(slot.is_none(), "candidate slot is not empty");
        let new_live_bytes =
            match checked_add_live_bytes(*self.current_live_bytes, allocation.live_bytes()) {
                Ok(bytes) => bytes,
                Err(err) => {
                    allocation.release(backend, &self.counters)?;
                    return Err(err);
                }
            };
        *slot = Some(Box::new(allocation));
        self.roles.candidate = Some(slot_index);
        *self.current_live_bytes = new_live_bytes;
        *self.observed_peak_live_bytes = (*self.observed_peak_live_bytes).max(new_live_bytes);
        Ok(())
    }

    pub(crate) fn release_candidate(
        &mut self,
        backend: &mut B,
        slot_index: usize,
        slot: &mut ScanoutSlot<B>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.roles.candidate == Some(slot_index),
            "slot {slot_index} is not the current candidate"
        );
        ensure!(
            slot_index != self.roles.active,
            "candidate slot aliases active scanout"
        );
        let mut allocation = slot.take().context("candidate scanout slot is empty")?;
        let released_bytes = allocation.live_bytes();
        let release_result = allocation.release(backend, &self.counters);
        *self.current_live_bytes = self
            .current_live_bytes
            .checked_sub(released_bytes)
            .context("candidate live-byte accounting underflow")?;
        self.roles.candidate = None;
        if let Some(routes) = self.dynamic_routes.as_mut() {
            routes.candidate_key = None;
        }
        release_result
    }

    #[allow(dead_code)]
    pub(crate) fn promote_candidate(&mut self) -> anyhow::Result<(usize, usize)> {
        let candidate = self
            .roles
            .candidate
            .take()
            .context("no candidate scanout to promote")?;
        let old_active = self.roles.active;
        self.roles.active = candidate;
        Ok((old_active, candidate))
    }

    pub(crate) fn activate_slot(&mut self, target: usize) -> anyhow::Result<usize> {
        ensure!(target < SCANOUT_SLOT_COUNT, "invalid scanout slot {target}");
        let old_active = self.roles.active;
        if self.roles.candidate == Some(target) {
            self.roles.candidate = None;
        } else {
            ensure!(
                target == self.roles.baseline,
                "target slot {target} is neither the candidate nor the baseline"
            );
        }
        self.roles.active = target;
        Ok(old_active)
    }

    pub(crate) fn release_inactive(
        &mut self,
        backend: &mut B,
        slot_index: usize,
        slot: &mut ScanoutSlot<B>,
    ) -> anyhow::Result<()> {
        ensure!(
            slot_index != self.roles.active,
            "cannot release active scanout"
        );
        if slot_index == self.roles.baseline {
            return Ok(());
        }
        let Some(mut allocation) = slot.take() else {
            return Ok(());
        };
        let released_bytes = allocation.live_bytes();
        let release_result = allocation.release(backend, &self.counters);
        *self.current_live_bytes = self
            .current_live_bytes
            .checked_sub(released_bytes)
            .context("released scanout live-byte accounting underflow")?;
        release_result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gud_gadget::{DisplayMode, DisplayStateSnapshot, GUD_PIXEL_FORMAT_RGB565};

    use crate::modes::{ModeKey, PhysicalCatalogMode, RouteCatalog};
    use crate::{
        attempt_mapped_switch, invalidate_dynamic_pending_off_baseline, prepare_dynamic_check,
        MappedSwitch,
    };

    use super::{
        checked_add_live_bytes, logical_memory_estimate, MappedTransition, ScanoutAllocation,
        ScanoutBackend, ScanoutCounters, ScanoutManager, BUFFERS_PER_SCANOUT,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Operation {
        CreateBuffer,
        AddFramebuffer,
        MapBuffer,
        RemoveFramebuffer,
        DestroyBuffer,
        SetCrtc,
        Dirty,
        PageFlip,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Failure {
        operation: Operation,
        occurrence: usize,
    }

    #[derive(Debug)]
    struct MockBuffer {
        id: u32,
        pitch: u32,
        bytes: Vec<u8>,
    }

    struct MockMapping<'a>(&'a mut [u8]);

    impl AsMut<[u8]> for MockMapping<'_> {
        fn as_mut(&mut self) -> &mut [u8] {
            self.0
        }
    }

    #[derive(Default)]
    struct MockBackend {
        next_id: u32,
        failure: Option<Failure>,
        operation_counts: [usize; 8],
        live_buffers: BTreeSet<u32>,
        live_framebuffers: BTreeSet<u32>,
        mapping_padding: usize,
    }

    impl MockBackend {
        fn with_failure(operation: Operation, occurrence: usize) -> Self {
            Self {
                failure: Some(Failure {
                    operation,
                    occurrence,
                }),
                ..Self::default()
            }
        }

        fn operation_index(operation: Operation) -> usize {
            match operation {
                Operation::CreateBuffer => 0,
                Operation::AddFramebuffer => 1,
                Operation::MapBuffer => 2,
                Operation::RemoveFramebuffer => 3,
                Operation::DestroyBuffer => 4,
                Operation::SetCrtc => 5,
                Operation::Dirty => 6,
                Operation::PageFlip => 7,
            }
        }

        fn hit(&mut self, operation: Operation) -> anyhow::Result<()> {
            let index = Self::operation_index(operation);
            self.operation_counts[index] += 1;
            if self.failure
                == Some(Failure {
                    operation,
                    occurrence: self.operation_counts[index],
                })
            {
                anyhow::bail!("injected {operation:?} failure");
            }
            Ok(())
        }

        fn count(&self, operation: Operation) -> usize {
            self.operation_counts[Self::operation_index(operation)]
        }
    }

    impl ScanoutBackend for MockBackend {
        type Buffer = MockBuffer;
        type Framebuffer = u32;
        type Mode = (u32, u32);
        type Mapping<'a> = MockMapping<'a>;

        fn mode_size(&self, mode: Self::Mode) -> (u32, u32) {
            mode
        }

        fn create_buffer(&mut self, width: u32, height: u32) -> anyhow::Result<Self::Buffer> {
            self.hit(Operation::CreateBuffer)?;
            self.next_id += 1;
            let id = self.next_id;
            self.live_buffers.insert(id);
            let pitch = width.checked_mul(2).unwrap();
            let len = pitch as usize * height as usize + self.mapping_padding;
            Ok(MockBuffer {
                id,
                pitch,
                bytes: vec![0; len],
            })
        }

        fn buffer_pitch(&self, buffer: &Self::Buffer) -> u32 {
            buffer.pitch
        }

        fn add_framebuffer(&mut self, buffer: &Self::Buffer) -> anyhow::Result<Self::Framebuffer> {
            self.hit(Operation::AddFramebuffer)?;
            let id = buffer.id + 10_000;
            self.live_framebuffers.insert(id);
            Ok(id)
        }

        fn map_buffer<'a>(
            &mut self,
            buffer: &'a mut Self::Buffer,
        ) -> anyhow::Result<Self::Mapping<'a>> {
            self.hit(Operation::MapBuffer)?;
            Ok(MockMapping(buffer.bytes.as_mut_slice()))
        }

        fn remove_framebuffer(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
            self.hit(Operation::RemoveFramebuffer)?;
            self.live_framebuffers.remove(&framebuffer);
            Ok(())
        }

        fn destroy_buffer(&mut self, buffer: Self::Buffer) -> anyhow::Result<()> {
            self.hit(Operation::DestroyBuffer)?;
            self.live_buffers.remove(&buffer.id);
            Ok(())
        }

        fn dirty_framebuffer(
            &mut self,
            _framebuffer: Self::Framebuffer,
            _x1: u16,
            _y1: u16,
            _x2: u16,
            _y2: u16,
        ) -> anyhow::Result<()> {
            self.hit(Operation::Dirty)
        }

        fn set_crtc(
            &mut self,
            _framebuffer: Self::Framebuffer,
            _mode: Self::Mode,
        ) -> anyhow::Result<()> {
            self.hit(Operation::SetCrtc)
        }

        fn page_flip(&mut self, _framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
            self.hit(Operation::PageFlip)
        }
    }

    fn display_mode(clock: u32, width: u16, height: u16) -> DisplayMode {
        DisplayMode {
            clock,
            hdisplay: width,
            hsync_start: width + 4,
            hsync_end: width + 8,
            htotal: width + 16,
            vdisplay: height,
            vsync_start: height + 1,
            vsync_end: height + 2,
            vtotal: height + 4,
            flags: 0,
        }
    }

    fn snapshot(mode: &DisplayMode, generation: u64) -> DisplayStateSnapshot {
        DisplayStateSnapshot {
            mode: mode.clone(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
            generation,
        }
    }

    #[test]
    fn partial_construction_failures_release_every_created_resource() {
        for (operation, occurrences) in [
            (Operation::CreateBuffer, 2),
            (Operation::AddFramebuffer, 2),
            (Operation::MapBuffer, 2),
        ] {
            for occurrence in 1..=occurrences {
                let mut backend = MockBackend::with_failure(operation, occurrence);
                let counters = ScanoutCounters::default();

                assert!(
                    ScanoutAllocation::create(&mut backend, (64, 32), &counters).is_err(),
                    "{operation:?} occurrence {occurrence}"
                );
                assert!(backend.live_buffers.is_empty());
                assert!(backend.live_framebuffers.is_empty());
            }
        }
    }

    #[test]
    fn release_is_explicit_idempotent_and_ordered_by_resource_kind() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut allocation = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();

        allocation.release(&mut backend, &counters).unwrap();
        allocation.release(&mut backend, &counters).unwrap();

        assert_eq!(backend.count(Operation::RemoveFramebuffer), 2);
        assert_eq!(backend.count(Operation::DestroyBuffer), 2);
        assert!(backend.live_buffers.is_empty());
        assert!(backend.live_framebuffers.is_empty());
        let snapshot = counters.snapshot();
        assert_eq!(snapshot.releases, 1);
        assert_eq!(snapshot.framebuffer_removes, 2);
        assert_eq!(snapshot.dumb_buffer_destroys, 2);
    }

    #[test]
    fn mapped_transition_keeps_old_and_target_live_together() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut old = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
        let mut target = ScanoutAllocation::create(&mut backend, (80, 40), &counters).unwrap();

        {
            let old = old.map(&mut backend, &counters).unwrap();
            let target = target.map(&mut backend, &counters).unwrap();
            let mut transition = MappedTransition { old, target };
            transition.old.front_buffer_mut()[0] = 0x11;
            transition.target.front_buffer_mut()[0] = 0x22;
            backend
                .set_crtc(
                    transition.target.front_framebuffer(),
                    transition.target.mode(),
                )
                .unwrap();
            assert_eq!(transition.old.front_buffer_mut()[0], 0x11);
            assert_eq!(transition.target.front_buffer_mut()[0], 0x22);
        }

        assert_eq!(counters.snapshot().maps, counters.snapshot().unmaps);
        old.release(&mut backend, &counters).unwrap();
        target.release(&mut backend, &counters).unwrap();
    }

    #[test]
    fn successful_commit_switches_once_and_keeps_target_mapping_live() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut old = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
        let mut target = ScanoutAllocation::create(&mut backend, (80, 40), &counters).unwrap();
        let mut old_mapped = old.map(&mut backend, &counters).unwrap();
        old_mapped.front_buffer_mut()[0] = 0x35;
        let maps_before = counters.snapshot().maps;
        let sets_before = backend.count(Operation::SetCrtc);

        let mut active =
            match attempt_mapped_switch(&mut backend, old_mapped, &mut target, &counters) {
                MappedSwitch::Switched { active, .. } => active,
                MappedSwitch::Retained { error, .. } => {
                    panic!("unexpected retained route: {error:#}")
                }
            };
        counters.switched();

        assert_eq!(backend.count(Operation::SetCrtc), sets_before + 1);
        assert_eq!(
            counters.snapshot().maps,
            maps_before + BUFFERS_PER_SCANOUT as u64
        );
        assert_eq!(
            counters.snapshot().maps,
            counters.snapshot().unmaps + BUFFERS_PER_SCANOUT as u64
        );
        let maps_after_switch = counters.snapshot().maps;
        for _ in 0..10 {
            counters.no_op();
            active.front_buffer_mut()[0] ^= 1;
        }
        assert_eq!(backend.count(Operation::SetCrtc), sets_before + 1);
        assert_eq!(counters.snapshot().maps, maps_after_switch);
        assert_eq!(counters.snapshot().switches, 1);
        assert_eq!(counters.snapshot().no_ops, 10);

        drop(active);
        old.release(&mut backend, &counters).unwrap();
        target.release(&mut backend, &counters).unwrap();
        assert_eq!(counters.snapshot().maps, counters.snapshot().unmaps);
    }

    #[test]
    fn commit_switch_failure_retains_old_mapping_without_retry() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut old = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
        let mut target = ScanoutAllocation::create(&mut backend, (80, 40), &counters).unwrap();
        let mut old_mapped = old.map(&mut backend, &counters).unwrap();
        old_mapped.front_buffer_mut()[0] = 0x6b;
        let set_occurrence = backend.count(Operation::SetCrtc) + 1;
        backend.failure = Some(Failure {
            operation: Operation::SetCrtc,
            occurrence: set_occurrence,
        });

        let mut active =
            match attempt_mapped_switch(&mut backend, old_mapped, &mut target, &counters) {
                MappedSwitch::Retained { active, .. } => active,
                MappedSwitch::Switched { .. } => panic!("injected set_crtc unexpectedly succeeded"),
            };

        assert_eq!(backend.count(Operation::SetCrtc), set_occurrence);
        assert_eq!(active.front_buffer_mut()[0], 0x6b);
        assert_eq!(
            counters.snapshot().maps,
            counters.snapshot().unmaps + BUFFERS_PER_SCANOUT as u64
        );
        active.front_buffer_mut()[1] = 0x4d;
        assert_eq!(active.front_buffer_mut()[1], 0x4d);

        drop(active);
        backend.failure = None;
        old.release(&mut backend, &counters).unwrap();
        target.release(&mut backend, &counters).unwrap();
        assert_eq!(counters.snapshot().maps, counters.snapshot().unmaps);
    }

    #[test]
    fn target_map_failure_does_not_attempt_a_modeset() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut old = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
        let mut target = ScanoutAllocation::create(&mut backend, (80, 40), &counters).unwrap();
        let mut old_mapped = old.map(&mut backend, &counters).unwrap();
        old_mapped.front_buffer_mut()[0] = 0x2a;
        let sets_before = backend.count(Operation::SetCrtc);
        backend.failure = Some(Failure {
            operation: Operation::MapBuffer,
            occurrence: backend.count(Operation::MapBuffer) + 1,
        });

        let mut active =
            match attempt_mapped_switch(&mut backend, old_mapped, &mut target, &counters) {
                MappedSwitch::Retained { active, .. } => active,
                MappedSwitch::Switched { .. } => panic!("injected map unexpectedly succeeded"),
            };

        assert_eq!(backend.count(Operation::SetCrtc), sets_before);
        assert_eq!(active.front_buffer_mut()[0], 0x2a);
        drop(active);
        backend.failure = None;
        old.release(&mut backend, &counters).unwrap();
        target.release(&mut backend, &counters).unwrap();
    }

    #[test]
    fn front_index_persists_across_mapped_scopes() {
        let mut backend = MockBackend::default();
        let counters = ScanoutCounters::default();
        let mut allocation = ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
        {
            let mut mapped = allocation.map(&mut backend, &counters).unwrap();
            assert_eq!(mapped.front_index(), 0);
            mapped.set_front_index(1);
        }
        assert_eq!(
            allocation
                .map(&mut backend, &counters)
                .unwrap()
                .front_index(),
            1
        );
        allocation.release(&mut backend, &counters).unwrap();
    }

    #[test]
    fn disabled_and_same_mode_no_ops_do_not_remap() {
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        let counters = manager.counters();
        {
            let mut active = manager.active_mut().map(&mut backend, &counters).unwrap();
            let maps_before = counters.snapshot().maps;
            for _ in 0..100 {
                counters.no_op();
                active.front_buffer_mut()[0] ^= 1;
            }
            assert_eq!(counters.snapshot().maps, maps_before);
            assert_eq!(counters.snapshot().unmaps + 2, maps_before);
        }
        assert_eq!(counters.snapshot().maps, counters.snapshot().unmaps);
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn candidate_replacement_stays_bounded() {
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();

        for width in 65..165 {
            manager.stage_candidate(&mut backend, (width, 32)).unwrap();
            assert_eq!(backend.live_buffers.len(), 4);
            assert_eq!(backend.live_framebuffers.len(), 4);
        }

        manager.release_candidate(&mut backend).unwrap();
        assert_eq!(backend.live_buffers.len(), BUFFERS_PER_SCANOUT);
        assert_eq!(backend.live_framebuffers.len(), BUFFERS_PER_SCANOUT);
        manager.release_all(&mut backend).unwrap();
        assert!(backend.live_buffers.is_empty());
        assert!(backend.live_framebuffers.is_empty());
    }

    #[test]
    fn actual_mapping_lengths_drive_current_and_peak_accounting() {
        let mut backend = MockBackend {
            mapping_padding: 4096,
            ..MockBackend::default()
        };
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        let baseline_bytes = 2 * (64 * 32 * 2 + 4096);
        assert_eq!(manager.current_live_bytes(), baseline_bytes);
        assert_eq!(manager.observed_peak_live_bytes(), baseline_bytes);

        manager.stage_candidate(&mut backend, (80, 40)).unwrap();
        let candidate_bytes = 2 * (80 * 40 * 2 + 4096);
        assert_eq!(
            manager.current_live_bytes(),
            baseline_bytes + candidate_bytes
        );
        assert_eq!(
            manager.observed_peak_live_bytes(),
            baseline_bytes + candidate_bytes
        );

        manager.release_candidate(&mut backend).unwrap();
        assert_eq!(manager.current_live_bytes(), baseline_bytes);
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn logical_estimates_cover_1080p_4k_and_overflow() {
        let estimate =
            logical_memory_estimate([(1920, 1080), (1280, 720)], [(1920, 1080), (3840, 2160)], 2)
                .unwrap();

        assert_eq!(estimate.max_shadow_bytes, 3840 * 2160 * 2);
        assert_eq!(estimate.scanout_bytes, 3 * 2 * 1920 * 1080 * 2);
        assert_eq!(
            estimate.peak_bytes,
            estimate.scanout_bytes + estimate.max_shadow_bytes
        );
        assert!(logical_memory_estimate([(u32::MAX, u32::MAX)], [(1, 1)], 2).is_err());
        assert!(logical_memory_estimate([], [(1, 1)], 2).is_err());
        assert!(logical_memory_estimate([(1, 1)], [], 2).is_err());
    }

    #[test]
    fn accounting_overflow_releases_the_new_candidate() {
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        manager.current_live_bytes = usize::MAX;

        assert!(manager.stage_candidate(&mut backend, (80, 40)).is_err());
        assert_eq!(backend.live_buffers.len(), BUFFERS_PER_SCANOUT);
        assert_eq!(backend.live_framebuffers.len(), BUFFERS_PER_SCANOUT);
        assert!(manager.pool.candidate().is_none());
        assert!(checked_add_live_bytes(usize::MAX, 1).is_err());

        manager.current_live_bytes = manager.pool.active_mut().live_bytes();
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn preparation_failures_retain_the_active_allocation() {
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        backend.failure = Some(Failure {
            operation: Operation::MapBuffer,
            occurrence: backend.count(Operation::MapBuffer) + 1,
        });

        assert!(manager.stage_candidate(&mut backend, (80, 40)).is_err());
        assert_eq!(manager.roles().active, manager.roles().baseline);
        assert!(manager.pool.candidate().is_none());
        assert_eq!(backend.live_buffers.len(), BUFFERS_PER_SCANOUT);
        assert_eq!(backend.live_framebuffers.len(), BUFFERS_PER_SCANOUT);

        backend.failure = None;
        let counters = manager.counters();
        {
            let mut active = manager.active_mut().map(&mut backend, &counters).unwrap();
            active.front_buffer_mut()[0] = 0x7a;
            assert_eq!(active.front_buffer_mut()[0], 0x7a);
        }
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn presentation_failures_leave_the_mapped_active_buffer_usable() {
        for operation in [Operation::Dirty, Operation::PageFlip, Operation::SetCrtc] {
            let mut backend = MockBackend::default();
            let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
            let counters = manager.counters();
            {
                let mut active = manager.active_mut().map(&mut backend, &counters).unwrap();
                active.front_buffer_mut()[0] = 0x4c;
                backend.failure = Some(Failure {
                    operation,
                    occurrence: 1,
                });
                let result = match operation {
                    Operation::Dirty => {
                        backend.dirty_framebuffer(active.front_framebuffer(), 0, 0, 64, 32)
                    }
                    Operation::PageFlip => backend.page_flip(active.front_framebuffer()),
                    Operation::SetCrtc => {
                        backend.set_crtc(active.front_framebuffer(), active.mode())
                    }
                    _ => unreachable!(),
                };
                assert!(result.is_err());
                assert_eq!(active.front_buffer_mut()[0], 0x4c);
            }
            backend.failure = None;
            manager.release_all(&mut backend).unwrap();
        }
    }

    #[test]
    fn stable_roles_change_without_moving_allocations() {
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        let baseline_address = manager.pool.slots[manager.roles().baseline]
            .as_deref()
            .unwrap() as *const _;
        let candidate_index = manager.stage_candidate(&mut backend, (80, 40)).unwrap();
        let candidate_address = manager.pool.slots[candidate_index].as_deref().unwrap() as *const _;

        let (old_active, new_active) = manager.promote_candidate().unwrap();

        assert_eq!(old_active, manager.roles().baseline);
        assert_eq!(new_active, candidate_index);
        assert_eq!(
            manager.pool.slots[manager.roles().baseline]
                .as_deref()
                .unwrap() as *const _,
            baseline_address
        );
        assert_eq!(
            manager.pool.slots[new_active].as_deref().unwrap() as *const _,
            candidate_address
        );

        manager
            .release_replaced_active(&mut backend, old_active)
            .unwrap();
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn cleanup_failures_are_deterministic_and_do_not_double_release() {
        for operation in [Operation::RemoveFramebuffer, Operation::DestroyBuffer] {
            let mut backend = MockBackend::default();
            let counters = ScanoutCounters::default();
            let mut allocation =
                ScanoutAllocation::create(&mut backend, (64, 32), &counters).unwrap();
            backend.failure = Some(Failure {
                operation,
                occurrence: 1,
            });

            assert!(allocation.release(&mut backend, &counters).is_err());
            let counts_after_failure = (
                backend.count(Operation::RemoveFramebuffer),
                backend.count(Operation::DestroyBuffer),
            );
            allocation.release(&mut backend, &counters).unwrap();
            assert_eq!(
                counts_after_failure,
                (
                    backend.count(Operation::RemoveFramebuffer),
                    backend.count(Operation::DestroyBuffer)
                )
            );
        }
    }

    #[test]
    fn dynamic_checks_prepare_rekey_and_invalidate_one_candidate() {
        let baseline_timing = display_mode(1_000, 64, 32);
        let candidate_timing = display_mode(2_000, 80, 40);
        let catalog = RouteCatalog::build(
            0,
            &[
                PhysicalCatalogMode {
                    mode: (64, 32),
                    timing: baseline_timing.clone(),
                },
                PhysicalCatalogMode {
                    mode: (80, 40),
                    timing: candidate_timing.clone(),
                },
            ],
            0,
            &[],
        )
        .unwrap();
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        manager
            .initialize_dynamic_routes(ModeKey::new(0, &baseline_timing))
            .unwrap();
        let counters = manager.counters();
        let (slots, mut runtime) = manager.split_runtime();
        let [slot0, slot1, slot2] = slots;
        let mut active = slot0
            .as_deref_mut()
            .unwrap()
            .map(&mut backend, &counters)
            .unwrap();
        active.front_buffer_mut()[0] = 0x5a;
        let set_crtc_before = backend.count(Operation::SetCrtc);

        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&candidate_timing, 1),
            0,
        )
        .unwrap();
        let allocations_after_first = counters.snapshot().allocations;
        let candidate_slot = runtime.roles().candidate.unwrap();
        assert!(matches!(
            runtime
                .dynamic_routes()
                .unwrap()
                .pending_plan
                .as_ref()
                .unwrap()
                .kind,
            super::PendingPlanKind::ExactCandidate { .. }
        ));

        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&candidate_timing, 2),
            0,
        )
        .unwrap();
        assert_eq!(counters.snapshot().allocations, allocations_after_first);
        assert_eq!(runtime.roles().candidate, Some(candidate_slot));
        assert_eq!(
            runtime
                .dynamic_routes()
                .unwrap()
                .pending_plan
                .as_ref()
                .unwrap()
                .snapshot
                .generation,
            2
        );
        assert_eq!(active.front_buffer_mut()[0], 0x5a);

        invalidate_dynamic_pending_off_baseline(&mut backend, &mut runtime, slot1, slot2).unwrap();
        assert!(runtime.roles().candidate.is_none());
        assert!(runtime.dynamic_routes().unwrap().pending_plan.is_none());
        assert_eq!(active.front_buffer_mut()[0], 0x5a);
        assert_eq!(backend.count(Operation::SetCrtc), set_crtc_before);

        drop(active);
        drop(runtime);
        manager.release_all(&mut backend).unwrap();
    }

    #[test]
    fn failed_route_cache_is_checked_before_a_new_allocation() {
        let baseline_timing = display_mode(1_000, 64, 32);
        let candidate_timing = display_mode(2_000, 80, 40);
        let synthetic_timing = display_mode(3_000, 70, 70);
        let catalog = RouteCatalog::build(
            0,
            &[
                PhysicalCatalogMode {
                    mode: (64, 32),
                    timing: baseline_timing.clone(),
                },
                PhysicalCatalogMode {
                    mode: (80, 40),
                    timing: candidate_timing.clone(),
                },
            ],
            0,
            std::slice::from_ref(&synthetic_timing),
        )
        .unwrap();
        let mut backend = MockBackend::default();
        let mut manager = ScanoutManager::new_baseline(&mut backend, (64, 32)).unwrap();
        manager
            .initialize_dynamic_routes(ModeKey::new(0, &baseline_timing))
            .unwrap();
        let counters = manager.counters();
        let (slots, mut runtime) = manager.split_runtime();
        let [slot0, slot1, slot2] = slots;
        let active = slot0
            .as_deref_mut()
            .unwrap()
            .map(&mut backend, &counters)
            .unwrap();
        backend.failure = Some(Failure {
            operation: Operation::CreateBuffer,
            occurrence: backend.count(Operation::CreateBuffer) + 1,
        });

        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&candidate_timing, 1),
            0,
        )
        .unwrap();
        assert!(matches!(
            runtime
                .dynamic_routes()
                .unwrap()
                .pending_plan
                .as_ref()
                .unwrap()
                .kind,
            super::PendingPlanKind::ScaledCurrentAfterFailure(super::FallbackReason::PrepareFailed)
        ));
        assert_eq!(runtime.dynamic_routes().unwrap().failed_routes.len(), 1);

        backend.failure = None;
        let creates_before_cache_hit = backend.count(Operation::CreateBuffer);
        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&candidate_timing, 2),
            0,
        )
        .unwrap();
        assert_eq!(
            backend.count(Operation::CreateBuffer),
            creates_before_cache_hit
        );
        assert!(
            runtime
                .dynamic_routes()
                .unwrap()
                .pending_plan
                .as_ref()
                .unwrap()
                .failed_cache_hit
        );

        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&synthetic_timing, 3),
            0,
        )
        .unwrap();
        prepare_dynamic_check(
            &mut backend,
            &mut runtime,
            slot1,
            slot2,
            &catalog,
            snapshot(&candidate_timing, 4),
            0,
        )
        .unwrap();
        assert_eq!(
            backend.count(Operation::CreateBuffer),
            creates_before_cache_hit
        );

        drop(active);
        drop(runtime);
        manager.release_all(&mut backend).unwrap();
    }
}

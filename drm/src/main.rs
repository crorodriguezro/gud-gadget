mod modes;
mod scanout;

use anyhow::{ensure, Context};
use drm::buffer::Buffer;
use drm::control::{
    connector, crtc, dumbbuffer::DumbBuffer, dumbbuffer::DumbMapping, framebuffer, ClipRect,
    Device, Mode, ModeTypeFlags, PageFlipFlags,
};
use gud_gadget::{
    DisplayMode, Event, ProtocolInvalidationReason, GUD_COMPRESSION_LZ4,
    GUD_DISPLAY_FLAG_STATUS_ON_SET,
};
use modes::{CatalogRoute, ModeKey, PhysicalCatalogMode, RouteCatalog};
use scanout::{
    logical_memory_estimate, FallbackReason, MappedActive, PendingPlan, PendingPlanKind,
    PresentationRoute, ScanoutAllocation, ScanoutBackend, ScanoutCounters, ScanoutManager,
    ScanoutRuntime, ScanoutSlot, StableMappedActive,
};
use std::env::{args, var_os};
use std::ffi::OsStr;
use std::fs::{rename, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};
use usb_gadget::function::custom::{Custom, Event as FunctionFsEvent, Interface};
use usb_gadget::{default_udc, Class, Config, Gadget, RegGadget, Strings};

const CRTC_SET_RETRIES: usize = 20;
const CRTC_SET_RETRY_DELAY: Duration = Duration::from_millis(250);

fn record_host_activity(had_host_session: &mut bool, event: &Event<'_>) {
    if event.is_host_activity() && !*had_host_session {
        *had_host_session = true;
        info!(
            had_host_session = true,
            event = ?event,
            "session_activity"
        );
    }
}

const BULK_RECEIVE_DEADLINE: Duration = Duration::from_millis(1_000);
const STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE: Duration = Duration::from_millis(1_000);
const STATUS_ON_SET_CLEANUP_STATUS_COUNT: u8 = 2;
const STATUS_ON_SET_DIAGNOSTIC_TRANSACTION_LIMIT: u8 = 2;
const E1_T05_PROCESSING_BARRIER_DEADLINE: Duration = Duration::from_secs(30);
const E1_T05_INFLIGHT_DEADLINE_MAX: Duration = Duration::from_secs(120);

fn parse_e1_t05_inflight_deadline(value: Option<&OsStr>) -> anyhow::Result<Duration> {
    let Some(value) = value else {
        return Ok(BULK_RECEIVE_DEADLINE);
    };
    ensure!(
        cfg!(debug_assertions),
        "GUD_TEST_E1_T05_INFLIGHT_DEADLINE_MS is unavailable in release builds"
    );
    let milliseconds = value
        .to_str()
        .context("GUD_TEST_E1_T05_INFLIGHT_DEADLINE_MS must be valid UTF-8")?
        .parse::<u64>()
        .context("GUD_TEST_E1_T05_INFLIGHT_DEADLINE_MS must be an integer")?;
    let deadline = Duration::from_millis(milliseconds);
    ensure!(
        deadline > BULK_RECEIVE_DEADLINE && deadline <= E1_T05_INFLIGHT_DEADLINE_MAX,
        "GUD_TEST_E1_T05_INFLIGHT_DEADLINE_MS must be greater than {} and no more than {}",
        BULK_RECEIVE_DEADLINE.as_millis(),
        E1_T05_INFLIGHT_DEADLINE_MAX.as_millis()
    );
    Ok(deadline)
}

#[derive(Debug)]
struct E1T05ProcessingBarrier {
    directory: PathBuf,
    used: bool,
}

impl E1T05ProcessingBarrier {
    fn from_env() -> anyhow::Result<Option<Self>> {
        let requested = var_os("GUD_TEST_E1_T05_PROCESSING_BARRIER");
        let directory = var_os("GUD_TEST_E1_T05_PROCESSING_BARRIER_DIR");
        ensure!(
            requested.is_some() == directory.is_some(),
            "GUD_TEST_E1_T05_PROCESSING_BARRIER and GUD_TEST_E1_T05_PROCESSING_BARRIER_DIR must be set together"
        );
        let Some(requested) = requested else {
            return Ok(None);
        };
        ensure!(
            cfg!(debug_assertions),
            "GUD_TEST_E1_T05_PROCESSING_BARRIER is unavailable in release builds"
        );
        ensure!(
            requested == "processing-before-idle",
            "GUD_TEST_E1_T05_PROCESSING_BARRIER must be processing-before-idle"
        );
        Ok(Some(Self {
            directory: PathBuf::from(directory.expect("paired barrier directory")),
            used: false,
        }))
    }

    /// Blocks at one declared Processing boundary without cancelling the
    /// accepted request. The release marker injects one lifecycle observation.
    fn inject_once(&mut self, sequence: u64) -> anyhow::Result<bool> {
        if self.used {
            return Ok(false);
        }
        self.used = true;
        std::fs::create_dir_all(&self.directory).context("create E1-T05 barrier directory")?;
        let ready = self
            .directory
            .join(format!("processing-ready-seq-{sequence}"));
        let release = self
            .directory
            .join(format!("processing-release-seq-{sequence}"));
        std::fs::write(&ready, b"ready\n").context("publish E1-T05 Processing barrier")?;
        tracing::warn!(
            event = "e1_t05_processing_barrier_entered",
            transaction_seq = sequence,
            ready = %ready.display(),
            release = %release.display(),
            deadline_ms = E1_T05_PROCESSING_BARRIER_DEADLINE.as_millis(),
            "TEST-ONLY debug Processing barrier entered"
        );
        let deadline = Instant::now() + E1_T05_PROCESSING_BARRIER_DEADLINE;
        while Instant::now() < deadline {
            if release.exists() {
                tracing::warn!(
                    event = "e1_t05_processing_barrier_injected",
                    transaction_seq = sequence,
                    "TEST-ONLY debug Processing lifecycle injection released"
                );
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::bail!(
            "E1-T05 Processing barrier timed out after {} ms without release marker",
            E1_T05_PROCESSING_BARRIER_DEADLINE.as_millis()
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusOnSetCleanupPhase {
    AwaitingInitial,
    Receiving {
        trailing_statuses: u8,
    },
    AwaitingTrailing {
        trailing_statuses: u8,
        deadline: Instant,
    },
    Drained,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusOnSetCleanupObservation {
    Initial,
    TrailingEarly { drained: u8, remaining: u8 },
    TrailingAfterPayload { drained: u8, remaining: u8 },
}

#[derive(Debug)]
struct StatusOnSetCleanupDrain {
    transaction: u8,
    phase: StatusOnSetCleanupPhase,
}

impl StatusOnSetCleanupDrain {
    fn new(transaction: u8) -> Self {
        Self {
            transaction,
            phase: StatusOnSetCleanupPhase::AwaitingInitial,
        }
    }

    fn note_status(&mut self, status: u8) -> anyhow::Result<StatusOnSetCleanupObservation> {
        ensure!(
            status == 0,
            "STATUS_ON_SET diagnostic STATUS_ON_SET received non-OK status {status}"
        );
        match self.phase {
            StatusOnSetCleanupPhase::AwaitingInitial => {
                self.phase = StatusOnSetCleanupPhase::Receiving {
                    trailing_statuses: 0,
                };
                Ok(StatusOnSetCleanupObservation::Initial)
            }
            StatusOnSetCleanupPhase::Receiving { trailing_statuses } => {
                ensure!(
                    trailing_statuses < STATUS_ON_SET_CLEANUP_STATUS_COUNT,
                    "received an extra GET_STATUS while all STATUS_ON_SET cleanup statuses were already drained"
                );
                let drained = trailing_statuses + 1;
                let remaining = STATUS_ON_SET_CLEANUP_STATUS_COUNT - drained;
                self.phase = StatusOnSetCleanupPhase::Receiving {
                    trailing_statuses: drained,
                };
                Ok(StatusOnSetCleanupObservation::TrailingEarly { drained, remaining })
            }
            StatusOnSetCleanupPhase::AwaitingTrailing {
                trailing_statuses,
                deadline,
            } => {
                ensure!(
                    trailing_statuses < STATUS_ON_SET_CLEANUP_STATUS_COUNT,
                    "received an extra GET_STATUS while all STATUS_ON_SET cleanup statuses were already drained"
                );
                let drained = trailing_statuses + 1;
                let remaining = STATUS_ON_SET_CLEANUP_STATUS_COUNT - drained;
                if remaining == 0 {
                    self.phase = StatusOnSetCleanupPhase::Drained;
                } else {
                    self.phase = StatusOnSetCleanupPhase::AwaitingTrailing {
                        trailing_statuses: drained,
                        deadline,
                    };
                }
                Ok(StatusOnSetCleanupObservation::TrailingAfterPayload { drained, remaining })
            }
            StatusOnSetCleanupPhase::Drained => anyhow::bail!(
                "received an extra GET_STATUS after STATUS_ON_SET cleanup was drained"
            ),
            StatusOnSetCleanupPhase::TimedOut => {
                anyhow::bail!("received GET_STATUS after STATUS_ON_SET cleanup timeout")
            }
        }
    }

    /// Returns true when a trailing status arrived while completion/processing
    /// was in progress, so the diagnostic can detach immediately.
    fn payload_processed(&mut self, now: Instant) -> anyhow::Result<bool> {
        match self.phase {
            StatusOnSetCleanupPhase::Receiving { trailing_statuses }
                if trailing_statuses == STATUS_ON_SET_CLEANUP_STATUS_COUNT =>
            {
                self.phase = StatusOnSetCleanupPhase::Drained;
                Ok(true)
            }
            StatusOnSetCleanupPhase::Receiving { trailing_statuses } => {
                self.phase = StatusOnSetCleanupPhase::AwaitingTrailing {
                    trailing_statuses,
                    deadline: now + STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE,
                };
                Ok(false)
            }
            phase => anyhow::bail!(
                "STATUS_ON_SET diagnostic payload processing completed while status phase was {phase:?}"
            ),
        }
    }

    fn timed_out(&mut self, now: Instant) -> bool {
        let StatusOnSetCleanupPhase::AwaitingTrailing { deadline, .. } = self.phase else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.phase = StatusOnSetCleanupPhase::TimedOut;
        true
    }

    fn is_drained(&self) -> bool {
        self.phase == StatusOnSetCleanupPhase::Drained
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FrameDumpMode {
    Disabled,
    SingleFrame,
}

impl FrameDumpMode {
    fn from_env(value: Option<&OsStr>) -> anyhow::Result<Self> {
        match value.and_then(OsStr::to_str).unwrap_or("disabled") {
            "disabled" => Ok(Self::Disabled),
            "single-frame" => Ok(Self::SingleFrame),
            value => {
                anyhow::bail!("GUD_FRAME_DUMP_MODE must be disabled or single-frame, got {value:?}")
            }
        }
    }

    fn permits_startup_dump(self) -> bool {
        matches!(self, Self::SingleFrame)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum BulkReceiveState {
    Idle = 0,
    Arming = 1,
    InFlight = 2,
    Processing = 3,
    Poisoned = 4,
    ShuttingDown = 5,
    AbortedBySessionDestruction = 6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsbSessionState {
    WaitingForHost,
    Active,
    Suspended,
    Contained,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsbLifecycleInput {
    ProtocolInvalidation,
    Bind,
    Enable,
    Suspend,
    Resume,
    Disable,
    Unbind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UsbLifecycleResult {
    Continue(UsbSessionState),
    PreserveOwned(UsbSessionState),
    SessionBoundary(UsbSessionState),
    Contained,
    FatalUnbind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProvenIdleGate {
    aggregate_idle: bool,
    exact_aio_idle: bool,
    accepted_operation_absent: bool,
    aio_queue_empty: bool,
    pending_exact_receive_absent: bool,
    ready_exact_payload_absent: bool,
    cleanup_drain_absent: bool,
    diagnostic_guard_inactive: bool,
    prearmed_read_inactive: bool,
}

impl ProvenIdleGate {
    fn is_proven_idle(self) -> bool {
        self.aggregate_idle
            && self.exact_aio_idle
            && self.accepted_operation_absent
            && self.aio_queue_empty
            && self.pending_exact_receive_absent
            && self.ready_exact_payload_absent
            && self.cleanup_drain_absent
            && self.diagnostic_guard_inactive
            && self.prearmed_read_inactive
    }
}

fn classify_usb_lifecycle(
    state: UsbSessionState,
    input: UsbLifecycleInput,
    proven_idle: bool,
) -> UsbLifecycleResult {
    if !proven_idle {
        return match input {
            UsbLifecycleInput::Suspend => {
                UsbLifecycleResult::PreserveOwned(UsbSessionState::Suspended)
            }
            UsbLifecycleInput::Resume => UsbLifecycleResult::PreserveOwned(UsbSessionState::Active),
            UsbLifecycleInput::Disable => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::Enable => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::Active)
            }
            UsbLifecycleInput::Unbind => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::Bind => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::ProtocolInvalidation => UsbLifecycleResult::Contained,
        };
    }
    if state == UsbSessionState::Contained {
        return match input {
            UsbLifecycleInput::Disable => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::Enable => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::Active)
            }
            UsbLifecycleInput::Unbind => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::Suspend | UsbLifecycleInput::Resume => {
                UsbLifecycleResult::PreserveOwned(state)
            }
            UsbLifecycleInput::Bind => {
                UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
            }
            UsbLifecycleInput::ProtocolInvalidation => UsbLifecycleResult::Contained,
        };
    }
    match input {
        UsbLifecycleInput::ProtocolInvalidation => UsbLifecycleResult::Continue(state),
        UsbLifecycleInput::Bind => UsbLifecycleResult::Continue(UsbSessionState::WaitingForHost),
        UsbLifecycleInput::Enable => UsbLifecycleResult::Continue(UsbSessionState::Active),
        UsbLifecycleInput::Suspend => UsbLifecycleResult::Continue(UsbSessionState::Suspended),
        UsbLifecycleInput::Resume => UsbLifecycleResult::Continue(UsbSessionState::Active),
        UsbLifecycleInput::Disable => UsbLifecycleResult::Continue(UsbSessionState::WaitingForHost),
        UsbLifecycleInput::Unbind => UsbLifecycleResult::FatalUnbind,
    }
}

fn usb_lifecycle_input(reason: ProtocolInvalidationReason) -> UsbLifecycleInput {
    match reason {
        ProtocolInvalidationReason::Bind => UsbLifecycleInput::Bind,
        ProtocolInvalidationReason::Enable => UsbLifecycleInput::Enable,
        ProtocolInvalidationReason::Suspend => UsbLifecycleInput::Suspend,
        ProtocolInvalidationReason::Resume => UsbLifecycleInput::Resume,
        ProtocolInvalidationReason::Disconnected => UsbLifecycleInput::Disable,
        ProtocolInvalidationReason::Unbind => UsbLifecycleInput::Unbind,
        ProtocolInvalidationReason::InvalidStateCheck
        | ProtocolInvalidationReason::CommitWithoutPending => {
            UsbLifecycleInput::ProtocolInvalidation
        }
    }
}

impl BulkReceiveState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Idle,
            1 => Self::Arming,
            2 => Self::InFlight,
            3 => Self::Processing,
            4 => Self::Poisoned,
            5 => Self::ShuttingDown,
            6 => Self::AbortedBySessionDestruction,
            _ => unreachable!("invalid bulk receive state {raw}"),
        }
    }
}

#[derive(Clone)]
struct BulkReceiveSession {
    state: Arc<AtomicU8>,
    epoch_id: Arc<AtomicU64>,
    deadline: Duration,
}

#[derive(Debug, Default)]
struct ReceiveTelemetry {
    set_buffer_seen: u64,
    accepted: u64,
    rejected_busy: u64,
    submit_attempts: u64,
    submit_accepted: u64,
    completed: u64,
    processing_started: u64,
    processing_completed: u64,
    processing_failed: u64,
    poisoned: u64,
    timed_out: u64,
    lifecycle_events: u64,
    cleanup_attempts: u64,
    unsafe_teardown_suppressed: u64,
    last_sequence: u64,
    validation_to_accept_ms_total: u128,
    receive_ms_total: u128,
    processing_ms_total: u128,
    transaction_ms_total: u128,
}

impl ReceiveTelemetry {
    fn emit(&self, session: &BulkReceiveSession, endpoint: &gud_gadget::PixelDataEndpoint) {
        info!(
            event = "production_receive_telemetry",
            set_buffer_seen = self.set_buffer_seen,
            accepted_transactions = self.accepted,
            rejected_busy_transactions = self.rejected_busy,
            aio_submission_attempts = self.submit_attempts,
            aio_accepted = self.submit_accepted,
            aio_completed = self.completed,
            processing_started = self.processing_started,
            processing_completed = self.processing_completed,
            processing_failed = self.processing_failed,
            poisoned_transactions = self.poisoned,
            timed_out_transactions = self.timed_out,
            lifecycle_events = self.lifecycle_events,
            cleanup_attempts = self.cleanup_attempts,
            unsafe_teardown_suppressed = self.unsafe_teardown_suppressed,
            usb_epoch = session.epoch_id(),
            currently_owned = session.currently_owned(),
            aggregate_state = ?session.current_state(),
            aio_state = ?endpoint.exact_aio_state(),
            current_or_last_sequence = endpoint.exact_aio_sequence(),
            accepted_operation_id = endpoint.exact_aio_operation_id(),
            last_sequence = self.last_sequence,
            validation_to_aio_accept_ms_total = self.validation_to_accept_ms_total,
            aio_accept_to_status_readiness = "logged per transaction by status_after_arm",
            receive_ms_total = self.receive_ms_total,
            processing_ms_total = self.processing_ms_total,
            complete_transaction_ms_total = self.transaction_ms_total,
            "cumulative production exact-AIO lifecycle telemetry"
        );
    }
}

impl Default for BulkReceiveSession {
    fn default() -> Self {
        Self::with_deadline(BULK_RECEIVE_DEADLINE)
    }
}

impl BulkReceiveSession {
    fn with_deadline(deadline: Duration) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(BulkReceiveState::Idle as u8)),
            epoch_id: Arc::new(AtomicU64::new(0)),
            deadline,
        }
    }

    fn receive<T, F>(&self, receive: F) -> anyhow::Result<T>
    where
        F: FnOnce() -> anyhow::Result<T>,
    {
        let started = self.begin_receive()?;
        self.submission_accepted()?;
        self.finish_receive(started, receive())
    }

    fn begin_receive(&self) -> anyhow::Result<std::time::Instant> {
        self.state
            .compare_exchange(
                BulkReceiveState::Idle as u8,
                BulkReceiveState::Arming as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|raw| {
                anyhow::anyhow!(
                    "FunctionFS bulk receive session is {:?}; a fresh Pi boot may be required",
                    BulkReceiveState::from_raw(raw)
                )
            })?;
        debug!(
            event = "receiver_state_transition",
            receiver_state = "Arming",
            deadline_ms = self.deadline.as_millis(),
            "FunctionFS bulk receive session entered Arming"
        );

        Ok(std::time::Instant::now())
    }

    fn submission_accepted(&self) -> anyhow::Result<()> {
        self.state
            .compare_exchange(
                BulkReceiveState::Arming as u8,
                BulkReceiveState::InFlight as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|raw| {
                anyhow::anyhow!(
                    "cannot record accepted FunctionFS AIO from {:?}",
                    BulkReceiveState::from_raw(raw)
                )
            })?;
        debug!(
            event = "receiver_state_transition",
            receiver_state = "InFlight",
            "exact AIO accepted"
        );
        Ok(())
    }

    fn finish_receive<T>(
        &self,
        started: std::time::Instant,
        result: anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let elapsed = started.elapsed();
        match result {
            Err(err) => {
                self.poison();
                tracing::error!(
                    event = "receiver_state_transition",
                    receiver_state = "Poisoned",
                    "FunctionFS bulk receive session entered Poisoned after an error"
                );
                Err(err)
            }
            Ok(_) if elapsed > self.deadline => {
                self.poison();
                tracing::error!(
                    event = "receiver_state_transition",
                    receiver_state = "Poisoned",
                    elapsed_ms = elapsed.as_millis(),
                    deadline_ms = self.deadline.as_millis(),
                    "FunctionFS bulk receive session entered Poisoned after a late completion"
                );
                Err(anyhow::anyhow!(
                    "FunctionFS bulk receive exceeded the conservative {:?} safety deadline \
                     (elapsed {:?}); session poisoned",
                    self.deadline,
                    elapsed
                ))
            }
            Ok(value) => {
                self.state
                    .compare_exchange(
                        BulkReceiveState::InFlight as u8,
                        BulkReceiveState::Processing as u8,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .map_err(|raw| {
                        self.state
                            .store(BulkReceiveState::Poisoned as u8, Ordering::Release);
                        anyhow::anyhow!(
                            "FunctionFS bulk receive state changed unexpectedly to {:?}; \
                             session poisoned",
                            BulkReceiveState::from_raw(raw)
                        )
                    })?;
                debug!(
                    event = "receiver_state_transition",
                    receiver_state = "Processing",
                    elapsed_ms = elapsed.as_millis(),
                    "FunctionFS bulk receive completed; processing retains ownership"
                );
                Ok(value)
            }
        }
    }

    fn finish_processing(&self) -> anyhow::Result<()> {
        self.state
            .compare_exchange(
                BulkReceiveState::Processing as u8,
                BulkReceiveState::Idle as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|raw| {
                anyhow::anyhow!(
                    "cannot finalize FunctionFS processing from {:?}",
                    BulkReceiveState::from_raw(raw)
                )
            })?;
        debug!(
            event = "receiver_state_transition",
            receiver_state = "Idle",
            "processing finalized"
        );
        Ok(())
    }

    fn poison(&self) {
        self.state
            .store(BulkReceiveState::Poisoned as u8, Ordering::Release);
    }

    fn cancel_receive_before_io(&self) -> anyhow::Result<()> {
        self.state
            .compare_exchange(
                BulkReceiveState::Arming as u8,
                BulkReceiveState::Idle as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|raw| {
                anyhow::anyhow!(
                    "cannot cancel unsubmitted FunctionFS receive from state {:?}",
                    BulkReceiveState::from_raw(raw)
                )
            })?;
        debug!(
            event = "receiver_state_transition",
            receiver_state = "Idle",
            "FunctionFS bulk receive prearm failed before I/O submission"
        );
        Ok(())
    }

    /// A lifecycle event cannot prove an Arming request was never accepted.
    /// Ordinary teardown is permitted only when the session was already Idle.
    fn observe_lifecycle(&self) -> Result<(), BulkReceiveState> {
        loop {
            match self.current_state() {
                BulkReceiveState::Idle => return Ok(()),
                state @ (BulkReceiveState::Arming
                | BulkReceiveState::InFlight
                | BulkReceiveState::Processing) => {
                    if self
                        .state
                        .compare_exchange(
                            state as u8,
                            BulkReceiveState::Poisoned as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Err(state);
                    }
                }
                BulkReceiveState::AbortedBySessionDestruction => return Ok(()),
                state @ (BulkReceiveState::Poisoned | BulkReceiveState::ShuttingDown) => {
                    return Err(state);
                }
            }
        }
    }

    fn begin_shutdown(&self) -> Result<(), BulkReceiveState> {
        loop {
            match self.current_state() {
                BulkReceiveState::Idle | BulkReceiveState::AbortedBySessionDestruction => {
                    let expected = self.current_state();
                    if self
                        .state
                        .compare_exchange(
                            expected as u8,
                            BulkReceiveState::ShuttingDown as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                state @ (BulkReceiveState::Arming
                | BulkReceiveState::InFlight
                | BulkReceiveState::Processing) => {
                    if self
                        .state
                        .compare_exchange(
                            state as u8,
                            BulkReceiveState::Poisoned as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Err(state);
                    }
                }
                state @ (BulkReceiveState::Poisoned | BulkReceiveState::ShuttingDown) => {
                    return Err(state);
                }
            }
        }
    }

    fn current_state(&self) -> BulkReceiveState {
        BulkReceiveState::from_raw(self.state.load(Ordering::Acquire))
    }

    fn currently_owned(&self) -> bool {
        !matches!(
            self.current_state(),
            BulkReceiveState::Idle | BulkReceiveState::AbortedBySessionDestruction
        )
    }

    fn epoch_id(&self) -> u64 {
        self.epoch_id.load(Ordering::Acquire)
    }

    fn retire_after_session_destruction(&self) -> Result<u64, BulkReceiveState> {
        loop {
            let state = self.current_state();
            if !matches!(
                state,
                BulkReceiveState::Arming | BulkReceiveState::InFlight | BulkReceiveState::Poisoned
            ) {
                return Err(state);
            }
            if self
                .state
                .compare_exchange(
                    state as u8,
                    BulkReceiveState::AbortedBySessionDestruction as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(self.epoch_id());
            }
        }
    }

    fn begin_usb_epoch(&self, epoch_id: u64) -> anyhow::Result<()> {
        let state = self.current_state();
        ensure!(
            matches!(
                state,
                BulkReceiveState::Idle | BulkReceiveState::AbortedBySessionDestruction
            ),
            "cannot begin USB epoch {epoch_id} from {state:?}"
        );
        ensure!(
            epoch_id > self.epoch_id(),
            "USB epoch identifier did not increase"
        );
        self.epoch_id.store(epoch_id, Ordering::Release);
        self.state
            .store(BulkReceiveState::Idle as u8, Ordering::Release);
        Ok(())
    }
}

fn lifecycle_event_name(reason: ProtocolInvalidationReason) -> &'static str {
    match reason {
        ProtocolInvalidationReason::InvalidStateCheck => "invalid-state-check",
        ProtocolInvalidationReason::CommitWithoutPending => "commit-without-pending",
        ProtocolInvalidationReason::Bind => "bind",
        ProtocolInvalidationReason::Enable => "enable",
        ProtocolInvalidationReason::Suspend => "functionfs-suspend",
        ProtocolInvalidationReason::Resume => "functionfs-resume",
        ProtocolInvalidationReason::Disconnected => "functionfs-disable-disconnect",
        ProtocolInvalidationReason::Unbind => "functionfs-unbind",
    }
}

fn proven_idle_gate(
    session: &BulkReceiveSession,
    endpoint: &mut gud_gadget::PixelDataEndpoint,
    pending_exact_receive: &Option<(gud_gadget::SetBuffer, Instant, u64)>,
    ready_exact_payload: &Option<(gud_gadget::SetBuffer, gud_gadget::PayloadStats)>,
    status_on_set_cleanup_drain: &Option<StatusOnSetCleanupDrain>,
) -> ProvenIdleGate {
    let diagnostic_guard = gud_gadget::status_on_set_diagnostic_guard_state();
    ProvenIdleGate {
        aggregate_idle: matches!(
            session.current_state(),
            BulkReceiveState::Idle | BulkReceiveState::AbortedBySessionDestruction
        ),
        exact_aio_idle: matches!(
            endpoint.exact_aio_state(),
            gud_gadget::ExactAioState::Idle
                | gud_gadget::ExactAioState::AbortedBySessionDestruction
        ),
        accepted_operation_absent: endpoint.exact_aio_operation_id().is_none(),
        aio_queue_empty: endpoint.exact_aio_queue_is_empty(),
        pending_exact_receive_absent: pending_exact_receive.is_none(),
        ready_exact_payload_absent: ready_exact_payload.is_none(),
        cleanup_drain_absent: status_on_set_cleanup_drain.is_none(),
        diagnostic_guard_inactive: !diagnostic_guard.active,
        prearmed_read_inactive: !endpoint.prearmed_read_is_active(),
    }
}

fn event_timeout_for_ready_payload(event_timeout: Duration, has_ready_payload: bool) -> Duration {
    if has_ready_payload {
        Duration::ZERO
    } else {
        event_timeout
    }
}

fn conflicts_with_owned_exact_receive(event: &Event<'_>, has_ready_payload: bool) -> bool {
    !has_ready_payload && matches!(event, Event::Buffer(_))
}

struct LifecycleObservation<'a> {
    usb_session: &'a mut UsbSessionState,
    session: &'a BulkReceiveSession,
    endpoint: &'a mut gud_gadget::PixelDataEndpoint,
    pending_exact_receive: &'a Option<(gud_gadget::SetBuffer, Instant, u64)>,
    ready_exact_payload: &'a Option<(gud_gadget::SetBuffer, gud_gadget::PayloadStats)>,
    status_on_set_cleanup_drain: &'a Option<StatusOnSetCleanupDrain>,
}

/// Classifies a lifecycle event without releasing any ownership. Only a full
/// gate can reuse the FunctionFS objects for the next host activation.
fn observe_aio_lifecycle(
    reason: ProtocolInvalidationReason,
    input: UsbLifecycleInput,
    observation: LifecycleObservation<'_>,
    telemetry: &mut ReceiveTelemetry,
) -> UsbLifecycleResult {
    telemetry.lifecycle_events += 1;
    let observed_state = observation.session.current_state();
    let lifecycle_event = lifecycle_event_name(reason);
    let gate = proven_idle_gate(
        observation.session,
        observation.endpoint,
        observation.pending_exact_receive,
        observation.ready_exact_payload,
        observation.status_on_set_cleanup_drain,
    );
    let diagnostic_guard = gud_gadget::status_on_set_diagnostic_guard_state();
    let diagnostic_lifecycle_containment =
        diagnostic_guard.transaction_limit > 0 && diagnostic_guard.started_transactions > 0;
    let result = classify_usb_lifecycle(
        *observation.usb_session,
        input,
        gate.is_proven_idle() && !diagnostic_lifecycle_containment,
    );
    match result {
        UsbLifecycleResult::Continue(next_state) => {
            debug_assert!(observation.session.observe_lifecycle().is_ok());
            *observation.usb_session = next_state;
            info!(
                event = "e1_t05_lifecycle_observed",
                lifecycle_event,
                lifecycle_event_state = ?observed_state,
                usb_session_state = ?observation.usb_session,
                aggregate_idle = gate.aggregate_idle,
                exact_aio_idle = gate.exact_aio_idle,
                accepted_operation_absent = gate.accepted_operation_absent,
                aio_queue_empty = gate.aio_queue_empty,
                pending_exact_receive_absent = gate.pending_exact_receive_absent,
                ready_exact_payload_absent = gate.ready_exact_payload_absent,
                cleanup_drain_absent = gate.cleanup_drain_absent,
                diagnostic_guard_inactive = gate.diagnostic_guard_inactive,
                diagnostic_lifecycle_containment,
                prearmed_read_inactive = gate.prearmed_read_inactive,
                aggregate_state = ?observation.session.current_state(),
                aio_state = ?observation.endpoint.exact_aio_state(),
                transaction_seq = observation.endpoint.exact_aio_sequence(),
                activation_generation = ?observation.endpoint.exact_aio_activation_generation(),
                accepted_operation_id = observation.endpoint.exact_aio_operation_id(),
                currently_owned = false,
                "E1-T05 lifecycle event observed from proven Idle ownership"
            );
            UsbLifecycleResult::Continue(next_state)
        }
        UsbLifecycleResult::PreserveOwned(next_state) => {
            *observation.usb_session = next_state;
            warn!(
                event = "functionfs_lifecycle_preserved_owned",
                lifecycle_event,
                usb_session_state = ?next_state,
                aggregate_state = ?observation.session.current_state(),
                aio_state = ?observation.endpoint.exact_aio_state(),
                transaction_seq = observation.endpoint.exact_aio_sequence(),
                usb_epoch = observation.session.epoch_id(),
                currently_owned = observation.session.currently_owned(),
                "SUSPEND/RESUME preserved the live USB epoch and exact ownership"
            );
            UsbLifecycleResult::PreserveOwned(next_state)
        }
        UsbLifecycleResult::SessionBoundary(next_state) => {
            let unsafe_state = observation.session.current_state();
            if observation.session.currently_owned() {
                observation.session.poison();
                observation.endpoint.poison_exact_payload_aio(
                    "awaiting AIO harvest after source-proven endpoint destruction",
                );
            }
            *observation.usb_session = next_state;
            warn!(
                event = "epoch_destroy_proven",
                lifecycle_event,
                lifecycle_event_state = ?unsafe_state,
                usb_session_state = ?next_state,
                usb_epoch = observation.session.epoch_id(),
                transaction_seq = observation.endpoint.exact_aio_sequence(),
                accepted_operation_id = observation.endpoint.exact_aio_operation_id(),
                aio_queue_empty = gate.aio_queue_empty,
                "FunctionFS disabled the prior endpoint generation; waiting to harvest its AIO completion"
            );
            UsbLifecycleResult::SessionBoundary(next_state)
        }
        UsbLifecycleResult::Contained => {
            let unsafe_state = observation
                .session
                .observe_lifecycle()
                .err()
                .unwrap_or(observed_state);
            observation.session.poison();
            observation
                .endpoint
                .poison_exact_payload_aio("lifecycle event after non-Idle ownership");
            *observation.usb_session = UsbSessionState::Contained;
            telemetry.poisoned += 1;
            telemetry.unsafe_teardown_suppressed += 1;
            tracing::error!(
                event = "e1_t05_lifecycle_containment",
                lifecycle_event,
                lifecycle_event_state = ?unsafe_state,
                usb_session_state = ?observation.usb_session,
                aggregate_idle = gate.aggregate_idle,
                exact_aio_idle = gate.exact_aio_idle,
                accepted_operation_absent = gate.accepted_operation_absent,
                aio_queue_empty = gate.aio_queue_empty,
                pending_exact_receive_absent = gate.pending_exact_receive_absent,
                ready_exact_payload_absent = gate.ready_exact_payload_absent,
                cleanup_drain_absent = gate.cleanup_drain_absent,
                diagnostic_guard_inactive = gate.diagnostic_guard_inactive,
                diagnostic_lifecycle_containment,
                prearmed_read_inactive = gate.prearmed_read_inactive,
                aggregate_state = ?observation.session.current_state(),
                aio_state = ?observation.endpoint.exact_aio_state(),
                transaction_seq = observation.endpoint.exact_aio_sequence(),
                activation_generation = ?observation.endpoint.exact_aio_activation_generation(),
                accepted_operation_id = observation.endpoint.exact_aio_operation_id(),
                currently_owned = true,
                unsafe_teardown_suppressed = true,
                "E1-T05 contained lifecycle event; do not cancel, close, unbind, restart, or fall back"
            );
            telemetry.emit(observation.session, observation.endpoint);
            UsbLifecycleResult::Contained
        }
        UsbLifecycleResult::FatalUnbind => UsbLifecycleResult::FatalUnbind,
    }
}

trait GadgetUnbind: Send + Sync {
    fn unbind(&self) -> io::Result<()>;
}

impl GadgetUnbind for RegGadget {
    fn unbind(&self) -> io::Result<()> {
        self.bind(None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GadgetUnbindOutcome {
    Armed,
    DeferredUntilPublish,
    Unbound,
    AlreadyUnbound,
}

#[derive(Default)]
struct GadgetShutdownState {
    requested: bool,
    unbound: bool,
    target: Option<Weak<dyn GadgetUnbind>>,
}

#[derive(Clone, Default)]
struct GadgetShutdown {
    state: Arc<Mutex<GadgetShutdownState>>,
}

impl GadgetShutdown {
    fn publish(&self, target: &Arc<dyn GadgetUnbind>) -> io::Result<GadgetUnbindOutcome> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.target = Some(Arc::downgrade(target));
        state.unbound = false;

        if state.requested {
            Self::unbind_locked(&mut state)
        } else {
            Ok(GadgetUnbindOutcome::Armed)
        }
    }

    fn request_unbind(&self) -> io::Result<GadgetUnbindOutcome> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.requested = true;
        Self::unbind_locked(&mut state)
    }

    fn clear(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.target = None;
    }

    fn unbind_locked(state: &mut GadgetShutdownState) -> io::Result<GadgetUnbindOutcome> {
        if state.unbound {
            return Ok(GadgetUnbindOutcome::AlreadyUnbound);
        }

        let Some(target) = state.target.as_ref().and_then(Weak::upgrade) else {
            return Ok(GadgetUnbindOutcome::DeferredUntilPublish);
        };

        target.unbind()?;
        state.unbound = true;
        Ok(GadgetUnbindOutcome::Unbound)
    }
}

#[derive(Debug)]
pub struct Card(std::fs::File);

impl std::os::unix::io::AsFd for Card {
    fn as_fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}

impl Device for Card {}

impl Card {
    pub fn open(path: &str) -> Self {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        options.write(true);
        Card(options.open(path).unwrap())
    }

    pub fn open_global() -> Self {
        Self::open("/dev/dri/card0")
    }
}

struct DrmScanoutBackend {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    format: TransferFormat,
}

impl ScanoutBackend for DrmScanoutBackend {
    type Buffer = DumbBuffer;
    type Framebuffer = framebuffer::Handle;
    type Mode = Mode;
    type Mapping<'a> = DumbMapping<'a>;

    fn mode_size(&self, mode: Self::Mode) -> (u32, u32) {
        let (width, height) = mode.size();
        (width.into(), height.into())
    }

    fn create_buffer(&mut self, width: u32, height: u32) -> anyhow::Result<Self::Buffer> {
        self.card
            .create_dumb_buffer((width, height), self.format.drm_fourcc(), self.format.bpp())
            .context("create dumb buffer")
    }

    fn buffer_pitch(&self, buffer: &Self::Buffer) -> u32 {
        buffer.pitch()
    }

    fn add_framebuffer(&mut self, buffer: &Self::Buffer) -> anyhow::Result<Self::Framebuffer> {
        self.card
            .add_framebuffer(buffer, self.format.depth(), self.format.bpp())
            .context("add DRM framebuffer")
    }

    fn map_buffer<'a>(
        &mut self,
        buffer: &'a mut Self::Buffer,
    ) -> anyhow::Result<Self::Mapping<'a>> {
        self.card
            .map_dumb_buffer(buffer)
            .context("map DRM scanout buffer")
    }

    fn remove_framebuffer(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
        self.card
            .destroy_framebuffer(framebuffer)
            .context("remove DRM framebuffer")
    }

    fn destroy_buffer(&mut self, buffer: Self::Buffer) -> anyhow::Result<()> {
        self.card
            .destroy_dumb_buffer(buffer)
            .context("destroy DRM dumb buffer")
    }

    fn dirty_framebuffer(
        &mut self,
        framebuffer: Self::Framebuffer,
        x1: u16,
        y1: u16,
        x2: u16,
        y2: u16,
    ) -> anyhow::Result<()> {
        self.card
            .dirty_framebuffer(framebuffer, &[ClipRect::new(x1, y1, x2, y2)])
            .context("dirty DRM framebuffer")
    }

    fn set_crtc(&mut self, framebuffer: Self::Framebuffer, mode: Self::Mode) -> anyhow::Result<()> {
        self.card
            .set_crtc(
                self.crtc,
                Some(framebuffer),
                (0, 0),
                &[self.connector],
                Some(mode),
            )
            .context("set DRM CRTC")
    }

    fn page_flip(&mut self, framebuffer: Self::Framebuffer) -> anyhow::Result<()> {
        self.card
            .page_flip(self.crtc, framebuffer, PageFlipFlags::empty(), None)
            .context("page flip DRM framebuffer")
    }
}

fn dump_pixel_buffer_ppm(
    path: &Path,
    fb: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    format: TransferFormat,
) -> anyhow::Result<()> {
    let width = width as usize;
    let height = height as usize;
    validate_raster(
        fb,
        pitch,
        width,
        height,
        format.bytes_per_pixel(),
        "framebuffer dump",
    )?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gud-framebuffer.ppm");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    let mut writer = BufWriter::new(File::create(&tmp_path)?);
    write!(writer, "P6\n{} {}\n255\n", width, height)?;

    for y in 0..height {
        for x in 0..width {
            let color = read_pixel(fb, pitch, x, y, format)?;
            writer.write_all(&[color.r, color.g, color.b])?;
        }
    }

    writer.flush()?;
    drop(writer);
    rename(&tmp_path, path)?;
    Ok(())
}

fn dump_raw_buffer(path: &Path, fb: &[u8]) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gud-framebuffer.raw");
    let tmp_path = path.with_file_name(format!(".{}.tmp", file_name));
    std::fs::write(&tmp_path, fb)?;
    rename(&tmp_path, path)?;
    Ok(())
}

fn dump_framebuffer_if_enabled(
    dump_path: Option<&Path>,
    fb: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    format: TransferFormat,
) {
    if let Some(path) = dump_path {
        if let Err(err) = dump_pixel_buffer_ppm(path, fb, pitch, width, height, format) {
            warn!("Failed to dump framebuffer to {}: {}", path.display(), err);
        } else {
            debug!("Wrote framebuffer dump to {}", path.display());
        }
    }
}

fn dump_framebuffer_raw_if_enabled(dump_path: Option<&Path>, fb: &[u8]) {
    if let Some(path) = dump_path {
        if let Err(err) = dump_raw_buffer(path, fb) {
            warn!(
                "Failed to dump raw framebuffer to {}: {}",
                path.display(),
                err
            );
        } else {
            debug!("Wrote raw framebuffer dump to {}", path.display());
        }
    }
}

fn advertised_preferred_mode_index(mode_types: impl IntoIterator<Item = ModeTypeFlags>) -> usize {
    mode_types
        .into_iter()
        .position(|mode_type| mode_type.contains(ModeTypeFlags::PREFERRED))
        .unwrap_or(0)
}

fn derive_mode_from_native(
    native: &DisplayMode,
    target_width: u16,
    target_height: u16,
) -> DisplayMode {
    let scale_dimension = |delta: u16, native_active: u16, target_active: u16| -> u16 {
        let scaled = (delta as u32 * target_active as u32) / native_active as u32;
        scaled.max(1) as u16
    };

    let h_front = scale_dimension(
        native.hsync_start - native.hdisplay,
        native.hdisplay,
        target_width,
    );
    let h_sync = scale_dimension(
        native.hsync_end - native.hsync_start,
        native.hdisplay,
        target_width,
    );
    let h_back = scale_dimension(
        native.htotal - native.hsync_end,
        native.hdisplay,
        target_width,
    );
    let v_front = scale_dimension(
        native.vsync_start - native.vdisplay,
        native.vdisplay,
        target_height,
    );
    let v_sync = scale_dimension(
        native.vsync_end - native.vsync_start,
        native.vdisplay,
        target_height,
    );
    let v_back = scale_dimension(
        native.vtotal - native.vsync_end,
        native.vdisplay,
        target_height,
    );

    let hsync_start = target_width + h_front;
    let hsync_end = hsync_start + h_sync;
    let htotal = hsync_end + h_back;
    let vsync_start = target_height + v_front;
    let vsync_end = vsync_start + v_sync;
    let vtotal = vsync_end + v_back;
    let clock = (native.clock as u64 * htotal as u64 * vtotal as u64
        / native.htotal as u64
        / native.vtotal as u64) as u32;

    DisplayMode {
        clock,
        hdisplay: target_width,
        hsync_start,
        hsync_end,
        htotal,
        vdisplay: target_height,
        vsync_start,
        vsync_end,
        vtotal,
        flags: 0,
    }
}

const RGB565_GREEN: u16 = 0x07e0;
const RGB565_WHITE: u16 = 0xffff;

const WAITING_SCREEN_LINES: [&str; 5] = [
    "   _~_        .------------.",
    "  (o o)       |  WAITING   |",
    " /  V  \\      '------------'",
    "/(  _  )\\         |    |",
    "  ^^ ^^           |____|",
];
const WAITING_HIGHLIGHT_LINE: usize = 1;
const WAITING_HIGHLIGHT_TEXT: &str = "WAITING";
const WAITING_GLYPH_WIDTH: usize = 5;
const WAITING_GLYPH_HEIGHT: usize = 7;
const WAITING_GLYPH_SPACING: usize = 1;
const WAITING_LINE_SPACING: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatternMode {
    Off,
    Startup,
    Hold,
    Usb,
}

impl PatternMode {
    fn from_env() -> Self {
        let Some(raw) = var_os("GUD_PATTERN_MODE") else {
            return Self::Off;
        };

        match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "startup" => Self::Startup,
            "hold" => Self::Hold,
            "usb" => Self::Usb,
            other => {
                warn!("Unknown GUD_PATTERN_MODE={other:?}, defaulting to off");
                Self::Off
            }
        }
    }

    fn uses_startup_pattern(self) -> bool {
        matches!(self, Self::Startup | Self::Hold | Self::Usb)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferFormat {
    Rgb565,
    Rgb888,
    Xrgb8888,
}

impl TransferFormat {
    fn from_env() -> anyhow::Result<Self> {
        Self::from_env_value(var_os("GUD_TRANSFER_FORMAT").as_deref())
    }

    fn from_env_value(raw: Option<&OsStr>) -> anyhow::Result<Self> {
        let Some(raw) = raw else {
            return Ok(Self::Rgb565);
        };

        let raw = raw
            .to_str()
            .context("GUD_TRANSFER_FORMAT must be valid UTF-8")?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "rgb565" => Ok(Self::Rgb565),
            "rgb888" => Ok(Self::Rgb888),
            "xrgb8888" => Ok(Self::Xrgb8888),
            _ => anyhow::bail!(
                "invalid GUD_TRANSFER_FORMAT={raw:?}; supported values: rgb565, rgb888, xrgb8888"
            ),
        }
    }

    fn gud_pixel_format(self) -> u8 {
        match self {
            Self::Rgb565 => gud_gadget::GUD_PIXEL_FORMAT_RGB565,
            Self::Rgb888 => gud_gadget::GUD_PIXEL_FORMAT_RGB888,
            Self::Xrgb8888 => gud_gadget::GUD_PIXEL_FORMAT_XRGB8888,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rgb565 => "rgb565",
            Self::Rgb888 => "rgb888",
            Self::Xrgb8888 => "xrgb8888",
        }
    }

    fn drm_fourcc(self) -> drm::buffer::DrmFourcc {
        match self {
            Self::Rgb565 => drm::buffer::DrmFourcc::Rgb565,
            Self::Rgb888 => drm::buffer::DrmFourcc::Rgb888,
            Self::Xrgb8888 => drm::buffer::DrmFourcc::Xrgb8888,
        }
    }

    fn depth(self) -> u32 {
        match self {
            Self::Rgb565 => 16,
            Self::Rgb888 => 24,
            Self::Xrgb8888 => 24,
        }
    }

    fn bpp(self) -> u32 {
        match self {
            Self::Rgb565 => 16,
            Self::Rgb888 => 24,
            Self::Xrgb8888 => 32,
        }
    }

    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
            Self::Xrgb8888 => 4,
        }
    }

    fn drm_fourcc_name(self) -> &'static str {
        match self {
            Self::Rgb565 => "Rgb565",
            Self::Rgb888 => "Rgb888",
            Self::Xrgb8888 => "Xrgb8888",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RgbColor {
    r: u8,
    g: u8,
    b: u8,
}

const COLOR_BLACK: RgbColor = RgbColor { r: 0, g: 0, b: 0 };
const COLOR_BLUE: RgbColor = RgbColor { r: 0, g: 0, b: 255 };
const COLOR_GREEN: RgbColor = RgbColor { r: 0, g: 255, b: 0 };
const COLOR_CYAN: RgbColor = RgbColor {
    r: 0,
    g: 255,
    b: 255,
};
const COLOR_RED: RgbColor = RgbColor { r: 255, g: 0, b: 0 };
const COLOR_MAGENTA: RgbColor = RgbColor {
    r: 255,
    g: 0,
    b: 255,
};
const COLOR_YELLOW: RgbColor = RgbColor {
    r: 255,
    g: 255,
    b: 0,
};
const COLOR_WHITE: RgbColor = RgbColor {
    r: 255,
    g: 255,
    b: 255,
};
const COLOR_DARK_GRAY: RgbColor = RgbColor {
    r: 66,
    g: 65,
    b: 66,
};
const COLOR_LIGHT_GRAY: RgbColor = RgbColor {
    r: 198,
    g: 195,
    b: 198,
};
const COLOR_ORANGE: RgbColor = RgbColor {
    r: 255,
    g: 166,
    b: 0,
};

fn pixel_offset(fb: &[u8], pitch: usize, x: usize, y: usize, bpp: usize) -> anyhow::Result<usize> {
    ensure!(bpp > 0, "bytes per pixel must be greater than zero");
    let offset = y
        .checked_mul(pitch)
        .and_then(|offset| {
            x.checked_mul(bpp)
                .and_then(|x_offset| offset.checked_add(x_offset))
        })
        .context("pixel offset overflow")?;
    let end = offset
        .checked_add(bpp)
        .context("pixel end offset overflow")?;
    ensure!(
        end <= fb.len(),
        "pixel ({x}, {y}) exceeds buffer length {}",
        fb.len()
    );
    Ok(offset)
}

fn write_pixel(
    fb: &mut [u8],
    pitch: usize,
    x: usize,
    y: usize,
    format: TransferFormat,
    color: RgbColor,
) -> anyhow::Result<()> {
    let bpp = format.bytes_per_pixel();
    let offset = pixel_offset(fb, pitch, x, y, bpp)?;
    match format {
        TransferFormat::Rgb565 => {
            let pixel = ((color.r as u16 >> 3) << 11)
                | ((color.g as u16 >> 2) << 5)
                | (color.b as u16 >> 3);
            fb[offset..offset + 2].copy_from_slice(&pixel.to_le_bytes());
        }
        // DRM_FORMAT_RGB888 is [23:0] R:G:B, so little-endian memory is B, G, R.
        TransferFormat::Rgb888 => {
            fb[offset..offset + 3].copy_from_slice(&[color.b, color.g, color.r])
        }
        // DRM_FORMAT_XRGB8888 is [31:0] x:R:G:B, so little-endian memory is B, G, R, x.
        TransferFormat::Xrgb8888 => {
            fb[offset..offset + 4].copy_from_slice(&[color.b, color.g, color.r, 0]);
        }
    }
    Ok(())
}

fn read_pixel(
    fb: &[u8],
    pitch: usize,
    x: usize,
    y: usize,
    format: TransferFormat,
) -> anyhow::Result<RgbColor> {
    let offset = pixel_offset(fb, pitch, x, y, format.bytes_per_pixel())?;
    Ok(match format {
        TransferFormat::Rgb565 => {
            let pixel = u16::from_le_bytes([fb[offset], fb[offset + 1]]);
            let red = ((pixel >> 11) & 0x1f) as u8;
            let green = ((pixel >> 5) & 0x3f) as u8;
            let blue = (pixel & 0x1f) as u8;
            RgbColor {
                r: (red << 3) | (red >> 2),
                g: (green << 2) | (green >> 4),
                b: (blue << 3) | (blue >> 2),
            }
        }
        TransferFormat::Rgb888 => RgbColor {
            r: fb[offset + 2],
            g: fb[offset + 1],
            b: fb[offset],
        },
        TransferFormat::Xrgb8888 => RgbColor {
            r: fb[offset + 2],
            g: fb[offset + 1],
            b: fb[offset],
        },
    })
}

fn validate_raster(
    pixels: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
    bpp: usize,
    name: &str,
) -> anyhow::Result<()> {
    ensure!(width > 0, "{name} width must be greater than zero");
    ensure!(height > 0, "{name} height must be greater than zero");
    ensure!(bpp > 0, "{name} bytes per pixel must be greater than zero");
    let minimum_pitch = width
        .checked_mul(bpp)
        .context("{name} minimum pitch overflow")?;
    ensure!(
        pitch >= minimum_pitch,
        "{name} pitch {pitch} is smaller than required {minimum_pitch}"
    );
    let required_len = pitch
        .checked_mul(height)
        .context("{name} required length overflow")?;
    ensure!(
        pixels.len() >= required_len,
        "{name} is too short: got {} bytes, need at least {required_len}",
        pixels.len()
    );
    Ok(())
}

fn write_rgb565_pixel(fb: &mut [u8], pitch: usize, x: usize, y: usize, color: u16) {
    let offset = y * pitch + x * 2;
    let [lo, hi] = color.to_le_bytes();
    fb[offset] = lo;
    fb[offset + 1] = hi;
}

fn fill_solid(
    fb: &mut [u8],
    pitch: usize,
    width: usize,
    height: usize,
    format: TransferFormat,
    color: RgbColor,
) -> anyhow::Result<()> {
    validate_raster(
        fb,
        pitch,
        width,
        height,
        format.bytes_per_pixel(),
        "solid fill",
    )?;
    for y in 0..height {
        for x in 0..width {
            write_pixel(fb, pitch, x, y, format, color)?;
        }
    }
    Ok(())
}

fn fill_rgb565_rect(
    fb: &mut [u8],
    pitch: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    color: u16,
) {
    for yy in y..(y + height) {
        for xx in x..(x + width) {
            write_rgb565_pixel(fb, pitch, xx, yy, color);
        }
    }
}

fn waiting_scene_glyph(ch: char) -> Option<[u8; WAITING_GLYPH_HEIGHT]> {
    Some(match ch {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '~' => [0, 0b01010, 0b10101, 0, 0, 0, 0],
        '(' => [
            0b00110, 0b01000, 0b10000, 0b10000, 0b10000, 0b01000, 0b00110,
        ],
        ')' => [
            0b01100, 0b00010, 0b00001, 0b00001, 0b00001, 0b00010, 0b01100,
        ],
        'o' => [0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110, 0],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0],
        '\\' => [0b10000, 0b01000, 0b00100, 0b00010, 0b00001, 0, 0],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b01010, 0b00100,
        ],
        '^' => [0b00100, 0b01010, 0b10001, 0, 0, 0, 0],
        '|' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        '.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '\'' => [0b00100, 0b00100, 0b00010, 0, 0, 0, 0],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        _ => return None,
    })
}

fn render_bitmap_glyph(
    fb: &mut [u8],
    pitch: usize,
    x: usize,
    y: usize,
    scale: usize,
    rows: [u8; WAITING_GLYPH_HEIGHT],
    color: u16,
) {
    for (row_idx, bits) in rows.iter().enumerate() {
        for col_idx in 0..WAITING_GLYPH_WIDTH {
            if bits & (1 << (WAITING_GLYPH_WIDTH - 1 - col_idx)) == 0 {
                continue;
            }
            fill_rgb565_rect(
                fb,
                pitch,
                x + col_idx * scale,
                y + row_idx * scale,
                scale,
                scale,
                color,
            );
        }
    }
}

fn render_waiting_screen(
    fb: &mut [u8],
    pitch: usize,
    width: u32,
    height: u32,
    format: TransferFormat,
) -> anyhow::Result<()> {
    let width = width as usize;
    let height = height as usize;

    if format != TransferFormat::Rgb565 {
        fill_solid(fb, pitch, width, height, format, COLOR_BLACK)?;
        return Ok(());
    }

    fill_solid(fb, pitch, width, height, format, COLOR_BLACK)?;

    let line_count = WAITING_SCREEN_LINES.len();
    let max_cols = WAITING_SCREEN_LINES
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    ensure!(
        line_count > 0 && max_cols > 0,
        "waiting screen must not be empty"
    );

    let scene_width_units =
        max_cols * WAITING_GLYPH_WIDTH + max_cols.saturating_sub(1) * WAITING_GLYPH_SPACING;
    let scene_height_units =
        line_count * WAITING_GLYPH_HEIGHT + line_count.saturating_sub(1) * WAITING_LINE_SPACING;
    let scale = (width / scene_width_units)
        .min(height / scene_height_units)
        .max(1);
    let scene_width = scene_width_units * scale;
    let scene_height = scene_height_units * scale;
    let origin_x = (width - scene_width) / 2;
    let origin_y = (height - scene_height) / 2;
    let waiting_start = WAITING_SCREEN_LINES[WAITING_HIGHLIGHT_LINE]
        .find(WAITING_HIGHLIGHT_TEXT)
        .context("waiting highlight text missing from scene")?;
    let waiting_end = waiting_start + WAITING_HIGHLIGHT_TEXT.len();

    for (line_idx, line) in WAITING_SCREEN_LINES.iter().enumerate() {
        let baseline_y =
            origin_y + line_idx * (WAITING_GLYPH_HEIGHT + WAITING_LINE_SPACING) * scale;
        for (col_idx, ch) in line.chars().enumerate() {
            if ch == ' ' {
                continue;
            }
            let glyph = waiting_scene_glyph(ch)
                .with_context(|| format!("unsupported waiting-scene glyph: {ch:?}"))?;
            let color = if line_idx == WAITING_HIGHLIGHT_LINE
                && (waiting_start..waiting_end).contains(&col_idx)
            {
                RGB565_GREEN
            } else {
                RGB565_WHITE
            };
            let glyph_x =
                origin_x + col_idx * (WAITING_GLYPH_WIDTH + WAITING_GLYPH_SPACING) * scale;
            render_bitmap_glyph(fb, pitch, glyph_x, baseline_y, scale, glyph, color);
        }
    }

    Ok(())
}

fn present_waiting_screen<B: ScanoutBackend>(
    backend: &mut B,
    active: &mut StableMappedActive<'_, B>,
    dump_path: Option<&Path>,
    dump_raw_path: Option<&Path>,
    format: TransferFormat,
) -> anyhow::Result<()> {
    let (width, height) = active.size();
    let pitch = active.pitch() as usize;
    for index in 0..2 {
        render_waiting_screen(active.buffer_mut(index), pitch, width, height, format)?;
    }

    match backend.dirty_framebuffer(
        active.front_framebuffer(),
        0,
        0,
        width as u16,
        height as u16,
    ) {
        Ok(()) => tracing::debug!("Waiting screen flushed"),
        Err(err) => tracing::debug!("dirty_framebuffer for waiting screen failed: {}", err),
    }

    dump_framebuffer_if_enabled(
        dump_path,
        active.front_buffer_mut(),
        pitch,
        width,
        height,
        format,
    );
    dump_framebuffer_raw_if_enabled(dump_raw_path, active.front_buffer_mut());

    Ok(())
}

fn diagnostic_pattern_color(x: usize, y: usize, width: usize, height: usize) -> RgbColor {
    const BORDER: usize = 48;
    const CORNER: usize = 128;
    const CENTER_THICKNESS: usize = 24;
    const GRID_X_STEP: usize = 180;
    const GRID_Y_STEP: usize = 240;
    const GRID_THICKNESS: usize = 4;

    let mut color = COLOR_DARK_GRAY;

    if y < BORDER {
        color = COLOR_RED;
    }
    if x >= width.saturating_sub(BORDER) {
        color = COLOR_YELLOW;
    }
    if y >= height.saturating_sub(BORDER) {
        color = COLOR_GREEN;
    }
    if x < BORDER {
        color = COLOR_BLUE;
    }

    if x < CORNER && y < CORNER {
        color = COLOR_WHITE;
    }
    if x >= width.saturating_sub(CORNER) && y < CORNER {
        color = COLOR_CYAN;
    }
    if x < CORNER && y >= height.saturating_sub(CORNER) {
        color = COLOR_MAGENTA;
    }
    if x >= width.saturating_sub(CORNER) && y >= height.saturating_sub(CORNER) {
        color = COLOR_ORANGE;
    }

    let center_x = width / 2;
    let center_y = height / 2;
    if x >= center_x.saturating_sub(CENTER_THICKNESS / 2) && x < center_x + (CENTER_THICKNESS / 2) {
        color = COLOR_WHITE;
    }
    if y >= center_y.saturating_sub(CENTER_THICKNESS / 2) && y < center_y + (CENTER_THICKNESS / 2) {
        color = COLOR_MAGENTA;
    }

    if x % GRID_X_STEP < GRID_THICKNESS || y % GRID_Y_STEP < GRID_THICKNESS {
        color = COLOR_LIGHT_GRAY;
    }

    color
}

#[allow(clippy::too_many_arguments)] // Rectangle geometry and destination raster are intentionally explicit.
fn fill_diagnostic_pattern_rect(
    fb: &mut [u8],
    pitch: usize,
    fb_width: u32,
    fb_height: u32,
    rect_x: u32,
    rect_y: u32,
    rect_width: u32,
    rect_height: u32,
    format: TransferFormat,
) -> anyhow::Result<()> {
    ensure!(
        rect_x <= fb_width,
        "rect x {} exceeds fb width {}",
        rect_x,
        fb_width
    );
    ensure!(
        rect_y <= fb_height,
        "rect y {} exceeds fb height {}",
        rect_y,
        fb_height
    );
    ensure!(
        rect_x
            .checked_add(rect_width)
            .context("diagnostic rectangle x extent overflow")?
            <= fb_width,
        "rect width {} at x {} exceeds fb width {}",
        rect_width,
        rect_x,
        fb_width
    );
    ensure!(
        rect_y
            .checked_add(rect_height)
            .context("diagnostic rectangle y extent overflow")?
            <= fb_height,
        "rect height {} at y {} exceeds fb height {}",
        rect_height,
        rect_y,
        fb_height
    );

    let fb_width = fb_width as usize;
    let fb_height = fb_height as usize;
    let rect_x = rect_x as usize;
    let rect_y = rect_y as usize;
    let rect_width = rect_width as usize;
    let rect_height = rect_height as usize;

    validate_raster(
        fb,
        pitch,
        fb_width,
        fb_height,
        format.bytes_per_pixel(),
        "diagnostic framebuffer",
    )?;

    for y in rect_y..(rect_y + rect_height) {
        for x in rect_x..(rect_x + rect_width) {
            let color = diagnostic_pattern_color(x, y, fb_width, fb_height);
            write_pixel(fb, pitch, x, y, format, color)?;
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScaledLayout {
    dst_x: usize,
    dst_y: usize,
    dst_width: usize,
    dst_height: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShadowRasterIdentity {
    width: u32,
    height: u32,
    format: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShadowActivation {
    Preserved,
    Zeroed,
}

#[derive(Debug, Default)]
struct ShadowFramebuffer {
    width: u32,
    height: u32,
    pitch: usize,
    pixels: Vec<u8>,
    preallocated_capacity: Option<usize>,
    identity: Option<ShadowRasterIdentity>,
    content_valid: bool,
}

impl ShadowFramebuffer {
    fn preallocated(max_capacity: usize) -> anyhow::Result<Self> {
        ensure!(
            max_capacity > 0,
            "maximum source shadow capacity must be non-zero"
        );
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(max_capacity)
            .context("allocate maximum source shadow")?;
        pixels.resize(max_capacity, 0);
        Ok(Self {
            pixels,
            preallocated_capacity: Some(max_capacity),
            ..Self::default()
        })
    }

    fn activate_scaled(
        &mut self,
        identity: ShadowRasterIdentity,
    ) -> anyhow::Result<ShadowActivation> {
        let bpp = gud_gadget::bytes_per_pixel(identity.format)?;
        let pitch = (identity.width as usize)
            .checked_mul(bpp)
            .context("source shadow pitch overflow")?;
        let len = pitch
            .checked_mul(identity.height as usize)
            .context("source shadow length overflow")?;

        if let Some(capacity) = self.preallocated_capacity {
            ensure!(
                len <= capacity && self.pixels.len() == capacity,
                "committed shadow raster requires {len} bytes but preallocated capacity is \
                 {capacity}"
            );
        } else if self.pixels.len() != len {
            self.pixels = vec![0; len];
        }
        self.width = identity.width;
        self.height = identity.height;
        self.pitch = pitch;
        let preserve = self.identity == Some(identity) && self.content_valid;
        self.identity = Some(identity);
        self.content_valid = true;
        if preserve {
            Ok(ShadowActivation::Preserved)
        } else {
            self.pixels[..len].fill(0);
            Ok(ShadowActivation::Zeroed)
        }
    }

    fn ensure_size(&mut self, identity: ShadowRasterIdentity) -> anyhow::Result<()> {
        self.activate_scaled(identity).map(|_| ())
    }

    fn is_active(&self, identity: ShadowRasterIdentity) -> bool {
        self.identity == Some(identity) && self.content_valid
    }

    fn invalidate_content(&mut self) {
        self.content_valid = false;
    }
}

fn compute_scaled_layout(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> anyhow::Result<ScaledLayout> {
    ensure!(src_width > 0, "source width must be greater than zero");
    ensure!(src_height > 0, "source height must be greater than zero");
    ensure!(dst_width > 0, "destination width must be greater than zero");
    ensure!(
        dst_height > 0,
        "destination height must be greater than zero"
    );

    let width_limited_height = (dst_width as u64 * src_height as u64) / src_width as u64;
    let (scaled_width, scaled_height) = if width_limited_height <= dst_height as u64 {
        (dst_width as usize, width_limited_height.max(1) as usize)
    } else {
        let width = (dst_height as u64 * src_width as u64) / src_height as u64;
        (width.max(1) as usize, dst_height as usize)
    };

    let dst_width = dst_width as usize;
    let dst_height = dst_height as usize;

    Ok(ScaledLayout {
        dst_x: (dst_width - scaled_width) / 2,
        dst_y: (dst_height - scaled_height) / 2,
        dst_width: scaled_width,
        dst_height: scaled_height,
    })
}

#[allow(clippy::too_many_arguments)] // Source and destination raster metadata must be independently validated.
fn scale_to_fit(
    src: &[u8],
    src_pitch: usize,
    src_width: u32,
    src_height: u32,
    dst: &mut [u8],
    dst_pitch: usize,
    dst_width: u32,
    dst_height: u32,
    format: TransferFormat,
) -> anyhow::Result<(ScaledLayout, u128)> {
    let start = std::time::Instant::now();
    let layout = compute_scaled_layout(src_width, src_height, dst_width, dst_height)?;

    let bpp = format.bytes_per_pixel();
    validate_raster(
        src,
        src_pitch,
        src_width as usize,
        src_height as usize,
        bpp,
        "scale source",
    )?;
    validate_raster(
        dst,
        dst_pitch,
        dst_width as usize,
        dst_height as usize,
        bpp,
        "scale destination",
    )?;
    fill_solid(
        dst,
        dst_pitch,
        dst_width as usize,
        dst_height as usize,
        format,
        COLOR_BLACK,
    )?;

    let src_width = src_width as usize;
    let src_height = src_height as usize;
    for dst_y_rel in 0..layout.dst_height {
        let src_y = dst_y_rel * src_height / layout.dst_height;
        let dst_y = layout.dst_y + dst_y_rel;
        let src_row = src_y * src_pitch;
        let dst_row = dst_y * dst_pitch;
        for dst_x_rel in 0..layout.dst_width {
            let src_x = dst_x_rel * src_width / layout.dst_width;
            let src_off = src_row + src_x * bpp;
            let dst_off = dst_row + (layout.dst_x + dst_x_rel) * bpp;
            dst[dst_off..dst_off + bpp].copy_from_slice(&src[src_off..src_off + bpp]);
        }
    }

    Ok((layout, start.elapsed().as_millis()))
}

fn parse_functionfs_read_size(raw: Option<&OsStr>) -> anyhow::Result<usize> {
    let Some(raw) = raw else {
        return Ok(gud_gadget::DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE);
    };
    let raw = raw
        .to_str()
        .context("GUD_FFS_READ_SIZE must be valid UTF-8")?;
    raw.parse::<usize>()
        .with_context(|| format!("invalid GUD_FFS_READ_SIZE={raw:?}"))
}

fn parse_test_compression(raw: Option<&OsStr>) -> anyhow::Result<u8> {
    let Some(raw) = raw else {
        return Ok(GUD_COMPRESSION_LZ4);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_COMPRESSION must be valid UTF-8")?;
    match raw {
        "lz4" => Ok(GUD_COMPRESSION_LZ4),
        "none" => Ok(0),
        _ => anyhow::bail!(
            "invalid GUD_TEST_COMPRESSION={raw:?}; expected exactly \"lz4\" or \"none\""
        ),
    }
}

fn parse_test_max_buffer_size(raw: Option<&OsStr>) -> anyhow::Result<Option<u32>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_MAX_BUFFER_SIZE must be valid UTF-8")?;
    let max_buffer_size = raw
        .parse::<u32>()
        .with_context(|| format!("invalid GUD_TEST_MAX_BUFFER_SIZE={raw:?}"))?;
    ensure!(
        max_buffer_size > 0,
        "GUD_TEST_MAX_BUFFER_SIZE must be greater than zero"
    );
    Ok(Some(max_buffer_size))
}

fn parse_test_prearm_once_bytes(raw: Option<&OsStr>) -> anyhow::Result<Option<usize>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_PREARM_ONCE_BYTES must be valid UTF-8")?;
    let bytes = raw
        .parse::<usize>()
        .with_context(|| format!("invalid GUD_TEST_PREARM_ONCE_BYTES={raw:?}"))?;
    ensure!(
        (4 * 1024..=64 * 1024).contains(&bytes) && bytes % 512 == 0,
        "GUD_TEST_PREARM_ONCE_BYTES must be a 512-byte multiple from 4096 through 65536"
    );
    Ok(Some(bytes))
}

fn parse_test_status_on_set_bytes(raw: Option<&OsStr>) -> anyhow::Result<Option<usize>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_STATUS_ON_SET_BYTES must be valid UTF-8")?;
    let bytes = raw
        .parse::<usize>()
        .with_context(|| format!("invalid GUD_TEST_STATUS_ON_SET_BYTES={raw:?}"))?;
    ensure!(
        bytes == 12_800,
        "GUD_TEST_STATUS_ON_SET_BYTES diagnostic mode requires exactly 12800"
    );
    Ok(Some(bytes))
}

fn parse_test_status_on_set_two_transactions_bytes(
    raw: Option<&OsStr>,
) -> anyhow::Result<Option<usize>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_STATUS_ON_SET_TWO_TRANSACTIONS_BYTES must be valid UTF-8")?;
    let bytes = raw.parse::<usize>().with_context(|| {
        format!("invalid GUD_TEST_STATUS_ON_SET_TWO_TRANSACTIONS_BYTES={raw:?}")
    })?;
    ensure!(
        bytes == 12_800,
        "GUD_TEST_STATUS_ON_SET_TWO_TRANSACTIONS_BYTES diagnostic mode requires exactly 12800"
    );
    Ok(Some(bytes))
}

fn parse_test_dynamic_mode_match(raw: Option<&OsStr>) -> anyhow::Result<bool> {
    let Some(raw) = raw else {
        return Ok(false);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_DYNAMIC_MODE_MATCH must be valid UTF-8")?;
    ensure!(
        raw == "1",
        "invalid GUD_TEST_DYNAMIC_MODE_MATCH={raw:?}; expected exactly \"1\""
    );
    Ok(true)
}

fn parse_test_row_crc_trace(raw: Option<&OsStr>) -> anyhow::Result<bool> {
    let Some(raw) = raw else {
        return Ok(false);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_ROW_CRC_TRACE must be valid UTF-8")?;
    ensure!(
        raw == "1",
        "invalid GUD_TEST_ROW_CRC_TRACE={raw:?}; expected exactly \"1\""
    );
    Ok(true)
}

fn row_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;

    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }

    !crc
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TestOutputMode {
    width: u16,
    height: u16,
}

fn parse_test_output_mode(raw: Option<&OsStr>) -> anyhow::Result<Option<TestOutputMode>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw
        .to_str()
        .context("GUD_TEST_OUTPUT_MODE must be valid UTF-8")?;
    let (width, height) = raw
        .split_once('x')
        .context("GUD_TEST_OUTPUT_MODE must use WIDTHxHEIGHT")?;
    let width = width
        .parse::<u16>()
        .with_context(|| format!("invalid GUD_TEST_OUTPUT_MODE width in {raw:?}"))?;
    let height = height
        .parse::<u16>()
        .with_context(|| format!("invalid GUD_TEST_OUTPUT_MODE height in {raw:?}"))?;
    ensure!(
        width > 0 && height > 0,
        "GUD_TEST_OUTPUT_MODE dimensions must be greater than zero"
    );
    Ok(Some(TestOutputMode { width, height }))
}

fn select_output_mode(
    connector_modes: &[Mode],
    test_output_mode: Option<TestOutputMode>,
) -> anyhow::Result<&Mode> {
    ensure!(
        !connector_modes.is_empty(),
        "connected DRM connector has no modes"
    );
    let Some(requested) = test_output_mode else {
        return Ok(&connector_modes[0]);
    };

    connector_modes
        .iter()
        .find(|mode| mode.size() == (requested.width, requested.height))
        .with_context(|| {
            format!(
                "GUD_TEST_OUTPUT_MODE={}x{} is not supported by the connected DRM connector",
                requested.width, requested.height
            )
        })
}

fn validate_test_mode_policy(
    dynamic_mode_match: bool,
    test_output_mode: Option<TestOutputMode>,
) -> anyhow::Result<()> {
    ensure!(
        !(dynamic_mode_match && test_output_mode.is_some()),
        "GUD_TEST_DYNAMIC_MODE_MATCH and GUD_TEST_OUTPUT_MODE are mutually exclusive"
    );
    Ok(())
}

fn release_dynamic_candidate_in_available_slots<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    primary_index: usize,
    primary: &mut ScanoutSlot<B>,
    secondary: &mut Option<(usize, &mut ScanoutSlot<B>)>,
) -> anyhow::Result<()> {
    match runtime.roles().candidate {
        None => Ok(()),
        Some(index) if index == primary_index => {
            runtime.release_candidate(backend, primary_index, primary)
        }
        Some(index) => match secondary.as_mut() {
            Some((secondary_index, secondary)) if index == *secondary_index => {
                runtime.release_candidate(backend, *secondary_index, secondary)
            }
            _ => anyhow::bail!(
                "candidate slot {index} is not available while slot {} is active",
                runtime.roles().active
            ),
        },
    }
}

#[allow(dead_code, clippy::too_many_arguments)] // Active/candidate slots must remain explicit for borrow safety.
fn prepare_dynamic_check_in_available_slots<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    primary_index: usize,
    primary: &mut ScanoutSlot<B>,
    mut secondary: Option<(usize, &mut ScanoutSlot<B>)>,
    catalog: &RouteCatalog<B::Mode>,
    snapshot: gud_gadget::DisplayStateSnapshot,
    state_check_control_ms: u128,
) -> anyhow::Result<()>
where
    B::Mode: std::fmt::Debug,
{
    let prepare_start = std::time::Instant::now();
    let logical_key = ModeKey::from_snapshot(&snapshot);
    let catalog_entry = catalog
        .entry_for_snapshot(&snapshot)
        .context("checked snapshot is absent from route catalog")?;
    let current_physical_key = runtime.dynamic_routes()?.current_physical_key;
    let candidate_key = runtime.dynamic_routes()?.candidate_key;
    let failed_key = runtime.dynamic_routes()?.failed_key(logical_key);
    let failed_cache_hit = runtime
        .dynamic_routes()?
        .failed_routes
        .contains(&failed_key);

    let mut new_candidate_key = None;
    let (kind, requested_physical_key, requested_mode, cache_hit) = match catalog_entry.route {
        CatalogRoute::Exact(physical_mode) if catalog_entry.key == current_physical_key => {
            release_dynamic_candidate_in_available_slots(
                backend,
                runtime,
                primary_index,
                primary,
                &mut secondary,
            )?;
            runtime.counters().no_op();
            (
                PendingPlanKind::ExactActiveNoOp,
                Some(catalog_entry.key),
                Some(physical_mode),
                false,
            )
        }
        CatalogRoute::Exact(physical_mode)
            if candidate_key == Some(catalog_entry.key) && runtime.roles().candidate.is_some() =>
        {
            new_candidate_key = candidate_key;
            (
                PendingPlanKind::ExactCandidate {
                    slot: runtime
                        .roles()
                        .candidate
                        .expect("candidate role was checked"),
                },
                Some(catalog_entry.key),
                Some(physical_mode),
                false,
            )
        }
        CatalogRoute::Exact(physical_mode) if failed_cache_hit => {
            release_dynamic_candidate_in_available_slots(
                backend,
                runtime,
                primary_index,
                primary,
                &mut secondary,
            )?;
            runtime.counters().fallback();
            (
                PendingPlanKind::ScaledCurrentAfterFailure(FallbackReason::FailedCacheHit),
                Some(catalog_entry.key),
                Some(physical_mode),
                true,
            )
        }
        CatalogRoute::Exact(physical_mode) => {
            release_dynamic_candidate_in_available_slots(
                backend,
                runtime,
                primary_index,
                primary,
                &mut secondary,
            )?;
            let prepared_slot = (|| {
                let allocation =
                    ScanoutAllocation::create(backend, physical_mode, &runtime.counters())?;
                if let Err(err) = allocation.flush_black_initialization(backend) {
                    debug!(
                        error = %format_args!("{err:#}"),
                        "Candidate black-buffer dirty flush is unsupported or failed"
                    );
                }
                let (slot_index, slot) = if primary.is_none() {
                    (primary_index, &mut *primary)
                } else {
                    let (slot_index, slot) = secondary
                        .as_mut()
                        .context("no second candidate scanout slot is available")?;
                    ensure!(slot.is_none(), "no empty candidate scanout slot");
                    (*slot_index, &mut **slot)
                };
                runtime.stage_candidate(backend, slot_index, slot, allocation)?;
                Ok(slot_index)
            })();
            match prepared_slot {
                Ok(slot_index) => {
                    new_candidate_key = Some(catalog_entry.key);
                    (
                        PendingPlanKind::ExactCandidate { slot: slot_index },
                        Some(catalog_entry.key),
                        Some(physical_mode),
                        false,
                    )
                }
                Err(err) => {
                    runtime
                        .dynamic_routes_mut()?
                        .failed_routes
                        .insert(failed_key);
                    runtime.counters().fallback();
                    warn!(
                        generation = snapshot.generation,
                        logical = ?logical_key,
                        current_physical = ?current_physical_key,
                        error = %format_args!("{err:#}"),
                        "Exact scanout candidate preparation failed; retaining scaled current \
                         scanout"
                    );
                    (
                        PendingPlanKind::ScaledCurrentAfterFailure(FallbackReason::PrepareFailed),
                        Some(catalog_entry.key),
                        Some(physical_mode),
                        false,
                    )
                }
            }
        }
        CatalogRoute::ScaledFallback => {
            release_dynamic_candidate_in_available_slots(
                backend,
                runtime,
                primary_index,
                primary,
                &mut secondary,
            )?;
            runtime.counters().fallback();
            (
                PendingPlanKind::ScaledBaseline {
                    switch_required: runtime.roles().active != runtime.roles().baseline,
                },
                None,
                None,
                false,
            )
        }
    };
    let mode_prepare_ms = prepare_start.elapsed().as_millis();
    if mode_prepare_ms > 1_000 {
        warn!(
            generation = snapshot.generation,
            mode_prepare_ms,
            "Dynamic mode preparation exceeded the provisional 1000 ms acceptance budget"
        );
    }
    {
        let routes = runtime.dynamic_routes_mut()?;
        routes.candidate_key = new_candidate_key;
        routes.pending_plan = Some(PendingPlan {
            snapshot: snapshot.clone(),
            logical_key,
            requested_physical_key,
            requested_mode,
            kind,
            failed_cache_hit: cache_hit,
            mode_prepare_ms,
        });
    }
    info!(
        event = "mode_check_decision",
        generation = snapshot.generation,
        connector = snapshot.connector,
        logical = ?logical_key,
        current_physical = ?current_physical_key,
        requested_physical = ?requested_physical_key,
        ?kind,
        failed_cache_hit = cache_hit,
        state_check_control_ms,
        mode_prepare_ms,
        candidate_slot = ?runtime.roles().candidate,
        counters = ?runtime.counters().snapshot(),
        "Prepared generation-bound dynamic mode plan"
    );
    Ok(())
}

fn invalidate_dynamic_pending_in_available_slots<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    primary_index: usize,
    primary: &mut ScanoutSlot<B>,
    mut secondary: Option<(usize, &mut ScanoutSlot<B>)>,
) -> anyhow::Result<()> {
    release_dynamic_candidate_in_available_slots(
        backend,
        runtime,
        primary_index,
        primary,
        &mut secondary,
    )?;
    runtime.dynamic_routes_mut()?.invalidate_pending();
    Ok(())
}

#[allow(dead_code)]
fn prepare_dynamic_check<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    slot1: &mut ScanoutSlot<B>,
    slot2: &mut ScanoutSlot<B>,
    catalog: &RouteCatalog<B::Mode>,
    snapshot: gud_gadget::DisplayStateSnapshot,
    state_check_control_ms: u128,
) -> anyhow::Result<()>
where
    B::Mode: std::fmt::Debug,
{
    prepare_dynamic_check_in_available_slots(
        backend,
        runtime,
        1,
        slot1,
        Some((2, slot2)),
        catalog,
        snapshot,
        state_check_control_ms,
    )
}

#[allow(dead_code)]
fn invalidate_dynamic_pending_off_baseline<B: ScanoutBackend>(
    backend: &mut B,
    runtime: &mut ScanoutRuntime<'_, B>,
    slot1: &mut ScanoutSlot<B>,
    slot2: &mut ScanoutSlot<B>,
) -> anyhow::Result<()> {
    invalidate_dynamic_pending_in_available_slots(backend, runtime, 1, slot1, Some((2, slot2)))
}

enum MappedSwitch<'old, 'target, B: ScanoutBackend + 'old + 'target> {
    Switched {
        active: MappedActive<'target, B>,
        mode_switch_ms: u128,
    },
    Retained {
        active: MappedActive<'old, B>,
        error: anyhow::Error,
        mode_switch_ms: u128,
    },
}

fn attempt_mapped_switch<'old, 'target, B: ScanoutBackend + 'old + 'target>(
    backend: &mut B,
    old: MappedActive<'old, B>,
    target: &'target mut ScanoutAllocation<B>,
    counters: &ScanoutCounters,
) -> MappedSwitch<'old, 'target, B> {
    let switch_start = std::time::Instant::now();
    let target = match target.map(backend, counters) {
        Ok(target) => target,
        Err(error) => {
            return MappedSwitch::Retained {
                active: old,
                error: error.context("map prepared target scanout"),
                mode_switch_ms: switch_start.elapsed().as_millis(),
            };
        }
    };
    match backend.set_crtc(target.front_framebuffer(), target.mode()) {
        Ok(()) => {
            drop(old);
            MappedSwitch::Switched {
                active: target,
                mode_switch_ms: switch_start.elapsed().as_millis(),
            }
        }
        Err(error) => {
            drop(target);
            MappedSwitch::Retained {
                active: old,
                error: error.context("single commit-time set_crtc"),
                mode_switch_ms: switch_start.elapsed().as_millis(),
            }
        }
    }
}

fn shadow_identity(snapshot: &gud_gadget::DisplayStateSnapshot) -> ShadowRasterIdentity {
    ShadowRasterIdentity {
        width: snapshot.mode.hdisplay.into(),
        height: snapshot.mode.vdisplay.into(),
        format: snapshot.format,
    }
}

fn record_committed_route<B: ScanoutBackend>(
    runtime: &mut ScanoutRuntime<'_, B>,
    snapshot: gud_gadget::DisplayStateSnapshot,
    route: PresentationRoute,
    new_physical_key: Option<ModeKey>,
) -> anyhow::Result<()> {
    let routes = runtime.dynamic_routes_mut()?;
    routes.committed_snapshot = Some(snapshot);
    routes.presentation_route = Some(route);
    if let Some(key) = new_physical_key {
        routes.current_physical_key = key;
    }
    routes.candidate_key = None;
    Ok(())
}

fn take_matching_pending_plan<M>(
    pending: &mut Option<PendingPlan<M>>,
    committed: &gud_gadget::DisplayStateSnapshot,
) -> Result<PendingPlan<M>, bool> {
    match pending.take() {
        Some(plan) if plan.snapshot == *committed => Ok(plan),
        Some(_) => Err(true),
        None => Err(false),
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    info!("gud-drm starting (XDISP-P0.1 lifecycle and read-size repair)");

    let card_path = args()
        .nth(1)
        .expect("specify full path to /dev/dri/cardN as program argument");
    let functionfs_read_size = parse_functionfs_read_size(var_os("GUD_FFS_READ_SIZE").as_deref())?;
    let test_compression_raw = var_os("GUD_TEST_COMPRESSION");
    let test_max_buffer_size_raw = var_os("GUD_TEST_MAX_BUFFER_SIZE");
    let test_prearm_once_raw = var_os("GUD_TEST_PREARM_ONCE_BYTES");
    let receive_mode = var_os("GUD_RECEIVE_MODE")
        .and_then(|value| value.into_string().ok())
        .unwrap_or_else(|| "status-on-set-aio".to_string());
    ensure!(
        matches!(receive_mode.as_str(), "status-on-set-aio" | "blocking"),
        "GUD_RECEIVE_MODE must be status-on-set-aio or blocking"
    );
    let aio_mode = receive_mode == "status-on-set-aio";
    let test_status_on_set_raw = var_os("GUD_TEST_STATUS_ON_SET_BYTES");
    let test_status_on_set_two_transactions_raw =
        var_os("GUD_TEST_STATUS_ON_SET_TWO_TRANSACTIONS_BYTES");
    let test_output_mode_raw = var_os("GUD_TEST_OUTPUT_MODE");
    let test_dynamic_mode_match_raw = var_os("GUD_TEST_DYNAMIC_MODE_MATCH");
    let mut e1_t05_processing_barrier = E1T05ProcessingBarrier::from_env()?;
    let bulk_receive_deadline =
        parse_e1_t05_inflight_deadline(var_os("GUD_TEST_E1_T05_INFLIGHT_DEADLINE_MS").as_deref())?;
    let descriptor_compression = parse_test_compression(test_compression_raw.as_deref())?;
    let descriptor_max_buffer_size =
        parse_test_max_buffer_size(test_max_buffer_size_raw.as_deref())?;
    let test_prearm_once_bytes = parse_test_prearm_once_bytes(test_prearm_once_raw.as_deref())?;
    let legacy_status_on_set_bytes =
        parse_test_status_on_set_bytes(test_status_on_set_raw.as_deref())?;
    let two_transaction_status_on_set_bytes = parse_test_status_on_set_two_transactions_bytes(
        test_status_on_set_two_transactions_raw.as_deref(),
    )?;
    ensure!(
        legacy_status_on_set_bytes.is_none() || two_transaction_status_on_set_bytes.is_none(),
        "GUD_TEST_STATUS_ON_SET_BYTES cannot be combined with GUD_TEST_STATUS_ON_SET_TWO_TRANSACTIONS_BYTES"
    );
    let test_status_on_set_bytes =
        legacy_status_on_set_bytes.or(two_transaction_status_on_set_bytes);
    let status_on_set_transaction_limit = if two_transaction_status_on_set_bytes.is_some() {
        STATUS_ON_SET_DIAGNOSTIC_TRANSACTION_LIMIT
    } else if test_status_on_set_bytes.is_some() {
        1
    } else {
        0
    };
    let test_output_mode = parse_test_output_mode(test_output_mode_raw.as_deref())?;
    let dynamic_mode_match = parse_test_dynamic_mode_match(test_dynamic_mode_match_raw.as_deref())?;
    validate_test_mode_policy(dynamic_mode_match, test_output_mode)?;
    if let Some(prearm_bytes) = test_prearm_once_bytes {
        ensure!(
            descriptor_max_buffer_size == Some(prearm_bytes as u32),
            "GUD_TEST_PREARM_ONCE_BYTES requires an identical GUD_TEST_MAX_BUFFER_SIZE"
        );
        ensure!(
            descriptor_compression == 0,
            "GUD_TEST_PREARM_ONCE_BYTES requires GUD_TEST_COMPRESSION=none"
        );
        ensure!(
            prearm_bytes <= functionfs_read_size,
            "GUD_TEST_PREARM_ONCE_BYTES exceeds GUD_FFS_READ_SIZE"
        );
    }
    if let Some(status_on_set_bytes) = test_status_on_set_bytes {
        ensure!(
            test_prearm_once_bytes.is_none(),
            "GUD_TEST_STATUS_ON_SET_BYTES cannot be combined with old prearm mode"
        );
        ensure!(
            descriptor_max_buffer_size == Some(status_on_set_bytes as u32),
            "GUD_TEST_STATUS_ON_SET_BYTES requires an identical GUD_TEST_MAX_BUFFER_SIZE"
        );
        ensure!(
            descriptor_compression == 0,
            "GUD_TEST_STATUS_ON_SET_BYTES requires GUD_TEST_COMPRESSION=none"
        );
    }
    ensure!(
        !(aio_mode && test_prearm_once_bytes.is_some()),
        "status-on-set-aio cannot be combined with old blocking prearm mode"
    );
    ensure!(
        aio_mode || test_status_on_set_bytes.is_none(),
        "blocking mode cannot be combined with STATUS_ON_SET diagnostics"
    );
    gud_gadget::configure_status_on_set_diagnostic(status_on_set_transaction_limit);
    info!(
        event = "receive_mode",
        receive_mode = receive_mode.as_str(),
        status_on_set = aio_mode,
        aio_queue_depth = if aio_mode { 1 } else { 0 },
        "selected receive mode before UDC bind"
    );
    if status_on_set_transaction_limit > 0 {
        info!(
            event = "status_on_set_diagnostic_configured",
            transaction_limit = status_on_set_transaction_limit,
            expected_bytes = test_status_on_set_bytes,
            "configured bounded STATUS_ON_SET diagnostic"
        );
    }
    if let Some(barrier) = e1_t05_processing_barrier.as_ref() {
        warn!(
            event = "e1_t05_processing_barrier_configured",
            directory = %barrier.directory.display(),
            deadline_ms = E1_T05_PROCESSING_BARRIER_DEADLINE.as_millis(),
            "TEST-ONLY debug E1-T05 Processing barrier enabled"
        );
    }
    let (mut gud_data, gud_data_ep) =
        gud_gadget::PixelDataEndpoint::new_with_read_size(functionfs_read_size)
            .context("configure FunctionFS bulk OUT read size")?;
    let frame_dump_mode = FrameDumpMode::from_env(var_os("GUD_FRAME_DUMP_MODE").as_deref())?;
    let configured_dump_path = var_os("GUD_DUMP_FB_PATH").map(PathBuf::from);
    let configured_dump_raw_path = var_os("GUD_DUMP_FB_RAW_PATH").map(PathBuf::from);
    let dump_path = frame_dump_mode
        .permits_startup_dump()
        .then(|| configured_dump_path.clone())
        .flatten();
    let dump_raw_path = frame_dump_mode
        .permits_startup_dump()
        .then(|| configured_dump_raw_path.clone())
        .flatten();
    let pattern_mode = PatternMode::from_env();
    let transfer_format = TransferFormat::from_env()?;
    let row_crc_trace = parse_test_row_crc_trace(var_os("GUD_TEST_ROW_CRC_TRACE").as_deref())?;
    info!(?frame_dump_mode, "Frame dump policy");
    if frame_dump_mode.permits_startup_dump() {
        if let Some(path) = dump_path.as_deref() {
            info!(
                "Single startup framebuffer dump enabled: {}",
                path.display()
            );
        }
        if let Some(path) = dump_raw_path.as_deref() {
            info!(
                "Single startup raw framebuffer dump enabled: {}",
                path.display()
            );
        }
    } else if configured_dump_path.is_some() || configured_dump_raw_path.is_some() {
        info!("Framebuffer dump paths ignored by disabled dump policy");
    }
    info!("Pattern mode: {:?}", pattern_mode);
    info!(row_crc_trace, "Per-row CRC trace");
    info!(
        transfer_format = transfer_format.name(),
        gud_format = format_args!("{:#04x}", transfer_format.gud_pixel_format()),
        drm_fourcc = transfer_format.drm_fourcc_name(),
        depth = transfer_format.depth(),
        bpp = transfer_format.bpp(),
        bytes_per_pixel = transfer_format.bytes_per_pixel(),
        "Selected transfer format"
    );
    info!(
        "FunctionFS bulk OUT read ceiling: {} bytes; conservative receive safety deadline: {} ms",
        functionfs_read_size,
        bulk_receive_deadline.as_millis()
    );
    if bulk_receive_deadline != BULK_RECEIVE_DEADLINE {
        warn!(
            event = "e1_t05_inflight_deadline_override",
            deadline_ms = bulk_receive_deadline.as_millis(),
            "TEST-ONLY debug E1-T05 InFlight deadline override enabled"
        );
    }
    if test_compression_raw.is_some() || test_max_buffer_size_raw.is_some() {
        warn!(
            compression = if descriptor_compression == 0 {
                "none"
            } else {
                "lz4"
            },
            ?descriptor_max_buffer_size,
            "TEST-ONLY GUD descriptor override enabled; a fresh USB enumeration is required"
        );
    }
    if let Some(prearm_bytes) = test_prearm_once_bytes {
        warn!(
            prearm_bytes,
            "TEST-ONLY one-payload FunctionFS prearm enabled; the gadget will unbind after one payload"
        );
    }
    if let Some(output_mode) = test_output_mode {
        warn!(
            width = output_mode.width,
            height = output_mode.height,
            "TEST-ONLY physical DRM output-mode override enabled; normal mode selection is unchanged \
             when GUD_TEST_OUTPUT_MODE is absent"
        );
    }
    if dynamic_mode_match {
        warn!(
            "TEST-ONLY dynamic physical-mode matching enabled; this is not normal/default \
             behavior and requires the XDISP-P2.1 hardware gates before promotion"
        );
    }
    info!("Opening DRM device: {}", card_path);
    let card = Card::open(&card_path);
    let udc = default_udc().expect("no UDC found");
    info!("Using UDC: {:?}", udc);

    let resources = card.resource_handles().expect("load drm resources failed");

    let connector = resources
        .connectors()
        .iter()
        .map(|h| {
            card.get_connector(*h, false)
                .expect("get drm connector failed")
        })
        .find(|c| c.state() == drm::control::connector::State::Connected)
        .expect("no connected connectors found");

    let encoder_handle = connector
        .current_encoder()
        .or_else(|| connector.encoders().first().copied())
        .expect("no encoder found for connected connector");
    let encoder = card
        .get_encoder(encoder_handle)
        .expect("get drm encoder failed");
    let crtc_handle = resources
        .filter_crtcs(encoder.possible_crtcs())
        .into_iter()
        .next()
        .expect("no compatible crtc found for connected connector");
    let crtc = card
        .get_crtc(crtc_handle)
        .expect("get compatible drm crtc failed");

    let connector_handle = connector.handle();
    let connector_modes = connector.modes().to_vec();
    let mode = *select_output_mode(&connector_modes, test_output_mode)?;

    let mut min_width = u32::MAX;
    let mut min_height = u32::MAX;
    let mut max_width = 0;
    let mut max_height = 0;
    for connector_mode in &connector_modes {
        let (width, height) = connector_mode.size();
        let width = width as u32;
        let height = height as u32;
        if width < min_width {
            min_width = width
        }
        if width > max_width {
            max_width = width
        }
        if height < min_height {
            min_height = height
        }
        if height > max_height {
            max_height = height
        }
    }

    min_width = min_width.min(640);
    min_height = min_height.min(480);
    max_width = max_width.max(1920);
    max_height = max_height.max(1080);
    let serialized_max_buffer_size = descriptor_max_buffer_size.unwrap_or(
        max_width
            .checked_mul(max_height)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("serialized descriptor maximum buffer size overflow")?,
    );
    let descriptor_flags = if aio_mode {
        GUD_DISPLAY_FLAG_STATUS_ON_SET
    } else {
        0
    };
    info!(
        event = "descriptor_config",
        magic = "0x1d50614d",
        version = 1,
        flags = descriptor_flags,
        compression = descriptor_compression,
        max_buffer_size = serialized_max_buffer_size,
        min_width,
        max_width,
        min_height,
        max_height,
        dynamic_mode_match,
        "Serialized GUD display descriptor fields"
    );

    usb_gadget::remove_all().expect("UDC init failed");
    info!("USB gadgets removed");

    info!("Created pixel data endpoint");

    let mut builder = Custom::builder().with_interface(
        Interface::new(Class::vendor_specific(Class::VENDOR_SPECIFIC, 0), "GUD")
            .with_endpoint(gud_data_ep),
    );
    builder.ffs_no_disconnect = true;
    let (mut gud, gud_handle) = builder.build();
    info!("Built USB gadget");

    let running = Arc::new(AtomicBool::new(true));
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let gadget_shutdown = GadgetShutdown::default();
    let bulk_receive_session = BulkReceiveSession::with_deadline(bulk_receive_deadline);

    let signal_shutdown_requested = shutdown_requested.clone();
    ctrlc::set_handler(move || {
        signal_shutdown_requested.store(true, Ordering::SeqCst);
        // Only the event-loop owner may unbind, after proving every FunctionFS
        // owner is idle. Claiming aggregate ownership here would make that gate fail.
        info!("Shutdown requested; waiting for the event loop ownership gate");
    })
    .expect("cleanup handler registration failed");

    // The physical test override must not alter which USB mode is advertised
    // as preferred. If DRM supplies no preference, preserve the normal
    // connector-order fallback instead of falling back to the override-selected
    // physical mode.
    let preferred_mode_index =
        advertised_preferred_mode_index(connector_modes.iter().map(Mode::mode_type));
    let physical_catalog_modes: Vec<PhysicalCatalogMode<Mode>> = connector_modes
        .iter()
        .map(|mode| {
            let (hdisplay, vdisplay) = mode.size();
            let (hsync_start, hsync_end, htotal) = mode.hsync();
            let (vsync_start, vsync_end, vtotal) = mode.vsync();
            PhysicalCatalogMode {
                mode: *mode,
                timing: DisplayMode {
                    clock: mode.clock(),
                    hdisplay,
                    htotal,
                    hsync_end,
                    hsync_start,
                    vtotal,
                    vdisplay,
                    vsync_end,
                    vsync_start,
                    flags: mode.flags().bits(),
                },
            }
        })
        .collect();

    let native_mode = physical_catalog_modes[preferred_mode_index].timing.clone();

    let portrait_modes = [
        (900_u16, 1900_u16),
        (810_u16, 1710_u16),
        (720_u16, 1520_u16),
    ];
    let mut synthetic_modes = Vec::new();
    for (w, h) in portrait_modes {
        if !physical_catalog_modes
            .iter()
            .any(|candidate| candidate.timing.hdisplay == w && candidate.timing.vdisplay == h)
        {
            synthetic_modes.push(derive_mode_from_native(&native_mode, w, h));
        }
    }
    let route_catalog = RouteCatalog::build(
        0,
        &physical_catalog_modes,
        preferred_mode_index,
        &synthetic_modes,
    )
    .context("build complete-timing route catalog")?;
    let advertised_modes = route_catalog.advertised_modes();
    info!(
        connector = route_catalog.connector(),
        entries = route_catalog.entries().len(),
        preferred = ?route_catalog.preferred_entry().key,
        exact_routes = route_catalog
            .entries()
            .iter()
            .filter(|entry| matches!(entry.route, CatalogRoute::Exact(_)))
            .count(),
        scaled_routes = route_catalog
            .entries()
            .iter()
            .filter(|entry| entry.route == CatalogRoute::ScaledFallback)
            .count(),
        "Built normalized complete-timing route catalog"
    );
    let logical_memory = logical_memory_estimate(
        connector_modes.iter().map(|mode| {
            let (width, height) = mode.size();
            (width.into(), height.into())
        }),
        advertised_modes
            .iter()
            .map(|mode| (mode.hdisplay.into(), mode.vdisplay.into())),
        transfer_format.bytes_per_pixel(),
    )
    .context("calculate catalog-derived logical memory estimate")?;
    info!(
        logical_scanout_estimate_bytes = logical_memory.scanout_bytes,
        max_shadow_bytes = logical_memory.max_shadow_bytes,
        logical_peak_estimate_bytes = logical_memory.peak_bytes,
        transfer_format = ?transfer_format,
        "Catalog-derived memory estimate; actual DRM pitch/mapping lengths are recorded \
         per allocation"
    );
    let mut shadow = if dynamic_mode_match {
        let shadow = ShadowFramebuffer::preallocated(logical_memory.max_shadow_bytes)
            .context("preallocate dynamic-mode fallback shadow before UDC bind")?;
        info!(
            max_shadow_bytes = logical_memory.max_shadow_bytes,
            "Preallocated maximum source shadow before UDC bind"
        );
        shadow
    } else {
        ShadowFramebuffer::default()
    };
    gud_gadget::configure_state_check_validation(
        1,
        &[transfer_format.gud_pixel_format()],
        &advertised_modes,
    );

    info!(
        width = mode.size().0,
        height = mode.size().1,
        test_override = test_output_mode.is_some(),
        dynamic_mode_match,
        ?mode,
        "Picked deterministic physical DRM startup/fallback mode"
    );

    let mut backend = DrmScanoutBackend {
        card,
        crtc: crtc.handle(),
        connector: connector_handle,
        format: transfer_format,
    };
    let mut scanout_manager =
        ScanoutManager::new_baseline(&mut backend, mode).context("create baseline scanout")?;
    let baseline_timing = physical_catalog_modes
        .iter()
        .find(|physical| physical.mode == mode)
        .context("selected startup mode is absent from physical catalog")?;
    if dynamic_mode_match {
        scanout_manager
            .initialize_dynamic_routes(ModeKey::new(0, &baseline_timing.timing))
            .context("initialize dynamic route state")?;
    }
    let scanout_counters = scanout_manager.counters();
    let (scanout_slots, mut scanout_runtime) = scanout_manager.split_runtime();
    let scanout_slots_ptr = scanout_slots as *mut [ScanoutSlot<DrmScanoutBackend>; 3];
    // `MappedActive` deliberately keeps the active slot's dumb-buffer mappings alive across
    // control events. Rust cannot express a mutable borrow that migrates among stable array
    // elements across loop iterations, so slot access goes through this stable pointer. Every
    // call site must uphold the role invariant: never borrow the currently mapped slot, except
    // after its mapping has been moved out and dropped during a transition.
    macro_rules! slot_mut {
        ($index:expr) => {{
            // SAFETY: the fixed three-slot array outlives the event loop. Role checks at each
            // call site ensure that simultaneous mutable references target distinct slots and
            // that an allocation is never mutated while one of its mappings is live.
            unsafe { &mut (*scanout_slots_ptr)[$index] }
        }};
    }
    let baseline = slot_mut!(0)
        .as_deref_mut()
        .context("baseline scanout slot is empty")?;
    debug!(
        width = baseline.size().0,
        height = baseline.size().1,
        pitch = baseline.pitch(),
        mapping_lengths = ?baseline.mapping_lengths(),
        live_bytes = baseline.live_bytes(),
        "Created owned baseline double-buffer scanout"
    );
    let mut active = StableMappedActive::Slot0(
        baseline
            .map(&mut backend, &scanout_counters)
            .context("map baseline scanout")?,
    );

    for attempt in 1..=CRTC_SET_RETRIES {
        match backend.set_crtc(active.front_framebuffer(), mode) {
            Ok(()) => break,
            Err(err) if attempt < CRTC_SET_RETRIES => {
                warn!(
                    attempt,
                    retries = CRTC_SET_RETRIES,
                    error = ?err,
                    "set_crtc not ready; retrying before exposing the USB gadget"
                );
                std::thread::sleep(CRTC_SET_RETRY_DELAY);
            }
            Err(err) => return Err(err).context("Could not set CRTC after retries"),
        }
    }
    info!("CRTC set, display should be active");

    let (panel_width, panel_height) = active.size();
    let pitch = active.pitch();
    let width = panel_width;
    let height = panel_height;

    for index in 0..2 {
        let fb_data = active.buffer_mut(index);
        if pattern_mode.uses_startup_pattern() {
            fill_diagnostic_pattern_rect(
                fb_data,
                pitch as usize,
                width,
                height,
                0,
                0,
                width,
                height,
                transfer_format,
            )?;
        } else {
            render_waiting_screen(fb_data, pitch as usize, width, height, transfer_format)?;
        }
    }
    if pattern_mode.uses_startup_pattern() {
        info!("Filled both framebuffers with diagnostic startup pattern");
    } else {
        info!("Filled both framebuffers with waiting screen");
    }
    let mut waiting_screen_visible = matches!(pattern_mode, PatternMode::Off);

    match backend.dirty_framebuffer(
        active.front_framebuffer(),
        0,
        0,
        width as u16,
        height as u16,
    ) {
        Ok(()) => info!("Test pattern flushed to display"),
        Err(err) => warn!("Failed to flush test pattern: {}", err),
    }
    if frame_dump_mode.permits_startup_dump() {
        dump_framebuffer_if_enabled(
            dump_path.as_deref(),
            active.front_buffer_mut(),
            pitch as usize,
            width,
            height,
            transfer_format,
        );
        dump_framebuffer_raw_if_enabled(dump_raw_path.as_deref(), active.front_buffer_mut());
    }

    // Keep the USB gadget disconnected until DRM has a working CRTC and the
    // initial framebuffer is ready. Otherwise a host can begin SET_BUFFER
    // while a transient DRM ownership failure tears the gadget down.
    let reg = Arc::new(
        Gadget::new(
            Class::interface_specific(),
            gud_gadget::OPENMOKO_GUD_ID,
            Strings::new("The Internet", "Generic USB Display", ""),
        )
        .with_config(Config::new("gud").with_function(gud_handle))
        .bind(&udc)
        .expect("UDC binding failed"),
    );
    info!("USB gadget bound to UDC");

    tracing::info!("Entering main event loop");
    let mut had_host_session = false;
    let mut usb_session = UsbSessionState::WaitingForHost;
    let mut lifecycle_error = None;
    let mut prearmed_receive_started = None;
    let mut pending_exact_receive: Option<(gud_gadget::SetBuffer, std::time::Instant, u64)> = None;
    let mut ready_exact_payload: Option<(gud_gadget::SetBuffer, gud_gadget::PayloadStats)> = None;
    let mut status_on_set_cleanup_drain: Option<StatusOnSetCleanupDrain> = None;
    let mut pending_epoch_boundary: Option<(ProtocolInvalidationReason, Option<u64>)> = None;
    let mut receive_telemetry = ReceiveTelemetry::default();
    let mut row_immediate_crcs: Vec<Option<u32>> = Vec::new();
    let unbind_target: Arc<dyn GadgetUnbind> = reg.clone();
    match gadget_shutdown.publish(&unbind_target) {
        Ok(GadgetUnbindOutcome::Armed) => {}
        Ok(GadgetUnbindOutcome::Unbound) => {
            info!("USB gadget unbound for shutdown immediately after bind")
        }
        Ok(GadgetUnbindOutcome::DeferredUntilPublish) | Ok(GadgetUnbindOutcome::AlreadyUnbound) => {
            unreachable!()
        }
        Err(err) => {
            running.store(false, Ordering::SeqCst);
            lifecycle_error = Some(
                anyhow::Error::new(err)
                    .context("failed to unbind USB gadget after an early shutdown request"),
            );
        }
    }
    drop(unbind_target);

    'event_loop: while running.load(Ordering::Relaxed) {
        if let Some((boundary, new_epoch)) = pending_epoch_boundary {
            if pending_exact_receive.is_none() && gud_data.exact_aio_queue_is_empty() {
                tracing::warn!(
                    event = "epoch_retire_begin",
                    usb_epoch = bulk_receive_session.epoch_id(),
                    transaction_seq = gud_data.exact_aio_sequence(),
                    terminal_boundary = lifecycle_event_name(boundary),
                    "retiring destroyed FunctionFS endpoint generation after terminal AIO harvest"
                );
                let retired = gud_data
                    .retire_exact_aio_after_session_destruction()
                    .context("retire exact AIO from destroyed FunctionFS epoch")?;
                bulk_receive_session
                    .retire_after_session_destruction()
                    .map_err(|state| {
                        anyhow::anyhow!(
                            "cannot retire aggregate receiver from destroyed epoch state {state:?}"
                        )
                    })?;
                gud_gadget::retire_status_on_set_transaction_after_session_destruction();
                status_on_set_cleanup_drain = None;
                ready_exact_payload = None;
                tracing::warn!(
                    event = "epoch_retire_complete",
                    epoch_id = retired.epoch_id,
                    transaction_seq = retired.transaction_seq,
                    disposition = "ABORTED_BY_SESSION_DESTRUCTION",
                    currently_owned = false,
                    "old USB epoch reached a terminal historical disposition"
                );
                if let Some(epoch_id) = new_epoch {
                    gud_data.begin_exact_aio_usb_epoch(epoch_id)?;
                    bulk_receive_session.begin_usb_epoch(epoch_id)?;
                    usb_session = UsbSessionState::Active;
                }
                pending_epoch_boundary = None;
                continue;
            }
        }
        if let Some(drain) = status_on_set_cleanup_drain.as_mut() {
            if drain.timed_out(Instant::now()) {
                tracing::error!(
                    event = "functionfs_status_on_set_trailing_status_timeout",
                    timeout_ms = STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE.as_millis(),
                    exact_aio_state = ?gud_data.exact_aio_state(),
                    receiver_state = ?bulk_receive_session.current_state(),
                    "STATUS_ON_SET diagnostic payload completed but the host never requested its trailing GET_STATUS; ending the diagnostic through explicit containment"
                );
                lifecycle_error = Some(anyhow::anyhow!(
                    "STATUS_ON_SET diagnostic trailing GET_STATUS did not arrive within {:?}",
                    STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE
                ));
                break 'event_loop;
            }
        }
        if shutdown_requested.load(Ordering::Acquire)
            && proven_idle_gate(
                &bulk_receive_session,
                &mut gud_data,
                &pending_exact_receive,
                &ready_exact_payload,
                &status_on_set_cleanup_drain,
            )
            .is_proven_idle()
        {
            info!("Shutdown ownership gate is proven Idle");
            break 'event_loop;
        }
        if let Some((info, started, _)) = pending_exact_receive.as_ref() {
            match gud_data.try_complete_exact_payload_aio(info.length as usize) {
                Ok(Some(stats)) => {
                    receive_telemetry.completed += 1;
                    receive_telemetry.processing_started += 1;
                    receive_telemetry.receive_ms_total += started.elapsed().as_millis();
                    let (info, started, _) = pending_exact_receive
                        .take()
                        .expect("pending exact receive disappeared");
                    match bulk_receive_session.finish_receive(started, Ok(stats)) {
                        Ok(stats) => ready_exact_payload = Some((info, stats)),
                        Err(err) => {
                            gud_data.poison_exact_payload_aio(
                                "outer receive guard changed during exact completion",
                            );
                            tracing::error!(
                                error = %format_args!("{err:#}"),
                                "Preserving terminal containment after exact completion raced with lifecycle/shutdown"
                            );
                            continue;
                        }
                    }
                }
                Ok(None)
                    if bulk_receive_session.current_state() == BulkReceiveState::Poisoned
                        || started.elapsed() <= bulk_receive_session.deadline => {}
                Ok(None) => {
                    let (_, started, transaction_seq) = pending_exact_receive
                        .as_ref()
                        .expect("timed-out exact receive disappeared");
                    gud_data.poison_exact_payload_aio("missing completion timeout");
                    receive_telemetry.timed_out += 1;
                    receive_telemetry.poisoned += 1;
                    let _ = bulk_receive_session.finish_receive::<()>(
                        *started,
                        Err(anyhow::anyhow!(
                            "exact FunctionFS AIO transaction {transaction_seq} exceeded completion deadline"
                        )),
                    );
                    continue;
                }
                Err(err) => {
                    let (_, started, _) = pending_exact_receive
                        .take()
                        .expect("invalid exact receive disappeared");
                    gud_data.poison_exact_payload_aio("invalid AIO completion");
                    receive_telemetry.poisoned += 1;
                    let _ = bulk_receive_session.finish_receive::<()>(started, Err(err));
                    continue;
                }
            }
        }
        if pending_epoch_boundary.is_some() {
            // ENABLE can be queued immediately behind a coalesced DISABLE. Hold EP0
            // briefly instead of consuming the new host's first probe request until
            // the old one-slot AIO completion is harvested and the epoch can retire.
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let event_timeout =
            if pending_exact_receive.is_some() || status_on_set_cleanup_drain.is_some() {
                Duration::from_millis(10)
            } else {
                Duration::from_millis(100)
            };
        // A queued DISABLE must win over a completed bulk payload. Otherwise the
        // payload could be processed to Idle before the disconnect is observed.
        let event = match gud.event_timeout(event_timeout_for_ready_payload(
            event_timeout,
            ready_exact_payload.is_some(),
        )) {
            Ok(event) => event,
            Err(err) => {
                if let Some((_, started, _)) = pending_exact_receive.as_ref() {
                    gud_data.poison_exact_payload_aio("EP0 event failure while exact AIO accepted");
                    let _ = bulk_receive_session.finish_receive::<()>(
                        *started,
                        Err(anyhow::Error::new(err)
                            .context("process EP0 while exact AIO request was accepted")),
                    );
                    continue;
                }
                tracing::error!("Failed to read GUD event: {}", err);
                if !running.load(Ordering::Acquire) {
                    tracing::info!("Control endpoint read cancelled for shutdown");
                    break 'event_loop;
                }
                if waiting_screen_visible || !matches!(pattern_mode, PatternMode::Off) {
                    continue;
                }
                tracing::debug!(
                    error = %err,
                    "EP0 read failed without a FunctionFS lifecycle event; retaining persistent session"
                );
                continue;
            }
        };
        let completed_payload = if event.is_none() {
            ready_exact_payload.take()
        } else {
            None
        };
        if event.is_none() && completed_payload.is_none() {
            continue;
        }

        let control_event_start = std::time::Instant::now();
        let mut ready_payload_stats = None;
        let parsed_gud_event = if let Some((info, stats)) = completed_payload {
            ready_payload_stats = Some(stats);
            Ok(Some(Event::Buffer(info)))
        } else {
            let event = event.expect("raw event disappeared");
            tracing::debug!("Received event: {:?}", event);
            if bulk_receive_session.current_state() == BulkReceiveState::Poisoned
                && !matches!(
                    event,
                    FunctionFsEvent::Enable
                        | FunctionFsEvent::Bind
                        | FunctionFsEvent::Disable
                        | FunctionFsEvent::Unbind
                        | FunctionFsEvent::Suspend
                        | FunctionFsEvent::Resume
                )
            {
                tracing::warn!(
                    event = "contained_non_lifecycle_ep0_suppressed",
                    usb_epoch = bulk_receive_session.epoch_id(),
                    transaction_seq = gud_data.exact_aio_sequence(),
                    "suppressed EP0 protocol work until a terminal FunctionFS boundary is observed"
                );
                continue;
            }
            gud_gadget::event(event)
        };
        let control_event_ms = control_event_start.elapsed().as_millis();
        match parsed_gud_event {
            Ok(Some(gud_event)) => {
                tracing::debug!("GUD event: {:?}", gud_event);
                record_host_activity(&mut had_host_session, &gud_event);
                if let Event::ProtocolStateInvalidated { reason, .. } = &gud_event {
                    let lifecycle_result = observe_aio_lifecycle(
                        *reason,
                        usb_lifecycle_input(*reason),
                        LifecycleObservation {
                            usb_session: &mut usb_session,
                            session: &bulk_receive_session,
                            endpoint: &mut gud_data,
                            pending_exact_receive: &pending_exact_receive,
                            ready_exact_payload: &ready_exact_payload,
                            status_on_set_cleanup_drain: &status_on_set_cleanup_drain,
                        },
                        &mut receive_telemetry,
                    );
                    match lifecycle_result {
                        UsbLifecycleResult::Continue(_) => {
                            if *reason == ProtocolInvalidationReason::Enable {
                                let epoch_id = gud_data.usb_activation_generation();
                                if epoch_id > bulk_receive_session.epoch_id() {
                                    gud_data.begin_exact_aio_usb_epoch(epoch_id)?;
                                    bulk_receive_session.begin_usb_epoch(epoch_id)?;
                                }
                            }
                        }
                        UsbLifecycleResult::PreserveOwned(_) => continue,
                        UsbLifecycleResult::SessionBoundary(_) => {
                            let new_epoch = (*reason == ProtocolInvalidationReason::Enable)
                                .then(|| gud_data.usb_activation_generation());
                            pending_epoch_boundary = Some((*reason, new_epoch));
                            continue;
                        }
                        UsbLifecycleResult::Contained => continue,
                        UsbLifecycleResult::FatalUnbind => {
                            lifecycle_error = Some(anyhow::anyhow!(
                                "FunctionFS UNBIND observed from proven Idle; administrative removal is not a cable disconnect"
                            ));
                            break 'event_loop;
                        }
                    }
                }
                // A second accepted SET_BUFFER would conflict with the exact EP1
                // receive. Other GUD events are EP0 control-plane work and remain
                // serviceable while that receive is safely owned.
                if pending_exact_receive.is_some()
                    && conflicts_with_owned_exact_receive(&gud_event, ready_payload_stats.is_some())
                {
                    let reason = format!(
                        "unexpected high-level event while exact AIO accepted: {gud_event:?}"
                    );
                    let (_, started, _) = pending_exact_receive
                        .take()
                        .expect("pending exact receive disappeared during containment");
                    gud_data.poison_exact_payload_aio(&reason);
                    let _ = bulk_receive_session
                        .finish_receive::<()>(started, Err(anyhow::anyhow!(reason)));
                    continue;
                }
                match gud_event {
                    Event::GetDescriptor(req) => {
                        if let Err(err) = req.send_descriptor_with_options(
                            min_width,
                            min_height,
                            max_width,
                            max_height,
                            descriptor_compression,
                            descriptor_max_buffer_size,
                            descriptor_flags,
                        ) {
                            tracing::error!("Failed to send descriptor: {}", err);
                        } else {
                            tracing::debug!("Sent descriptor");
                        }
                    }
                    Event::GetPixelFormats(req) => {
                        if let Err(err) =
                            req.send_pixel_formats(&[transfer_format.gud_pixel_format()])
                        {
                            tracing::error!("Failed to send pixel formats: {}", err);
                        } else {
                            tracing::debug!("Sent pixel formats: {:?}", transfer_format);
                        }
                    }
                    Event::GetDisplayModes(req) => {
                        tracing::debug!("Sending {} display modes", advertised_modes.len());
                        if let Err(err) = req.send_modes(&advertised_modes) {
                            tracing::error!("Failed to send modes: {}", err);
                        } else {
                            tracing::debug!("Sent display modes");
                        }
                    }
                    Event::StatusSent(status) => {
                        let observation = status_on_set_cleanup_drain
                            .as_mut()
                            .map(|drain| drain.note_status(status))
                            .transpose();
                        if let Err(err) = observation {
                            if let Some((_, started, _)) = pending_exact_receive.take() {
                                gud_data.poison_exact_payload_aio(
                                    "invalid STATUS_ON_SET diagnostic GET_STATUS ordering",
                                );
                                let _ = bulk_receive_session.finish_receive::<()>(
                                    started,
                                    Err(err.context(
                                        "validate STATUS_ON_SET diagnostic GET_STATUS ordering",
                                    )),
                                );
                                continue;
                            }
                            lifecycle_error =
                                Some(err.context(
                                    "validate STATUS_ON_SET diagnostic GET_STATUS ordering",
                                ));
                            break 'event_loop;
                        }
                        match gud_data.note_status_sent(status) {
                            Err(err) => {
                                if let Some((_, started, _)) = pending_exact_receive.take() {
                                    gud_data.poison_exact_payload_aio(
                                        "invalid GET_STATUS result after arm",
                                    );
                                    let _ = bulk_receive_session.finish_receive::<()>(
                                        started,
                                        Err(err.context("validate GET_STATUS after exact AIO arm")),
                                    );
                                }
                                tracing::error!("Invalid status transition");
                            }
                            Ok(false) => {
                                tracing::debug!(
                                    status,
                                    exact_aio_state = ?gud_data.exact_aio_state(),
                                    receiver_state = ?bulk_receive_session.current_state(),
                                    "GET_STATUS belongs to later control activity and does not change exact receive ownership"
                                );
                            }
                            Ok(true) => {}
                        }
                        if let Some(observation) =
                            observation.expect("STATUS_ON_SET diagnostic observation disappeared")
                        {
                            let transaction = status_on_set_cleanup_drain
                                .as_ref()
                                .expect("STATUS_ON_SET cleanup drain disappeared")
                                .transaction;
                            tracing::info!(
                                event = "functionfs_status_on_set_status_observed",
                                transaction,
                                ?observation,
                                exact_aio_state = ?gud_data.exact_aio_state(),
                                receiver_state = ?bulk_receive_session.current_state(),
                                "Observed STATUS_ON_SET diagnostic control status"
                            );
                            let cleanup_complete = status_on_set_cleanup_drain
                                .as_ref()
                                .is_some_and(StatusOnSetCleanupDrain::is_drained);
                            if cleanup_complete {
                                ensure!(
                                    gud_data.exact_aio_state() == gud_gadget::ExactAioState::Idle
                                        && bulk_receive_session.current_state() == BulkReceiveState::Idle,
                                    "STATUS_ON_SET cleanup drained before both receive guards returned to Idle"
                                );
                                gud_gadget::finish_status_on_set_diagnostic_transaction().context(
                                    "return bounded STATUS_ON_SET receive guard to Idle",
                                )?;
                                tracing::info!(
                                    event = "status_on_set_cleanup_statuses_drained",
                                    transaction,
                                    exact_aio_state = ?gud_data.exact_aio_state(),
                                    receiver_state = ?bulk_receive_session.current_state(),
                                    "Drained both host cleanup statuses after STATUS_ON_SET payload"
                                );
                                if transaction == status_on_set_transaction_limit {
                                    tracing::info!(
                                        event = "status_on_set_diagnostic_complete",
                                        transaction_limit = status_on_set_transaction_limit,
                                        completion_count = transaction,
                                        "all bounded STATUS_ON_SET transactions completed; detaching only from Idle"
                                    );
                                    break 'event_loop;
                                }
                                tracing::info!(
                                    event = "status_on_set_transaction_returned_idle",
                                    transaction,
                                    next_transaction = transaction + 1,
                                    "transaction cleanup drained; allowing the next intentional host commit"
                                );
                                status_on_set_cleanup_drain = None;
                            }
                        }
                    }
                    Event::StateChecked(snapshot) => {
                        let catalog_route = route_catalog
                            .entry_for_snapshot(&snapshot)
                            .map(|entry| entry.route);
                        if dynamic_mode_match {
                            let result = match active {
                                StableMappedActive::Slot0(mapped) => {
                                    let result = prepare_dynamic_check_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        Some((2, slot_mut!(2))),
                                        &route_catalog,
                                        snapshot,
                                        control_event_ms,
                                    );
                                    active = StableMappedActive::Slot0(mapped);
                                    result
                                }
                                StableMappedActive::Slot1(mapped) => {
                                    let result = prepare_dynamic_check_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        2,
                                        slot_mut!(2),
                                        None,
                                        &route_catalog,
                                        snapshot,
                                        control_event_ms,
                                    );
                                    active = StableMappedActive::Slot1(mapped);
                                    result
                                }
                                StableMappedActive::Slot2(mapped) => {
                                    let result = prepare_dynamic_check_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        None,
                                        &route_catalog,
                                        snapshot,
                                        control_event_ms,
                                    );
                                    active = StableMappedActive::Slot2(mapped);
                                    result
                                }
                            };
                            if let Err(err) = result {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to prepare dynamic state-check plan; optimization gate \
                                     must fail"
                                );
                            }
                        } else {
                            scanout_counters.no_op();
                            tracing::debug!(
                                generation = snapshot.generation,
                                mode = ?snapshot.mode,
                                ?catalog_route,
                                "State check notification ignored while dynamic matching is disabled"
                            );
                        }
                    }
                    Event::StateCommitted(snapshot) => {
                        if !dynamic_mode_match {
                            tracing::debug!(
                                generation = snapshot.generation,
                                mode = ?snapshot.mode,
                                commit_control_ms = control_event_ms,
                                "State commit notification ignored while dynamic matching is \
                                 disabled"
                            );
                            continue;
                        }

                        let logical_key = ModeKey::from_snapshot(&snapshot);
                        let plan = match take_matching_pending_plan(
                            &mut scanout_runtime.dynamic_routes_mut()?.pending_plan,
                            &snapshot,
                        ) {
                            Ok(plan) => plan,
                            Err(had_plan) => {
                                let result = match active {
                                    StableMappedActive::Slot0(mapped) => {
                                        let result = invalidate_dynamic_pending_in_available_slots(
                                            &mut backend,
                                            &mut scanout_runtime,
                                            1,
                                            slot_mut!(1),
                                            Some((2, slot_mut!(2))),
                                        );
                                        active = StableMappedActive::Slot0(mapped);
                                        result
                                    }
                                    StableMappedActive::Slot1(mapped) => {
                                        let result = invalidate_dynamic_pending_in_available_slots(
                                            &mut backend,
                                            &mut scanout_runtime,
                                            2,
                                            slot_mut!(2),
                                            None,
                                        );
                                        active = StableMappedActive::Slot1(mapped);
                                        result
                                    }
                                    StableMappedActive::Slot2(mapped) => {
                                        let result = invalidate_dynamic_pending_in_available_slots(
                                            &mut backend,
                                            &mut scanout_runtime,
                                            1,
                                            slot_mut!(1),
                                            None,
                                        );
                                        active = StableMappedActive::Slot2(mapped);
                                        result
                                    }
                                };
                                if let Err(err) = result {
                                    tracing::error!(
                                        error = %format_args!("{err:#}"),
                                        "Failed to release candidate after commit-plan mismatch"
                                    );
                                }
                                shadow
                                    .activate_scaled(shadow_identity(&snapshot))
                                    .context("activate scaled shadow after commit-plan mismatch")?;
                                record_committed_route(
                                    &mut scanout_runtime,
                                    snapshot.clone(),
                                    PresentationRoute::ScaledCurrentAfterFailure {
                                        logical: logical_key,
                                        reason: FallbackReason::PlanMismatch,
                                    },
                                    None,
                                )?;
                                scanout_runtime.counters().fallback();
                                tracing::error!(
                                    event = "mode_commit_decision",
                                    generation = snapshot.generation,
                                    logical = ?logical_key,
                                    commit_control_ms = control_event_ms,
                                    had_plan,
                                    active_slot = active.index(),
                                    "Commit had no exact generation/snapshot plan; retained current \
                                     scanout and failed the optimization gate"
                                );
                                continue;
                            }
                        };

                        let old_physical_key =
                            scanout_runtime.dynamic_routes()?.current_physical_key;
                        let mut mode_switch_ms = 0u128;
                        let mut switch_error: Option<anyhow::Error> = None;
                        let route = match plan.kind {
                            PendingPlanKind::ExactActiveNoOp => {
                                shadow.invalidate_content();
                                scanout_runtime.counters().no_op();
                                PresentationRoute::DirectExact {
                                    logical: logical_key,
                                    physical: old_physical_key,
                                }
                            }
                            PendingPlanKind::ScaledCurrentAfterFailure(reason) => {
                                shadow.activate_scaled(shadow_identity(&snapshot)).context(
                                    "activate scaled shadow for cached/current fallback",
                                )?;
                                PresentationRoute::ScaledCurrentAfterFailure {
                                    logical: logical_key,
                                    reason,
                                }
                            }
                            PendingPlanKind::ScaledBaseline {
                                switch_required: false,
                            } => {
                                ensure!(
                                    active.index() == scanout_runtime.roles().baseline,
                                    "baseline no-switch plan does not match active slot"
                                );
                                shadow
                                    .activate_scaled(shadow_identity(&snapshot))
                                    .context("activate scaled baseline shadow")?;
                                PresentationRoute::ScaledBaseline {
                                    logical: logical_key,
                                }
                            }
                            PendingPlanKind::ExactCandidate {
                                slot: candidate_slot,
                            } => {
                                let requested_key = plan
                                    .requested_physical_key
                                    .context("exact candidate plan has no physical key")?;
                                let switch_succeeded;
                                match (active, candidate_slot) {
                                    (StableMappedActive::Slot0(old), 1) => {
                                        let target = slot_mut!(1)
                                            .as_deref_mut()
                                            .context("candidate slot 1 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(1)?;
                                                ensure!(old_slot == 0);
                                                active = StableMappedActive::Slot1(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                scanout_runtime.release_candidate(
                                                    &mut backend,
                                                    1,
                                                    slot_mut!(1),
                                                )?;
                                                active = StableMappedActive::Slot0(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    (StableMappedActive::Slot0(old), 2) => {
                                        let target = slot_mut!(2)
                                            .as_deref_mut()
                                            .context("candidate slot 2 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(2)?;
                                                ensure!(old_slot == 0);
                                                active = StableMappedActive::Slot2(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                scanout_runtime.release_candidate(
                                                    &mut backend,
                                                    2,
                                                    slot_mut!(2),
                                                )?;
                                                active = StableMappedActive::Slot0(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    (StableMappedActive::Slot1(old), 2) => {
                                        let target = slot_mut!(2)
                                            .as_deref_mut()
                                            .context("candidate slot 2 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(2)?;
                                                ensure!(old_slot == 1);
                                                scanout_runtime.release_inactive(
                                                    &mut backend,
                                                    1,
                                                    slot_mut!(1),
                                                )?;
                                                active = StableMappedActive::Slot2(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                scanout_runtime.release_candidate(
                                                    &mut backend,
                                                    2,
                                                    slot_mut!(2),
                                                )?;
                                                active = StableMappedActive::Slot1(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    (StableMappedActive::Slot2(old), 1) => {
                                        let target = slot_mut!(1)
                                            .as_deref_mut()
                                            .context("candidate slot 1 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(1)?;
                                                ensure!(old_slot == 2);
                                                scanout_runtime.release_inactive(
                                                    &mut backend,
                                                    2,
                                                    slot_mut!(2),
                                                )?;
                                                active = StableMappedActive::Slot1(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                scanout_runtime.release_candidate(
                                                    &mut backend,
                                                    1,
                                                    slot_mut!(1),
                                                )?;
                                                active = StableMappedActive::Slot2(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    (old, unexpected_slot) => {
                                        active = old;
                                        anyhow::bail!(
                                            "candidate slot {unexpected_slot} is incompatible with \
                                             active slot {}",
                                            active.index()
                                        );
                                    }
                                }
                                if switch_succeeded {
                                    scanout_runtime.counters().switched();
                                    shadow.invalidate_content();
                                    record_committed_route(
                                        &mut scanout_runtime,
                                        snapshot.clone(),
                                        PresentationRoute::DirectExact {
                                            logical: logical_key,
                                            physical: requested_key,
                                        },
                                        Some(requested_key),
                                    )?;
                                    PresentationRoute::DirectExact {
                                        logical: logical_key,
                                        physical: requested_key,
                                    }
                                } else {
                                    let failed_key =
                                        scanout_runtime.dynamic_routes()?.failed_key(logical_key);
                                    scanout_runtime
                                        .dynamic_routes_mut()?
                                        .failed_routes
                                        .insert(failed_key);
                                    scanout_runtime.counters().fallback();
                                    shadow.activate_scaled(shadow_identity(&snapshot)).context(
                                        "activate scaled shadow after exact switch failure",
                                    )?;
                                    PresentationRoute::ScaledCurrentAfterFailure {
                                        logical: logical_key,
                                        reason: FallbackReason::SwitchFailed,
                                    }
                                }
                            }
                            PendingPlanKind::ScaledBaseline {
                                switch_required: true,
                            } => {
                                let switch_succeeded;
                                match active {
                                    StableMappedActive::Slot1(old) => {
                                        let target = slot_mut!(0)
                                            .as_deref_mut()
                                            .context("baseline slot 0 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(0)?;
                                                ensure!(old_slot == 1);
                                                scanout_runtime.release_inactive(
                                                    &mut backend,
                                                    1,
                                                    slot_mut!(1),
                                                )?;
                                                active = StableMappedActive::Slot0(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                active = StableMappedActive::Slot1(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    StableMappedActive::Slot2(old) => {
                                        let target = slot_mut!(0)
                                            .as_deref_mut()
                                            .context("baseline slot 0 is empty at commit")?;
                                        match attempt_mapped_switch(
                                            &mut backend,
                                            old,
                                            target,
                                            &scanout_counters,
                                        ) {
                                            MappedSwitch::Switched {
                                                active: target,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                let old_slot = scanout_runtime.activate_slot(0)?;
                                                ensure!(old_slot == 2);
                                                scanout_runtime.release_inactive(
                                                    &mut backend,
                                                    2,
                                                    slot_mut!(2),
                                                )?;
                                                active = StableMappedActive::Slot0(target);
                                                switch_succeeded = true;
                                            }
                                            MappedSwitch::Retained {
                                                active: old,
                                                error,
                                                mode_switch_ms: elapsed,
                                            } => {
                                                mode_switch_ms = elapsed;
                                                switch_error = Some(error);
                                                active = StableMappedActive::Slot2(old);
                                                switch_succeeded = false;
                                            }
                                        }
                                    }
                                    StableMappedActive::Slot0(old) => {
                                        drop(old);
                                        anyhow::bail!(
                                            "baseline switch plan was marked required while \
                                             baseline was already active"
                                        );
                                    }
                                }
                                shadow
                                    .activate_scaled(shadow_identity(&snapshot))
                                    .context("activate shadow for scaled baseline route")?;
                                if switch_succeeded {
                                    scanout_runtime.counters().switched();
                                    PresentationRoute::ScaledBaseline {
                                        logical: logical_key,
                                    }
                                } else {
                                    scanout_runtime.counters().fallback();
                                    PresentationRoute::ScaledCurrentAfterFailure {
                                        logical: logical_key,
                                        reason: FallbackReason::BaselineSwitchFailed,
                                    }
                                }
                            }
                        };

                        if !matches!(plan.kind, PendingPlanKind::ExactCandidate { .. })
                            || !matches!(route, PresentationRoute::DirectExact { .. })
                        {
                            record_committed_route(
                                &mut scanout_runtime,
                                snapshot.clone(),
                                route,
                                None,
                            )?;
                        }
                        if mode_switch_ms > 250 {
                            tracing::warn!(
                                generation = snapshot.generation,
                                mode_switch_ms,
                                "Commit-time physical switch exceeded the provisional 250 ms \
                                 acceptance budget; the hardware gate must fail"
                            );
                        }
                        if let Some(error) = switch_error.as_ref() {
                            tracing::warn!(
                                generation = snapshot.generation,
                                logical = ?logical_key,
                                retained_physical = ?scanout_runtime
                                    .dynamic_routes()?
                                    .current_physical_key,
                                error = %format_args!("{error:#}"),
                                "Single commit-time physical switch failed; retained old mapped \
                                 scanout without retry"
                            );
                        }
                        tracing::info!(
                            event = "mode_commit_decision",
                            generation = snapshot.generation,
                            logical = ?logical_key,
                            old_physical = ?old_physical_key,
                            new_physical = ?scanout_runtime.dynamic_routes()?.current_physical_key,
                            ?route,
                            active_slot = active.index(),
                            active_size = ?active.size(),
                            active_pitch = active.pitch(),
                            active_mapping_lengths = ?active.mapping_lengths(),
                            state_commit_control_ms = control_event_ms,
                            mode_prepare_ms = plan.mode_prepare_ms,
                            mode_switch_ms,
                            failed_cache_hit = plan.failed_cache_hit,
                            counters = ?scanout_runtime.counters().snapshot(),
                            current_live_bytes = scanout_runtime.current_live_bytes(),
                            observed_peak_live_bytes =
                                scanout_runtime.observed_peak_live_bytes(),
                            "Committed generation-bound dynamic presentation route"
                        );
                    }
                    Event::ProtocolStateInvalidated {
                        generation,
                        reason:
                            reason @ (ProtocolInvalidationReason::InvalidStateCheck
                            | ProtocolInvalidationReason::CommitWithoutPending
                            | ProtocolInvalidationReason::Bind
                            | ProtocolInvalidationReason::Enable
                            | ProtocolInvalidationReason::Suspend
                            | ProtocolInvalidationReason::Resume
                            | ProtocolInvalidationReason::Unbind),
                    } => {
                        let diagnostic_guard = gud_gadget::status_on_set_diagnostic_guard_state();
                        if diagnostic_guard.transaction_limit > 0
                            && diagnostic_guard.started_transactions > 0
                        {
                            tracing::error!(
                                event = "status_on_set_lifecycle_containment",
                                transaction_limit = diagnostic_guard.transaction_limit,
                                started_transactions = diagnostic_guard.started_transactions,
                                active = diagnostic_guard.active,
                                ?reason,
                                "lifecycle invalidation is contained rather than resetting diagnostic state"
                            );
                            continue;
                        }
                        if reason == ProtocolInvalidationReason::Enable {
                            if let Some(prearm_bytes) = test_prearm_once_bytes {
                                ensure!(
                                    prearmed_receive_started.is_none(),
                                    "received a second FunctionFS Enable while the STATUS_ON_SET diagnostic prearmed read was active"
                                );
                                let started = bulk_receive_session.begin_receive().context(
                                    "begin STATUS_ON_SET diagnostic prearmed FunctionFS receive",
                                )?;
                                if let Err(err) = gud_data.prearm_payload_read(prearm_bytes) {
                                    let cancel_result = bulk_receive_session
                                        .cancel_receive_before_io()
                                        .context("cancel failed FunctionFS prearm state");
                                    lifecycle_error = Some(match cancel_result {
                                        Ok(()) => err.context(
                                            "submit STATUS_ON_SET diagnostic prearmed FunctionFS bulk OUT read",
                                        ),
                                        Err(cancel_err) => cancel_err.context(format!(
                                            "prearm submission also failed: {err:#}"
                                        )),
                                    });
                                    break 'event_loop;
                                }
                                prearmed_receive_started = Some(started);
                                tracing::info!(
                                    event = "functionfs_prearm_ready",
                                    prearm_bytes,
                                    "STATUS_ON_SET diagnostic FunctionFS bulk OUT read is active before SET_BUFFER"
                                );
                            }
                        }
                        if dynamic_mode_match {
                            let result = match active {
                                StableMappedActive::Slot0(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        Some((2, slot_mut!(2))),
                                    );
                                    active = StableMappedActive::Slot0(mapped);
                                    result
                                }
                                StableMappedActive::Slot1(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        2,
                                        slot_mut!(2),
                                        None,
                                    );
                                    active = StableMappedActive::Slot1(mapped);
                                    result
                                }
                                StableMappedActive::Slot2(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        None,
                                    );
                                    active = StableMappedActive::Slot2(mapped);
                                    result
                                }
                            };
                            if let Err(err) = result {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to release invalidated dynamic candidate"
                                );
                            }
                        }
                        tracing::debug!(
                            ?generation,
                            ?reason,
                            had_host_session,
                            "Protocol state invalidated"
                        );
                    }
                    Event::ProtocolStateInvalidated {
                        generation,
                        reason: ProtocolInvalidationReason::Disconnected,
                    } => {
                        let diagnostic_guard = gud_gadget::status_on_set_diagnostic_guard_state();
                        if diagnostic_guard.transaction_limit > 0
                            && diagnostic_guard.started_transactions > 0
                        {
                            tracing::error!(
                                event = "status_on_set_lifecycle_containment",
                                transaction_limit = diagnostic_guard.transaction_limit,
                                started_transactions = diagnostic_guard.started_transactions,
                                active = diagnostic_guard.active,
                                "disconnect is contained rather than resetting diagnostic state"
                            );
                            continue;
                        }
                        if dynamic_mode_match {
                            let result = match active {
                                StableMappedActive::Slot0(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        Some((2, slot_mut!(2))),
                                    );
                                    active = StableMappedActive::Slot0(mapped);
                                    result
                                }
                                StableMappedActive::Slot1(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        2,
                                        slot_mut!(2),
                                        None,
                                    );
                                    active = StableMappedActive::Slot1(mapped);
                                    result
                                }
                                StableMappedActive::Slot2(mapped) => {
                                    let result = invalidate_dynamic_pending_in_available_slots(
                                        &mut backend,
                                        &mut scanout_runtime,
                                        1,
                                        slot_mut!(1),
                                        None,
                                    );
                                    active = StableMappedActive::Slot2(mapped);
                                    result
                                }
                            };
                            if let Err(err) = result {
                                tracing::error!(
                                    error = %format_args!("{err:#}"),
                                    "Failed to release disconnected dynamic candidate"
                                );
                            }
                        }
                        tracing::info!(?generation, had_host_session, "Host disconnected");
                        if matches!(pattern_mode, PatternMode::Off) {
                            if let Err(err) = present_waiting_screen(
                                &mut backend,
                                &mut active,
                                dump_path.as_deref(),
                                dump_raw_path.as_deref(),
                                transfer_format,
                            ) {
                                tracing::error!(
                                    "Failed to render waiting screen after disconnect: {}",
                                    err
                                );
                            } else {
                                waiting_screen_visible = true;
                            }
                        }
                    }
                    Event::Buffer(info) => {
                        // Completion processing is re-dispatched as Buffer; only EP0 input is
                        // an actual SET_BUFFER admission attempt.
                        if ready_payload_stats.is_none() {
                            receive_telemetry.set_buffer_seen += 1;
                        }
                        let frame_start = std::time::Instant::now();
                        tracing::debug!(
                            "Buffer: x={} y={} {}x{} len={} compression={}",
                            info.x,
                            info.y,
                            info.width,
                            info.height,
                            info.length,
                            info.compression
                        );
                        let clip = ClipRect::new(
                            info.x as u16,
                            info.y as u16,
                            (info.x + info.width) as u16,
                            (info.y + info.height) as u16,
                        );
                        let receive_result = if aio_mode {
                            if let Some(stats) = ready_payload_stats.take() {
                                Ok(stats)
                            } else {
                                let started = bulk_receive_session
                                    .begin_receive()
                                    .context("enter Arming before exact AIO submission")?;
                                receive_telemetry.submit_attempts += 1;
                                let transaction_seq = match gud_data.arm_exact_payload_aio(&info) {
                                    Ok(sequence) => sequence,
                                    Err(err) => {
                                        bulk_receive_session
                                            .cancel_receive_before_io()
                                            .context("return Idle after rejected AIO submission")?;
                                        tracing::error!(
                                            error = %format_args!("{err:#}"),
                                            "Exact FunctionFS AIO submission was rejected; GET_STATUS will report an error"
                                        );
                                        continue;
                                    }
                                };
                                receive_telemetry.accepted += 1;
                                receive_telemetry.submit_accepted += 1;
                                receive_telemetry.last_sequence = transaction_seq;
                                receive_telemetry.validation_to_accept_ms_total +=
                                    frame_start.elapsed().as_millis();
                                bulk_receive_session
                                    .submission_accepted()
                                    .context("record exact AIO acceptance")?;
                                let transaction =
                                    match gud_gadget::begin_status_on_set_diagnostic_transaction() {
                                        Ok(transaction) => transaction,
                                        Err(err) => {
                                            gud_data.poison_exact_payload_aio(
                                            "diagnostic receive guard could not claim accepted AIO",
                                        );
                                            let _ = bulk_receive_session.finish_receive::<()>(
                                                started,
                                                Err(err.context(
                                                    "claim bounded STATUS_ON_SET transaction",
                                                )),
                                            );
                                            continue;
                                        }
                                    };
                                if status_on_set_transaction_limit > 0 {
                                    ensure!(
                                        u64::from(transaction) == transaction_seq,
                                        "STATUS_ON_SET transaction number {transaction} does not match AIO sequence {transaction_seq}"
                                    );
                                }
                                if status_on_set_transaction_limit > 0 {
                                    status_on_set_cleanup_drain =
                                        Some(StatusOnSetCleanupDrain::new(transaction));
                                }
                                tracing::info!(
                                    event = "status_on_set_transaction_armed",
                                    transaction,
                                    transaction_limit = status_on_set_transaction_limit,
                                    expected_bytes = test_status_on_set_bytes,
                                    aio_sequence = transaction_seq,
                                    receiver_state = ?bulk_receive_session.current_state(),
                                    "accepted valid SET_BUFFER and armed exactly one AIO request"
                                );
                                pending_exact_receive = Some((info, started, transaction_seq));
                                continue;
                            }
                        } else if prearmed_receive_started.take().is_some() {
                            // A prearmed read may wait for the host indefinitely. Apply the
                            // receive deadline only from the point where SET_BUFFER hands the
                            // completed (or still pending) read back to the normal event path.
                            let started = std::time::Instant::now();
                            let result = gud_data.recv_payload_bytes(&info);
                            bulk_receive_session.finish_receive(started, result)
                        } else {
                            bulk_receive_session.receive(|| gud_data.recv_payload_bytes(&info))
                        };
                        let payload_stats = match receive_result {
                            Ok(result) => result,
                            Err(err) => {
                                if let Some(io_err) = err
                                    .chain()
                                    .find_map(|cause| cause.downcast_ref::<std::io::Error>())
                                {
                                    tracing::error!(
                                        error = ?err,
                                        error_chain = %format_args!("{err:#}"),
                                        io_error_kind = ?io_err.kind(),
                                        raw_os_error = ?io_err.raw_os_error(),
                                        "Failed to receive buffer payload"
                                    );
                                } else {
                                    tracing::error!(
                                        error = ?err,
                                        error_chain = %format_args!("{err:#}"),
                                        "Failed to receive buffer payload"
                                    );
                                }
                                if !running.load(Ordering::Acquire) {
                                    tracing::info!("Bulk endpoint read cancelled for shutdown");
                                    break 'event_loop;
                                }
                                tracing::error!(
                                    state = ?bulk_receive_session.current_state(),
                                    "FunctionFS bulk receive session is terminal; refusing all \
                                     further USB/control processing and automatic teardown. Do not \
                                     stop, restart, reboot, or shut down this service instance; \
                                     recover with a physical/hardware reset and collect \
                                     previous-boot evidence"
                                );
                                continue;
                            }
                        };
                        let (payload, payload_stats) = match gud_data.finish_payload(
                            &info,
                            transfer_format.bytes_per_pixel(),
                            payload_stats,
                        ) {
                            Ok(result) => result,
                            Err(err) => {
                                if aio_mode {
                                    gud_data
                                        .poison_exact_payload_aio("payload post-processing failed");
                                    bulk_receive_session.poison();
                                    receive_telemetry.processing_failed += 1;
                                    receive_telemetry.poisoned += 1;
                                    receive_telemetry.emit(&bulk_receive_session, &gud_data);
                                }
                                tracing::error!(
                                    error = ?err,
                                    error_chain = %format_args!("{err:#}"),
                                    "Failed to post-process received buffer payload; production \
                                     exact-AIO session is contained"
                                );
                                continue;
                            }
                        };
                        tracing::debug!(
                            event = "payload_completed",
                            payload_seq = payload_stats.payload_seq,
                            transfer_bytes = payload_stats.transfer_bytes,
                            read_calls = payload_stats.read_calls,
                            "FunctionFS payload completed before framebuffer processing"
                        );
                        if let Some(prearm_bytes) = test_prearm_once_bytes {
                            tracing::info!(
                                event = "functionfs_prearm_one_shot_complete",
                                prearm_bytes,
                                payload_seq = payload_stats.payload_seq,
                                transfer_bytes = payload_stats.transfer_bytes,
                                read_calls = payload_stats.read_calls,
                                "STATUS_ON_SET diagnostic prearmed FunctionFS payload completed; leaving the event loop before another SET_BUFFER"
                            );
                            break 'event_loop;
                        }

                        let active_state = gud_gadget::active_scanout_state();
                        let source_width =
                            active_state.map(|state| state.width).unwrap_or(panel_width);
                        let source_height = active_state
                            .map(|state| state.height)
                            .unwrap_or(panel_height);
                        let (physical_width, physical_height) = active.size();
                        let physical_pitch = active.pitch();
                        let scaled_mode = if dynamic_mode_match {
                            matches!(
                                scanout_runtime.dynamic_routes()?.presentation_route,
                                Some(
                                    PresentationRoute::ScaledBaseline { .. }
                                        | PresentationRoute::ScaledCurrentAfterFailure { .. }
                                )
                            )
                        } else {
                            source_width != physical_width || source_height != physical_height
                        };
                        let mut copy_ms = 0u128;
                        let mut scale_ms = 0u128;
                        let framebuffer_changed = match pattern_mode {
                            PatternMode::Off | PatternMode::Startup => {
                                if scaled_mode {
                                    let identity = ShadowRasterIdentity {
                                        width: source_width,
                                        height: source_height,
                                        format: transfer_format.gud_pixel_format(),
                                    };
                                    if dynamic_mode_match {
                                        ensure!(
                                            shadow.is_active(identity),
                                            "dynamic scaled route shadow identity changed outside \
                                             STATE_COMMIT"
                                        );
                                    } else if let Err(err) = shadow.ensure_size(identity) {
                                        if aio_mode {
                                            gud_data.poison_exact_payload_aio(
                                                "shadow allocation failed during processing",
                                            );
                                            bulk_receive_session.poison();
                                        }
                                        tracing::error!(
                                            "Failed to activate source shadow framebuffer: {}",
                                            err
                                        );
                                        continue;
                                    }
                                    let copy_result = gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer_with_stats(
                                      &info,
                                      payload,
                                      shadow.pixels.as_mut_slice(),
                                      shadow.pitch,
                                      transfer_format.bytes_per_pixel(),
                                  );
                                    match copy_result {
                                        Ok(stats) => {
                                            copy_ms = stats.copy_ms;
                                        }
                                        Err(err) => {
                                            if aio_mode {
                                                gud_data.poison_exact_payload_aio("shadow framebuffer copy failed during processing");
                                                bulk_receive_session.poison();
                                            }
                                            tracing::error!(
                                                "Failed to copy buffer to shadow framebuffer: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }

                                    match scale_to_fit(
                                        shadow.pixels.as_slice(),
                                        shadow.pitch,
                                        source_width,
                                        source_height,
                                        active.back_buffer_mut(),
                                        physical_pitch as usize,
                                        physical_width,
                                        physical_height,
                                        transfer_format,
                                    ) {
                                        Ok((_layout, elapsed_ms)) => {
                                            scale_ms = elapsed_ms;
                                            true
                                        }
                                        Err(err) => {
                                            if aio_mode {
                                                gud_data.poison_exact_payload_aio(
                                                    "shadow scaling failed during processing",
                                                );
                                                bulk_receive_session.poison();
                                            }
                                            tracing::error!(
                                                "Failed to scale shadow framebuffer to panel: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }
                                } else {
                                    let copy_result = gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer_with_stats(
                                        &info,
                                        payload,
                                        active.front_buffer_mut(),
                                        physical_pitch as usize,
                                        transfer_format.bytes_per_pixel(),
                                    );
                                    match copy_result {
                                        Ok(stats) => {
                                            copy_ms = stats.copy_ms;
                                            true
                                        }
                                        Err(err) => {
                                            if aio_mode {
                                                gud_data.poison_exact_payload_aio(
                                                    "framebuffer copy failed during processing",
                                                );
                                                bulk_receive_session.poison();
                                            }
                                            tracing::error!(
                                                "Failed to copy buffer to framebuffer: {}",
                                                err
                                            );
                                            continue;
                                        }
                                    }
                                }
                            }
                            PatternMode::Hold => {
                                tracing::debug!("Discarded payload in hold mode");
                                false
                            }
                            PatternMode::Usb => {
                                match fill_diagnostic_pattern_rect(
                                    active.front_buffer_mut(),
                                    physical_pitch as usize,
                                    physical_width,
                                    physical_height,
                                    info.x,
                                    info.y,
                                    info.width,
                                    info.height,
                                    transfer_format,
                                ) {
                                    Ok(()) => true,
                                    Err(err) => {
                                        if aio_mode {
                                            gud_data.poison_exact_payload_aio("diagnostic framebuffer fill failed during processing");
                                            bulk_receive_session.poison();
                                        }
                                        tracing::error!(
                                            "Failed to render diagnostic pattern for update rect: {}",
                                            err
                                        );
                                        continue;
                                    }
                                }
                            }
                        };

                        if row_crc_trace
                            && !scaled_mode
                            && matches!(pattern_mode, PatternMode::Off | PatternMode::Startup)
                        {
                            let bpp = transfer_format.bytes_per_pixel();
                            let row_bytes = info.width as usize * bpp;
                            if info.x == 0 && info.y == 0 && info.width == source_width {
                                row_immediate_crcs = vec![None; source_height as usize];
                            }
                            {
                                let framebuffer = active.front_buffer_mut();
                                for row_offset in 0..info.height as usize {
                                    let row = info.y as usize + row_offset;
                                    let payload_start = row_offset * row_bytes;
                                    let payload_end = payload_start + row_bytes;
                                    let framebuffer_start =
                                        row * physical_pitch as usize + info.x as usize * bpp;
                                    let framebuffer_end = framebuffer_start + row_bytes;
                                    let logical_crc =
                                        row_crc32(&payload[payload_start..payload_end]);
                                    let immediate_crc =
                                        row_crc32(&framebuffer[framebuffer_start..framebuffer_end]);

                                    if row < row_immediate_crcs.len() {
                                        row_immediate_crcs[row] = Some(immediate_crc);
                                    }
                                    tracing::info!(
                                        event = "row_crc_immediate",
                                        row,
                                        pi_logical_rx_crc =
                                            format_args!("{logical_crc:08x}"),
                                        pi_fb_immediate_crc =
                                            format_args!("{immediate_crc:08x}"),
                                        "captured logical receive and immediate framebuffer row CRC"
                                    );
                                }
                            }

                            if info.x == 0
                                && info.width == source_width
                                && info.y + info.height == source_height
                            {
                                let framebuffer = active.front_buffer_mut();
                                for row in 0..source_height as usize {
                                    let start = row * physical_pitch as usize;
                                    let end = start + source_width as usize * bpp;
                                    let final_crc = row_crc32(&framebuffer[start..end]);
                                    let immediate_crc =
                                        row_immediate_crcs.get(row).and_then(|crc| *crc);
                                    tracing::info!(
                                        event = "row_crc_final",
                                        row,
                                        pi_fb_immediate_crc = format_args!(
                                            "{:08x}",
                                            immediate_crc.unwrap_or_default()
                                        ),
                                        pi_fb_final_crc = format_args!("{final_crc:08x}"),
                                        immediate_present = immediate_crc.is_some(),
                                        "captured final framebuffer row CRC"
                                    );
                                }
                            }
                        }

                        let mut flush_ms = 0u128;
                        if framebuffer_changed {
                            let flush_start = std::time::Instant::now();
                            if scaled_mode
                                && matches!(pattern_mode, PatternMode::Off | PatternMode::Startup)
                            {
                                match backend.dirty_framebuffer(
                                    active.back_framebuffer(),
                                    0,
                                    0,
                                    physical_width as u16,
                                    physical_height as u16,
                                ) {
                                    Ok(()) => tracing::debug!("Back buffer flushed before flip"),
                                    Err(err) => tracing::debug!(
                                        "dirty_framebuffer on back buffer not supported or failed: {}",
                                        err
                                    ),
                                }

                                let present_result = backend
                                    .page_flip(active.back_framebuffer())
                                    .or_else(|page_flip_err| {
                                        tracing::warn!(
                                            "Page flip failed ({}), falling back to set_crtc",
                                            page_flip_err
                                        );
                                        backend.set_crtc(active.back_framebuffer(), active.mode())
                                    });

                                match present_result {
                                    Ok(()) => {
                                        let back_buffer_index = active.back_index();
                                        active.set_front_index(back_buffer_index);
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!(
                                            "Scaled framebuffer presented via back-buffer swap"
                                        );
                                    }
                                    Err(err) => {
                                        if aio_mode {
                                            gud_data.poison_exact_payload_aio(
                                                "framebuffer presentation failed during processing",
                                            );
                                            bulk_receive_session.poison();
                                        }
                                        tracing::error!(
                                            "Failed to present scaled framebuffer: {}",
                                            err
                                        );
                                        continue;
                                    }
                                }
                            } else {
                                match backend.dirty_framebuffer(
                                    active.front_framebuffer(),
                                    clip.x1(),
                                    clip.y1(),
                                    clip.x2(),
                                    clip.y2(),
                                ) {
                                    Ok(()) => {
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!("Framebuffer flushed")
                                    }
                                    Err(err) => {
                                        flush_ms = flush_start.elapsed().as_millis();
                                        tracing::debug!(
                                            "dirty_framebuffer not supported or failed: {}",
                                            err
                                        )
                                    }
                                }
                            }
                        } else {
                            tracing::debug!("Framebuffer unchanged for current buffer event");
                        }

                        let total_ms = frame_start.elapsed().as_millis();
                        let compression_ratio = if payload_stats.transfer_bytes > 0 {
                            payload_stats.output_bytes as f64 / payload_stats.transfer_bytes as f64
                        } else {
                            0.0
                        };
                        let usb_mib_per_s = if payload_stats.read_ms > 0 {
                            (payload_stats.transfer_bytes as f64 / (1024.0 * 1024.0))
                                / (payload_stats.read_ms as f64 / 1000.0)
                        } else {
                            0.0
                        };
                        tracing::info!(
                            "frame_stats payload_seq={} transfer_format={} gud_format={:#04x} bytes_per_pixel={} rect={}x{}+{},{} source={}x{} scaled={} transfer_bytes={} output_bytes={} read_size={} read_calls={} first_request_bytes={} last_request_bytes={} usb_packets_est={} compression={} ratio={:.2} recv_ms={} decompress_ms={} copy_ms={} scale_ms={} flush_ms={} total_ms={} usb_mib_s={:.2}",
                            payload_stats.payload_seq,
                            transfer_format.name(),
                            transfer_format.gud_pixel_format(),
                            transfer_format.bytes_per_pixel(),
                            info.width,
                            info.height,
                            info.x,
                            info.y,
                            source_width,
                            source_height,
                            scaled_mode,
                            payload_stats.transfer_bytes,
                            payload_stats.output_bytes,
                            payload_stats.read_size,
                            payload_stats.read_calls,
                            payload_stats.first_request_bytes,
                            payload_stats.last_request_bytes,
                            payload_stats.usb_packets_est,
                            info.compression,
                            compression_ratio,
                            payload_stats.read_ms,
                            payload_stats.decompress_ms,
                            copy_ms,
                            scale_ms,
                            flush_ms,
                            total_ms,
                            usb_mib_per_s
                        );
                        tracing::debug!(
                            event = "framebuffer_processing_completed",
                            payload_seq = payload_stats.payload_seq,
                            transfer_bytes = payload_stats.transfer_bytes,
                            total_ms,
                            "Normal userspace payload processing completed"
                        );

                        if let Some(status_on_set_bytes) = test_status_on_set_bytes {
                            gud_data.finish_exact_payload_processing().context(
                                "return diagnostic exact AIO transaction to Idle after framebuffer processing",
                            )?;
                            bulk_receive_session.finish_processing().context(
                                "return diagnostic receive session to Idle after framebuffer processing",
                            )?;
                            let drain = status_on_set_cleanup_drain.as_mut().expect(
                                "STATUS_ON_SET diagnostic payload completed without a status drain",
                            );
                            let transaction = drain.transaction;
                            let trailing_status_was_early = drain
                                .payload_processed(Instant::now())
                                .context("advance STATUS_ON_SET diagnostic trailing-status drain after payload processing")?;
                            tracing::info!(
                                event = "status_on_set_payload_complete",
                                transaction,
                                transaction_limit = status_on_set_transaction_limit,
                                status_on_set_bytes,
                                payload_seq = payload_stats.payload_seq,
                                transfer_bytes = payload_stats.transfer_bytes,
                                read_calls = payload_stats.read_calls,
                                exact_aio_state = ?gud_data.exact_aio_state(),
                                receiver_state = ?bulk_receive_session.current_state(),
                                trailing_status_was_early,
                                "STATUS_ON_SET payload completed; retaining admission guard while host cleanup statuses drain"
                            );
                            if trailing_status_was_early {
                                ensure!(
                                    gud_data.exact_aio_state() == gud_gadget::ExactAioState::Idle
                                        && bulk_receive_session.current_state() == BulkReceiveState::Idle,
                                    "STATUS_ON_SET early cleanup drain completed before both receive guards returned to Idle"
                                );
                                gud_gadget::finish_status_on_set_diagnostic_transaction().context(
                                    "return bounded STATUS_ON_SET receive guard to Idle",
                                )?;
                                tracing::info!(
                                    event = "status_on_set_cleanup_statuses_drained",
                                    transaction,
                                    payload_seq = payload_stats.payload_seq,
                                    exact_aio_state = ?gud_data.exact_aio_state(),
                                    receiver_state = ?bulk_receive_session.current_state(),
                                    "Both host cleanup statuses arrived before payload processing completed"
                                );
                                if transaction == status_on_set_transaction_limit {
                                    tracing::info!(
                                        event = "status_on_set_diagnostic_complete",
                                        transaction_limit = status_on_set_transaction_limit,
                                        completion_count = transaction,
                                        "all bounded STATUS_ON_SET transactions completed; detaching only from Idle"
                                    );
                                    break 'event_loop;
                                }
                                tracing::info!(
                                    event = "status_on_set_transaction_returned_idle",
                                    transaction,
                                    next_transaction = transaction + 1,
                                    "transaction cleanup drained; allowing the next intentional host commit"
                                );
                                status_on_set_cleanup_drain = None;
                            }
                            continue;
                        }

                        if aio_mode {
                            if let Some(barrier) = e1_t05_processing_barrier.as_mut() {
                                match barrier.inject_once(gud_data.exact_aio_sequence()) {
                                    Ok(true) => {
                                        let _ = observe_aio_lifecycle(
                                            ProtocolInvalidationReason::Disconnected,
                                            UsbLifecycleInput::Disable,
                                            LifecycleObservation {
                                                usb_session: &mut usb_session,
                                                session: &bulk_receive_session,
                                                endpoint: &mut gud_data,
                                                pending_exact_receive: &pending_exact_receive,
                                                ready_exact_payload: &ready_exact_payload,
                                                status_on_set_cleanup_drain:
                                                    &status_on_set_cleanup_drain,
                                            },
                                            &mut receive_telemetry,
                                        );
                                        continue;
                                    }
                                    Ok(false) => {}
                                    Err(err) => {
                                        gud_data.poison_exact_payload_aio(
                                            "E1-T05 Processing barrier timed out",
                                        );
                                        bulk_receive_session.poison();
                                        receive_telemetry.poisoned += 1;
                                        tracing::error!(
                                            event = "e1_t05_processing_barrier_failed",
                                            error = %format_args!("{err:#}"),
                                            "TEST-ONLY Processing barrier failed; session contained"
                                        );
                                        continue;
                                    }
                                }
                            }
                            gud_data.finish_exact_payload_processing().context(
                                "return exact AIO transaction to Idle after framebuffer processing",
                            )?;
                            bulk_receive_session.finish_processing().context(
                                "return aggregate receive session to Idle after framebuffer processing",
                            )?;
                            gud_gadget::finish_status_on_set_diagnostic_transaction()
                                .context("return production exact AIO admission guard to Idle")?;
                            receive_telemetry.processing_completed += 1;
                            receive_telemetry.processing_ms_total +=
                                frame_start.elapsed().as_millis();
                            receive_telemetry.transaction_ms_total +=
                                frame_start.elapsed().as_millis();
                            receive_telemetry.emit(&bulk_receive_session, &gud_data);
                        }

                        if matches!(pattern_mode, PatternMode::Off) {
                            waiting_screen_visible = false;
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                if let Some((_, started, _)) = pending_exact_receive.as_ref() {
                    gud_data.poison_exact_payload_aio(
                        "GUD protocol parse failure while exact AIO accepted",
                    );
                    tracing::error!(
                        error = ?err,
                        error_chain = %format_args!("{err:#}"),
                        "GUD event processing failed while exact AIO remained accepted"
                    );
                    let _ = bulk_receive_session.finish_receive::<()>(*started, Err(err));
                    continue;
                }
                tracing::warn!("Failed to parse GUD event: {}", err);
            }
        }
    }

    receive_telemetry.cleanup_attempts += 1;
    let shutdown_gate = proven_idle_gate(
        &bulk_receive_session,
        &mut gud_data,
        &pending_exact_receive,
        &ready_exact_payload,
        &status_on_set_cleanup_drain,
    );
    if !shutdown_gate.is_proven_idle() {
        receive_telemetry.unsafe_teardown_suppressed += 1;
        tracing::error!(
            event = "e1_t05_teardown_suppressed",
            usb_session_state = ?usb_session,
            ?shutdown_gate,
            aggregate_state = ?bulk_receive_session.current_state(),
            aio_state = ?gud_data.exact_aio_state(),
            transaction_seq = gud_data.exact_aio_sequence(),
            activation_generation = ?gud_data.exact_aio_activation_generation(),
            accepted_operation_id = gud_data.exact_aio_operation_id(),
            "Refusing teardown because centralized ownership gate is not proven Idle; use physical containment"
        );
        receive_telemetry.emit(&bulk_receive_session, &gud_data);
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    match bulk_receive_session.begin_shutdown() {
        Ok(()) => info!("Idle FunctionFS bulk session claimed for safe detach teardown"),
        Err(BulkReceiveState::ShuttingDown) => {
            info!("Idle FunctionFS bulk session was already claimed for safe teardown")
        }
        Err(state) => {
            receive_telemetry.unsafe_teardown_suppressed += 1;
            tracing::error!(
                event = "e1_t05_teardown_suppressed",
                ?state,
                aggregate_state = ?bulk_receive_session.current_state(),
                aio_state = ?gud_data.exact_aio_state(),
                transaction_seq = gud_data.exact_aio_sequence(),
                accepted_operation_id = gud_data.exact_aio_operation_id(),
                unsafe_teardown_suppressed = true,
                "Refusing teardown because ownership is not proven Idle; use physical containment"
            );
            receive_telemetry.emit(&bulk_receive_session, &gud_data);
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    receive_telemetry.emit(&bulk_receive_session, &gud_data);

    tracing::info!("Shutting down");

    match gadget_shutdown.request_unbind() {
        Ok(GadgetUnbindOutcome::Unbound) => info!("USB gadget unbound from UDC"),
        Ok(GadgetUnbindOutcome::AlreadyUnbound) => {
            debug!("USB gadget already unbound from UDC")
        }
        Ok(GadgetUnbindOutcome::DeferredUntilPublish) | Ok(GadgetUnbindOutcome::Armed) => {
            unreachable!()
        }
        Err(err) => {
            tracing::error!("Failed to unbind USB gadget from UDC: {}", err);
            if lifecycle_error.is_none() {
                lifecycle_error =
                    Some(anyhow::Error::new(err).context("failed to unbind USB gadget from UDC"));
            }
        }
    }

    gadget_shutdown.clear();
    let remove_result = match Arc::try_unwrap(reg) {
        Ok(reg) => reg
            .remove()
            .map_err(anyhow::Error::new)
            .context("failed to remove USB gadget and close FunctionFS"),
        Err(reg) => {
            drop(reg);
            Err(anyhow::anyhow!(
                "USB gadget still had an unexpected strong owner during shutdown"
            ))
        }
    };
    match remove_result {
        Ok(()) => info!("USB gadget removed and FunctionFS teardown completed"),
        Err(err) => {
            tracing::error!("{:#}", err);
            if lifecycle_error.is_none() {
                lifecycle_error = Some(err);
            }
        }
    }

    drop(gud_data);
    drop(gud);
    info!("FunctionFS endpoint owners dropped; DRM release follows");
    drop(active);
    drop(scanout_runtime);
    if let Err(err) = scanout_manager.release_all(&mut backend) {
        tracing::error!("Failed to explicitly release DRM scanout resources: {err:#}");
        if lifecycle_error.is_none() {
            lifecycle_error = Some(err.context("explicit DRM scanout release failed"));
        }
    } else {
        info!(
            counters = ?scanout_manager.counter_snapshot(),
            "Explicit DRM framebuffer removal and dumb-buffer destruction completed"
        );
    }

    if let Some(err) = lifecycle_error {
        return Err(err);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        advertised_preferred_mode_index, classify_usb_lifecycle, compute_scaled_layout,
        conflicts_with_owned_exact_receive, derive_mode_from_native, diagnostic_pattern_color,
        dump_pixel_buffer_ppm, event_timeout_for_ready_payload, fill_diagnostic_pattern_rect,
        parse_e1_t05_inflight_deadline, parse_functionfs_read_size, parse_test_compression,
        parse_test_dynamic_mode_match, parse_test_max_buffer_size, parse_test_output_mode,
        parse_test_prearm_once_bytes, parse_test_row_crc_trace, parse_test_status_on_set_bytes,
        parse_test_status_on_set_two_transactions_bytes, read_pixel, record_host_activity,
        render_waiting_screen, row_crc32, scale_to_fit, validate_test_mode_policy,
        waiting_scene_glyph, write_pixel, BulkReceiveSession, BulkReceiveState, DisplayMode,
        GadgetShutdown, GadgetUnbind, GadgetUnbindOutcome, ModeKey, PendingPlan, PendingPlanKind,
        ProvenIdleGate, ScaledLayout, ShadowActivation, ShadowFramebuffer, ShadowRasterIdentity,
        StatusOnSetCleanupDrain, StatusOnSetCleanupObservation, StatusOnSetCleanupPhase,
        TestOutputMode, TransferFormat, UsbLifecycleInput, UsbLifecycleResult, UsbSessionState,
        BULK_RECEIVE_DEADLINE, COLOR_BLACK, COLOR_BLUE, COLOR_CYAN, COLOR_DARK_GRAY, COLOR_GREEN,
        COLOR_LIGHT_GRAY, COLOR_MAGENTA, COLOR_RED, COLOR_WHITE, COLOR_YELLOW, RGB565_GREEN,
        RGB565_WHITE, STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE,
    };
    use drm::control::ModeTypeFlags;
    use gud_gadget::{
        DisplayStateSnapshot, Event, ProtocolInvalidationReason, SetBuffer,
        GUD_DISPLAY_MODE_FLAG_PREFERRED, GUD_PIXEL_FORMAT_RGB565,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    #[test]
    fn functionfs_read_size_parser_uses_default_and_accepts_numeric_override() {
        assert_eq!(
            parse_functionfs_read_size(None).unwrap(),
            gud_gadget::DEFAULT_FUNCTIONFS_BULK_OUT_READ_SIZE
        );
        assert_eq!(
            parse_functionfs_read_size(Some(OsStr::new("16384"))).unwrap(),
            16_384
        );
        assert!(parse_functionfs_read_size(Some(OsStr::new("not-a-size"))).is_err());
    }

    #[test]
    fn e1_t05_inflight_deadline_defaults_and_bounds_test_override() {
        assert_eq!(
            parse_e1_t05_inflight_deadline(None).unwrap(),
            BULK_RECEIVE_DEADLINE
        );
        assert!(parse_e1_t05_inflight_deadline(Some(OsStr::new("1000"))).is_err());
        assert!(parse_e1_t05_inflight_deadline(Some(OsStr::new("120001"))).is_err());
        assert!(parse_e1_t05_inflight_deadline(Some(OsStr::new("not-a-number"))).is_err());
        if cfg!(debug_assertions) {
            assert_eq!(
                parse_e1_t05_inflight_deadline(Some(OsStr::new("15000"))).unwrap(),
                Duration::from_secs(15)
            );
        }
    }

    #[test]
    fn cleanup_drain_handles_initial_completion_then_trailing_status() {
        let mut drain = StatusOnSetCleanupDrain::new(1);

        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::Initial
        );

        assert!(!drain.payload_processed(Instant::now()).unwrap());
        assert!(matches!(
            drain.phase,
            StatusOnSetCleanupPhase::AwaitingTrailing { .. }
        ));
        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingAfterPayload {
                drained: 1,
                remaining: 1,
            }
        );
        assert!(!drain.is_drained());
        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingAfterPayload {
                drained: 2,
                remaining: 0,
            }
        );
        assert_eq!(drain.phase, StatusOnSetCleanupPhase::Drained);
    }

    #[test]
    fn cleanup_drain_handles_initial_trailing_then_aio_completion() {
        let mut drain = StatusOnSetCleanupDrain::new(1);

        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::Initial
        );
        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingEarly {
                drained: 1,
                remaining: 1,
            }
        );

        assert!(!drain.payload_processed(Instant::now()).unwrap());
        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingAfterPayload {
                drained: 2,
                remaining: 0,
            }
        );
        assert_eq!(drain.phase, StatusOnSetCleanupPhase::Drained);
    }

    #[test]
    fn cleanup_drain_handles_both_trailing_statuses_before_payload_processing() {
        let mut drain = StatusOnSetCleanupDrain::new(1);

        assert_eq!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::Initial
        );
        assert!(matches!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingEarly {
                drained: 1,
                remaining: 1,
            }
        ));
        assert!(matches!(
            drain.note_status(0).unwrap(),
            StatusOnSetCleanupObservation::TrailingEarly {
                drained: 2,
                remaining: 0,
            }
        ));

        assert!(drain.payload_processed(Instant::now()).unwrap());
        assert!(drain.is_drained());
    }

    #[test]
    fn cleanup_drain_missing_trailing_status_times_out_with_containment_state() {
        let mut drain = StatusOnSetCleanupDrain::new(1);
        drain.note_status(0).unwrap();
        let now = Instant::now();
        assert!(!drain.payload_processed(now).unwrap());
        assert!(
            !drain.timed_out(now + STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE - Duration::from_nanos(1))
        );
        assert!(drain.timed_out(now + STATUS_ON_SET_CLEANUP_DRAIN_DEADLINE));
        assert_eq!(drain.phase, StatusOnSetCleanupPhase::TimedOut);
    }

    #[test]
    fn transfer_format_parser_defaults_and_fails_closed() {
        assert_eq!(
            TransferFormat::from_env_value(None).unwrap(),
            TransferFormat::Rgb565
        );
        assert_eq!(
            TransferFormat::from_env_value(Some(OsStr::new("rgb565"))).unwrap(),
            TransferFormat::Rgb565
        );
        assert_eq!(
            TransferFormat::from_env_value(Some(OsStr::new("rgb888"))).unwrap(),
            TransferFormat::Rgb888
        );
        assert_eq!(
            TransferFormat::from_env_value(Some(OsStr::new("xrgb8888"))).unwrap(),
            TransferFormat::Xrgb8888
        );
        let err = TransferFormat::from_env_value(Some(OsStr::new("bgr565"))).unwrap_err();
        assert!(err.to_string().contains("bgr565"));
        assert!(err.to_string().contains("rgb565, rgb888, xrgb8888"));
    }

    #[test]
    fn transfer_format_metadata_matches_drm_and_gud() {
        assert_eq!(
            (
                TransferFormat::Rgb565.gud_pixel_format(),
                TransferFormat::Rgb565.drm_fourcc(),
                TransferFormat::Rgb565.depth(),
                TransferFormat::Rgb565.bpp(),
                TransferFormat::Rgb565.bytes_per_pixel(),
            ),
            (
                gud_gadget::GUD_PIXEL_FORMAT_RGB565,
                drm::buffer::DrmFourcc::Rgb565,
                16,
                16,
                2,
            )
        );
        assert_eq!(
            (
                TransferFormat::Rgb888.gud_pixel_format(),
                TransferFormat::Rgb888.drm_fourcc(),
                TransferFormat::Rgb888.depth(),
                TransferFormat::Rgb888.bpp(),
                TransferFormat::Rgb888.bytes_per_pixel(),
            ),
            (
                gud_gadget::GUD_PIXEL_FORMAT_RGB888,
                drm::buffer::DrmFourcc::Rgb888,
                24,
                24,
                3,
            )
        );
        assert_eq!(
            (
                TransferFormat::Xrgb8888.gud_pixel_format(),
                TransferFormat::Xrgb8888.drm_fourcc(),
                TransferFormat::Xrgb8888.depth(),
                TransferFormat::Xrgb8888.bpp(),
                TransferFormat::Xrgb8888.bytes_per_pixel(),
            ),
            (
                gud_gadget::GUD_PIXEL_FORMAT_XRGB8888,
                drm::buffer::DrmFourcc::Xrgb8888,
                24,
                32,
                4,
            )
        );
    }

    #[test]
    fn pixel_encoding_has_expected_little_endian_drm_layout() {
        let colors = [
            (COLOR_BLACK, [0, 0, 0, 0]),
            (COLOR_WHITE, [255, 255, 255, 0]),
            (COLOR_RED, [0, 0, 255, 0]),
            (COLOR_GREEN, [0, 255, 0, 0]),
            (COLOR_BLUE, [255, 0, 0, 0]),
            (COLOR_CYAN, [255, 255, 0, 0]),
            (COLOR_MAGENTA, [255, 0, 255, 0]),
            (COLOR_YELLOW, [0, 255, 255, 0]),
        ];

        for (color, xrgb_bytes) in colors {
            for format in [
                TransferFormat::Rgb565,
                TransferFormat::Rgb888,
                TransferFormat::Xrgb8888,
            ] {
                let mut fb = vec![0; format.bytes_per_pixel()];
                let pitch = fb.len();
                write_pixel(&mut fb, pitch, 0, 0, format, color).unwrap();
                match format {
                    TransferFormat::Rgb565 => {
                        let expected = ((color.r as u16 >> 3) << 11)
                            | ((color.g as u16 >> 2) << 5)
                            | (color.b as u16 >> 3);
                        assert_eq!(fb, expected.to_le_bytes());
                    }
                    TransferFormat::Rgb888 => assert_eq!(&fb, &xrgb_bytes[..3]),
                    TransferFormat::Xrgb8888 => assert_eq!(&fb, &xrgb_bytes),
                }
            }
        }
    }

    #[test]
    fn ppm_dump_decodes_all_supported_formats_in_logical_rgb_order() {
        let path = PathBuf::from(format!("/tmp/gud-drm-ppm-{}.ppm", std::process::id()));
        for format in [
            TransferFormat::Rgb565,
            TransferFormat::Rgb888,
            TransferFormat::Xrgb8888,
        ] {
            let bpp = format.bytes_per_pixel();
            let mut fb = vec![0; 2 * bpp];
            write_pixel(&mut fb, 2 * bpp, 0, 0, format, COLOR_RED).unwrap();
            write_pixel(&mut fb, 2 * bpp, 1, 0, format, COLOR_CYAN).unwrap();
            if format == TransferFormat::Xrgb8888 {
                fb[3] = 0x7f;
                fb[7] = 0xa5;
            }
            dump_pixel_buffer_ppm(&path, &fb, 2 * bpp, 2, 1, format).unwrap();
            let ppm = fs::read(&path).unwrap();
            assert_eq!(&ppm[..11], b"P6\n2 1\n255\n");
            assert_eq!(&ppm[11..14], &[255, 0, 0]);
            assert_eq!(&ppm[14..17], &[0, 255, 255]);
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_compression_parser_preserves_default_and_accepts_named_values() {
        assert_eq!(parse_test_compression(None).unwrap(), 1);
        assert_eq!(parse_test_compression(Some(OsStr::new("lz4"))).unwrap(), 1);
        assert_eq!(parse_test_compression(Some(OsStr::new("none"))).unwrap(), 0);
        assert!(parse_test_compression(Some(OsStr::new("off"))).is_err());
    }

    #[test]
    fn test_max_buffer_size_parser_accepts_positive_u32_only() {
        assert_eq!(parse_test_max_buffer_size(None).unwrap(), None);
        assert_eq!(
            parse_test_max_buffer_size(Some(OsStr::new("64000"))).unwrap(),
            Some(64_000)
        );
        assert!(parse_test_max_buffer_size(Some(OsStr::new("0"))).is_err());
        assert!(parse_test_max_buffer_size(Some(OsStr::new("-1"))).is_err());
        assert!(parse_test_max_buffer_size(Some(OsStr::new("not-a-size"))).is_err());
    }

    #[test]
    fn test_prearm_once_parser_accepts_only_safe_functionfs_read_sizes() {
        assert_eq!(parse_test_prearm_once_bytes(None).unwrap(), None);
        assert_eq!(
            parse_test_prearm_once_bytes(Some(OsStr::new("12800"))).unwrap(),
            Some(12_800)
        );
        for value in ["0", "4095", "12801", "66048", "not-a-size"] {
            assert!(
                parse_test_prearm_once_bytes(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn status_on_set_diagnostic_parser_accepts_only_exact_qualified_size() {
        assert_eq!(parse_test_status_on_set_bytes(None).unwrap(), None);
        assert_eq!(
            parse_test_status_on_set_bytes(Some(OsStr::new("12800"))).unwrap(),
            Some(12_800)
        );
        for value in ["0", "12799", "12801", "65536", "not-a-size"] {
            assert!(
                parse_test_status_on_set_bytes(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn two_transaction_status_on_set_parser_accepts_only_exact_qualified_size() {
        assert_eq!(
            parse_test_status_on_set_two_transactions_bytes(Some(OsStr::new("12800"))).unwrap(),
            Some(12_800)
        );
        for value in ["0", "12799", "12801", "65536", "not-a-size"] {
            assert!(
                parse_test_status_on_set_two_transactions_bytes(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn dynamic_mode_match_parser_accepts_exact_test_value_only() {
        assert!(!parse_test_dynamic_mode_match(None).unwrap());
        assert!(parse_test_dynamic_mode_match(Some(OsStr::new("1"))).unwrap());
        for value in ["", "0", "true", "yes", "01", "1\n"] {
            assert!(
                parse_test_dynamic_mode_match(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn row_crc_trace_parser_and_crc_are_deterministic() {
        assert!(!parse_test_row_crc_trace(None).unwrap());
        assert!(parse_test_row_crc_trace(Some(OsStr::new("1"))).unwrap());
        assert!(parse_test_row_crc_trace(Some(OsStr::new("0"))).is_err());
        assert_eq!(row_crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn test_output_mode_parser_preserves_default_and_accepts_exact_dimensions() {
        assert_eq!(parse_test_output_mode(None).unwrap(), None);
        assert_eq!(
            parse_test_output_mode(Some(OsStr::new("1280x720"))).unwrap(),
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        );
    }

    #[test]
    fn test_output_mode_parser_rejects_ambiguous_or_invalid_values() {
        for value in ["1280", "1280X720", "1280x720x60", "0x720", "1280x0"] {
            assert!(
                parse_test_output_mode(Some(OsStr::new(value))).is_err(),
                "unexpectedly accepted {value:?}"
            );
        }
    }

    #[test]
    fn dynamic_and_fixed_output_test_policies_are_mutually_exclusive() {
        assert!(validate_test_mode_policy(false, None).is_ok());
        assert!(validate_test_mode_policy(
            false,
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        )
        .is_ok());
        assert!(validate_test_mode_policy(true, None).is_ok());
        assert!(validate_test_mode_policy(
            true,
            Some(TestOutputMode {
                width: 1280,
                height: 720,
            })
        )
        .is_err());
    }

    #[test]
    fn advertised_preferred_mode_falls_back_to_first_connector_mode() {
        assert_eq!(
            advertised_preferred_mode_index([
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER,
            ]),
            0
        );
    }

    #[test]
    fn advertised_preferred_mode_uses_explicit_drm_preference() {
        assert_eq!(
            advertised_preferred_mode_index([
                ModeTypeFlags::DRIVER,
                ModeTypeFlags::DRIVER | ModeTypeFlags::PREFERRED,
                ModeTypeFlags::DRIVER,
            ]),
            1
        );
    }

    #[test]
    fn failed_bulk_receive_poison_prevents_a_second_attempt() {
        let session = BulkReceiveSession::default();
        let attempts = AtomicUsize::new(0);

        let first: anyhow::Result<()> = session.receive(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("injected receive failure"))
        });
        assert!(first.is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));

        let second = session.receive(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(second.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(second.unwrap_err().to_string().contains("fresh Pi boot"));
    }

    #[test]
    fn in_flight_bulk_receive_refuses_concurrent_unbind() {
        let session = BulkReceiveSession::default();
        let worker_session = session.clone();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            worker_session.receive(|| {
                worker_entered.wait();
                worker_release.wait();
                Ok(())
            })
        });

        entered.wait();
        assert_eq!(session.current_state(), BulkReceiveState::InFlight);
        let unbind_calls = AtomicUsize::new(0);
        if session.begin_shutdown().is_ok() {
            unbind_calls.fetch_add(1, Ordering::SeqCst);
        }
        assert_eq!(unbind_calls.load(Ordering::SeqCst), 0);
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));

        release.wait();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn late_bulk_receive_is_poisoned_at_safety_threshold() {
        let session = BulkReceiveSession::with_deadline(Duration::from_millis(1));

        let result = session.receive(|| {
            std::thread::sleep(Duration::from_millis(5));
            Ok(())
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("safety deadline"));
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn processing_failure_requires_containment_and_refuses_shutdown() {
        let session = BulkReceiveSession::default();
        session.receive(|| Ok(())).unwrap();

        std::thread::sleep(Duration::from_millis(5));
        let post_read_result: anyhow::Result<()> =
            Err(anyhow::anyhow!("injected post-read failure"));

        assert!(post_read_result.is_err());
        assert_eq!(session.current_state(), BulkReceiveState::Processing);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Processing));
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn idle_lifecycle_observation_permits_cleanup() {
        let session = BulkReceiveSession::default();

        assert_eq!(session.observe_lifecycle(), Ok(()));
        assert_eq!(session.current_state(), BulkReceiveState::Idle);
        assert!(session.begin_shutdown().is_ok());
    }

    #[test]
    fn arming_lifecycle_observation_is_ambiguous_and_contained() {
        let session = BulkReceiveSession::default();
        session.begin_receive().unwrap();

        assert_eq!(session.observe_lifecycle(), Err(BulkReceiveState::Arming));
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));
    }

    #[test]
    fn processing_lifecycle_observation_is_contained() {
        let session = BulkReceiveSession::default();
        session.receive(|| Ok(())).unwrap();
        assert_eq!(session.current_state(), BulkReceiveState::Processing);

        assert_eq!(
            session.observe_lifecycle(),
            Err(BulkReceiveState::Processing)
        );
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::Poisoned));
    }

    #[test]
    fn idle_bulk_session_can_be_claimed_for_shutdown_once() {
        let session = BulkReceiveSession::default();

        assert!(session.begin_shutdown().is_ok());
        assert_eq!(session.current_state(), BulkReceiveState::ShuttingDown);
        assert_eq!(
            session.begin_shutdown(),
            Err(BulkReceiveState::ShuttingDown)
        );
    }

    #[test]
    fn ready_payload_polls_ep0_before_processing() {
        assert_eq!(
            event_timeout_for_ready_payload(Duration::from_millis(100), true),
            Duration::ZERO
        );
        assert_eq!(
            event_timeout_for_ready_payload(Duration::from_millis(100), false),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn owned_exact_receive_allows_state_check_but_not_another_buffer() {
        let state_check = Event::StateChecked(DisplayStateSnapshot {
            mode: DisplayMode {
                clock: 74_250,
                hdisplay: 1280,
                hsync_start: 1390,
                hsync_end: 1430,
                htotal: 1650,
                vdisplay: 720,
                vsync_start: 725,
                vsync_end: 730,
                vtotal: 750,
                flags: 5,
            },
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
            generation: 23,
        });
        let buffer = Event::Buffer(SetBuffer {
            x: 0,
            y: 0,
            width: 1280,
            height: 1,
            length: 5120,
            compression: 0,
            compressed_length: 0,
        });

        assert!(!conflicts_with_owned_exact_receive(&state_check, false));
        assert!(conflicts_with_owned_exact_receive(&buffer, false));
        assert!(!conflicts_with_owned_exact_receive(&buffer, true));
    }

    #[test]
    fn proven_idle_gate_rejects_each_non_idle_component() {
        let idle = ProvenIdleGate {
            aggregate_idle: true,
            exact_aio_idle: true,
            accepted_operation_absent: true,
            aio_queue_empty: true,
            pending_exact_receive_absent: true,
            ready_exact_payload_absent: true,
            cleanup_drain_absent: true,
            diagnostic_guard_inactive: true,
            prearmed_read_inactive: true,
        };
        assert!(idle.is_proven_idle());
        for non_idle in [
            ProvenIdleGate {
                aggregate_idle: false,
                ..idle
            },
            ProvenIdleGate {
                exact_aio_idle: false,
                ..idle
            },
            ProvenIdleGate {
                accepted_operation_absent: false,
                ..idle
            },
            ProvenIdleGate {
                aio_queue_empty: false,
                ..idle
            },
            ProvenIdleGate {
                pending_exact_receive_absent: false,
                ..idle
            },
            ProvenIdleGate {
                ready_exact_payload_absent: false,
                ..idle
            },
            ProvenIdleGate {
                cleanup_drain_absent: false,
                ..idle
            },
            ProvenIdleGate {
                diagnostic_guard_inactive: false,
                ..idle
            },
            ProvenIdleGate {
                prearmed_read_inactive: false,
                ..idle
            },
        ] {
            assert!(!non_idle.is_proven_idle());
        }
    }

    #[test]
    fn persistent_usb_session_transitions_require_proven_idle() {
        let mut state = UsbSessionState::WaitingForHost;
        for (input, expected) in [
            (UsbLifecycleInput::Bind, UsbSessionState::WaitingForHost),
            (UsbLifecycleInput::Enable, UsbSessionState::Active),
            (UsbLifecycleInput::Enable, UsbSessionState::Active),
            (UsbLifecycleInput::Suspend, UsbSessionState::Suspended),
            (UsbLifecycleInput::Resume, UsbSessionState::Active),
            (UsbLifecycleInput::Disable, UsbSessionState::WaitingForHost),
            (UsbLifecycleInput::Enable, UsbSessionState::Active),
        ] {
            assert_eq!(
                classify_usb_lifecycle(state, input, true),
                UsbLifecycleResult::Continue(expected)
            );
            state = expected;
        }
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Active, UsbLifecycleInput::Suspend, false),
            UsbLifecycleResult::PreserveOwned(UsbSessionState::Suspended)
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Suspended, UsbLifecycleInput::Resume, false),
            UsbLifecycleResult::PreserveOwned(UsbSessionState::Active)
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Active, UsbLifecycleInput::Disable, false),
            UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Contained, UsbLifecycleInput::Enable, false),
            UsbLifecycleResult::SessionBoundary(UsbSessionState::Active)
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Contained, UsbLifecycleInput::Bind, false),
            UsbLifecycleResult::SessionBoundary(UsbSessionState::WaitingForHost)
        );
        assert_eq!(
            classify_usb_lifecycle(
                UsbSessionState::Active,
                UsbLifecycleInput::ProtocolInvalidation,
                false
            ),
            UsbLifecycleResult::Contained
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Active, UsbLifecycleInput::Unbind, true),
            UsbLifecycleResult::FatalUnbind
        );
        assert_eq!(
            classify_usb_lifecycle(UsbSessionState::Contained, UsbLifecycleInput::Enable, true),
            UsbLifecycleResult::SessionBoundary(UsbSessionState::Active)
        );
    }

    #[test]
    fn destroyed_outer_epoch_is_historical_and_fresh_epoch_starts_unowned() {
        let session = BulkReceiveSession::default();
        session.begin_usb_epoch(1).unwrap();
        session.begin_receive().unwrap();
        session.submission_accepted().unwrap();
        session.poison();

        assert_eq!(session.retire_after_session_destruction(), Ok(1));
        assert_eq!(
            session.current_state(),
            BulkReceiveState::AbortedBySessionDestruction
        );
        assert!(!session.currently_owned());
        assert!(session.begin_receive().is_err());

        session.begin_usb_epoch(2).unwrap();
        assert_eq!(session.epoch_id(), 2);
        assert_eq!(session.current_state(), BulkReceiveState::Idle);
        assert!(!session.currently_owned());
    }

    #[test]
    fn live_ambiguous_outer_epoch_still_blocks_shutdown() {
        let session = BulkReceiveSession::default();
        session.begin_usb_epoch(1).unwrap();
        session.begin_receive().unwrap();
        session.submission_accepted().unwrap();
        assert_eq!(session.begin_shutdown(), Err(BulkReceiveState::InFlight));
        assert_eq!(session.current_state(), BulkReceiveState::Poisoned);
    }

    #[test]
    fn shutdown_after_destroyed_outer_epoch_is_bounded() {
        let session = BulkReceiveSession::default();
        session.begin_usb_epoch(1).unwrap();
        session.begin_receive().unwrap();
        session.submission_accepted().unwrap();
        session.poison();
        session.retire_after_session_destruction().unwrap();
        assert_eq!(session.begin_shutdown(), Ok(()));
        assert_eq!(session.current_state(), BulkReceiveState::ShuttingDown);
    }

    #[test]
    fn lifecycle_only_events_do_not_create_a_host_session() {
        let mut had_host_session = false;
        for reason in [
            ProtocolInvalidationReason::Bind,
            ProtocolInvalidationReason::Enable,
            ProtocolInvalidationReason::Suspend,
            ProtocolInvalidationReason::Resume,
        ] {
            record_host_activity(
                &mut had_host_session,
                &Event::ProtocolStateInvalidated {
                    generation: None,
                    reason,
                },
            );
        }
        assert!(!had_host_session);

        record_host_activity(
            &mut had_host_session,
            &Event::StateChecked(DisplayStateSnapshot {
                mode: native_mode(),
                format: GUD_PIXEL_FORMAT_RGB565,
                connector: 0,
                generation: 1,
            }),
        );
        assert!(had_host_session);
    }

    #[derive(Default)]
    struct MockGadget {
        unbind_calls: AtomicUsize,
    }

    impl GadgetUnbind for MockGadget {
        fn unbind(&self) -> io::Result<()> {
            self.unbind_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailOnceGadget {
        unbind_calls: AtomicUsize,
    }

    impl GadgetUnbind for FailOnceGadget {
        fn unbind(&self) -> io::Result<()> {
            if self.unbind_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(io::Error::other("injected first unbind failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn shutdown_unbinds_published_gadget_once() {
        let shutdown = GadgetShutdown::default();
        let gadget = Arc::new(MockGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();

        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Armed
        );
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::AlreadyUnbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_unbind_remains_retryable() {
        let shutdown = GadgetShutdown::default();
        let gadget = Arc::new(FailOnceGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();

        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Armed
        );
        assert!(shutdown.request_unbind().is_err());
        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn shutdown_before_publish_unbinds_as_soon_as_gadget_is_available() {
        let shutdown = GadgetShutdown::default();

        assert_eq!(
            shutdown.request_unbind().unwrap(),
            GadgetUnbindOutcome::DeferredUntilPublish
        );

        let gadget = Arc::new(MockGadget::default());
        let target: Arc<dyn GadgetUnbind> = gadget.clone();
        assert_eq!(
            shutdown.publish(&target).unwrap(),
            GadgetUnbindOutcome::Unbound
        );
        assert_eq!(gadget.unbind_calls.load(Ordering::SeqCst), 1);
    }

    fn native_mode() -> DisplayMode {
        DisplayMode {
            clock: 174_359,
            hdisplay: 1080,
            hsync_start: 1192,
            hsync_end: 1208,
            htotal: 1244,
            vdisplay: 2280,
            vsync_start: 2316,
            vsync_end: 2324,
            vtotal: 2336,
            flags: GUD_DISPLAY_MODE_FLAG_PREFERRED,
        }
    }

    #[test]
    fn compute_scaled_layout_preserves_aspect_ratio_for_4_3_mode() {
        assert_eq!(
            compute_scaled_layout(1024, 768, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 0,
                dst_y: 735,
                dst_width: 1080,
                dst_height: 810,
            }
        );
    }

    #[test]
    fn diagnostic_pattern_is_equivalent_and_partial_updates_stay_in_rect() {
        let width = 400usize;
        let height = 300usize;
        let mut rgb565 = vec![0xaa; width * height * 2];
        let mut xrgb = vec![0xaa; width * height * 4];
        fill_diagnostic_pattern_rect(
            &mut rgb565,
            width * 2,
            width as u32,
            height as u32,
            0,
            0,
            width as u32,
            height as u32,
            TransferFormat::Rgb565,
        )
        .unwrap();
        fill_diagnostic_pattern_rect(
            &mut xrgb,
            width * 4,
            width as u32,
            height as u32,
            0,
            0,
            width as u32,
            height as u32,
            TransferFormat::Xrgb8888,
        )
        .unwrap();

        for (x, y, expected) in [
            (180, 100, COLOR_LIGHT_GRAY),
            (50, 50, COLOR_WHITE),
            (350, 50, COLOR_CYAN),
            (50, 250, COLOR_MAGENTA),
            (350, 250, super::COLOR_ORANGE),
            (160, 100, COLOR_DARK_GRAY),
            (200, 150, COLOR_MAGENTA),
        ] {
            assert_eq!(
                read_pixel(&xrgb, width * 4, x, y, TransferFormat::Xrgb8888).unwrap(),
                expected
            );
            let decoded = read_pixel(&rgb565, width * 2, x, y, TransferFormat::Rgb565).unwrap();
            assert!((decoded.r as i16 - expected.r as i16).abs() <= 7);
            assert!((decoded.g as i16 - expected.g as i16).abs() <= 3);
            assert!((decoded.b as i16 - expected.b as i16).abs() <= 7);
        }

        let mut partial = vec![0x5a; width * height * 4];
        fill_diagnostic_pattern_rect(
            &mut partial,
            width * 4,
            width as u32,
            height as u32,
            100,
            100,
            50,
            50,
            TransferFormat::Xrgb8888,
        )
        .unwrap();
        assert!(partial[..100 * width * 4].iter().all(|byte| *byte == 0x5a));
        assert_eq!(
            read_pixel(&partial, width * 4, 120, 120, TransferFormat::Xrgb8888).unwrap(),
            diagnostic_pattern_color(120, 120, width, height)
        );
    }

    #[test]
    fn scale_to_fit_copies_pixels_and_preserves_padding_for_supported_formats() {
        for format in [TransferFormat::Rgb565, TransferFormat::Xrgb8888] {
            let bpp = format.bytes_per_pixel();
            let src_pitch = 2 * bpp + 3;
            let mut src = vec![0xee; src_pitch * 2];
            write_pixel(&mut src, src_pitch, 0, 0, format, COLOR_RED).unwrap();
            write_pixel(&mut src, src_pitch, 1, 0, format, COLOR_GREEN).unwrap();
            write_pixel(&mut src, src_pitch, 0, 1, format, COLOR_BLUE).unwrap();
            write_pixel(&mut src, src_pitch, 1, 1, format, COLOR_WHITE).unwrap();
            let dst_pitch = 4 * bpp + 5;
            let mut dst = vec![0xcc; dst_pitch * 4];
            let (layout, _) =
                scale_to_fit(&src, src_pitch, 2, 2, &mut dst, dst_pitch, 4, 4, format).unwrap();
            assert_eq!(
                layout,
                ScaledLayout {
                    dst_x: 0,
                    dst_y: 0,
                    dst_width: 4,
                    dst_height: 4
                }
            );
            assert_eq!(
                read_pixel(&dst, dst_pitch, 0, 0, format).unwrap(),
                COLOR_RED
            );
            assert_eq!(
                read_pixel(&dst, dst_pitch, 3, 0, format).unwrap(),
                COLOR_GREEN
            );
            assert_eq!(
                read_pixel(&dst, dst_pitch, 0, 3, format).unwrap(),
                COLOR_BLUE
            );
            assert_eq!(
                read_pixel(&dst, dst_pitch, 3, 3, format).unwrap(),
                COLOR_WHITE
            );
            assert!(dst
                .chunks_exact(dst_pitch)
                .all(|row| row[4 * bpp..].iter().all(|byte| *byte == 0xcc)));
        }
    }

    #[test]
    fn scale_to_fit_centers_and_letterboxes_and_rejects_invalid_rasters() {
        let mut src = vec![0; 4 * 2];
        write_pixel(&mut src, 4, 0, 0, TransferFormat::Rgb565, COLOR_RED).unwrap();
        write_pixel(&mut src, 4, 1, 0, TransferFormat::Rgb565, COLOR_GREEN).unwrap();
        let mut dst = vec![0xff; 4 * 4 * 2];
        let (layout, _) =
            scale_to_fit(&src, 4, 2, 1, &mut dst, 8, 4, 4, TransferFormat::Rgb565).unwrap();
        assert_eq!(
            layout,
            ScaledLayout {
                dst_x: 0,
                dst_y: 1,
                dst_width: 4,
                dst_height: 2
            }
        );
        assert_eq!(
            read_pixel(&dst, 8, 0, 0, TransferFormat::Rgb565).unwrap(),
            COLOR_BLACK
        );
        assert_eq!(
            read_pixel(&dst, 8, 0, 1, TransferFormat::Rgb565).unwrap(),
            COLOR_RED
        );
        assert!(scale_to_fit(
            &src[..3],
            4,
            2,
            1,
            &mut dst,
            8,
            4,
            4,
            TransferFormat::Rgb565
        )
        .is_err());
        assert!(scale_to_fit(&src, 3, 2, 1, &mut dst, 8, 4, 4, TransferFormat::Rgb565).is_err());
        assert!(scale_to_fit(
            &src,
            4,
            2,
            1,
            &mut dst[..7],
            8,
            4,
            4,
            TransferFormat::Rgb565
        )
        .is_err());
        assert!(scale_to_fit(&src, 4, 2, 1, &mut dst, 7, 4, 4, TransferFormat::Rgb565).is_err());
    }

    #[test]
    fn compute_scaled_layout_keeps_native_mode_fullscreen() {
        assert_eq!(
            compute_scaled_layout(1080, 2280, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 0,
                dst_y: 0,
                dst_width: 1080,
                dst_height: 2280,
            }
        );
    }

    #[test]
    fn compute_scaled_layout_centers_tall_source() {
        assert_eq!(
            compute_scaled_layout(720, 2280, 1080, 2280).unwrap(),
            ScaledLayout {
                dst_x: 180,
                dst_y: 0,
                dst_width: 720,
                dst_height: 2280,
            }
        );
    }

    #[test]
    fn derive_mode_from_native_preserves_portrait_shape() {
        let derived = derive_mode_from_native(&native_mode(), 720, 1520);

        assert_eq!(derived.hdisplay, 720);
        assert_eq!(derived.vdisplay, 1520);
        assert!(derived.hsync_start > derived.hdisplay);
        assert!(derived.hsync_end > derived.hsync_start);
        assert!(derived.htotal > derived.hsync_end);
        assert!(derived.vsync_start > derived.vdisplay);
        assert!(derived.vsync_end > derived.vsync_start);
        assert!(derived.vtotal > derived.vsync_end);
        assert_eq!(derived.flags, 0);
    }

    #[test]
    fn preallocated_shadow_preserves_identity_and_zeros_only_when_required() {
        let mut shadow = ShadowFramebuffer::preallocated(32).unwrap();
        let identity = ShadowRasterIdentity {
            width: 4,
            height: 4,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Zeroed
        );
        shadow.pixels[0..4].copy_from_slice(&[1, 2, 3, 4]);

        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Preserved
        );
        assert_eq!(&shadow.pixels[0..4], &[1, 2, 3, 4]);

        shadow.invalidate_content();
        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Zeroed
        );
        assert_eq!(&shadow.pixels[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn same_geometry_timing_change_preserves_partial_shadow_without_allocation() {
        let mut shadow = ShadowFramebuffer::preallocated(64).unwrap();
        let identity = ShadowRasterIdentity {
            width: 4,
            height: 4,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        shadow.activate_scaled(identity).unwrap();
        shadow.pixels[6..10].copy_from_slice(&[9, 8, 7, 6]);
        let pointer = shadow.pixels.as_ptr();
        let capacity = shadow.pixels.capacity();

        // Timing is intentionally absent from raster identity: a same-size,
        // different-timing scaled commit must retain partial source pixels.
        assert_eq!(
            shadow.activate_scaled(identity).unwrap(),
            ShadowActivation::Preserved
        );
        assert_eq!(&shadow.pixels[6..10], &[9, 8, 7, 6]);
        assert_eq!(shadow.pixels.as_ptr(), pointer);
        assert_eq!(shadow.pixels.capacity(), capacity);
    }

    #[test]
    fn commit_plan_matching_consumes_missing_stale_and_mismatched_identity() {
        let mode = DisplayMode {
            clock: 74_250,
            hdisplay: 1280,
            hsync_start: 1390,
            hsync_end: 1430,
            htotal: 1650,
            vdisplay: 720,
            vsync_start: 725,
            vsync_end: 730,
            vtotal: 750,
            flags: 0,
        };
        let committed = DisplayStateSnapshot {
            mode: mode.clone(),
            format: GUD_PIXEL_FORMAT_RGB565,
            connector: 0,
            generation: 4,
        };
        let make_plan = |snapshot: DisplayStateSnapshot| PendingPlan {
            logical_key: ModeKey::from_snapshot(&snapshot),
            snapshot,
            requested_physical_key: None,
            requested_mode: None::<()>,
            kind: PendingPlanKind::ExactActiveNoOp,
            failed_cache_hit: false,
            mode_prepare_ms: 0,
        };

        let mut missing: Option<PendingPlan<()>> = None;
        assert_eq!(
            super::take_matching_pending_plan(&mut missing, &committed),
            Err(false)
        );

        let mut stale_snapshot = committed.clone();
        stale_snapshot.generation -= 1;
        let mut stale = Some(make_plan(stale_snapshot));
        assert_eq!(
            super::take_matching_pending_plan(&mut stale, &committed),
            Err(true)
        );
        assert!(stale.is_none());

        let mut mismatched_snapshot = committed.clone();
        mismatched_snapshot.mode.clock += 1;
        let mut mismatched = Some(make_plan(mismatched_snapshot));
        assert_eq!(
            super::take_matching_pending_plan(&mut mismatched, &committed),
            Err(true)
        );
        assert!(mismatched.is_none());

        let mut matching = Some(make_plan(committed.clone()));
        assert!(super::take_matching_pending_plan(&mut matching, &committed).is_ok());
        assert!(matching.is_none());
    }

    #[test]
    fn shadow_format_or_geometry_change_zeros_once_without_reallocation() {
        let mut shadow = ShadowFramebuffer::preallocated(64).unwrap();
        let original_ptr = shadow.pixels.as_ptr();
        let rgb565 = ShadowRasterIdentity {
            width: 4,
            height: 4,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        shadow.activate_scaled(rgb565).unwrap();
        shadow.pixels[0] = 0xff;

        let format_change = ShadowRasterIdentity {
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB888,
            ..rgb565
        };
        assert_eq!(
            shadow.activate_scaled(format_change).unwrap(),
            ShadowActivation::Zeroed
        );
        shadow.pixels[0] = 0xaa;

        let geometry_change = ShadowRasterIdentity {
            width: 2,
            height: 2,
            format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
        };
        assert_eq!(
            shadow.activate_scaled(geometry_change).unwrap(),
            ShadowActivation::Zeroed
        );
        assert_eq!(shadow.pixels[0], 0);
        assert_eq!(shadow.pixels.as_ptr(), original_ptr);
    }

    #[test]
    fn maximum_shadow_allocation_and_capacity_fail_before_use() {
        assert!(ShadowFramebuffer::preallocated(usize::MAX).is_err());
        let mut shadow = ShadowFramebuffer::preallocated(8).unwrap();
        assert!(shadow
            .activate_scaled(ShadowRasterIdentity {
                width: 3,
                height: 2,
                format: gud_gadget::GUD_PIXEL_FORMAT_RGB565,
            })
            .is_err());
    }

    #[test]
    fn memory_accounting_and_shadow_capacity_cover_format_sizes() {
        let dimensions = [(1280, 720)];
        let rgb565 = crate::scanout::logical_memory_estimate(dimensions, dimensions, 2).unwrap();
        let rgb888 = crate::scanout::logical_memory_estimate(dimensions, dimensions, 3).unwrap();
        let xrgb8888 = crate::scanout::logical_memory_estimate(dimensions, dimensions, 4).unwrap();
        assert_eq!(rgb888.max_shadow_bytes * 2, rgb565.max_shadow_bytes * 3);
        assert_eq!(xrgb8888.max_shadow_bytes, rgb565.max_shadow_bytes * 2);

        let mut adequate = ShadowFramebuffer::preallocated(1280 * 720 * 4).unwrap();
        assert!(adequate
            .activate_scaled(ShadowRasterIdentity {
                width: 1280,
                height: 720,
                format: gud_gadget::GUD_PIXEL_FORMAT_XRGB8888,
            })
            .is_ok());
        let mut insufficient = ShadowFramebuffer::preallocated(1280 * 720 * 4 - 1).unwrap();
        assert!(insufficient
            .activate_scaled(ShadowRasterIdentity {
                width: 1280,
                height: 720,
                format: gud_gadget::GUD_PIXEL_FORMAT_XRGB8888,
            })
            .is_err());
    }

    #[test]
    fn waiting_scene_supports_all_glyphs() {
        for line in super::WAITING_SCREEN_LINES {
            for ch in line.chars() {
                assert!(
                    waiting_scene_glyph(ch).is_some(),
                    "missing glyph for {:?}",
                    ch
                );
            }
        }
    }

    #[test]
    fn waiting_screen_contains_black_white_and_green_pixels() {
        let width = 320usize;
        let height = 240usize;
        let pitch = width * 2;
        let mut fb = vec![0u8; pitch * height];

        render_waiting_screen(
            &mut fb,
            pitch,
            width as u32,
            height as u32,
            TransferFormat::Rgb565,
        )
        .unwrap();

        let mut has_black = false;
        let mut has_white = false;
        let mut has_green = false;
        for chunk in fb.chunks_exact(2) {
            let pixel = u16::from_le_bytes([chunk[0], chunk[1]]);
            has_black |= pixel == 0;
            has_white |= pixel == RGB565_WHITE;
            has_green |= pixel == RGB565_GREEN;
        }

        assert!(has_black);
        assert!(has_white);
        assert!(has_green);
    }

    #[test]
    fn xrgb_waiting_screen_is_completely_black_and_bounded() {
        let width = 3usize;
        let height = 2usize;
        let pitch = width * 4 + 3;
        let mut fb = vec![0xa5; pitch * height];
        render_waiting_screen(
            &mut fb,
            pitch,
            width as u32,
            height as u32,
            TransferFormat::Xrgb8888,
        )
        .unwrap();
        for row in fb.chunks_exact(pitch) {
            assert!(row[..width * 4].iter().all(|byte| *byte == 0));
            assert!(row[width * 4..].iter().all(|byte| *byte == 0xa5));
        }
    }

    #[test]
    fn copy_path_accepts_all_supported_pixel_widths_and_rejects_short_destination() {
        let info = SetBuffer {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
            length: 0,
            compression: 0,
            compressed_length: 0,
        };
        for bpp in [2, 3, 4] {
            let src = (0..2 * bpp).map(|value| value as u8).collect::<Vec<_>>();
            let mut dst = vec![0; 4 * bpp * 2];
            gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer(
                &info,
                &src,
                &mut dst,
                4 * bpp,
                bpp,
            )
            .unwrap();
            assert_eq!(&dst[5 * bpp..7 * bpp], &src);
            assert!(gud_gadget::PixelDataEndpoint::copy_buffer_to_framebuffer(
                &info,
                &src,
                &mut dst[..4 * bpp],
                4 * bpp,
                bpp,
            )
            .is_err());
        }
    }
}

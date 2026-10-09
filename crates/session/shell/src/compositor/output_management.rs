//! `zwlr_output_manager_v1` and `zwlr_output_power_manager_v1`: display
//! configuration and panel power for external tools (`wlr-randr`,
//! `kanshi`, `way-displays`, `wlopm`).
//!
//! Both globals change what every display shows, so both are visible only
//! to connections accepted on the compositor's privileged socket — the
//! same gate as the clipboard-manager and session-lock protocols. The
//! sandbox launcher never bind-mounts that socket, so an app cannot
//! reconfigure or blank the screens.
//!
//! A tool's `apply` does not touch KMS directly. It becomes a
//! [`DisplayTransaction`] — exactly what the Settings display pane writes —
//! so the DRM backend validates it against the connectors it owns before
//! applying it. `succeeded` is sent once the backend has applied the plan,
//! `failed` when it rejects it.
//!
//! Like every other compositor, Tilo keeps a tool's change as soon as it is
//! applied — kanshi reacting to a dock is not something to confirm. Only a
//! change that could leave the person unable to see or reach their screen
//! ([`assess_change`]) waits for them to keep it, and is restored after
//! fifteen seconds if they do not (see
//! [`OutputManagementState::take_confirmation`]). A tool they chose to
//! trust — keyed by the executable behind the connection — skips even that,
//! unless the change would leave no display on.
//!
//! A tool that answers a revert by applying the same layout again is
//! refused for a while rather than asked again, so a revert cannot turn
//! into a loop of prompts.

use settings_store::ConditionalUpdate;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{
    DisplayConfig, DisplayProfiles, DisplaySet, DisplayTransaction, DisplayTransactionState,
    OutputMode, OutputTransform,
};
use smithay::reexports::wayland_protocols_wlr::output_management::v1::server::{
    zwlr_output_configuration_head_v1::{self, ZwlrOutputConfigurationHeadV1},
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};
use smithay::reexports::wayland_protocols_wlr::output_power_management::v1::server::{
    zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
    zwlr_output_power_v1::{self, ZwlrOutputPowerV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};

use super::backend::OutputId;
use super::output_heads::{
    ArrangementError, ChangeContext, ChangeRisk, HeadChanges, HeadRequest, ModeRequest, OutputHead,
    assess_change, leaves_a_display_on, plan_arrangement,
};
use super::state::State;

const MANAGER_VERSION: u32 = 4;
const MODE_VERSION: u32 = 3;
const POWER_MANAGER_VERSION: u32 = 1;
const MAX_PENDING_DISPLAY_REQUESTS: usize = 32;

/// How long after the person reverted a tool's layout the same layout from
/// the same tool is refused instead of asked about again.
pub const REAPPLY_AFTER_REVERT: Duration = Duration::from_secs(60);

/// The head a `zwlr_output_head_v1` stands for.
#[derive(Debug)]
pub struct HeadData {
    key: String,
}

/// The mode a `zwlr_output_mode_v1` stands for, and whose head it is.
#[derive(Debug)]
pub struct ModeData {
    key: String,
    mode: OutputMode,
}

/// Requests collected on one `zwlr_output_configuration_v1`.
#[derive(Debug, Default)]
struct PendingConfiguration {
    heads: BTreeMap<String, HeadRequest>,
    /// `apply` or `test` was sent; the object accepts nothing further.
    used: bool,
    /// A head asked for something valid in the protocol that Tilo cannot
    /// scan out (a flipped transform); the configuration will fail.
    unsupported: bool,
}

#[derive(Debug)]
pub struct ConfigurationData {
    serial: u32,
    pending: Arc<Mutex<PendingConfiguration>>,
}

#[derive(Debug)]
pub struct ConfigurationHeadData {
    key: String,
    pending: Arc<Mutex<PendingConfiguration>>,
}

/// The output a `zwlr_output_power_v1` controls; `None` when the requested
/// `wl_output` was not a live compositor output (the object is failed).
#[derive(Debug)]
pub struct PowerData {
    output: Option<OutputId>,
}

struct BoundHead {
    key: String,
    resource: ZwlrOutputHeadV1,
    modes: Vec<(OutputMode, ZwlrOutputModeV1)>,
    sent: OutputHead,
}

struct BoundManager {
    manager: ZwlrOutputManagerV1,
    heads: Vec<BoundHead>,
}

/// A configuration whose plan was staged and is waiting on the backend.
struct PendingStaging {
    configuration: ZwlrOutputConfigurationV1,
    result: mpsc::Receiver<std::io::Result<DisplayTransaction>>,
    tool: Option<DisplayTool>,
    risk: ChangeRisk,
}

struct PendingApply {
    configuration: ZwlrOutputConfigurationV1,
    transaction: u64,
    tool: Option<DisplayTool>,
    risk: ChangeRisk,
}

struct PendingKeep {
    result: crate::shell::display_persistence::PendingAnswer,
    confirmation: DisplayConfirmation,
    asked: AskedChange,
}

/// A tool's change the person was asked to keep.
struct AskedChange {
    transaction: u64,
    tool: Option<DisplayTool>,
    set: String,
    plan: DisplayConfig,
}

/// A tool's change the person reverted, and whether they were already told
/// the tool tried it again.
struct RevertedChange {
    tool: Option<DisplayTool>,
    set: String,
    plan: DisplayConfig,
    at: Instant,
    told: bool,
}

/// Something about a display tool the shell should tell the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayToolNotice {
    /// The tool applied a layout again right after the person reverted it,
    /// and was refused.
    ReappliedAfterRevert { tool: Option<DisplayTool> },
}

/// A risky display change from a tool that the backend applied and the
/// person has to keep or revert before the transaction's deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayConfirmation {
    pub transaction: u64,
    /// The program that asked for it, when its connection revealed one.
    pub tool: Option<DisplayTool>,
}

/// Server-side state of both protocols.
pub struct OutputManagementState {
    manager_global: GlobalId,
    power_global: GlobalId,
    heads: Vec<OutputHead>,
    /// Bumped on every change to [`Self::heads`]; a configuration created
    /// against an older serial is cancelled rather than applied.
    serial: u32,
    managers: Vec<BoundManager>,
    applies: Vec<PendingApply>,
    staging: Vec<PendingStaging>,
    keeps: Vec<PendingKeep>,
    wake: Option<crate::session_runtime::Wake>,
    powers: Vec<(ZwlrOutputPowerV1, OutputId)>,
    /// Outputs a client has switched off. Absent means on.
    powered_off: BTreeSet<OutputId>,
    /// Session idle blanking overrides individual output intent.
    session_blanked: bool,
    power_requests: Vec<(OutputId, bool)>,
    confirmation: Option<DisplayConfirmation>,
    asked: Option<AskedChange>,
    reverted: Option<RevertedChange>,
    notices: Vec<DisplayToolNotice>,
}

impl OutputManagementState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            manager_global: display
                .create_global::<State, ZwlrOutputManagerV1, _>(MANAGER_VERSION, ()),
            power_global: display
                .create_global::<State, ZwlrOutputPowerManagerV1, _>(POWER_MANAGER_VERSION, ()),
            heads: Vec::new(),
            serial: 1,
            managers: Vec::new(),
            applies: Vec::new(),
            staging: Vec::new(),
            keeps: Vec::new(),
            wake: None,
            powers: Vec::new(),
            powered_off: BTreeSet::new(),
            session_blanked: false,
            power_requests: Vec::new(),
            confirmation: None,
            asked: None,
            reverted: None,
            notices: Vec::new(),
        }
    }

    /// Reuse the session loop's existing mailbox wake for storage results.
    pub fn set_wake(&mut self, wake: crate::session_runtime::Wake) {
        self.wake = Some(wake);
    }

    pub fn manager_global(&self) -> GlobalId {
        self.manager_global.clone()
    }

    pub fn power_global(&self) -> GlobalId {
        self.power_global.clone()
    }

    /// The heads currently advertised.
    pub fn heads(&self) -> &[OutputHead] {
        &self.heads
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// Replace the advertised heads, telling every bound client what
    /// changed. A no-op when nothing did, so calling this on every output
    /// reconfiguration costs no traffic.
    pub fn set_heads(&mut self, display: &DisplayHandle, heads: Vec<OutputHead>) {
        if heads == self.heads {
            return;
        }
        self.heads = heads;
        self.serial = self.serial.wrapping_add(1).max(1);
        let live: Vec<OutputId> = self.heads.iter().filter_map(|h| h.output).collect();
        self.powered_off.retain(|output| live.contains(output));
        self.power_requests
            .retain(|(output, _)| live.contains(output));
        self.powers.retain(|(power, output)| {
            let alive = live.contains(output);
            if !alive {
                power.failed();
            }
            alive
        });
        let heads = &self.heads;
        let serial = self.serial;
        for bound in &mut self.managers {
            publish(display, bound, heads, serial);
        }
    }

    /// Outputs whose power a client changed since the last call, for the
    /// backend to act on.
    pub fn take_power_requests(&mut self) -> Vec<(OutputId, bool)> {
        std::mem::take(&mut self.power_requests)
    }

    /// A tool's applied, risky display change the person must now keep or
    /// revert, once per change.
    pub fn take_confirmation(&mut self) -> Option<DisplayConfirmation> {
        self.confirmation.take()
    }

    /// What the shell should tell the person about display tools since the
    /// last call.
    pub fn take_notices(&mut self) -> Vec<DisplayToolNotice> {
        std::mem::take(&mut self.notices)
    }

    /// Drain durable storage results before reading the backend's transaction.
    /// A completion can arrive after the file-change edge; pump it every loop.
    pub fn poll_staging(&mut self) -> bool {
        let mut completed = self.poll_keeps();
        self.staging.retain(|pending| match pending.result.try_recv() {
            Ok(Ok(transaction)) => {
                self.applies.push(PendingApply {
                    configuration: pending.configuration.clone(),
                    transaction: transaction.id,
                    tool: pending.tool.clone(),
                    risk: pending.risk,
                });
                completed = true;
                false
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "output management: could not stage display configuration");
                pending.configuration.failed();
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                pending.configuration.failed();
                false
            }
        });
        completed
    }

    fn poll_keeps(&mut self) -> bool {
        let mut completed = false;
        let confirmation = &mut self.confirmation;
        let asked = &mut self.asked;
        self.keeps.retain(|pending| match pending.result.poll() {
            Ok(None) => true,
            Ok(Some(())) => { completed = true; false }
            Err(error) => {
                tracing::warn!(%error, "output management: automatic Keep failed; asking the person");
                if DisplayTransaction::load().is_some_and(|transaction| {
                    transaction.id == pending.confirmation.transaction
                        && transaction.state == DisplayTransactionState::AwaitingConfirmation
                }) {
                    *confirmation = Some(pending.confirmation.clone());
                    *asked = Some(AskedChange {
                        transaction: pending.asked.transaction,
                        tool: pending.asked.tool.clone(),
                        set: pending.asked.set.clone(),
                        plan: pending.asked.plan.clone(),
                    });
                }
                completed = true;
                false
            }
        });
        completed
    }

    pub fn has_pending_apply(&self) -> bool {
        !self.applies.is_empty()
    }

    /// Report the outcome of staged configurations from the current
    /// transaction. Called after the backend reconciled a settings change.
    ///
    /// A change the backend applied is kept at once unless it is risky;
    /// a risky one is kept at once only from a trusted tool, and only when
    /// it leaves a display on. Otherwise the person is asked.
    pub fn resolve_transactions(&mut self, current: Option<&DisplayTransaction>) {
        self.note_answer(current, Instant::now());
        let heads = &self.heads;
        let confirmation = &mut self.confirmation;
        let asked = &mut self.asked;
        let keeps = &mut self.keeps;
        self.applies.retain(|apply| {
            let state = current
                .filter(|transaction| transaction.id == apply.transaction)
                .map(|transaction| transaction.state);
            match state {
                Some(DisplayTransactionState::Requested) => true,
                Some(DisplayTransactionState::AwaitingConfirmation) => {
                    apply.configuration.succeeded();
                    if let Some(transaction) = current {
                        *confirmation = keep_or_ask(transaction, apply, heads, keeps);
                        if confirmation.is_some() {
                            *asked = Some(AskedChange {
                                transaction: transaction.id,
                                tool: apply.tool.clone(),
                                set: transaction.set.key.clone(),
                                plan: transaction.candidate.clone(),
                            });
                        }
                    }
                    false
                }
                Some(
                    DisplayTransactionState::ConfirmRequested | DisplayTransactionState::Confirmed,
                ) => {
                    apply.configuration.succeeded();
                    false
                }
                // Rejected, reverted before it was shown, or replaced by a
                // newer request.
                Some(
                    DisplayTransactionState::Rejected
                    | DisplayTransactionState::RevertRequested
                    | DisplayTransactionState::Reverted,
                )
                | None => {
                    apply.configuration.failed();
                    false
                }
            }
        });
    }

    /// Follow the answer to the change the person was asked about: a
    /// revert (or letting the countdown run out) is remembered so the tool
    /// cannot put the same layout straight back.
    fn note_answer(&mut self, current: Option<&DisplayTransaction>, now: Instant) {
        let Some(asked) = self.asked.as_ref() else {
            return;
        };
        let state = current
            .filter(|transaction| transaction.id == asked.transaction)
            .map(|transaction| transaction.state);
        match state {
            Some(DisplayTransactionState::AwaitingConfirmation) => {}
            Some(DisplayTransactionState::RevertRequested | DisplayTransactionState::Reverted) => {
                if let Some(asked) = self.asked.take() {
                    tracing::info!(
                        tool = asked.tool.as_ref().map(DisplayTool::name),
                        "output management: the person reverted a tool's display change"
                    );
                    self.reverted = Some(RevertedChange {
                        tool: asked.tool,
                        set: asked.set,
                        plan: asked.plan,
                        at: now,
                        told: false,
                    });
                }
            }
            _ => self.asked = None,
        }
    }

    /// Whether `tool` applying `plan` repeats a layout the person just
    /// reverted. The first repeat queues one notice for the shell.
    fn refuses_reapply(
        &mut self,
        tool: Option<&DisplayTool>,
        plan: &DisplayConfig,
        now: Instant,
    ) -> bool {
        self.note_answer(DisplayTransaction::load().as_ref(), now);
        let set = self.display_set().key;
        let Some(reverted) = self.reverted.as_mut() else {
            return false;
        };
        if now.duration_since(reverted.at) > REAPPLY_AFTER_REVERT {
            self.reverted = None;
            return false;
        }
        if reverted.tool.as_ref() != tool || reverted.set != set || reverted.plan != *plan {
            return false;
        }
        tracing::info!(
            tool = tool.map(DisplayTool::name),
            "output management: refused a display layout the person just reverted"
        );
        if !reverted.told {
            reverted.told = true;
            self.notices.push(DisplayToolNotice::ReappliedAfterRevert {
                tool: tool.cloned(),
            });
        }
        true
    }

    /// The monitor set the heads describe; display intent is remembered
    /// per set.
    fn display_set(&self) -> DisplaySet {
        let descriptors: Vec<_> = self.heads.iter().map(|h| h.descriptor.clone()).collect();
        DisplaySet::of_descriptors(&descriptors)
    }

    /// How risky `plan` is, given which heads the person is using and what
    /// each monitor has shown before.
    fn assess(
        &self,
        plan: &DisplayConfig,
        requests: &BTreeMap<String, HeadRequest>,
        watched_outputs: &BTreeSet<OutputId>,
    ) -> ChangeRisk {
        let set = self.display_set();
        let profiles = DisplayProfiles::load();
        let watched: BTreeSet<String> = self
            .heads
            .iter()
            .filter(|head| head.output.is_some_and(|id| watched_outputs.contains(&id)))
            .map(|head| head.name().to_owned())
            .collect();
        let history = self
            .heads
            .iter()
            .filter_map(|head| {
                profiles
                    .history_for(&set, head.name())
                    .map(|history| (head.name().to_owned(), history.clone()))
            })
            .collect();
        assess_change(
            plan,
            &self.heads,
            requests,
            ChangeContext {
                watched: &watched,
                history: &history,
            },
        )
    }

    fn stage(
        &mut self,
        configuration: ZwlrOutputConfigurationV1,
        plan: DisplayConfig,
        tool: Option<DisplayTool>,
        risk: ChangeRisk,
    ) {
        if self.staging.len().saturating_add(self.applies.len()) >= MAX_PENDING_DISPLAY_REQUESTS {
            configuration.failed();
            return;
        }
        let admitted = DisplayTransaction::prepare_request(self.display_set(), plan);
        let Ok(transaction) = admitted else {
            configuration.failed();
            return;
        };
        let source = settings_store::installed_source();
        let (sender, result) = mpsc::channel();
        let wake = self.wake.clone();
        let submitted = crate::shell::durable_writer::global().submit_ordered(move || {
            let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                persist_admitted(transaction, source.as_deref())
            }))
            .unwrap_or_else(|_| Err(std::io::Error::other("display request storage panicked")));
            let _ = sender.send(saved);
            if let Some(wake) = wake {
                wake();
            }
        });
        match submitted {
            Ok(()) => self.staging.push(PendingStaging {
                configuration,
                result,
                tool,
                risk,
            }),
            Err(error) => {
                tracing::warn!(%error, "output management: state writer refused display request");
                configuration.failed();
            }
        }
    }

    /// Reflect the DPMS transition sent to the backend and presenter. Idle
    /// blanking does not replace a tool's per-output off intent.
    pub fn set_session_blanked(&mut self, blanked: bool) {
        if self.session_blanked == blanked {
            return;
        }
        self.session_blanked = blanked;
        for (power, output) in &self.powers {
            power.mode(power_mode(self.output_power_on(*output)));
        }
    }

    fn output_power_on(&self, output: OutputId) -> bool {
        !self.session_blanked && !self.powered_off.contains(&output)
    }

    fn set_power(&mut self, output: OutputId, on: bool) {
        if on {
            self.powered_off.remove(&output);
        } else {
            self.powered_off.insert(output);
        }
        // Reassert explicit intent even when it matches the cached state:
        // idle blanking and a connector reset can have switched scanout off.
        self.power_requests.push((output, on));
        for (power, _) in self.powers.iter().filter(|(_, o)| *o == output) {
            power.mode(power_mode(self.output_power_on(output)));
        }
    }
}

/// A session record can be durable even when rewriting its compatibility copy
/// fails. Only an exact published snapshot proves that boundary; a storage
/// error with no matching record remains a failure. No backend receipt is sent.
fn persist_admitted(
    transaction: DisplayTransaction,
    source: Option<&dyn settings_store::SettingsSource>,
) -> std::io::Result<DisplayTransaction> {
    let value = serde_json::to_value(&transaction).map_err(std::io::Error::other)?;
    let values = value
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("invalid display request"))?;
    let stored = match source {
        Some(source) => source.update_section_report(
            settings_store::hardware::DISPLAY_TRANSACTION_SECTION,
            values,
        ),
        None => settings_store::update_section(
            settings_store::hardware::DISPLAY_TRANSACTION_SECTION,
            values,
        )
        .map(|()| ConditionalUpdate::Stored),
    }?;
    match stored {
        ConditionalUpdate::Stored => {}
        ConditionalUpdate::Superseded => {
            return Err(std::io::Error::other(
                "display request was superseded during storage",
            ));
        }
        ConditionalUpdate::StoredWithCopyRepair(error) => {
            // The operation itself proves durable acceptance. A later
            // replacement must still fail this original request; this read
            // never turns a generic storage error into a receipt.
            let current = source
                .and_then(|source| source.load().get("display_transaction").cloned())
                .and_then(|value| serde_json::from_value::<DisplayTransaction>(value).ok());
            if current
                .as_ref()
                .is_none_or(|current| !same_admission(&transaction, current))
            {
                return Err(std::io::Error::other(
                    "display request was replaced after durable storage",
                ));
            }
            tracing::warn!(%error, transaction = transaction.id, "output management: durable request accepted; settings copy repair remains pending");
        }
    }
    Ok(transaction)
}

/// Mutable backend state may advance while the storage result is queued.
/// Match every immutable admission field, never just the clock-derived ID.
fn same_admission(admitted: &DisplayTransaction, durable: &DisplayTransaction) -> bool {
    admitted.id == durable.id
        && admitted.set == durable.set
        && admitted.previous == durable.previous
        && admitted.candidate == durable.candidate
}

/// Keep an applied change, or hand it to the person when it is risky.
///
/// A risky change from a trusted tool is kept too, except one that leaves
/// every head off: with no display showing, nobody could see what was kept.
fn keep_or_ask(
    transaction: &DisplayTransaction,
    apply: &PendingApply,
    heads: &[OutputHead],
    keeps: &mut Vec<PendingKeep>,
) -> Option<DisplayConfirmation> {
    let tool = apply.tool.as_ref();
    let risk = if leaves_a_display_on(&transaction.candidate, heads) {
        apply.risk
    } else {
        ChangeRisk::NoDisplay
    };
    let keep = match risk {
        ChangeRisk::Safe => true,
        ChangeRisk::Risky => tool.is_some_and(|tool| TrustedDisplayTools::load().contains(tool)),
        ChangeRisk::NoDisplay => false,
    };
    if keep {
        match crate::shell::display_persistence::answer_for(
            crate::shell::display_persistence::Answer::Keep,
            Some(transaction.id),
        ) {
            Ok(result) => {
                keeps.push(PendingKeep {
                    result,
                    confirmation: DisplayConfirmation {
                        transaction: transaction.id,
                        tool: tool.cloned(),
                    },
                    asked: AskedChange {
                        transaction: transaction.id,
                        tool: tool.cloned(),
                        set: transaction.set.key.clone(),
                        plan: transaction.candidate.clone(),
                    },
                });
                return None;
            }
            Err(error) => {
                tracing::warn!(%error, "output management: could not queue automatic Keep");
            }
        }
    }
    Some(DisplayConfirmation {
        transaction: transaction.id,
        tool: tool.cloned(),
    })
}

/// Bring one client's view of the heads up to date, then `done`.
fn publish(display: &DisplayHandle, bound: &mut BoundManager, heads: &[OutputHead], serial: u32) {
    bound.heads.retain(|head| {
        let keep = heads.iter().any(|h| h.name() == head.key);
        if !keep {
            for (_, mode) in &head.modes {
                mode.finished();
            }
            head.resource.finished();
        }
        keep
    });
    let Ok(client) = display.get_client(bound.manager.id()) else {
        return;
    };
    for head in heads {
        if let Some(existing) = bound.heads.iter_mut().find(|b| b.key == head.name()) {
            if existing.sent != *head {
                send_head(display, &client, existing, head, false);
                existing.sent = head.clone();
            }
            continue;
        }
        let Ok(resource) = client.create_resource::<ZwlrOutputHeadV1, _, State>(
            display,
            bound.manager.version(),
            HeadData {
                key: head.name().to_owned(),
            },
        ) else {
            continue;
        };
        bound.manager.head(&resource);
        let mut fresh = BoundHead {
            key: head.name().to_owned(),
            resource,
            modes: Vec::new(),
            sent: head.clone(),
        };
        send_head(display, &client, &mut fresh, head, true);
        bound.heads.push(fresh);
    }
    bound.manager.done(serial);
}

fn send_head(
    display: &DisplayHandle,
    client: &Client,
    bound: &mut BoundHead,
    head: &OutputHead,
    initial: bool,
) {
    let resource = &bound.resource;
    if initial {
        resource.name(head.name().to_owned());
        resource.description(head.description.clone());
        if let Some((width, height)) = head.physical_size_mm {
            resource.physical_size(to_i32(width), to_i32(height));
        }
        if let Some(monitor) = head.descriptor.monitor.as_ref()
            && resource.version() >= 2
        {
            for (value, send) in [
                (
                    &monitor.make,
                    ZwlrOutputHeadV1::make as fn(&ZwlrOutputHeadV1, String),
                ),
                (&monitor.model, ZwlrOutputHeadV1::model),
                (&monitor.serial, ZwlrOutputHeadV1::serial_number),
            ] {
                if !value.is_empty() {
                    send(resource, value.clone());
                }
            }
        }
    }
    bound.modes.retain(|(mode, object)| {
        let keep = head.descriptor.modes.contains(mode);
        if !keep {
            object.finished();
        }
        keep
    });
    for mode in &head.descriptor.modes {
        if bound.modes.iter().any(|(known, _)| known == mode) {
            continue;
        }
        let Ok(object) = client.create_resource::<ZwlrOutputModeV1, _, State>(
            display,
            resource.version().min(MODE_VERSION),
            ModeData {
                key: head.name().to_owned(),
                mode: *mode,
            },
        ) else {
            continue;
        };
        resource.mode(&object);
        object.size(to_i32(mode.width), to_i32(mode.height));
        if mode.refresh_mhz > 0 {
            object.refresh(to_i32(mode.refresh_mhz));
        }
        if Some(*mode) == head.current_mode && !head.enabled {
            // The connector's native mode is the best guess at its
            // preference while it is off.
            object.preferred();
        }
        bound.modes.push((*mode, object));
    }
    resource.enabled(i32::from(head.enabled));
    if head.enabled {
        if let Some((_, object)) = bound
            .modes
            .iter()
            .find(|(mode, _)| Some(*mode) == head.current_mode)
        {
            resource.current_mode(object);
        }
        resource.position(head.position.0, head.position.1);
        resource.transform(wl_transform(head.transform));
        resource.scale(head.scale);
    }
    if head.descriptor.vrr && resource.version() >= 4 {
        resource.adaptive_sync(if head.adaptive_sync {
            zwlr_output_head_v1::AdaptiveSyncState::Enabled
        } else {
            zwlr_output_head_v1::AdaptiveSyncState::Disabled
        });
    }
}

fn to_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn wl_transform(transform: OutputTransform) -> wl_output::Transform {
    match transform {
        OutputTransform::Normal => wl_output::Transform::Normal,
        OutputTransform::Rotate90 => wl_output::Transform::_90,
        OutputTransform::Rotate180 => wl_output::Transform::_180,
        OutputTransform::Rotate270 => wl_output::Transform::_270,
    }
}

/// `None` for the flipped transforms, which Tilo's presenter cannot scan out.
fn tilo_transform(transform: wl_output::Transform) -> Option<OutputTransform> {
    match transform {
        wl_output::Transform::Normal => Some(OutputTransform::Normal),
        wl_output::Transform::_90 => Some(OutputTransform::Rotate90),
        wl_output::Transform::_180 => Some(OutputTransform::Rotate180),
        wl_output::Transform::_270 => Some(OutputTransform::Rotate270),
        _ => None,
    }
}

fn power_mode(on: bool) -> zwlr_output_power_v1::Mode {
    if on {
        zwlr_output_power_v1::Mode::On
    } else {
        zwlr_output_power_v1::Mode::Off
    }
}

fn lock(pending: &Mutex<PendingConfiguration>) -> std::sync::MutexGuard<'_, PendingConfiguration> {
    pending.lock().unwrap_or_else(PoisonError::into_inner)
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let management = &mut state.output_management;
        let mut bound = BoundManager {
            manager,
            heads: Vec::new(),
        };
        publish(display, &mut bound, &management.heads, management.serial);
        management.managers.push(bound);
    }

    fn can_view(client: Client, (): &()) -> bool {
        super::state::is_privileged_client(&client)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        (): &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    ConfigurationData {
                        serial,
                        pending: Arc::default(),
                    },
                );
            }
            zwlr_output_manager_v1::Request::Stop => {
                manager.finished();
                state
                    .output_management
                    .managers
                    .retain(|bound| bound.manager != *manager);
            }
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ZwlrOutputManagerV1, (): &()) {
        state
            .output_management
            .managers
            .retain(|bound| bound.manager != *manager);
    }
}

impl Dispatch<ZwlrOutputHeadV1, HeadData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _head: &ZwlrOutputHeadV1,
        _request: zwlr_output_head_v1::Request,
        _data: &HeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `release` is the only request; its destructor needs no work.
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        _request: zwlr_output_mode_v1::Request,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // `release` is the only request; its destructor needs no work.
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ConfigurationData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        configuration: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &ConfigurationData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_v1::{Error, Request};
        let mut pending = lock(&data.pending);
        let already_used = |configuration: &ZwlrOutputConfigurationV1| {
            configuration.post_error(
                Error::AlreadyUsed,
                "configuration was already applied or tested",
            );
        };
        match request {
            Request::EnableHead { id, head } => {
                let key = head.data::<HeadData>().map(|d| d.key.clone());
                let config_head = data_init.init(
                    id,
                    ConfigurationHeadData {
                        key: key.clone().unwrap_or_default(),
                        pending: data.pending.clone(),
                    },
                );
                if pending.used {
                    return already_used(configuration);
                }
                let Some(key) = key else {
                    return;
                };
                if pending.heads.contains_key(&key) {
                    config_head.post_error(
                        Error::AlreadyConfiguredHead,
                        format!("head {key} is already configured"),
                    );
                    return;
                }
                pending
                    .heads
                    .insert(key, HeadRequest::Enable(HeadChanges::default()));
            }
            Request::DisableHead { head } => {
                if pending.used {
                    return already_used(configuration);
                }
                let Some(key) = head.data::<HeadData>().map(|d| d.key.clone()) else {
                    return;
                };
                if pending.heads.contains_key(&key) {
                    configuration.post_error(
                        Error::AlreadyConfiguredHead,
                        format!("head {key} is already configured"),
                    );
                    return;
                }
                pending.heads.insert(key, HeadRequest::Disable);
            }
            Request::Apply | Request::Test => {
                if pending.used {
                    return already_used(configuration);
                }
                pending.used = true;
                let apply = matches!(request, Request::Apply);
                let requests = std::mem::take(&mut pending.heads);
                let unsupported = pending.unsupported;
                drop(pending);
                let watched = if apply {
                    state.watched_outputs()
                } else {
                    BTreeSet::new()
                };
                let management = &mut state.output_management;
                if data.serial != management.serial {
                    configuration.cancelled();
                    return;
                }
                if unsupported {
                    configuration.failed();
                    return;
                }
                match plan_arrangement(
                    &DisplayConfig::load_effective(&management.display_set()),
                    &management.heads,
                    &requests,
                ) {
                    Ok(plan) if apply => {
                        let tool = client
                            .get_data::<super::state::ClientState>()
                            .and_then(|data| data.executable.clone())
                            .map(DisplayTool::new);
                        if management.refuses_reapply(tool.as_ref(), &plan, Instant::now()) {
                            configuration.failed();
                            return;
                        }
                        let risk = management.assess(&plan, &requests, &watched);
                        management.stage(configuration.clone(), plan, tool, risk);
                    }
                    Ok(_) => configuration.succeeded(),
                    Err(error) => {
                        log_refusal(&error);
                        configuration.failed();
                    }
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        configuration: &ZwlrOutputConfigurationV1,
        _data: &ConfigurationData,
    ) {
        state
            .output_management
            .applies
            .retain(|apply| apply.configuration != *configuration);
        state
            .output_management
            .staging
            .retain(|pending| pending.configuration != *configuration);
    }
}

fn log_refusal(error: &ArrangementError) {
    tracing::info!(?error, "output management: refused a display configuration");
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData> for State {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per protocol request, each a short validate-then-record step"
    )]
    fn request(
        _state: &mut Self,
        _client: &Client,
        head: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::{Error, Request};
        let mut pending = lock(&data.pending);
        if pending.used {
            return;
        }
        let Some(HeadRequest::Enable(changes)) = pending.heads.get_mut(&data.key) else {
            return;
        };
        let mut unsupported_transform = false;
        let already_set = |head: &ZwlrOutputConfigurationHeadV1, what: &str| {
            head.post_error(Error::AlreadySet, format!("{what} was already set"));
        };
        match request {
            Request::SetMode { mode } => {
                if changes.mode.is_some() {
                    return already_set(head, "mode");
                }
                match mode.data::<ModeData>() {
                    Some(mode) if mode.key == data.key => {
                        changes.mode = Some(ModeRequest::Advertised(mode.mode));
                    }
                    _ => head.post_error(Error::InvalidMode, "mode belongs to another head"),
                }
            }
            Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if changes.mode.is_some() {
                    return already_set(head, "mode");
                }
                match (
                    u32::try_from(width).ok().filter(|w| *w > 0),
                    u32::try_from(height).ok().filter(|h| *h > 0),
                    u32::try_from(refresh).ok(),
                ) {
                    (Some(width), Some(height), Some(refresh_mhz)) => {
                        changes.mode = Some(ModeRequest::Custom {
                            width,
                            height,
                            refresh_mhz,
                        });
                    }
                    _ => head.post_error(Error::InvalidCustomMode, "mode size must be positive"),
                }
            }
            Request::SetPosition { x, y } => {
                if changes.position.is_some() {
                    return already_set(head, "position");
                }
                changes.position = Some((x, y));
            }
            Request::SetTransform { transform } => {
                if changes.transform.is_some() {
                    return already_set(head, "transform");
                }
                match transform {
                    WEnum::Value(transform) => match tilo_transform(transform) {
                        Some(transform) => changes.transform = Some(transform),
                        // A valid transform the presenter cannot show: the
                        // configuration fails at test/apply, it is not a
                        // protocol violation.
                        None => unsupported_transform = true,
                    },
                    WEnum::Unknown(_) => {
                        head.post_error(Error::InvalidTransform, "unknown transform");
                    }
                }
            }
            Request::SetScale { scale } => {
                if changes.scale.is_some() {
                    return already_set(head, "scale");
                }
                if scale.is_finite() && scale > 0.0 {
                    changes.scale = Some(scale);
                } else {
                    head.post_error(Error::InvalidScale, "scale must be positive");
                }
            }
            Request::SetAdaptiveSync { state } => {
                if changes.adaptive_sync.is_some() {
                    return already_set(head, "adaptive sync");
                }
                match state {
                    WEnum::Value(zwlr_output_head_v1::AdaptiveSyncState::Enabled) => {
                        changes.adaptive_sync = Some(true);
                    }
                    WEnum::Value(zwlr_output_head_v1::AdaptiveSyncState::Disabled) => {
                        changes.adaptive_sync = Some(false);
                    }
                    _ => head.post_error(
                        Error::InvalidAdaptiveSyncState,
                        "unknown adaptive sync state",
                    ),
                }
            }
            _ => {}
        }
        if unsupported_transform {
            pending.unsupported = true;
        }
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, (): &()) -> bool {
        super::state::is_privileged_client(&client)
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        (): &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request {
            let management = &mut state.output_management;
            let live = super::output_globals::output_id_of(&output).filter(|output| {
                management
                    .heads
                    .iter()
                    .any(|head| head.output == Some(*output))
            });
            let power = data_init.init(id, PowerData { output: live });
            match live {
                Some(output) => {
                    power.mode(power_mode(management.output_power_on(output)));
                    management.powers.push((power, output));
                }
                None => power.failed(),
            }
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, PowerData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        power: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        data: &PowerData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let zwlr_output_power_v1::Request::SetMode { mode } = request else {
            return;
        };
        let on = match mode {
            WEnum::Value(zwlr_output_power_v1::Mode::On) => true,
            WEnum::Value(zwlr_output_power_v1::Mode::Off) => false,
            _ => {
                power.post_error(
                    zwlr_output_power_v1::Error::InvalidMode,
                    "unknown power mode",
                );
                return;
            }
        };
        let management = &mut state.output_management;
        // A failed object stays inert; so does one whose output went away.
        if let Some(output) = data
            .output
            .filter(|_| management.powers.iter().any(|(p, _)| p == power))
        {
            management.set_power(output, on);
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, power: &ZwlrOutputPowerV1, _: &PowerData) {
        state
            .output_management
            .powers
            .retain(|(bound, _)| bound != power);
    }
}

impl State {
    /// Advertise `heads` to output-management clients.
    pub fn set_output_heads(&mut self, heads: Vec<OutputHead>) {
        self.output_management
            .set_heads(&self.display_handle, heads);
    }
}

#[cfg(test)]
mod staging_tests {
    use super::*;

    #[test]
    fn output_staging_durable_identity_allows_backend_advancement_but_not_replacement() {
        let admitted = DisplayTransaction {
            id: 71,
            ..DisplayTransaction::default()
        };
        let mut durable = admitted.clone();
        for state in [
            DisplayTransactionState::AwaitingConfirmation,
            DisplayTransactionState::Confirmed,
            DisplayTransactionState::Rejected,
        ] {
            durable.state = state;
            durable.message = Some("backend outcome".into());
            durable.deadline_unix_ms = Some(123);
            assert!(same_admission(&admitted, &durable));
        }
        durable.id += 1;
        assert!(!same_admission(&admitted, &durable));
        durable.id = admitted.id;
        durable.set.key = "different displays".into();
        assert!(!same_admission(&admitted, &durable));
        durable.set = admitted.set.clone();
        durable.previous.outputs.insert(
            "changed previous".into(),
            settings_store::hardware::OutputConfig::default(),
        );
        assert!(!same_admission(&admitted, &durable));
        durable.previous = admitted.previous.clone();
        durable.candidate.outputs.insert(
            "changed candidate".into(),
            settings_store::hardware::OutputConfig::default(),
        );
        assert!(!same_admission(&admitted, &durable));
    }
}

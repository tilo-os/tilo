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
//! so the DRM backend validates it against the connectors it owns, applies
//! it temporarily and restores the previous layout unless the user keeps
//! it. A tool that picks a mode the panel cannot show leaves a black screen
//! for fifteen seconds, not for the rest of the session. `succeeded` is
//! sent once the backend has applied the plan, `failed` when it rejects it.
//!
//! The shell asks the person to keep or revert each applied change (see
//! [`OutputManagementState::take_confirmation`]). A tool they chose to
//! trust — keyed by the executable behind the connection — has its changes
//! kept without asking, unless the change would leave no display on.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{
    DisplayConfig, DisplaySet, DisplayTransaction, DisplayTransactionState, OutputMode,
    OutputTransform,
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
    ArrangementError, HeadChanges, HeadRequest, ModeRequest, OutputHead, plan_arrangement,
};
use super::state::State;

const MANAGER_VERSION: u32 = 4;
const MODE_VERSION: u32 = 3;
const POWER_MANAGER_VERSION: u32 = 1;

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
struct PendingApply {
    configuration: ZwlrOutputConfigurationV1,
    transaction: u64,
    tool: Option<DisplayTool>,
}

/// A tool's display change the backend applied and the person has to keep
/// or revert before the transaction's deadline.
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
    powers: Vec<(ZwlrOutputPowerV1, OutputId)>,
    /// Outputs a client has switched off. Absent means on.
    powered_off: BTreeSet<OutputId>,
    power_requests: Vec<(OutputId, bool)>,
    confirmation: Option<DisplayConfirmation>,
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
            powers: Vec::new(),
            powered_off: BTreeSet::new(),
            power_requests: Vec::new(),
            confirmation: None,
        }
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

    /// A tool's applied display change the person must now keep or
    /// revert, once per change.
    pub fn take_confirmation(&mut self) -> Option<DisplayConfirmation> {
        self.confirmation.take()
    }

    /// Report the outcome of staged configurations from the current
    /// transaction. Called after the backend reconciled a settings change.
    ///
    /// A change the backend applied is kept at once when it came from a
    /// trusted tool and leaves a display on; otherwise the person is asked.
    pub fn resolve_transactions(&mut self, current: Option<&DisplayTransaction>) {
        let heads = &self.heads;
        let confirmation = &mut self.confirmation;
        self.applies.retain(|apply| {
            let state = current
                .filter(|transaction| transaction.id == apply.transaction)
                .map(|transaction| transaction.state);
            match state {
                Some(DisplayTransactionState::Requested) => true,
                Some(DisplayTransactionState::AwaitingConfirmation) => {
                    apply.configuration.succeeded();
                    if let Some(transaction) = current {
                        *confirmation = confirm_or_ask(transaction, apply.tool.as_ref(), heads);
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

    /// The monitor set the heads describe; display intent is remembered
    /// per set.
    fn display_set(&self) -> DisplaySet {
        let descriptors: Vec<_> = self.heads.iter().map(|h| h.descriptor.clone()).collect();
        DisplaySet::of_descriptors(&descriptors)
    }

    fn stage(
        &mut self,
        configuration: ZwlrOutputConfigurationV1,
        plan: DisplayConfig,
        tool: Option<DisplayTool>,
    ) {
        match DisplayTransaction::request(self.display_set(), plan) {
            Ok(transaction) => {
                tracing::info!(
                    transaction = transaction.id,
                    tool = tool.as_ref().map(DisplayTool::name),
                    "output management: staged an external display configuration"
                );
                self.applies.push(PendingApply {
                    configuration,
                    transaction: transaction.id,
                    tool,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "output management: could not stage display configuration");
                configuration.failed();
            }
        }
    }

    fn set_power(&mut self, output: OutputId, on: bool) {
        let was_on = !self.powered_off.contains(&output);
        if on {
            self.powered_off.remove(&output);
        } else {
            self.powered_off.insert(output);
        }
        if was_on != on {
            self.power_requests.push((output, on));
        }
        for (power, _) in self.powers.iter().filter(|(_, o)| *o == output) {
            power.mode(power_mode(on));
        }
    }
}

/// Keep an applied change from a trusted tool, or hand it to the person.
///
/// A change that leaves every head off is never kept on the tool's word:
/// with no display showing, nobody could see what was kept.
fn confirm_or_ask(
    transaction: &DisplayTransaction,
    tool: Option<&DisplayTool>,
    heads: &[OutputHead],
) -> Option<DisplayConfirmation> {
    let trusted = tool.is_some_and(|tool| TrustedDisplayTools::load().contains(tool));
    if trusted && leaves_a_display_on(&transaction.candidate, heads) {
        let mut transaction = transaction.clone();
        match transaction.request_confirmation() {
            Ok(()) => {
                tracing::info!(
                    transaction = transaction.id,
                    tool = tool.map(DisplayTool::name),
                    "output management: kept a trusted tool's display change"
                );
                return None;
            }
            Err(error) => {
                tracing::warn!(%error, "output management: could not keep a trusted change");
            }
        }
    }
    Some(DisplayConfirmation {
        transaction: transaction.id,
        tool: tool.cloned(),
    })
}

/// Whether `plan` keeps at least one of `heads` enabled.
fn leaves_a_display_on(plan: &DisplayConfig, heads: &[OutputHead]) -> bool {
    heads.iter().any(|head| {
        plan.output_for(head.name())
            .is_some_and(|output| output.enabled)
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
                        management.stage(configuration.clone(), plan, tool);
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
                    power.mode(power_mode(!management.powered_off.contains(&output)));
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

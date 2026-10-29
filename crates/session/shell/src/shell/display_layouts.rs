//! What the shell holds for the Super+P display sheet
//! ([`tilo_sheets::displays`]): the attached displays, the committed layout
//! and the change this session staged, the person's other screens, and the
//! wireless receivers. Displays come from what the compositor published in
//! memory; the layout and the change from the settings document, read again
//! only when it changed. The sheet's choices are carried out here through
//! the shell's published display actions, so the person at the sheet passes
//! the same authority check as an agent or a script. None of it is read
//! while painting.

use std::time::{Duration, Instant};

use settings_store::display_mode::{self, DisplayLayoutError, DisplayLayoutMode};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
    DisplayTransactionState,
};
use settings_store::person_displays::PersonDisplays;
use tilo_sheets::displays::{
    Band, Discovery, Layout, LayoutRow, Link, Receiver, Screen, Sheet, Stream,
};
use ui::i18n::ArgValue;

use crate::shell::i18n::{t, t_args};
use crate::shell::projection::{
    DisplayResolutionLimit, ProjectionController, ProjectionMode, ProjectionProtocol,
    ProjectionStatus,
};
use crate::shell::shared_display_actions::{self, SharedDisplayView};

/// How often an open sheet asks whether the settings document holding the
/// committed layout and the staged change moved on. It parses the document
/// again only when it did.
const DISPLAY_POLL: Duration = Duration::from_millis(250);

/// The sheet's name for a desktop layout.
pub(crate) const fn layout_of(mode: DisplayLayoutMode) -> Layout {
    match mode {
        DisplayLayoutMode::Extend => Layout::Extend,
        DisplayLayoutMode::Mirror => Layout::Mirror,
        DisplayLayoutMode::BuiltinOnly => Layout::BuiltinOnly,
        DisplayLayoutMode::ExternalOnly => Layout::ExternalOnly,
    }
}

/// The desktop layout the sheet names.
pub(crate) const fn mode_of(layout: Layout) -> DisplayLayoutMode {
    match layout {
        Layout::Extend => DisplayLayoutMode::Extend,
        Layout::Mirror => DisplayLayoutMode::Mirror,
        Layout::BuiltinOnly => DisplayLayoutMode::BuiltinOnly,
        Layout::ExternalOnly => DisplayLayoutMode::ExternalOnly,
    }
}

/// The wireless stream layout that corresponds to a desktop layout. A
/// receiver is an external display, so "only this screen" has no stream.
fn wireless_mode_for(mode: DisplayLayoutMode) -> Option<ProjectionMode> {
    match mode {
        DisplayLayoutMode::Extend => Some(ProjectionMode::Extend),
        DisplayLayoutMode::Mirror => Some(ProjectionMode::Mirror),
        DisplayLayoutMode::ExternalOnly => Some(ProjectionMode::ExternalOnly),
        DisplayLayoutMode::BuiltinOnly => None,
    }
}

fn layout_for_wireless(mode: ProjectionMode) -> DisplayLayoutMode {
    match mode {
        ProjectionMode::Mirror => DisplayLayoutMode::Mirror,
        ProjectionMode::Extend => DisplayLayoutMode::Extend,
        ProjectionMode::ExternalOnly => DisplayLayoutMode::ExternalOnly,
    }
}

fn waiting(transaction: &DisplayTransaction) -> bool {
    matches!(
        transaction.state,
        DisplayTransactionState::Requested
            | DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::ConfirmRequested
    )
}

/// A display choice the sheet asks the host to make through its published
/// action, so the person at the sheet passes the same authority check as an
/// agent or a script invoking it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayRequest {
    Layout(DisplayLayoutMode),
    Keep,
    Revert,
}

impl DisplayRequest {
    /// The published action this choice is.
    pub(crate) fn action_id(self) -> &'static str {
        use crate::shell::actions as ids;
        match self {
            Self::Layout(DisplayLayoutMode::Extend) => ids::DISPLAY_EXTEND_ID,
            Self::Layout(DisplayLayoutMode::Mirror) => ids::DISPLAY_MIRROR_ID,
            Self::Layout(DisplayLayoutMode::BuiltinOnly) => ids::DISPLAY_BUILTIN_ONLY_ID,
            Self::Layout(DisplayLayoutMode::ExternalOnly) => ids::DISPLAY_EXTERNAL_ONLY_ID,
            Self::Keep => ids::DISPLAY_KEEP_ID,
            Self::Revert => ids::DISPLAY_REVERT_ID,
        }
    }
}

/// The display change a layout action staged, from its receipt.
fn staged_transaction(receipt: &crate::shell::search::ActionReceipt) -> Option<u64> {
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &receipt.update else {
        return None;
    };
    output.value.as_ref()?.get("transaction")?.as_u64()
}

/// Which version of the settings document the sheet last read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SettingsSeen {
    /// Not read yet, or read while it may still change under the same stamp.
    #[default]
    Unread,
    /// Read at this stamp (`None`: there was no document to stamp).
    At(Option<settings_store::SettingsStamp>),
}

/// Attached displays, the committed layout and the transaction this
/// session staged. Displays come from what the compositor published in
/// memory; the layout and the change from the settings document, read again
/// only when it changed. None of it is read while painting.
#[derive(Default)]
pub(crate) struct DisplayLayouts {
    outputs: Vec<DisplayOutputSnapshot>,
    committed: DisplayConfig,
    transaction: Option<DisplayTransaction>,
    polled_at: Option<Instant>,
    outputs_seen: Option<u64>,
    /// Generation of the published person's displays `person` came from.
    person_seen: Option<u64>,
    /// Settings document version `committed` and `transaction` were read at.
    settings_seen: SettingsSeen,
    /// Id of the transaction this sheet staged, so an old rejection left in
    /// settings is never reported as the answer to a new choice.
    staged: Option<u64>,
    /// Id of the change an action opened the sheet for, once it was first
    /// read for [`Self::settled`].
    followed: Option<u64>,
    notice: Option<String>,
    /// The Keep or Revert action this sheet invoked, until it is recorded.
    answer: Option<crate::shell::search::PendingInvocation>,
    /// The layout action this sheet invoked, until it is recorded.
    staging: Option<crate::shell::search::PendingInvocation>,

    /// The person's displays across their devices, as this device routes by
    /// them; `None` while display continuity is off.
    person: Option<SharedDisplayView>,
}

impl DisplayLayouts {
    fn refresh_person(&mut self) {
        self.person = shared_display_actions::published();
    }

    /// The other devices' screens in display order.
    fn remote(&self) -> Vec<Screen> {
        let Some(view) = self.person.as_ref() else {
            return Vec::new();
        };
        view.layout
            .listed()
            .filter(|display| !display.local)
            .map(|display| Screen {
                device: display.device.clone(),
                output: display.output.clone(),
                name: display.name.clone(),
                number: display.order.saturating_add(1),
                revision: view
                    .arrangement
                    .entry(&display.device, &display.output)
                    .map_or(0, |entry| entry.rev),
                device_label: display.device_label.clone(),
                online: display.online,
                active: display.rect.width > 0,
                enabled: display.enabled,
            })
            .collect()
    }

    /// Take what the compositor published since the last look, from
    /// memory: the attached displays and the person's other screens.
    /// Returns whether anything changed.
    fn sync_published(&mut self) -> bool {
        let mut changed = false;
        let generation = DisplayHardwareSnapshot::published_generation();
        if self.outputs_seen != Some(generation) {
            self.outputs_seen = Some(generation);
            let outputs = DisplayHardwareSnapshot::published()
                .map(|snapshot| snapshot.outputs.clone())
                .unwrap_or_default();
            if outputs != self.outputs {
                self.outputs = outputs;
                // The committed layout is the attached set's own.
                self.settings_seen = SettingsSeen::Unread;
                self.polled_at = None;
                changed = true;
            }
        }
        let generation = PersonDisplays::published_generation();
        if self.person_seen != Some(generation) {
            self.person_seen = Some(generation);
            self.refresh_person();
            changed = true;
        }
        changed
    }

    /// Read the committed layout and the staged change from the settings
    /// document, and the arrangement of the person's screens with them.
    fn read_settings(&mut self, now: Instant) {
        let stamp = settings_store::stamp();
        self.committed =
            DisplayConfig::load_for(&settings_store::display_mode::attached_set(&self.outputs));
        self.transaction = DisplayTransaction::load();
        self.refresh_person();
        self.polled_at = Some(now);
        self.settings_seen = SettingsSeen::At(stamp);
    }

    /// Everything at once: when the sheet opens.
    fn refresh(&mut self, now: Instant) {
        self.sync_published();
        self.read_settings(now);
    }

    fn poll_completions(&mut self) -> bool {
        let mut changed = false;
        if let Some(staging) = self.staging.as_ref() {
            match staging() {
                Ok(None) => {}
                Ok(Some(receipt)) => {
                    changed = true;
                    self.staging = None;
                    self.staged = staged_transaction(&receipt);
                    self.notice = None;
                    self.settings_seen = SettingsSeen::Unread;
                    self.polled_at = None;
                }
                Err(error) => {
                    changed = true;
                    self.staging = None;
                    tracing::warn!(%error, "display layout was not staged");
                    self.notice = Some(error.to_string());
                }
            }
        }
        if let Some(answer) = self.answer.as_ref() {
            let done = match answer() {
                Ok(None) => None,
                Ok(Some(_)) => Some(None),
                Err(error) => Some(Some(error.to_string())),
            };
            if let Some(notice) = done {
                changed = true;
                self.answer = None;
                self.notice = notice;
                self.settings_seen = SettingsSeen::Unread;
                self.polled_at = None;
            }
        }
        changed
    }

    /// The ordinary update while the sheet is open: completions, what the
    /// compositor published, and the settings document if it moved on, at
    /// most every [`DISPLAY_POLL`]. Returns whether anything changed.
    fn poll(&mut self, now: Instant) -> bool {
        let mut changed = self.poll_completions();
        changed |= self.sync_published();
        if self
            .polled_at
            .is_none_or(|at| now.duration_since(at) >= DISPLAY_POLL)
        {
            self.polled_at = Some(now);
            let stamp = settings_store::stamp();
            if self.settings_seen != SettingsSeen::At(stamp) {
                let before = (self.committed.clone(), self.transaction.clone());
                self.read_settings(now);
                changed |= before != (self.committed.clone(), self.transaction.clone());
            }
        }
        changed
    }

    /// The staged transaction while it still waits for Keep or Revert.
    fn pending(&self) -> Option<&DisplayTransaction> {
        self.transaction
            .as_ref()
            .filter(|transaction| waiting(transaction))
    }

    /// Whether display change `id` is over: kept, reverted, refused, or
    /// replaced by another. Storage is asked at most every
    /// [`DISPLAY_POLL`], and at once when a new change is first followed.
    fn settled(&mut self, id: u64, now: Instant) -> bool {
        if self.followed != Some(id) {
            self.followed = Some(id);
            self.polled_at = None;
            self.settings_seen = SettingsSeen::Unread;
        }
        self.poll(now);
        self.read_as_settled(id)
    }

    /// Whether `id` is settled as last read, or not read since it was
    /// first followed. Never reads storage.
    fn read_as_settled(&self, id: u64) -> bool {
        self.followed != Some(id)
            || self
                .transaction
                .as_ref()
                .is_none_or(|transaction| transaction.id != id || !waiting(transaction))
    }

    fn effective(&self) -> &DisplayConfig {
        self.pending()
            .map_or(&self.committed, |transaction| &transaction.candidate)
    }

    fn multiple_displays(&self) -> bool {
        self.outputs.len() >= 2
    }

    fn active(&self) -> Option<DisplayLayoutMode> {
        display_mode::active_layout(&self.outputs, self.effective())
    }

    fn plan_error(&self, mode: DisplayLayoutMode) -> Option<DisplayLayoutError> {
        display_mode::plan_layout(mode, &self.outputs, &self.committed).err()
    }

    /// Follow the action the host invoked for `request`: wait for its
    /// receipt, or show why it was refused.
    fn follow(
        &mut self,
        request: DisplayRequest,
        outcome: Result<
            crate::shell::search::InvocationDisposition,
            crate::shell::search::action_api::ActionError,
        >,
    ) {
        use crate::shell::search::InvocationDisposition;
        let ticket: crate::shell::search::PendingInvocation = match outcome {
            Ok(InvocationDisposition::Pending(ticket)) => ticket,
            Ok(InvocationDisposition::Accepted(receipt)) => {
                std::rc::Rc::new(move || Ok(Some(receipt.clone())))
            }
            Err(error) => {
                tracing::warn!(%error, action = request.action_id(), "display choice refused");
                self.notice = Some(error.to_string());
                return;
            }
        };
        self.notice = None;
        match request {
            DisplayRequest::Layout(_) => self.staging = Some(ticket),
            DisplayRequest::Keep | DisplayRequest::Revert => self.answer = Some(ticket),
        }
    }

    /// Seconds left before an unconfirmed layout restores itself.
    fn seconds_left(&self, now_unix_ms: u64) -> Option<u64> {
        let transaction = self.pending()?;
        let deadline = transaction.deadline_unix_ms?;
        Some(deadline.saturating_sub(now_unix_ms).div_ceil(1000))
    }

    /// Why the layout this sheet staged was refused, if it was.
    fn rejection(&self) -> Option<String> {
        if let Some(notice) = &self.notice {
            return Some(notice.clone());
        }
        let transaction = self.transaction.as_ref()?;
        (transaction.state == DisplayTransactionState::Rejected
            && self.staged == Some(transaction.id))
        .then(|| transaction.message.clone().unwrap_or_default())
    }
}

/// Everything the display sheet shows and carries out, held by the shell.
pub(crate) struct DisplaySheetModel {
    controller: ProjectionController,
    displays: DisplayLayouts,
    /// The receiver the person chose on this opening, by id. Its stream
    /// options (layout, audio, quality, a PIN) show only once one is chosen
    /// or a stream runs.
    chosen: Option<String>,
    /// The open-time state (display snapshot, settings) was taken for the
    /// current opening.
    open: bool,
}

impl Default for DisplaySheetModel {
    fn default() -> Self {
        Self::from_controller(ProjectionController::default())
    }
}

impl DisplaySheetModel {
    pub(crate) fn from_controller(controller: ProjectionController) -> Self {
        Self {
            controller,
            displays: DisplayLayouts::default(),
            chosen: None,
            open: false,
        }
    }

    pub(crate) fn controller(&self) -> &ProjectionController {
        &self.controller
    }

    /// Whether a receiver session that streams captured frames is being
    /// established or is live.
    pub(crate) fn capture_requested(&self) -> bool {
        let (ProjectionStatus::Connecting { device_id }
        | ProjectionStatus::Connected { device_id }) = &self.controller.status
        else {
            return false;
        };
        self.controller
            .devices
            .iter()
            .find(|device| &device.id == device_id)
            .is_some_and(|device| {
                matches!(
                    device.protocol,
                    ProjectionProtocol::Chromecast | ProjectionProtocol::FCast
                )
            })
    }

    /// Take the open-time state the moment the sheet is open (the display
    /// snapshot, the settings, a fresh search), or forget it once closed.
    /// Whether it opened.
    pub(crate) fn follow_open(&mut self, open: bool) -> bool {
        if open && !self.open {
            self.open = true;
            self.displays.notice = None;
            self.displays.staged = None;
            self.chosen = None;
            self.displays.refresh(Instant::now());
            self.controller.refresh();
            true
        } else {
            if !open {
                self.open = false;
            }
            false
        }
    }

    /// The ordinary host update: the receiver worker and the display
    /// actions' receipts, even at rest, and, while open, what the
    /// compositor published and what changed in settings. Whether anything
    /// changed.
    pub(crate) fn poll(&mut self, open: bool) -> bool {
        let opened = self.follow_open(open);
        let mut changed = opened | self.controller.poll();
        changed |= if open {
            self.displays.poll(Instant::now())
        } else {
            self.displays.poll_completions()
        };
        changed
    }

    /// Whether display change `id`, which an action opened the sheet for,
    /// has settled, so the sheet that only showed it can close.
    pub(crate) fn followed_change_settled(&mut self, id: u64, now: Instant) -> bool {
        self.displays.settled(id, now)
    }

    /// Whether `id` read as settled at the last look; never reads storage.
    pub(crate) fn followed_change_read_as_settled(&self, id: u64) -> bool {
        self.displays.read_as_settled(id)
    }

    /// What invoking the action for `request` answered: a job to follow to
    /// its receipt, or why it was refused.
    pub(crate) fn follow_request(
        &mut self,
        request: DisplayRequest,
        outcome: Result<
            crate::shell::search::InvocationDisposition,
            crate::shell::search::action_api::ActionError,
        >,
    ) {
        self.displays.follow(request, outcome);
    }

    /// Discovery and handshakes in flight, and the Keep/Revert countdown
    /// while the sheet is open, need frames.
    pub(crate) fn needs_frame(&self, open: bool) -> bool {
        matches!(
            self.controller.status,
            ProjectionStatus::Scanning | ProjectionStatus::Connecting { .. }
        ) || (open && self.displays.pending().is_some())
    }

    /// The layout the sheet marks as current: the physical layout when two
    /// or more displays are attached, else the wireless stream layout.
    fn active_layout(&self) -> Option<DisplayLayoutMode> {
        if self.displays.multiple_displays() {
            self.displays.active()
        } else if self.controller.devices.is_empty() {
            None
        } else {
            Some(layout_for_wireless(self.controller.options.mode))
        }
    }

    /// The receiver whose stream the sheet configures: the one streaming
    /// or connecting, else the one the person chose on this opening.
    fn target(&self) -> Option<&crate::shell::projection::ProjectionDevice> {
        let live = match &self.controller.status {
            ProjectionStatus::Connecting { device_id }
            | ProjectionStatus::Connected { device_id } => Some(device_id.as_str()),
            _ => None,
        };
        let id = live.or(self.chosen.as_deref())?;
        self.controller
            .devices
            .iter()
            .find(|device| device.id == id)
    }

    /// Whether `mode` is something this device can do now: the attached
    /// displays can be planned into it, or the chosen receiver can stream
    /// it. Nothing else is shown.
    fn layout_possible(&self, mode: DisplayLayoutMode) -> bool {
        self.displays.plan_error(mode).is_none()
            || (self.target().is_some()
                && wireless_mode_for(mode)
                    .is_some_and(|wireless| self.controller.selected_device_supports(wireless)))
    }

    /// Whether choosing `mode` does anything right now: it is shown and no
    /// earlier choice is still being saved.
    fn layout_enabled(&self, mode: DisplayLayoutMode) -> bool {
        self.displays.staging.is_none() && self.layout_possible(mode)
    }

    /// Whether the chosen receiver is an `AirPlay` one, which may ask for a
    /// PIN or password.
    fn airplay_selected(&self) -> bool {
        self.target()
            .is_some_and(|device| device.protocol == ProjectionProtocol::AirPlay)
    }

    /// What the sheet shows now; `screens_editable` when the person may
    /// change the shared display layout here.
    pub(crate) fn sheet(&self, screens_editable: bool) -> Sheet {
        let active = self.active_layout();
        let layouts = DisplayLayoutMode::ALL
            .into_iter()
            .filter(|mode| self.layout_possible(*mode))
            .map(|mode| LayoutRow {
                layout: layout_of(mode),
                enabled: self.layout_enabled(mode),
                current: active == Some(mode),
                stages: self.displays.plan_error(mode).is_none(),
            })
            .collect();
        let band = if self.displays.staging.is_some() {
            Band::Saving
        } else if self.displays.pending().is_some() {
            let status = if self.displays.answer.is_some() {
                t("display-confirm-answer-pending")
            } else if let Some(notice) = self.displays.notice.as_ref() {
                t_args(
                    "display-confirm-answer-failed",
                    &[("reason", ArgValue::Text(notice.clone()))],
                )
            } else {
                crate::shell::display_confirm::countdown_status(
                    self.displays.seconds_left(tilo_clock::unix_now_ms()),
                )
            };
            Band::Confirm {
                status,
                answering: self.displays.answer.is_some(),
            }
        } else if let Some(reason) = self.displays.rejection() {
            Band::Refused(reason)
        } else {
            Band::Idle
        };
        let chosen = self.target().map(|device| device.id.clone());
        let receivers = self
            .controller
            .devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                let link = match &self.controller.status {
                    ProjectionStatus::Connected { device_id } if *device_id == device.id => {
                        Link::Connected
                    }
                    ProjectionStatus::Connecting { device_id } if *device_id == device.id => {
                        Link::Connecting
                    }
                    ProjectionStatus::Failed(failure) if index == self.controller.selected => {
                        Link::Failed(failure.title())
                    }
                    _ => Link::Idle,
                };
                Receiver {
                    id: device.id.clone(),
                    name: device.name.clone(),
                    protocol: device.protocol.label().to_owned(),
                    detail: device.detail.clone(),
                    link,
                    asks_pin: device.protocol == ProjectionProtocol::AirPlay,
                    chosen: chosen.as_ref() == Some(&device.id),
                }
            })
            .collect();
        let discovery = match self.controller.status {
            ProjectionStatus::Scanning => Discovery::Searching,
            ProjectionStatus::Failed(failure) if self.controller.devices.is_empty() => {
                Discovery::Failed {
                    title: failure.title(),
                    next_step: failure.next_step(),
                }
            }
            _ => Discovery::NoneFound,
        };
        let policy = &self.controller.options.stream_policy;
        Sheet {
            layouts,
            band,
            screens: self.displays.remote(),
            screens_editable,
            receivers,
            discovery,
            stream: self.target().map(|_| Stream {
                audio: self.controller.options.audio,
                adaptive: policy.adaptive,
                resolution: policy.resolution_limit.label().to_owned(),
                fps: policy.max_fps,
            }),
            asks_pin: self.airplay_selected(),
        }
    }

    /// Choose `layout`: make it the stream layout for wireless receivers,
    /// and, when it applies to the attached displays, the request to stage
    /// it through its published action.
    pub(crate) fn choose_layout(&mut self, layout: Layout) -> Option<DisplayRequest> {
        let mode = mode_of(layout);
        if !self.layout_enabled(mode) {
            return None;
        }
        if let Some(wireless) = wireless_mode_for(mode)
            && (self.controller.devices.is_empty()
                || self.controller.selected_device_supports(wireless))
            && self.controller.options.mode != wireless
        {
            self.controller.options.mode = wireless;
            self.restart_live_stream();
        }
        self.displays
            .plan_error(mode)
            .is_none()
            .then_some(DisplayRequest::Layout(mode))
    }

    /// Keep or Revert may be asked for: a change waits and no answer is on
    /// its way.
    pub(crate) fn may_answer(&self) -> bool {
        self.displays.answer.is_none() && self.displays.transaction.is_some()
    }

    /// What including or leaving out one of the other devices' screens
    /// answered.
    pub(crate) fn included(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                self.displays.refresh_person();
                self.displays.notice = None;
            }
            Err(error) => self.displays.notice = Some(error),
        }
    }

    fn select(&mut self, receiver: &str) -> bool {
        let Some(index) = self
            .controller
            .devices
            .iter()
            .position(|device| device.id == receiver)
        else {
            return false;
        };
        if self.controller.selected != index {
            self.controller.options.credential = None;
        }
        self.controller.selected = index;
        self.chosen = Some(receiver.to_owned());
        true
    }

    /// Choose `receiver` for its stream options, without connecting.
    pub(crate) fn choose_receiver(&mut self, receiver: &str) {
        self.select(receiver);
    }

    /// Stream to `receiver`, with the PIN typed for it when there is one.
    pub(crate) fn connect(&mut self, receiver: &str, pin: String) {
        if self.select(receiver) {
            self.controller.options.credential = (!pin.is_empty()).then_some(pin);
            self.controller.connect_selected();
        }
    }

    pub(crate) fn stop(&mut self) {
        self.controller.disconnect();
    }

    pub(crate) fn search(&mut self) {
        self.controller.refresh();
    }

    pub(crate) fn share_audio(&mut self, on: bool) {
        self.controller.options.audio = on;
        self.restart_live_stream();
    }

    pub(crate) fn adaptive_quality(&mut self, on: bool) {
        self.controller.options.stream_policy.adaptive = on;
        self.restart_live_stream();
    }

    pub(crate) fn next_resolution(&mut self) {
        let limits = DisplayResolutionLimit::ALL;
        let current = limits
            .iter()
            .position(|limit| *limit == self.controller.options.stream_policy.resolution_limit)
            .unwrap_or(0);
        let next = current
            .wrapping_add(1)
            .checked_rem(limits.len())
            .unwrap_or(0);
        if let Some(&limit) = limits.get(next) {
            self.controller.options.stream_policy.resolution_limit = limit;
        }
        self.restart_live_stream();
    }

    pub(crate) fn frame_rate(&mut self, fps: u32) {
        self.controller.options.stream_policy.max_fps = if fps <= 30 { 30 } else { 60 };
        self.restart_live_stream();
    }

    /// Restart the running stream so a changed option takes effect: the
    /// receiver is sent the options once, when it connects.
    fn restart_live_stream(&mut self) {
        let live = matches!(
            &self.controller.status,
            ProjectionStatus::Connected { device_id }
                if self.controller.devices.get(self.controller.selected)
                    .is_some_and(|device| &device.id == device_id)
        );
        if live {
            self.controller.connect_selected();
        }
    }
}

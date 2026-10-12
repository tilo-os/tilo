//! The Super+P display sheet: where the desktop appears.
//!
//! The upper section switches the whole desktop between the four familiar
//! layouts — extend, mirror, only this screen, only external — and stages
//! the choice as a display transaction the compositor applies at once and
//! restores unless the user keeps it. Wireless receivers are the lower
//! section of the same sheet, so "put my desktop on that screen" has one
//! home whether the screen is on a cable or on the network.
//!
//! Between them, the person's other devices' screens are listed, each with
//! whether it takes part in their shared display layout: on, this device's
//! pointer and keyboard can move onto it. The four layouts only ever plan
//! this device's own hardware; another device's screen is arranged, never
//! driven, from here.
//!
//! The sheet owns its state, animation, rendering, credential editing and
//! hit-testing. The overlay host only coordinates visibility and invalidation.

use std::time::{Duration, Instant};
use ui::num::{f32_of_f64, f64_of_usize, usize_of_f64};

use settings_store::display_mode::{self, DisplayLayoutError, DisplayLayoutMode};
use settings_store::hardware::{
    DisplayConfig, DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
    DisplayTransactionState,
};
use settings_store::person_displays::PersonDisplays;
use taffy::{Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::i18n::ArgValue;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{EventCtx, TextInput, Widget};

use crate::shell::i18n::{t, t_args};
use crate::shell::projection::{
    DisplayResolutionLimit, ProjectionController, ProjectionMode, ProjectionProtocol,
    ProjectionStatus,
};
use crate::shell::shared_display_actions::{self, SharedDisplayView};
use ui::TextRole;
use ui::widget::ButtonRole;

const OPEN_ANIM_MS: u64 = 220;
const SCRIM_ALPHA: f32 = 0.45;
/// How often an open sheet asks whether the settings document holding the
/// committed layout and the staged change moved on. It parses the document
/// again only when it did. Never asked while painting.
const DISPLAY_POLL: Duration = Duration::from_millis(250);

/// A settings file modified this recently may be modified again within the
/// same timestamp tick, so a read taken now is not trusted to stay current.
const RACY_STAMP: Duration = Duration::from_secs(1);

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "alpha is clamped to the u8 range"
)]
fn f32_to_u8_saturating(v: f32) -> u8 {
    v.clamp(0.0, 255.0) as u8
}

fn draw_scrim(scene: &mut Scene, transform: Affine, rect: Rect, openness: f32, theme: &Theme) {
    let alpha = f32_to_u8_saturating((SCRIM_ALPHA * openness.clamp(0.0, 1.0) * 255.0).round());
    if alpha == 0 {
        return;
    }
    let c = theme.tokens.colors.surfaces.scrim.to_rgba8();
    scene.fill(
        Fill::NonZero,
        transform,
        Color::from_rgba8(c.r, c.g, c.b, alpha),
        None,
        &rect,
    );
}

/// Title key of a layout.
pub(crate) fn layout_title_key(mode: DisplayLayoutMode) -> &'static str {
    match mode {
        DisplayLayoutMode::Extend => "display-mode-extend",
        DisplayLayoutMode::Mirror => "display-mode-mirror",
        DisplayLayoutMode::BuiltinOnly => "display-mode-builtin-only",
        DisplayLayoutMode::ExternalOnly => "display-mode-external-only",
    }
}

fn layout_detail_key(mode: DisplayLayoutMode) -> &'static str {
    match mode {
        DisplayLayoutMode::Extend => "display-mode-extend-detail",
        DisplayLayoutMode::Mirror => "display-mode-mirror-detail",
        DisplayLayoutMode::BuiltinOnly => "display-mode-builtin-only-detail",
        DisplayLayoutMode::ExternalOnly => "display-mode-external-only-detail",
    }
}

fn unavailable_key(error: &DisplayLayoutError) -> &'static str {
    match error {
        DisplayLayoutError::NoBuiltin => "display-mode-unavailable-builtin",
        DisplayLayoutError::NoCommonMirrorMode => "display-mode-unavailable-mirror",
        DisplayLayoutError::SingleDisplay
        | DisplayLayoutError::NoHardwareSnapshot
        | DisplayLayoutError::Invalid(_) => "display-mode-unavailable-single",
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

/// A display change still waiting to be applied, kept or reverted.
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
        use crate::shell::search::providers::actions as ids;
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
    /// Generation of the published display snapshot `outputs` came from.
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

/// One of the person's other devices' screens, as the sheet lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteDisplay {
    device: String,
    output: String,
    name: String,
    number: usize,
    revision: u64,
    device_label: String,
    online: bool,
    /// Its device shows something on it; a connected screen switched off
    /// there is listed so its place in the layout can still be chosen.
    active: bool,
    /// Takes part in the shared layout.
    enabled: bool,
}

impl RemoteDisplay {
    fn title(&self) -> String {
        t_args(
            "display-remote-title",
            &[
                ("number", self.number.to_string().into()),
                ("name", self.name.clone().into()),
                ("device", self.device_label.clone().into()),
            ],
        )
    }

    fn detail(&self) -> String {
        t(match (self.online, self.enabled, self.active) {
            (false, false, _) => "display-remote-offline-excluded",
            (false, true, _) => "display-remote-offline",
            (true, false, _) => "display-remote-out-of-layout",
            (true, true, false) => "display-remote-switched-off",
            (true, true, true) => "display-remote-in-layout",
        })
    }
}

impl DisplayLayouts {
    fn refresh_person(&mut self) {
        self.person = shared_display_actions::published();
    }

    /// The other devices' screens in display order.
    fn remote(&self) -> Vec<RemoteDisplay> {
        let Some(view) = self.person.as_ref() else {
            return Vec::new();
        };
        view.layout
            .listed()
            .filter(|display| !display.local)
            .map(|display| RemoteDisplay {
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
        // A file stamped just now can change again within the same tick
        // and keep its stamp; read it once more on the next poll.
        let racy = matches!(stamp, Some(settings_store::SettingsStamp::Modified(at))
            if std::time::SystemTime::now().duration_since(at).is_ok_and(|age| age < RACY_STAMP));
        self.settings_seen = if racy {
            SettingsSeen::Unread
        } else {
            SettingsSeen::At(stamp)
        };
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
                    // Show the stored change, with its Keep and Revert.
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

/// Keyboard focus inside the sheet, in visual order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetFocus {
    Layout(DisplayLayoutMode),
    Keep,
    Revert,
    /// One of the other devices' screens, by its position in the list.
    Remote(usize),
    Receivers,
}

/// Result of routing an input event through the open sheet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventOutcome {
    pub consumed: bool,
    pub close_requested: bool,
    pub invalidate: bool,
}

impl EventOutcome {
    fn consumed(invalidate: bool) -> Self {
        Self {
            consumed: true,
            close_requested: false,
            invalidate,
        }
    }

    fn close() -> Self {
        Self {
            consumed: true,
            close_requested: true,
            invalidate: true,
        }
    }
}

pub struct ProjectionOverlay {
    controller: ProjectionController,
    authority: Option<crate::shell::chrome_state::Chrome>,
    displays: DisplayLayouts,
    focus: SheetFocus,
    transition: ui::Transition,
    was_open: bool,
    /// Whether the open-time state (display snapshot, initial focus) was
    /// taken for the current opening. Events can arrive before the first
    /// frame, so both paths take it.
    session_open: bool,
    host_rect: Rect,
    layout_rects: [Rect; 4],
    keep_rect: Rect,
    revert_rect: Rect,
    remote_rects: Vec<(RemoteDisplay, Rect)>,
    remote_scroll: ui::widget::ScrollRegion,
    remote_theme: Option<Theme>,
    remote_focus_seen: Option<usize>,
    device_rects: Vec<Rect>,
    refresh_rect: Rect,
    close_rect: Rect,
    audio_rect: Rect,
    adaptive_rect: Rect,
    resolution_rect: Rect,
    fps_rect: Rect,
    credential_input: TextInput,
    credential_rect: Rect,
    /// A display choice made on the sheet, for the host to invoke as its
    /// published action.
    requested: Option<DisplayRequest>,
}

impl Default for ProjectionOverlay {
    fn default() -> Self {
        Self::from_controller(ProjectionController::default())
    }
}

impl ProjectionOverlay {
    pub(crate) fn from_controller(controller: ProjectionController) -> Self {
        Self {
            controller,
            authority: None,
            displays: DisplayLayouts::default(),
            focus: SheetFocus::Layout(DisplayLayoutMode::Extend),
            transition: ui::Transition::closed(),
            was_open: false,
            session_open: false,
            host_rect: Rect::ZERO,
            layout_rects: [Rect::ZERO; 4],
            keep_rect: Rect::ZERO,
            revert_rect: Rect::ZERO,
            remote_rects: Vec::new(),
            remote_scroll: ui::widget::ScrollRegion::vertical(),
            remote_theme: None,
            remote_focus_seen: None,
            device_rects: Vec::new(),
            refresh_rect: Rect::ZERO,
            close_rect: Rect::ZERO,
            audio_rect: Rect::ZERO,
            adaptive_rect: Rect::ZERO,
            resolution_rect: Rect::ZERO,
            fps_rect: Rect::ZERO,
            credential_input: TextInput::local()
                .with_id("projection-airplay-credential")
                .with_placeholder(t("wireless-credential-placeholder"))
                .with_secret(true)
                .with_enter_submits(true),
            credential_rect: Rect::ZERO,
            requested: None,
        }
    }

    pub(crate) fn bind_shared_display_authority(
        &mut self,
        chrome: crate::shell::chrome_state::Chrome,
    ) {
        self.authority = Some(chrome);
    }

    fn shared_display_editable(&self) -> bool {
        self.authority.as_ref().is_some_and(|chrome| {
            crate::shell::search::providers::actions::current_shell_action_authorized(
                &chrome.borrow(),
                shared_display_actions::INCLUDE_ID,
            )
        })
    }

    pub fn controller(&self) -> &ProjectionController {
        &self.controller
    }

    pub fn capture_requested(&self) -> bool {
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

    pub fn contains(host_rect: Rect, point: Point) -> bool {
        Self::rect_for_host(host_rect).contains(point)
    }

    /// Where the sheet shows the `AirPlay` PIN or password field, while it
    /// does, covering every pose of the sheet's slide-in. A capture draws it
    /// as the placeholder — see [`crate::shell::capture_protection`].
    pub fn secret_prompt_rect(&self, now: Instant) -> Option<Rect> {
        if !self.transition.is_visible(now)
            || !self.airplay_selected()
            || self.credential_rect.area() <= 0.0
        {
            return None;
        }
        // `draw_at_theme_scale` slides the sheet in from the right by up to
        // this much.
        let slide = self.sheet_rect().width() * 0.22;
        let field = self.credential_rect.inflate(2.0, 2.0);
        Some(Rect::new(field.x0, field.y0, field.x1 + slide, field.y1))
    }

    /// The sheet's part of the ordinary host update: take the open-time
    /// state when it opens, drain the receiver worker and the display
    /// actions' receipts (even at rest), and, while open, take up what the
    /// compositor published and what changed in settings. Painting reads
    /// none of it again. Waiting itself is not animation; the existing native
    /// wake schedules this update and only a change invalidates the sheet.
    pub(crate) fn poll_storage(&mut self, open: bool) -> bool {
        let opened = open && !self.session_open;
        self.sync_open_state(open);
        let mut changed = opened | self.controller.poll();
        changed |= if open {
            self.poll_displays(Instant::now())
        } else {
            self.displays.poll_completions()
        };
        changed
    }

    /// The display choice made on the sheet since the last look, for the
    /// host to invoke as its published action.
    pub(crate) fn take_request(&mut self) -> Option<DisplayRequest> {
        self.requested.take()
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

    /// Whether the display change an action opened this sheet for, `id`,
    /// has settled, so the sheet that only showed it can close.
    pub(crate) fn followed_change_settled(&mut self, id: u64, now: Instant) -> bool {
        self.displays.settled(id, now)
    }

    pub fn needs_animation_frame(&self, now: Instant) -> bool {
        self.transition.is_animating(now)
            || matches!(
                self.controller.status,
                ProjectionStatus::Scanning | ProjectionStatus::Connecting { .. }
            )
            || (self.was_open && self.remote_theme.as_ref().is_some_and(|theme|
                self.remote_scroll.needs_animation_frame(theme)))
            // The Keep/Revert countdown ticks while the sheet is open.
            || (self.was_open && self.displays.pending().is_some())
            // The change an action opened the sheet for read as settled: one
            // more host update closes the sheet.
            || self.followed_change_read_as_settled()
    }

    fn followed_change_read_as_settled(&self) -> bool {
        let Some(state) = self
            .authority
            .as_ref()
            .and_then(|chrome| chrome.try_borrow().ok())
        else {
            return false;
        };
        let crate::shell::chrome_state::ProjectionFollow::Transaction(id) = state.projection_follow
        else {
            return false;
        };
        state.overlays.projection && self.displays.read_as_settled(id)
    }

    /// The layout the sheet highlights as current: the physical layout when
    /// two or more displays are attached, else the wireless stream layout.
    fn active_layout(&self) -> Option<DisplayLayoutMode> {
        if self.displays.multiple_displays() {
            self.displays.active()
        } else if self.controller.devices.is_empty() {
            None
        } else {
            Some(layout_for_wireless(self.controller.options.mode))
        }
    }

    /// Whether choosing `mode` does anything: it can be planned for the
    /// attached displays, or the selected receiver can stream it.
    fn layout_enabled(&self, mode: DisplayLayoutMode) -> bool {
        if self.displays.staging.is_some() {
            return false;
        }
        self.displays.plan_error(mode).is_none()
            || (!self.controller.devices.is_empty()
                && wireless_mode_for(mode)
                    .is_some_and(|wireless| self.controller.selected_device_supports(wireless)))
    }

    fn enabled_layouts(&self) -> Vec<DisplayLayoutMode> {
        DisplayLayoutMode::ALL
            .into_iter()
            .filter(|mode| self.layout_enabled(*mode))
            .collect()
    }

    /// Take the display snapshot and put focus on the current layout the
    /// first time an opening is seen; forget it once closed.
    fn sync_open_state(&mut self, open: bool) {
        if open && !self.session_open {
            self.session_open = true;
            self.displays.notice = None;
            self.displays.staged = None;
            self.displays.refresh(Instant::now());
            self.controller.refresh();
            let initial = self
                .active_layout()
                .or_else(|| self.enabled_layouts().first().copied());
            // With no layout to choose on this device's own screens, the
            // first choice is the other devices' screens.
            self.focus = match initial {
                Some(mode) => SheetFocus::Layout(mode),
                None if !self.displays.remote().is_empty() => SheetFocus::Remote(0),
                None => SheetFocus::Layout(DisplayLayoutMode::Extend),
            };
        } else if !open {
            self.session_open = false;
        }
    }

    /// Super+P while open: move the highlight to the next layout that can
    /// be chosen, wrapping around, starting from the highlighted one.
    fn cycle_layout(&mut self) {
        let enabled = self.enabled_layouts();
        let from = match self.focus {
            SheetFocus::Layout(mode) => Some(mode),
            _ => self.active_layout(),
        };
        let next = from
            .and_then(|from| {
                DisplayLayoutMode::ALL
                    .iter()
                    .cycle()
                    .skip(from.index().saturating_add(1))
                    .take(DisplayLayoutMode::ALL.len())
                    .find(|mode| enabled.contains(mode))
                    .copied()
            })
            .or_else(|| enabled.first().copied());
        if let Some(next) = next {
            self.focus = SheetFocus::Layout(next);
        }
    }

    /// Choose `mode`: stage it on the attached displays when it applies to
    /// them, and make it the stream layout for wireless receivers.
    fn choose_layout(&mut self, mode: DisplayLayoutMode) {
        if !self.layout_enabled(mode) {
            return;
        }
        if let Some(wireless) = wireless_mode_for(mode)
            && (self.controller.devices.is_empty()
                || self.controller.selected_device_supports(wireless))
        {
            self.controller.options.mode = wireless;
        }
        if self.displays.plan_error(mode).is_none() {
            self.requested = Some(DisplayRequest::Layout(mode));
            self.focus = SheetFocus::Keep;
        } else {
            self.focus = SheetFocus::Layout(mode);
        }
    }

    fn confirming(&self) -> bool {
        self.displays.pending().is_some()
    }

    /// Route `event` to the sheet shown over `host_rect`. The outcome says
    /// whether the sheet used it; `ctx.handled` is left to the caller.
    pub fn handle_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        cursor: Option<Point>,
        host_rect: Rect,
        open: bool,
    ) -> EventOutcome {
        self.sync_open_state(open);
        if !open {
            return EventOutcome::default();
        }
        self.host_rect = host_rect;
        if self.airplay_selected()
            && let Some(outcome) = self.handle_credential_event(event, ctx, cursor)
        {
            return outcome;
        }
        let walks_rows = matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown),
                    ..
                },
                ..
            }
        );
        if !walks_rows && let Some(theme) = self.remote_theme.as_ref() {
            let previously_handled = *ctx.handled;
            let changed = self.remote_scroll.handle_event(event, ctx, theme);
            if changed || (!previously_handled && *ctx.handled) {
                return EventOutcome::consumed(changed);
            }
        }
        match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        repeat,
                        ..
                    },
                ..
            } => {
                if *repeat
                    && matches!(logical_key, Key::Character(s) if s.eq_ignore_ascii_case("p"))
                {
                    return EventOutcome::consumed(false);
                }
                self.handle_key(logical_key, *ctx.modifiers)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => cursor.map_or_else(
                || EventOutcome::consumed(false),
                |point| self.handle_click(point),
            ),
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => EventOutcome::consumed(false),
            _ => EventOutcome::default(),
        }
    }

    fn handle_credential_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut EventCtx<'_>,
        cursor: Option<Point>,
    ) -> Option<EventOutcome> {
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            self.credential_input
                .set_focus(cursor.is_some_and(|p| self.credential_rect.contains(p)));
        }
        let mut input_handled = false;
        let mut input_taffy = TaffyTree::<()>::new();
        let Ok(input_node) = input_taffy.new_leaf(Style::default()) else {
            return Some(EventOutcome::default());
        };
        self.credential_input.handle_event(
            event,
            &mut EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: &mut input_handled,
                focus_target: &mut *ctx.focus_target,
                node_id: input_node,
                taffy: &input_taffy,
            },
        );
        if let Some(value) = self.credential_input.take_dirty() {
            self.controller.options.credential = (!value.is_empty()).then_some(value);
        }
        if self.credential_input.take_submit() {
            self.controller.connect_selected();
            self.credential_input.set_text("");
            return Some(EventOutcome::consumed(true));
        }
        input_handled.then(|| EventOutcome::consumed(false))
    }

    /// Up/Down walk the sheet in visual order: layouts, Keep/Revert while a
    /// change waits for confirmation, the other devices' screens, then the
    /// receivers.
    fn move_focus(&mut self, down: bool) {
        let confirming = self.confirming();
        let last_layout = DisplayLayoutMode::ExternalOnly;
        let remote = self.displays.remote().len();
        // Where focus goes below the layouts and the confirmation band.
        let below = if remote > 0 {
            Some(SheetFocus::Remote(0))
        } else if self.controller.devices.is_empty() {
            None
        } else {
            Some(SheetFocus::Receivers)
        };
        // Where focus goes above the other devices' screens.
        let above = if confirming {
            SheetFocus::Keep
        } else {
            SheetFocus::Layout(last_layout)
        };
        let next = match (self.focus, down) {
            (SheetFocus::Layout(mode), true) if mode == last_layout => {
                if confirming {
                    SheetFocus::Keep
                } else {
                    below.unwrap_or(self.focus)
                }
            }
            (SheetFocus::Layout(mode), true) => DisplayLayoutMode::ALL
                .get(mode.index().saturating_add(1))
                .map_or(self.focus, |next| SheetFocus::Layout(*next)),
            (SheetFocus::Layout(mode), false) => mode
                .index()
                .checked_sub(1)
                .and_then(|index| DisplayLayoutMode::ALL.get(index))
                .map_or(self.focus, |previous| SheetFocus::Layout(*previous)),
            (SheetFocus::Keep | SheetFocus::Revert, true) => below.unwrap_or(self.focus),
            (SheetFocus::Keep | SheetFocus::Revert, false) => SheetFocus::Layout(last_layout),
            (SheetFocus::Remote(index), true) if index.saturating_add(1) < remote => {
                SheetFocus::Remote(index.saturating_add(1))
            }
            (SheetFocus::Remote(_), true) => {
                if self.controller.devices.is_empty() {
                    self.focus
                } else {
                    SheetFocus::Receivers
                }
            }
            (SheetFocus::Remote(index), false) => {
                index.checked_sub(1).map_or(above, SheetFocus::Remote)
            }
            (SheetFocus::Receivers, true) => {
                self.controller.move_selection(1);
                self.focus
            }
            (SheetFocus::Receivers, false) => {
                if self.controller.selected == 0 {
                    remote.checked_sub(1).map_or(above, SheetFocus::Remote)
                } else {
                    self.controller.move_selection(-1);
                    self.focus
                }
            }
        };
        if next == SheetFocus::Receivers && self.focus != SheetFocus::Receivers {
            self.controller.selected = 0;
        }
        self.focus = next;
    }

    /// Ask for Keep or Revert on the change waiting for one, unless an
    /// answer is already on its way.
    fn answer(&mut self, request: DisplayRequest) {
        if self.displays.answer.is_none() && self.displays.transaction.is_some() {
            self.requested = Some(request);
        }
    }

    fn poll_displays(&mut self, now: Instant) -> bool {
        let focused = if let SheetFocus::Remote(index) = self.focus {
            self.displays.remote().get(index).cloned()
        } else {
            None
        };
        let changed = self.displays.poll(now);
        if let Some(display) = focused {
            self.focus = self
                .displays
                .remote()
                .iter()
                .position(|row| row.device == display.device && row.output == display.output)
                .map_or(SheetFocus::Receivers, SheetFocus::Remote);
        }
        changed
    }

    fn toggle_remote(&mut self, index: usize) {
        if let Some(display) = self.displays.remote().get(index).cloned() {
            self.toggle_shared_output(&display);
        }
    }

    fn toggle_shared_output(&mut self, display: &RemoteDisplay) {
        if let Some(index) = self
            .displays
            .remote()
            .iter()
            .position(|row| row.device == display.device && row.output == display.output)
        {
            self.focus = SheetFocus::Remote(index);
        }
        let result = self
            .authority
            .as_ref()
            .ok_or_else(|| {
                crate::shell::search::action_api::ActionError::Refused(t(
                    "display-remote-unavailable",
                ))
            })
            .and_then(|chrome| {
                shared_display_actions::include(
                    &chrome.borrow(),
                    &display.device,
                    &display.output,
                    display.revision,
                    !display.enabled,
                )
            });
        match result {
            Ok(()) => {
                self.displays.refresh_person();
                self.displays.notice = None;
                if let Some(index) =
                    self.displays.remote().iter().position(|row| {
                        row.device == display.device && row.output == display.output
                    })
                {
                    self.focus = SheetFocus::Remote(index);
                }
            }
            Err(error) => self.displays.notice = Some(error.to_string()),
        }
    }

    fn activate_focus(&mut self) {
        match self.focus {
            SheetFocus::Layout(mode) => self.choose_layout(mode),
            SheetFocus::Keep => {
                self.answer(DisplayRequest::Keep);
                self.focus = self.active_layout().map_or(self.focus, SheetFocus::Layout);
            }
            SheetFocus::Revert => {
                self.answer(DisplayRequest::Revert);
                self.focus = self.active_layout().map_or(self.focus, SheetFocus::Layout);
            }
            SheetFocus::Remote(index) => self.toggle_remote(index),
            SheetFocus::Receivers => {
                let selected_id = self
                    .controller
                    .devices
                    .get(self.controller.selected)
                    .map(|device| device.id.as_str());
                if matches!(
                    &self.controller.status,
                    ProjectionStatus::Connected { device_id }
                        if Some(device_id.as_str()) == selected_id
                ) {
                    self.controller.disconnect();
                } else {
                    self.controller.connect_selected();
                }
            }
        }
    }

    fn handle_key(&mut self, key: &Key, modifiers: Modifiers) -> EventOutcome {
        let super_down = modifiers.state().super_key();
        let shift_down = modifiers.state().shift_key();
        match key {
            Key::Named(NamedKey::Escape) => return EventOutcome::close(),
            Key::Character(s) if super_down && s.eq_ignore_ascii_case("p") => {
                if shift_down {
                    // Super+Shift+P belongs to picture-in-picture capture.
                    return EventOutcome::close();
                }
                self.cycle_layout();
            }
            Key::Named(NamedKey::ArrowUp) => self.move_focus(false),
            Key::Named(NamedKey::ArrowDown) => self.move_focus(true),
            Key::Named(NamedKey::ArrowLeft) if self.focus == SheetFocus::Revert => {
                self.focus = SheetFocus::Keep;
            }
            Key::Named(NamedKey::ArrowRight) if self.focus == SheetFocus::Keep => {
                self.focus = SheetFocus::Revert;
            }
            Key::Named(NamedKey::Enter) => self.activate_focus(),
            Key::Named(NamedKey::Space) if matches!(self.focus, SheetFocus::Remote(_)) => {
                self.activate_focus();
            }
            Key::Named(NamedKey::Space) => {
                self.controller.options.audio = !self.controller.options.audio;
            }
            Key::Character(s) if s.eq_ignore_ascii_case("r") => self.controller.refresh(),
            Key::Character(s) if s.eq_ignore_ascii_case("a") => {
                self.controller.options.stream_policy.adaptive =
                    !self.controller.options.stream_policy.adaptive;
            }
            Key::Character(s) if s.eq_ignore_ascii_case("q") => self.cycle_resolution(),
            Key::Character(s) if s.eq_ignore_ascii_case("f") => self.toggle_fps(),
            Key::Character(s) if matches!(s.as_str(), "1" | "2" | "3" | "4") => {
                let index = s.parse::<usize>().unwrap_or(1).saturating_sub(1);
                if let Some(&mode) = DisplayLayoutMode::ALL.get(index) {
                    self.choose_layout(mode);
                }
            }
            _ => {}
        }
        EventOutcome::consumed(true)
    }

    fn handle_click(&mut self, point: Point) -> EventOutcome {
        if self.close_rect.contains(point) || !self.sheet_rect().contains(point) {
            return EventOutcome::close();
        } else if self.keep_rect.contains(point) {
            self.focus = SheetFocus::Keep;
            self.activate_focus();
        } else if self.revert_rect.contains(point) {
            self.focus = SheetFocus::Revert;
            self.activate_focus();
        } else if let Some(index) = self
            .layout_rects
            .iter()
            .position(|rect| rect.area() > 0.0 && rect.contains(point))
        {
            if let Some(&mode) = DisplayLayoutMode::ALL.get(index) {
                self.focus = SheetFocus::Layout(mode);
                self.choose_layout(mode);
            }
        } else if let Some(display) = self
            .remote_rects
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(display, _)| display.clone())
        {
            self.toggle_shared_output(&display);
        } else if self.refresh_rect.contains(point) {
            self.controller.refresh();
        } else if self.audio_rect.contains(point) {
            self.controller.options.audio = !self.controller.options.audio;
        } else if self.adaptive_rect.contains(point) {
            self.controller.options.stream_policy.adaptive =
                !self.controller.options.stream_policy.adaptive;
        } else if self.resolution_rect.contains(point) {
            self.cycle_resolution();
        } else if self.fps_rect.contains(point) {
            self.toggle_fps();
        } else if let Some(index) = self
            .device_rects
            .iter()
            .position(|rect| rect.contains(point))
        {
            self.click_device(index);
        }
        EventOutcome::consumed(true)
    }

    fn click_device(&mut self, index: usize) {
        self.focus = SheetFocus::Receivers;
        let selection_changed = self.controller.selected != index;
        self.controller.selected = index;
        if selection_changed {
            self.controller.options.credential = None;
            self.credential_input.set_text("");
        }
        let id = self
            .controller
            .devices
            .get(index)
            .map(|device| device.id.as_str());
        if matches!(
            &self.controller.status,
            ProjectionStatus::Connected { device_id } if Some(device_id.as_str()) == id
        ) {
            self.controller.disconnect();
        } else if !selection_changed
            || self
                .controller
                .devices
                .get(index)
                .is_none_or(|device| device.protocol != ProjectionProtocol::AirPlay)
        {
            self.controller.connect_selected();
            self.credential_input.set_text("");
        } else {
            self.credential_input.set_focus(true);
        }
    }

    fn cycle_resolution(&mut self) {
        let limits = DisplayResolutionLimit::ALL;
        let current = limits
            .iter()
            .position(|limit| *limit == self.controller.options.stream_policy.resolution_limit)
            .unwrap_or(0);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "max(1) keeps the divisor nonzero"
        )]
        let next = current.wrapping_add(1) % limits.len().max(1);
        if let Some(&limit) = limits.get(next) {
            self.controller.options.stream_policy.resolution_limit = limit;
        }
    }

    fn toggle_fps(&mut self) {
        self.controller.options.stream_policy.max_fps =
            if self.controller.options.stream_policy.max_fps == 60 {
                30
            } else {
                60
            };
    }

    /// Semantic tree of the open sheet: one button per layout (selected is
    /// the highlighted one, checked is the current one), Keep and Revert
    /// while a change waits, and one button per receiver.
    pub fn accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};

        let mut sheet = AccessNode::new(Role::Dialog)
            .with_name(t("display-sheet-title"))
            .with_description(
                if self.displays.staging.is_some() {
                    Some(t("display-sheet-saving-layout"))
                } else {
                    // Why the last choice was refused is said, not only shown.
                    self.displays
                        .rejection()
                        .filter(|reason| !reason.is_empty())
                }
                .map_or_else(
                    || t("display-sheet-a11y-description"),
                    |status| format!("{status}. {}", t("display-sheet-a11y-description")),
                ),
            )
            .with_bounds(self.sheet_rect())
            .with_state(AccessState::new().modal(true));
        let active = self.active_layout();
        for (index, mode) in DisplayLayoutMode::ALL.into_iter().enumerate() {
            let description = self.displays.plan_error(mode).map_or_else(
                || t(layout_detail_key(mode)),
                |error| {
                    if self.layout_enabled(mode) {
                        t(layout_detail_key(mode))
                    } else {
                        t(unavailable_key(&error))
                    }
                },
            );
            sheet.children.push(
                AccessNode::new(Role::Button)
                    .with_name(t(layout_title_key(mode)))
                    .with_description(description)
                    .with_bounds(self.layout_rects.get(index).copied().unwrap_or_default())
                    .with_state(
                        AccessState::new()
                            .focused(self.focus == SheetFocus::Layout(mode))
                            .selected(self.focus == SheetFocus::Layout(mode))
                            .checked(active == Some(mode))
                            .disabled(!self.layout_enabled(mode)),
                    ),
            );
        }
        if self.confirming() {
            for (focus, key, rect) in [
                (SheetFocus::Keep, "display-confirm-keep", self.keep_rect),
                (
                    SheetFocus::Revert,
                    "display-confirm-revert",
                    self.revert_rect,
                ),
            ] {
                sheet.children.push(
                    AccessNode::new(Role::Button)
                        .with_name(t(key))
                        .with_description(t("display-confirm-title"))
                        .with_bounds(rect)
                        .with_state(
                            AccessState::new()
                                .focused(self.focus == focus)
                                .disabled(self.displays.answer.is_some()),
                        ),
                );
            }
        }
        for (index, display) in self.displays.remote().iter().enumerate() {
            sheet
                .children
                .push(self.remote_accessibility(index, display));
        }
        for (index, device) in self.controller.devices.iter().enumerate() {
            sheet.children.push(
                AccessNode::new(Role::Button)
                    .with_name(device.name.clone())
                    .with_description(device.protocol.label())
                    .with_bounds(self.device_rects.get(index).copied().unwrap_or_default())
                    .with_state(AccessState::new().focused(
                        self.focus == SheetFocus::Receivers && self.controller.selected == index,
                    )),
            );
        }
        sheet
    }

    fn remote_accessibility(&self, index: usize, display: &RemoteDisplay) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        AccessNode::new(Role::Switch)
            .with_name(display.title())
            .with_description(display.detail())
            .with_semantic_key(format!(
                "shared-output:{}:{}{}",
                display.device.len(),
                display.device,
                display.output
            ))
            .with_bounds(
                self.remote_rects
                    .iter()
                    .find(|(painted, _)| {
                        painted.device == display.device && painted.output == display.output
                    })
                    .map_or(Rect::ZERO, |(_, rect)| *rect),
            )
            .with_state(
                AccessState::new()
                    .focused(self.focus == SheetFocus::Remote(index))
                    .checked(display.enabled)
                    .disabled(!self.shared_display_editable()),
            )
    }

    /// Paint the complete sheet from the state the host update took. Reads
    /// no storage and changes no state but its own geometry.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        host_rect: Rect,
        open: bool,
    ) {
        // Theme type tokens already carry the user's text scale, and this
        // sheet lays out and paints with those resolved sizes; the renderer
        // must not scale them a second time (HIG §7a), so it is reset for
        // the pass exactly as the lens pass resets it, and restored after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.draw_at_theme_scale(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            host_rect,
            open,
        );
        text_renderer.set_text_scale(renderer_scale);
    }

    fn draw_at_theme_scale(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        host_rect: Rect,
        open: bool,
    ) {
        self.host_rect = host_rect;
        self.was_open = open;
        let now = Instant::now();
        self.transition.set_open(
            open,
            if theme.reduce_motion {
                Duration::ZERO
            } else {
                Duration::from_millis(OPEN_ANIM_MS)
            },
            ui::Curve::EaseOut,
        );
        if self.transition.is_visible(now) {
            let openness = self.transition.value(now);
            draw_scrim(scene, transform, host_rect, openness, theme);
            let sheet = self.sheet_rect();
            let slide = (1.0 - f64::from(openness)) * (sheet.width() * 0.22);
            let sheet_transform = transform.pre_translate((slide, 0.0).into());
            if openness < 1.0 {
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    openness,
                    transform,
                    &host_rect,
                );
            }
            self.draw_content(scene, sheet_transform, text_renderer, theme, presence);
            if openness < 1.0 {
                scene.pop_layer();
            }
        } else {
            self.clear_geometry();
        }
    }

    fn clear_geometry(&mut self) {
        self.layout_rects = [Rect::ZERO; 4];
        self.keep_rect = Rect::ZERO;
        self.revert_rect = Rect::ZERO;
        self.remote_rects.clear();
        self.remote_theme = None;
        self.remote_focus_seen = None;
        self.device_rects.clear();
        self.refresh_rect = Rect::ZERO;
        self.close_rect = Rect::ZERO;
        self.audio_rect = Rect::ZERO;
        self.adaptive_rect = Rect::ZERO;
        self.resolution_rect = Rect::ZERO;
        self.fps_rect = Rect::ZERO;
        self.credential_rect = Rect::ZERO;
    }

    /// Height of the Keep/Revert band: a title line and a status line with
    /// the two buttons beside them.
    fn confirm_band_height() -> f64 {
        crate::shell::display_confirm::band_height()
    }

    /// Receiver rows are 56 px with an 8 px gap between them.
    fn device_row_pitch() -> f64 {
        64.0
    }

    fn rect_for_host(host_rect: Rect) -> Rect {
        let margin = 20.0;
        let width = 432.0_f64.min((host_rect.width() - margin * 2.0).max(280.0));
        let top = host_rect.y0 + 56.0;
        let bottom = (top + 780.0).min(host_rect.y1 - 24.0);
        Rect::new(
            host_rect.x1 - margin - width,
            top,
            host_rect.x1 - margin,
            bottom,
        )
    }

    fn sheet_rect(&self) -> Rect {
        Self::rect_for_host(self.host_rect)
    }

    fn layout_row_rect(&self, index: usize) -> Rect {
        let r = self.sheet_rect();
        // Rows of 52 px on a 58 px pitch, below the 80 px header.
        let y0 = r.y0 + 80.0 + f64_of_usize(index) * 58.0;
        Rect::new(r.x0 + 16.0, y0, r.x1 - 16.0, y0 + 52.0)
    }

    /// Top of the confirmation band, directly under the layout rows.
    fn confirm_top(&self) -> f64 {
        self.layout_row_rect(DisplayLayoutMode::ALL.len().saturating_sub(1))
            .y1
            + 10.0
    }

    /// Top of the other devices' screens, after any display confirmation.
    fn remote_top(&self) -> f64 {
        let band = if self.displays.staging.is_some()
            || self.confirming()
            || self.displays.rejection().is_some()
        {
            Self::confirm_band_height() + 10.0
        } else {
            0.0
        };
        self.confirm_top() + band + 8.0
    }

    fn remote_spacing(&self) -> ui::tokens::Spacing {
        self.remote_theme
            .as_ref()
            .map_or(ui::tokens::Spacing::BASE, |theme| theme.tokens.spacing)
    }

    fn remote_metrics(&self) -> ui::tokens::Metrics {
        self.remote_theme
            .as_ref()
            .map_or(ui::tokens::Metrics::BASE, |theme| theme.tokens.metrics)
    }

    fn remote_row_pitch(&self) -> f64 {
        f64::from(self.remote_metrics().panel_row_height + self.remote_spacing().s2)
    }

    /// Bound the shared-output viewport; the shared scroll region retains every row.
    fn remote_viewport(&self) -> Rect {
        let r = self.sheet_rect();
        let spacing = self.remote_spacing();
        let heading = f64::from(self.remote_metrics().menu_row_height);
        let gutter = f64::from(spacing.s3);
        let pitch = self.remote_row_pitch();
        let footer = f64::from(self.remote_metrics().menu_row_height + spacing.s2);
        let available = (r.y1
            - self.remote_top()
            - heading
            - gutter
            - self.wireless_controls_height()
            - Self::device_row_pitch()
            - footer)
            .max(pitch);
        let height = (f64_of_usize(self.displays.remote().len().min(3)) * pitch).min(available);
        Rect::new(
            r.x0 + gutter,
            self.remote_top() + heading,
            r.x1 - gutter,
            self.remote_top() + heading + height,
        )
    }

    fn remote_row_rect(&self, index: usize) -> Rect {
        let y0 = f64_of_usize(index) * self.remote_row_pitch();
        Rect::new(
            0.0,
            y0,
            self.remote_scroll.content_rect().width(),
            y0 + f64::from(self.remote_metrics().panel_row_height),
        )
    }

    /// Wireless receivers keep their section below the bounded shared-output viewport.
    fn wireless_top(&self) -> f64 {
        if self.displays.remote().is_empty() {
            self.remote_top()
        } else {
            self.remote_viewport().y1 + f64::from(self.remote_spacing().s3)
        }
    }

    fn row_rect(&self, index: usize) -> Rect {
        let r = self.sheet_rect();
        let y0 = self.device_rows_top() + f64_of_usize(index) * Self::device_row_pitch();
        Rect::new(r.x0 + 16.0, y0, r.x1 - 16.0, y0 + 56.0)
    }

    fn airplay_selected(&self) -> bool {
        self.controller
            .devices
            .get(self.controller.selected)
            .is_some_and(|device| {
                device.protocol == crate::shell::projection::ProjectionProtocol::AirPlay
            })
    }

    fn wireless_controls_height(&self) -> f64 {
        if self.airplay_selected() {
            214.0
        } else {
            118.0
        }
    }

    fn device_rows_top(&self) -> f64 {
        self.wireless_top() + self.wireless_controls_height()
    }

    fn draw_content(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let r = self.sheet_rect();
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;

        self.draw_header(scene, transform, text_renderer, theme, r);
        self.draw_layout_rows(scene, transform, text_renderer, theme);
        self.draw_confirm_band(scene, transform, text_renderer, theme, r);
        self.draw_remote_displays(scene, transform, text_renderer, theme, r);
        self.draw_devices_heading(scene, transform, text_renderer, theme, r);
        self.draw_audio_toggle(scene, transform, text_renderer, theme, r);
        self.draw_stream_chips(scene, transform, text_renderer, theme, r);
        self.draw_credential_field(scene, transform, text_renderer, theme, presence, r);

        self.device_rects.clear();
        let rows_top = self.device_rows_top();
        let max_rows = usize_of_f64((r.y1 - rows_top - 40.0) / Self::device_row_pitch()).max(1);
        if self.controller.devices.is_empty() {
            self.draw_devices_empty(scene, transform, text_renderer, theme, r);
        } else {
            self.draw_devices_list(scene, transform, text_renderer, theme, max_rows);
        }

        let footer = t("wireless-protocols");
        let (fw, _) =
            text_renderer.measure_style(&footer, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &footer,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(fw) / 2.0, r.y1 - 28.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
        );
    }

    /// The sheet's card, title, subtitle and close button.
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let radius = ui::chrome::ChromeRole::Panel.radius(theme, r.height());
        let card = RoundedRect::from_rect(r, radius);
        ui::chrome::shell::panel_shadow(scene, transform, r, radius, theme, 1.0);
        ui::floating_surface::paint_material(
            scene,
            transform,
            &card,
            ui::floating_surface::Material::Floating,
            theme,
        );
        // Panel edge derived against the ground (HIG §9.5), not declared.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::shell::panel_border(theme),
            None,
            &card,
        );

        text_renderer.draw_style(
            scene,
            &t("display-sheet-title"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 18.0).into()),
            None,
            &type_scale.style(TextRole::Title),
        );
        text_renderer.draw_style(
            scene,
            &t("display-sheet-subtitle"),
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, r.y0 + 49.0).into()),
            Some(f32_of_f64(r.width() - 88.0)),
            &type_scale.style(TextRole::Caption),
        );

        self.close_rect = Rect::new(r.x1 - 48.0, r.y0 + 12.0, r.x1 - 12.0, r.y0 + 48.0);
        ui::icons::draw_icon(
            scene,
            transform,
            self.close_rect.inset(-9.0),
            ui::icons::Icon::X,
            colors.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    /// The four desktop layouts as a vertical list, each with a two-screen
    /// pictogram, its name and what it does (or why it cannot be chosen).
    fn draw_layout_rows(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        // Control role (HIG §9.1/§9.3): the highlighted layout is a lift with
        // the tier's hairline, never an accent wash (the accent means live
        // state, §9.4); idle rows rest on the lift floor with an edge
        // derived from that floor (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        let active = self.active_layout();
        for (index, mode) in DisplayLayoutMode::ALL.into_iter().enumerate() {
            let row = self.layout_row_rect(index);
            if let Some(slot) = self.layout_rects.get_mut(index) {
                *slot = row;
            }
            let enabled = self.layout_enabled(mode);
            let highlighted = self.focus == SheetFocus::Layout(mode);
            let radius = ui::chrome::ChromeRole::Control.radius(theme, row.height());
            let shape = RoundedRect::from_rect(row, radius);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(highlighted));
            scene.fill(
                Fill::NonZero,
                transform,
                if highlighted { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(row, radius),
            );
            if highlighted {
                ui::widget::paint_focus_ring(scene, transform, row, radius, theme);
            }
            let pictogram = Rect::new(row.x0 + 12.0, row.y0 + 14.0, row.x0 + 56.0, row.y1 - 14.0);
            draw_layout_pictogram(scene, transform, pictogram, mode, theme, enabled);
            let text_x = pictogram.x1 + 14.0;
            let text_w = f32_of_f64(row.x1 - text_x - 44.0);
            text_renderer.draw_style(
                scene,
                &t(layout_title_key(mode)),
                if enabled {
                    colors.text_primary
                } else {
                    colors.text_dim.with_alpha_factor(0.62)
                },
                transform.pre_translate((text_x, row.y0 + 8.0).into()),
                Some(text_w),
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            let detail = if enabled {
                t(layout_detail_key(mode))
            } else {
                self.displays
                    .plan_error(mode)
                    .map_or_else(|| t(layout_detail_key(mode)), |e| t(unavailable_key(&e)))
            };
            let detail = text_renderer.elide_to_width_style(
                &detail,
                text_w,
                &type_scale.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &detail,
                colors
                    .text_dim
                    .with_alpha_factor(if enabled { 1.0 } else { 0.62 }),
                transform.pre_translate((text_x, row.y0 + 29.0).into()),
                None,
                &type_scale.style(TextRole::Caption),
            );
            if active == Some(mode) {
                ui::icons::draw_icon(
                    scene,
                    transform,
                    Rect::new(
                        row.x1 - 34.0,
                        row.center().y - 9.0,
                        row.x1 - 16.0,
                        row.center().y + 9.0,
                    ),
                    ui::icons::Icon::Check,
                    colors.semantic.success,
                    ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                );
            }
        }
    }

    /// Keep/Revert with the restore countdown while a staged layout waits
    /// for confirmation, or why the last choice was refused.
    fn draw_confirm_band(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        self.keep_rect = Rect::ZERO;
        self.revert_rect = Rect::ZERO;
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let top = self.confirm_top();
        let band = Rect::new(
            r.x0 + 16.0,
            top,
            r.x1 - 16.0,
            top + Self::confirm_band_height(),
        );
        if self.displays.staging.is_some() {
            text_renderer.draw_style(
                scene,
                &t("display-sheet-saving-layout"),
                colors.text_dim,
                transform.pre_translate((band.x0 + 4.0, band.y0 + 8.0).into()),
                Some(f32_of_f64(band.width() - 8.0)),
                &type_scale.style(TextRole::Caption),
            );
        } else if self.confirming() {
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
            let focus = match self.focus {
                SheetFocus::Keep => Some(crate::shell::display_confirm::BandFocus::Keep),
                SheetFocus::Revert => Some(crate::shell::display_confirm::BandFocus::Revert),
                SheetFocus::Layout(_) | SheetFocus::Remote(_) | SheetFocus::Receivers => None,
            };
            let buttons = crate::shell::display_confirm::paint_keep_revert_band(
                scene,
                transform,
                text_renderer,
                theme,
                band,
                &t("display-confirm-title"),
                &status,
                crate::shell::display_confirm::BandState {
                    focus,
                    disabled: self.displays.answer.is_some(),
                },
            );
            self.keep_rect = buttons.keep;
            self.revert_rect = buttons.revert;
        } else if let Some(reason) = self.displays.rejection() {
            let message = t_args("display-rejected", &[("reason", ArgValue::Text(reason))]);
            text_renderer.draw_style(
                scene,
                &message,
                colors.semantic.danger,
                transform.pre_translate((band.x0 + 4.0, band.y0 + 8.0).into()),
                Some(f32_of_f64(band.width() - 8.0)),
                &type_scale.style(TextRole::Caption),
            );
        }
    }

    /// Shared output rows use retained-control faces and the shared scroll engine.
    fn draw_remote_displays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        self.remote_rects.clear();
        let remote = self.displays.remote();
        if remote.is_empty() {
            return;
        }
        self.remote_theme = Some(theme.clone());
        text_renderer.draw_style(
            scene,
            &t("display-remote-heading"),
            theme.tokens.colors.text_primary,
            transform.pre_translate(
                (r.x0 + f64::from(theme.tokens.spacing.s3), self.remote_top()).into(),
            ),
            None,
            &theme.text(TextRole::Label),
        );
        self.remote_scroll
            .set_viewport(self.remote_viewport(), theme);
        self.remote_scroll.set_content_size(
            self.remote_viewport().width(),
            f64_of_usize(remote.len()) * self.remote_row_pitch(),
        );
        let focused = if let SheetFocus::Remote(index) = self.focus {
            Some(index)
        } else {
            None
        };
        self.remote_scroll.set_focused(focused.is_some());
        if focused != self.remote_focus_seen
            && let Some(index) = focused
        {
            self.remote_scroll
                .scroll_into_view(self.remote_row_rect(index));
        }
        self.remote_focus_seen = focused;
        let content_transform = self.remote_scroll.content_transform(Affine::IDENTITY);
        let viewport = self.remote_scroll.content_rect();
        let rows = (0..remote.len())
            .map(|index| self.remote_row_rect(index))
            .collect::<Vec<_>>();
        self.remote_rects = remote
            .iter()
            .zip(&rows)
            .map(|(display, row)| {
                (
                    display.clone(),
                    content_transform
                        .transform_rect_bbox(*row)
                        .intersect(viewport),
                )
            })
            .collect();
        let disabled = !self.shared_display_editable();
        self.remote_scroll
            .paint(scene, transform, theme, |scene, transform| {
                for (index, (display, row)) in remote.iter().zip(&rows).enumerate() {
                    Self::paint_remote_row(
                        scene,
                        transform,
                        text_renderer,
                        theme,
                        *row,
                        display,
                        ui::widget::ToggleInteract {
                            focused: focused == Some(index),
                            disabled,
                            ..Default::default()
                        },
                    );
                }
            });
    }

    fn paint_remote_row(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        display: &RemoteDisplay,
        interact: ui::widget::ToggleInteract,
    ) {
        let title = t_args(
            "display-remote-row",
            &[
                ("title", display.title().into()),
                ("status", display.detail().into()),
            ],
        );
        let toggle_width =
            ui::widget::ToggleFace::size(theme).0 + f64::from(theme.tokens.spacing.s3);
        let label_rect = Rect::new(row.x0, row.y0, row.x1 - toggle_width, row.y1);
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(label_rect, &title)
                .role(ButtonRole::Plain)
                .icon(ui::icons::Icon::Monitor)
                .focused(interact.focused)
                .disabled(interact.disabled),
        );
        ui::widget::Toggle::paint(
            scene,
            transform,
            theme,
            &ui::widget::ToggleFace::new(
                Rect::new(label_rect.x1, row.y0, row.x1, row.y1),
                display.enabled,
            )
            .focused(interact.focused)
            .disabled(interact.disabled),
        );
    }

    /// "Wireless displays" heading plus the refresh button beside it.
    fn draw_devices_heading(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let heading_y = self.wireless_top();
        text_renderer.draw_style(
            scene,
            &t("wireless-heading"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 20.0, heading_y).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Semibold),
        );
        self.refresh_rect = Rect::new(r.x1 - 48.0, heading_y - 8.0, r.x1 - 12.0, heading_y + 28.0);
        ui::icons::draw_icon(
            scene,
            transform,
            self.refresh_rect.inset(-9.0),
            ui::icons::Icon::Refresh,
            colors.text_dim,
            1.5,
        );
    }

    /// The "Share audio" row: glyph, label and an on/off toggle knob.
    fn draw_audio_toggle(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let top = self.wireless_top() + 32.0;
        self.audio_rect = Rect::new(r.x0 + 20.0, top, r.x1 - 62.0, top + 38.0);
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::new(r.x0 + 22.0, top + 9.0, r.x0 + 42.0, top + 29.0),
            if self.controller.options.audio {
                ui::icons::Icon::Volume2
            } else {
                ui::icons::Icon::VolumeX
            },
            colors.text_dim,
            1.5,
        );
        text_renderer.draw_style(
            scene,
            &t("wireless-share-audio"),
            colors.text_primary,
            transform.pre_translate((r.x0 + 52.0, top + 10.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
        );
        let toggle = Rect::new(r.x1 - 58.0, top + 6.0, r.x1 - 20.0, top + 28.0);
        // The shared switch (HIG §9.9): track, knob and their palette come
        // from `Toggle`, not a hand-rolled accent fill over a ramp slot.
        ui::widget::Toggle::paint(
            scene,
            transform,
            theme,
            &ui::widget::ToggleFace::new(toggle, self.controller.options.audio),
        );
    }

    /// The Stream row's three chips: Auto/Fixed, resolution limit, fps.
    fn draw_stream_chips(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let top = self.wireless_top() + 76.0;
        text_renderer.draw_style(
            scene,
            &t("wireless-stream"),
            colors.text_dim,
            transform.pre_translate((r.x0 + 20.0, top + 10.0).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let x0 = r.x0 + 76.0;
        let fps = f64::from(self.controller.options.stream_policy.max_fps);
        let labels = [
            if self.controller.options.stream_policy.adaptive {
                t("wireless-stream-auto")
            } else {
                t("wireless-stream-fixed")
            },
            self.controller
                .options
                .stream_policy
                .resolution_limit
                .label()
                .to_owned(),
            t_args("wireless-stream-fps", &[("fps", ArgValue::Number(fps))]),
        ];
        let widths = [76.0, 88.0, 76.0];
        let mut chip_x = x0;
        // Control chips at the Control radius: the engaged "Auto" choice is
        // a lift with the tier's hairline, idle chips rest on the lift floor
        // with a derived edge.
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        for (i, (label, width)) in labels.iter().zip(widths).enumerate() {
            let chip = Rect::new(chip_x, top, chip_x + width, top + 30.0);
            match i {
                0 => self.adaptive_rect = chip,
                1 => self.resolution_rect = chip,
                _ => self.fps_rect = chip,
            }
            let on = i == 0 && self.controller.options.stream_policy.adaptive;
            let radius = ui::chrome::ChromeRole::Control.radius(theme, chip.height());
            let shape = RoundedRect::from_rect(chip, radius);
            let tier = ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(on));
            scene.fill(
                Fill::NonZero,
                transform,
                if on { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(chip, radius),
            );
            let (label_w, label_h) =
                text_renderer.measure_style(label, None, &type_scale.style(TextRole::Caption));
            text_renderer.draw_style(
                scene,
                label,
                colors.text_primary,
                transform.pre_translate(
                    (
                        chip.center().x - f64::from(label_w) / 2.0,
                        chip.center().y - f64::from(label_h) / 2.0,
                    )
                        .into(),
                ),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            chip_x += width + 6.0;
        }
    }

    /// The AirPlay-only credential field, shown when the selected device
    /// needs a PIN or password; cleared and unfocused otherwise.
    fn draw_credential_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        if self.airplay_selected() {
            let top = self.wireless_top() + 120.0;
            text_renderer.draw_style(
                scene,
                &t("wireless-credential-label"),
                colors.text_primary,
                transform.pre_translate((r.x0 + 20.0, top).into()),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Semibold),
            );
            self.credential_rect = Rect::new(r.x0 + 20.0, top + 21.0, r.x1 - 20.0, top + 63.0);
            self.credential_input
                .apply_layout_external(self.credential_rect);
            self.credential_input
                .draw(scene, transform, text_renderer, theme, presence);
            text_renderer.draw_style(
                scene,
                &t("wireless-credential-hint"),
                colors.text_dim,
                transform.pre_translate((r.x0 + 20.0, top + 71.0).into()),
                Some(f32_of_f64(r.width() - 40.0)),
                &type_scale.style(TextRole::Caption),
            );
        } else {
            self.credential_rect = Rect::ZERO;
            self.credential_input.set_focus(false);
        }
    }

    /// An honest zero-state for the receiver list: what's happening
    /// (scanning / errored / nothing found) and what to do about it.
    fn draw_devices_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        let (message, detail) = match &self.controller.status {
            ProjectionStatus::Scanning => (t("wireless-scanning"), t("wireless-scanning-detail")),
            ProjectionStatus::Error(error) => (t("wireless-scan-failed"), error.clone()),
            _ => (t("wireless-none"), t("wireless-none-detail")),
        };
        let top = self.device_rows_top();
        let icon = Rect::new(r.center().x - 16.0, top, r.center().x + 16.0, top + 32.0);
        ui::icons::draw_icon(
            scene,
            transform,
            icon,
            ui::icons::Icon::Monitor,
            colors.text_dim,
            1.5,
        );
        let (mw, _) =
            text_renderer.measure_style(&message, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &message,
            colors.text_primary,
            transform.pre_translate((r.center().x - f64::from(mw) / 2.0, top + 40.0).into()),
            None,
            &type_scale
                .style(TextRole::Caption)
                .with_weight(TextWeight::Medium),
        );
        let detail = text_renderer.elide_to_width_style(
            &detail,
            f32_of_f64(r.width() - 56.0),
            &type_scale.style(TextRole::Caption),
        );
        let (dw, _) =
            text_renderer.measure_style(&detail, None, &type_scale.style(TextRole::Caption));
        text_renderer.draw_style(
            scene,
            &detail,
            colors.text_dim,
            transform.pre_translate((r.center().x - f64::from(dw) / 2.0, top + 64.0).into()),
            None,
            &type_scale.style(TextRole::Caption),
        );
    }

    /// One row per discovered device, up to `max_rows`: icon, name,
    /// status sub-line and a Connect/Stop/Wait action pill.
    fn draw_devices_list(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        max_rows: usize,
    ) {
        let colors = &theme.tokens.colors;
        let type_scale = &theme.tokens.typography;
        // Control role: the selected device is a lift (HIG §9.3); every row
        // carries an edge derived from its fill (§9.5).
        let floor = colors.lift(0.35);
        let idle_edge = ui::surface::edge_on(floor);
        let receivers_focused = self.focus == SheetFocus::Receivers;
        for (i, device) in self.controller.devices.iter().take(max_rows).enumerate() {
            let row = self.row_rect(i);
            self.device_rects.push(row);
            let selected = self.controller.selected == i;
            let connected = matches!(&self.controller.status, ProjectionStatus::Connected { device_id } if device_id == &device.id);
            let connecting = matches!(&self.controller.status, ProjectionStatus::Connecting { device_id } if device_id == &device.id);
            let radius = ui::chrome::ChromeRole::Control.radius(theme, row.height());
            let shape = RoundedRect::from_rect(row, radius);
            let tier =
                ui::chrome::selection_tier(theme, ui::chrome::SelectionState::active(selected));
            scene.fill(
                Fill::NonZero,
                transform,
                if selected { tier.fill } else { floor },
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                tier.border.unwrap_or(idle_edge),
                None,
                &ui::surface::edge_outline(row, radius),
            );
            if selected && receivers_focused {
                ui::widget::paint_focus_ring(scene, transform, row, radius, theme);
            }
            ui::icons::draw_icon(
                scene,
                transform,
                Rect::new(row.x0 + 12.0, row.y0 + 17.0, row.x0 + 34.0, row.y0 + 39.0),
                ui::icons::Icon::Monitor,
                if connected {
                    colors.semantic.success
                } else {
                    colors.text_dim
                },
                1.5,
            );
            let name = text_renderer.elide_to_width_style(
                &device.name,
                f32_of_f64(row.width() - 142.0),
                &type_scale.style(TextRole::Caption),
            );
            text_renderer.draw_style(
                scene,
                &name,
                colors.text_primary,
                transform.pre_translate((row.x0 + 46.0, row.y0 + 8.0).into()),
                None,
                &type_scale
                    .style(TextRole::Caption)
                    .with_weight(TextWeight::Medium),
            );
            let sub = if connected {
                t("wireless-connected")
            } else if connecting {
                t("wireless-connecting")
            } else if device.detail.is_empty() {
                device.protocol.label().to_string()
            } else {
                format!("{} · {}", device.protocol.label(), device.detail)
            };
            text_renderer.draw_style(
                scene,
                &sub,
                if connected {
                    colors.semantic.success
                } else {
                    colors.text_dim
                },
                transform.pre_translate((row.x0 + 46.0, row.y0 + 31.0).into()),
                Some(f32_of_f64(row.width() - 142.0)),
                &type_scale.style(TextRole::Caption),
            );
            Self::draw_device_action(
                scene,
                transform,
                text_renderer,
                theme,
                row,
                connected,
                connecting,
            );
        }
    }

    /// The Connect/Stop/Wait action on a device row, painted by the shared
    /// button face.
    fn draw_device_action(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        connected: bool,
        connecting: bool,
    ) {
        let action = Rect::new(row.x1 - 86.0, row.y0 + 11.0, row.x1 - 10.0, row.y1 - 11.0);
        let label = if connected {
            t("wireless-stop")
        } else if connecting {
            t("wireless-wait")
        } else {
            t("wireless-connect")
        };
        ui::widget::Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ui::widget::ButtonFace::new(action, &label)
                .role(if connected {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Secondary
                })
                .disabled(connecting),
        );
    }
}

/// Two small screens — this one on the left, the external one on the right
/// — showing what each layout does: one desktop spanning both, the same
/// picture on both, or one of them dark.
fn draw_layout_pictogram(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    mode: DisplayLayoutMode,
    theme: &Theme,
    enabled: bool,
) {
    let colors = &theme.tokens.colors;
    let alpha = if enabled { 1.0 } else { 0.5 };
    let lit = colors.text_primary.with_alpha_factor(alpha);
    let dark = colors.text_dim.with_alpha_factor(0.45 * alpha);
    let content = theme
        .tokens
        .colors
        .text_primary
        .with_alpha_factor(0.85 * alpha);
    let gap = 4.0;
    let width = (rect.width() - gap) / 2.0;
    let left = Rect::new(rect.x0, rect.y0, rect.x0 + width, rect.y1);
    let right = Rect::new(left.x1 + gap, rect.y0, rect.x1, rect.y1);
    let (left_on, right_on) = match mode {
        DisplayLayoutMode::Extend | DisplayLayoutMode::Mirror => (true, true),
        DisplayLayoutMode::BuiltinOnly => (true, false),
        DisplayLayoutMode::ExternalOnly => (false, true),
    };
    let stroke = vello::kurbo::Stroke::new(1.25);
    for (screen, on) in [(left, left_on), (right, right_on)] {
        let shape = RoundedRect::from_rect(screen, 2.5);
        scene.stroke(
            &stroke,
            transform,
            if on { lit } else { dark },
            None,
            &shape,
        );
    }
    match mode {
        DisplayLayoutMode::Extend => {
            // One band continuing from the left screen onto the right.
            let band = Rect::new(
                left.x0 + 4.0,
                rect.center().y - 2.5,
                right.x1 - 4.0,
                rect.center().y + 2.5,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                content,
                None,
                &RoundedRect::from_rect(band, 1.5),
            );
        }
        DisplayLayoutMode::Mirror => {
            for screen in [left, right] {
                let dot = Rect::from_center_size(screen.center(), (6.0, 6.0));
                scene.fill(
                    Fill::NonZero,
                    transform,
                    content,
                    None,
                    &RoundedRect::from_rect(dot, 1.5),
                );
            }
        }
        DisplayLayoutMode::BuiltinOnly | DisplayLayoutMode::ExternalOnly => {
            let screen = if left_on { left } else { right };
            let dot = Rect::from_center_size(screen.center(), (6.0, 6.0));
            scene.fill(
                Fill::NonZero,
                transform,
                content,
                None,
                &RoundedRect::from_rect(dot, 1.5),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use settings_store::hardware::{OutputDescriptor, OutputMode};

    use super::*;

    #[test]
    fn projection_sheet_owns_its_geometry_options_and_close_intent() {
        /// HIG §9.5: the sheet's Panel border on its ground, and the derived edge
        /// of its lift-floor rows, must register on every palette.
        fn sheet_cells_keep_an_edge_on_every_palette() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let ground = ui::chrome::shell::panel_ground(&theme);
                let floor = theme.tokens.colors.lift(0.35);
                for (what, edge, fill) in [
                    (
                        "panel border",
                        ui::chrome::shell::panel_border(&theme),
                        ground,
                    ),
                    ("layout / device row", ui::surface::edge_on(floor), floor),
                ] {
                    let delta = ui::surface::edge_delta(edge, fill);
                    assert!(
                        delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                        "{}: the {what} must separate from its fill (Δ luminance {delta:.4})",
                        theme.name
                    );
                }
            }
        }

        fn airplay_selection_owns_masked_pairing_geometry() {
            let mut overlay = overlay_with(Vec::new());
            overlay.controller.devices = vec![airplay_device()];
            overlay.controller.selected = 0;
            assert!(overlay.airplay_selected());
            assert!(overlay.credential_input.is_secret());
            let expected = overlay.wireless_top() + 214.0;
            assert!((overlay.device_rows_top() - expected).abs() < f64::EPSILON);
        }

        fn the_airplay_credential_is_a_secret_prompt_only_while_it_is_shown() {
            let mut overlay = overlay_with(Vec::new());
            overlay.controller.devices = vec![airplay_device()];
            overlay.controller.selected = 0;
            overlay.credential_rect = Rect::new(900.0, 300.0, 1200.0, 342.0);
            let now = Instant::now();
            assert_eq!(overlay.secret_prompt_rect(now), None, "the sheet is closed");
            overlay.transition = ui::Transition::open();
            let rect = overlay
                .secret_prompt_rect(now)
                .expect("an open sheet asking for the AirPlay credential");
            let slide = overlay.sheet_rect().width() * 0.22;
            assert!(rect.contains(Point::new(901.0, 301.0)));
            assert!(rect.contains(Point::new(1199.0 + slide, 341.0)));
            overlay.controller.devices.clear();
            assert_eq!(
                overlay.secret_prompt_rect(now),
                None,
                "no AirPlay receiver selected, no credential field"
            );
        }

        fn keyboard_controls_update_owned_options_and_close_intent() {
            let mut overlay = overlay_with(Vec::new());
            let none = Modifiers::default();
            let old_audio = overlay.controller.options.audio;
            assert!(
                overlay
                    .handle_key(&Key::Named(NamedKey::Space), none)
                    .consumed
            );
            assert_ne!(overlay.controller.options.audio, old_audio);
            let old_fps = overlay.controller.options.stream_policy.max_fps;
            overlay.handle_key(&Key::Character("f".into()), none);
            assert_ne!(overlay.controller.options.stream_policy.max_fps, old_fps);
            assert!(
                overlay
                    .handle_key(&Key::Named(NamedKey::Escape), none)
                    .close_requested
            );
        }

        fn outside_click_requests_host_owned_visibility_close() {
            let mut overlay = overlay_with(Vec::new());
            assert_eq!(
                overlay.handle_click(Point::new(10.0, 10.0)),
                EventOutcome::close()
            );
        }

        sheet_cells_keep_an_edge_on_every_palette();
        airplay_selection_owns_masked_pairing_geometry();
        the_airplay_credential_is_a_secret_prompt_only_while_it_is_shown();
        keyboard_controls_update_owned_options_and_close_intent();
        outside_click_requests_host_owned_visibility_close();
    }

    fn airplay_device() -> crate::shell::projection::ProjectionDevice {
        crate::shell::projection::ProjectionDevice {
            id: "airplay:test".to_owned(),
            name: "Living Room".to_owned(),
            protocol: ProjectionProtocol::AirPlay,
            detail: "Apple TV".to_owned(),
            modes: vec![ProjectionMode::Mirror],
            addresses: vec!["192.0.2.8".to_owned()],
            port: 7000,
        }
    }

    fn two_displays() -> Vec<DisplayOutputSnapshot> {
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        ["eDP-1", "HDMI-A-1"]
            .into_iter()
            .map(|connector| DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector: connector.into(),
                    connector_label: connector.into(),
                    modes: vec![mode],
                    hdr: false,
                    vrr: false,
                    color_profiles: Vec::new(),
                    monitor: None,
                },
                current_mode: mode,
                scale: 1.0,
                builtin: connector == "eDP-1",
                sink_hdr: false,
                hdr_unavailable_reason: None,
                sink_vrr: false,
                vrr_unavailable_reason: None,
                color_profile_labels: BTreeMap::new(),
            })
            .collect()
    }

    /// An overlay whose display state is already taken, so key handling does
    /// not read the developer machine's settings.
    fn overlay_with(outputs: Vec<DisplayOutputSnapshot>) -> ProjectionOverlay {
        let mut overlay = ProjectionOverlay {
            host_rect: Rect::new(0.0, 0.0, 1280.0, 900.0),
            session_open: true,
            ..ProjectionOverlay::default()
        };
        overlay.displays.outputs = outputs;
        overlay.displays.outputs_seen = Some(DisplayHardwareSnapshot::published_generation());
        overlay.displays.polled_at = Some(Instant::now());
        overlay
    }

    #[test]
    fn a_layout_choice_is_the_published_action_and_its_refusal_is_said() {
        let mut overlay = overlay_with(two_displays());
        overlay.focus = SheetFocus::Layout(DisplayLayoutMode::Mirror);
        overlay.handle_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        let request = overlay
            .take_request()
            .expect("Enter hands the choice to the host as its action");
        assert_eq!(request.action_id(), "display.layout.mirror");
        assert!(
            overlay.displays.staging.is_none(),
            "the sheet stages nothing itself"
        );
        overlay.follow_request(
            request,
            Err(crate::shell::search::action_api::ActionError::Refused(
                "not permitted here".into(),
            )),
        );
        assert!(
            overlay
                .accessibility()
                .description
                .is_some_and(|description| description.contains("not permitted here")),
            "the refusal is said, not only drawn"
        );

        // A staged layout's receipt names the change to answer.
        let receipt = crate::shell::search::ActionReceipt::completed_with(
            serde_json::json!({ "transaction": 7 }),
        );
        overlay.follow_request(
            DisplayRequest::Layout(DisplayLayoutMode::Mirror),
            Ok(crate::shell::search::InvocationDisposition::Accepted(
                receipt,
            )),
        );
        assert!(overlay.displays.poll_completions());
        assert_eq!(overlay.displays.staged, Some(7));
        assert!(overlay.displays.notice.is_none());
    }

    #[test]
    fn keep_and_revert_wait_for_a_change_and_for_the_answer_in_flight() {
        let mut overlay = overlay_with(two_displays());
        overlay.focus = SheetFocus::Keep;
        overlay.activate_focus();
        assert_eq!(overlay.take_request(), None, "no change waits for Keep");
        overlay.displays.transaction = Some(DisplayTransaction::default());
        overlay.focus = SheetFocus::Revert;
        overlay.activate_focus();
        let request = overlay.take_request();
        assert_eq!(request, Some(DisplayRequest::Revert));
        let in_flight: crate::shell::search::PendingInvocation = std::rc::Rc::new(|| Ok(None));
        overlay.follow_request(
            DisplayRequest::Revert,
            Ok(crate::shell::search::InvocationDisposition::Pending(
                in_flight,
            )),
        );
        overlay.focus = SheetFocus::Keep;
        overlay.activate_focus();
        assert_eq!(overlay.take_request(), None, "one answer at a time");
    }

    #[test]
    fn super_p_cycles_through_every_layout_the_displays_allow() {
        let mut overlay = overlay_with(two_displays());
        overlay.focus = SheetFocus::Layout(DisplayLayoutMode::Extend);
        let sup = Modifiers::from(winit::keyboard::ModifiersState::SUPER);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let outcome = overlay.handle_key(&Key::Character("p".into()), sup);
            assert!(outcome.consumed && !outcome.close_requested);
            let SheetFocus::Layout(mode) = overlay.focus else {
                panic!("Super+P keeps the highlight on a layout");
            };
            seen.push(mode);
        }
        assert_eq!(
            seen,
            [
                DisplayLayoutMode::Mirror,
                DisplayLayoutMode::BuiltinOnly,
                DisplayLayoutMode::ExternalOnly,
                DisplayLayoutMode::Extend,
            ]
        );
    }

    #[test]
    fn a_single_display_offers_no_physical_layout() {
        let overlay = overlay_with(two_displays().into_iter().take(1).collect());
        assert!(overlay.enabled_layouts().is_empty());
    }

    #[test]
    fn arrows_walk_layouts_then_receivers() {
        let mut overlay = overlay_with(two_displays());
        overlay.controller.devices = vec![airplay_device()];
        overlay.focus = SheetFocus::Layout(DisplayLayoutMode::BuiltinOnly);
        let none = Modifiers::default();
        overlay.handle_key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(
            overlay.focus,
            SheetFocus::Layout(DisplayLayoutMode::ExternalOnly)
        );
        overlay.handle_key(&Key::Named(NamedKey::ArrowDown), none);
        assert_eq!(overlay.focus, SheetFocus::Receivers);
        overlay.handle_key(&Key::Named(NamedKey::ArrowUp), none);
        assert_eq!(
            overlay.focus,
            SheetFocus::Layout(DisplayLayoutMode::ExternalOnly)
        );
    }
    #[test]
    fn shared_output_rows_keep_offline_state_and_keyboard_inclusion_separate_from_hardware() {
        let _home = shared_display_actions::test_support::publish();
        let mut overlay = overlay_with(two_displays().into_iter().take(1).collect());
        overlay.bind_shared_display_authority(shared_display_actions::test_support::owner());
        overlay.displays.refresh_person();
        assert!(overlay.enabled_layouts().is_empty());
        let rows = overlay.displays.remote();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].title().contains('2') && rows[0].title().contains("Orange Pi"));
        assert!(rows[1].detail().contains("Offline"));
        let mode = overlay.controller.options.mode;
        overlay.focus = SheetFocus::Remote(0);
        overlay.handle_key(&Key::Named(NamedKey::Space), Modifiers::default());
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(!saved.entry("bb02", "panel").unwrap().enabled);
        assert!(saved.entry("cc03", "panel").unwrap().enabled);
        let rows = overlay.displays.remote();
        let peer = rows
            .iter()
            .position(|row| row.device == "bb02" && row.output == "panel")
            .unwrap();
        assert!(!rows[peer].enabled);
        assert_eq!(
            overlay.focus,
            SheetFocus::Remote(peer),
            "focus follows the certified pair, not its old row index"
        );
        assert_eq!(overlay.controller.options.mode, mode);
        assert!(overlay.displays.staging.is_none());
        overlay.handle_key(&Key::Named(NamedKey::Enter), Modifiers::default());
        assert!(
            settings_store::person_displays::Arrangement::load()
                .entry("bb02", "panel")
                .unwrap()
                .enabled
        );
        overlay.handle_key(&Key::Named(NamedKey::ArrowDown), Modifiers::default());
        assert_eq!(overlay.focus, SheetFocus::Remote(1));
        overlay.handle_key(&Key::Named(NamedKey::Space), Modifiers::default());
        let offline = overlay.displays.remote().remove(1);
        assert!(!offline.online && !offline.enabled);
        assert!(offline.detail().contains("Offline") && offline.detail().contains("Not in layout"));
        let accessible = overlay.accessibility();
        let switches = accessible
            .children
            .iter()
            .filter(|node| node.role == ui::a11y::Role::Switch)
            .collect::<Vec<_>>();
        assert_eq!(switches.len(), 2);
        assert_eq!(switches[1].state.checked, Some(false));
        assert!(
            switches[1]
                .description
                .as_ref()
                .unwrap()
                .contains("Offline")
        );
    }

    #[test]
    fn shared_output_scroll_keeps_all_screens_reachable_in_every_theme() {
        let _home = shared_display_actions::test_support::publish();
        let mut facts = settings_store::person_displays::PersonDisplays::load().unwrap();
        let peer = facts
            .devices
            .iter_mut()
            .find(|device| device.device == "bb02")
            .unwrap();
        let output = peer.outputs[0].clone();
        for index in 1..8 {
            let mut next = output.clone();
            next.id = format!("extra-{index}");
            next.name = format!("Extra screen {index}");
            next.x = index * 960;
            peer.outputs.push(next);
        }
        facts.save().unwrap();
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let mut overlay = overlay_with(two_displays().into_iter().take(1).collect());
            overlay.bind_shared_display_authority(shared_display_actions::test_support::owner());
            overlay.displays.refresh_person();
            let count = overlay.displays.remote().len();
            assert_eq!(count, 9);
            overlay.focus = SheetFocus::Remote(count - 1);
            overlay.draw_remote_displays(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &theme,
                overlay.sheet_rect(),
            );
            assert!(overlay.remote_scroll.overflows_vertically());
            assert!(overlay.remote_rects[count - 1].1.area() > 0.0);
            assert!(overlay.remote_rects[0].1.area() <= 0.0);
            assert!(overlay.wireless_top() < overlay.sheet_rect().y1);
            assert_eq!(
                overlay
                    .accessibility()
                    .children
                    .iter()
                    .filter(|node| node.role == ui::a11y::Role::Switch)
                    .count(),
                count
            );
            overlay.handle_key(&Key::Named(NamedKey::ArrowUp), Modifiers::default());
            assert_eq!(overlay.focus, SheetFocus::Remote(count - 2));
        }
    }
    #[test]
    fn shared_output_focus_tracks_the_certified_pair_after_a_collaborator_edit() {
        let _home = shared_display_actions::test_support::publish();
        let mut overlay = overlay_with(two_displays().into_iter().take(1).collect());
        let chrome = shared_display_actions::test_support::owner();
        overlay.bind_shared_display_authority(chrome.clone());
        overlay.displays.refresh_person();
        overlay.focus = SheetFocus::Remote(0);
        shared_display_actions::include(&chrome.borrow(), "bb02", "panel", 0, false).unwrap();
        let view = shared_display_actions::current().unwrap();
        view.layout
            .move_to_order(&view.arrangement, "cc03", "panel", 0)
            .save()
            .unwrap();
        overlay.displays.polled_at = None;
        overlay.poll_displays(Instant::now());
        let rows = overlay.displays.remote();
        let peer = rows
            .iter()
            .position(|row| row.device == "bb02" && row.output == "panel")
            .unwrap();
        assert_eq!(overlay.focus, SheetFocus::Remote(peer));
        assert_ne!(
            peer, 0,
            "the collaborator edit reordered the actual resolved layout"
        );
        overlay.handle_key(&Key::Named(NamedKey::Space), Modifiers::default());
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(saved.entry("bb02", "panel").unwrap().enabled);
        assert!(saved.entry("cc03", "panel").unwrap().enabled);
    }

    #[test]
    fn shared_output_clicks_keep_the_painted_identity_when_a_collaborator_reorders_rows() {
        let _home = shared_display_actions::test_support::publish();
        let mut overlay = overlay_with(two_displays().into_iter().take(1).collect());
        overlay.bind_shared_display_authority(shared_display_actions::test_support::owner());
        overlay.displays.refresh_person();
        let theme = Theme::dark();
        overlay.draw_remote_displays(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            overlay.sheet_rect(),
        );
        let painted_peer = overlay.remote_rects[0].1.center();
        overlay.handle_click(painted_peer);
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(!saved.entry("bb02", "panel").unwrap().enabled);
        assert!(saved.entry("cc03", "panel").unwrap().enabled);
        let view = shared_display_actions::current().unwrap();
        view.layout
            .move_to_order(&view.arrangement, "cc03", "panel", 0)
            .save()
            .unwrap();
        overlay.displays.polled_at = None;
        overlay.poll_displays(Instant::now());
        assert_eq!(
            overlay.displays.remote()[0].device,
            "cc03",
            "a real collaborator reorder changed the row index"
        );
        // Another click before repaint must never edit the screen that has
        // since moved into this row index. Its painted binding is now stale.
        overlay.handle_click(painted_peer);
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(
            saved.entry("cc03", "panel").unwrap().enabled,
            "the remembered offline TV was never the clicked target"
        );
        assert!(!saved.entry("bb02", "panel").unwrap().enabled);
    }
}

//! A transient floating **status bubble**: a small floating Panel that
//! appears when a status changes (a page turn, a zoom step, a sync that
//! finished) or while the pointer rests on its corner, then fades away.
//!
//! Transient by contract: it is shown on change and hides on its own. What a
//! person needs to read at any time (the cursor's line and column, a word
//! count, conflict counts) is persistent status and belongs in
//! [`crate::chrome::status_bar`], which reserves its band. A capture never
//! forces the bubble open; only a screenshot scenario that stages the change
//! shows it.
//!
//! The bubble floats in the bottom-trailing corner of the area it reports
//! on, one gutter in, clear of the content's leading edge. It owns its text,
//! its timing and its paint; the app says what changed.

use crate::time::{Duration, Instant};

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::text_metrics::centered_text_top;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;

/// How long the bubble lingers after a poke (or after the pointer leaves its
/// zone) before it fades out, absent any new activity.
pub const DEFAULT_LINGER: Duration = Duration::from_millis(1900);
/// The tail of the linger spent fading to fully transparent, so the bubble
/// leaves quietly rather than blinking out.
const FADE: Duration = Duration::from_millis(300);

/// Every method takes an explicit `now: Instant`, so a test drives the
/// whole show → linger → fade → hide cycle without a wall clock.
#[derive(Clone, Debug)]
pub struct StatusBubble {
    linger: Duration,
    /// `None` while the pointer pins the bubble open.
    shown_at: Option<Instant>,
    hovered: bool,
    /// Pinned open by its owner until it says something else
    /// ([`Self::hold`]): a status still in progress.
    held: bool,
    /// What the bubble says; [`Self::show`] pokes when it changes.
    text: String,
}

impl Default for StatusBubble {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBubble {
    pub fn new() -> Self {
        Self {
            linger: DEFAULT_LINGER,
            shown_at: None,
            hovered: false,
            held: false,
            text: String::new(),
        }
    }

    #[must_use]
    pub fn with_linger(mut self, linger: Duration) -> Self {
        self.linger = linger;
        self
    }

    /// Returns whether visibility changed (so the caller can request a redraw).
    pub fn poke(&mut self, now: Instant) -> bool {
        let was = self.is_visible(now);
        self.shown_at = Some(now);
        !was
    }

    /// While `true` the bubble is pinned fully open; on the transition to
    /// `false` it starts a fresh linger countdown.
    pub fn set_hover(&mut self, over: bool, now: Instant) -> bool {
        let was = self.is_visible(now);
        if over {
            self.hovered = true;
            self.shown_at = None;
        } else if self.hovered {
            self.hovered = false;
            self.shown_at = Some(now);
        }
        self.is_visible(now) != was
    }

    pub fn is_visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// Fully opaque while hovered or fresh, ramping to zero across the
    /// final `FADE` of the linger.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.hovered || self.held {
            return 1.0;
        }
        let Some(t) = self.shown_at else { return 0.0 };
        let e = now.saturating_duration_since(t);
        if e >= self.linger {
            return 0.0;
        }
        let Some(remain) = self.linger.checked_sub(e) else {
            // `e >= self.linger` already returned above, so this is
            // unreachable in practice; treat it as fully faded rather than
            // panic if that invariant ever slips.
            return 0.0;
        };
        if remain < FADE {
            (remain.as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// Report the status. A change shows the bubble (and restarts its
    /// linger); the same text again does nothing, so calling this every
    /// frame is fine. Returns whether visibility changed.
    pub fn show(&mut self, text: &str, now: Instant) -> bool {
        if self.text == text && !self.held {
            return false;
        }
        let was = self.is_visible(now);
        self.held = false;
        text.clone_into(&mut self.text);
        self.shown_at = Some(now);
        !was
    }

    /// Say `text` and stay open until the owner says something else
    /// ([`Self::show`], [`Self::hold`]) or [`Self::hide`]s it: a status that
    /// is still in progress. Returns whether visibility changed.
    pub fn hold(&mut self, text: &str, now: Instant) -> bool {
        let was = self.is_visible(now);
        text.clone_into(&mut self.text);
        self.held = true;
        !was
    }

    /// Take the bubble away at once, with nothing left to say.
    pub fn hide(&mut self) {
        self.text.clear();
        self.held = false;
        self.shown_at = None;
    }

    /// Change what the bubble says without showing it: a count that follows
    /// the document reads current while the bubble shows, but does not
    /// reopen it.
    pub fn set_text(&mut self, text: &str) {
        text.clone_into(&mut self.text);
    }

    /// What the bubble says.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The bubble's rect when it floats in `area`: the bottom-trailing
    /// corner, `spacing.s4` in.
    pub fn rect_in(&self, area: Rect, tr: &mut TextRenderer, theme: &Theme) -> Rect {
        let (w, h) = pill_size(&self.text, tr, theme, 0.0);
        let gutter = f64::from(theme.tokens.spacing.s4);
        let x1 = area.x1 - gutter;
        let y1 = area.y1 - gutter;
        Rect::new((x1 - w).max(area.x0), (y1 - h).max(area.y0), x1, y1)
    }

    /// The pointer moved to `p`: resting on the bubble's corner (the
    /// bubble plus a gutter around it) pins it open, leaving starts its
    /// linger. Returns whether visibility changed.
    pub fn pointer_moved(
        &mut self,
        area: Rect,
        p: Point,
        tr: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) -> bool {
        let gutter = f64::from(theme.tokens.spacing.s6);
        let zone = self.rect_in(area, tr, theme).inflate(gutter, gutter);
        self.set_hover(!self.text.is_empty() && zone.contains(p), now)
    }

    /// Paint the bubble in `area` at its current opacity; nothing while it
    /// is hidden. Returns the painted rect.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        now: Instant,
    ) -> Option<Rect> {
        let alpha = self.alpha_in(theme, now);
        if alpha <= 0.0 || self.text.is_empty() {
            return None;
        }
        let rect = self.rect_in(area, tr, theme);
        let faded = alpha < 1.0;
        if faded {
            let reach = f64::from(theme.tokens.elevation.floating.blur) * 2.0;
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                transform,
                &rect.inflate(reach, reach),
            );
        }
        draw_pill(scene, transform, tr, theme, rect, &self.text);
        if faded {
            scene.pop_layer();
        }
        Some(rect)
    }

    /// [`Self::alpha`] as `theme` shows it: under reduced motion the bubble
    /// cuts in and out instead of fading.
    pub fn alpha_in(&self, theme: &Theme, now: Instant) -> f32 {
        let alpha = self.alpha(now);
        if theme.reduce_motion && alpha > 0.0 {
            1.0
        } else {
            alpha
        }
    }

    /// Hovered/hidden states need no frames.
    pub fn needs_frame(&self, now: Instant) -> bool {
        !self.hovered
            && !self.held
            && self
                .shown_at
                .is_some_and(|t| now.saturating_duration_since(t) < self.linger)
    }
}

/// The status pill's label: a `Label` in the theme.
fn pill_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Label)
}

/// The pill's size for `text`. `control` is a leading+trailing slot in px
/// each for a control inside the pill (`0.0` for a plain label).
pub fn pill_size(text: &str, tr: &mut TextRenderer, theme: &Theme, control: f64) -> (f64, f64) {
    let (tw, th) = tr.measure_style(text, None, &pill_style(theme));
    let pad_x = f64::from(theme.tokens.spacing.s3);
    let w = f64::from(tw) + 2.0 * pad_x + 2.0 * control;
    let h = pill_height(theme).max(f64::from(th) + 2.0 * pad_y(theme));
    (w, h)
}

/// Vertical padding of the pill's label.
fn pad_y(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s1) * 1.5
}

/// Height shared by layout reservations and the rendered pill: the label's
/// line box plus the pill's vertical padding, never below a Small control.
pub fn pill_height(theme: &Theme) -> f64 {
    (f64::from(pill_style(theme).line_height_px()) + 2.0 * pad_y(theme))
        .max(f64::from(theme.tokens.metrics.bar_control_height))
}

/// Paint a bubble with `text` in `rect`: a floating Panel (the blurred
/// material, one soft shadow) with the label centred. For a bubble that
/// hosts controls beside its label; a plain status uses
/// [`StatusBubble::paint`].
pub fn draw_pill(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    text: &str,
) {
    crate::widget::floating_surface::paint(scene, transform, rect, theme);
    let style = pill_style(theme);
    let ink = theme
        .on_ground(crate::chrome::panel_ground(theme))
        .tokens
        .colors
        .text_primary;
    let (tw, th) = tr.measure_style(text, None, &style);
    let tx = rect.x0 + (rect.width() - f64::from(tw)) / 2.0;
    // Centre the measured line box, not the font size.
    let ty = centered_text_top(rect.y0, rect.height(), f64::from(th));
    tr.draw_style(
        scene,
        text,
        ink,
        transform.pre_translate(Vec2::new(tx, ty)),
        None,
        &style,
    );
}

/// What a document's save state is, as the save status surface shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveState {
    /// Nothing to report (memory only, or nothing changed yet).
    Idle,
    /// A save is in flight.
    Saving,
    /// The last change is stored.
    Saved,
    /// The last save failed; the text says why.
    Failed(String),
}

impl From<&crate::record::RecordSaveStatus> for SaveState {
    fn from(status: &crate::record::RecordSaveStatus) -> Self {
        use crate::record::RecordSaveStatus;
        match status {
            RecordSaveStatus::Saved => Self::Saved,
            RecordSaveStatus::Saving => Self::Saving,
            RecordSaveStatus::Failed(reason) => Self::Failed(reason.clone()),
            RecordSaveStatus::MemoryOnly => Self::Idle,
        }
    }
}

/// What a person did on the save status surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveAction {
    /// "Try again" on a save that failed or gave up waiting.
    Retry,
    /// Dismissed the failure.
    Dismiss,
}

/// How long a save may take before the surface says "Saving…". A save
/// that lands sooner passes without a word: under a second the person is
/// still with what they were doing and has no reason to wonder, so neither
/// "Saving…" nor "Saved" would tell them anything.
pub const SAVE_NOTICEABLE: Duration = Duration::from_secs(1);
/// How long "Saving…" may wait for the store to confirm before the surface
/// says so in plain words and asks the host to submit the changes again.
pub const SAVE_STALL: Duration = Duration::from_secs(10);
/// How long a save may stay unconfirmed before the surface stops waiting
/// and asks the person to try again: the first submission's
/// [`SAVE_STALL`], then as long again for the retry it asked for, so the
/// retry gets the same chance before the person is asked.
pub const SAVE_GIVE_UP: Duration = Duration::from_secs(20);
/// How long a save that gave up may go unpainted before the surface takes
/// it that nobody sees the document: a window in view paints in the frame
/// its wake asks for, far sooner.
const UNSEEN_GRACE: Duration = Duration::from_secs(1);

/// How far an unconfirmed save has gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    /// Not long enough to mention.
    #[default]
    Quiet,
    /// Past [`SAVE_NOTICEABLE`]: "Saving…".
    Saving,
    /// Past [`SAVE_STALL`]: "Couldn't save yet. Retrying…".
    Retrying,
    /// Past [`SAVE_GIVE_UP`]: "Not saved" with "Try again".
    NotSaved,
}

/// What to tell a person who cannot see a document whose save gave up:
/// post it as a notification whose action is "Try again", and route that
/// action to [`SaveStatusSurface::retry_from_notice`]. See
/// [`SaveStatusSurface::take_notice`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveNotice {
    pub title: String,
    pub body: String,
    pub action: String,
}

/// A document's save status, on the framework's status surfaces (HIG §7c).
///
/// A save that lands within [`SAVE_NOTICEABLE`] says nothing: a document
/// that is saving as it should needs no status. One that takes longer says
/// "Saving…" in the transient [`StatusBubble`] for as long as it waits,
/// then "Saved" for [`ACKNOWLEDGEMENT_HOLD`], the time a passing
/// acknowledgement holds. Unconfirmed after [`SAVE_STALL`], the bubble says
/// "Couldn't save yet. Retrying…" and the surface asks for one more
/// submission ([`Self::take_retry`]); after [`SAVE_GIVE_UP`] it says "Not
/// saved" with a "Try again" button, in place, because the person is
/// looking at the document. If nobody paints it then (the document is out
/// of sight), the surface asks the host to raise a notification
/// ([`Self::take_notice`]), since the person could lose work they cannot
/// see.
///
/// A save that failed with a reason is an error toast (danger icon, the
/// reason, "Try again" and a close button) that stays until the save
/// succeeds or it is dismissed, and the save that then succeeds says
/// "Saved" once, however quickly it landed. The state itself stays
/// readable through [`Self::state`] for an app's accessibility status and
/// automation. An app reports the state and routes clicks; it never pins a
/// bubble open or paints its own save pill.
///
/// Given the document's header row ([`SaveStatusSurface::set_header_slot`]),
/// the save's progress is quiet text there, beside the title, covering none
/// of the document. Without one, and for a failure's card, they float in the
/// bottom-trailing corner of the area they report on, clear of its scroll
/// bar gutter.
///
/// [`ACKNOWLEDGEMENT_HOLD`]: crate::widget::acknowledgement::ACKNOWLEDGEMENT_HOLD
#[derive(Clone, Debug)]
pub struct SaveStatusSurface {
    bubble: StatusBubble,
    state: Option<SaveState>,
    /// A save failed and none has succeeded since: the next "Saved" is
    /// news.
    recovering: bool,
    /// The current state is the save that ended a failure: assistive
    /// technology hears it until the state changes again.
    recovered: bool,
    dismissed: bool,
    hit: crate::widget::toast::ToastCardHit,
    /// "Try again" in the bubble of a save that gave up.
    retry_rect: Rect,
    hover: Option<Point>,
    /// When the current save began waiting.
    saving_since: Option<Instant>,
    stage: Stage,
    /// A submission the surface asked for, not yet taken by the host.
    retry: bool,
    /// When a save gave up without being painted since.
    unseen_since: Option<Instant>,
    /// Whether a notification about this save is up.
    notified: bool,
    /// A notification to take down, not yet taken by the host.
    withdraw: bool,
    /// Room in the document's header row ([`Self::set_header_slot`]).
    slot: Rect,
    /// Where the status last painted in [`Self::slot`].
    in_slot: Rect,
}

impl Default for SaveStatusSurface {
    fn default() -> Self {
        Self {
            bubble: StatusBubble::new()
                .with_linger(crate::widget::acknowledgement::ACKNOWLEDGEMENT_HOLD),
            state: None,
            recovering: false,
            recovered: false,
            dismissed: false,
            hit: crate::widget::toast::ToastCardHit::default(),
            retry_rect: Rect::ZERO,
            hover: None,
            saving_since: None,
            stage: Stage::Quiet,
            retry: false,
            unseen_since: None,
            notified: false,
            withdraw: false,
            slot: Rect::ZERO,
            in_slot: Rect::ZERO,
        }
    }
}

impl SaveStatusSurface {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Say the save's progress in `slot`, the room the document's header
    /// row leaves beside its title (`NavShell::header_status_slot`), instead
    /// of floating over the document's corner where it would cover what
    /// the person reads. A failure's card still floats: it carries a reason
    /// and two choices. Where the slot is too narrow for what there is to
    /// say, the corner is used after all.
    pub fn set_header_slot(&mut self, slot: Rect) {
        self.slot = slot;
    }

    /// Report the save state. A change shows it; the same state again does
    /// nothing, so calling this every frame is fine. Returns whether what
    /// is shown changed.
    pub fn set(&mut self, state: SaveState, now: Instant) -> bool {
        if self.state.as_ref() == Some(&state) {
            return false;
        }
        let shown = |surface: &Self| {
            (
                surface.is_visible(now),
                surface.bubble.text().to_owned(),
                surface.failure().map(str::to_owned),
            )
        };
        let before = shown(self);
        let noticed = self.stage > Stage::Quiet;
        self.dismissed = false;
        self.saving_since = (state == SaveState::Saving).then_some(now);
        self.stage = Stage::Quiet;
        self.unseen_since = None;
        self.withdraw |= self.notified;
        self.notified = false;
        self.retry_rect = Rect::ZERO;
        let recovered = self.recovering && state == SaveState::Saved;
        self.recovered = recovered;
        // A retry passes through "Saving…" before it lands.
        self.recovering = match state {
            SaveState::Failed(_) => true,
            SaveState::Saving => self.recovering,
            SaveState::Saved | SaveState::Idle => false,
        };
        self.state = Some(state);
        match &self.state {
            // Only a save the person saw waiting, or the one that ends a
            // failure, is worth confirming.
            Some(SaveState::Saved) if noticed || recovered => {
                let text = self.bubble_label();
                self.bubble.show(&text, now);
            }
            _ => self.bubble.hide(),
        }
        before != shown(self)
    }

    /// What the bubble says for the current state: the stage of a save
    /// that waits, or "Saved".
    fn bubble_label(&self) -> String {
        match (&self.state, self.stage) {
            (Some(SaveState::Saving), Stage::Saving) => crate::i18n!("save-status-saving"),
            (Some(SaveState::Saving), Stage::Retrying) => crate::i18n!("save-status-stalled"),
            (Some(SaveState::Saving), Stage::NotSaved) => crate::i18n!("save-status-not-saved"),
            (Some(SaveState::Saved), _) => crate::i18n!("save-status-saved"),
            _ => String::new(),
        }
    }

    /// The save state last reported, for an app's accessibility status and
    /// automation.
    #[must_use]
    pub fn state(&self) -> Option<&SaveState> {
        self.state.as_ref()
    }

    /// Move an unconfirmed save along at `now`: "Saving…" once it is
    /// noticeable, "Retrying…" (asking for a submission) past
    /// [`SAVE_STALL`], "Not saved" past [`SAVE_GIVE_UP`]. [`Self::paint`]
    /// calls this; a host calls it where it needs the surface current
    /// without painting. Returns whether what is shown changed.
    pub fn advance(&mut self, now: Instant) -> bool {
        if self.state != Some(SaveState::Saving) {
            return false;
        }
        let since = *self.saving_since.get_or_insert(now);
        let waited = now.saturating_duration_since(since);
        let stage = if waited >= SAVE_GIVE_UP {
            Stage::NotSaved
        } else if waited >= SAVE_STALL {
            Stage::Retrying
        } else if waited >= SAVE_NOTICEABLE {
            Stage::Saving
        } else {
            Stage::Quiet
        };
        if stage <= self.stage {
            return false;
        }
        if self.stage < Stage::Retrying && stage >= Stage::Retrying {
            self.retry = true;
        }
        if stage == Stage::NotSaved {
            // Seen only once a frame paints it.
            self.unseen_since = Some(now);
        }
        self.stage = stage;
        let text = self.bubble_label();
        self.bubble.hold(&text, now);
        true
    }

    /// Whether the surface asked for the record's changes to be submitted
    /// again (a save that stalled), once. A host routes it like
    /// [`SaveAction::Retry`].
    pub fn take_retry(&mut self) -> bool {
        std::mem::take(&mut self.retry)
    }

    /// A notification to raise about a save that gave up while nobody
    /// painted the document, once, at `now`. It names the `document` (its
    /// title, if it has one) and says the changes are still in `app`, since
    /// whoever reads it is somewhere else. The host posts it and routes its
    /// action to [`Self::retry_from_notice`]; [`Self::take_withdrawal`]
    /// says when to take it down.
    pub fn take_notice(
        &mut self,
        now: Instant,
        document: Option<&str>,
        app: &str,
    ) -> Option<SaveNotice> {
        self.advance(now);
        let unseen = self.unseen_since?;
        if self.notified || now.saturating_duration_since(unseen) < UNSEEN_GRACE {
            return None;
        }
        self.notified = true;
        let title = match document.map(str::trim).filter(|title| !title.is_empty()) {
            Some(document) => crate::i18n!("save-status-not-saved-named", "document" => document),
            None => crate::i18n!("save-status-not-saved"),
        };
        Some(SaveNotice {
            title,
            body: crate::i18n!("save-status-not-saved-notice", "app" => app),
            action: crate::i18n!("save-status-retry"),
        })
    }

    /// Whether a notification [`Self::take_notice`] raised should come
    /// down (the save landed or failed differently, the person tried again,
    /// or the document is in view again and says it in place), once.
    pub fn take_withdrawal(&mut self) -> bool {
        std::mem::take(&mut self.withdraw)
    }

    /// The text of the failure toast: its title and the reason under it.
    fn failure_text(&self) -> Option<(String, &str)> {
        let reason = self.failure()?;
        Some((crate::i18n!("save-status-failed"), reason))
    }

    /// The pointer moved to `p` over `area`: resting on the bubble's corner
    /// pins it open (see [`StatusBubble::pointer_moved`]). Returns whether
    /// visibility changed.
    pub fn hover_bubble(
        &mut self,
        area: Rect,
        p: Point,
        tr: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) -> bool {
        if self.failure().is_some() || !(self.bubble.hovered || self.bubble.is_visible(now)) {
            return false;
        }
        if !self.in_slot.is_zero_area() {
            let gutter = f64::from(theme.tokens.spacing.s2);
            let over = self.in_slot.inflate(gutter, gutter).contains(p);
            return self
                .bubble
                .set_hover(!self.bubble.text.is_empty() && over, now);
        }
        let corner = Self::corner(area, theme);
        self.bubble.pointer_moved(corner, p, tr, theme, now)
    }

    /// The surface for assistive technology, announcing what it shows and
    /// nothing else: a failure's reason, with "Try again" and "Dismiss"
    /// buttons whose action targets are `{target}.retry` and
    /// `{target}.dismiss`; a save that gave up, with "Try again"; and the
    /// save that ends a failure, until the state changes again (a state
    /// change, unlike the bubble's timed hold, is something a host's
    /// accessibility tree follows). Autosave, slow or not, is silent.
    #[must_use]
    pub fn accessibility(&self, target: &str) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Politeness, Role};
        let button = |name: String, part: &str, rect: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(format!("{target}.{part}"))
                .with_bounds(rect)
        };
        let retry = |rect| button(crate::i18n!("save-status-retry"), "retry", rect);
        if self.gave_up() {
            return Some(
                AccessNode::new(Role::Group)
                    .with_name(crate::i18n!("save-status-not-saved"))
                    .with_description(crate::i18n!("save-status-not-saved-detail"))
                    .with_live(Politeness::Assertive)
                    .with_children(vec![retry(self.retry_rect)]),
            );
        }
        let Some((title, reason)) = self.failure_text() else {
            return self.recovered.then(|| {
                AccessNode::new(Role::StaticText)
                    .with_name(crate::i18n!("save-status-saved"))
                    .with_live(Politeness::Polite)
                    .with_hashed_key(format!("{target}.recovered"))
            });
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name(title)
                .with_description(reason.to_owned())
                .with_live(Politeness::Assertive)
                .with_bounds(self.hit.card_rect)
                .with_children(vec![
                    retry(self.hit.primary_action_rect),
                    button(
                        crate::i18n!("save-status-dismiss"),
                        "dismiss",
                        self.hit.secondary_action_rect,
                    ),
                ]),
        )
    }

    /// Assistive technology activated `action_target` (one of
    /// [`Self::accessibility`]'s buttons under `target`): the action it is.
    pub fn perform(&mut self, target: &str, action_target: &str) -> Option<SaveAction> {
        if self.failure().is_none() && !self.gave_up() {
            return None;
        }
        let part = action_target.strip_prefix(target)?.strip_prefix('.')?;
        let action = match part {
            "retry" => SaveAction::Retry,
            "dismiss" if !self.gave_up() => SaveAction::Dismiss,
            _ => return None,
        };
        self.acted(action);
        Some(action)
    }

    /// Dismissing hides the failure until the state changes; trying a save
    /// that gave up again starts its wait over, quietly at first.
    fn acted(&mut self, action: SaveAction) {
        match action {
            SaveAction::Dismiss => self.dismissed = true,
            SaveAction::Retry if self.gave_up() => self.wait_again(),
            SaveAction::Retry => {}
        }
    }

    /// Start an unconfirmed save's wait over: the person (or the
    /// notification's action) asked to try again.
    fn wait_again(&mut self) {
        // The next [`Self::advance`] starts the wait at its `now`.
        self.saving_since = None;
        self.stage = Stage::Quiet;
        self.unseen_since = None;
        self.withdraw |= self.notified;
        self.notified = false;
        self.retry_rect = Rect::ZERO;
        self.bubble.hide();
    }

    /// "Try again" from the notification [`Self::take_notice`] raised: the
    /// host submits the changes again and the wait starts over.
    pub fn retry_from_notice(&mut self) {
        if self.gave_up() {
            self.wait_again();
        }
    }

    /// Whether the current save gave up waiting and asks the person.
    #[must_use]
    pub fn gave_up(&self) -> bool {
        self.state == Some(SaveState::Saving) && self.stage == Stage::NotSaved
    }

    /// The reason of a failed save that is showing, if any.
    #[must_use]
    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            Some(SaveState::Failed(reason)) if !self.dismissed => Some(reason.as_str()),
            _ => None,
        }
    }

    /// Whether anything is showing at `now`.
    #[must_use]
    pub fn is_visible(&self, now: Instant) -> bool {
        self.failure().is_some() || self.bubble.is_visible(now)
    }

    /// What the bubble says, empty when it has nothing to say.
    #[must_use]
    pub fn bubble_text(&self) -> &str {
        self.bubble.text()
    }

    /// Frames are needed only while the bubble lingers or fades.
    #[must_use]
    pub fn needs_frame(&self, now: Instant) -> bool {
        self.failure().is_none() && self.bubble.needs_frame(now)
    }

    /// Wake once to remove a transient, including when reduced motion
    /// disables fades, and when an unconfirmed save next moves along. Keep
    /// an overdue deadline until paint consumes it, so a parked host sees
    /// it.
    #[must_use]
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        if self.failure().is_some() {
            return None;
        }
        let since = self.saving_since;
        let stage = match self.stage {
            _ if self.state != Some(SaveState::Saving) => None,
            Stage::Quiet => since.and_then(|t| t.checked_add(SAVE_NOTICEABLE)),
            Stage::Saving => since.and_then(|t| t.checked_add(SAVE_STALL)),
            Stage::Retrying => since.and_then(|t| t.checked_add(SAVE_GIVE_UP)),
            Stage::NotSaved => self
                .unseen_since
                .filter(|_| !self.notified)
                .and_then(|t| t.checked_add(UNSEEN_GRACE)),
        };
        let fade = (!self.bubble.hovered && !self.bubble.held && !self.bubble.text.is_empty())
            .then(|| self.bubble.shown_at?.checked_add(self.bubble.linger))
            .flatten();
        match (fade, stage) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The pointer moved to `p`; returns whether the hover changed.
    pub fn pointer_moved(&mut self, p: Point) -> bool {
        let before = self.hovered_action();
        self.hover = Some(p);
        before != self.hovered_action()
    }

    fn hovered_action(&self) -> Option<SaveAction> {
        let p = self.hover?;
        if self.gave_up() {
            return self.retry_rect.contains(p).then_some(SaveAction::Retry);
        }
        self.failure()?;
        if self.hit.primary_action_rect.contains(p) {
            Some(SaveAction::Retry)
        } else if self.hit.secondary_action_rect.contains(p) {
            Some(SaveAction::Dismiss)
        } else {
            None
        }
    }

    /// A click at `p`: the action it hits on a showing failure or a save
    /// that gave up. Dismissing hides the failure until the state changes
    /// again.
    pub fn click(&mut self, p: Point) -> Option<SaveAction> {
        self.hover = Some(p);
        let action = self.hovered_action()?;
        self.acted(action);
        Some(action)
    }

    /// Whether `p` is on something the surface acts on (the failure card,
    /// a save's "Try again"), so a host keeps the click.
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        if self.gave_up() {
            return self.retry_rect.contains(p);
        }
        self.failure().is_some() && self.hit.card_rect.contains(p)
    }

    /// The corner of `area` the surface floats in: `area` less its scroll
    /// bar gutter on the trailing edge.
    fn corner(area: Rect, theme: &Theme) -> Rect {
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        Rect::new(area.x0, area.y0, (area.x1 - gutter).max(area.x0), area.y1)
    }

    /// The failure card in `corner`: the reason's two lines over the action
    /// row, a gutter in from the bottom-trailing corner.
    fn failure_rect(corner: Rect, theme: &Theme) -> Rect {
        let s = &theme.tokens.spacing;
        let gutter = f64::from(s.s4);
        let title = f64::from(theme.text(TextRole::Label).line_height_px());
        let body = f64::from(theme.text(TextRole::Caption).line_height_px());
        let actions = f64::from(theme.tokens.metrics.min_pointer_target);
        let height = f64::from(s.s2) * 2.0 + title + body + f64::from(s.s2) + actions;
        let width = f64::from(theme.tokens.metrics.panel_width_utility)
            .min((corner.width() - 2.0 * gutter).max(0.0));
        let x1 = corner.x1 - gutter;
        let y1 = corner.y1 - gutter;
        Rect::new(x1 - width, (y1 - height).max(corner.y0), x1, y1)
    }

    /// Paint what is showing in `area`; returns the painted rect.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        now: Instant,
    ) -> Option<Rect> {
        self.advance(now);
        if self.unseen_since.take().is_some() {
            // The person sees it here; a notification would only repeat it.
            self.withdraw |= self.notified;
            self.notified = false;
        }
        let corner = Self::corner(area, theme);
        self.in_slot = Rect::ZERO;
        if self.gave_up() {
            self.hit = crate::widget::toast::ToastCardHit::default();
            if let Some(rect) = self.paint_gave_up_in_slot(scene, transform, tr, theme) {
                self.in_slot = rect;
                return Some(rect);
            }
            return Some(self.paint_gave_up(scene, transform, tr, theme, corner));
        }
        self.retry_rect = Rect::ZERO;
        let Some((title, reason)) = self
            .failure_text()
            .map(|(title, reason)| (title, reason.to_owned()))
        else {
            self.hit = crate::widget::toast::ToastCardHit::default();
            if !self.bubble.is_visible(now) {
                self.bubble.shown_at = None;
            }
            if self.slot_fits(tr, theme) {
                let painted = self.paint_in_slot(scene, transform, tr, theme, now);
                self.in_slot = painted.unwrap_or_default();
                return painted;
            }
            return self.bubble.paint(scene, transform, tr, theme, corner, now);
        };
        let hovered = self.hovered_action();
        let retry = crate::i18n!("save-status-retry");
        let dismiss = crate::i18n!("save-status-dismiss");
        let card =
            crate::widget::toast::ToastCard::new(&title, crate::widget::toast::ToastKind::Error)
                .body(&reason)
                .actions(
                    &dismiss,
                    crate::icons::Icon::X,
                    hovered == Some(SaveAction::Dismiss),
                    &retry,
                    crate::icons::Icon::RotateCw,
                    hovered == Some(SaveAction::Retry),
                );
        let rect = Self::failure_rect(corner, theme);
        self.hit = crate::widget::toast::paint_card(scene, transform, tr, theme, rect, &card);
        if self.hit.secondary_action_rect.is_zero_area() {
            return None;
        }
        (!self.hit.card_rect.is_zero_area()).then_some(self.hit.card_rect)
    }

    /// Whether the header slot can say anything legible: a caption of four
    /// line heights at least, like the sync status beside it.
    fn slot_fits(&self, tr: &mut TextRenderer, theme: &Theme) -> bool {
        let (_, line) = tr.measure_style("Ag", None, &theme.text(TextRole::Caption));
        self.slot.height() > 0.0 && self.slot.width() >= f64::from(line) * 4.0
    }

    /// The bubble's text as quiet caption text, right-aligned in the header
    /// slot and fading as the bubble does; `None` when there is nothing to
    /// say. See [`Self::slot_fits`].
    fn paint_in_slot(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        now: Instant,
    ) -> Option<Rect> {
        let slot = self.slot;
        let style = theme.text(TextRole::Caption);
        let alpha = self.bubble.alpha_in(theme, now);
        if alpha <= 0.0 || self.bubble.text.is_empty() {
            return None;
        }
        let text = self.bubble.text.clone();
        let (w, _) = tr.measure_style(&text, None, &style);
        let text = if f64::from(w) > slot.width() {
            tr.elide_to_width_style(&text, crate::num::f32_of_f64(slot.width()), &style)
        } else {
            text
        };
        let (w, h) = tr.measure_style(&text, None, &style);
        let (w, h) = (f64::from(w), f64::from(h));
        let rect = Rect::new(
            slot.x1 - w,
            slot.center().y - h / 2.0,
            slot.x1,
            slot.center().y + h / 2.0,
        );
        let faded = alpha < 1.0;
        if faded {
            scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &rect);
        }
        tr.draw_style(
            scene,
            &text,
            theme.tokens.colors.text_dim,
            transform.pre_translate(Vec2::new(rect.x0, rect.y0)),
            None,
            &style,
        );
        if faded {
            scene.pop_layer();
        }
        Some(rect)
    }

    /// "Not saved" after a warning mark, then a compact "Try again", where
    /// the save's progress showed in the header slot. `None` when the slot
    /// cannot hold all three (the bubble floats instead).
    fn paint_gave_up_in_slot(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> Option<Rect> {
        use crate::widget::{Button, ButtonFace, ControlSize};
        let slot = self.slot;
        let label = self.bubble.text().to_owned();
        let retry = crate::i18n!("save-status-retry");
        let style = theme.text(TextRole::Caption);
        let (label_w, label_h) = tr.measure_style(&label, None, &style);
        let face = |rect: Rect| {
            ButtonFace::new(rect, &retry)
                .control_size(ControlSize::Small)
                .icon(crate::icons::Icon::RotateCw)
        };
        let button_w = face(Rect::ZERO).intrinsic_width(tr, theme);
        let button_h = ButtonFace::height(ControlSize::Small, theme);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let mark_gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let gap = f64::from(theme.tokens.spacing.s2);
        let width = icon + mark_gap + f64::from(label_w) + gap + button_w;
        if slot.height() < button_h || slot.width() < width {
            return None;
        }
        let cy = slot.center().y;
        let x0 = slot.x1 - width;
        crate::icons::Icon::TriangleAlert.draw(
            scene,
            transform,
            Rect::new(x0, cy - icon / 2.0, x0 + icon, cy + icon / 2.0),
            theme.tokens.colors.semantic.warning,
        );
        let label_h = f64::from(label_h);
        tr.draw_style(
            scene,
            &label,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(x0 + icon + mark_gap, cy - label_h / 2.0)),
            None,
            &style,
        );
        self.retry_rect = Rect::new(
            slot.x1 - button_w,
            cy - button_h / 2.0,
            slot.x1,
            cy + button_h / 2.0,
        );
        let hovered = self.hovered_action() == Some(SaveAction::Retry);
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &face(self.retry_rect).hovered(hovered),
        );
        Some(Rect::new(
            x0,
            cy - button_h / 2.0,
            slot.x1,
            cy + button_h / 2.0,
        ))
    }

    /// "Not saved" with a compact "Try again" button, in the bubble where
    /// the save's progress showed: the person is looking at the document,
    /// so the answer stays where they read the question.
    fn paint_gave_up(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        corner: Rect,
    ) -> Rect {
        use crate::widget::{Button, ButtonFace, ControlSize};
        let label = self.bubble.text().to_owned();
        let retry = crate::i18n!("save-status-retry");
        let style = pill_style(theme);
        let (label_w, label_h) = tr.measure_style(&label, None, &style);
        let face = |rect: Rect| {
            ButtonFace::new(rect, &retry)
                .control_size(ControlSize::Small)
                .icon(crate::icons::Icon::RotateCw)
        };
        let button_w = face(Rect::ZERO).intrinsic_width(tr, theme);
        let button_h = ButtonFace::height(ControlSize::Small, theme);
        let s = &theme.tokens.spacing;
        let inset = f64::from(s.s1);
        let pad_x = f64::from(s.s3);
        let gap = f64::from(s.s2);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let mark = icon + f64::from(theme.tokens.metrics.icon_label_gap);
        let width = pad_x + mark + f64::from(label_w) + gap + button_w + inset;
        let height = pill_height(theme).max(button_h + 2.0 * inset);
        let gutter = f64::from(s.s4);
        let x1 = corner.x1 - gutter;
        let y1 = corner.y1 - gutter;
        let rect = Rect::new(
            (x1 - width).max(corner.x0),
            (y1 - height).max(corner.y0),
            x1,
            y1,
        );
        crate::widget::floating_surface::paint(scene, transform, rect, theme);
        let ink = theme
            .on_ground(crate::chrome::panel_ground(theme))
            .tokens
            .colors
            .text_primary;
        let cy = rect.center().y;
        crate::icons::Icon::TriangleAlert.draw(
            scene,
            transform,
            Rect::new(
                rect.x0 + pad_x,
                cy - icon / 2.0,
                rect.x0 + pad_x + icon,
                cy + icon / 2.0,
            ),
            theme.tokens.colors.semantic.warning,
        );
        let ty = centered_text_top(rect.y0, rect.height(), f64::from(label_h));
        tr.draw_style(
            scene,
            &label,
            ink,
            transform.pre_translate(Vec2::new(rect.x0 + pad_x + mark, ty)),
            None,
            &style,
        );
        let button_y0 = rect.y0 + (rect.height() - button_h) / 2.0;
        self.retry_rect = Rect::new(
            rect.x1 - inset - button_w,
            button_y0,
            rect.x1 - inset,
            button_y0 + button_h,
        );
        let hovered = self.hovered_action() == Some(SaveAction::Retry);
        Button::paint(
            scene,
            transform,
            tr,
            theme,
            &face(self.retry_rect).hovered(hovered),
        );
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document the tests' notifications name.
    const DOC: Option<&str> = Some("Launch checklist");

    #[test]
    fn status_bubble_geometry_reveal_linger_and_fade_follow_pokes_and_hover() {
        // Case `pill_reservation_matches_the_renderer_line_box_at_supported_scales`.
        {
            for scale in [1.0, 1.5, 2.0, 4.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, measured) = pill_size("Saved", &mut tr, &theme, 0.0);
                let reserved = pill_height(&theme);
                assert!(
                    (reserved - measured).abs() < 1e-3,
                    "layout reservation must equal draw geometry at {scale}x"
                );
            }
            let small = f64::from(Theme::dark().tokens.metrics.bar_control_height);
            assert!(pill_height(&Theme::dark()) >= small);
            assert!(pill_height(&Theme::dark().with_text_scale(2.0)) > pill_height(&Theme::dark()));
        }

        // Case `show_reveals_on_a_change_only_and_floats_in_the_trailing_corner`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            assert!(b.show("Saved", t0), "a new status shows the bubble");
            let later = t0 + Duration::from_millis(1500);
            assert!(!b.is_visible(later));
            assert!(!b.show("Saved", later), "the same status does not reshow");
            assert!(!b.is_visible(later));
            assert!(b.show("Synced", later), "a change does");
            let area = Rect::new(0.0, 0.0, 800.0, 600.0);
            let r = b.rect_in(area, &mut tr, &theme);
            let gutter = f64::from(theme.tokens.spacing.s4);
            assert!((r.x1 - (area.x1 - gutter)).abs() < 1e-9);
            assert!((r.y1 - (area.y1 - gutter)).abs() < 1e-9);
            let mut scene = Scene::new();
            assert_eq!(
                b.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, later),
                Some(r)
            );
            let hidden = later + Duration::from_secs(5);
            assert_eq!(
                b.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, hidden),
                None,
                "a faded bubble paints nothing"
            );
            assert!(b.pointer_moved(area, r.center(), &mut tr, &theme, hidden));
            assert!(
                b.is_visible(hidden + Duration::from_secs(60)),
                "hover pins it"
            );
        }

        // Case `poke_reveals_then_fades_after_linger`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            assert!(!b.is_visible(t0), "hidden until poked");
            assert!(b.poke(t0), "poke reveals and reports the change");
            assert!(b.is_visible(t0), "visible right after a poke");
            assert!((b.alpha(t0) - 1.0).abs() < 1e-6, "fully opaque when fresh");
            // Mid-linger, before the fade tail: still fully opaque.
            assert!((b.alpha(t0 + Duration::from_millis(500)) - 1.0).abs() < 1e-6);
            assert!(
                b.needs_frame(t0 + Duration::from_millis(500)),
                "counts down -> wants frames"
            );
            // Into the fade tail: partial opacity.
            let a = b.alpha(t0 + Duration::from_millis(850));
            assert!(a > 0.0 && a < 1.0, "fades out across the tail: {a}");
            // Past the linger: gone.
            assert!(!b.is_visible(t0 + Duration::from_secs(1)));
            assert!(!b.needs_frame(t0 + Duration::from_secs(1)));
            // Under reduced motion it cuts instead of fading.
            let reduced = Theme {
                reduce_motion: true,
                ..Theme::dark()
            };
            let tail = t0 + Duration::from_millis(850);
            assert!((b.alpha_in(&reduced, tail) - 1.0).abs() < 1e-6);
            assert!(b.alpha_in(&reduced, t0 + Duration::from_secs(1)) <= 0.0);
            assert!(b.alpha_in(&Theme::dark(), tail) < 1.0);
        }

        // Case `hover_pins_open_then_lingers_after_leave`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.set_hover(true, t0);
            // Pinned open regardless of time while the pointer stays over it.
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "hover pins the bubble open"
            );
            assert!(
                !b.needs_frame(t0 + Duration::from_secs(60)),
                "a pinned bubble needs no frames"
            );
            // Leaving starts a fresh linger.
            b.set_hover(false, t0 + Duration::from_secs(60));
            assert!(
                b.is_visible(t0 + Duration::from_secs(60)),
                "still up right after leaving"
            );
            assert!(
                !b.is_visible(t0 + Duration::from_secs(60) + Duration::from_secs(1)),
                "fades a linger after the pointer leaves"
            );
        }

        // Case `a_fresh_poke_restarts_the_countdown`.
        {
            let mut b = StatusBubble::new().with_linger(Duration::from_secs(1));
            let t0 = crate::time::now();
            b.poke(t0);
            // Almost expired…
            assert!(b.is_visible(t0 + Duration::from_millis(900)));
            // …then a new poke restarts the full linger.
            b.poke(t0 + Duration::from_millis(950));
            assert!(
                b.is_visible(t0 + Duration::from_millis(1800)),
                "the second poke extended it"
            );
            assert!(!b.is_visible(t0 + Duration::from_millis(1950)));
        }
    }

    use crate::widget::acknowledgement::ACKNOWLEDGEMENT_HOLD;

    /// A surface whose save has waited long enough to be noticed at `t0`.
    fn noticed_save(t0: Instant) -> SaveStatusSurface {
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Saving, t0);
        surface.advance(t0 + SAVE_NOTICEABLE);
        surface
    }

    #[test]
    fn reduced_motion_expiry_wakes_once_and_is_consumed_by_paint() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::dark().with_reduce_motion(true);
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let t0 = Instant::now();
        let now = t0 + SAVE_NOTICEABLE;
        let mut surface = noticed_save(t0);
        surface.set(SaveState::Saved, now);
        let deadline = surface.next_wake_deadline().expect("saved must expire");
        let mut scene = Scene::new();
        assert!(
            surface
                .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, now)
                .is_some()
        );
        let overdue = deadline + Duration::from_millis(1);
        assert_eq!(
            surface.next_wake_deadline(),
            Some(deadline),
            "a parked host must still observe the due wake"
        );
        scene.reset();
        assert!(
            surface
                .paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, overdue)
                .is_none()
        );
        assert!(
            scene.encoding().is_empty(),
            "the expired chip must leave the actual scene"
        );
        assert_eq!(
            surface.next_wake_deadline(),
            None,
            "consumed expiry must not spin the host"
        );
        surface.set(SaveState::Failed("Disk full".into()), overdue);
        assert_eq!(
            surface.next_wake_deadline(),
            None,
            "a failure stays until an explicit action"
        );
    }

    /// A save that lands before anyone could wonder says nothing; one that
    /// took noticeable time says "Saving…" while it waits and "Saved" for
    /// an acknowledgement's hold, then goes.
    #[test]
    fn saved_shows_briefly_and_only_after_a_save_that_took_noticeable_time() {
        crate::i18n::I18nManager::init("en-US");
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Saving, t0);
        assert!(!surface.is_visible(t0), "a save just begun says nothing");
        assert_eq!(surface.next_wake_deadline(), Some(t0 + SAVE_NOTICEABLE));
        let quick = t0 + Duration::from_millis(80);
        surface.advance(quick);
        surface.set(SaveState::Saved, quick);
        assert!(!surface.is_visible(quick), "a quick save is not confirmed");
        assert_eq!(surface.next_wake_deadline(), None, "and wakes nobody");

        let t1 = quick + Duration::from_secs(5);
        surface.set(SaveState::Saving, t1);
        let noticed = t1 + SAVE_NOTICEABLE;
        assert!(surface.advance(noticed));
        assert_eq!(surface.bubble_text(), "Saving…");
        assert!(
            surface.is_visible(noticed + Duration::from_secs(5)),
            "\"Saving…\" stays while the save waits"
        );
        let landed = noticed + Duration::from_secs(5);
        surface.set(SaveState::Saved, landed);
        assert_eq!(surface.bubble_text(), "Saved");
        assert!(surface.is_visible(landed));
        assert_eq!(
            surface.next_wake_deadline(),
            Some(landed + ACKNOWLEDGEMENT_HOLD)
        );
        assert!(
            !surface.is_visible(landed + ACKNOWLEDGEMENT_HOLD),
            "\"Saved\" holds as long as an acknowledgement, then goes"
        );
    }

    /// An empty document that never changed has nothing to say about
    /// saving, at any time.
    #[test]
    fn a_document_that_never_changed_shows_no_save_state() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Idle, t0);
        let mut scene = Scene::new();
        for later in [Duration::ZERO, SAVE_NOTICEABLE, SAVE_GIVE_UP * 2] {
            assert!(
                surface
                    .paint(
                        &mut scene,
                        Affine::IDENTITY,
                        &mut tr,
                        &theme,
                        area,
                        t0 + later
                    )
                    .is_none()
            );
        }
        assert!(scene.encoding().is_empty(), "nothing painted");
        assert_eq!(surface.next_wake_deadline(), None);
        assert!(surface.accessibility("save").is_none());
    }

    /// Saving and saved are transient bubble text; a failure is an error
    /// card with "Try again" that stays until the save succeeds or it is
    /// dismissed, painted in danger and never as the bubble.
    #[test]
    fn a_failed_save_is_an_error_with_an_action_and_success_is_transient() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let t0 = Instant::now();
        let now = t0 + SAVE_NOTICEABLE;
        let mut surface = noticed_save(t0);
        assert!(surface.set(SaveState::Saved, now));
        assert!(surface.failure().is_none() && surface.is_visible(now));
        let later = now + ACKNOWLEDGEMENT_HOLD + Duration::from_millis(10);
        assert!(!surface.is_visible(later), "saved fades by itself");

        assert!(surface.set(SaveState::Failed("Disk full".into()), later));
        assert_eq!(surface.failure(), Some("Disk full"));
        let much_later = later + DEFAULT_LINGER * 4;
        assert!(surface.is_visible(much_later), "a failure stays");
        assert!(!surface.needs_frame(much_later), "and asks for no frames");
        let mut scene = Scene::new();
        let card = surface
            .paint(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                area,
                much_later,
            )
            .expect("the failure is painted");
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        assert!(
            card.x1 <= area.x1 - gutter,
            "clear of the scroll bar gutter"
        );
        let retry = surface.hit.primary_action_rect;
        assert!(!retry.is_zero_area(), "it offers Try again");
        assert_eq!(surface.click(retry.center()), Some(SaveAction::Retry));
        assert!(
            surface.failure().is_some(),
            "retrying keeps it until the state changes"
        );
        let dismiss = surface.hit.secondary_action_rect;
        assert_eq!(surface.click(dismiss.center()), Some(SaveAction::Dismiss));
        assert!(surface.failure().is_none(), "dismissed");
        surface.set(SaveState::Saving, much_later);
        assert!(surface.advance(much_later + SAVE_NOTICEABLE));
        assert!(surface.is_visible(much_later + SAVE_NOTICEABLE));
    }

    /// On a fake clock: "Saving…", then at ten seconds "Couldn't save yet.
    /// Retrying…" with one more submission, then "Not saved" with "Try
    /// again" in the bubble itself while the document is in view, and no
    /// notification.
    #[test]
    fn an_unconfirmed_save_retries_then_asks_in_place_while_in_view() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut scene = Scene::new();
        let mut paint = |surface: &mut SaveStatusSurface, now| {
            surface.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, now)
        };
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Saving, t0);
        assert!(paint(&mut surface, t0).is_none(), "quiet at first");
        paint(&mut surface, t0 + SAVE_NOTICEABLE).expect("then \"Saving…\"");
        assert_eq!(surface.bubble_text(), "Saving…");
        assert_eq!(
            surface.next_wake_deadline(),
            Some(t0 + SAVE_STALL),
            "a parked window wakes when the wait runs out"
        );
        let almost = t0 + SAVE_STALL - Duration::from_millis(1);
        assert!(!surface.advance(almost));
        assert!(!surface.take_retry(), "nothing to retry yet");

        let stalled = t0 + SAVE_STALL;
        assert_eq!(SAVE_STALL, Duration::from_secs(10));
        assert!(paint(&mut surface, stalled).is_some());
        assert_eq!(surface.bubble_text(), "Couldn't save yet. Retrying…");
        assert!(
            surface.take_retry(),
            "and the host submits the changes again"
        );
        assert!(!surface.take_retry(), "once");
        assert!(surface.accessibility("save").is_none(), "nothing to do yet");
        assert!(
            !surface.set(SaveState::Saving, stalled),
            "the host reporting the same state does not restart the wait"
        );
        assert!(
            surface.is_visible(stalled + Duration::from_secs(5)),
            "it stays while it retries"
        );
        assert_eq!(surface.next_wake_deadline(), Some(t0 + SAVE_GIVE_UP));

        let given_up = t0 + SAVE_GIVE_UP;
        // The frame the wake asks for: the host's own tick first, then paint.
        assert_eq!(surface.take_notice(given_up, DOC, "Notes"), None);
        let bubble = paint(&mut surface, given_up).expect("the bubble is painted");
        assert!(surface.gave_up());
        assert_eq!(surface.bubble_text(), "Not saved");
        assert!(surface.failure().is_none(), "not a toast");
        assert_eq!(surface.hit.card_rect, Rect::ZERO, "no toast was painted");
        let retry = surface.retry_rect;
        assert!(
            bubble.contains(retry.center()),
            "Try again is inside the bubble"
        );
        let node = surface.accessibility("save").unwrap();
        assert_eq!(node.name.as_deref(), Some("Not saved"));
        assert_eq!(node.children[0].name.as_deref(), Some("Try again"));
        assert_eq!(
            surface.take_notice(given_up + UNSEEN_GRACE * 5, DOC, "Notes"),
            None,
            "in view, the bubble says it and nothing is notified"
        );
        assert!(!surface.take_withdrawal());
        assert!(!surface.take_retry(), "the person decides now");
        assert_eq!(surface.next_wake_deadline(), None, "and nothing times out");

        assert!(surface.contains(retry.center()));
        assert_eq!(surface.click(retry.center()), Some(SaveAction::Retry));
        assert!(!surface.gave_up(), "trying again waits afresh");
        let again = given_up + Duration::from_secs(1);
        assert!(paint(&mut surface, again).is_none(), "quietly at first");
        assert!(surface.advance(again + SAVE_NOTICEABLE));
        assert_eq!(surface.bubble_text(), "Saving…");

        assert!(surface.set(SaveState::Saved, again + SAVE_STALL));
        assert_eq!(surface.bubble_text(), "Saved");
        assert!(
            !surface.advance(again + SAVE_GIVE_UP),
            "a confirmed save is done"
        );
        assert!(!surface.gave_up());
    }

    /// Given the header row's room, the save's progress says itself there,
    /// and "Not saved" with its warning mark and "Try again" stays in it:
    /// nothing floats over the document.
    #[test]
    fn a_save_says_itself_in_the_header_row_not_over_the_document() {
        crate::i18n::I18nManager::init("en-US");
        let theme = Theme::default();
        let mut tr = TextRenderer::new();
        let area = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let slot = Rect::new(300.0, 0.0, 1100.0, 48.0);
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set_header_slot(slot);
        surface.set(SaveState::Saving, t0);
        let mut scene = Scene::new();
        let mut at = |surface: &mut SaveStatusSurface, now| {
            surface.paint(&mut scene, Affine::IDENTITY, &mut tr, &theme, area, now)
        };
        let saving = at(&mut surface, t0 + SAVE_NOTICEABLE).expect("Saving… shows");
        assert!(slot.contains(saving.origin()) && saving.x1 <= slot.x1);
        assert!(
            (saving.x1 - slot.x1).abs() < 0.5,
            "against the trailing end"
        );
        let retrying = at(&mut surface, t0 + SAVE_STALL).expect("Retrying… shows");
        assert!(retrying.y1 <= slot.y1 && retrying.x1 <= slot.x1);
        let not_saved = at(&mut surface, t0 + SAVE_GIVE_UP).expect("Not saved shows");
        assert!(surface.gave_up());
        assert!(not_saved.y0 >= slot.y0 && not_saved.y1 <= slot.y1);
        assert!(not_saved.x0 >= slot.x0 && not_saved.x1 <= slot.x1);
        assert!(not_saved.contains(surface.retry_rect.center()));
        assert_eq!(
            surface.click(surface.retry_rect.center()),
            Some(SaveAction::Retry)
        );

        // Too narrow for "Not saved" and "Try again": the corner after all.
        let mut surface = SaveStatusSurface::new();
        surface.set_header_slot(Rect::new(300.0, 0.0, 340.0, 48.0));
        surface.set(SaveState::Saving, t0);
        let floating = at(&mut surface, t0 + SAVE_GIVE_UP).expect("Not saved shows");
        assert!(floating.y1 > slot.y1, "{floating:?} floats in the corner");
    }

    /// A save that gives up while nothing paints the document raises a
    /// notification with "Try again"; it comes down once the document says
    /// it in place, or the save lands.
    #[test]
    fn a_save_that_gives_up_out_of_sight_raises_a_notification() {
        crate::i18n::I18nManager::init("en-US");
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Saving, t0);
        // Out of sight: only the host's tick runs, never paint.
        assert_eq!(surface.take_notice(t0 + SAVE_STALL, DOC, "Notes"), None);
        assert!(
            surface.take_retry(),
            "the retry is asked for out of sight too"
        );
        let given_up = t0 + SAVE_GIVE_UP;
        assert_eq!(
            surface.take_notice(given_up, DOC, "Notes"),
            None,
            "a frame may yet come"
        );
        assert_eq!(surface.next_wake_deadline(), Some(given_up + UNSEEN_GRACE));
        let notice = surface
            .take_notice(given_up + UNSEEN_GRACE, DOC, "Notes")
            .expect("nobody painted it: notify");
        assert_eq!(notice.title, "Launch checklist wasn't saved");
        assert_eq!(notice.body, "Your changes are still in Notes.");
        assert_eq!(notice.action, "Try again");
        assert_eq!(
            surface.take_notice(given_up + UNSEEN_GRACE * 3, DOC, "Notes"),
            None,
            "once"
        );
        assert_eq!(surface.next_wake_deadline(), None);

        surface.retry_from_notice();
        assert!(surface.take_withdrawal(), "trying again takes it down");
        assert!(!surface.gave_up(), "and waits afresh");

        let t1 = given_up + Duration::from_secs(5);
        surface.advance(t1);
        let given_up = t1 + SAVE_GIVE_UP;
        surface.take_notice(given_up, DOC, "Notes");
        let untitled = surface
            .take_notice(given_up + UNSEEN_GRACE, None, "Notes")
            .expect("notified again");
        assert_eq!(untitled.title, "Not saved", "a document without a title");
        surface.set(SaveState::Saved, given_up + UNSEEN_GRACE * 2);
        assert!(surface.take_withdrawal(), "a save that lands takes it down");
    }

    /// The save that ends a failure is news once, however quickly it
    /// landed: the retry's "Saving…" shows nothing until it is noticeable,
    /// the "Saved" after it a transient bubble, and the next quick save is
    /// quiet again.
    #[test]
    fn recovering_from_a_failure_says_saved_once() {
        crate::i18n::I18nManager::init("en-US");
        let t0 = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Failed("Disk full".into()), t0);
        assert!(surface.set(SaveState::Saving, t0), "the failure gives way");
        assert!(!surface.is_visible(t0), "the retry in flight is quiet");
        assert!(surface.set(SaveState::Saved, t0), "the recovery shows");
        assert!(surface.is_visible(t0) && surface.failure().is_none());
        assert_eq!(surface.bubble_text(), "Saved");
        assert!(surface.next_wake_deadline().is_some(), "and hides itself");
        let later = t0 + ACKNOWLEDGEMENT_HOLD + Duration::from_millis(10);
        assert!(!surface.is_visible(later));

        surface.set(SaveState::Saving, later);
        surface.set(SaveState::Saved, later);
        assert!(!surface.is_visible(later), "recovered once, quiet after");

        surface.set(SaveState::Failed("Disk full".into()), later);
        surface.set(SaveState::Saved, later);
        assert!(surface.is_visible(later), "a direct recovery shows too");
        surface.set(SaveState::Saving, later);
        assert!(!surface.is_visible(later), "typing on retires it at once");
        assert_eq!(surface.state(), Some(&SaveState::Saving), "but it is known");
    }

    /// Assistive technology hears what the screen shows: nothing while
    /// autosave cycles, the failure, and the save that ends it.
    #[test]
    fn announcements_follow_what_shows() {
        crate::i18n::I18nManager::init("en-US");
        let now = crate::time::now();
        let mut surface = SaveStatusSurface::new();
        for state in std::iter::repeat_n([SaveState::Saving, SaveState::Saved], 3).flatten() {
            surface.set(state, now);
            assert!(
                surface.accessibility("doc.save").is_none(),
                "autosave is silent"
            );
        }
        surface.set(SaveState::Failed("Disk full".into()), now);
        let failed = surface.accessibility("doc.save").expect("the failure");
        assert_eq!(failed.live, Some(crate::a11y::Politeness::Assertive));
        surface.set(SaveState::Saving, now);
        assert!(
            surface.accessibility("doc.save").is_none(),
            "the retry is quiet"
        );
        surface.set(SaveState::Saved, now);
        let recovered = surface.accessibility("doc.save").expect("the recovery");
        assert_eq!(recovered.live, Some(crate::a11y::Politeness::Polite));
        assert_eq!(
            recovered.name.as_deref(),
            Some(crate::i18n!("save-status-saved").as_str())
        );
        surface.set(SaveState::Saving, now);
        assert!(
            surface.accessibility("doc.save").is_none(),
            "and quiet after"
        );
    }

    #[test]
    fn the_failure_offers_its_actions_to_assistive_technology() {
        crate::i18n::I18nManager::init("en-US");
        let later = Instant::now();
        let mut surface = SaveStatusSurface::new();
        surface.set(SaveState::Saved, later);
        assert!(
            surface.accessibility("doc.save").is_none(),
            "nothing to act on"
        );
        surface.set(SaveState::Failed("Disk full".into()), later);
        let node = surface.accessibility("doc.save").expect("the failure");
        assert_eq!(node.description.as_deref(), Some("Disk full"));
        let targets: Vec<_> = node
            .children
            .iter()
            .filter_map(|child| child.action_target.clone())
            .collect();
        assert_eq!(targets, ["doc.save.retry", "doc.save.dismiss"]);
        assert_eq!(surface.perform("doc.save", "other.retry"), None);
        assert_eq!(
            surface.perform("doc.save", "doc.save.retry"),
            Some(SaveAction::Retry)
        );
        assert_eq!(
            surface.perform("doc.save", "doc.save.dismiss"),
            Some(SaveAction::Dismiss)
        );
        assert!(surface.failure().is_none());
    }
}

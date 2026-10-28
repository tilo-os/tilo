//! Transient notices and the notification card.
//!
//! - [`Transient`] and [`Notice`] keep a window's transient message (a
//!   confirmation, something under way, a failure) for its lifetime and
//!   show it in the window's bar ([`crate::window_toolbar::notify`]); no
//!   window paints a card for one.
//! - [`ToastCard`] and [`paint_card`] paint the shell's notification card,
//!   outside any window: the desktop's notification banners and the lock
//!   screen's.
//! - [`ToastKind`] maps to `Theme::tokens.colors.semantic.*`.

use crate::num::f32_of_f64;
use crate::time::Duration;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use crate::paint::PaintCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::ButtonRole;
use crate::widget::{Button, ButtonFace};

/// Toast cards have a fixed height and therefore paint each text field on a
/// single baseline. Collapse all Unicode whitespace before measuring or
/// eliding a field so an app-provided/localized newline cannot turn a card
/// into an overflowing multi-line block. This also makes a body containing
/// only whitespace behave like an omitted body.
fn single_line_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Severity / category of a toast. Maps to the semantic color ramp in
/// [`crate::tokens::Colors`] for the left accent bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// Whether a transient shown at `shown` for `ttl` has expired. Under the
/// capture clock (`--shots`) nothing expires, so a scenario that stages a
/// toast keeps it in frame.
#[must_use]
pub fn transient_expired(shown: crate::time::Instant, ttl: Duration) -> bool {
    !crate::time::wall_frozen() && crate::time::now().saturating_duration_since(shown) > ttl
}

/// How long a notice stays up: a [`Notice`] or a [`Transient`] shown in the
/// window's bar ([`crate::window_toolbar::notify`]).
pub const NOTICE_LIFETIME: Duration = Duration::from_secs(4);

/// A message an app shows over its content for a while. It is one of two
/// kinds:
///
/// - plain information (a confirmation, a status, a failure, [`Self::show`])
///   stops reporting once it has been up for [`NOTICE_LIFETIME`], so a
///   message nobody takes down still leaves; showing one again starts its
///   time over;
/// - progress (work under way, "Opening book.epub",
///   [`Self::show_progress`]) stays while the app reports the work as still
///   running ([`Self::keep_while`]) and leaves once it no longer does, or a
///   [`NOTICE_LIFETIME`] after it last did, so no stale progress stays.
///
/// The message shows in the window's bar ([`Self::publish`], on every sync):
/// the app asks to be woken at [`Self::deadline`] and, there, renews
/// progress and calls [`Self::expire`]. A message that needs the person to
/// act before it goes is not a transient one.
#[derive(Debug)]
pub struct Transient<T> {
    current: Option<Shown<T>>,
    /// Counts the messages shown, so the bar hears each one once.
    shown: u64,
    /// What [`Self::publish`] put in the window's bar, and which message it
    /// said: taken down once the message goes.
    published: std::cell::RefCell<Option<(u64, crate::window_toolbar::ToolbarNotice)>>,
}

/// A [`Transient`]'s message and the time its lifetime counts from: when it
/// went up, or, for progress, when the work was last reported running.
#[derive(Clone, Debug)]
struct Shown<T> {
    value: T,
    since: crate::time::Instant,
    progress: bool,
    /// Which message this is ([`Transient::shown`]).
    number: u64,
}

impl<T> Default for Transient<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A copy keeps the message but not the bar's notice: the original took it
/// up and takes it down.
impl<T: Clone> Clone for Transient<T> {
    fn clone(&self) -> Self {
        Self {
            current: self.current.clone(),
            shown: self.shown,
            published: std::cell::RefCell::new(None),
        }
    }
}

/// A message gone with its owner leaves the window's bar with it.
impl<T> Drop for Transient<T> {
    fn drop(&mut self) {
        if let Some((_, notice)) = self.published.get_mut().take() {
            crate::window_toolbar::withdraw(&notice);
        }
    }
}

impl<T> Transient<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: None,
            shown: 0,
            published: std::cell::RefCell::new(None),
        }
    }

    /// Show the message in the window's bar while it lasts, as `notice`
    /// says it, and take it down once it has gone; call on every sync. A
    /// notice shown since from elsewhere in the window stays: the newest
    /// replaces the older.
    pub fn publish(&self, notice: impl FnOnce(&T) -> crate::window_toolbar::ToolbarNotice) {
        let want = self
            .current
            .as_ref()
            .filter(|_| self.get().is_some())
            .map(|shown| (shown.number, notice(&shown.value)));
        let mut published = self.published.borrow_mut();
        if *published == want {
            return;
        }
        // The same message said differently replaces itself; another
        // message is news, and replaces whatever is up.
        if let Some((_, gone)) = published.take() {
            crate::window_toolbar::withdraw(&gone);
        }
        if let Some((number, notice)) = want {
            crate::window_toolbar::notify(Some(notice.clone()));
            *published = Some((number, notice));
        }
    }

    /// `value`, shown from now as plain information.
    #[must_use]
    pub fn showing(value: T) -> Self {
        let mut transient = Self::new();
        transient.show(value);
        transient
    }

    /// `value`, shown from now as progress of work under way.
    #[must_use]
    pub fn showing_progress(value: T) -> Self {
        let mut transient = Self::new();
        transient.show_progress(value);
        transient
    }

    /// Show `value` as plain information, replacing anything already up,
    /// for a full lifetime.
    pub fn show(&mut self, value: T) {
        self.put(value, false);
    }

    /// Show `value` as progress of work under way, replacing anything
    /// already up: it stays while [`Self::keep_while`] reports the work
    /// running.
    pub fn show_progress(&mut self, value: T) {
        self.put(value, true);
    }

    fn put(&mut self, value: T, progress: bool) {
        self.shown = self.shown.wrapping_add(1);
        self.current = Some(Shown {
            value,
            since: crate::time::now(),
            progress,
            number: self.shown,
        });
    }

    /// Report whether the work a progress message follows is still running,
    /// on every sync: while it is the message stays, once it is not the
    /// message goes. Plain information keeps its own time. Whether a
    /// message went.
    pub fn keep_while(&mut self, busy: bool) -> bool {
        let Some(shown) = self.current.as_mut().filter(|shown| shown.progress) else {
            return false;
        };
        if busy {
            shown.since = crate::time::now();
            return false;
        }
        self.dismiss()
    }

    /// Whether the message up is progress of work under way.
    #[must_use]
    pub fn is_progress(&self) -> bool {
        self.get().is_some() && self.current.as_ref().is_some_and(|shown| shown.progress)
    }

    /// Take the message down; whether one was up.
    pub fn dismiss(&mut self) -> bool {
        self.current.take().is_some()
    }

    /// The message, while its lifetime lasts.
    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.current
            .as_ref()
            .filter(|shown| !transient_expired(shown.since, NOTICE_LIFETIME))
            .map(|shown| &shown.value)
    }

    /// The message borrowed through its `Deref` (a `String` as a `&str`).
    #[must_use]
    pub fn as_deref(&self) -> Option<&T::Target>
    where
        T: std::ops::Deref,
    {
        self.get().map(std::ops::Deref::deref)
    }

    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.get().is_some()
    }

    /// When the message's time started, while it is up: when it went up,
    /// or, for progress, when the work was last reported running.
    #[must_use]
    pub fn since(&self) -> Option<crate::time::Instant> {
        self.get()?;
        self.current.as_ref().map(|shown| shown.since)
    }

    /// Drop the message once its lifetime is over; whether it did.
    pub fn expire(&mut self) -> bool {
        self.current.is_some() && self.get().is_none() && self.dismiss()
    }

    /// When the message leaves (progress: when it must next be renewed),
    /// for a widget's `next_wake_deadline`; none once it has, so a widget
    /// that has not yet [expired](Self::expire) it is not woken over and
    /// over. Under the capture clock nothing leaves.
    #[must_use]
    pub fn deadline(&self) -> Option<crate::time::Instant> {
        if crate::time::wall_frozen() {
            return None;
        }
        self.since().map(|shown| shown + NOTICE_LIFETIME)
    }
}

/// The one transient message an app shows in its window's bar: a
/// confirmation, something under way, or a failure. The app shows it and
/// [publishes](Self::publish) it on every sync; the notice keeps its own
/// lifetime ([`Transient`]), and the bar announces it.
#[derive(Clone, Debug, Default)]
pub struct Notice {
    current: Transient<(String, ToastKind)>,
}

impl Notice {
    /// Show `text` as plain information, replacing any notice already up.
    pub fn show(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.current.show((text.into(), kind));
    }

    /// Show `text` as progress of work under way, replacing any notice
    /// already up; see [`Transient::show_progress`].
    pub fn show_progress(&mut self, text: impl Into<String>) {
        self.current.show_progress((text.into(), ToastKind::Info));
    }

    /// Report whether the work a progress notice follows is still running;
    /// see [`Transient::keep_while`]. Whether the notice went.
    pub fn keep_while(&mut self, busy: bool) -> bool {
        self.current.keep_while(busy)
    }

    /// Take the notice down; whether one was up.
    pub fn dismiss(&mut self) -> bool {
        self.current.dismiss()
    }

    /// The notice's text and kind, while it is up.
    pub fn get(&self) -> Option<(&str, ToastKind)> {
        self.current
            .get()
            .map(|(text, kind)| (text.as_str(), *kind))
    }

    pub fn is_shown(&self) -> bool {
        self.current.is_shown()
    }

    /// Take the notice down once its lifetime is over; whether it did.
    pub fn expire(&mut self) -> bool {
        self.current.expire()
    }

    /// When the notice expires, for a widget's `next_wake_deadline`. Under
    /// the capture clock nothing expires.
    pub fn deadline(&self) -> Option<crate::time::Instant> {
        self.current.deadline()
    }

    /// Show the notice in the window's bar while it is up; call on every
    /// sync. See [`Transient::publish`].
    pub fn publish(&self) {
        self.current
            .publish(|(text, kind)| crate::window_toolbar::ToolbarNotice::new(text.clone(), *kind));
    }
}

/// How long a transient card has been up for someone to read, which is
/// what its lifetime counts against. The clock runs only while the card is
/// on screen and nobody holds it (the pointer on it, keyboard focus in it):
/// a card that never fit on screen, or one being read, never runs out under
/// the reader. The desktop's notification banners keep it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dwell {
    /// Time on screen, unheld, before the current run.
    spent: Duration,
    /// When the current run began; `None` while the clock is stopped.
    since: Option<crate::time::Instant>,
    /// When the card first came on screen.
    first_shown: Option<crate::time::Instant>,
}

impl Dwell {
    /// A card nobody has seen yet: nothing spent, the clock stopped.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spent: Duration::ZERO,
            since: None,
            first_shown: None,
        }
    }

    /// Time the card has been readable on screen at `now`.
    #[must_use]
    pub fn spent(&self, now: crate::time::Instant) -> Duration {
        self.since.map_or(self.spent, |since| {
            self.spent
                .saturating_add(now.saturating_duration_since(since))
        })
    }

    #[must_use]
    pub fn left(&self, lifetime: Duration, now: crate::time::Instant) -> Duration {
        lifetime.saturating_sub(self.spent(now))
    }

    /// Whether a `lifetime` has run out at `now`.
    #[must_use]
    pub fn is_over(&self, lifetime: Duration, now: crate::time::Instant) -> bool {
        self.left(lifetime, now).is_zero()
    }

    /// Say whether the card is `on_screen` and whether someone `held` it
    /// at `now`. The clock runs exactly while it is on screen and unheld.
    pub fn present(&mut self, now: crate::time::Instant, on_screen: bool, held: bool) {
        if on_screen {
            self.first_shown.get_or_insert(now);
        }
        match (on_screen && !held, self.since) {
            (true, None) => self.since = Some(now),
            (false, Some(_)) => {
                self.spent = self.spent(now);
                self.since = None;
            }
            _ => {}
        }
    }

    /// Count `elapsed` against the card, for a host that advances by
    /// frame deltas rather than reading a clock.
    pub fn spend(&mut self, elapsed: Duration) {
        self.spent = self.spent.saturating_add(elapsed);
    }

    /// Leave at least `floor` of a `lifetime` at `now`: a card the person
    /// just let go of gets a moment before it starts to leave, instead of
    /// fading the instant the pointer moves off it.
    pub fn grant(&mut self, lifetime: Duration, now: crate::time::Instant, floor: Duration) {
        let cap = lifetime.saturating_sub(floor);
        if self.spent(now) > cap {
            self.spent = cap;
            if self.since.is_some() {
                self.since = Some(now);
            }
        }
    }

    /// When a `lifetime` runs out while the clock keeps running; `None`
    /// while it is stopped.
    #[must_use]
    pub fn ends_at(&self, lifetime: Duration) -> Option<crate::time::Instant> {
        let since = self.since?;
        since.checked_add(lifetime.saturating_sub(self.spent))
    }

    /// When the card first came on screen; `None` before it has.
    #[must_use]
    pub fn first_shown(&self) -> Option<crate::time::Instant> {
        self.first_shown
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.since.is_some()
    }
}

/// The radius of a floating card: a notification banner, a toast, and the
/// status cards that stand in a column with them. One floating card is one
/// shape everywhere, the floating Panel's (`radii.panel`), so a banner, the
/// palette and a menu never disagree about their corners.
#[must_use]
pub fn card_radius(theme: &Theme) -> f64 {
    crate::widget::floating_surface::radius(theme)
}

/// The card's padding: `(x, y)`.
fn card_pad(theme: &Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.spacing.s3),
        f64::from(theme.tokens.spacing.s2),
    )
}

/// The severity icon, the same one a banner of that kind shows.
pub(crate) fn kind_icon(kind: ToastKind) -> crate::icons::Icon {
    match kind {
        ToastKind::Info => crate::widget::BannerKind::Info,
        ToastKind::Success => crate::widget::BannerKind::Success,
        ToastKind::Warning => crate::widget::BannerKind::Warning,
        ToastKind::Error => crate::widget::BannerKind::Error,
    }
    .default_icon()
}

/// Width of a card's leading column after its padding: the severity icon
/// and the gap to the text.
/// The side of the card's leading mark, if it has one: a person's avatar
/// is the size of a list row's, a glyph an icon's.
fn leading_side(theme: &Theme, card: &ToastCard<'_>) -> Option<f64> {
    if card.kind == ToastKind::Info && card.person.is_some() {
        return Some(crate::widget::AvatarSize::Regular.side(theme));
    }
    (card.kind != ToastKind::Info || card.icon.is_some() || card.app_image.is_some())
        .then(|| f64::from(theme.tokens.metrics.icon_md))
}

/// The width the leading mark takes before the text, gap included; none
/// for a card without a mark.
fn leading_column(theme: &Theme, card: &ToastCard<'_>) -> f64 {
    leading_side(theme, card).map_or(0.0, |side| {
        side + f64::from(theme.tokens.metrics.icon_label_gap)
    })
}

pub(crate) fn kind_color(kind: ToastKind, theme: &Theme) -> vello::peniko::Color {
    match kind {
        ToastKind::Info => theme.tokens.colors.semantic.info,
        ToastKind::Success => theme.tokens.colors.semantic.success,
        ToastKind::Warning => theme.tokens.colors.semantic.warning,
        ToastKind::Error => theme.tokens.colors.semantic.danger,
    }
}

// ---------------------------------------------------------------------------
// Shared notification-card renderer
// ---------------------------------------------------------------------------
//
// One immediate-mode painter for a notification card, for the shell's
// notification stack (`tilo_desktop::notifications::NotificationStack`) and
// the lock screen: outside any window.

/// Immediate-mode content for one notification / toast card, painted by
/// [`paint_card`]. Built fluently:
/// `ToastCard::new("Slow network", ToastKind::Warning).body("retrying")`.
#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent immediate-mode card affordances are supplied by the caller"
)]
pub struct ToastCard<'a> {
    /// Primary line — the summary. Semibold, `text_primary`.
    pub title: &'a str,
    /// Optional secondary line (dim). Empty = single-line card.
    pub body: &'a str,
    /// Optional attribution caption above the title (e.g. the sending app's
    /// name), a dim `Caption` in sentence case. Empty = no caption.
    pub caption: &'a str,
    /// Severity — drives the leading icon and its colour.
    pub kind: ToastKind,
    /// What plain information leads with instead of a severity glyph,
    /// which only warnings, errors and successes carry: a glyph naming
    /// what the card is about...
    pub icon: Option<crate::icons::Icon>,
    /// The person the card is from, as `(name, identity)`: their avatar
    /// leads the card, in their presence colour, ahead of any glyph. Plain
    /// information with neither a person nor an icon leads with its text.
    pub person: Option<(&'a str, &'a str)>,
    /// The sending application's own artwork, where a glyph cannot name it.
    /// A person's avatar leads ahead of it; a severity glyph wins over it,
    /// so an alert never loses its state colour.
    pub app_image: Option<&'a crate::icons::AppImage>,
    /// Enter/exit animation opacity in `[0, 1]`; `1.0` for a settled card.
    pub alpha: f32,
    /// Draw a dismiss (`X`) button at the top-right and hand its rect back
    /// via [`ToastCardHit`] so the caller keeps one hit-test on widget-owned
    /// geometry.
    pub closable: bool,
    /// Hover state for the dismiss button, when the caller tracks the cursor.
    pub close_hovered: bool,
    /// Whether the shared pointer controller has armed this control.
    pub close_pressed: bool,
    /// Draw an "actionable" chevron at the trailing edge (the card carries
    /// activatable actions).
    pub actionable: bool,
    /// Optional left/secondary and right/primary actions. These use the same
    /// immediate-mode [`Button`] painter as ordinary retained controls, so a
    /// notification can ask for a decision without inventing a second button
    /// language. Empty labels omit the action row entirely.
    pub secondary_action: &'a str,
    pub secondary_action_icon: Option<crate::icons::Icon>,
    pub secondary_action_hovered: bool,
    /// Whether the shared pointer controller has armed this control.
    pub secondary_action_pressed: bool,
    pub primary_action: &'a str,
    pub primary_action_icon: Option<crate::icons::Icon>,
    pub primary_action_hovered: bool,
    /// Whether the shared pointer controller has armed this control.
    pub primary_action_pressed: bool,
    /// How far the work the card follows is, `0..=1`: a determinate bar
    /// under the text. `None` draws none.
    pub progress: Option<f32>,
}

impl<'a> ToastCard<'a> {
    pub fn new(title: &'a str, kind: ToastKind) -> Self {
        Self {
            title,
            body: "",
            caption: "",
            kind,
            icon: None,
            person: None,
            app_image: None,
            alpha: 1.0,
            closable: false,
            close_hovered: false,
            close_pressed: false,
            actionable: false,
            secondary_action: "",
            secondary_action_icon: None,
            secondary_action_hovered: false,
            secondary_action_pressed: false,
            primary_action: "",
            primary_action_icon: None,
            primary_action_hovered: false,
            primary_action_pressed: false,
            progress: None,
        }
    }
    #[must_use]
    pub fn body(mut self, body: &'a str) -> Self {
        self.body = body;
        self
    }
    /// Follow work `progress` (`0..=1`) done with a determinate bar.
    #[must_use]
    pub fn progress(mut self, progress: f32) -> Self {
        self.progress = Some(progress);
        self
    }
    /// Lead plain information with `icon` (see [`Self::icon`]).
    #[must_use]
    pub fn icon(mut self, icon: Option<crate::icons::Icon>) -> Self {
        self.icon = icon;
        self
    }
    /// Lead the card with the avatar of the person it is from.
    #[must_use]
    pub fn person(mut self, name: &'a str, identity: &'a str) -> Self {
        self.person = Some((name, identity));
        self
    }
    /// Lead plain information with the sending application's artwork.
    #[must_use]
    pub fn app_image(mut self, image: Option<&'a crate::icons::AppImage>) -> Self {
        self.app_image = image;
        self
    }
    #[must_use]
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = caption;
        self
    }
    #[must_use]
    pub fn alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }
    #[must_use]
    pub fn closable(mut self, v: bool) -> Self {
        self.closable = v;
        self
    }
    #[must_use]
    pub fn close_hovered(mut self, v: bool) -> Self {
        self.close_hovered = v;
        self
    }
    #[must_use]
    pub fn actionable(mut self, v: bool) -> Self {
        self.actionable = v;
        self
    }
    /// One action, at the card's trailing edge beside its text ("Undo",
    /// "Open").
    #[must_use]
    pub fn action(mut self, label: &'a str, hovered: bool) -> Self {
        self.secondary_action = "";
        self.secondary_action_icon = None;
        self.primary_action = label;
        self.primary_action_icon = None;
        self.primary_action_hovered = hovered;
        self
    }

    /// Whether the card offers something to do: an action, or work under
    /// way it follows. Only such a card may stay up until it is taken down;
    /// any other leaves after [`NOTICE_LIFETIME`].
    #[must_use]
    pub fn stays_up(&self) -> bool {
        self.actionable
            || !self.primary_action.is_empty()
            || !self.secondary_action.is_empty()
            || self.progress.is_some()
    }

    /// Whether the card carries only one action, beside its text.
    fn single_action(&self) -> bool {
        self.secondary_action.is_empty() && !self.primary_action.is_empty()
    }

    /// Add a standard two-choice action row. The secondary action is painted
    /// neutral on the left; the primary action is painted with the theme's
    /// primary face on the right.
    #[must_use]
    pub fn actions(
        mut self,
        secondary: &'a str,
        secondary_icon: crate::icons::Icon,
        secondary_hovered: bool,
        primary: &'a str,
        primary_icon: crate::icons::Icon,
        primary_hovered: bool,
    ) -> Self {
        self.secondary_action = secondary;
        self.secondary_action_icon = Some(secondary_icon);
        self.secondary_action_hovered = secondary_hovered;
        self.primary_action = primary;
        self.primary_action_icon = Some(primary_icon);
        self.primary_action_hovered = primary_hovered;
        self
    }
}

/// Geometry [`paint_card`] hands back so callers hit-test the card's
/// interactive regions against widget-owned rects (no bespoke maths).
#[derive(Clone, Copy, Debug, Default)]
pub struct ToastCardHit {
    /// The painted card rect. `Rect::ZERO` when the card was not painted.
    pub card_rect: Rect,
    /// The dismiss (`X`) button rect. `Rect::ZERO` when `closable` was unset.
    pub close_rect: Rect,
    /// Left/secondary action rect. `Rect::ZERO` when the card has no actions.
    pub secondary_action_rect: Rect,
    /// Right/primary action rect. `Rect::ZERO` when the card has no actions.
    pub primary_action_rect: Rect,
}

/// Minimum height for a compact card with one elided line per text field.
/// Uses the same normalized text, roles, padding and shared action controls as
/// `paint_card`. A host choosing a fixed-width banner must reserve this height
/// before stacking cards; enlarging type must not paint text over its actions.
pub fn minimum_card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
) -> f64 {
    let (_, pad_y) = card_pad(theme);
    let text_height: f64 = [
        (card.caption, theme.text(TextRole::Caption)),
        (card.title, theme.text(TextRole::Label).strong()),
        (card.body, theme.text(TextRole::Caption)),
    ]
    .into_iter()
    .map(|(text, style)| {
        let line = single_line_text(text);
        if line.is_empty() {
            0.0
        } else {
            f64::from(text_renderer.measure_style(&line, None, &style).1)
        }
    })
    .sum();
    let control_height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let mut height = text_height
        .max(leading_side(theme, card).unwrap_or(f64::from(theme.tokens.metrics.icon_md)))
        .max(if card.closable || card.single_action() {
            control_height
        } else {
            0.0
        })
        + 2.0 * pad_y;
    if !card.secondary_action.is_empty() && !card.primary_action.is_empty() {
        height += control_height + pad_y + choice_lead(theme);
    }
    height.ceil()
}

/// Height of a card `width` wide whose body may wrap to a second line, as
/// a notification's line of context often needs to. A body longer than
/// that keeps the compact height and is elided to one line.
pub fn card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
    width: f64,
) -> f64 {
    let compact = minimum_card_height(text_renderer, theme, card);
    let body = single_line_text(card.body);
    if body.is_empty() {
        return compact;
    }
    let (pad_x, _) = card_pad(theme);
    let trailing = if card.closable {
        f64::from(theme.tokens.spacing.s2)
            + ButtonFace::height(crate::widget::ControlSize::Small, theme)
            + f64::from(theme.tokens.spacing.s1)
    } else {
        pad_x
    };
    let choices = !card.secondary_action.is_empty() && !card.primary_action.is_empty();
    if card.single_action() && single_action_inline(text_renderer, theme, card, width) {
        return compact;
    }
    if card.single_action() || choices {
        // The actions end the card, so the text has the card's width and
        // wraps there whole rather than eliding: a card that asks the
        // person to choose must say all of what they choose between.
        let control_height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
        let (_, pad_y) = card_pad(theme);
        let text_width =
            f32_of_f64((width - leading_column(theme, card) - 2.0 * pad_x - trailing).max(1.0));
        let block: f64 = [
            (card.caption, theme.text(TextRole::Caption)),
            (card.title, theme.text(TextRole::Label).strong()),
            (card.body, theme.text(TextRole::Caption)),
        ]
        .into_iter()
        .map(|(text, style)| {
            let text = single_line_text(text);
            if text.is_empty() {
                0.0
            } else {
                f64::from(
                    text_renderer
                        .measure_style(&text, Some(text_width), &style)
                        .1,
                )
            }
        })
        .sum();
        let wrapped = block
            .max(leading_side(theme, card).unwrap_or(0.0))
            .max(if card.closable { control_height } else { 0.0 })
            + 2.0 * pad_y
            + choice_lead(theme)
            + choice_height(theme)
            + choice_foot(theme);
        // `compact` already holds a pair of choices; a single action that
        // moved under the text adds its row.
        let least = if choices {
            compact
        } else {
            compact + control_height + pad_y + choice_lead(theme)
        };
        return wrapped.max(least).ceil();
    }
    let text_width = width - leading_column(theme, card) - pad_x - trailing;
    if text_width <= 0.0 {
        return compact;
    }
    let style = theme.text(TextRole::Caption);
    let one = f64::from(text_renderer.measure_style(&body, None, &style).1);
    let wrapped = f64::from(
        text_renderer
            .measure_style(&body, Some(f32_of_f64(text_width)), &style)
            .1,
    );
    if wrapped <= one * 1.5 || wrapped > one * 2.5 {
        return compact;
    }
    (compact + wrapped - one).ceil()
}

/// Paint a toast / notification card into `rect` and return the geometry a
/// caller needs to route clicks. The card is width/height-agnostic (it lays
/// out relative to `rect`), so a 48px single-line in-app toast and a 64px
/// three-line OS notification share this one renderer.
///
/// Layout: soft drop shadow → the floating material + edge → leading
/// severity icon → (optional) attribution caption / title / body text column →
/// (optional) ghost dismiss button → (optional) actionable chevron. Every
/// colour is modulated by `card.alpha` so the caller drives the enter/exit
/// fade without a second painter.
pub fn paint_card(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
) -> ToastCardHit {
    let mut hit = ToastCardHit::default();
    let a = card.alpha.clamp(0.0, 1.0);
    if rect.width() <= 0.0 || rect.height() <= 0.0 || a <= 0.0 {
        return hit;
    }
    hit.card_rect = rect;
    let (pad_x, pad_y) = card_pad(theme);
    paint_card_surface(scene, transform, rect, a, theme);

    let text_x0 = rect.x0 + leading_column(theme, card);

    // Dismiss (X) button — a ghost `Button` face, top-right.
    let mut text_right = if let Some(close_rect) =
        paint_dismiss_button(scene, transform, text_renderer, theme, rect, card, a)
    {
        hit.close_rect = close_rect;
        close_rect.x0 - f64::from(theme.tokens.spacing.s1)
    } else {
        rect.x1 - pad_x
    };
    let mut action_top = None;
    if card.single_action() {
        if single_action_inline(text_renderer, theme, card, rect.width()) {
            let action = paint_single_action(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(rect.x0, rect.y0, text_right, rect.y1),
                card,
                a,
            );
            hit.primary_action_rect = action;
            text_right = action.x0 - f64::from(theme.tokens.spacing.s3);
        } else {
            // Beside the text it would squeeze the text to nothing: it
            // ends the card instead, where a pair of choices would.
            let y1 = rect.y1 - choice_foot(theme);
            let y0 = y1 - choice_height(theme);
            let x1 = if card.closable {
                rect.x1 - pad_x
            } else {
                rect.x1
            };
            hit.primary_action_rect = paint_single_action(
                scene,
                transform,
                text_renderer,
                theme,
                Rect::new(rect.x0, y0, x1, y1),
                card,
                a,
            );
            action_top = Some(y0 - choice_lead(theme));
        }
    }

    // Text column: optional attribution caption, then the title, then the
    // optional body.
    let action_top = action_top.or(paint_card_actions(
        scene,
        transform,
        text_renderer,
        theme,
        rect,
        card,
        a,
        &mut hit,
    ));
    let text_bottom = action_top.unwrap_or(rect.y1) - progress_reserve(theme, card);
    let block = paint_card_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        Rect::new(text_x0, rect.y0, text_right, text_bottom),
        card,
        a,
    );
    // The work's progress: the standard determinate bar under the text,
    // as wide as the text may be.
    if let Some(progress) = card.progress {
        let y0 = block.bottom + f64::from(theme.tokens.spacing.s2);
        let bar = Rect::new(
            text_x0 + pad_x,
            y0,
            text_right,
            y0 + crate::widget::ProgressBarFace::height(theme),
        );
        let faded = a < 0.999;
        if faded {
            scene.push_layer(Fill::NonZero, BlendMode::default(), a, transform, &bar);
        }
        crate::widget::ProgressBarFace::new(progress).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            bar,
        );
        if faded {
            scene.pop_layer();
        }
    }

    // The severity is its icon's colour, as on a banner (HIG §9.5): the
    // at-a-glance state form, never a stripe crowding the panel's corner.
    // It sits on the title's line, where the eye starts reading, not in the
    // middle of a card that also holds a caption or actions. Colour is kept
    // for states that need attention; plain information stays neutral, so
    // a calm stack of notices never reads as a column of alerts.
    paint_leading(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        rect.x0 + pad_x,
        block,
        card,
        a,
    );

    // Actionable chevron at the trailing-bottom edge, keyed to severity:
    // only for a card the whole of which opens something. One that shows
    // its choices as buttons says it already, and the chevron would sit on
    // the last of them.
    if card.actionable && card.primary_action.is_empty() {
        let sz = f64::from(theme.tokens.metrics.icon_sm);
        let ix = rect.x1 - pad_x - sz;
        let iy = rect.y1 - pad_y - sz;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::new(ix, iy, ix + sz, iy + sz),
            crate::icons::Icon::ChevronRight,
            kind_color(card.kind, theme).with_alpha(a),
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }

    hit
}

/// The card's leading mark at `left`: a severity glyph for anything that
/// needs attention; for plain information whom it is from (their avatar,
/// level with the top of the text, as a message reads), else a glyph for
/// what it is about, on the title's line. A generic "i" or an app's letter
/// on every card would say nothing, so a card with neither leads with text.
fn paint_leading(
    ctx: &mut PaintCtx<'_>,
    left: f64,
    block: TextBlock,
    card: &ToastCard<'_>,
    alpha: f32,
) {
    let theme = ctx.theme;
    let Some(side) = leading_side(theme, card) else {
        return;
    };
    if let (ToastKind::Info, Some((name, identity))) = (card.kind, card.person) {
        // Top-aligned with a block at least as tall as the avatar; centred
        // on a shorter one, so a one-line card is not top-heavy.
        let centre_y = if block.bottom - block.top >= side {
            block.top + side / 2.0
        } else {
            (block.top + block.bottom) / 2.0
        };
        let rect = Rect::from_center_size(Point::new(left + side / 2.0, centre_y), (side, side));
        let faded = alpha < 0.999;
        if faded {
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                ctx.transform,
                &rect,
            );
        }
        crate::widget::AvatarFace::new(name, identity).paint(ctx, rect);
        if faded {
            ctx.scene.pop_layer();
        }
        return;
    }
    let mark = Rect::from_center_size(
        Point::new(left + side / 2.0, block.title_centre),
        (side, side),
    );
    if card.kind == ToastKind::Info
        && let Some(image) = card.app_image
    {
        let faded = alpha < 0.999;
        if faded {
            ctx.scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                alpha,
                ctx.transform,
                &mark,
            );
        }
        let painted = crate::icons::draw_app_image(ctx.scene, ctx.transform, mark, image);
        if faded {
            ctx.scene.pop_layer();
        }
        if painted {
            return;
        }
    }
    let (icon, color) = match (card.kind, card.icon) {
        (ToastKind::Info, Some(icon)) => (icon, theme.tokens.colors.text_dim),
        (ToastKind::Info, None) => return,
        (kind, _) => (kind_icon(kind), kind_color(kind, theme)),
    };
    crate::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        mark,
        icon,
        color.with_alpha(alpha),
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

/// Where a card's text landed: the centre of the title's first line and
/// the block's vertical extent, for the leading mark to line up with.
#[derive(Clone, Copy, Debug)]
struct TextBlock {
    title_centre: f64,
    top: f64,
    bottom: f64,
}

fn paint_card_actions(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
    hit: &mut ToastCardHit,
) -> Option<f64> {
    if card.secondary_action.is_empty() || card.primary_action.is_empty() {
        return None;
    }
    let (pad_x, _) = card_pad(theme);
    let left = rect.x0 + pad_x;
    let right = rect.x1 - pad_x;
    let y1 = rect.y1 - choice_foot(theme);
    let y0 = y1 - choice_height(theme);
    let (secondary, primary) = choice_rects(
        Rect::new(left, y0, right, y1),
        choice_gap(theme),
        choice_width(
            text_renderer,
            theme,
            card.secondary_action,
            card.secondary_action_icon,
        ),
        choice_width(
            text_renderer,
            theme,
            card.primary_action,
            card.primary_action_icon,
        ),
    );
    hit.secondary_action_rect = secondary;
    hit.primary_action_rect = primary;

    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &RoundedRect::from_rect(
                Rect::new(secondary.x0, y0, right, y1),
                f64::from(theme.tokens.radii.control),
            ),
        );
    }
    let mut secondary_face = ButtonFace::new(secondary, card.secondary_action)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(card.secondary_action_hovered)
        .pressed(card.secondary_action_pressed);
    if let Some(icon) = card.secondary_action_icon {
        secondary_face = secondary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &secondary_face);
    let mut primary_face = ButtonFace::new(primary, card.primary_action)
        .role(ButtonRole::Primary)
        .control_size(crate::widget::ControlSize::Small)
        .hovered(card.primary_action_hovered)
        .pressed(card.primary_action_pressed);
    if let Some(icon) = card.primary_action_icon {
        primary_face = primary_face.icon(icon);
    }
    Button::paint(scene, transform, text_renderer, theme, &primary_face);
    if faded {
        scene.pop_layer();
    }
    Some(y0 - choice_lead(theme))
}

/// The height of a card's choices: compact controls, so a card reads as
/// its message first.
pub fn choice_height(theme: &Theme) -> f64 {
    ButtonFace::height(crate::widget::ControlSize::Small, theme)
}

/// The step between a card's text and its choices: a full related step,
/// so the choices read as the message's answer rather than part of it.
pub fn choice_lead(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

/// The padding under a card's choices: the card's own vertical padding.
pub fn choice_foot(theme: &Theme) -> f64 {
    card_pad(theme).1
}

/// The space between a card's two choices.
pub fn choice_gap(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.rhythm.related)
}

/// The width a card's choice asks for: its label (and icon) on a compact
/// face, never narrower than a comfortable target.
pub fn choice_width(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    icon: Option<crate::icons::Icon>,
) -> f64 {
    let mut face =
        ButtonFace::new(Rect::ZERO, label).control_size(crate::widget::ControlSize::Small);
    if let Some(icon) = icon {
        face = face.icon(icon);
    }
    face.intrinsic_width(text_renderer, theme)
        .max(2.0 * choice_height(theme))
}

/// Where a pair of choices sits on `band` given the widths they ask for:
/// at the trailing edge at their own width, the answer last, as a dialog's
/// do, so the card reads as a sentence that ends in its choices rather than
/// a pair of slabs. Equal halves only when both cannot fit at their width.
/// Returns `(secondary, primary)`.
pub fn choice_rects(band: Rect, gap: f64, secondary: f64, primary: f64) -> (Rect, Rect) {
    if primary + gap + secondary <= band.width() {
        let primary = Rect::new(band.x1 - primary, band.y0, band.x1, band.y1);
        let x1 = primary.x0 - gap;
        (Rect::new(x1 - secondary, band.y0, x1, band.y1), primary)
    } else {
        let mid = (band.x0 + band.x1 - gap) / 2.0;
        (
            Rect::new(band.x0, band.y0, mid, band.y1),
            Rect::new(mid + gap, band.y0, band.x1, band.y1),
        )
    }
}

/// The card's body: the one floating card
/// ([`crate::widget::floating_surface::paint_card`]), fading with the card.
///
/// Anything that stands in a column of cards (a locked-screen stand-in for
/// them, the card for the ones that did not fit) paints this same body, so
/// the column is one material and one shape, and the palette's.
pub fn paint_card_surface(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    alpha: f32,
    theme: &Theme,
) {
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &crate::widget::floating_surface::painted_bounds(rect, theme),
        );
    }
    crate::widget::floating_surface::paint_card(scene, transform, rect, theme);
    if faded {
        scene.pop_layer();
    }
}

/// A card's ground and its edge: the floating card's opaque panel ground
/// and the panel edge.
#[must_use]
pub fn card_ground(theme: &Theme) -> (vello::peniko::Color, vello::peniko::Color) {
    (
        crate::widget::floating_surface::card_ground(theme),
        crate::chrome::panel_border(theme),
    )
}

/// The width of a card's single action face.
/// Whether a card `width` wide keeps its one action beside the text: only
/// while the text keeps room to read, its whole title or some eight lines'
/// height of width. Large text in a narrow column moves the action under
/// the text instead.
fn single_action_inline(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
    width: f64,
) -> bool {
    let (pad_x, _) = card_pad(theme);
    let trailing = if card.closable {
        f64::from(theme.tokens.spacing.s2)
            + ButtonFace::height(crate::widget::ControlSize::Small, theme)
            + f64::from(theme.tokens.spacing.s1)
    } else {
        pad_x
    };
    let room = width
        - leading_column(theme, card)
        - pad_x
        - trailing
        - single_action_width(text_renderer, theme, card.primary_action)
        - f64::from(theme.tokens.spacing.s3);
    let title = theme.text(TextRole::Label).strong();
    let (title_width, _) = text_renderer.measure_style(&single_line_text(card.title), None, &title);
    let (_, line) = text_renderer.measure_style("Ag", None, &theme.text(TextRole::Caption));
    room >= f64::from(title_width) || room >= f64::from(line) * 8.0
}

fn single_action_width(text_renderer: &mut TextRenderer, theme: &Theme, label: &str) -> f64 {
    ButtonFace::new(Rect::ZERO, label)
        .control_size(crate::widget::ControlSize::Small)
        .intrinsic_width(text_renderer, theme)
}

/// Paint a card's one action at the trailing edge of `row`, centred on the
/// card, and return its rect.
fn paint_single_action(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> Rect {
    let width = single_action_width(text_renderer, theme, card.primary_action);
    let height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let x1 = if card.closable {
        row.x1
    } else {
        row.x1 - card_pad(theme).0
    };
    let rect = Rect::new(
        x1 - width,
        row.center().y - height / 2.0,
        x1,
        row.center().y + height / 2.0,
    );
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &rect);
    }
    Button::paint(
        scene,
        transform,
        text_renderer,
        theme,
        &ButtonFace::new(rect, card.primary_action)
            .control_size(crate::widget::ControlSize::Small)
            .hovered(card.primary_action_hovered)
            .pressed(card.primary_action_pressed),
    );
    if faded {
        scene.pop_layer();
    }
    rect
}

/// The height a card's progress bar and the gap above it take.
fn progress_reserve(theme: &Theme, card: &ToastCard<'_>) -> f64 {
    if card.progress.is_some() {
        f64::from(theme.tokens.spacing.s2) + crate::widget::ProgressBarFace::height(theme)
    } else {
        0.0
    }
}

/// How much smaller than a small icon the resting dismiss glyph is.
const DISMISS_GLYPH_SCALE: f64 = 0.8;

/// Where a card's dismiss control sits: a Small icon button inset one step
/// from the top-trailing corner, centred on a single-line card.
fn dismiss_rect(card: Rect, theme: &Theme) -> Rect {
    let side = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let inset = f64::from(theme.tokens.spacing.s2);
    let cx1 = card.x1 - inset;
    let cy0 = (card.y0 + inset).min(card.center().y - side / 2.0);
    Rect::new(cx1 - side, cy0, cx1, cy0 + side)
}

/// Paints the optional dismiss (`X`) button, top-right of `rect`, and
/// returns its rect when the card is closable. Wrapped in an alpha layer
/// during the enter/exit fade because `Button::paint` has no opacity
/// parameter of its own.
fn paint_dismiss_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> Option<Rect> {
    if !card.closable {
        return None;
    }
    let close_rect = dismiss_rect(rect, theme);
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            alpha,
            transform,
            &close_rect,
        );
    }
    if card.close_hovered || card.close_pressed {
        Button::paint(
            scene,
            transform,
            text_renderer,
            theme,
            &ButtonFace::new(close_rect, "")
                .role(ButtonRole::Plain)
                .icon(crate::icons::Icon::X)
                .control_size(crate::widget::control_size::ControlSize::Small)
                .hovered(card.close_hovered)
                .pressed(card.close_pressed)
                .accessible_name("Dismiss"),
        );
    } else {
        // At rest the close is a small glyph in the faintest ink, well
        // under the title's weight: it is the way out, not the message.
        // The pointer on it brings the full control back.
        let side = f64::from(theme.tokens.metrics.icon_sm) * DISMISS_GLYPH_SCALE;
        crate::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(close_rect.center(), (side, side)),
            crate::icons::Icon::X,
            theme.tokens.colors.text_tertiary,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
    if faded {
        scene.pop_layer();
    }
    Some(close_rect)
}

/// Paints the text column — optional attribution caption, then the title,
/// then the optional body — vertically centred in `column`, after its left
/// pad. `column` runs from the card's top-left to the dismiss button's left
/// edge (or the card's own right pad when there is none) and the top of the
/// action row (or the card's bottom).
fn paint_card_text(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> TextBlock {
    let theme = ctx.theme;
    let (pad_x, pad_y) = card_pad(theme);
    let text_x = column.x0 + pad_x;
    let width_avail = f32_of_f64((column.x1 - text_x).max(0.0));
    // A card sized for its text wraps it; one that is
    // shorter than its wrapped text keeps each field to one elided line.
    if let Some(block) = paint_wrapped_card_text(ctx, column, card, alpha, text_x, width_avail) {
        return block;
    }
    let text_renderer = &mut *ctx.text;
    let width_avail = f32_of_f64((column.x1 - text_x).max(0.0));
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);

    // A desktop notification may carry arbitrary localized or application
    // text. Keep every field one-line before deriving heights and widths;
    // otherwise `draw_text` interprets an embedded newline as a second line
    // even though this card's geometry is intentionally fixed-height.
    let caption_text = single_line_text(card.caption);
    let title_text = single_line_text(card.title);
    let body_text = single_line_text(card.body);
    let has_caption = !caption_text.is_empty();
    let has_body = !body_text.is_empty();
    // Stack the measured line boxes, not the font sizes: a face's line
    // height is ~1.3× its size, so size-derived heights under-reserved
    // every line and the block sat high in the card. The roles' own
    // leading is the space between lines.
    let line_gap = 0.0;
    let line_h = |text_renderer: &mut TextRenderer, text: &str, style: &TextStyle| {
        f64::from(text_renderer.measure_style(text, None, style).1)
    };
    let cap_h = if has_caption {
        line_h(text_renderer, &caption_text, &caption_style) + line_gap
    } else {
        0.0
    };
    let title_h =
        line_h(text_renderer, &title_text, &title_style) + if has_body { line_gap } else { 0.0 };
    let body_h = if has_body {
        line_h(text_renderer, &body_text, &body_style)
    } else {
        0.0
    };
    let block_h = cap_h + title_h + body_h;
    let text_height = column.height().max(0.0);
    let mut ty = column.y0 + ((text_height - block_h) / 2.0).max(pad_y);
    let top = ty;

    // `paint_card` deliberately owns a fixed-height notification surface.
    // Wrapping an untrusted summary/body into that surface makes the title
    // paint over the body (and can push the close affordance into the text).
    // Elide each line against the *actual* width left by the close button;
    // long diagnostics belong in the structured log, not in chrome.
    let caption = has_caption
        .then(|| text_renderer.elide_to_width_style(&caption_text, width_avail, &caption_style));
    let title = text_renderer.elide_to_width_style(&title_text, width_avail, &title_style);
    let body =
        has_body.then(|| text_renderer.elide_to_width_style(&body_text, width_avail, &body_style));

    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);

    if has_caption {
        ctx.text(
            caption.as_deref().unwrap_or_default(),
            &caption_style,
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
        ty += cap_h;
    }
    ctx.text(
        &title,
        &title_style,
        primary,
        Point::new(text_x, ty),
        Some(width_avail),
    );
    let title_centre = ty + (title_h - line_gap) / 2.0;
    ty += title_h;
    if has_body {
        ctx.text(
            body.as_deref().unwrap_or_default(),
            &body_style,
            dim,
            Point::new(text_x, ty),
            Some(width_avail),
        );
    }
    TextBlock {
        title_centre,
        top,
        bottom: top + block_h,
    }
}

/// Paint the card's text wrapped at `width`, centred in `column`, when it
/// fits there whole. Returns whether it painted.
fn paint_wrapped_card_text(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
    text_x: f64,
    width: f32,
) -> Option<TextBlock> {
    let theme = ctx.theme;
    let (_, pad_y) = card_pad(theme);
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);
    let caption = single_line_text(card.caption);
    let title = single_line_text(card.title);
    let body = single_line_text(card.body);
    let mut measure = |text: &str, style: &TextStyle| {
        if text.is_empty() {
            0.0
        } else {
            f64::from(ctx.text.measure_style(text, Some(width), style).1)
        }
    };
    let caption_h = measure(&caption, &caption_style);
    let title_h = measure(&title, &title_style);
    let body_h = measure(&body, &body_style);
    let single = f64::from(ctx.text.measure_style("Ag", None, &title_style).1);
    let block = caption_h + title_h + body_h;
    // Only take over when the text actually wraps and the card holds it.
    if title_h <= single * 1.2 && body_h <= single * 1.2 {
        return None;
    }
    if block > column.height() - 2.0 * pad_y {
        return None;
    }
    let mut y = column.y0 + (column.height() - block) / 2.0;
    let extent = TextBlock {
        // The first line of the title, however many it wraps to.
        title_centre: y + caption_h + single.min(title_h) / 2.0,
        top: y,
        bottom: y + block,
    };
    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);
    for (text, style, ink, h) in [
        (&caption, &caption_style, dim, caption_h),
        (&title, &title_style, primary, title_h),
        (&body, &body_style, dim, body_h),
    ] {
        if text.is_empty() {
            continue;
        }
        ctx.text(text, style, ink, Point::new(text_x, y), Some(width));
        y += h;
    }
    Some(extent)
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod tests;

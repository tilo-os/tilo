//! Transient notices and the notification card.
use crate::i18n::widget_text;
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
use crate::widget::text_input::FieldFace;
use crate::widget::{Button, ButtonFace, ControlSize};

/// Toast cards have a fixed height and therefore paint each text field on a
/// single baseline. Collapse all Unicode whitespace before measuring or
/// eliding a field so an app-provided/localized newline cannot turn a card
/// into an overflowing multi-line block. This also makes a body containing
/// only whitespace behave like an omitted body.
fn single_line_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

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
    shown: u64,
    published: std::cell::RefCell<Option<(u64, crate::window_toolbar::ToolbarNotice)>>,
}

#[derive(Clone, Debug)]
struct Shown<T> {
    value: T,
    since: crate::time::Instant,
    progress: bool,
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
        if let Some((_, gone)) = published.take() {
            crate::window_toolbar::withdraw(&gone);
        }
        if let Some((number, notice)) = want {
            crate::window_toolbar::notify(Some(notice.clone()));
            *published = Some((number, notice));
        }
    }

    #[must_use]
    pub fn showing(value: T) -> Self {
        let mut transient = Self::new();
        transient.show(value);
        transient
    }

    #[must_use]
    pub fn showing_progress(value: T) -> Self {
        let mut transient = Self::new();
        transient.show_progress(value);
        transient
    }

    pub fn show(&mut self, value: T) {
        self.put(value, false);
    }

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

    #[must_use]
    pub fn is_progress(&self) -> bool {
        self.get().is_some() && self.current.as_ref().is_some_and(|shown| shown.progress)
    }

    pub fn dismiss(&mut self) -> bool {
        self.current.take().is_some()
    }

    #[must_use]
    pub fn get(&self) -> Option<&T> {
        self.current
            .as_ref()
            .filter(|shown| !transient_expired(shown.since, NOTICE_LIFETIME))
            .map(|shown| &shown.value)
    }

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

    #[must_use]
    pub fn since(&self) -> Option<crate::time::Instant> {
        self.get()?;
        self.current.as_ref().map(|shown| shown.since)
    }

    pub fn expire(&mut self) -> bool {
        self.current.is_some() && self.get().is_none() && self.dismiss()
    }

    /// When the message leaves (progress: when it must next be renewed),
    /// for a widget's `next_wake_deadline`. It stays due while the window's
    /// bar still shows the message, so the frame that takes it down runs at
    /// that time without further input; once a sync has [published](Self::publish)
    /// its leaving there is none, so the widget is not woken over and over.
    /// Under the capture clock nothing leaves.
    #[must_use]
    pub fn deadline(&self) -> Option<crate::time::Instant> {
        if crate::time::wall_frozen() {
            return None;
        }
        let shown = self.current.as_ref()?;
        let in_the_bar = self
            .published
            .borrow()
            .as_ref()
            .is_some_and(|(number, _)| *number == shown.number);
        (self.get().is_some() || in_the_bar).then(|| shown.since + NOTICE_LIFETIME)
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
    pub fn show(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.current.show((text.into(), kind));
    }

    pub fn show_progress(&mut self, text: impl Into<String>) {
        self.current.show_progress((text.into(), ToastKind::Info));
    }

    pub fn keep_while(&mut self, busy: bool) -> bool {
        self.current.keep_while(busy)
    }

    pub fn dismiss(&mut self) -> bool {
        self.current.dismiss()
    }

    pub fn get(&self) -> Option<(&str, ToastKind)> {
        self.current
            .get()
            .map(|(text, kind)| (text.as_str(), *kind))
    }

    pub fn is_shown(&self) -> bool {
        self.current.is_shown()
    }

    pub fn expire(&mut self) -> bool {
        self.current.expire()
    }

    pub fn deadline(&self) -> Option<crate::time::Instant> {
        self.current.deadline()
    }

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
    spent: Duration,
    since: Option<crate::time::Instant>,
    first_shown: Option<crate::time::Instant>,
}

impl Dwell {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            spent: Duration::ZERO,
            since: None,
            first_shown: None,
        }
    }

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

    #[must_use]
    pub fn is_over(&self, lifetime: Duration, now: crate::time::Instant) -> bool {
        self.left(lifetime, now).is_zero()
    }

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

    #[must_use]
    pub fn ends_at(&self, lifetime: Duration) -> Option<crate::time::Instant> {
        let since = self.since?;
        since.checked_add(lifetime.saturating_sub(self.spent))
    }

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

fn card_pad(theme: &Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.spacing.s3),
        f64::from(theme.tokens.spacing.s2),
    )
}

pub(crate) fn kind_icon(kind: ToastKind) -> crate::icons::Icon {
    match kind {
        ToastKind::Info => crate::widget::BannerKind::Info,
        ToastKind::Success => crate::widget::BannerKind::Success,
        ToastKind::Warning => crate::widget::BannerKind::Warning,
        ToastKind::Error => crate::widget::BannerKind::Error,
    }
    .default_icon()
}

fn leading_side(theme: &Theme, card: &ToastCard<'_>) -> Option<f64> {
    if card.kind == ToastKind::Info && card.person.is_some() {
        return Some(crate::widget::AvatarSize::Regular.side(theme));
    }
    (card.kind != ToastKind::Info || card.icon.is_some() || card.app_image.is_some())
        .then(|| f64::from(theme.tokens.metrics.icon_md))
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderMark<'a> {
    pub image: Option<&'a crate::icons::AppImage>,
    /// The glyph painted when there is no artwork, or it cannot be decoded.
    pub icon: Option<crate::icons::Icon>,
}

impl<'a> SenderMark<'a> {
    #[must_use]
    pub const fn new(
        image: Option<&'a crate::icons::AppImage>,
        icon: Option<crate::icons::Icon>,
    ) -> Self {
        Self { image, icon }
    }

    /// Paint the mark filling `rect`: the artwork keeps its own colours, the
    /// glyph takes `color`. Returns whether anything was painted.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        color: vello::peniko::Color,
    ) -> bool {
        if let Some(image) = self.image
            && crate::icons::draw_app_image(scene, transform, rect, image)
        {
            return true;
        }
        let Some(icon) = self.icon else {
            return false;
        };
        crate::icons::draw_icon(
            scene,
            transform,
            rect,
            icon,
            color,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        true
    }
}

#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent immediate-mode card affordances are supplied by the caller"
)]
pub struct ToastCard<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub caption: &'a str,
    pub time: &'a str,
    pub reply: bool,
    pub reply_text: &'a str,
    pub reply_focused: bool,
    pub reply_hovered: bool,
    pub kind: ToastKind,
    /// What plain information leads with instead of a severity glyph,
    /// which only warnings, errors and successes carry: a glyph naming
    /// what the card is about...
    pub icon: Option<crate::icons::Icon>,
    pub person: Option<(&'a str, &'a str)>,
    pub person_agent: bool,
    /// The sending application's own artwork, where a glyph cannot name it.
    /// A person's avatar leads ahead of it; a severity glyph wins over it,
    /// so an alert never loses its state colour.
    pub app_image: Option<&'a crate::icons::AppImage>,
    pub alpha: f32,
    pub closable: bool,
    pub close_hovered: bool,
    pub close_pressed: bool,
    pub actionable: bool,
    pub secondary_action: &'a str,
    pub secondary_action_icon: Option<crate::icons::Icon>,
    pub secondary_action_hovered: bool,
    pub secondary_action_pressed: bool,
    pub primary_action: &'a str,
    pub primary_action_icon: Option<crate::icons::Icon>,
    pub primary_action_hovered: bool,
    pub primary_action_pressed: bool,
    pub progress: Option<f32>,
}

impl<'a> ToastCard<'a> {
    pub fn new(title: &'a str, kind: ToastKind) -> Self {
        Self {
            title,
            body: "",
            caption: "",
            time: "",
            reply: false,
            reply_text: "",
            reply_focused: false,
            reply_hovered: false,
            kind,
            icon: None,
            person: None,
            person_agent: false,
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
    #[must_use]
    pub fn progress(mut self, progress: f32) -> Self {
        self.progress = Some(progress);
        self
    }
    #[must_use]
    pub fn icon(mut self, icon: Option<crate::icons::Icon>) -> Self {
        self.icon = icon;
        self
    }
    #[must_use]
    pub fn person(mut self, name: &'a str, identity: &'a str) -> Self {
        self.person = Some((name, identity));
        self.person_agent = false;
        self
    }
    #[must_use]
    pub fn agent(mut self, name: &'a str, identity: &'a str) -> Self {
        self.person = Some((name, identity));
        self.person_agent = true;
        self
    }
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
    pub fn time(mut self, time: &'a str) -> Self {
        self.time = time;
        self
    }
    #[must_use]
    pub fn reply(mut self, text: &'a str, focused: bool) -> Self {
        self.reply = true;
        self.reply_text = text;
        self.reply_focused = focused;
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
    #[must_use]
    pub fn action(mut self, label: &'a str, hovered: bool) -> Self {
        self.secondary_action = "";
        self.secondary_action_icon = None;
        self.primary_action = label;
        self.primary_action_icon = None;
        self.primary_action_hovered = hovered;
        self
    }

    fn single_action(&self) -> bool {
        self.secondary_action.is_empty() && !self.primary_action.is_empty()
    }

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

#[derive(Clone, Copy, Debug, Default)]
pub struct ToastCardHit {
    pub card_rect: Rect,
    pub close_rect: Rect,
    pub secondary_action_rect: Rect,
    pub primary_action_rect: Rect,
    pub reply_rect: Rect,
}

fn reply_row_height(theme: &Theme, card: &ToastCard<'_>) -> f64 {
    if !card.reply {
        return 0.0;
    }
    choice_lead(theme) + FieldFace::height(ControlSize::Small, theme) + choice_foot(theme)
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
    let text = CardText::of(card);
    let text_height: f64 = [
        (text.caption.as_str(), theme.text(TextRole::Caption)),
        (text.title.as_str(), theme.text(TextRole::Label).strong()),
        (text.body.as_str(), theme.text(TextRole::Caption)),
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
    height += reply_row_height(theme, card);
    height.ceil()
}

/// A card's body never takes more lines than this: a notification says
/// enough to choose, and a title that ran on would bury its actions.
const BODY_LINES: usize = 2;

/// A card's title wraps to at most this many lines: on a phone a title such
/// as "Claude Code finished: 8 changes in Workspace" is the whole news and
/// must read whole, yet a title that ran on would bury the card's actions.
const TITLE_LINES: usize = 2;

struct CardText {
    caption: String,
    time: String,
    title: String,
    body: String,
}

impl CardText {
    fn of(card: &ToastCard<'_>) -> Self {
        let caption = single_line_text(card.caption);
        let time = single_line_text(card.time);
        let body = single_line_text(card.body);
        if caption.is_empty() && !time.is_empty() {
            let body = if body.is_empty() {
                time
            } else {
                format!("{body} \u{b7} {time}")
            };
            return Self {
                caption,
                time: String::new(),
                title: single_line_text(card.title),
                body,
            };
        }
        Self {
            caption,
            time,
            title: single_line_text(card.title),
            body,
        }
    }

    fn has_header(&self) -> bool {
        !self.caption.is_empty() || !self.time.is_empty()
    }
}

fn line_height(text_renderer: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(text_renderer.measure_style("Ag", None, style).1)
}

/// Height of a card `width` wide: the attribution line (the app, and how
/// long ago it arrived), the title, the body clamped to two lines, the
/// actions and the padding. A card never reserves less room than
/// [`paint_card`] paints into it.
pub fn card_height(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    card: &ToastCard<'_>,
    width: f64,
) -> f64 {
    let compact = minimum_card_height(text_renderer, theme, card);
    let text = CardText::of(card);
    if text.title.is_empty() && text.body.is_empty() {
        return compact;
    }
    let (pad_x, pad_y) = card_pad(theme);
    let control_height = ButtonFace::height(crate::widget::ControlSize::Small, theme);
    let choices = !card.secondary_action.is_empty() && !card.primary_action.is_empty();
    let inline_action =
        card.single_action() && single_action_inline(text_renderer, theme, card, width);
    let trailing = if card.closable {
        f64::from(theme.tokens.spacing.s2) + control_height + f64::from(theme.tokens.spacing.s1)
    } else {
        pad_x
    } + if inline_action {
        single_action_width(text_renderer, theme, card.primary_action)
            + f64::from(theme.tokens.spacing.s3)
    } else {
        0.0
    };
    let text_width =
        f32_of_f64((width - leading_column(theme, card) - 2.0 * pad_x - trailing).max(1.0));
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);
    let header_h = if text.has_header() {
        line_height(text_renderer, &caption_style)
    } else {
        0.0
    };
    let title_h = if text.title.is_empty() {
        0.0
    } else {
        let lines = text_renderer
            .clamp_to_lines(&text.title, &title_style, text_width, TITLE_LINES)
            .len()
            .max(1);
        lines as f64 * line_height(text_renderer, &title_style)
    };
    let body_h = if text.body.is_empty() {
        0.0
    } else {
        let lines = text_renderer
            .clamp_to_lines(&text.body, &body_style, text_width, BODY_LINES)
            .len()
            .max(1);
        lines as f64 * line_height(text_renderer, &body_style)
    };
    let mut height = (header_h + title_h + body_h)
        .max(leading_side(theme, card).unwrap_or(f64::from(theme.tokens.metrics.icon_md)))
        .max(if card.closable || inline_action {
            control_height
        } else {
            0.0
        })
        + 2.0 * pad_y;
    if choices || (card.single_action() && !inline_action) {
        height += choice_lead(theme) + control_height + choice_foot(theme);
    }
    height += reply_row_height(theme, card);
    height.max(compact).ceil()
}

#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn painted_words(
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    title: &str,
    body: &str,
    caption: &str,
    width: f64,
) -> (usize, usize) {
    let card = ToastCard::new(title, ToastKind::Info)
        .body(body)
        .caption(caption);
    let height = card_height(text_renderer, theme, &card, width);
    let mut scene = Scene::new();
    paint_card(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        theme,
        Rect::new(0.0, 0.0, width, height),
        &card,
    );
    let runs = &scene.encoding().resources.glyph_runs;
    let glyphs = runs.iter().map(|run| run.glyphs.len()).sum();
    (glyphs, runs.len())
}

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

    let mut text_right = if let Some(close_rect) =
        paint_dismiss_button(scene, transform, text_renderer, theme, rect, card, a)
    {
        hit.close_rect = close_rect;
        close_rect.x0 - f64::from(theme.tokens.spacing.s1)
    } else {
        rect.x1 - pad_x
    };
    let mut action_top = None;
    if card.single_action() && single_action_inline(text_renderer, theme, card, rect.width()) {
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
    }
    let mut bottom = rect.y1;
    if card.reply {
        let height = FieldFace::height(ControlSize::Small, theme);
        let y1 = bottom - choice_foot(theme);
        let y0 = y1 - height;
        hit.reply_rect = Rect::new(text_x0, y0, text_right, y1);
        paint_reply_field(
            scene,
            transform,
            text_renderer,
            theme,
            hit.reply_rect,
            card,
            a,
        );
        bottom = y0 - choice_lead(theme);
    }
    if card.single_action() && !single_action_inline(text_renderer, theme, card, rect.width()) {
        let y1 = bottom - choice_foot(theme);
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

    let action_top = action_top.or(paint_card_actions(
        scene,
        transform,
        text_renderer,
        theme,
        Rect::new(rect.x0, rect.y0, rect.x1, bottom),
        card,
        a,
        &mut hit,
    ));
    let text_bottom = action_top.unwrap_or(bottom) - progress_reserve(theme, card);
    let block = paint_card_text(
        &mut PaintCtx::new(scene, transform, text_renderer, theme),
        Rect::new(text_x0, rect.y0, text_right, text_bottom),
        card,
        a,
    );
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

fn agent_or_person_face<'a>(
    card: &ToastCard<'_>,
    name: &'a str,
    identity: &'a str,
) -> crate::widget::AvatarFace<'a> {
    let face = crate::widget::AvatarFace::new(name, identity);
    if !card.person_agent {
        return face;
    }
    match crate::tokens::colors::PresenceHue::of_agent_client(name) {
        Some(hue) => face.hue(hue).agent(),
        None => face.agent(),
    }
}

fn paint_reply_field(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) {
    let faded = alpha < 0.999;
    if faded {
        scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, transform, &rect);
    }
    let placeholder = widget_text("toast-reply-placeholder");
    let face = FieldFace::new(rect, card.reply_text)
        .placeholder(&placeholder)
        .text_role(TextRole::Caption)
        .hovered(card.reply_hovered)
        .focused(card.reply_focused);
    face.paint(scene, transform, text_renderer, theme);
    if card.reply_focused {
        let style = theme.text(TextRole::Caption);
        let room = face.text_rect(theme);
        let (x, line_h) = if card.reply_text.is_empty() {
            let (_, h) = text_renderer.measure_style("Ag", None, &style);
            (room.x0, f64::from(h))
        } else {
            let width = f32_of_f64(room.width().max(1.0));
            let line = text_renderer.elide_to_width_style(card.reply_text, width, &style);
            let (w, h) = text_renderer.measure_style(&line, None, &style);
            (
                (room.x0 + f64::from(w) + 1.0).min(room.x1 - 1.0),
                f64::from(h),
            )
        };
        let y0 = room.y0 + (room.height() - line_h) / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.text_primary,
            None,
            &Rect::new(x, y0, x + 1.5, y0 + line_h),
        );
    }
    if faded {
        scene.pop_layer();
    }
}

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
        agent_or_person_face(card, name, identity).paint(ctx, rect);
        if faded {
            ctx.scene.pop_layer();
        }
        return;
    }
    let mark = Rect::from_center_size(
        Point::new(left + side / 2.0, block.title_centre),
        (side, side),
    );
    let (image, icon, color) = match card.kind {
        ToastKind::Info => (card.app_image, card.icon, theme.tokens.colors.text_primary),
        kind => (None, Some(kind_icon(kind)), kind_color(kind, theme)),
    };
    if image.is_none() && icon.is_none() {
        return;
    }
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
    SenderMark::new(image, icon).paint(ctx.scene, ctx.transform, mark, color);
    if faded {
        ctx.scene.pop_layer();
    }
}

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
    let theme = &on_card(theme);
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

pub fn choice_height(theme: &Theme) -> f64 {
    ButtonFace::height(crate::widget::ControlSize::Small, theme)
}

pub fn choice_lead(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s2)
}

pub fn choice_foot(theme: &Theme) -> f64 {
    card_pad(theme).1
}

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

fn paint_single_action(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    row: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> Rect {
    let theme = &on_card(theme);
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

fn on_card(theme: &Theme) -> Theme {
    theme.on_ground(crate::widget::floating_surface::card_ground(theme))
}

fn progress_reserve(theme: &Theme, card: &ToastCard<'_>) -> f64 {
    if card.progress.is_some() {
        f64::from(theme.tokens.spacing.s2) + crate::widget::ProgressBarFace::height(theme)
    } else {
        0.0
    }
}

const DISMISS_GLYPH_SCALE: f64 = 0.8;

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
                .accessible_name(&crate::i18n::widget_format("widget-dismiss", &[])),
        );
    } else {
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

fn paint_card_text(
    ctx: &mut PaintCtx<'_>,
    column: Rect,
    card: &ToastCard<'_>,
    alpha: f32,
) -> TextBlock {
    let theme = ctx.theme;
    let (pad_x, pad_y) = card_pad(theme);
    let text_x = column.x0 + pad_x;
    let width = f32_of_f64((column.x1 - text_x).max(0.0));
    let caption_style = theme.text(TextRole::Caption);
    let title_style = theme.text(TextRole::Label).strong();
    let body_style = theme.text(TextRole::Caption);
    let text = CardText::of(card);

    // The block is the attribution line (app and time), the title in
    // semibold wrapped to two lines, then the body clamped to two lines. A
    // card laid out shorter than that (a compact in-app card, the lock
    // screen's alert) keeps the title and the body on one line each.
    let body_line_h = line_height(ctx.text, &body_style);
    let title_line_h = line_height(ctx.text, &title_style);
    let header_h = if text.has_header() {
        line_height(ctx.text, &caption_style)
    } else {
        0.0
    };
    // The title takes a second line only where the body keeps its first.
    let body_first = if text.body.is_empty() {
        0.0
    } else {
        body_line_h
    };
    let title_room = column.height() - 2.0 * pad_y - header_h - body_first;
    let title_cap = if title_line_h > 0.0 {
        (title_room / title_line_h)
            .floor()
            .clamp(1.0, TITLE_LINES as f64) as usize
    } else {
        1
    };
    let title_lines = if text.title.is_empty() {
        Vec::new()
    } else {
        ctx.text
            .clamp_to_lines(&text.title, &title_style, width, title_cap)
    };
    let title_h = title_lines.len() as f64 * title_line_h;
    let room = column.height() - 2.0 * pad_y - header_h - title_h;
    let body_cap = if body_line_h > 0.0 {
        (room / body_line_h).floor().clamp(1.0, BODY_LINES as f64) as usize
    } else {
        1
    };
    let body_lines = if text.body.is_empty() {
        Vec::new()
    } else {
        ctx.text
            .clamp_to_lines(&text.body, &body_style, width, body_cap)
    };
    let body_h = body_lines.len() as f64 * body_line_h;
    let block_h = header_h + title_h + body_h;
    let mut ty = column.y0 + ((column.height() - block_h) / 2.0).max(pad_y);
    let top = ty;

    let dim = theme.tokens.colors.text_dim.with_alpha(alpha);
    let primary = theme.tokens.colors.text_primary.with_alpha(alpha);

    if text.has_header() {
        let mut label_width = width;
        if !text.time.is_empty() {
            let (time_width, _) = ctx.text.measure_style(&text.time, None, &caption_style);
            let time_width = time_width.min(width);
            let time_x = text_x + f64::from(width - time_width);
            ctx.text(
                &text.time,
                &caption_style,
                dim,
                Point::new(time_x, ty),
                Some(time_width),
            );
            label_width =
                (width - time_width - f32_of_f64(f64::from(theme.tokens.spacing.s2))).max(0.0);
        }
        if !text.caption.is_empty() && label_width > 0.0 {
            let caption = ctx
                .text
                .elide_to_width_style(&text.caption, label_width, &caption_style);
            ctx.text(
                &caption,
                &caption_style,
                dim,
                Point::new(text_x, ty),
                Some(label_width),
            );
        }
        ty += header_h;
    }
    let title_centre = ty + title_line_h / 2.0;
    for line in &title_lines {
        ctx.text(
            line,
            &title_style,
            primary,
            Point::new(text_x, ty),
            Some(width),
        );
        ty += title_line_h;
    }
    for line in &body_lines {
        ctx.text(line, &body_style, dim, Point::new(text_x, ty), Some(width));
        ty += body_line_h;
    }
    TextBlock {
        title_centre,
        top,
        bottom: top + block_h,
    }
}

#[cfg(test)]
#[path = "toast_tests.rs"]
mod tests;

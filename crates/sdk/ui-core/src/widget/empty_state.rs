//! `EmptyState` — the centered "there's nothing here yet" placeholder that
//! almost every list-shaped app hand-rolls (Inbox with no mail, a feed with
//! no articles, a contacts book with no contacts, …). Before this widget the
//! pattern was copy-pasted as a `Column` of a big icon + a heading `Text` +
//! a dim subtitle + an optional call-to-action `Button` across the fleet.
//!
//! `EmptyState` folds that into one theme-driven widget:
//!
//! - an optional large [`Icon`] in tertiary ink, or an illustration the app
//!   paints into the same slot (a weather scene),
//! - a **title** in the `Title` role (the "No messages" headline),
//! - an optional **message**: one sentence in secondary ink, `rhythm` s3
//!   under the title,
//! - an optional primary **action**, s4 under the text.
//!
//! An error state ([`EmptyState::error`]) tints its icon with the danger
//! status colour and bounds its message to one elided line; assistive
//! technology still reads the whole message.
//!
//! The content is centered on both axes inside whatever space the parent
//! gives the widget, so dropping it as the sole child of a content pane
//! "just works". The action is surfaced through the toolkit's drain channel
//! ([`EmptyState::take_action`]) rather than a callback, matching the
//! CRDT-native input model used by [`super::ContextMenu`] /
//! [`super::TriStateControl`].

use crate::widget::{AppModel, ModelPath};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{ParagraphAlign, TextRenderer, TextStyle, TextWrapMode};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{Scrollable, TaffyTreeExt, Widget};

/// Which elements a caller asked for.
#[derive(Clone, Copy, Debug)]
struct Parts {
    icon: bool,
    message: bool,
    action: bool,
}

/// Measured heights the stack is built from.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    title_h: f64,
    msg_h: f64,
    action_w: f64,
    inner_width: f32,
    pad_x: f64,
    gap: f64,
    text_gap: f64,
    btn_h: f64,
    hero: f64,
}

/// Which elements actually fit, and how tall the result is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    icon: bool,
    message: bool,
    total: f64,
}

/// Paints an empty state's illustration into the square it is given.
pub type Illustration<'a> = &'a dyn Fn(&mut PaintCtx<'_>, Rect);

/// What an empty state shows: optional icon or illustration, title,
/// optional message and optional action label.
#[derive(Clone, Copy)]
pub struct EmptyStateContent<'a> {
    pub icon: Option<Icon>,
    /// Painted in place of the icon, in a larger square.
    pub illustration: Option<Illustration<'a>>,
    pub title: &'a str,
    pub message: Option<&'a str>,
    pub action_label: Option<&'a str>,
    /// Something failed: the icon takes the danger colour and the message
    /// is bounded to one line.
    pub error: bool,
}

impl std::fmt::Debug for EmptyStateContent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmptyStateContent")
            .field("icon", &self.icon)
            .field("illustration", &self.illustration.is_some())
            .field("title", &self.title)
            .field("message", &self.message)
            .field("action_label", &self.action_label)
            .field("error", &self.error)
            .finish()
    }
}

impl<'a> EmptyStateContent<'a> {
    /// Just a title, the minimal case.
    pub fn new(title: &'a str) -> Self {
        Self {
            icon: None,
            illustration: None,
            title,
            message: None,
            action_label: None,
            error: false,
        }
    }

    fn parts(&self) -> Parts {
        Parts {
            icon: self.icon.is_some() || self.illustration.is_some(),
            message: self.message.is_some(),
            action: self.action_label.is_some(),
        }
    }
}

/// The typography and spacing an empty-state stack is measured with.
#[derive(Clone, Debug)]
struct StackStyle {
    title: TextStyle,
    message: TextStyle,
    action: TextStyle,
    gap: f64,
    text_gap: f64,
    pad_y: f64,
    min_button: f64,
    glyph: f64,
}

impl StackStyle {
    /// The theme's roles, as the retained widget lays out. An inline empty
    /// state is a section's: a `Heading` title and a smaller hero.
    fn layout(theme: &Theme, inline: bool) -> Self {
        let wrapped = |role| theme.text(role).with_wrap_mode(TextWrapMode::WordOrGlyph);
        let glyph = f64::from(theme.tokens.metrics.icon_lg);
        Self {
            title: wrapped(if inline {
                TextRole::Heading
            } else {
                TextRole::Title
            }),
            message: wrapped(TextRole::Body),
            action: wrapped(TextRole::Label),
            gap: f64::from(theme.tokens.spacing.s4),
            text_gap: f64::from(theme.tokens.spacing.s3),
            pad_y: f64::from(theme.tokens.spacing.s2),
            min_button: f64::from(theme.tokens.metrics.min_pointer_target),
            glyph: if inline { glyph } else { glyph * 2.0 },
        }
    }

    /// The same roles centred, for painting.
    fn resolved(theme: &Theme, inline: bool) -> Self {
        let layout = Self::layout(theme, inline);
        let centered = |style: TextStyle| style.with_align(ParagraphAlign::Center);
        Self {
            title: centered(layout.title),
            message: centered(layout.message),
            action: centered(layout.action),
            ..layout
        }
    }

    fn measure(
        &self,
        text_renderer: &mut TextRenderer,
        width: f64,
        content: &EmptyStateContent<'_>,
    ) -> Metrics {
        let gap = self.gap;
        let pad_x = gap;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "window widths are logical pixels well within f32 range"
        )]
        let inner_width = (width - gap * 2.0).max(1.0) as f32;
        let (_, title_h) =
            text_renderer.measure_style(content.title, Some(inner_width), &self.title);
        let (_, msg_h) = content.message.map_or((0.0, 0.0), |value| {
            if content.error {
                // An error detail is one bounded line.
                text_renderer.measure_style("Ag", None, &self.message)
            } else {
                text_renderer.measure_style(value, Some(inner_width), &self.message)
            }
        });
        let (action_w, action_h) = content.action_label.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.action)
        });
        Metrics {
            title_h: f64::from(title_h),
            msg_h: f64::from(msg_h),
            action_w: f64::from(action_w),

            inner_width,
            pad_x,
            gap,
            text_gap: self.text_gap,
            btn_h: if content.action_label.is_some() {
                self.min_button.max(f64::from(action_h) + self.pad_y * 2.0)
            } else {
                0.0
            },
            hero: if content.illustration.is_some() {
                self.glyph * 2.0
            } else {
                self.glyph
            },
        }
    }
}

impl Fit {
    /// Shed decorative elements rather than overflow the rect: the hero disc
    /// is the largest element and carries the least information, so it goes
    /// first; the message goes next, only if the title alone still doesn't
    /// fit. The title and the action are never dropped — the first is the
    /// whole point and the second is the only thing the user can do here.
    fn choose(height: f64, parts: Parts, m: Metrics) -> Fit {
        let stack = |icon: bool, message: bool| {
            let mut t = m.title_h;
            if icon {
                t += m.hero + m.gap;
            }
            if message {
                t += m.text_gap + m.msg_h;
            }
            if parts.action {
                t += m.gap + m.btn_h;
            }
            t
        };
        let mut icon = parts.icon;
        let mut message = parts.message;
        if icon && stack(true, message) > height {
            icon = false;
        }
        if message && stack(icon, true) > height {
            message = false;
        }
        Fit {
            icon,
            message,
            total: stack(icon, message),
        }
    }
}

/// Where everything in an empty state goes inside a rect: what
/// [`EmptyState::paint`] draws, and what [`EmptyState::layout`] hands back
/// without painting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmptyStateLayout {
    /// The height the stack needs at this width with everything shown.
    pub natural_height: f64,
    /// The hero slot (icon, illustration or spinner), when it fits.
    pub hero: Option<Rect>,
    /// The action button, when there is one and it is on screen.
    pub action: Option<Rect>,
}

/// The resolved stack: styles, measurements and positions.
struct Placed {
    style: StackStyle,
    metrics: Metrics,
    hero: Option<Rect>,
    title_y: f64,
    message_y: Option<f64>,
    action: Option<Rect>,
    natural: f64,
}

/// Lay the stack out centred in `rect`, shedding the hero and then the
/// message rather than overflow.
fn place(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    content: &EmptyStateContent<'_>,
    inline: bool,
) -> Placed {
    let mut style = StackStyle::resolved(theme, inline);
    let mut metrics = style.measure(text, rect.width(), content);
    let natural = Fit::choose(f64::INFINITY, content.parts(), metrics).total;
    let mut fit = Fit::choose(rect.height(), content.parts(), metrics);
    // On a phone at a high text scale a three-line headline can crowd out
    // the recovery explanation even after the decorative icon is shed.
    // Preserve the action *and* the explanation by compacting only the
    // display size of the heading, while keeping its heading semantics and
    // full accessible name intact.
    if content.message.is_some() && !fit.message {
        let original = style.title.clone();
        for factor in [0.82_f32, 0.72, 0.60] {
            let mut candidate = style.clone();
            candidate.title = original.clone();
            candidate.title.size *= factor;
            let candidate_metrics = candidate.measure(text, rect.width(), content);
            let candidate_fit = Fit::choose(
                rect.height(),
                Parts {
                    message: true,
                    ..content.parts()
                },
                candidate_metrics,
            );
            if candidate_fit.message {
                style = candidate;
                metrics = candidate_metrics;
                fit = candidate_fit;
                break;
            }
        }
    }
    let gap = metrics.gap;
    let total = fit.total;
    let cx = rect.center().x;
    let mut y = if total > rect.height() {
        rect.y0
    } else {
        rect.center().y - total / 2.0
    };
    let hero = fit.icon.then(|| {
        let slot = Rect::from_center_size(
            Point::new(cx, y + metrics.hero / 2.0),
            (metrics.hero, metrics.hero),
        );
        y += metrics.hero + gap;
        slot
    });
    let title_y = y;
    y += metrics.title_h;
    let message_y = (fit.message && content.message.is_some()).then(|| {
        y += metrics.text_gap;
        let at = y;
        y += metrics.msg_h;
        at
    });
    let action = content.action_label.and_then(|_| {
        y += gap;
        let bw = (metrics.action_w + metrics.pad_x * 2.0).min(rect.width());
        let x0 = cx - bw / 2.0;
        let action_rect = Rect::new(x0, y, x0 + bw, y + metrics.btn_h);
        (action_rect.y1 > rect.y0 && action_rect.y0 < rect.y1).then_some(action_rect)
    });
    Placed {
        style,
        metrics,
        hero,
        title_y,
        message_y,
        action,
        natural,
    }
}

/// Shared paint for the empty state, centered inside `rect`. Used by both
/// [`EmptyState`]'s [`Widget::draw`] and the immediate-mode
/// [`EmptyState::paint`]. Returns the action's rect (for hit-testing) when
/// `action_label` is `Some` and the action is on screen. `hover` is the
/// pointer position for the action's hover state.
fn paint_empty_state(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &EmptyStateContent<'_>,
    hover: Point,
    action_focused: bool,
    inline: bool,
    spinner: Option<&mut crate::widget::Spinner>,
) -> Option<Rect> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let theme = ctx.theme;
    let transform = ctx.transform;
    let placed = place(ctx.text, theme, rect, content, inline);
    let Placed {
        style,
        metrics,
        hero,
        title_y,
        message_y,
        action,
        ..
    } = placed;

    if let Some(slot) = hero {
        if let Some(spinner) = spinner {
            spinner.set_rect(slot);
            spinner.draw(
                ctx.scene,
                transform,
                ctx.text,
                theme,
                &PresenceCtx::default(),
            );
        } else if let Some(illustration) = content.illustration {
            illustration(ctx, slot);
        } else if let Some(icon) = content.icon {
            let ink = if content.error {
                theme.tokens.colors.semantic.danger
            } else {
                theme.text_tertiary
            };
            icon.draw(ctx.scene, transform, slot, ink);
        }
    }

    let text_x = rect.center().x - f64::from(metrics.inner_width) / 2.0;
    ctx.text(
        content.title,
        &style.title,
        theme.text_primary,
        Point::new(text_x, title_y),
        Some(metrics.inner_width),
    );

    if let (Some(msg), Some(y)) = (content.message, message_y) {
        if content.error {
            let line = ctx
                .text
                .elide_to_width_style(msg, metrics.inner_width, &style.message);
            ctx.text(
                &line,
                &style.message,
                theme.text_dim,
                Point::new(text_x, y),
                Some(metrics.inner_width),
            );
        } else {
            ctx.text(
                msg,
                &style.message,
                theme.text_dim,
                Point::new(text_x, y),
                Some(metrics.inner_width),
            );
        }
    }

    if let (Some(label), Some(action_rect)) = (content.action_label, action) {
        let face = crate::widget::ButtonFace::new(action_rect, label)
            .role(crate::widget::ButtonRole::Primary)
            .hovered(action_rect.contains(hover))
            .focused(action_focused);
        crate::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &face);
        // An empty-state action is often the only route out of a first-use
        // screen: keep the normal, high-visibility focus treatment.
        if action_focused {
            crate::widget::paint_focus_ring(
                ctx.scene,
                transform,
                action_rect,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
    }
    action
}

/// What the action does when it is activated.
#[derive(Clone)]
enum Handler {
    Binding(tilo_actions::ActionBinding<()>),
    View(Rc<RefCell<Box<dyn FnMut()>>>),
    Effect(Rc<RefCell<Box<dyn FnMut()>>>),
}

/// A centered empty-state placeholder: hero icon + title + message + action.
pub struct EmptyState {
    icon: Option<Icon>,
    illustration: Option<Box<dyn Fn(&mut PaintCtx<'_>, Rect)>>,
    error: bool,
    title: String,
    message: Option<String>,
    action_label: Option<String>,
    action_id: Option<String>,
    action_enabled: bool,
    /// What the action is bound to. When set, the action is a published
    /// [`Button`](crate::widget::Button) (`engine`) and every activation path
    /// goes through it, like [`super::Button::on_action`].
    handler: Option<Handler>,
    engine: Option<crate::widget::Button>,
    /// Drained flag set when the action is clicked.
    action_fired: bool,
    rect: Rect,
    action_rect: Rect,
    last_cursor: Point,
    /// The action is immediate-mode, but still participates in the normal
    /// Tab / Enter contract through this owning widget.
    action_focused: bool,
    layout_style: StackStyle,
    scroll_content: bool,
    /// An empty section inside a page: hugs its content.
    inline: bool,
    /// Something is on its way: a spinner in the hero slot.
    spinner: Option<crate::widget::Spinner>,
    /// Whether the spinner shows (kept apart from it while it paints).
    busy: bool,
    /// The title, message and action label through the i18n layer.
    resolved: (String, Option<String>, Option<String>),
    i18n_revision: u64,
    layout_dirty: bool,
    /// What the last draw showed (rect, action hovered, focused, enabled);
    /// while it still holds there is nothing to redraw.
    drawn: Option<(Rect, bool, bool, bool)>,
}

impl EmptyState {
    /// Construct with just a title (the minimal case). Chain
    /// [`with_icon`](Self::with_icon) / [`with_message`](Self::with_message) /
    /// [`with_action`](Self::with_action) for the fuller treatment. The title,
    /// message and action label are resolved through the i18n layer, like
    /// [`Text`](crate::widget::Text).
    pub fn new(title: impl Into<String>) -> Self {
        let mut state = Self {
            icon: None,
            illustration: None,
            error: false,
            title: title.into(),
            message: None,
            action_label: None,
            action_id: None,
            action_enabled: true,
            handler: None,
            engine: None,
            action_fired: false,
            rect: Rect::ZERO,
            action_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            action_focused: false,
            layout_style: StackStyle::layout(&Theme::default(), false),
            scroll_content: false,
            inline: false,
            spinner: None,
            busy: false,
            resolved: (String::new(), None, None),
            i18n_revision: crate::i18n::I18nManager::revision(),
            layout_dirty: true,
            drawn: None,
        };
        state.resolve();
        state
    }

    /// The state a draw paints: where, whether the action is hovered or
    /// focused, and whether it is enabled.
    fn painted_state(&self) -> (Rect, bool, bool, bool) {
        (
            self.rect,
            self.action_label.is_some() && self.action_rect.contains(self.last_cursor),
            self.action_focused,
            self.is_enabled(),
        )
    }

    /// Something failed to load or open: `title` says what, `detail` says
    /// why. The detail shows as one bounded line; assistive technology
    /// reads all of it.
    pub fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        let mut state = Self::new(title).with_message(detail);
        state.error = true;
        state
    }

    /// An empty section inside a page ("No servers yet" under a heading):
    /// the title is a `Heading`, the stack hugs its content and claims no
    /// full-pane height.
    #[must_use]
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self.layout_dirty = true;
        self
    }

    /// Something is on its way ("Scanning your disk"): a spinner in the hero
    /// slot that keeps itself moving.
    #[must_use]
    pub fn busy(mut self) -> Self {
        self.spinner = Some(crate::widget::Spinner::new());
        self.busy = true;
        self.layout_dirty = true;
        self
    }

    /// Show or hide the spinner.
    pub fn set_busy(&mut self, busy: bool) {
        if busy != self.spinner.is_some() {
            self.spinner = busy.then(crate::widget::Spinner::new);
            self.busy = busy;
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    /// A large icon in tertiary ink above the title.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// One sentence under the title, in secondary ink.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self.resolve();
        self
    }

    /// Add the one primary action under the message.
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self.resolve();
        self.rebuild_engine();
        self
    }

    /// Keep the recovery action visible while its binding is unavailable.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.action_enabled = enabled;
        if let Some(engine) = &mut self.engine {
            engine.set_enabled(enabled);
        }
        self.layout_dirty = true;
        self
    }

    /// Stable identity for the optional call-to-action. Supplying it makes the
    /// action automatically available to accessibility, search, and agents.
    #[must_use]
    pub fn with_action_id(mut self, id: impl Into<String>) -> Self {
        self.action_id = Some(id.into());
        self.rebuild_engine();
        self
    }

    /// Route pointer, keyboard and accessibility activation through an
    /// exported resource action. The host revalidates the binding against the
    /// current resource tree before dispatch, so empty-state recovery controls
    /// follow the same policy/history path as ordinary buttons.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.handler = Some(Handler::Binding(binding));
        self.action_fired = false;
        self.rebuild_engine();
        self
    }

    /// Publish the action as a transient presentation action that runs `f`,
    /// like [`super::Button::on_view`].
    #[must_use]
    pub fn on_view(mut self, f: impl FnMut() + 'static) -> Self {
        self.handler = Some(Handler::View(Rc::new(RefCell::new(Box::new(f)))));
        self.rebuild_engine();
        self
    }

    /// Publish the action as an effect that runs `f`, like
    /// [`super::Button::on_effect`].
    #[must_use]
    pub fn on_effect(mut self, f: impl FnMut() + 'static) -> Self {
        self.handler = Some(Handler::Effect(Rc::new(RefCell::new(Box::new(f)))));
        self.rebuild_engine();
        self
    }

    /// The control id the action is published, addressed and tested by:
    /// [`Self::with_action_id`], or one derived from the title.
    pub fn action_control_id(&self) -> String {
        self.action_id
            .clone()
            .unwrap_or_else(|| format!("empty-state:{}", self.title))
    }

    /// Build the published button the action runs through.
    fn rebuild_engine(&mut self) {
        let (Some(handler), Some(label)) = (self.handler.clone(), self.action_label.clone()) else {
            self.engine = None;
            return;
        };
        let id = self.action_control_id();
        let button = crate::widget::Button::new(&id, &label)
            .role(crate::widget::ButtonRole::Primary)
            .with_enabled(self.action_enabled);
        self.engine = Some(match handler {
            Handler::Binding(binding) => button.on_action(binding),
            Handler::View(f) => button.on_view(move || (f.borrow_mut())()),
            Handler::Effect(f) => button.on_effect(move || (f.borrow_mut())()),
        });
    }

    /// Re-read the title, message and action label through the i18n layer.
    fn resolve(&mut self) {
        let resolved = (
            crate::i18n!(&self.title),
            self.message.as_deref().map(|m| crate::i18n!(m)),
            self.action_label.as_deref().map(|a| crate::i18n!(a)),
        );
        if resolved != self.resolved {
            self.resolved = resolved;
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    fn fire_action(&mut self, state: &mut crate::widget::Edits<'_>, path: &ObjId) {
        if let Some(engine) = &mut self.engine {
            let _ = engine.perform_action(&crate::a11y::Action::Activate, &[], state, path);
            return;
        }
        self.action_fired = true;
    }

    /// Drain the "action was clicked" signal (fires once per click). The
    /// host reacts by mutating its own model — e.g. opening a compose sheet.
    pub fn take_action(&mut self) -> bool {
        std::mem::take(&mut self.action_fired)
    }

    /// The resolved bounds of the action (empty until laid out + drawn).
    /// Exposed so tests can assert the hit target without rendering to pixels.
    pub fn action_rect(&self) -> Rect {
        self.action_rect
    }

    /// Wrap this retained empty state in the toolkit's vertical viewport.
    /// The child keeps at least the viewport height for ordinary centering and
    /// grows to its measured content height when wrapped text needs more room,
    /// making a below-fold action reachable by wheel, keyboard, or scrollbar.
    #[must_use]
    pub fn into_scrollable(mut self) -> Scrollable {
        self.scroll_content = true;
        Scrollable::new_vertical().with_child(Box::new(self))
    }

    /// Minimum height required to show the title and optional action at
    /// `width`, after wrapping and accessibility scaling. Immediate-mode
    /// callers with a shorter viewport must provide scrolling; [`Self::paint`]
    /// anchors at the top and does not return a wholly offscreen hit target.
    pub fn minimum_essential_height(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        title: &str,
        action_label: Option<&str>,
    ) -> f64 {
        let content = EmptyStateContent {
            action_label,
            ..EmptyStateContent::new(title)
        };
        let metrics = StackStyle::resolved(theme, false).measure(text_renderer, width, &content);
        metrics.title_h + action_label.map_or(0.0, |_| metrics.gap + metrics.btn_h)
    }

    /// Where [`Self::paint`] puts the stack in `rect`, without painting: its
    /// natural height at this width, the hero slot and the action's rect.
    pub fn layout(
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        content: &EmptyStateContent<'_>,
    ) -> EmptyStateLayout {
        let placed = place(text_renderer, theme, rect, content, false);
        EmptyStateLayout {
            natural_height: placed.natural,
            hero: placed.hero,
            action: placed.action,
        }
    }

    /// Place the empty state inside a rect computed by an immediate-mode
    /// parent.
    ///
    /// Some application shells use Taffy for their root allocation but split
    /// their internal panes with production-specific geometry. This keeps the
    /// empty state a real widget (focus, accessibility and activation included)
    /// without forcing those shells to duplicate its paint and hit-testing.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Immediate-mode paint of an empty state centered inside `rect`, for the
    /// many apps that render straight to a `Scene` instead of building a
    /// widget tree. Returns the action's rect when `action_label` is visible
    /// so the caller can hit-test it against its own pointer state (pass
    /// `hover` for the hover state, or an off-screen point to suppress).
    /// Call [`Self::minimum_essential_height`] first when the supplied pane can
    /// be physically shorter than the title and action; that parent must own
    /// scrolling, and a wholly offscreen action produces no false hit target.
    pub fn paint(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
    ) -> Option<Rect> {
        paint_empty_state(ctx, rect, content, hover, false, false, None)
    }

    /// Paint a state whose action has keyboard focus, using the shared focus ring.
    pub fn paint_focused(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
        focused: bool,
    ) -> Option<Rect> {
        paint_empty_state(ctx, rect, content, hover, focused, false, None)
    }

    fn content(&self) -> EmptyStateContent<'_> {
        EmptyStateContent {
            icon: if self.busy {
                // The spinner takes the hero slot; the slot is still sized.
                self.icon.or(Some(Icon::Info))
            } else {
                self.icon
            },
            illustration: self
                .illustration
                .as_deref()
                .filter(|_| !self.busy)
                .map(|paint| paint as Illustration<'_>),
            title: &self.resolved.0,
            message: self.resolved.1.as_deref(),
            action_label: self.resolved.2.as_deref(),
            error: self.error,
        }
    }
}

impl Widget for EmptyState {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        let revision = crate::i18n::I18nManager::revision();
        if revision != self.i18n_revision {
            self.i18n_revision = revision;
            self.resolve();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_style = StackStyle::layout(theme, self.inline);
        // A rebuild may bring a new theme or text scale.
        self.drawn = None;
        // Auto height contributes wrapped content to a Scrollable's extent;
        // the percentage minimum still fills a normal definite-height pane.
        // An inline empty state hugs its content.
        let style = Style {
            flex_grow: if self.inline { 0.0 } else { 1.0 },
            flex_shrink: if self.inline { 0.0 } else { 1.0 },
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: percent(0.0_f32),
                height: if self.inline {
                    taffy::Dimension::auto()
                } else {
                    percent(1.0_f32)
                },
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.or(match available.width {
            taffy::AvailableSpace::Definite(width) => Some(width),
            taffy::AvailableSpace::MinContent | taffy::AvailableSpace::MaxContent => None,
        });
        let measured_width = width.unwrap_or(240.0).max(1.0);
        let metrics =
            self.layout_style
                .measure(text_renderer, f64::from(measured_width), &self.content());
        let essential_height = metrics.title_h
            + self
                .action_label
                .as_ref()
                .map_or(0.0, |_| metrics.gap + metrics.btn_h);
        let natural = Fit::choose(f64::INFINITY, self.content().parts(), metrics).total;
        let intrinsic_height = if self.inline {
            natural
        } else if self.scroll_content {
            essential_height
        } else {
            natural.max(220.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "Taffy uses f32 layout sizes; measured text geometry is intentionally rounded to that precision"
        )]
        let height = intrinsic_height as f32;
        taffy::Size {
            width: measured_width,
            height,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.layout_dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.rect == Rect::ZERO
    }

    fn needs_animation_frame(&self) -> bool {
        self.spinner
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let rect = self.rect;
        let hover = self.last_cursor;
        let focused = self.action_focused;
        let inline = self.inline;
        let mut spinner = self.spinner.take();
        self.action_rect = paint_empty_state(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            rect,
            &self.content(),
            hover,
            focused,
            inline,
            spinner.as_mut(),
        )
        .unwrap_or(Rect::ZERO);
        self.spinner = spinner;
        if let Some(engine) = &mut self.engine {
            engine.apply_layout_external(self.action_rect);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.is_enabled()
                && self.action_label.is_some()
                && self.action_rect.contains(self.last_cursor) =>
            {
                self.fire_action(&mut ctx.state, ctx.path);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.action_label.is_some() && self.action_rect.contains(point) {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        self.action_label.is_some() && self.is_enabled()
    }

    fn is_enabled(&self) -> bool {
        self.action_enabled && self.engine.as_ref().is_none_or(Widget::is_enabled)
    }

    fn set_focus(&mut self, focused: bool) {
        self.action_focused = focused && self.action_label.is_some();
    }

    fn is_focused(&self) -> bool {
        self.action_focused
    }

    fn needs_redraw(&self) -> bool {
        // A spinner moves every frame; new words repaint too.
        self.drawn != Some(self.painted_state())
            || self.layout_dirty
            || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.drawn = Some(self.painted_state());
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = crate::doc::document(state, path);
        if self.is_enabled()
            && self.action_label.is_some()
            && matches!(action, crate::a11y::Action::Activate)
        {
            self.fire_action(state, path);
            true
        } else {
            false
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.action_label.is_some()
            && self.action_control_id() == target
            && self.perform_action(action, &[], state, path)
    }

    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        match &mut self.engine {
            Some(engine) => engine.start_operation(op, doc, path),
            None => Ok(None),
        }
    }

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.engine
            .as_ref()
            .map(Widget::resource_operation_handlers)
            .unwrap_or_default()
    }

    fn name(&self) -> &'static str {
        "EmptyState"
    }

    /// The action's id, when it has one: the empty state is addressed by
    /// the control it holds.
    fn test_id(&self) -> Option<&str> {
        self.action_label.as_ref().and(self.action_id.as_deref())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // The empty state is a labelled group; the title is its accessible
        // name and the message its description so a screen reader announces
        // both without the caller wiring anything.
        let mut node = AccessNode::new(Role::Group).with_name(self.resolved.0.clone());
        if let Some(msg) = &self.resolved.1 {
            node = node.with_description(msg.clone());
        }
        if self.spinner.is_some() {
            node = node.with_state(AccessState {
                busy: true,
                ..Default::default()
            });
        }
        if let Some(action) = &self.resolved.2 {
            node = node
                .with_state(AccessState {
                    focusable: true,
                    focused: self.action_focused,
                    busy: self.spinner.is_some(),
                    ..Default::default()
                })
                .with_children(vec![
                    AccessNode::new(Role::Button)
                        .with_name(action.clone())
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.action_focused,
                            disabled: !self.is_enabled(),
                            ..Default::default()
                        })
                        .with_action_target(self.action_control_id()),
                ]);
        }
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        matches!(path, [0]).then_some(self.action_rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        if matches!(path, [0]) && self.action_label.is_some() && self.is_enabled() {
            self.action_focused = true;
            true
        } else {
            false
        }
    }
}

impl crate::resources::PublishesResources for EmptyState {
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        self.engine
            .as_ref()
            .map(|engine| crate::resources::PublishesResources::resources(engine, doc))
            .unwrap_or_default()
    }
}

impl crate::widget::controls::ActionControls for EmptyState {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(engine) = &mut self.engine {
            crate::widget::controls::ActionControls::install_control_custody(
                engine, domain, attachment,
            );
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.engine.as_mut().and_then(|engine| {
            crate::widget::controls::ActionControls::take_control_invocation(engine)
        })
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        if self.action_label.is_none() {
            return Vec::new();
        }
        if let Some(engine) = &self.engine {
            return crate::widget::controls::ActionControls::control_action_bindings(engine);
        }
        vec![crate::widget::ControlBindingMetadata::semantic(
            self.action_control_id(),
        )]
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(engine) = &mut self.engine {
            crate::widget::controls::ActionControls::refresh_control_action_availability(
                engine, allowed,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::dispatch;
    use vello_encoding::DrawTag;

    fn lay_out(w: &mut dyn Widget, vw: f32, vh: f32) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(vw),
                    height: taffy::AvailableSpace::Definite(vh),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn draw(w: &mut dyn Widget) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    use crate::synthetic_input::mouse_press;

    /// Representative numbers: the dark theme's `xl` title and `sm` message at
    /// the spacing scale a docked panel is drawn with.
    fn metrics() -> Metrics {
        Metrics {
            title_h: 24.0,
            msg_h: 18.0,
            action_w: 0.0,

            inner_width: 0.0,
            pad_x: 0.0,
            gap: 16.0,
            text_gap: 12.0,
            btn_h: 34.0,
            hero: 72.0,
        }
    }

    #[test]
    fn short_and_narrow_panes_shed_decoration_but_keep_the_action() {
        /// A zero-state in a short pane (a docked panel body, a couple hundred
        /// pixels tall) must shed elements rather than push the message through
        /// the bottom edge.
        fn a_short_pane_sheds_the_hero_disc_before_it_overflows() {
            let want = Parts {
                icon: true,
                message: true,
                action: false,
            };
            let m = metrics();

            // Tall pane: everything fits, nothing is dropped.
            let roomy = Fit::choose(400.0, want, m);
            assert!(
                roomy.icon && roomy.message,
                "a full pane keeps the whole stack"
            );
            assert!(roomy.total <= 400.0);

            // Docked panel: the disc alone is 72px plus a 16px gap, so the stack
            // does not fit and the disc is what goes.
            let docked = Fit::choose(120.0, want, m);
            assert!(!docked.icon, "the disc is shed first");
            assert!(docked.message, "the explanation is kept while it fits");
            assert!(
                docked.total <= 120.0,
                "and the result fits: {} in 120",
                docked.total
            );

            // Barely any room: the message goes too, but never the title.
            let cramped = Fit::choose(30.0, want, m);
            assert!(!cramped.icon && !cramped.message);
            assert_eq!(cramped.total, m.title_h, "the title always survives");
        }

        /// The action pill is the only thing the user can do, so it is never shed —
        /// and it still counts toward what fits.
        fn the_action_is_never_shed_and_is_counted() {
            let m = metrics();
            let with_action = Parts {
                icon: true,
                message: true,
                action: true,
            };
            let fit = Fit::choose(120.0, with_action, m);
            assert!(!fit.icon, "the disc gives way to the action");
            assert!(
                fit.total >= m.title_h + m.gap + m.btn_h,
                "the pill is still in the measured stack"
            );
        }

        fn narrow_large_text_recovery_copy_wraps_inside_the_empty_state() {
            let theme = Theme::dark().with_text_scale(2.0);
            let rect = Rect::new(0.0, 0.0, 390.0, 680.0);
            let gap = f64::from(theme.tokens.spacing.s4);
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test viewport is an ordinary logical-pixel dimension"
            )]
            let width = (rect.width() - gap * 2.0).max(1.0) as f32;
            let title = "Couldn’t open this page";
            let message = "The site did not respond. Check your connection and try again.";
            let title_style = theme
                .text(TextRole::Title)
                .with_align(ParagraphAlign::Center);
            let message_style = theme
                .text(TextRole::Caption)
                .with_align(ParagraphAlign::Center);
            let mut renderer = TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let (_, title_one_line_height) = renderer.measure_style(title, None, &title_style);
            let (title_width, title_height) =
                renderer.measure_style(title, Some(width), &title_style);
            let (message_width, message_height) =
                renderer.measure_style(message, Some(width), &message_style);

            assert!(title_width <= width);
            assert!(message_width <= width);
            assert!(title_height > title_one_line_height);
            assert!(message_height > 0.0);

            let mut state = EmptyState::new(title)
                .with_message(message)
                .with_action("Try again");
            state.apply_layout_external(rect);
            let mut scene = Scene::new();
            state.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &theme,
                &PresenceCtx::default(),
            );
            let action = state.action_rect();
            assert!(rect.contains(action.center()));
            let action_style = theme
                .text(TextRole::Label)
                .with_wrap_mode(TextWrapMode::WordOrGlyph)
                .with_align(ParagraphAlign::Center);
            let (_, action_line_height) =
                renderer.measure_style("Try again", Some(width), &action_style);
            assert!(
                action.height()
                    >= f64::from(action_line_height) + f64::from(theme.tokens.spacing.s2) * 2.0,
                "large-text action reserves its label's full line height"
            );
        }

        fn long_localized_content_wraps_inside_a_narrow_scaled_pane() {
            for width in [240.0, 480.0] {
                for scale in [1.0, 2.0] {
                    let theme = Theme::dark().with_text_scale(scale);
                    let mut tr = crate::font_catalog::fixture_renderer();
                    tr.set_text_scale(scale);
                    let mut scene = Scene::new();
                    let rect = Rect::new(20.0, 30.0, 20.0 + width, 1330.0);
                    let action = EmptyState::paint(
                        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                        rect,
                        &EmptyStateContent {
                            icon: Some(Icon::Inbox),
                            illustration: None,
                            error: false,
                            title: "Keine synchronisierten Nachrichten in diesem Arbeitsbereich",
                            message: Some(
                        "Sobald eine Verbindung verfügbar ist, erscheinen neue Nachrichten automatisch.",
                    ),
                            action_label: Some(
                        "Verbindungseinstellungen jetzt vollständig überprüfen und sichere Wiederherstellung vorbereiten",
                    ),
                        },
                        Point::new(-1.0, -1.0),
                    )
                .expect("action remains available");

                    assert!(action.x0 >= rect.x0 && action.x1 <= rect.x1, "{action:?}");
                    assert!(action.height() > 34.0, "wrapped action grows: {action:?}");
                    let encoding = scene.encoding();
                    for run in &encoding.resources.glyph_runs {
                        for glyph in &encoding.resources.glyphs[run.glyphs.clone()] {
                            let x = f64::from(run.transform.translation[0] + glyph.x);
                            assert!(
                                x >= rect.x0 && x <= rect.x1,
                                "{width}px/{scale}x glyph x={x} outside {rect:?}"
                            );
                        }
                    }
                }
            }
        }

        fn tiny_immediate_pane_exposes_required_height_without_offscreen_hit_geometry() {
            let theme = Theme::dark().with_text_scale(2.0);
            let mut tr = crate::font_catalog::fixture_renderer();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            let rect = Rect::new(10.0, 20.0, 250.0, 45.0);
            let minimum = EmptyState::minimum_essential_height(
                &mut tr,
                &theme,
                rect.width(),
                "No messages are available",
                Some("Review connection settings"),
            );
            assert!(minimum > rect.height());
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: Some(Icon::Inbox),
                    illustration: None,
                    error: false,
                    title: "No messages are available",
                    message: Some("Reconnect to load messages."),
                    action_label: Some("Review connection settings"),
                },
                Point::new(-1.0, -1.0),
            );

            assert!(
                action.is_none(),
                "a wholly offscreen action is not exposed as a usable hit target"
            );
        }

        a_short_pane_sheds_the_hero_disc_before_it_overflows();
        the_action_is_never_shed_and_is_counted();
        narrow_large_text_recovery_copy_wraps_inside_the_empty_state();
        long_localized_content_wraps_inside_a_narrow_scaled_pane();
        tiny_immediate_pane_exposes_required_height_without_offscreen_hit_geometry();
    }

    #[test]
    fn exported_action_button_is_placed_where_activation_clicks() {
        let mut es = EmptyState::new("No response yet").with_action("Send request");
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);

        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        es.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let update = crate::accesskit_bridge::build_tree_update(&es, &node_map, None, 1.0);
        let (_, button) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == accesskit::Role::Button)
            .expect("the action is exported as a button");
        let bounds = button.bounds().expect("the action button has bounds");
        let action = es.action_rect();
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (action.x0, action.y0, action.x1, action.y1)
        );
    }

    #[test]
    fn empty_state_action_fires_once_through_pointer_keyboard_and_semantics() {
        fn action_pill_click_fires_once() {
            let mut es = EmptyState::new("No messages")
                .with_icon(Icon::Inbox)
                .with_message("Your inbox is empty")
                .with_action("Compose");
            lay_out(&mut es, 600.0, 400.0);
            // draw resolves the action rect.
            let _ = draw(&mut es);
            let target = es.action_rect().center();
            assert!(target.x > 0.0, "action rect must resolve after draw");
            dispatch(&mut es, &cursor(target));
            assert!(
                dispatch(&mut es, &mouse_press()),
                "click on the action pill is consumed"
            );
            assert!(es.take_action(), "the action signal fires");
            assert!(!es.take_action(), "the action signal drains after one read");
        }

        fn click_outside_action_does_not_fire() {
            let mut es = EmptyState::new("Nothing here").with_action("Add");
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            // Top-left corner is well outside the centered pill.
            dispatch(&mut es, &cursor(Point::new(2.0, 2.0)));
            assert!(
                !dispatch(&mut es, &mouse_press()),
                "click away from the pill is not consumed"
            );
            assert!(!es.take_action());
        }

        fn action_is_keyboard_reachable_and_exposed_as_a_named_button() {
            let mut es = EmptyState::new("No notes yet")
                .with_message("Create one to get started")
                .with_action("Create note");
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            assert!(Widget::is_focusable(&es));
            Widget::set_focus(&mut es, true);
            let node = Widget::accessibility(&es).expect("empty state is semantic");
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, crate::a11y::Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Create note"));
            assert!(node.children[0].state.focused);
            assert_eq!(
                es.synthetic_accessibility_rect(&[0]),
                Some(es.action_rect())
            );

            let mut doc = AutoCommit::new();
            assert!(Widget::perform_action(
                &mut es,
                &crate::a11y::Action::Activate,
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert!(es.take_action(), "Enter/Space/AT Activate reaches the CTA");
        }

        fn action_path_is_shared_by_pointer_and_accessibility_activation() {
            use automerge::ReadDoc as _;
            use std::cell::Cell;
            let invoked = Rc::new(Cell::new(0));
            let counted = invoked.clone();
            let empty = EmptyState::new("Nothing here")
                .with_action("Open")
                .with_action_id("empty.open")
                .on_view(move || counted.set(counted.get() + 1));
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                empty,
                (600, 400),
                Theme::dark(),
            );
            let action = harness.root::<EmptyState>().action_rect();
            assert!(action.area() > 0.0, "the recovery action has a target");
            harness.click_at(action.center().x, action.center().y);
            assert_eq!(
                invoked.get(),
                1,
                "pointer activation invokes the published action"
            );
            harness
                .perform_named("empty.open", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                invoked.get(),
                2,
                "accessibility invokes the same published action"
            );
            assert!(
                harness
                    .state()
                    .doc
                    .get(automerge::ROOT, "empty.open")
                    .unwrap()
                    .is_none(),
                "a view action never writes a click pulse into the resource"
            );
        }

        fn canonical_action_binding_replaces_the_legacy_document_channel() {
            let resource = tilo_actions::Resource::new("catalog", "catalog", "Catalog")
                .with_action(
                    tilo_actions::ActionSpec::operation(
                        "refresh",
                        "Refresh catalog",
                        tilo_actions::OperationKind::Effect,
                        "test.refresh",
                        &(),
                    )
                    .unwrap(),
                );
            let binding = tilo_actions::ActionBinding::unit_for(&resource, "refresh").unwrap();
            let mut es = EmptyState::new("Unavailable")
                .with_action("Refresh catalog")
                .with_action_id("catalog.empty.refresh")
                .on_action(binding);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);

            let target = es.action_rect().center();
            dispatch(&mut es, &cursor(target));
            assert!(dispatch(&mut es, &mouse_press()));
            let invocation = crate::widget::controls::take_control_invocation(&mut es)
                .expect("canonical invocation");
            assert_eq!(
                invocation.address,
                tilo_actions::ActionAddress::new("catalog", "refresh")
            );
            assert!(
                !es.take_action(),
                "action-bound activation cannot also fire the legacy drain channel"
            );
            assert!(matches!(
                crate::widget::controls::control_action_bindings(&es).as_slice(),
                [crate::widget::ControlBindingMetadata {
                    control_id,
                    kind: crate::widget::ControlBindingKind::Action(address),
                }] if control_id == "catalog.empty.refresh"
                    && address == &tilo_actions::ActionAddress::new("catalog", "refresh")
            ));
        }

        fn without_action_paints_no_pill_and_has_no_hit_target() {
            let mut es = EmptyState::new("Empty").with_icon(Icon::Folder);
            lay_out(&mut es, 600.0, 400.0);
            let _ = draw(&mut es);
            assert_eq!(es.action_rect(), Rect::ZERO, "no action ⇒ no pill rect");
            // A press anywhere is ignored when there's no action.
            let center = es.get_rect().center();
            dispatch(&mut es, &cursor(center));
            assert!(!dispatch(&mut es, &mouse_press()));
            assert!(!es.take_action());
        }

        action_pill_click_fires_once();
        click_outside_action_does_not_fire();
        action_is_keyboard_reachable_and_exposed_as_a_named_button();
        action_path_is_shared_by_pointer_and_accessibility_activation();
        canonical_action_binding_replaces_the_legacy_document_channel();
        without_action_paints_no_pill_and_has_no_hit_target();
    }

    #[test]
    fn empty_state_paints_scrolls_and_centres_its_essential_content() {
        fn draws_something_visible() {
            let mut es = EmptyState::new("No contacts yet")
                .with_icon(Icon::Inbox)
                .with_message("Add your first contact to get started")
                .with_action("Add contact");
            lay_out(&mut es, 600.0, 400.0);
            let scene = draw(&mut es);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            // disc + icon strokes + title glyphs + message glyphs + pill + label
            // glyphs ⇒ comfortably many COLOR tags.
            assert!(
                fills > 5,
                "a fully-populated empty state paints multiple fills, got {fills}"
            );
        }

        fn immediate_mode_paint_returns_action_rect() {
            // The canvas-app entry point paints into an arbitrary rect and hands
            // back the pill hit target, matching what Widget::draw resolves.
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 500.0, 300.0);
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: Some(Icon::Inbox),
                    illustration: None,
                    error: false,
                    title: "No mail",
                    message: Some("You're all caught up"),
                    action_label: Some("Compose"),
                },
                Point::new(-1.0, -1.0),
            );
            let r = action.expect("action label ⇒ a returned pill rect");
            assert!(rect.contains(r.center()), "pill sits inside the paint rect");
            // No action label ⇒ no returned rect.
            let none = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    illustration: None,
                    error: false,
                    title: "Empty",
                    message: None,
                    action_label: None,
                },
                Point::new(-1.0, -1.0),
            );
            assert!(none.is_none());
        }

        fn retained_empty_state_has_a_scrollable_route_to_essential_content() {
            use std::cell::Cell;
            let invoked = Rc::new(Cell::new(0));
            let counted = invoked.clone();
            let scroll = EmptyState::new("No messages are available")
                .with_icon(Icon::Inbox)
                .with_message("Reconnect to load messages.")
                .with_action("Review connection settings")
                .with_action_id("empty.review")
                .on_view(move || counted.set(counted.get() + 1))
                .into_scrollable()
                .with_height(50.0);
            let mut harness = crate::headless::FrameHarness::for_root(
                crate::State::new(),
                scroll,
                (240, 50),
                Theme::dark(),
            );
            assert!(harness.root::<Scrollable>().vertical_overflows());
            let end = harness.root::<Scrollable>().max_offset_y();
            assert!(end > 0.0);
            harness.root_mut::<Scrollable>().scroll_by(0.0, end);
            harness.relayout();
            let _ = harness.inspection();
            let scroll = harness.root::<Scrollable>();
            let action = scroll.children()[0]
                .synthetic_accessibility_rect(&[0])
                .expect("retained action geometry");
            assert!(
                scroll.content_viewport_rect().contains(action.center()),
                "the scrolled action is visible: {action:?}"
            );
            harness.click_at(action.center().x, action.center().y);
            assert_eq!(
                invoked.get(),
                1,
                "visible recovery action invokes through the host"
            );
            harness
                .perform_named("empty.review", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(
                invoked.get(),
                2,
                "accessibility uses the same recovery action"
            );
        }

        fn roomy_scrollable_retains_the_icon_and_explanatory_message() {
            let make = |icon| {
                let mut scroll = EmptyState::new("No messages")
                    .with_message("Reconnect to load messages.")
                    .with_action("Review settings");
                if icon {
                    scroll = scroll.with_icon(Icon::Inbox);
                }
                scroll.into_scrollable().with_height(400.0)
            };
            let mut decorated = make(true);
            lay_out(&mut decorated, 480.0, 400.0);
            let decorated_scene = draw(&mut decorated);
            let decorated_encoding = decorated_scene.encoding();
            assert!(
                decorated_encoding.resources.glyph_runs.len() >= 3,
                "title, explanatory message, and action all remain painted"
            );

            let mut plain = make(false);
            lay_out(&mut plain, 480.0, 400.0);
            let plain_scene = draw(&mut plain);
            let decorated_colors = decorated_encoding
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            let plain_colors = plain_scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == DrawTag::COLOR)
                .count();
            assert!(
                decorated_colors > plain_colors,
                "roomy scrollable keeps the icon disc and strokes"
            );
        }

        fn roomy_manual_pane_keeps_the_essential_block_centered() {
            let theme = Theme::dark();
            let mut tr = crate::font_catalog::fixture_renderer();
            let rect = Rect::new(15.0, 25.0, 495.0, 425.0);
            let minimum = EmptyState::minimum_essential_height(
                &mut tr,
                &theme,
                rect.width(),
                "No messages",
                Some("Compose"),
            );
            let mut scene = Scene::new();
            let action = EmptyState::paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
                rect,
                &EmptyStateContent {
                    icon: None,
                    illustration: None,
                    error: false,
                    title: "No messages",
                    message: None,
                    action_label: Some("Compose"),
                },
                Point::new(-1.0, -1.0),
            )
            .expect("roomy pane exposes action");
            let top = action.y1 - minimum;
            assert!(((top - rect.y0) - (rect.y1 - action.y1)).abs() < 0.001);

            let mut retained = EmptyState::new("No messages").with_action("Compose");
            lay_out(&mut retained, 480.0, 400.0);
            assert_eq!(retained.get_rect(), Rect::new(0.0, 0.0, 480.0, 400.0));
            let _ = draw(&mut retained);
            let retained_action = retained.action_rect();
            let retained_top = retained_action.y1 - minimum;
            assert!((retained_top - (400.0 - retained_action.y1)).abs() < 0.001);
        }

        fn accessibility_is_a_named_group() {
            use crate::a11y::Role;
            let es = EmptyState::new("No results").with_message("Try another search");
            let node = Widget::accessibility(&es).expect("empty state exposes a11y");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("No results"));
            assert_eq!(node.description.as_deref(), Some("Try another search"));
        }

        draws_something_visible();
        immediate_mode_paint_returns_action_rect();
        retained_empty_state_has_a_scrollable_route_to_essential_content();
        roomy_scrollable_retains_the_icon_and_explanatory_message();
        roomy_manual_pane_keeps_the_essential_block_centered();
        accessibility_is_a_named_group();
    }

    #[test]
    fn errors_use_the_danger_tone_and_one_bounded_line() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let detail = "The file ends before its cross-reference table. ".repeat(6);
        let content = EmptyStateContent {
            icon: Some(Icon::FileText),
            message: Some(detail.as_str()),
            error: true,
            ..EmptyStateContent::new("Can't open this document")
        };
        let style = StackStyle::resolved(&theme, false);
        let error = style.measure(&mut text, 360.0, &content);
        let plain = style.measure(
            &mut text,
            360.0,
            &EmptyStateContent {
                error: false,
                ..content
            },
        );
        assert!(
            error.msg_h < plain.msg_h,
            "the detail is bounded to one line"
        );
        assert_eq!(error.text_gap, f64::from(theme.tokens.spacing.s3));
        assert_eq!(error.gap, f64::from(theme.tokens.spacing.s4));

        let state = EmptyState::error("Can't open this document", detail.clone());
        let node = Widget::accessibility(&state).unwrap();
        assert_eq!(
            node.description.as_deref(),
            Some(detail.as_str()),
            "full text"
        );

        // The glyph is the danger colour, not an accent or the inverted fill.
        let mut scene = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 360.0, 400.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        let danger = theme.tokens.colors.semantic.danger.to_rgba8();
        let encoded = scene.encoding();
        let has_danger = encoded.draw_data.chunks(1).any(|word| {
            word.first().is_some_and(|w| {
                let [r, g, b, a] = w.to_le_bytes();
                (r, g, b, a) == (danger.r, danger.g, danger.b, danger.a)
                    || (a, b, g, r) == (danger.r, danger.g, danger.b, danger.a)
            })
        });
        assert!(has_danger, "error glyph is painted in the danger colour");
    }

    #[test]
    fn an_illustration_takes_the_hero_slot_and_sheds_first() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let painted = std::cell::Cell::new(None);
        let paint = |_: &mut PaintCtx<'_>, rect: Rect| painted.set(Some(rect));
        let content = EmptyStateContent {
            illustration: Some(&paint),
            message: Some("Search for a city to see its forecast."),
            ..EmptyStateContent::new("No places yet")
        };
        let mut scene = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 400.0, 500.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        let slot = painted.take().expect("illustration painted");
        assert!((slot.width() - f64::from(theme.tokens.metrics.icon_lg) * 4.0).abs() < 0.5);
        let mut cramped = Scene::new();
        paint_empty_state(
            &mut PaintCtx::new(&mut cramped, Affine::IDENTITY, &mut text, &theme),
            Rect::new(0.0, 0.0, 400.0, 80.0),
            &content,
            Point::new(-1.0, -1.0),
            false,
            false,
            None,
        );
        assert!(
            painted.take().is_none(),
            "a short pane sheds the illustration"
        );
    }

    #[test]
    fn a_handler_publishes_the_action_as_a_certified_bound_button() {
        let opened = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = opened.clone();
        let root = EmptyState::new("No places yet")
            .with_action("Add a place")
            .with_action_id("weather.empty.add")
            .on_view(move || seen.set(seen.get() + 1));
        let mut driver = crate::headless::FrameHarness::new(
            crate::state::State::new(),
            Box::new(root),
            (480, 360),
        );
        let audit = driver.native_app_audit();
        assert!(audit.is_certified(), "{audit:#?}");
        driver
            .perform_named("weather.empty.add", &crate::a11y::Action::Activate)
            .expect("the action is addressable by its id");
        assert_eq!(opened.get(), 1, "the view handler ran once");
    }

    #[test]
    fn an_inline_empty_state_hugs_its_content_and_a_busy_one_keeps_moving() {
        let mut inline = EmptyState::new("No servers yet")
            .with_message("Add one to start.")
            .inline();
        let mut full = EmptyState::new("No servers yet").with_message("Add one to start.");
        lay_out(&mut inline, 600.0, 600.0);
        lay_out(&mut full, 600.0, 600.0);
        assert!(
            inline.get_rect().height() < 120.0,
            "inline hugs its content: {:?}",
            inline.get_rect()
        );
        assert!(full.get_rect().height() >= 220.0);

        let busy = EmptyState::new("Scanning your disk").busy();
        assert!(busy.needs_animation_frame(), "a busy state keeps moving");
        let node = Widget::accessibility(&busy).unwrap();
        assert!(node.state.busy);
    }

    #[test]
    fn layout_reports_the_action_where_paint_puts_it() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let rect = Rect::new(0.0, 0.0, 480.0, 400.0);
        let content = EmptyStateContent {
            icon: Some(Icon::Inbox),
            message: Some("Your inbox is empty"),
            action_label: Some("Compose"),
            ..EmptyStateContent::new("No messages")
        };
        let layout = EmptyState::layout(&mut text, &theme, rect, &content);
        let mut scene = Scene::new();
        let painted = EmptyState::paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
            rect,
            &content,
            Point::new(-1.0, -1.0),
        );
        assert_eq!(layout.action, painted);
        assert!(layout.natural_height > 0.0 && layout.natural_height < rect.height());
    }
}

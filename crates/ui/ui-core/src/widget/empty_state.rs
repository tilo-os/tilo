//! `EmptyState` — the centered "there's nothing here yet" placeholder almost every list-
//! shaped app hand-rolled (Inbox with no mail, a feed with no articles): one theme-driven
//! widget for an optional large [`Icon`] in tertiary ink (or an illustration the app paints
//! in the same slot), a **title** in the `Title` role, an optional one-sentence **message**
//! in secondary ink s3 under it, and an optional primary **action** s4 under the text.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui_base::i18n::widget_text;
use tilo_ui_base::{widget::AppModel, widget::ModelPath};

use automerge::ObjId;
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::widget::Scrollable;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::interaction::InteractionEnvironment;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{
    text_render::ParagraphAlign, text_render::TextRenderer, text_render::TextStyle,
    text_render::TextWrapMode,
};
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

#[derive(Clone, Copy, Debug)]
struct Parts {
    icon: bool,
    message: bool,
    quote: bool,
    documents: bool,
    action: bool,
    hint: bool,
}

#[derive(Clone, Copy, Debug)]
struct Metrics {
    title_h: f64,
    msg_h: f64,
    quote_h: f64,
    hint_h: f64,
    documents_h: f64,
    action_w: f64,
    inner_width: f32,
    pad_x: f64,
    gap: f64,
    text_gap: f64,
    btn_h: f64,
    hero: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    icon: bool,
    message: bool,
    quote: bool,
    documents: bool,
    hint: bool,
    total: f64,
}

pub type Illustration<'a> = &'a dyn Fn(&mut PaintCtx<'_>, Rect);

/// What an empty state shows: optional icon or illustration, title, optional message, the
/// thing's own words as a quote, the documents the workspace holds, and optional action label.
#[derive(Clone, Copy)]
pub struct EmptyStateContent<'a> {
    pub icon: Option<Icon>,
    pub illustration: Option<Illustration<'a>>,
    pub title: &'a str,
    pub message: Option<&'a str>,
    /// What the thing here said itself, in its own words: a panic's message, a service's
    /// answer. Shown under the message as a quote, bounded to a few lines, never behind
    /// Details.
    pub quote: Option<&'a str>,
    pub documents: &'a [crate::widget::DocumentOffer<'a>],
    pub action_label: Option<&'a str>,
    /// A quiet line under the action: the other way in ("Or drop a file
    /// here"), never a step of its own.
    pub hint: Option<&'a str>,
    /// Whether the hint is a route of its own to press, rather than a sentence about this one:
    /// drawn as a link-like line and announcing a button.
    pub hint_action: bool,
    pub error: bool,
}

impl std::fmt::Debug for EmptyStateContent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmptyStateContent")
            .field("icon", &self.icon)
            .field("illustration", &self.illustration.is_some())
            .field("title", &self.title)
            .field("message", &self.message)
            .field("quote", &self.quote)
            .field("documents", &self.documents.len())
            .field("action_label", &self.action_label)
            .field("hint", &self.hint)
            .field("hint_action", &self.hint_action)
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
            quote: None,
            documents: &[],
            action_label: None,
            hint: None,
            hint_action: false,
            error: false,
        }
    }

    /// A quote worth showing: one with something in it.
    fn quote(&self) -> Option<&'a str> {
        self.quote.filter(|quote| !quote.trim().is_empty())
    }

    /// The accessibility of a state painted with [`EmptyState::paint`]: a
    /// group over `rect` named by the title and described by the message,
    /// what the thing said, and its action as a button where it was drawn (`action`, the rect
    /// `paint` returned), answering `action_target`. A hint that is a route of its own
    /// (`hint_action`) is a button where it was drawn too (`hint`, answering `hint_target`);
    /// one that only explains the action is part of the description. A widget that paints
    /// an empty state itself adds this node, so what is drawn is also there
    /// for assistive technology and agents.
    pub fn access_node(
        &self,
        rect: Rect,
        action: Option<Rect>,
        action_target: &str,
        hint: Option<Rect>,
        hint_target: &str,
    ) -> tilo_ui_base::a11y::AccessNode {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        let mut node = AccessNode::new(Role::Group)
            .with_name(self.title)
            .with_bounds(rect);
        let said_hint = self.hint.filter(|_| !self.hint_action);
        if let Some(described) = described(self.message, said_hint, self.quote()) {
            node = node.with_description(described);
        }
        let mut children = Vec::new();
        if let (Some(label), Some(action)) = (self.action_label, action) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_bounds(action)
                    .with_state(AccessState::new().focusable(true))
                    .with_action_target(action_target),
            );
        }
        if let (Some(label), Some(hint)) = (self.hint.filter(|_| self.hint_action), hint) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label)
                    .with_bounds(hint)
                    .with_state(AccessState::new().focusable(true))
                    .with_action_target(hint_target),
            );
        }
        if !children.is_empty() {
            node = node.with_children(children);
        }
        node
    }

    fn parts(&self) -> Parts {
        Parts {
            icon: self.icon.is_some() || self.illustration.is_some(),
            message: self.message.is_some(),
            quote: self.quote().is_some(),
            documents: !self.documents.is_empty(),
            action: self.action_label.is_some(),
            hint: self.hint.is_some(),
        }
    }
}

/// What a state reads as, for assistive technology: its message, its other way in, and the
/// thing's own words under it, the last named as a quote so they read as its, not the
/// shell's.
fn described(message: Option<&str>, hint: Option<&str>, quote: Option<&str>) -> Option<String> {
    let mut said: Vec<String> = message.into_iter().chain(hint).map(str::to_owned).collect();
    if let Some(quote) = quote {
        said.push(tilo_ui_base::i18n::widget_format(
            "empty-state-quote",
            &[("said", quote.into())],
        ));
    }
    (!said.is_empty()).then(|| said.join(" "))
}

#[derive(Clone, Debug)]
struct StackStyle {
    title: TextStyle,
    message: TextStyle,
    quote: TextStyle,
    action: TextStyle,
    hint: TextStyle,
    gap: f64,
    text_gap: f64,
    measure: f64,
    pad_y: f64,
    min_button: f64,
    glyph: f64,
    details_lines: details::Style,
    details_toggle: f64,
    details_gap: f64,
    quote_pad_x: f64,
    quote_pad_y: f64,
    quote_radius: f64,
}

impl StackStyle {
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
            // The thing's own words in the code face, wrapped by glyph so a long line still
            // reads, and left-aligned even where the prose around it is centred: a quote is
            // not the shell's sentence.
            quote: wrapped(TextRole::Code).with_align(ParagraphAlign::Start),
            action: wrapped(TextRole::Label),
            hint: wrapped(TextRole::Caption),
            gap: f64::from(theme.tokens.spacing.s4),
            text_gap: f64::from(theme.tokens.spacing.s3),
            measure: f64::from(theme.tokens.metrics.message_measure),
            pad_y: f64::from(theme.tokens.spacing.s2),
            min_button: f64::from(theme.tokens.metrics.min_pointer_target),
            glyph: if inline { glyph } else { glyph * 2.0 },
            details_lines: details::Style::new(theme),
            details_toggle: crate::widget::ButtonFace::height(
                crate::widget::ControlSize::Small,
                theme,
            ),
            details_gap: f64::from(theme.tokens.spacing.s2),
            quote_pad_x: f64::from(theme.tokens.spacing.s2),
            quote_pad_y: f64::from(theme.tokens.spacing.s1),
            quote_radius: f64::from(theme.tokens.radii.item),
        }
    }

    fn details_height(
        &self,
        text_renderer: &mut TextRenderer,
        width: f64,
        details: Option<&str>,
        open: bool,
    ) -> f64 {
        let Some(details) = details else {
            return 0.0;
        };
        let mut height = self.gap + self.details_toggle;
        if open {
            let (_, lines_h) = details::lay_out(
                text_renderer,
                &self.details_lines,
                details,
                0.0,
                0.0,
                f64::from(details_width(width, self.gap)),
            );
            height += self.details_gap + lines_h;
        }
        height
    }

    fn resolved(theme: &Theme, inline: bool) -> Self {
        let layout = Self::layout(theme, inline);
        let centered = |style: TextStyle| style.with_align(ParagraphAlign::Center);
        Self {
            title: centered(layout.title),
            message: centered(layout.message),
            action: centered(layout.action),
            hint: centered(layout.hint),
            ..layout
        }
    }

    fn measure(
        &self,
        text_renderer: &mut TextRenderer,
        width: f64,
        content: &EmptyStateContent<'_>,
        documents_h: f64,
    ) -> Metrics {
        let gap = self.gap;
        let pad_x = gap;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "window widths are logical pixels well within f32 range"
        )]
        let inner_width = (width - gap * 2.0).clamp(1.0, self.measure) as f32;
        let (_, title_h) =
            text_renderer.measure_style(content.title, Some(inner_width), &self.title);
        let (_, msg_h) = content.message.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.message)
        });
        let quote_h = content.quote().map_or(0.0, |quote| {
            let wrap = (f64::from(inner_width) - self.quote_pad_x * 2.0).max(1.0);
            let (_, said_h) = quote_layout(text_renderer, &self.quote, wrap, quote);
            said_h + self.quote_pad_y * 2.0
        });
        let (action_w, action_h) = content.action_label.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.action)
        });
        let (_, hint_h) = content.hint.map_or((0.0, 0.0), |value| {
            text_renderer.measure_style(value, Some(inner_width), &self.hint)
        });
        Metrics {
            title_h: f64::from(title_h),
            msg_h: f64::from(msg_h),
            quote_h,
            hint_h: f64::from(hint_h),
            documents_h,
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

    fn measure_with(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        content: &EmptyStateContent<'_>,
    ) -> Metrics {
        let mut metrics = self.measure(text_renderer, width, content, 0.0);
        metrics.documents_h = crate::widget::document_offers::height(
            text_renderer,
            theme,
            content.documents,
            f64::from(metrics.inner_width),
        );
        metrics
    }
}

fn details_width(width: f64, gap: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "window widths are logical pixels well within f32 range"
    )]
    let width = (width - gap * 2.0).clamp(1.0, 520.0) as f32;
    width
}

/// The most of an app's quote a state shows, in lines.
const QUOTE_LINES: u32 = 3;

/// `quote` as one paragraph, at most [`QUOTE_LINES`] lines of it at `width`: its runs of
/// whitespace read as single spaces, its first lines are what is shown, and a longer quote
/// ends with an ellipsis. Returns the words to draw and the height they take. A state caps
/// what the app said, so 10 KB of output cannot push the card apart.
fn quote_layout(
    text: &mut TextRenderer,
    style: &TextStyle,
    width: f64,
    quote: &str,
) -> (String, f64) {
    let words = quote.split_whitespace().collect::<Vec<_>>().join(" ");
    let wrap = tilo_ui_base::num::f32_of_f64(width.max(1.0));
    let (_, height) = text.measure_style(&words, Some(wrap), style);
    let height = f64::from(height);
    // A line as the renderer actually paints it, so the bound holds at any text scale.
    let (_, one) = text.measure_style("Ag", Some(wrap), style);
    let most = f64::from(one) * f64::from(QUOTE_LINES);
    if height <= most || words.is_empty() {
        return (words, height);
    }
    // The longest beginning that still fits, ended with an ellipsis: what the app said is
    // cut where the card can show it, never left to overflow the stack.
    let chars: Vec<char> = words.chars().collect();
    let (mut low, mut high) = (0usize, chars.len());
    let mut best = "…".to_owned();
    let mut best_h = 0.0;
    while low <= high {
        let mid = low + (high - low) / 2;
        let mut candidate: String = chars[..mid].iter().collect();
        while candidate.ends_with(char::is_whitespace) {
            candidate.pop();
        }
        candidate.push('…');
        let (_, candidate_h) = text.measure_style(&candidate, Some(wrap), style);
        let candidate_h = f64::from(candidate_h);
        if candidate_h <= most {
            best = candidate;
            best_h = candidate_h;
            low = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            high = mid - 1;
        }
    }
    (best, best_h)
}

fn details_face(toggle: Rect, open: bool) -> crate::widget::ButtonFace<'static> {
    crate::widget::ButtonFace::new(toggle, "empty-state-details")
        .role(crate::widget::ButtonRole::Plain)
        .control_size(crate::widget::ControlSize::Small)
        .icon(if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        })
}

fn details_toggle(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    bottom: f64,
    inline: bool,
    open: bool,
) -> Rect {
    let style = StackStyle::resolved(theme, inline);
    let width = details_face(Rect::ZERO, open)
        .intrinsic_width(text, theme)
        .min(rect.width());
    let x0 = rect.center().x - width / 2.0;
    let y0 = bottom + style.gap;
    Rect::new(x0, y0, x0 + width, y0 + style.details_toggle)
}

impl Fit {
    /// Shed elements rather than overflow the rect: the hero disc is the
    /// largest element and carries the least information, so it goes first;
    /// the message goes next, then the app's own words, and the documents the state lists
    /// last of the optional ones, only if the title alone still doesn't fit. The title
    /// and the action are never dropped — the first is the whole point and
    /// the second is the only thing the person can do here.
    fn choose(height: f64, parts: Parts, m: Metrics) -> Fit {
        let stack = |icon: bool, message: bool, quote: bool, documents: bool, hint: bool| {
            let mut t = m.title_h;
            if icon {
                t += m.hero + m.gap;
            }
            if message {
                t += m.text_gap + m.msg_h;
            }
            if quote {
                t += m.text_gap + m.quote_h;
            }
            if documents {
                t += m.gap + m.documents_h;
            }
            if parts.action {
                t += m.gap + m.btn_h;
            }
            if hint {
                t += m.text_gap + m.hint_h;
            }
            t
        };
        let mut icon = parts.icon;
        let mut message = parts.message;
        let mut quote = parts.quote;
        let mut documents = parts.documents;
        let mut hint = parts.hint;
        if icon && stack(true, message, quote, documents, hint) > height {
            icon = false;
        }
        if message && stack(icon, true, quote, documents, hint) > height {
            message = false;
        }
        if quote && stack(icon, message, true, documents, hint) > height {
            quote = false;
        }
        if documents && stack(icon, message, quote, true, hint) > height {
            documents = false;
        }
        if hint && stack(icon, message, quote, documents, true) > height {
            hint = false;
        }
        Fit {
            icon,
            message,
            quote,
            documents,
            hint,
            total: stack(icon, message, quote, documents, hint),
        }
    }
}

/// Where everything in an empty state goes inside a rect: what [`EmptyState::paint`] draws,
/// and what [`EmptyState::layout`] hands back without painting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EmptyStateLayout {
    pub natural_height: f64,
    pub hero: Option<Rect>,
    pub action: Option<Rect>,
}

struct Placed {
    style: StackStyle,
    metrics: Metrics,
    hero: Option<Rect>,
    title_y: f64,
    message_y: Option<f64>,
    /// The app's own words where they go: what is drawn, in the well that holds them.
    quote: Option<(String, Rect)>,
    documents: Option<Rect>,
    action: Option<Rect>,
    hint: Option<Rect>,
    natural: f64,
    bottom: f64,
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
    let mut metrics = style.measure_with(text, theme, rect.width(), content);
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
            let candidate_metrics = candidate.measure_with(text, theme, rect.width(), content);
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
    let quote = content.quote().filter(|_| fit.quote).map(|quote| {
        y += metrics.text_gap;
        let width = f64::from(metrics.inner_width);
        let wrap = (width - style.quote_pad_x * 2.0).max(1.0);
        let (said, said_h) = quote_layout(text, &style.quote, wrap, quote);
        let rect = Rect::new(
            cx - width / 2.0,
            y,
            cx + width / 2.0,
            y + said_h + style.quote_pad_y * 2.0,
        );
        y = rect.y1;
        (said, rect)
    });
    let documents = (fit.documents && !content.documents.is_empty()).then(|| {
        y += gap;
        let width = f64::from(metrics.inner_width);
        let at = Rect::new(
            cx - width / 2.0,
            y,
            cx + width / 2.0,
            y + metrics.documents_h,
        );
        y += metrics.documents_h;
        at
    });
    let action = content.action_label.and_then(|_| {
        y += gap;
        let bw = (metrics.action_w + metrics.pad_x * 2.0).min(rect.width());
        let x0 = cx - bw / 2.0;
        let action_rect = Rect::new(x0, y, x0 + bw, y + metrics.btn_h);
        (action_rect.y1 > rect.y0 && action_rect.y0 < rect.y1).then_some(action_rect)
    });
    let bottom = if content.action_label.is_some() {
        y + metrics.btn_h
    } else {
        y
    };
    let hint = (fit.hint && content.hint.is_some()).then(|| {
        y = bottom + metrics.text_gap;
        Rect::new(
            cx - f64::from(metrics.inner_width) / 2.0,
            y,
            cx + f64::from(metrics.inner_width) / 2.0,
            y + metrics.hint_h,
        )
    });
    Placed {
        style,
        metrics,
        hero,
        title_y,
        message_y,
        quote,
        documents,
        action,
        hint,
        natural,
        bottom,
    }
}

/// What one paint of an empty state drew that a press answers: the action's rect when
/// `action_label` is `Some` and it is on screen, and one row per document the state listed,
/// in the order the documents came.
#[derive(Clone, Debug, Default)]
pub struct PaintedStack {
    pub action: Option<Rect>,
    pub documents: Vec<Rect>,
}

fn shown_content<'a>(
    content: &EmptyStateContent<'a>,
    waiting: Option<&'a tilo_ui_base::workspace_sync::Waiting>,
) -> EmptyStateContent<'a> {
    let Some(waiting) = waiting else {
        return *content;
    };
    EmptyStateContent {
        icon: Some(waiting.icon),
        illustration: None,
        title: &waiting.title,
        message: waiting.message.as_deref(),
        quote: content.quote,
        // While the host cannot be reached, the person works on what this
        // device has: the app's action, and the documents kept here, stay.
        // The other way in stays too: it goes to this device's files.
        documents: content.documents,
        action_label: content.action_label.filter(|_| waiting.keeps_action()),
        hint: content.hint,
        hint_action: content.hint_action,
        error: false,
    }
}

fn paint_empty_state(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    content: &EmptyStateContent<'_>,
    hover: Point,
    action_focused: bool,
    hint_focused: bool,
    inline: bool,
    spinner: Option<&mut crate::widget::Spinner>,
) -> PaintedStack {
    let mut painted = PaintedStack::default();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return painted;
    }
    let waiting = tilo_ui_base::workspace_sync::waiting();
    let mut waiting_spinner = None;
    let mut waiting_step = None;
    if let Some(since) = waiting.as_ref().and_then(|waiting| waiting.spinner_since) {
        let turning = ctx.theme.motion().loops();
        let now = crate::time::now();
        let (step, next) = if turning {
            (
                tilo_ui_base::animation::indicator_step(since, now),
                Some(tilo_ui_base::animation::next_indicator_step(since, now)),
            )
        } else {
            (0, None)
        };
        waiting_step = Some((step, next));
        waiting_spinner = Some(crate::widget::Spinner::started_at(since));
    }
    let content = shown_content(content, waiting.as_ref());
    let content = &content;
    let spinner = if waiting.is_some() {
        waiting_spinner.as_mut()
    } else {
        spinner
    };
    let theme = ctx.theme;
    let transform = ctx.transform;
    let placed = place(ctx.text, theme, rect, content, inline);
    let Placed {
        style,
        metrics,
        hero,
        title_y,
        message_y,
        quote,
        documents,
        action,
        hint,
        ..
    } = placed;

    if let Some(slot) = hero {
        if let Some((step, next)) = waiting_step {
            tilo_ui_base::workspace_sync::spinner_painted(
                step,
                next,
                transform.transform_rect_bbox(slot),
            );
        }
        if let Some(spinner) = spinner {
            spinner.set_rect(slot);
            spinner.draw_child(
                ctx.scene,
                transform,
                ctx.text,
                theme,
                &PresenceCtx::default().draw_env(),
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
        ctx.text(
            msg,
            &style.message,
            theme.text_dim,
            Point::new(text_x, y),
            Some(metrics.inner_width),
        );
    }

    if let Some((said, rect)) = &quote {
        // The thing's own words, set apart as a quote of them: a quiet well in the code face,
        // left-aligned under the shell's centred sentence.
        let well = tilo_ui_base::tokens::colors::over(
            theme.tokens.colors.surfaces.content,
            theme.text_primary,
            0.06,
        );
        ctx.fill_round(*rect, style.quote_radius, well);
        let wrap = (rect.width() - style.quote_pad_x * 2.0).max(1.0);
        ctx.text(
            said,
            &style.quote,
            theme.text_primary,
            Point::new(rect.x0 + style.quote_pad_x, rect.y0 + style.quote_pad_y),
            Some(tilo_ui_base::num::f32_of_f64(wrap)),
        );
    }

    if let Some(list) = documents {
        painted.documents =
            crate::widget::document_offers::paint(ctx, list, content.documents, hover).rows();
    }

    if let (Some(label), Some(action_rect)) = (content.action_label, action) {
        let face = crate::widget::ButtonFace::new(action_rect, label)
            .role(crate::widget::ButtonRole::Primary)
            .hovered(action_rect.contains(hover))
            .focused(action_focused);
        crate::widget::Button::paint(ctx.scene, transform, ctx.text, theme, &face);
        note_painted_action(
            content,
            transform.transform_rect_bbox(rect),
            transform.transform_rect_bbox(action_rect),
        );
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
    if let (Some(line), Some(rect)) = (content.hint, hint) {
        // A hint that is only a sentence about the action is tertiary ink, never a step of its
        // own. One that is a route says so: it stands in the same place, one ink up, and takes
        // the primary ink under the pointer and while a reader has it.
        let ink = if content.hint_action {
            if rect.contains(hover) || hint_focused {
                theme.text_primary
            } else {
                theme.text_dim
            }
        } else {
            theme.text_tertiary
        };
        ctx.text(
            line,
            &style.hint,
            ink,
            Point::new(rect.x0, rect.y0),
            Some(metrics.inner_width),
        );
        if hint_focused {
            crate::widget::paint_focus_ring(
                ctx.scene,
                transform,
                rect,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
    }
    painted.action = action;
    painted
}

thread_local! {
    static PAINTED: RefCell<Vec<Vec<PaintedAction>>> = const { RefCell::new(Vec::new()) };
}

/// Most painted actions one recording keeps: a recorder nobody drains
/// stays bounded.
const MAX_PAINTED: usize = 256;

/// An empty-state action as it was painted with [`EmptyState::paint`]: the state it belongs
/// to and where both stand, through the paint transform they were drawn with.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedAction {
    pub label: String,
    pub rect: Rect,
    pub title: String,
    pub message: Option<String>,
    pub area: Rect,
}

/// Keep every empty-state action painted on this thread from now on, until
/// the matching [`take_painted_actions`] hands them over. Recordings nest:
/// what an inner one takes is also kept by the one around it.
///
/// An app that paints an empty state itself draws its action as pixels;
/// its host records them so the action is also described in the
/// accessibility tree, and a check that every action drawn is described
/// compares the two.
pub fn record_painted_actions() {
    PAINTED.with(|painted| painted.borrow_mut().push(Vec::new()));
}

/// The empty-state actions painted since the matching [`record_painted_actions`], ending
/// that recording.
#[must_use]
pub fn take_painted_actions() -> Vec<PaintedAction> {
    PAINTED.with(|painted| {
        let mut painted = painted.borrow_mut();
        let taken = painted.pop().unwrap_or_default();
        if let Some(outer) = painted.last_mut() {
            let spare = MAX_PAINTED.saturating_sub(outer.len());
            outer.extend(taken.iter().take(spare).cloned());
        }
        taken
    })
}

fn note_painted_action(content: &EmptyStateContent<'_>, area: Rect, rect: Rect) {
    PAINTED.with(|painted| {
        if let Some(painted) = painted.borrow_mut().last_mut()
            && painted.len() < MAX_PAINTED
            && let Some(label) = content.action_label
        {
            painted.push(PaintedAction {
                label: label.to_owned(),
                rect,
                title: content.title.to_owned(),
                message: content.message.map(str::to_owned),
                area,
            });
        }
    });
}

const PAINTED_TARGET: &str = "tilo.painted-empty-state:";

/// Whether `target` names a painted empty state's action ([`PaintedEmptyStates`]): no
/// widget handles it, an activation presses where it is drawn.
pub fn is_painted_target(target: &str) -> bool {
    target.starts_with(PAINTED_TARGET)
}

/// The empty states a host's content painted itself with
/// [`EmptyState::paint`], described for it: each becomes a group named by
/// its title with its action as a button where it was drawn, unless the
/// content describes that button already. Activating the button clicks
/// where it is drawn, exactly as a pointer would, so the content's own
/// hit test runs it.
///
/// A host keeps one per content it draws: [`Self::draw`] around the
/// content's paint, [`Self::describe`] over its accessibility and
/// [`Self::action_rect`] for where an activation lands.
#[derive(Debug, Default)]
pub struct PaintedEmptyStates {
    painted: Vec<PaintedAction>,
}

impl PaintedEmptyStates {
    /// Whether the last draw painted no empty state: [`Self::describe`] then returns the
    /// content's node as it is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.painted.is_empty()
    }

    /// Run `draw`, keeping the empty-state actions it paints in the space `transform` maps
    /// from: the content's own coordinates.
    pub fn draw(&mut self, transform: Affine, draw: impl FnOnce()) {
        record_painted_actions();
        draw();
        let painted = take_painted_actions();
        let into_content = transform.inverse();
        self.painted = painted
            .into_iter()
            .map(|action| PaintedAction {
                rect: into_content.transform_rect_bbox(action.rect),
                area: into_content.transform_rect_bbox(action.area),
                ..action
            })
            .collect();
    }

    /// `node`, the content's own accessibility, with each painted state whose action
    /// neither it nor a widget inside `content` describes.
    #[must_use]
    pub fn describe(
        &self,
        node: Option<tilo_ui_base::a11y::AccessNode>,
        content: &dyn tilo_ui_base::widget::Widget,
    ) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        if self.painted.is_empty() {
            return node;
        }
        let mut node = node;
        if let Some(node) = &mut node {
            self.place_unplaced_buttons(node);
        }
        let mut described = Vec::new();
        if let Some(node) = &node {
            buttons_in(node, &mut described);
        }
        content.for_each_child(&mut |child| buttons_under(child, &mut described));
        // A group joins the content's own children, so it is addressed as
        // they are: keyed beside keyed controls, by position otherwise.
        let keyed = node
            .as_ref()
            .is_some_and(|node| node.children.iter().any(|child| child.key.is_some()));
        let missing = self
            .painted
            .iter()
            .enumerate()
            .filter(|(_, action)| {
                !described.iter().any(|(label, bounds)| {
                    *label == action.label
                        && bounds
                            .is_none_or(|bounds: Rect| bounds.intersect(action.rect).area() > 0.0)
                })
            })
            .map(|(index, action)| {
                let mut group = AccessNode::new(Role::Group)
                    .with_name(action.title.clone())
                    .with_bounds(action.area);
                if let Some(message) = &action.message {
                    group = group.with_description(message.clone());
                }
                if keyed {
                    group = group.with_hashed_key(format!("{PAINTED_TARGET}{index}"));
                }
                group.with_children(vec![
                    AccessNode::new(Role::Button)
                        .with_name(action.label.clone())
                        .with_bounds(action.rect)
                        .with_action_target(format!("{PAINTED_TARGET}{index}")),
                ])
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return node;
        }
        let mut node = node.unwrap_or_else(|| AccessNode::new(Role::Generic));
        node.children.extend(missing);
        Some(node)
    }

    fn place_unplaced_buttons(&self, node: &mut tilo_ui_base::a11y::AccessNode) {
        if node.role == tilo_ui_base::a11y::Role::Button
            && node.bounds.is_none()
            && let Some(name) = &node.name
            && let Some(action) = self.painted.iter().find(|action| action.label == *name)
        {
            node.bounds = Some(action.rect.into());
        }
        for child in &mut node.children {
            self.place_unplaced_buttons(child);
        }
    }

    /// Where the painted action at `path` beneath `described` (what [`Self::describe`]
    /// returned) is drawn; `None` for any other node.
    #[must_use]
    pub fn action_rect(
        &self,
        described: &tilo_ui_base::a11y::AccessNode,
        path: &[usize],
    ) -> Option<Rect> {
        let mut node = described;
        for &index in path {
            node = node.children.get(index)?;
        }
        let index = node
            .action_target
            .as_deref()?
            .strip_prefix(PAINTED_TARGET)?
            .parse::<usize>()
            .ok()?;
        self.painted.get(index).map(|action| action.rect)
    }
}

fn buttons_in(node: &tilo_ui_base::a11y::AccessNode, out: &mut Vec<(String, Option<Rect>)>) {
    if node.role == tilo_ui_base::a11y::Role::Button
        && let Some(name) = &node.name
    {
        out.push((
            name.clone(),
            node.bounds.map(tilo_ui_base::a11y::AccessBounds::to_rect),
        ));
    }
    for child in &node.children {
        buttons_in(child, out);
    }
}

fn buttons_under(widget: &dyn tilo_ui_base::widget::Widget, out: &mut Vec<(String, Option<Rect>)>) {
    if let Some(node) = widget.accessibility() {
        buttons_in(&node, out);
    }
    widget.for_each_child(&mut |child| buttons_under(child, out));
}

#[derive(Clone)]
enum Handler {
    Binding(tilo_ui_base::widget::BoundAction<()>),
    Press(Rc<RefCell<Box<dyn FnMut()>>>),
}

/// A centered empty-state placeholder: hero icon + title + message + action.
pub struct EmptyState {
    icon: Option<Icon>,
    illustration: Option<Box<dyn Fn(&mut PaintCtx<'_>, Rect)>>,
    error: bool,
    title: String,
    message: Option<String>,
    quote: Option<String>,
    action_label: Option<String>,
    action_id: Option<String>,
    action_enabled: bool,
    handler: Option<Handler>,
    engine: Option<crate::widget::Button>,
    /// A quieter line under the action, and how to run it when it is a route of its own.
    hint: Option<String>,
    hint_resolved: Option<String>,
    hint_handler: Option<Handler>,
    hint_engine: Option<crate::widget::Button>,
    hint_rect: Rect,
    hint_focused: bool,
    rect: Rect,
    action_rect: Rect,
    /// What the action's target must reach to be pressed.
    input: InteractionEnvironment,
    last_cursor: Point,
    action_focused: bool,
    layout_style: StackStyle,
    layout_theme: Theme,
    scroll_content: bool,
    inline: bool,
    spinner: Option<crate::widget::Spinner>,
    busy: bool,
    resolved: (String, Option<String>, Option<String>),
    i18n_revision: u64,
    layout_dirty: bool,
    details: Option<String>,
    details_open: bool,
    details_rect: Rect,
    details_focused: bool,
    /// The open Details' lines, where the layout put them.
    details_lines: Vec<details::Line>,
    /// The verbatim line whose Copy action the pointer is over.
    copy_hovered: Option<usize>,
    id: Option<String>,
    drawn: Option<Painted>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Painted {
    rect: Rect,
    action_hovered: bool,
    action_focused: bool,
    hint_hovered: bool,
    hint_focused: bool,
    enabled: bool,
    details_hovered: bool,
    details_open: bool,
    details_focused: bool,
    copy_hovered: Option<usize>,
}

impl EmptyState {
    /// Construct with just a title (the minimal case). Chain
    /// [`with_icon`](Self::with_icon) / [`with_message`](Self::with_message) /
    /// [`with_action`](Self::with_action) for the fuller treatment. The title,
    /// message and action label are resolved through the i18n layer, like
    /// [`Text`](crate::widget::Text).
    pub fn new(title: impl Into<String>) -> Self {
        Self::new_owned(title.into())
    }

    fn new_owned(title: String) -> Self {
        let mut state = Self {
            icon: None,
            illustration: None,
            error: false,
            title,
            message: None,
            quote: None,
            action_label: None,
            action_id: None,
            action_enabled: true,
            handler: None,
            engine: None,
            hint: None,
            hint_resolved: None,
            hint_handler: None,
            hint_engine: None,
            hint_rect: Rect::ZERO,
            hint_focused: false,
            rect: Rect::ZERO,
            action_rect: Rect::ZERO,
            input: InteractionEnvironment::POINTER,
            last_cursor: Point::ZERO,
            action_focused: false,
            layout_style: StackStyle::layout(&Theme::default(), false),
            layout_theme: Theme::default(),
            scroll_content: false,
            inline: false,
            spinner: None,
            busy: false,
            resolved: (String::new(), None, None),
            i18n_revision: tilo_ui_base::i18n::I18nManager::revision(),
            layout_dirty: true,
            details: None,
            details_open: false,
            details_rect: Rect::ZERO,
            details_focused: false,
            details_lines: Vec::new(),
            copy_hovered: None,
            id: None,
            drawn: None,
        };
        state.resolve();
        state
    }

    fn painted_state(&self) -> Painted {
        Painted {
            rect: self.rect,
            action_hovered: self.action_label.is_some()
                && self.action_rect.contains(self.last_cursor),
            action_focused: self.action_focused,
            hint_hovered: self.hint_handler.is_some() && self.hint_rect.contains(self.last_cursor),
            hint_focused: self.hint_focused,
            enabled: self.is_enabled(),
            details_hovered: self.details.is_some() && self.details_rect.contains(self.last_cursor),
            details_open: self.details_open,
            details_focused: self.details_focused,
            copy_hovered: self.copy_hovered,
        }
    }

    /// Keep `details`, the technical side of a failure (what a service answered, an error
    /// code), behind a "Details" disclosure under the stack.
    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.set_details(Some(details.into()));
        self
    }

    /// Replace the technical detail; an empty one removes the disclosure.
    pub fn set_details(&mut self, details: Option<String>) {
        let details = details.filter(|details| !details.trim().is_empty());
        if details != self.details {
            if details.is_none() {
                self.details_open = false;
                self.details_focused = false;
            }
            self.details = details;
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    #[must_use]
    pub fn details_open(&self) -> bool {
        self.details_open
    }

    /// Open or close the "Details" disclosure.
    pub fn toggle_details(&mut self) {
        if self.details.is_some() {
            self.details_open = !self.details_open;
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    fn details_control_id(&self) -> String {
        format!("{}:details", self.action_control_id())
    }

    fn action_target(&self) -> Rect {
        self.input.minimum_target_rect(self.action_rect)
    }

    /// Place the controls where the layout puts them: the action's face and
    /// the "Details" disclosure. Where a control stands is the layout's, never
    /// a paint's, so a frame read before it is drawn — a test, assistive
    /// technology, a press on a window that has laid out and has not yet
    /// painted — finds them inside the state rather than at the window's
    /// origin, where a control nothing placed stands.
    fn place_controls(&mut self) {
        let rect = self.rect;
        let theme = self.layout_theme.clone();
        let inline = self.inline;
        let details = self.details.clone();
        let open = self.details_open;
        let content = self.content();
        let (action, disclosure, lines, hint) = if rect.width() <= 0.0 || rect.height() <= 0.0 {
            (Rect::ZERO, Rect::ZERO, Vec::new(), Rect::ZERO)
        } else {
            let waiting = tilo_ui_base::workspace_sync::waiting();
            let content = shown_content(&content, waiting.as_ref());
            tilo_ui_base::text_render::with_layout_renderer(&theme, |text| {
                let details_h = StackStyle::resolved(&theme, inline).details_height(
                    text,
                    rect.width(),
                    details.as_deref(),
                    open,
                );
                let stack = Rect::new(
                    rect.x0,
                    rect.y0,
                    rect.x1,
                    (rect.y1 - details_h).max(rect.y0),
                );
                let placed = place(text, &theme, stack, &content, inline);
                let disclosure = details.as_deref().map_or(Rect::ZERO, |_| {
                    details_toggle(text, &theme, rect, placed.bottom, inline, open)
                });
                let lines = match details.as_deref() {
                    Some(details) if open => {
                        let style = StackStyle::layout(&theme, inline);
                        let width = f64::from(details_width(rect.width(), style.gap));
                        details::lay_out(
                            text,
                            &style.details_lines,
                            details,
                            rect.center().x - width / 2.0,
                            disclosure.y1 + style.details_gap,
                            width,
                        )
                        .0
                    }
                    _ => Vec::new(),
                };
                (
                    placed.action.unwrap_or(Rect::ZERO),
                    disclosure,
                    lines,
                    placed.hint.unwrap_or(Rect::ZERO),
                )
            })
        };
        self.action_rect = action;
        self.hint_rect = hint;
        self.details_rect = disclosure;
        if self.copy_hovered.is_some_and(|line| line >= lines.len()) {
            self.copy_hovered = None;
        }
        self.details_lines = lines;
        if let Some(engine) = &mut self.engine {
            engine.apply_layout_external(self.action_rect);
        }
        if let Some(engine) = &mut self.hint_engine {
            engine.apply_layout_external(self.hint_rect);
        }
    }

    fn paint_details(&self, ctx: &mut PaintCtx<'_>) {
        if self.details.is_none() {
            return;
        }
        let theme = ctx.theme;
        let style = StackStyle::resolved(theme, self.inline);
        let toggle = self.details_rect;
        let face = details_face(toggle, self.details_open)
            .hovered(toggle.contains(self.last_cursor))
            .focused(self.details_focused);
        crate::widget::Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        if self.details_focused {
            crate::widget::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                toggle,
                f64::from(theme.tokens.radii.control),
                theme,
            );
        }
        if self.details_open {
            let lines = &style.details_lines;
            let well = tilo_ui_base::tokens::colors::over(
                theme.tokens.colors.surfaces.content,
                theme.text_primary,
                0.06,
            );
            for (index, line) in self.details_lines.iter().enumerate() {
                let wrap = Some(tilo_ui_base::num::f32_of_f64(line.wrap));
                match line.kind {
                    details::Kind::Words => {
                        ctx.text(&line.text, &lines.words, theme.text_dim, line.origin, wrap);
                    }
                    details::Kind::Verbatim => {
                        ctx.fill_round(line.rect, lines.radius, well);
                        ctx.text(
                            &line.text,
                            &lines.verbatim,
                            theme.text_primary,
                            line.origin,
                            wrap,
                        );
                    }
                }
                if let Some(copy) = line.copy {
                    let hovered = self.copy_hovered == Some(index);
                    if hovered {
                        ctx.fill_round(
                            copy,
                            lines.radius,
                            theme.tokens.colors.intent.selection_hover,
                        );
                    }
                    let glyph = Rect::from_center_size(copy.center(), (lines.icon, lines.icon));
                    let ink = if hovered {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    };
                    Icon::Copy.draw(ctx.scene, ctx.transform, glyph, ink);
                }
            }
        }
    }

    /// The verbatim line whose Copy action stands at `point`.
    fn copy_at(&self, point: Point) -> Option<usize> {
        self.details_lines
            .iter()
            .position(|line| line.copy.is_some_and(|copy| copy.contains(point)))
    }

    fn copy_control_id(&self, line: usize) -> String {
        format!("{}:copy:{line}", self.details_control_id())
    }

    /// Copy the verbatim line `line` of the open Details.
    fn copy_line(&self, line: usize) -> bool {
        let Some(line) = self
            .details_lines
            .get(line)
            .filter(|line| line.kind == details::Kind::Verbatim)
        else {
            return false;
        };
        tilo_ui_base::text_selection::clipboard_copy(&line.text);
        true
    }

    /// Something failed to load or open: `title` says what, `detail` says
    /// why. The detail shows as one bounded line; assistive technology
    /// reads all of it.
    pub fn error(title: impl Into<String>, detail: impl Into<String>) -> Self {
        let mut state = Self::new(title).with_message(detail);
        state.error = true;
        state
    }

    /// An empty section inside a page ("No servers yet" under a heading): the title is a
    /// `Heading`, the stack hugs its content and claims no full-pane height.
    #[must_use]
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self.layout_dirty = true;
        self
    }

    /// Something is on its way ("Scanning your disk"): a spinner in the hero slot that
    /// keeps itself moving.
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

    /// What the thing here said itself, in its own words, shown under the message as a quote:
    /// a panic's message, a service's answer. A few lines of it stay in view — never behind
    /// "Details", where the whole of what was said can wait.
    #[must_use]
    pub fn with_quote(mut self, quote: impl Into<String>) -> Self {
        self.quote = Some(quote.into());
        self.layout_dirty = true;
        self.drawn = None;
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

    /// What a state without an action is addressed by (a scenario's expectation, a test); a
    /// state with one is addressed by its action.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Stable identity for the optional call-to-action.
    #[must_use]
    pub fn with_action_id(mut self, id: impl Into<String>) -> Self {
        self.action_id = Some(id.into());
        self.rebuild_engine();
        self
    }

    /// Route pointer, keyboard and accessibility activation through an exported resource
    /// action.
    #[must_use]
    pub fn on_action(mut self, binding: impl crate::Bind<()>) -> Self {
        self.handler = Some(Handler::Binding(crate::Bind::bound(binding)));
        self.rebuild_engine();
        self
    }

    /// Run `f` when the action is pressed.
    #[must_use]
    pub fn on_click(mut self, f: impl FnMut() + 'static) -> Self {
        self.handler = Some(Handler::Press(Rc::new(RefCell::new(Box::new(f)))));
        self.rebuild_engine();
        self
    }

    /// A press that opens or shows something; the same as [`Self::on_click`].
    #[must_use]
    pub fn on_open(self, f: impl FnMut() + 'static) -> Self {
        self.on_click(f)
    }

    /// A quieter line under the action, when its label alone does not say enough: "Add the
    /// people you message in Contacts". Without a hint action it is a sentence; with one it is
    /// a route of its own to press.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.set_hint(Some(hint.into()));
        self
    }

    /// Replace the hint; `None` takes it away.
    pub fn set_hint(&mut self, hint: Option<String>) {
        if hint != self.hint {
            self.hint = hint;
            self.resolve();
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    /// Run the hint's route when the line is pressed or activated.
    #[must_use]
    pub fn on_hint_action(mut self, binding: impl crate::Bind<()>) -> Self {
        self.hint_handler = Some(Handler::Binding(crate::Bind::bound(binding)));
        self.rebuild_hint_engine();
        self
    }

    /// A hint press that opens or shows something.
    #[must_use]
    pub fn on_hint_open(mut self, f: impl FnMut() + 'static) -> Self {
        self.hint_handler = Some(Handler::Press(Rc::new(RefCell::new(Box::new(f)))));
        self.rebuild_hint_engine();
        self
    }

    /// The control id the action is addressed and tested by:
    /// [`Self::with_action_id`], or one derived from the title.
    pub fn action_control_id(&self) -> String {
        self.action_id
            .clone()
            .unwrap_or_else(|| format!("empty-state:{}", self.title))
    }

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
            Handler::Press(f) => button.on_click(move || (f.borrow_mut())()),
        });
    }

    fn rebuild_hint_engine(&mut self) {
        let (Some(handler), Some(label)) = (self.hint_handler.clone(), self.hint_resolved.clone())
        else {
            self.hint_engine = None;
            return;
        };
        let id = self.hint_control_id();
        let button = crate::widget::Button::new(&id, &label)
            .role(crate::widget::ButtonRole::Plain)
            .with_enabled(self.action_enabled);
        self.hint_engine = Some(match handler {
            Handler::Binding(binding) => button.on_action(binding),
            Handler::Press(f) => button.on_click(move || (f.borrow_mut())()),
        });
    }

    /// Whether the hint is a route of its own, not only a sentence about the action.
    fn hint_acts(&self) -> bool {
        self.hint_handler.is_some()
    }

    fn hint_control_id(&self) -> String {
        format!("{}:hint", self.action_control_id())
    }

    /// The rect the hint answers a press in (empty until laid out or without a hint).
    pub fn hint_rect(&self) -> Rect {
        self.hint_rect
    }

    fn fire_hint(&mut self, state: &mut tilo_ui_base::widget::Edits<'_>, path: &ObjId) {
        if let Some(engine) = &mut self.hint_engine {
            let _ = engine.perform_action(&tilo_ui_base::a11y::Action::Activate, &[], state, path);
        }
    }

    fn resolve(&mut self) {
        let resolved = (
            widget_text(&self.title),
            self.message.as_deref().map(|m| widget_text(m)),
            self.action_label.as_deref().map(|a| widget_text(a)),
        );
        let hint = self.hint.as_deref().map(|h| widget_text(h));
        if resolved != self.resolved || hint != self.hint_resolved {
            self.resolved = resolved;
            self.hint_resolved = hint;
            self.rebuild_hint_engine();
            self.layout_dirty = true;
            self.drawn = None;
        }
    }

    fn fire_action(&mut self, state: &mut tilo_ui_base::widget::Edits<'_>, path: &ObjId) {
        if let Some(engine) = &mut self.engine {
            let _ = engine.perform_action(&tilo_ui_base::a11y::Action::Activate, &[], state, path);
        }
    }

    /// The resolved bounds of the action (empty until laid out).
    /// Exposed so tests can assert the hit target without rendering to pixels.
    pub fn action_rect(&self) -> Rect {
        self.action_rect
    }

    /// Wrap this retained empty state in the toolkit's vertical viewport.
    /// The child keeps at least the viewport height for ordinary centering and
    /// grows to its measured content height when wrapped text needs more height,
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
        let metrics =
            StackStyle::resolved(theme, false).measure(text_renderer, width, &content, 0.0);
        metrics.title_h + action_label.map_or(0.0, |_| metrics.gap + metrics.btn_h)
    }

    /// Where [`Self::paint`] puts the stack in `rect`, without painting: its natural height
    /// at this width, the hero slot and the action's rect.
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
        self.place_controls();
    }

    /// Where [`Self::paint`] draws `content`'s action inside `rect`, for hit-testing it
    /// between paints; `None` with no action or no width.
    #[must_use]
    pub fn action_rect_in(
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        content: &EmptyStateContent<'_>,
    ) -> Option<Rect> {
        if rect.width() <= 0.0
            || rect.height() <= 0.0
            || content.action_label.is_none()
            || tilo_ui_base::workspace_sync::waiting()
                .is_some_and(|waiting| !waiting.keeps_action())
        {
            return None;
        }
        place(text, theme, rect, content, false).action
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
        Self::paint_with_documents(ctx, rect, content, hover).0
    }

    /// [`Self::paint`], and where it put each of `content.documents`: what a press on one
    /// opens.
    pub fn paint_with_documents(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
    ) -> (Option<Rect>, Vec<Rect>) {
        let painted = paint_empty_state(ctx, rect, content, hover, false, false, false, None);
        (painted.action, painted.documents)
    }

    /// Paint a state whose action has keyboard focus, using the shared focus ring.
    pub fn paint_focused(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
        focused: bool,
    ) -> Option<Rect> {
        Self::paint_focused_with_documents(ctx, rect, content, hover, focused).0
    }

    /// [`Self::paint_focused`], and where it put each of `content.documents`.
    pub fn paint_focused_with_documents(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        content: &EmptyStateContent<'_>,
        hover: Point,
        focused: bool,
    ) -> (Option<Rect>, Vec<Rect>) {
        let painted = paint_empty_state(ctx, rect, content, hover, focused, false, false, None);
        (painted.action, painted.documents)
    }

    /// Whether the action is the first of the accessibility node's children:
    /// it is, unless a workspace that has not synced says so in its place.
    fn lists_action(&self) -> bool {
        self.resolved.2.is_some()
            && tilo_ui_base::workspace_sync::waiting().is_none_or(|waiting| waiting.keeps_action())
    }

    fn content(&self) -> EmptyStateContent<'_> {
        EmptyStateContent {
            icon: if self.busy {
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
            quote: self.quote.as_deref(),
            documents: &[],
            action_label: self.resolved.2.as_deref(),
            hint: self.hint_resolved.as_deref(),
            hint_action: self.hint_handler.is_some(),
            error: self.error,
        }
    }
}

impl Widget for EmptyState {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    tilo_ui_base::provides!(
        dyn tilo_ui_base::widget::controls::ActionControls,
        dyn tilo_ui_base::widget::ResourceOperations
    );

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        let revision = tilo_ui_base::i18n::I18nManager::revision();
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
        self.layout_theme = theme.clone();
        self.drawn = None;
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
        let metrics = self.layout_style.measure(
            text_renderer,
            f64::from(measured_width),
            &self.content(),
            0.0,
        );
        let essential_height = metrics.title_h
            + self
                .action_label
                .as_ref()
                .map_or(0.0, |_| metrics.gap + metrics.btn_h);
        let details = self.layout_style.details_height(
            text_renderer,
            f64::from(measured_width),
            self.details.as_deref(),
            self.details_open,
        );
        let essential_height = essential_height + details;
        let natural = Fit::choose(f64::INFINITY, self.content().parts(), metrics).total + details;
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
        // Whole pixels up: the layout rounds the box to the pixel grid, and a box a fraction
        // shorter than the stack would shed the glyph that fits.
        let height = intrinsic_height.ceil() as f32;
        taffy::Size {
            width: measured_width,
            height,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        self.place_controls();
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.rect == Rect::ZERO
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn needs_animation_frame(&self) -> bool {
        self.spinner
            .as_ref()
            .is_some_and(Widget::needs_animation_frame)
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        // A host that draws this state in a theme of its own (a zoomed
        // editor) draws it at a text scale the layout never saw: adopt it, so
        // the controls move with the words they stand in.
        if (theme.text_scale - self.layout_theme.text_scale).abs() > f32::EPSILON {
            self.layout_theme = theme.clone();
            self.place_controls();
        }
        let rect = self.rect;
        let hover = self.last_cursor;
        let focused = self.action_focused;
        let inline = self.inline;
        let details_h = StackStyle::resolved(theme, inline).details_height(
            text_renderer,
            rect.width(),
            self.details.as_deref(),
            self.details_open,
        );
        let stack = Rect::new(
            rect.x0,
            rect.y0,
            rect.x1,
            (rect.y1 - details_h).max(rect.y0),
        );
        let mut spinner = self.spinner.take();
        let hint_focused = self.hint_focused;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        paint_empty_state(
            &mut ctx,
            stack,
            &self.content(),
            hover,
            focused,
            hint_focused,
            inline,
            spinner.as_mut(),
        );
        self.spinner = spinner;
        self.paint_details(&mut ctx);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.copy_hovered = self.copy_at(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.copy_at(self.last_cursor).is_some() => {
                if let Some(line) = self.copy_at(self.last_cursor) {
                    self.copy_line(line);
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.is_enabled()
                && self.action_label.is_some()
                && self.action_target().contains(self.last_cursor) =>
            {
                self.fire_action(&mut ctx.state, ctx.path);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.is_enabled()
                && self.hint_acts()
                && self.hint_rect.contains(self.last_cursor) =>
            {
                self.fire_hint(&mut ctx.state, ctx.path);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.details.is_some() && self.details_rect.contains(self.last_cursor) => {
                self.toggle_details();
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if (self.action_label.is_some() && self.action_target().contains(point))
            || (self.hint_acts() && self.hint_rect.contains(point))
            || (self.details.is_some() && self.details_rect.contains(point))
            || self.copy_at(point).is_some()
        {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        (self.action_label.is_some() && self.is_enabled())
            || (self.action_label.is_none() && self.details.is_some())
    }

    fn is_enabled(&self) -> bool {
        self.action_enabled && self.engine.as_ref().is_none_or(Widget::is_enabled)
    }

    fn set_focus(&mut self, focused: bool) {
        self.action_focused = focused && self.action_label.is_some();
        self.details_focused = focused && self.action_label.is_none() && self.details.is_some();
        if !focused {
            self.hint_focused = false;
        }
    }

    fn is_focused(&self) -> bool {
        self.action_focused || self.details_focused
    }

    fn needs_redraw(&self) -> bool {
        self.drawn != Some(self.painted_state())
            || self.layout_dirty
            || self.needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        self.drawn = Some(self.painted_state());
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        _key_path: &[u64],
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = tilo_ui_base::doc::document(state, path);
        if self.is_enabled()
            && self.action_label.is_some()
            && matches!(action, tilo_ui_base::a11y::Action::Activate)
        {
            self.fire_action(state, path);
            true
        } else if self.action_label.is_none()
            && self.details.is_some()
            && matches!(action, tilo_ui_base::a11y::Action::Activate)
        {
            self.toggle_details();
            true
        } else {
            false
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if self.details.is_some()
            && self.details_control_id() == target
            && matches!(action, tilo_ui_base::a11y::Action::Activate)
        {
            self.toggle_details();
            return true;
        }
        if matches!(action, tilo_ui_base::a11y::Action::Activate)
            && let Some(line) =
                (0..self.details_lines.len()).find(|&line| self.copy_control_id(line) == target)
        {
            return self.copy_line(line);
        }
        if self.hint_acts()
            && self.hint_control_id() == target
            && self.is_enabled()
            && matches!(action, tilo_ui_base::a11y::Action::Activate)
        {
            let (state, path) = tilo_ui_base::doc::document(state, path);
            self.fire_hint(state, path);
            return true;
        }
        self.action_label.is_some()
            && self.action_control_id() == target
            && self.perform_action(action, &[], state, path)
    }

    fn name(&self) -> &'static str {
        "EmptyState"
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        if self.input != environment {
            self.input = environment;
            self.drawn = None;
        }
    }

    fn test_id(&self) -> Option<&str> {
        match &self.action_label {
            Some(_) => self.action_id.as_deref(),
            None => self.id.as_deref(),
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The action is the state's own button, drawn inside it: that is what a press reaches, and
    /// where it says what the state does.
    fn pressable_child(&self) -> Option<usize> {
        self.lists_action().then_some(0)
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Politeness, a11y::Role};
        let waiting = tilo_ui_base::workspace_sync::waiting();
        let (title, message, busy) = waiting.as_ref().map_or(
            (
                &self.resolved.0,
                self.resolved.1.as_ref(),
                self.spinner.is_some(),
            ),
            |waiting| {
                (
                    &waiting.title,
                    waiting.message.as_ref(),
                    waiting.spinner_since.is_some(),
                )
            },
        );
        let mut node = AccessNode::new(Role::Group).with_name(title.clone());
        let said_hint = self.hint_resolved.as_deref().filter(|_| !self.hint_acts());
        if let Some(described) = described(
            message.map(String::as_str),
            said_hint,
            self.quote.as_deref(),
        ) {
            node = node.with_description(described);
        }
        if busy {
            node = node.with_state(AccessState::new().busy(true));
        }
        if self.error {
            node = node.with_live(Politeness::Assertive);
        } else if self.spinner.is_some() {
            node = node.with_live(Politeness::Polite);
        }
        let mut children = Vec::new();
        if let Some(action) = self.resolved.2.as_ref().filter(|_| {
            waiting
                .as_ref()
                .is_none_or(|waiting| waiting.keeps_action())
        }) {
            node = node.with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.action_focused)
                    .busy(self.spinner.is_some()),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(action.clone())
                    .with_bounds(self.action_target())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.action_focused)
                            .disabled(!self.is_enabled()),
                    )
                    .with_action_target(self.action_control_id()),
            );
        }
        if let Some(details) = &self.details {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(widget_text("empty-state-details"))
                    .with_description(details.clone())
                    .with_bounds(self.details_rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.details_focused)
                            .expanded(self.details_open),
                    )
                    .with_action_target(self.details_control_id()),
            );
            // Each line to type or paste, as a value to read exactly and copy.
            for (index, line) in self.details_lines.iter().enumerate() {
                if line.kind != details::Kind::Verbatim {
                    continue;
                }
                let copy = AccessNode::new(Role::Button)
                    .with_name(widget_text("widget-copy"))
                    .with_bounds(line.copy.unwrap_or(line.rect))
                    .with_state(AccessState::new().focusable(true))
                    .with_action_target(self.copy_control_id(index));
                children.push(
                    AccessNode::new(Role::TextInput)
                        .with_value(line.text.clone())
                        .with_bounds(line.rect)
                        .with_state(AccessState::new().read_only(true))
                        .with_children(vec![copy]),
                );
            }
        }
        if let Some(hint) = self.hint_resolved.as_deref().filter(|_| self.hint_acts()) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(hint.to_owned())
                    .with_bounds(self.hint_rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.hint_focused)
                            .disabled(!self.is_enabled()),
                    )
                    .with_action_target(self.hint_control_id()),
            );
        }
        if !children.is_empty() {
            node = node.with_children(children);
        }
        Some(node)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let mut index = 0;
        if self.lists_action() {
            if path == [index] {
                return Some(self.action_rect);
            }
            index += 1;
        }
        if self.details.is_some() {
            if path == [index] {
                return Some(self.details_rect);
            }
            index += 1;
        }
        if self.hint_acts() && path == [index] {
            return Some(self.hint_rect);
        }
        None
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let mut index = 0;
        if self.lists_action() {
            if path == [index] {
                self.action_focused = self.is_enabled();
                return self.action_focused;
            }
            index += 1;
        }
        if self.details.is_some() {
            if path == [index] {
                self.details_focused = true;
                return true;
            }
            index += 1;
        }
        if self.hint_acts() && path == [index] && self.is_enabled() {
            self.hint_focused = true;
            return true;
        }
        false
    }
}

impl tilo_ui_base::widget::ResourceOperations for EmptyState {
    fn start_operation(
        &mut self,
        op: &tilo_ui_base::widget::ResourceOperation<'_>,
        doc: &mut tilo_ui_base::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<tilo_ui_base::widget::ResourceOperationStart>, String> {
        match &mut self.engine {
            Some(engine) => tilo_ui_base::widget::start_operation(&mut *engine, op, doc, path),
            None => Ok(None),
        }
    }

    fn resource_operation_handlers(&self) -> std::collections::HashSet<String> {
        self.engine
            .as_ref()
            .map(tilo_ui_base::widget::resource_operation_handlers)
            .unwrap_or_default()
    }
}

impl tilo_ui_base::widget::controls::ActionControls for EmptyState {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        if let Some(engine) = &mut self.engine {
            tilo_ui_base::widget::controls::ActionControls::attach_controls(
                engine, domain, attachment,
            );
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.engine.as_mut().and_then(|engine| {
            tilo_ui_base::widget::controls::ActionControls::take_control_invocation(engine)
        })
    }
    fn control_action_bindings(&self) -> Vec<tilo_ui_base::widget::ControlBindingMetadata> {
        let mut bindings = if self.action_label.is_none() {
            Vec::new()
        } else if let Some(engine) = &self.engine {
            tilo_ui_base::widget::controls::ActionControls::control_action_bindings(engine)
        } else {
            vec![tilo_ui_base::widget::ControlBindingMetadata::semantic(
                self.action_control_id(),
            )]
        };
        // The "Details" disclosure only shows what the state already holds:
        // a view-local control, never an effect on the app's resources.
        if self.details.is_some() {
            bindings.push(tilo_ui_base::widget::ControlBindingMetadata::semantic(
                self.details_control_id(),
            ));
        }
        // So is copying a line of them.
        for (index, line) in self.details_lines.iter().enumerate() {
            if line.kind == details::Kind::Verbatim {
                bindings.push(tilo_ui_base::widget::ControlBindingMetadata::semantic(
                    self.copy_control_id(index),
                ));
            }
        }
        bindings
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(engine) = &mut self.engine {
            tilo_ui_base::widget::controls::ActionControls::refresh_control_action_availability(
                engine, allowed,
            );
        }
    }
}

#[cfg(test)]
mod tests;

mod component;
mod details;
pub use component::EmptyStateProps;

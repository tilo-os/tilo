//! `EmptyState` — the centered "there's nothing here yet" placeholder that
//! almost every list-shaped app hand-rolls (Inbox with no mail, a feed with
//! no articles, a contacts book with no contacts, …). Before this widget the
//! pattern was copy-pasted as a `Column` of a big icon + a heading `Text` +
//! a dim subtitle + an optional call-to-action `Button` across the fleet.
//!
//! `EmptyState` folds that into one theme-driven widget:
//!
//! - an optional hero [`Icon`] inside a soft tinted disc,
//! - a **title** line (the "No messages" headline),
//! - an optional **message** line (the dim one-liner underneath),
//! - an optional **action** pill (e.g. "Compose", "Add contact").
//!
//! The content is centered on both axes inside whatever space the parent
//! gives the widget, so dropping it as the sole child of a content pane
//! "just works". The action is surfaced through the toolkit's drain channel
//! ([`EmptyState::take_action`]) rather than a callback, matching the
//! CRDT-native input model used by [`super::ContextMenu`] /
//! [`super::TriStateControl`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// `kurbo::Affine` composition is plain f64 matrix math with no panic
/// path; `arithmetic_side_effects` flags the `*` operator overload anyway.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "kurbo::Affine * Affine composes two 2D transforms via f64 \
              matrix math; it never panics or overflows, unlike integer +/-/*"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// Diameter of the hero-icon disc.
const DISC_D: f64 = 72.0;
/// Icon glyph size drawn inside the disc.
const ICON_D: f64 = 34.0;

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
    gap: f64,
    btn_h: f64,
}

/// Which elements actually fit, and how tall the result is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    icon: bool,
    message: bool,
    total: f64,
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
                t += DISC_D + m.gap;
            }
            if message {
                t += m.gap * 0.5 + m.msg_h;
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

/// Shared layout + paint for the empty state, centered inside `rect`. Used by
/// both [`EmptyState`]'s [`Widget::draw`] and the public immediate-mode
/// [`EmptyState::paint`] entry point. Returns the action pill's rect (for
/// hit-testing) when `action_label` is `Some`, else `None`. `hover` is the
/// pointer position for the pill hover state (pass an off-screen point to
/// suppress hover).
#[allow(clippy::too_many_arguments)]
fn paint_empty_state(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    icon: Option<Icon>,
    title: &str,
    message: Option<&str>,
    action_label: Option<&str>,
    hover: Point,
) -> Option<Rect> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let gap = f64::from(theme.tokens.spacing.s4);
    let title_size = theme.tokens.typography.scale.xl;
    let msg_size = theme.tokens.typography.scale.sm;
    let btn_size = theme.tokens.typography.scale.sm;
    let btn_h = 34.0_f64;

    let (title_w, title_h) = text_renderer.measure(title, title_size, None);
    let (msg_w, msg_h) = message.map_or((0.0, 0.0), |m| text_renderer.measure(m, msg_size, None));
    let (act_w, _) = action_label.map_or((0.0, 0.0), |a| text_renderer.measure(a, btn_size, None));

    let fit = Fit::choose(
        rect.height(),
        Parts {
            icon: icon.is_some(),
            message: message.is_some(),
            action: action_label.is_some(),
        },
        Metrics {
            title_h: f64::from(title_h),
            msg_h: f64::from(msg_h),
            gap,
            btn_h,
        },
    );
    let icon = if fit.icon { icon } else { None };
    let message = if fit.message { message } else { None };
    let total = fit.total;

    let cx = rect.center().x;
    let mut y = rect.center().y - total / 2.0;

    if let Some(icon) = icon {
        let disc_center = Point::new(cx, y + DISC_D / 2.0);
        let disc = Circle::new(disc_center, DISC_D / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.accent_primary.with_alpha_factor(0.14),
            None,
            &disc,
        );
        let icon_rect = Rect::from_center_size(disc_center, (ICON_D, ICON_D));
        icon.draw_with_stroke(scene, transform, icon_rect, theme.accent_primary, 1.75);
        y += DISC_D + gap;
    }

    text_renderer.draw_text(
        scene,
        title,
        title_size,
        theme.text_primary,
        translated(transform, cx - f64::from(title_w) / 2.0, y),
        None,
    );
    y += f64::from(title_h);

    if let Some(msg) = message {
        y += gap * 0.5;
        text_renderer.draw_text(
            scene,
            msg,
            msg_size,
            theme.text_dim,
            translated(transform, cx - f64::from(msg_w) / 2.0, y),
            None,
        );
        y += f64::from(msg_h);
    }

    if let Some(label) = action_label {
        y += gap;
        let pad_x = f64::from(theme.tokens.spacing.s4);
        let bw = f64::from(act_w) + pad_x * 2.0;
        let x0 = cx - bw / 2.0;
        let action_rect = Rect::new(x0, y, x0 + bw, y + btn_h);
        let hovered = action_rect.contains(hover);
        let bg = if hovered {
            theme.accent_hover
        } else {
            theme.accent_primary
        };
        let pill = RoundedRect::from_rect(action_rect, f64::from(theme.tokens.radii.md));
        scene.fill(Fill::NonZero, transform, bg, None, &pill);
        let ty = action_rect.y0 + (btn_h - f64::from(btn_size)) / 2.0;
        text_renderer.draw_text(
            scene,
            label,
            btn_size,
            Color::WHITE,
            translated(transform, cx - f64::from(act_w) / 2.0, ty),
            None,
        );
        Some(action_rect)
    } else {
        None
    }
}

/// A centered empty-state placeholder: hero icon + title + message + action.
pub struct EmptyState {
    icon: Option<Icon>,
    title: String,
    message: Option<String>,
    action_label: Option<String>,
    /// Drained flag set when the action pill is clicked.
    action_fired: bool,
    rect: Rect,
    action_rect: Rect,
    last_cursor: Point,
}

impl EmptyState {
    /// Construct with just a title (the minimal case). Chain
    /// [`with_icon`](Self::with_icon) / [`with_message`](Self::with_message) /
    /// [`with_action`](Self::with_action) for the fuller treatment.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            icon: None,
            title: title.into(),
            message: None,
            action_label: None,
            action_fired: false,
            rect: Rect::ZERO,
            action_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
        }
    }

    /// Add a hero icon rendered inside a soft tinted disc above the title.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Add the dim one-line message under the title.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Add an accent call-to-action pill under the message. The label is a
    /// pre-resolved display string (callers do the `i18n!` lookup).
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action_label = Some(label.into());
        self
    }

    /// Drain the "action pill was clicked" signal (fires once per click). The
    /// host reacts by mutating its own model — e.g. opening a compose sheet.
    pub fn take_action(&mut self) -> bool {
        std::mem::take(&mut self.action_fired)
    }

    /// The resolved bounds of the action pill (empty until laid out + drawn).
    /// Exposed so tests can assert the hit target without rendering to pixels.
    pub fn action_rect(&self) -> Rect {
        self.action_rect
    }

    /// Immediate-mode paint of an empty state centered inside `rect`, for the
    /// many apps that render straight to a `Scene` instead of building a
    /// widget tree. Returns the action pill's rect when `action_label` is
    /// `Some` so the caller can hit-test it against its own pointer state
    /// (pass `hover` for the hover tint, or an off-screen point to suppress).
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        icon: Option<Icon>,
        title: &str,
        message: Option<&str>,
        action_label: Option<&str>,
        hover: Point,
    ) -> Option<Rect> {
        paint_empty_state(
            scene,
            transform,
            text_renderer,
            theme,
            rect,
            icon,
            title,
            message,
            action_label,
            hover,
        )
    }
}

impl Widget for EmptyState {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Fill the space the parent offers so the content can center inside it.
        let node = taffy.leaf_node(Style {
            flex_grow: 1.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            min_size: taffy::Size {
                width: percent(0.0_f32),
                height: percent(0.0_f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // A sensible intrinsic block so the widget is visible even in an
        // auto-sized slot.
        taffy::Size {
            width: 240.0,
            height: 220.0,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.action_rect = paint_empty_state(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            self.icon,
            &self.title,
            self.message.as_deref(),
            self.action_label.as_deref(),
            self.last_cursor,
        )
        .unwrap_or(Rect::ZERO);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.action_label.is_some() && self.action_rect.contains(self.last_cursor) => {
                self.action_fired = true;
                *handled = true;
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

    fn name(&self) -> &'static str {
        "EmptyState"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        // The empty state is a labelled group; the title is its accessible
        // name and the message its description so a screen reader announces
        // both without the caller wiring anything.
        let mut node = AccessNode::new(Role::Group).with_name(self.title.clone());
        if let Some(msg) = &self.message {
            node = node.with_description(msg.clone());
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn synth() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        handled
    }

    /// Representative numbers: the dark theme's `xl` title and `sm` message at
    /// the spacing scale a docked panel is drawn with.
    fn metrics() -> Metrics {
        Metrics {
            title_h: 24.0,
            msg_h: 18.0,
            gap: 16.0,
            btn_h: 34.0,
        }
    }

    /// A zero-state in a short pane (a docked panel body, a couple hundred
    /// pixels tall) must shed elements rather than push the message through
    /// the bottom edge.
    #[test]
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
    #[test]
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

    #[test]
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
            dispatch(&mut es, &press()),
            "click on the action pill is consumed"
        );
        assert!(es.take_action(), "the action signal fires");
        assert!(!es.take_action(), "the action signal drains after one read");
    }

    #[test]
    fn click_outside_action_does_not_fire() {
        let mut es = EmptyState::new("Nothing here").with_action("Add");
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);
        // Top-left corner is well outside the centered pill.
        dispatch(&mut es, &cursor(Point::new(2.0, 2.0)));
        assert!(
            !dispatch(&mut es, &press()),
            "click away from the pill is not consumed"
        );
        assert!(!es.take_action());
    }

    #[test]
    fn without_action_paints_no_pill_and_has_no_hit_target() {
        let mut es = EmptyState::new("Empty").with_icon(Icon::Folder);
        lay_out(&mut es, 600.0, 400.0);
        let _ = draw(&mut es);
        assert_eq!(es.action_rect(), Rect::ZERO, "no action ⇒ no pill rect");
        // A press anywhere is ignored when there's no action.
        let center = es.get_rect().center();
        dispatch(&mut es, &cursor(center));
        assert!(!dispatch(&mut es, &press()));
        assert!(!es.take_action());
    }

    #[test]
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

    #[test]
    fn immediate_mode_paint_returns_action_rect() {
        // The canvas-app entry point paints into an arbitrary rect and hands
        // back the pill hit target, matching what Widget::draw resolves.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let rect = Rect::new(0.0, 0.0, 500.0, 300.0);
        let action = EmptyState::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            Some(Icon::Inbox),
            "No mail",
            Some("You're all caught up"),
            Some("Compose"),
            Point::new(-1.0, -1.0),
        );
        let r = action.expect("action label ⇒ a returned pill rect");
        assert!(rect.contains(r.center()), "pill sits inside the paint rect");
        // No action label ⇒ no returned rect.
        let none = EmptyState::paint(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            rect,
            None,
            "Empty",
            None,
            None,
            Point::new(-1.0, -1.0),
        );
        assert!(none.is_none());
    }

    #[test]
    fn accessibility_is_a_named_group() {
        use crate::a11y::Role;
        let es = EmptyState::new("No results").with_message("Try another search");
        let node = Widget::accessibility(&es).expect("empty state exposes a11y");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("No results"));
        assert_eq!(node.description.as_deref(), Some("Try another search"));
    }
}

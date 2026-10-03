//! Escape or an outside click dismisses; clicks inside `primary_rect`/
//! `secondary_rect` fire the wired action.
//!
//! State lives outside the CRDT — modal callbacks are host-supplied closures
//! and do not flow through Automerge, mirroring how `permission_prompt`
//! exposes its result via `pending_choice`.

use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::platform::{DialogActionOrder, PlatformConventions};
use crate::presence::PresenceCtx;
use crate::text_metrics::centered_text_top;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::{Metrics, Spacing};
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::button::{Button, ButtonFace, ButtonRole};
use crate::widget::modal_frame::MODAL_SCRIM_ALPHA;

pub const MAX_DIALOG_WIDTH: f64 = 480.0;
pub const MAX_DIALOG_HEIGHT: f64 = 320.0;
/// Wide enough for short labels without truncating; matches
/// `permission_prompt`'s button width.
const ACTION_BUTTON_WIDTH: f64 = 110.0;

/// The dialog's chrome geometry, resolved from the theme once per layout pass
/// (and cached, because `recompute_layout` also runs from builder methods
/// that carry no theme). Each is a token expression, not a number: the title
/// bar is the heading's token size (`scale.lg`) plus `spacing.s4` above and
/// below — 48 at 1×, and taller at a large text setting so the bar is sized
/// for the heading it hosts; the footer is the shared `Button` height
/// (`metrics.bar_height`) plus an `spacing.s2` inset above and below (56 at
/// 1×), with the buttons `s4` in from the trailing edge and `s2` apart.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DialogChrome {
    title_bar: f64,
    footer: f64,
    button: f64,
    /// Inset of the button group from the card's trailing edge (`s4`).
    footer_inset_x: f64,
    /// Gap between the two footer buttons (`s2`).
    button_gap: f64,
}

impl DialogChrome {
    fn from_theme(theme: &Theme) -> Self {
        let t = &theme.tokens;
        Self::from_parts(
            t.typography.size(TextRole::Heading),
            t.spacing.s4,
            t.metrics.bar_height,
            t.spacing.s2,
        )
    }

    fn from_parts(heading: f32, title_pad: f32, button: f32, footer_pad: f32) -> Self {
        Self {
            title_bar: f64::from(heading) + 2.0 * f64::from(title_pad),
            footer: f64::from(button) + 2.0 * f64::from(footer_pad),
            button: f64::from(button),
            footer_inset_x: f64::from(title_pad),
            button_gap: f64::from(footer_pad),
        }
    }
}

impl Default for DialogChrome {
    /// The unscaled reference values, for a dialog laid out before it has
    /// seen a theme (builder methods, immediate-mode hosts).
    fn default() -> Self {
        Self::from_parts(
            crate::tokens::typography::stock().size(TextRole::Heading),
            Spacing::BASE.s4,
            Metrics::BASE.bar_height,
            Spacing::BASE.s2,
        )
    }
}

/// One footer-bar action button; boxed `FnMut` so the dialog can invoke it
/// from `handle_event` without taking ownership.
pub struct DialogAction {
    pub label: String,
    pub on_click: Box<dyn FnMut()>,
}

impl std::fmt::Debug for DialogAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogAction")
            .field("label", &self.label)
            .field("on_click", &"<callback>")
            .finish()
    }
}

/// Generic over the content type so callers can pass any `Widget` without
/// boxing — same pattern the file picker uses.
pub struct ModalDialog<C: Widget> {
    action_identity: uuid::Uuid,
    test_id: Option<String>,
    title: String,
    content: C,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    on_dismiss: Option<Box<dyn FnMut()>>,
    open: bool,
    destructive: bool,
    viewport: Rect,
    /// Box max width / height; override via [`with_max_size`](Self::with_max_size)
    /// when the default 480×320 reads as unbalanced for the content.
    max_w: f64,
    max_h: f64,
    /// Resolved dialog rect — recomputed each `apply_layout` and on
    /// [`with_viewport`].
    rect: Rect,
    /// Hit-test rect for the primary footer button; `Rect::ZERO` until laid out.
    primary_rect: Rect,
    /// Hit-test rect for the secondary footer button; `Rect::ZERO` until laid out.
    secondary_rect: Rect,
    /// Tracked here so `MouseInput` (which carries no position) can map
    /// to a button rect — same trick `permission_prompt` uses.
    last_cursor: Point,
    /// Whether `last_cursor` is current: cleared by `CursorLeft` (no
    /// `CursorMoved` follows one) and on open, so a footer button's hover
    /// face never outlives the pointer that caused it.
    pointer_inside: bool,
    /// Isolated taffy tree for the inner content, rebuilt by
    /// [`Self::place_content`] and kept alive (rather than dropped at the
    /// end of layout) so `handle_event` can forward events into the content
    /// with a valid node — what lets an embedded `TextInput` focus and take
    /// the caret on click. `None` until the first content layout.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
    platform: PlatformConventions,
    /// Chrome heights from the latest theme this dialog was laid out with.
    chrome: DialogChrome,
    /// The theme from the latest layout build. The content is laid out in
    /// its own taffy tree from `apply_layout`, which carries no theme, so it
    /// must measure with the theme the host actually uses rather than a
    /// default one — otherwise a light or large-text dialog lays its body
    /// out at dark-mode 1× metrics and paints it at the real ones.
    layout_theme: Theme,
}

impl<C: Widget> ModalDialog<C> {
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
            action_identity: uuid::Uuid::new_v4(),
            test_id: None,
            title: title.into(),
            content,
            primary_action: None,
            secondary_action: None,
            on_dismiss: None,
            open: false,
            destructive: false,
            viewport: Rect::ZERO,
            max_w: MAX_DIALOG_WIDTH,
            max_h: MAX_DIALOG_HEIGHT,
            rect: Rect::ZERO,
            primary_rect: Rect::ZERO,
            secondary_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            pointer_inside: false,
            content_taffy: None,
            content_node: None,
            platform: PlatformConventions::default(),
            chrome: DialogChrome::default(),
            layout_theme: Theme::dark(),
        }
    }

    /// Adopt `theme`'s chrome geometry, re-centring the box when it moved.
    /// Returns whether anything changed, so a caller with a renderer can
    /// re-place the content under the new title bar.
    fn refresh_chrome(&mut self, theme: &Theme) -> bool {
        let chrome = DialogChrome::from_theme(theme);
        if chrome == self.chrome {
            return false;
        }
        self.chrome = chrome;
        self.recompute_layout();
        true
    }

    /// Retain `theme` for the content layout that `apply_layout` runs
    /// without one, then [`Self::refresh_chrome`]. Layout passes only —
    /// paint refreshes the chrome and leaves the (allocating) clone alone.
    fn adopt_theme(&mut self, theme: &Theme) {
        self.layout_theme = theme.clone();
        self.refresh_chrome(theme);
    }

    /// A stable address for inspection, headless scenarios and `tilo-control`.
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// When closed, `draw` and `handle_event` are no-ops — the host can keep
    /// the widget in the tree at no cost while it's off-screen.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.set_open(open);
        self
    }

    /// Imperative show/hide after construction; also toggles whether the
    /// dialog [traps focus](Widget::traps_focus).
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
        // A closed dialog receives no moves, so whatever position it last
        // saw is stale by the time it reopens.
        self.pointer_inside = false;
    }

    /// The callback fires on a left-click inside `primary_rect`.
    #[must_use]
    pub fn primary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.primary_action = Some(DialogAction {
            label: label.into(),
            on_click: Box::new(on_click),
        });
        self
    }

    /// Same shape as `primary_action` but renders with the muted
    /// (raised-surface) treatment.
    #[must_use]
    pub fn secondary_action(
        mut self,
        label: impl Into<String>,
        on_click: impl FnMut() + 'static,
    ) -> Self {
        self.secondary_action = Some(DialogAction {
            label: label.into(),
            on_click: Box::new(on_click),
        });
        self
    }

    /// Fires on Escape (pressed) and on a backdrop click outside the box.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Paints the primary button as a warning (`semantic.danger`) rather
    /// than the ordinary accent — for delete/kill/erase confirmations.
    #[must_use]
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.destructive = on;
        self
    }

    /// Overrides the box's max width/height in logical px (still clamped to
    /// the viewport) and re-centers it immediately.
    #[must_use]
    pub fn with_max_size(mut self, max_w: f64, max_h: f64) -> Self {
        self.max_w = max_w.max(1.0);
        self.max_h = max_h.max(1.0);
        self.recompute_layout();
        self
    }

    /// Updates the dialog's size cap after construction. Detached hosts use
    /// this when their responsive dialog width depends on the live viewport.
    pub fn set_max_size(&mut self, max_w: f64, max_h: f64) {
        self.max_w = max_w.max(1.0);
        self.max_h = max_h.max(1.0);
        self.recompute_layout();
    }

    pub fn content(&self) -> &C {
        &self.content
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// Lay out a dialog embedded by an immediate-mode host that does not mount
    /// it in the host's retained Taffy tree. The dialog still lays out its own
    /// content normally; only the full-viewport wrapper is supplied directly.
    pub fn layout_in(&mut self, viewport: Rect, theme: &Theme, text_renderer: &mut TextRenderer) {
        self.layout_theme = theme.clone();
        self.viewport = viewport;
        self.adopt_theme(theme);
        self.recompute_layout();
        self.place_content(theme, text_renderer);
    }

    /// Sets the viewport rect outside a taffy pass and re-centers the box
    /// immediately, so a host painting the dialog as an absolutely-positioned
    /// overlay doesn't need to spin up a taffy tree just to update geometry.
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.viewport = viewport;
        self.recompute_layout();
        self
    }

    fn recompute_layout(&mut self) {
        if self.viewport.width() <= 0.0 || self.viewport.height() <= 0.0 {
            return;
        }
        // Clamped to the viewport as a safety net for tiny tablet chrome.
        let w = self.max_w.min(self.viewport.width());
        let h = self.max_h.min(self.viewport.height());
        let cx = self.viewport.x0 + (self.viewport.width() - w) / 2.0;
        let cy = self.viewport.y0 + (self.viewport.height() - h) / 2.0;
        self.rect = Rect::new(cx, cy, cx + w, cy + h);

        // Keep the group right-aligned while following the host platform's
        // semantic primary/secondary order.
        let chrome = self.chrome;
        let footer_top = self.rect.y1 - chrome.footer;
        let button_y0 = footer_top + (chrome.footer - chrome.button) / 2.0;
        let button_y1 = button_y0 + chrome.button;
        let right_edge = self.rect.x1 - chrome.footer_inset_x;
        let primary_x1 = right_edge;
        let primary_x0 = primary_x1 - ACTION_BUTTON_WIDTH;
        let secondary_x1 = primary_x0 - chrome.button_gap;
        let secondary_x0 = secondary_x1 - ACTION_BUTTON_WIDTH;
        let right = Rect::new(primary_x0, button_y0, primary_x1, button_y1);
        let left = Rect::new(secondary_x0, button_y0, secondary_x1, button_y1);
        match self.platform.dialog_action_order() {
            DialogActionOrder::PrimaryLast => {
                self.primary_rect = right;
                self.secondary_rect = left;
            }
            DialogActionOrder::PrimaryFirst => {
                self.primary_rect = left;
                self.secondary_rect = right;
            }
        }
    }

    /// Lays out the inner content between the title bar and the footer.
    /// Same rationale as `PopoverWidget::place_content` — the modal's outer
    /// taffy node is the viewport, not the body, so without this step the
    /// content never gets a non-zero rect and paints at scene origin.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let pad = f64::from(theme.tokens.spacing.s4);
        let chrome = self.chrome;
        let inner = Rect::new(
            self.rect.x0 + pad,
            self.rect.y0 + chrome.title_bar,
            (self.rect.x1 - pad).max(self.rect.x0 + pad),
            (self.rect.y1 - chrome.footer).max(self.rect.y0 + chrome.title_bar),
        );
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        let mut inner_taffy = TaffyTree::<()>::new();
        let mut inner_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let content_node = self
            .content
            .build_layout(&mut inner_taffy, theme, &mut inner_map);
        // `inner` is a dialog-box sub-rect, capped by MAX_DIALOG_WIDTH/HEIGHT
        // (or a caller's `with_max_size`) and the viewport — both well
        // within f32's range for on-screen layout math.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "dialog geometry is bounded screen-pixel size, never near f32's range limit"
        )]
        let (inner_w, inner_h) = (inner.width() as f32, inner.height() as f32);
        let _ = inner_taffy.compute_layout_with_measure(
            content_node,
            Size {
                width: taffy::AvailableSpace::Definite(inner_w),
                height: taffy::AvailableSpace::Definite(inner_h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = inner_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        );
        self.content
            .apply_layout(&inner_taffy, content_node, Point::new(inner.x0, inner.y0));
        self.content_taffy = Some(inner_taffy);
        self.content_node = Some(content_node);
    }

    /// No-op until the first content layout has run. Returns whether the
    /// content consumed the event; the dialog still blocks propagation at
    /// its own level, but uses this result before invoking a default action.
    /// `focus_target` IS threaded so an embedded focusable can request focus.
    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return false;
        };
        let mut inner_handled = false;
        self.content.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut inner_handled,
                focus_target,
                node_id: node,
                taffy,
            },
        );
        inner_handled
    }

    pub fn dialog_rect(&self) -> Rect {
        self.rect
    }
    pub fn primary_rect(&self) -> Rect {
        self.primary_rect
    }
    pub fn secondary_rect(&self) -> Rect {
        self.secondary_rect
    }
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// Local to this module rather than reusing `permission_prompt::draw_button`,
/// which has a different disabled/hover visual treatment this doesn't need.
// Standard immediate-mode paint-helper signature for this vello renderer
// (scene + transform + text_renderer + theme, plus this draw's own
// content); every parameter is independently load-bearing.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
)]
impl<C: Widget + 'static> ModalDialog<C> {
    /// Route an event when an immediate-mode host embeds this dialog outside
    /// its retained widget tree. This is the event-side counterpart to
    /// [`Self::layout_in`] and removes the need for every app to manufacture a
    /// throwaway Taffy leaf and focus target.
    pub fn handle_detached(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut taffy = TaffyTree::new();
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return true;
        };
        let mut handled = false;
        let mut focus_target = None;
        self.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }
}

impl<C: Widget + 'static> Widget for ModalDialog<C> {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.content.set_platform_conventions(conventions);
        self.recompute_layout();
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // The dialog holds no CRDT-backed state itself; forward so embedded
        // content widgets still get their sync pass.
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt_theme(theme);
        // The body is registered as a child node so its focusable widgets
        // join the host's keyboard focus order (and the focus trap), even
        // though `place_content` re-lays it out separately for drawing.
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[content_node],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        // The resolved node rect: the screen-sized parent under a stack
        // overlay, or the window itself when wired directly under `App`.
        self.viewport = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.recompute_layout();
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(self.layout_theme.text_scale, |tr| {
            self.place_content(&theme, tr)
        });
    }

    #[allow(
        clippy::arithmetic_side_effects,
        reason = "the `transform * Affine::translate(..)` call below composes 2D transforms via kurbo's float `Mul` impl, which cannot overflow, wrap, or panic like integer arithmetic"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        // A theme swap between layout and paint (appearance reconcile, a
        // detached host that never laid out) must not paint the chrome at
        // stale heights — nor the body under a title bar that just grew.
        if self.refresh_chrome(theme) {
            self.place_content(theme, text_renderer);
        }

        // The one modal scrim: the same painter and strength `ModalFrame`
        // uses, so a retained dialog and an immediate-mode sheet darken the
        // desktop identically instead of one flat literal-black veil.
        crate::chrome::fill_scrim(scene, transform, self.viewport, MODAL_SCRIM_ALPHA, theme);

        // Panel role (HIG §9.1): panel ground with the derived panel edge
        // as a crisp 1 px inset stroke, not a declared `border_standard`
        // that vanished against `raised` on the dark preset.
        let radius = f64::from(theme.tokens.radii.control);
        let box_shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &box_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        let title_style = theme.text(TextRole::Heading);
        let title_x = self.rect.x0 + f64::from(theme.tokens.spacing.s4);
        // Centre the title on its measured line box, not the font size.
        let (_, title_h) = text_renderer.measure_style(&self.title, None, &title_style);
        let title_y = centered_text_top(self.rect.y0, self.chrome.title_bar, f64::from(title_h));
        text_renderer.draw_style(
            scene,
            &self.title,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((title_x, title_y)),
            None,
            &title_style,
        );

        self.content
            .draw(scene, transform, text_renderer, theme, presence);

        // Skipped when no action is wired (e.g. an info-only dialog with
        // just the Escape-to-dismiss affordance). A dialog button *is* a
        // `Button`: the shared face brings the hover state, the disabled
        // and focus treatment and the contrast-checked ink that a local
        // painter had to re-derive (HIG §8: never hand-roll a control).
        let hovered = |rect: Rect| self.pointer_inside && rect.contains(self.last_cursor);
        if let Some(action) = &self.primary_action {
            // A destructive dialog is the confirmation step, the one place
            // a destructive button is filled.
            let face = ButtonFace::new(self.primary_rect, &action.label);
            let face = if self.destructive {
                face.role(ButtonRole::Destructive).confirming()
            } else {
                face.role(ButtonRole::Primary)
            }
            .hovered(hovered(self.primary_rect));
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
        if let Some(action) = &self.secondary_action {
            let face = ButtonFace::new(self.secondary_rect, &action.label)
                .role(ButtonRole::Secondary)
                .hovered(hovered(self.secondary_rect));
            Button::paint(scene, transform, text_renderer, theme, &face);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        // Blocking: while open, marks every event handled itself so it
        // stays modal even hosted directly (tests, headless) without the
        // App's `modal_capture` cooperation.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.pointer_inside = true;
                // A TextInput only takes a click while hovered, and this
                // CursorMoved is what arms it.
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
            }
            // No `CursorMoved` follows the pointer out of the window, so the
            // footer buttons' hover has to be dropped here.
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if let Some(cb) = self.on_dismiss.as_mut() {
                    cb();
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                let content_handled = self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                if !content_handled && let Some(action) = self.primary_action.as_mut() {
                    (action.on_click)();
                }
                *ctx.handled = true;
            }
            // Text arrives from an input method (and in a browser page only
            // from one), not only as key presses.
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                // Button rects sit INSIDE the dialog rect, so check first.
                if self.primary_action.is_some() && self.primary_rect.contains(p) {
                    if let Some(action) = self.primary_action.as_mut() {
                        (action.on_click)();
                    }
                } else if self.secondary_action.is_some() && self.secondary_rect.contains(p) {
                    if let Some(action) = self.secondary_action.as_mut() {
                        (action.on_click)();
                    }
                } else if self.rect.contains(p) {
                    self.forward_to_content(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        ctx.focus_target,
                    );
                } else if let Some(cb) = self.on_dismiss.as_mut() {
                    cb();
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput { .. } => {
                // Still forward in-box presses so e.g. a right-click context
                // affordance in the content can react.
                if self.rect.contains(self.last_cursor) {
                    self.forward_to_content(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        ctx.focus_target,
                    );
                }
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ModalDialog"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.open {
            vec![&self.content]
        } else {
            Vec::new()
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open {
            visitor(&self.content);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open {
            vec![&mut self.content]
        } else {
            Vec::new()
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn traps_focus(&self) -> bool {
        self.open
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Title is the accessible name (FTL-resolved). A closed dialog is
        // hidden from the tree.
        let title = crate::i18n!(&self.title);
        let name = (!title.is_empty()).then_some(title);
        let mut node = AccessNode::new(Role::Dialog)
            .with_optional_name(name)
            .with_state(AccessState {
                modal: true,
                hidden: !self.open,
                ..Default::default()
            });
        if self.open {
            for (key, action, bounds) in [
                ("primary", &self.primary_action, self.primary_rect),
                ("secondary", &self.secondary_action, self.secondary_rect),
            ] {
                if let Some(action) = action {
                    node.children.push(
                        AccessNode::new(Role::Button)
                            .with_name(crate::i18n!(&action.label))
                            .with_key(if key == "primary" { 1 } else { 2 })
                            .with_state(AccessState::new().focusable(true))
                            .with_bounds(bounds)
                            .with_action_target(format!("dialog:{}:{key}", self.action_identity)),
                    );
                }
            }
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut AppModel,
        _: &ModelPath,
    ) -> bool {
        if !self.open || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let callback = if target == "modal.primary"
            || target == format!("dialog:{}:primary", self.action_identity)
        {
            self.primary_action.as_mut()
        } else if target == "modal.secondary"
            || target == format!("dialog:{}:secondary", self.action_identity)
        {
            self.secondary_action.as_mut()
        } else {
            None
        };
        if let Some(callback) = callback {
            (callback.on_click)();
            true
        } else {
            false
        }
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.accessibility()?
            .children
            .get(*index)?
            .bounds
            .map(super::super::a11y::AccessBounds::to_rect)
    }
}

#[cfg(test)]
mod tests {
    //! `TestContent` stands in for an arbitrary `C: Widget` so the generic
    //! surface is exercised end-to-end without depending on another
    //! widget's behaviour.
    use super::*;
    use crate::widget::Text;
    use std::cell::Cell;
    use std::rc::Rc;
    use vello_encoding::DrawTag;
    use winit::event::{DeviceId, KeyEvent, MouseButton};
    use winit::keyboard::{KeyLocation, PhysicalKey};

    /// Fixed empty rect, no paint, no events — a generic content
    /// placeholder so `ModalDialog<C>` compiles in tests.
    struct TestContent;

    impl Widget for TestContent {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style::default());
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {}
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn name(&self) -> &'static str {
            "TestContent"
        }
        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }
    }

    /// Counts how many events it was forwarded, so the modal-trap
    /// forwarding contract can be asserted. Sizes itself to a real rect
    /// so it lands inside the dialog box.
    #[derive(Default)]
    struct CountingContent {
        events: std::rc::Rc<std::cell::Cell<usize>>,
        rect: Rect,
        consume: bool,
    }

    impl Widget for CountingContent {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(40.0),
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
            taffy::Size {
                width: 200.0,
                height: 40.0,
            }
        }
        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
            self.rect = Rect::new(origin.x, origin.y, origin.x + 200.0, origin.y + 40.0);
        }
        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }
        fn handle_event(&mut self, _event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            self.events.set(self.events.get().saturating_add(1));
            *ctx.handled = self.consume;
        }
        fn name(&self) -> &'static str {
            "CountingContent"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// SAFETY: opaque `DeviceId` whose all-zero bit pattern is tolerated
    /// by winit — same trick used in `permission_prompt::tests` and in
    /// `crates/session/shell/tests/e2e_shell_flows.rs::key_press`.
    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Built over a zeroed buffer since winit's `KeyEventExtra` field is
    /// `pub(crate)` and can't be struct-literalled; all public fields
    /// tolerate all-zero, so we overwrite just the bits the dialog inspects.
    fn key_press_event(named: NamedKey) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(named);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch<C: Widget + 'static>(d: &mut ModalDialog<C>, event: &WindowEvent) {
        let _ = dispatch_handled(d, event);
    }

    /// Same as [`dispatch`] but returns whether the dialog marked the event
    /// handled — needed to assert the modal-blocking contract.
    fn dispatch_handled<C: Widget + 'static>(d: &mut ModalDialog<C>, event: &WindowEvent) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        d.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
        handled
    }

    /// Used to assert that a non-Escape key while open is swallowed.
    fn char_key_event() -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Tab);
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// Through the real five-pass path so `content_taffy` / `content_node`
    /// are populated and in-box forwarding works.
    fn laid_out_counting(
        open: bool,
    ) -> (
        ModalDialog<CountingContent>,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let events = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let content = CountingContent {
            events: events.clone(),
            rect: Rect::ZERO,
            consume: false,
        };
        let mut d = ModalDialog::new("Title", content).open(open);
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = d.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        d.apply_layout(&taffy, node, Point::ZERO);
        (d, events)
    }

    #[test]
    fn dialog_accessibility_exposes_open_content_and_real_actions() {
        fn semantic_footer_uses_current_dialog_visibility_and_existing_actions() {
            use automerge::transaction::Transactable;
            let fired = Rc::new(Cell::new(0));
            let callback = fired.clone();
            let mut dialog = ModalDialog::new("Review", TestContent)
                .open(true)
                .primary_action("Restore", move || callback.set(callback.get() + 1));
            let target = dialog.accessibility().unwrap().children[0]
                .action_target
                .clone()
                .unwrap();
            let mut doc = AutoCommit::new();
            assert!(dialog.perform_semantic_action(
                &target,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(fired.get(), 1);
            dialog.set_open(false);
            assert!(dialog.accessibility().unwrap().children.is_empty());
            assert!(!dialog.perform_semantic_action(
                &target,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            dialog.set_open(true);
            dialog.primary_action = None; // Unavailable footer action is absent in all input paths.
            assert!(!dialog.perform_semantic_action(
                &target,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            let callback = fired.clone();
            let mut replacement = ModalDialog::new("Review", TestContent)
                .open(true)
                .primary_action("Restore", move || callback.set(callback.get() + 1));
            assert!(!replacement.perform_semantic_action(
                &target,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            let fresh = replacement.accessibility().unwrap().children[0]
                .action_target
                .clone()
                .unwrap();
            assert!(replacement.perform_semantic_action(
                &fresh,
                &crate::a11y::Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(fired.get(), 2);
            assert_eq!(doc.pending_ops(), 0);
        }

        fn accessibility_footer_actions_reach_the_real_callbacks() {
            use crate::a11y::Action;
            let clicked = Rc::new(Cell::new(0));
            let observed = clicked.clone();
            let mut dialog = ModalDialog::new("Confirm", TestContent)
                .open(true)
                .primary_action("Apply", move || observed.set(observed.get() + 1))
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            let node = dialog.accessibility().unwrap();
            assert!(
                node.children
                    .iter()
                    .any(|child| child.name.as_deref() == Some("Apply")),
                "the painted primary action must be accessible"
            );
            let mut doc = AutoCommit::new();
            assert!(dialog.perform_semantic_action(
                "modal.primary",
                &Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(clicked.get(), 1);
            dialog.set_open(false);
            assert!(!dialog.perform_semantic_action(
                "modal.primary",
                &Action::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert_eq!(
                clicked.get(),
                1,
                "closed controls cannot activate stale callbacks"
            );
        }

        fn accessibility_reports_dialog_role_modal_and_title() {
            use crate::a11y::Role;
            let d = ModalDialog::new("Confirm delete", TestContent).open(true);
            let node = Widget::accessibility(&d).expect("dialog is not presentational");
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Confirm delete"));
            assert!(node.state.modal, "a backdrop dialog is modal");
            assert!(!node.state.hidden, "an open dialog is visible");
        }

        fn accessibility_closed_dialog_is_hidden() {
            let d = ModalDialog::new("Confirm delete", TestContent).open(false);
            let node = Widget::accessibility(&d).unwrap();
            assert!(node.state.hidden, "a closed dialog is hidden from the tree");
        }

        fn children_exposes_dialog_content() {
            let d = ModalDialog::new("Title", TestContent).open(true);
            assert_eq!(
                Widget::children(&d).len(),
                1,
                "the dialog body must be reachable as a child for the a11y tree",
            );
        }

        fn closed_dialog_prunes_content_from_the_production_accessibility_tree() {
            use crate::accesskit_bridge::build_tree_update;
            use crate::widget::Button;
            use accesskit::Role as AccessKitRole;

            let mut d = ModalDialog::new(
                "Confirm",
                Button::new("modal-child", "Confirm action", None),
            )
            .open(true);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            d.build_layout(&mut taffy, &Theme::dark(), &mut node_map);

            let open = build_tree_update(&d, &node_map, None, 1.0);
            assert!(
                open.nodes
                    .iter()
                    .any(|(_, node)| node.role() == AccessKitRole::Button),
                "test precondition: open dialog exports its content button",
            );

            d.set_open(false);
            let closed = build_tree_update(&d, &node_map, None, 1.0);
            assert!(
                closed
                    .nodes
                    .iter()
                    .all(|(_, node)| node.role() != AccessKitRole::Button),
                "a closed dialog must not leave content controls in the production accessibility tree",
            );
            assert!(
                Widget::children_mut(&mut d).is_empty(),
                "closed modal content must also be pruned from mutable propagation",
            );
        }

        semantic_footer_uses_current_dialog_visibility_and_existing_actions();
        accessibility_footer_actions_reach_the_real_callbacks();
        accessibility_reports_dialog_role_modal_and_title();
        accessibility_closed_dialog_is_hidden();
        children_exposes_dialog_content();
        closed_dialog_prunes_content_from_the_production_accessibility_tree();
    }

    #[test]
    fn detached_and_retained_layout_place_dialog_content() {
        fn detached_layout_places_dialog_and_content() {
            let content = CountingContent::default();
            let mut dialog = ModalDialog::new("Detached", content).open(true);
            dialog.layout_in(
                Rect::new(20.0, 30.0, 820.0, 630.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );

            assert_eq!(dialog.get_rect(), Rect::new(180.0, 170.0, 660.0, 490.0));
            assert_ne!(dialog.content().get_rect(), Rect::ZERO);

            dialog.set_max_size(400.0, 200.0);
            dialog.layout_in(
                Rect::new(20.0, 30.0, 820.0, 630.0),
                &Theme::dark(),
                &mut TextRenderer::new(),
            );
            assert_eq!(dialog.get_rect(), Rect::new(220.0, 230.0, 620.0, 430.0));
        }

        fn retained_layout_preserves_large_text_before_the_first_draw() {
            let mut dialog = ModalDialog::new(
                "Large text",
                Text::new("Scaled dialog content").with_role(crate::tokens::TextRole::Title),
            )
            .open(true);
            let theme = Theme::dark().with_text_scale(2.0);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = dialog.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(800.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            dialog.apply_layout(&taffy, node, Point::ZERO);

            assert!(
                dialog.content().get_rect().height() >= 49.0,
                "dialog content must reserve its 200% line height before paint, got {:?}",
                dialog.content().get_rect()
            );
        }

        fn detached_event_path_preserves_modal_dismissal() {
            let dismissed = Rc::new(Cell::new(false));
            let flag = Rc::clone(&dismissed);
            let mut dialog = ModalDialog::new("Detached", TestContent)
                .on_dismiss(move || flag.set(true))
                .open(true);

            assert!(dialog.handle_detached(
                &key_press_event(NamedKey::Escape),
                1.0,
                Modifiers::default(),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert!(dismissed.get());
        }

        /// The title bar is the heading line plus `s4` above and below (48 at
        /// 1×) and grows with the text scale instead of clipping a large
        /// heading; the footer is the shared `Button` height plus `s2` (56 at
        /// 1×) and follows `ui_scale` like every other bar.
        fn chrome_heights_follow_the_theme() {
            let base = DialogChrome::default();
            assert_eq!(base, DialogChrome::from_theme(&Theme::dark()));
            assert!((base.title_bar - 48.0).abs() < 1e-6, "{base:?}");
            assert!((base.footer - 56.0).abs() < 1e-6, "{base:?}");
            assert!((base.button - 40.0).abs() < 1e-6, "{base:?}");

            let large = DialogChrome::from_theme(&Theme::dark().with_text_scale(2.0));
            assert!(
                large.title_bar
                    >= base.title_bar
                        + f64::from(crate::tokens::typography::stock().size(TextRole::Heading))
                        - 1e-6,
                "a 2× heading gets a taller bar: {large:?}"
            );
            assert!(
                (large.footer - base.footer).abs() < 1e-6,
                "text scale does not move the footer: {large:?}"
            );

            let ui = DialogChrome::from_theme(&Theme::dark().with_ui_scale(2.0));
            assert!((ui.footer - 112.0).abs() < 1e-6, "{ui:?}");
            assert!((ui.button - 80.0).abs() < 1e-6, "{ui:?}");
        }

        /// The body is placed under the title bar the theme resolves, with the
        /// theme the host laid out with — not a default dark 1× one — so a 200 %
        /// heading never paints over the content.
        fn content_starts_below_the_title_bar_at_a_large_text_scale() {
            let theme = Theme::dark().with_text_scale(2.0);
            let content = CountingContent::default();
            let mut d = ModalDialog::new("Title", content).open(true);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = d.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(800.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                )
                .unwrap();
            d.apply_layout(&taffy, node, Point::ZERO);
            let chrome = DialogChrome::from_theme(&theme);
            assert_eq!(d.chrome, chrome, "the dialog adopted the layout theme");
            let content_top = d.content().get_rect().y0;
            assert!(
                (content_top - (d.rect.y0 + chrome.title_bar)).abs() < 0.5,
                "content top {content_top} vs title bar bottom {}",
                d.rect.y0 + chrome.title_bar,
            );
            assert!(
                d.primary_rect == Rect::ZERO || d.primary_rect.height() == chrome.button,
                "footer buttons are one Button tall"
            );
        }

        detached_layout_places_dialog_and_content();
        retained_layout_preserves_large_text_before_the_first_draw();
        detached_event_path_preserves_modal_dismissal();
        chrome_heights_follow_the_theme();
        content_starts_below_the_title_bar_at_a_large_text_scale();
    }

    fn color_fill_count<C: Widget + 'static>(d: &mut ModalDialog<C>) -> usize {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        d.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn dialog_dismisses_and_invokes_primary_by_key_and_pointer() {
        fn modal_dialog_dismiss_on_esc() {
            let dismissed = Rc::new(Cell::new(false));
            let dismissed_clone = Rc::clone(&dismissed);
            let mut d = ModalDialog::new("Title", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                .on_dismiss(move || dismissed_clone.set(true));
            dispatch(&mut d, &key_press_event(NamedKey::Escape));
            assert!(
                dismissed.get(),
                "Escape on an open dialog must invoke the on_dismiss callback",
            );
        }

        fn modal_dialog_backdrop_click_dismisses() {
            let dismissed = Rc::new(Cell::new(false));
            let dismissed_clone = Rc::clone(&dismissed);
            let mut d = ModalDialog::new("Title", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                .on_dismiss(move || dismissed_clone.set(true));
            // Well outside the centered 480×320 dialog rect but inside the
            // 800×600 backdrop.
            let click_point = Point::new(10.0, 10.0);
            assert!(
                !d.dialog_rect().contains(click_point),
                "test geometry sanity: (10,10) must be outside the dialog rect",
            );
            dispatch(&mut d, &cursor_at(click_point));
            dispatch(&mut d, &left_press());
            assert!(
                dismissed.get(),
                "backdrop click outside the dialog box must invoke on_dismiss",
            );
        }

        fn modal_dialog_primary_action_invokes_callback() {
            let fired = Rc::new(Cell::new(false));
            let fired_clone = Rc::clone(&fired);
            let mut d = ModalDialog::new("Title", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                .primary_action("OK", move || fired_clone.set(true));
            // Drive a real `apply_layout` pass so `primary_rect` is populated
            // through the five-pass path (not just `with_viewport`).
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let theme = Theme::dark();
            let node = d.build_layout(&mut taffy, &theme, &mut node_map);
            let avail = taffy::Size {
                width: taffy::AvailableSpace::Definite(800.0),
                height: taffy::AvailableSpace::Definite(600.0),
            };
            taffy.compute_layout(node, avail).unwrap();
            d.apply_layout(&taffy, node, Point::ZERO);
            let click_point = d.primary_rect().center();
            assert!(
                d.primary_rect().contains(click_point),
                "test sanity: rect.center() must lie inside primary_rect",
            );
            dispatch(&mut d, &cursor_at(click_point));
            dispatch(&mut d, &left_press());
            assert!(
                fired.get(),
                "click inside primary_rect must invoke the primary action callback",
            );
        }

        fn modal_dialog_enter_invokes_primary_unless_content_consumes_it() {
            let fired = Rc::new(Cell::new(0usize));
            let fired_clone = Rc::clone(&fired);
            let (mut d, _) = laid_out_counting(true);
            d.primary_action = Some(DialogAction {
                label: "Save".into(),
                on_click: Box::new(move || fired_clone.set(fired_clone.get() + 1)),
            });

            dispatch(&mut d, &key_press_event(NamedKey::Enter));
            assert_eq!(fired.get(), 1, "Enter invokes the semantic primary action");

            d.content_mut().consume = true;
            dispatch(&mut d, &key_press_event(NamedKey::Enter));
            assert_eq!(
                fired.get(),
                1,
                "an editor that consumes Enter suppresses the dialog default"
            );
        }

        fn modal_dialog_action_order_follows_platform_conventions() {
            use crate::platform::{Platform, PlatformConventions};

            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut dialog = ModalDialog::new("Title", TestContent).with_viewport(viewport);
            dialog.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
            assert!(dialog.primary_rect().x0 > dialog.secondary_rect().x0);

            dialog.set_platform_conventions(PlatformConventions::new(Platform::Windows));
            assert!(dialog.primary_rect().x0 < dialog.secondary_rect().x0);
        }

        modal_dialog_dismiss_on_esc();
        modal_dialog_backdrop_click_dismisses();
        modal_dialog_primary_action_invokes_callback();
        modal_dialog_enter_invokes_primary_unless_content_consumes_it();
        modal_dialog_action_order_follows_platform_conventions();
    }

    #[test]
    fn dialog_paints_backdrop_destructive_and_hover_faces() {
        /// Asserted via the Vello encoding directly so this doesn't depend on
        /// pixel readback.
        fn modal_dialog_paints_backdrop_when_open() {
            let mut d = ModalDialog::<TestContent>::new("Title", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            let open_fills = color_fill_count(&mut d);
            assert!(
                open_fills >= 1,
                "an open dialog must emit at least one COLOR fill (the backdrop); got {open_fills}",
            );

            // Guards against a future refactor that paints the backdrop
            // unconditionally.
            let mut closed = ModalDialog::<TestContent>::new("Title", TestContent)
                .open(false)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            let closed_fills = color_fill_count(&mut closed);
            assert_eq!(
                closed_fills, 0,
                "a closed dialog must emit zero COLOR fills; got {closed_fills}",
            );
        }

        /// Drives the real draw path for two variants identical but for
        /// `with_destructive`, so only the primary button's fill colour can
        /// account for a diff in the encoded fill data.
        fn destructive_primary_paints_a_different_fill_than_accent() {
            fn draw_data(destructive: bool) -> Vec<u32> {
                let mut d = ModalDialog::new("Delete?", TestContent)
                    .open(true)
                    .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                    .with_destructive(destructive)
                    .primary_action("Delete", || {});
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let theme = Theme::dark();
                let node = d.build_layout(&mut taffy, &theme, &mut node_map);
                taffy
                    .compute_layout(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(800.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                    )
                    .unwrap();
                d.apply_layout(&taffy, node, Point::ZERO);
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                d.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
                scene.encoding().draw_data.clone()
            }
            assert_ne!(
                draw_data(false),
                draw_data(true),
                "a destructive primary must paint a different colour than the accent primary",
            );
        }

        /// A dialog button is a `Button`: the pointer resting on it repaints
        /// the face (HIG §7 — hover must be visible), and the pointer leaving
        /// the window takes the face back to rest.
        fn primary_action_paints_hover_when_the_pointer_rests_on_it() {
            fn draw_data(hover: bool, then_leave: bool) -> Vec<u32> {
                let mut d = ModalDialog::new("Save?", TestContent)
                    .open(true)
                    .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                    .primary_action("Save", || {});
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let theme = Theme::dark();
                let node = d.build_layout(&mut taffy, &theme, &mut node_map);
                taffy
                    .compute_layout(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(800.0),
                            height: taffy::AvailableSpace::Definite(600.0),
                        },
                    )
                    .unwrap();
                d.apply_layout(&taffy, node, Point::ZERO);
                if hover {
                    let centre = d.primary_rect.center();
                    dispatch(&mut d, &cursor_at(centre));
                }
                if then_leave {
                    dispatch(
                        &mut d,
                        &WindowEvent::CursorLeft {
                            device_id: synth_device(),
                        },
                    );
                }
                let mut tr = TextRenderer::new();
                let mut scene = Scene::new();
                d.draw(
                    &mut scene,
                    Affine::IDENTITY,
                    &mut tr,
                    &theme,
                    &PresenceCtx::default(),
                );
                scene.encoding().draw_data.clone()
            }
            let rest = draw_data(false, false);
            assert_ne!(
                draw_data(true, false),
                rest,
                "the hovered primary action paints the same as one at rest",
            );
            assert_eq!(
                draw_data(true, true),
                rest,
                "the pointer left the window but the button still paints hovered",
            );
        }

        modal_dialog_paints_backdrop_when_open();
        destructive_primary_paints_a_different_fill_than_accent();
        primary_action_paints_hover_when_the_pointer_rests_on_it();
    }

    #[test]
    fn open_dialog_captures_input_and_closed_dialog_is_inert() {
        /// An OPEN dialog reports its box rect via `modal_capture`; a closed
        /// one reports `None`. This is the signal the App uses to suppress its
        /// own focus shortcuts and swallow click-through.
        fn modal_capture_reflects_open_state() {
            let mut d = ModalDialog::new("T", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            assert!(
                Widget::modal_capture(&d).is_some(),
                "an open dialog must report a modal-capture rect"
            );
            // Mutate to closed via a fresh instance (open() consumes self).
            d = ModalDialog::new("T", TestContent)
                .open(false)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            assert!(
                Widget::modal_capture(&d).is_none(),
                "a closed dialog must NOT report modal capture"
            );
        }

        /// Every keyboard event while open is consumed (Tab included) so it
        /// never reaches the host's focus-cycling shortcut.
        fn open_dialog_swallows_all_keyboard_events() {
            let (mut d, _events) = laid_out_counting(true);
            let handled = dispatch_handled(&mut d, &char_key_event());
            assert!(handled, "an open dialog must consume every keyboard event");
        }

        /// A press OUTSIDE the box is consumed (swallowed) AND dismisses; it
        /// must not leak to widgets behind the backdrop.
        fn open_dialog_swallows_outside_press_and_dismisses() {
            let dismissed = Rc::new(Cell::new(false));
            let dc = Rc::clone(&dismissed);
            let mut d = ModalDialog::new("T", TestContent)
                .open(true)
                .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
                .on_dismiss(move || dc.set(true));
            dispatch(&mut d, &cursor_at(Point::new(5.0, 5.0)));
            let handled = dispatch_handled(&mut d, &left_press());
            assert!(handled, "outside press must be swallowed (handled=true)");
            assert!(dismissed.get(), "outside press must also dismiss");
        }

        /// A press INSIDE the box (not on an action button) is consumed AND
        /// forwarded to the content so an embedded field can focus/click.
        fn open_dialog_forwards_inbox_press_to_content() {
            let (mut d, events) = laid_out_counting(true);
            let center = d.dialog_rect().center();
            // Arm hover, then press inside the box.
            dispatch(&mut d, &cursor_at(center));
            let before = events.get();
            let handled = dispatch_handled(&mut d, &left_press());
            assert!(handled, "in-box press must be consumed (modal-blocking)");
            assert!(
                events.get() > before,
                "in-box press must be forwarded to the content widget so its \
             fields can focus/click (got {} forwarded events)",
                events.get()
            );
        }

        /// A closed dialog ignores everything (no forwarding, not handled).
        fn closed_dialog_is_inert() {
            let (mut d, events) = laid_out_counting(false);
            let handled = dispatch_handled(&mut d, &left_press());
            assert!(!handled, "a closed dialog must not consume events");
            assert_eq!(events.get(), 0, "a closed dialog must not forward events");
        }

        modal_capture_reflects_open_state();
        open_dialog_swallows_all_keyboard_events();
        open_dialog_swallows_outside_press_and_dismisses();
        open_dialog_forwards_inbox_press_to_content();
        closed_dialog_is_inert();
    }
}

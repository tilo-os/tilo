//! Modal dialog widget — a centered, focus-grabbing panel painted on top
//! of a dimmed backdrop covering the full viewport.
//!
//! Layout / paint contract (matches the brief for Unit 7):
//!  - Backdrop: full-viewport rect at `Color::BLACK.with_alpha_factor(0.7)`.
//!  - Box: centered on viewport, max 480 × 320 px. `surfaces.raised` fill,
//!    `border_standard` stroke, `radii.md` corners.
//!  - Title bar: `text_primary` text at `typography.scale.lg`, left-padded
//!    by `spacing.s4`.
//!  - Footer: action buttons aligned to the right with `spacing.s2` gap.
//!    Primary button uses the accent token + white text, secondary uses
//!    `surfaces.raised` + `text_primary`, both at `radii.sm`.
//!
//! Event contract:
//!  - Escape (pressed) ⇒ `on_dismiss`.
//!  - Left-click outside the dialog box but inside the backdrop ⇒ `on_dismiss`.
//!  - Left-click inside `primary_rect` ⇒ `primary_action.on_click`.
//!  - Left-click inside `secondary_rect` ⇒ `secondary_action.on_click`.
//!
//! State lives outside the CRDT — modal callbacks are host-supplied closures
//! and do not flow through Automerge. This mirrors how `permission_prompt`
//! exposes its result via `pending_choice`: the host owns the mutation.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Max dialog box width in logical pixels — the brief pins this at 480.
pub const MAX_DIALOG_WIDTH: f64 = 480.0;
/// Max dialog box height in logical pixels — the brief pins this at 320.
pub const MAX_DIALOG_HEIGHT: f64 = 320.0;
/// Title-bar height. Sized to fit `typography.scale.lg` (16 px) plus the
/// vertical padding implied by `spacing.s4` (16 px) — 48 logical pixels
/// keeps the title centered with breathing room above and below.
const TITLE_BAR_HEIGHT: f64 = 48.0;
/// Footer-bar height, sized to fit the action buttons with `spacing.s2`
/// padding above/below. 56 logical pixels gives a 40-pixel button height
/// (matching `Button`) plus 8-px top/bottom inset.
const FOOTER_HEIGHT: f64 = 56.0;
/// Default action button width — wide enough for short labels like
/// "Cancel" / "OK" without truncating, narrow enough that two fit
/// comfortably in the footer of a 320-wide dialog. Matches the
/// `permission_prompt` button width pattern.
const ACTION_BUTTON_WIDTH: f64 = 110.0;
/// Default action button height — matches `Button::build_layout`'s
/// fixed 40-pixel height so the visual rhythm carries across widgets.
const ACTION_BUTTON_HEIGHT: f64 = 40.0;

/// One footer-bar action button. The callback is a boxed `FnMut` so the
/// dialog can invoke it from `handle_event` without taking ownership;
/// the host supplies it via [`ModalDialog::primary_action`] /
/// [`ModalDialog::secondary_action`].
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

/// Modal dialog widget. Generic over the content type so callers can pass
/// any `Widget` without boxing — same generic pattern the file picker uses
/// in its body slot.
pub struct ModalDialog<C: Widget> {
    title: String,
    content: C,
    primary_action: Option<DialogAction>,
    secondary_action: Option<DialogAction>,
    on_dismiss: Option<Box<dyn FnMut()>>,
    open: bool,
    /// When true the primary button paints with the `semantic.danger` token
    /// instead of the accent — for destructive confirmations (delete / kill /
    /// erase) where the affirmative action needs a warning colour. Purely
    /// visual; the callback wiring is unchanged.
    destructive: bool,
    viewport: Rect,
    /// Max box width / height in logical px. Default to the brief's
    /// [`MAX_DIALOG_WIDTH`]×[`MAX_DIALOG_HEIGHT`]; a host can override via
    /// [`with_max_size`](Self::with_max_size) when its content wants a wider or
    /// shorter box than the 480×320 default (e.g. a single labelled field reads
    /// as unbalanced in a 320-tall box).
    max_w: f64,
    max_h: f64,
    /// Resolved dialog rect — recomputed each `apply_layout` (and on the
    /// fly when the viewport changes via [`with_viewport`]).
    rect: Rect,
    /// Hit-test rect for the primary footer button. `Rect::ZERO` until
    /// the first layout pass populates it.
    primary_rect: Rect,
    /// Hit-test rect for the secondary footer button. `Rect::ZERO` until
    /// the first layout pass populates it.
    secondary_rect: Rect,
    /// Tracked here so `MouseInput` (which carries no position) can map
    /// to a button rect — same trick `permission_prompt` uses.
    last_cursor: Point,
    /// Isolated taffy tree for the inner content, rebuilt by
    /// [`Self::place_content`]. Kept alive (rather than dropped at the end
    /// of layout) so `handle_event` can forward pointer/keyboard events
    /// into the content widget with a valid node — this is what lets an
    /// embedded `TextInput` actually focus + take the caret on a click
    /// inside the dialog box. `None` until the first content layout.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
}

impl<C: Widget> ModalDialog<C> {
    /// Construct a closed modal dialog with the given title + content.
    /// Callers wire actions via the builder methods below and flip
    /// `open` true once the dialog should be displayed.
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
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
            content_taffy: None,
            content_node: None,
        }
    }

    /// Toggle the dialog open / closed. When closed, `draw` and
    /// `handle_event` are no-ops — the host can keep the widget in the
    /// tree without paying any cost when it's not on-screen.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Imperatively show or hide the dialog after construction. Flipping this
    /// toggles whether the dialog [traps focus](Widget::traps_focus), so the
    /// host can open it in response to a click and close it without rebuilding
    /// the widget tree.
    pub fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    /// Attach a primary action. The label drives the button text; the
    /// callback fires on a left-click inside `primary_rect`.
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

    /// Attach a secondary action. Same shape as [`primary_action`] but
    /// renders with the muted (raised-surface) treatment.
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

    /// Set the dismiss callback. Fires on Escape (pressed) and on
    /// a backdrop click outside the dialog box.
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Mark the primary action as destructive so its button paints with the
    /// `semantic.danger` token instead of the accent. Use for confirmations
    /// that delete / kill / erase — the affirmative button then reads as a
    /// warning rather than an ordinary accented action. Chainable.
    pub fn with_destructive(mut self, on: bool) -> Self {
        self.destructive = on;
        self
    }

    /// Override the box's maximum width / height in logical px (still clamped
    /// to the viewport). Use it when the default 480×320 box reads as
    /// unbalanced for the content — e.g. a single labelled field wants a wider,
    /// shorter box. The box is re-centered immediately.
    pub fn with_max_size(mut self, max_w: f64, max_h: f64) -> Self {
        self.max_w = max_w.max(1.0);
        self.max_h = max_h.max(1.0);
        self.recompute_layout();
        self
    }

    /// Borrow the inner content widget (e.g. to read an embedded field's value
    /// on submit). Pairs with [`content_mut`](Self::content_mut).
    pub fn content(&self) -> &C {
        &self.content
    }

    /// Mutably borrow the inner content widget (e.g. to focus an embedded field
    /// when the dialog opens, or reset it between uses).
    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    /// Override the viewport rect outside a taffy pass. The dialog box is
    /// re-centered immediately so tests / hosts that paint the dialog as
    /// an absolutely-positioned overlay don't have to spin up a taffy
    /// tree just to update geometry.
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.viewport = viewport;
        self.recompute_layout();
        self
    }

    /// Recompute `rect`, `primary_rect`, `secondary_rect` from the
    /// current viewport. Called from `with_viewport` and `apply_layout`.
    fn recompute_layout(&mut self) {
        if self.viewport.width() <= 0.0 || self.viewport.height() <= 0.0 {
            return;
        }
        // Dialog size: capped at the brief's 480×320 maximum, but never
        // larger than the viewport. Real-world usage gives the dialog
        // its requested max in almost every case (viewports are sized
        // for ≥720p windows); the clamp is a safety net for tiny tablet
        // chrome.
        let w = self.max_w.min(self.viewport.width());
        let h = self.max_h.min(self.viewport.height());
        let cx = self.viewport.x0 + (self.viewport.width() - w) / 2.0;
        let cy = self.viewport.y0 + (self.viewport.height() - h) / 2.0;
        self.rect = Rect::new(cx, cy, cx + w, cy + h);

        // Footer button rects — right-aligned with `spacing.s2` (8 px)
        // gap between primary and secondary. Primary sits rightmost
        // (Western reading order: "OK" lands on the right, "Cancel" to
        // the left).
        let footer_top = self.rect.y1 - FOOTER_HEIGHT;
        let button_y0 = footer_top + (FOOTER_HEIGHT - ACTION_BUTTON_HEIGHT) / 2.0;
        let button_y1 = button_y0 + ACTION_BUTTON_HEIGHT;
        let right_edge = self.rect.x1 - 16.0; // `spacing.s4` right inset
        let primary_x1 = right_edge;
        let primary_x0 = primary_x1 - ACTION_BUTTON_WIDTH;
        let secondary_x1 = primary_x0 - 8.0; // `spacing.s2` gap
        let secondary_x0 = secondary_x1 - ACTION_BUTTON_WIDTH;
        self.primary_rect = Rect::new(primary_x0, button_y0, primary_x1, button_y1);
        self.secondary_rect = Rect::new(secondary_x0, button_y0, secondary_x1, button_y1);
    }

    /// Lay out the inner content widget inside the dialog box, between
    /// the title bar and the footer. Same rationale as
    /// `PopoverWidget::place_content` — the modal's outer taffy node is
    /// the viewport, not the body, so the content widget never gets a
    /// non-zero rect from the parent's taffy compute. Without this step
    /// the body content (e.g. a confirm-message text widget) paints at
    /// scene origin instead of inside the dialog box.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let pad = theme.tokens.spacing.s4 as f64;
        let inner = Rect::new(
            self.rect.x0 + pad,
            self.rect.y0 + TITLE_BAR_HEIGHT,
            (self.rect.x1 - pad).max(self.rect.x0 + pad),
            (self.rect.y1 - FOOTER_HEIGHT).max(self.rect.y0 + TITLE_BAR_HEIGHT),
        );
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        let mut inner_taffy = TaffyTree::<()>::new();
        let mut inner_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let content_node = self.content.build_layout(&mut inner_taffy, theme, &mut inner_map);
        let _ = inner_taffy.compute_layout_with_measure(
            content_node,
            Size {
                width: taffy::AvailableSpace::Definite(inner.width() as f32),
                height: taffy::AvailableSpace::Definite(inner.height() as f32),
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
        // Retain the inner tree so `handle_event` can route pointer +
        // keyboard events into the content with a live node — required for
        // embedded fields to focus/click.
        self.content_taffy = Some(inner_taffy);
        self.content_node = Some(content_node);
    }

    /// Forward an event into the inner content widget using the retained
    /// inner taffy tree. No-op until the first content layout has run.
    /// `handled` is intentionally NOT threaded out — the dialog decides
    /// consumption at its own level (it's always modal-blocking); the
    /// content only needs the event delivered so its internal state
    /// (focus, caret, hover) updates. `focus_target` IS threaded so an
    /// embedded focusable can request focus.
    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return;
        };
        let mut inner_handled = false;
        self.content.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            &mut inner_handled,
            focus_target,
            node,
            taffy,
        );
    }

    /// Test/host accessor for the resolved dialog box rect.
    pub fn dialog_rect(&self) -> Rect { self.rect }
    /// Test/host accessor for the primary button rect.
    pub fn primary_rect(&self) -> Rect { self.primary_rect }
    /// Test/host accessor for the secondary button rect.
    pub fn secondary_rect(&self) -> Rect { self.secondary_rect }
    /// Whether the dialog is currently open.
    pub fn is_open(&self) -> bool { self.open }
}

/// Draw a single footer button — local to this module so we don't pull in
/// `permission_prompt::draw_button` (it has a different visual treatment
/// for disabled / hover state that the modal dialog doesn't need).
fn draw_action_button(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    is_primary: bool,
    destructive: bool,
) {
    let radius = theme.tokens.radii.sm as f64;
    let shape = RoundedRect::from_rect(rect, radius);
    let bg = if is_primary {
        if destructive {
            theme.tokens.colors.semantic.danger
        } else {
            theme.tokens.colors.accent.stops[crate::tokens::colors::ACCENT_PRIMARY_IDX]
        }
    } else {
        theme.tokens.colors.surfaces.raised
    };
    scene.fill(Fill::NonZero, transform, bg, None, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        theme.tokens.colors.border_standard,
        None,
        &shape,
    );
    let text_color = if is_primary {
        // The fill is a theme token, so whether white survives it is a question
        // about the palette in force, not a constant.
        theme.tokens.colors.ink_on_any(
            &[bg],
            Color::WHITE,
            crate::tokens::colors::ON_FILL_MIN_CONTRAST,
        )
    } else {
        theme.tokens.colors.text_primary
    };
    let text_size = theme.tokens.typography.scale.sm;
    let (text_w, _) = text_renderer.measure(label, text_size, None);
    let text_x = rect.center().x - (text_w as f64 / 2.0);
    let text_y = rect.y0 + (rect.height() - text_size as f64) / 2.0;
    text_renderer.draw_text(
        scene,
        label,
        text_size,
        text_color,
        transform * Affine::translate((text_x, text_y)),
        None,
    );
}

impl<C: Widget + 'static> Widget for ModalDialog<C> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // The dialog itself holds no CRDT-backed state — its `open`
        // flag, callbacks and viewport are host-supplied. Forward to
        // the content so embedded widgets (text inputs, checkboxes,
        // etc.) still get their sync pass.
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The dialog overlays the entire viewport so it can paint the
        // backdrop everywhere outside the box. The body content is registered
        // as a child node so its focusable widgets join the host's keyboard
        // focus order (and, when the dialog is open, the focus trap) — even
        // though `place_content` re-lays-out the body itself for drawing.
        let content_node = self.content.build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[content_node],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        // Use the resolved node rect as the viewport — when the dialog is
        // hosted under a stack overlay this is the screen-sized parent;
        // when wired directly under `App` it's the window itself.
        self.viewport = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        self.recompute_layout();
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        self.place_content(&theme, &mut tr);
    }

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

        // 1. Backdrop — fill the full viewport at 70% black.
        let backdrop = Color::BLACK.with_alpha_factor(0.7);
        scene.fill(Fill::NonZero, transform, backdrop, None, &self.viewport);

        // 2. Dialog box — raised surface with the standard border + medium radius.
        let radius = theme.tokens.radii.md as f64;
        let box_shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &box_shape,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &box_shape,
        );

        // 3. Title bar — text only, left-padded by `spacing.s4`. The bar
        //    itself has no separator fill; the box stroke is enough to
        //    delineate the header zone.
        let title_size = theme.tokens.typography.scale.lg;
        let title_x = self.rect.x0 + theme.tokens.spacing.s4 as f64;
        let title_y = self.rect.y0 + (TITLE_BAR_HEIGHT - title_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.title,
            title_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((title_x, title_y)),
            None,
        );

        // 3b. Inner content — laid out by `place_content` between the title
        //     bar and the footer. Drawn on top of the box fill so embedded
        //     fields/messages are visible. (Before this the content was
        //     measured + positioned but never painted.)
        self.content
            .draw(scene, transform, text_renderer, theme, presence);

        // 4. Footer actions — right-aligned. Skip rendering when no
        //    action is wired (e.g. an info-only dialog with just the
        //    Escape-to-dismiss affordance).
        if let Some(action) = &self.primary_action {
            draw_action_button(
                scene,
                transform,
                text_renderer,
                theme,
                self.primary_rect,
                &action.label,
                true,
                self.destructive,
            );
        }
        if let Some(action) = &self.secondary_action {
            draw_action_button(
                scene,
                transform,
                text_renderer,
                theme,
                self.secondary_rect,
                &action.label,
                false,
                false,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if !self.open {
            return;
        }
        // A genuinely modal dialog is *blocking*: while open it consumes
        // every keyboard event and every pointer event, full stop. The App
        // additionally suppresses its own focus shortcuts and swallows
        // click-through (see `Widget::modal_capture`), but the dialog marks
        // events handled itself so it stays modal even when hosted directly
        // (tests, headless) without the App's cooperation.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                // Forward hover to the content so embedded inputs set their
                // hover flag — a TextInput only takes a click while hovered,
                // and the preceding CursorMoved is what arms it.
                self.forward_to_content(
                    event, scale_factor, modifiers, state, path, focus_target,
                );
                // Cursor moves don't "consume" in a way that blocks the App's
                // presence/redraw bookkeeping; leave `handled` for press/keys.
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
                *handled = true;
            }
            WindowEvent::KeyboardInput { .. } => {
                // Every other key goes to the content (typing into a field,
                // etc.) and is then consumed so it never reaches the host.
                self.forward_to_content(
                    event, scale_factor, modifiers, state, path, focus_target,
                );
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_cursor;
                // Order matters: button rects sit INSIDE the dialog rect,
                // so check them first.
                if self.primary_action.is_some() && self.primary_rect.contains(p) {
                    if let Some(action) = self.primary_action.as_mut() {
                        (action.on_click)();
                    }
                } else if self.secondary_action.is_some() && self.secondary_rect.contains(p) {
                    if let Some(action) = self.secondary_action.as_mut() {
                        (action.on_click)();
                    }
                } else if self.rect.contains(p) {
                    // Click inside the box but outside any action button —
                    // forward to the content so an embedded field focuses
                    // and lands its caret at the click.
                    self.forward_to_content(
                        event, scale_factor, modifiers, state, path, focus_target,
                    );
                } else {
                    // Click outside the dialog box but inside the backdrop —
                    // dismiss.
                    if let Some(cb) = self.on_dismiss.as_mut() {
                        cb();
                    }
                }
                // Every left-press while open is consumed — modals are
                // blocking, so a click never leaks to widgets behind the
                // backdrop regardless of where it landed.
                *handled = true;
            }
            WindowEvent::MouseInput { .. } => {
                // Other buttons / release while open: swallow, but still
                // forward in-box presses so e.g. a right-click context
                // affordance in the content can react.
                if self.rect.contains(self.last_cursor) {
                    self.forward_to_content(
                        event, scale_factor, modifiers, state, path, focus_target,
                    );
                }
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "ModalDialog" }

    fn modal_capture(&self) -> Option<Rect> {
        if self.open { Some(self.rect) } else { None }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.content]
    }

    fn get_rect(&self) -> Rect { self.rect }

    /// An open dialog traps keyboard focus: the host keeps Tab/Shift+Tab and
    /// arrow navigation inside the dialog's content while it's up, and
    /// restores the previously-focused element when it closes.
    fn traps_focus(&self) -> bool { self.open }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A dialog with a backdrop is modal by definition. The title is the
        // accessible name (FTL-resolved — falls back to the literal when no
        // translation exists). A closed dialog is hidden from the tree.
        let title = crate::i18n!(&self.title);
        let name = (!title.is_empty()).then_some(title);
        Some(
            AccessNode::new(Role::Dialog)
                .with_optional_name(name)
                .with_state(AccessState {
                    modal: true,
                    hidden: !self.open,
                    ..Default::default()
                }),
        )
    }
}

#[cfg(test)]
mod tests {
    //! Unit 7 coverage. The four required tests exercise the dismiss
    //! paths (Escape + backdrop click) and the primary-action click,
    //! plus a paint-contract assertion via the Vello encoding. The
    //! `TestContent` leaf below stands in for an arbitrary `C: Widget`
    //! so the generic surface is exercised end-to-end without depending
    //! on any other widget's behaviour.
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use vello_encoding::DrawTag;
    use winit::event::{DeviceId, KeyEvent, MouseButton};
    use winit::keyboard::{KeyLocation, PhysicalKey};

    /// Minimal leaf widget — fixed empty rect, no paint, no events.
    /// Used solely as the generic content placeholder in tests so the
    /// `ModalDialog<C>` generic compiles. Mirrors the synthetic-leaf
    /// pattern used in `tests/e2e_shell_flows.rs`.
    struct TestContent;

    impl Widget for TestContent {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(Style::default()).unwrap();
            node_map.insert(node, self as *mut dyn Widget);
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
        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
        }
        fn name(&self) -> &str { "TestContent" }
        fn get_rect(&self) -> Rect { Rect::ZERO }
    }

    /// Content leaf that counts how many events it was forwarded, so the
    /// modal-trap forwarding contract can be asserted. Sizes itself to a
    /// real rect so it lands inside the dialog box.
    #[derive(Default)]
    struct CountingContent {
        events: std::rc::Rc<std::cell::Cell<usize>>,
        rect: Rect,
    }

    impl Widget for CountingContent {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(200.0),
                        height: Dimension::length(40.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }
        fn measure(
            &mut self,
            _k: taffy::Size<Option<f32>>,
            _a: taffy::Size<taffy::AvailableSpace>,
            _tr: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            taffy::Size { width: 200.0, height: 40.0 }
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
        fn handle_event(
            &mut self,
            _event: &WindowEvent,
            _scale_factor: f64,
            _modifiers: &winit::event::Modifiers,
            _state: &mut AutoCommit,
            _path: &ObjId,
            _handled: &mut bool,
            _focus_target: &mut Option<NodeId>,
            _node_id: NodeId,
            _taffy: &TaffyTree<()>,
        ) {
            self.events.set(self.events.get() + 1);
        }
        fn name(&self) -> &str { "CountingContent" }
        fn get_rect(&self) -> Rect { self.rect }
    }

    /// SAFETY: opaque `DeviceId` whose all-zero bit pattern is tolerated
    /// by winit — same trick used in `permission_prompt::tests` and in
    /// `shell/tests/e2e_shell_flows.rs::key_press`.
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

    /// Build a winit `KeyEvent` over a zeroed buffer (winit's
    /// `KeyEventExtra` field is `pub(crate)` so we can't struct-literal
    /// one). All public fields tolerate the all-zero pattern; we then
    /// overwrite the bits the dialog actually inspects. Same approach
    /// `shell/tests/e2e_shell_flows.rs::key_press` uses.
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

    /// Drive `handle_event` through the dialog with default scaffolding.
    /// Mirrors `permission_prompt::tests::dispatch`.
    fn dispatch<C: Widget + 'static>(d: &mut ModalDialog<C>, event: &WindowEvent) {
        let _ = dispatch_handled(d, event);
    }

    /// Same as [`dispatch`] but returns whether the dialog marked the event
    /// handled — needed to assert the modal-blocking contract.
    fn dispatch_handled<C: Widget + 'static>(
        d: &mut ModalDialog<C>,
        event: &WindowEvent,
    ) -> bool {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        d.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
        handled
    }

    /// Build a winit character key event for an arbitrary key. Used to
    /// assert that a non-Escape key while open is swallowed.
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

    /// Lay out a `ModalDialog<CountingContent>` through the real five-pass
    /// path so `content_taffy` / `content_node` are populated and in-box
    /// forwarding works.
    fn laid_out_counting(open: bool) -> (ModalDialog<CountingContent>, std::rc::Rc<std::cell::Cell<usize>>) {
        let events = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let content = CountingContent { events: events.clone(), rect: Rect::ZERO };
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

    /// Dispatch one `draw` and return the count of `DrawTag::COLOR`
    /// encoding entries. Mirrors `badge::tests::color_fill_count`.
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

    /// Required test #1 — Escape (pressed) fires `on_dismiss`.
    #[test]
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

    /// Required test #2 — left-click outside the centered dialog box
    /// (but inside the viewport-sized backdrop) fires `on_dismiss`.
    #[test]
    fn modal_dialog_backdrop_click_dismisses() {
        let dismissed = Rc::new(Cell::new(false));
        let dismissed_clone = Rc::clone(&dismissed);
        let mut d = ModalDialog::new("Title", TestContent)
            .open(true)
            .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
            .on_dismiss(move || dismissed_clone.set(true));
        // (10, 10) is well outside the centered 480×320 dialog rect
        // (which spans roughly [160..640]×[140..460] inside the 800×600
        // viewport) but inside the backdrop.
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

    /// Required test #3 — left-click inside `primary_rect` fires the
    /// primary action callback.
    #[test]
    fn modal_dialog_primary_action_invokes_callback() {
        let fired = Rc::new(Cell::new(false));
        let fired_clone = Rc::clone(&fired);
        let mut d = ModalDialog::new("Title", TestContent)
            .open(true)
            .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0))
            .primary_action("OK", move || fired_clone.set(true));
        // Drive a taffy `apply_layout` pass so `primary_rect` is populated
        // through the actual five-pass path (not just `with_viewport`).
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

    /// Required test #4 — when `open == true`, `draw` emits at least
    /// one COLOR draw tag covering the full viewport (the backdrop fill
    /// is the first paint). We assert by inspecting the Vello encoding
    /// directly so we don't depend on pixel readback.
    #[test]
    fn modal_dialog_paints_backdrop_when_open() {
        let mut d = ModalDialog::<TestContent>::new("Title", TestContent)
            .open(true)
            .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        let open_fills = color_fill_count(&mut d);
        assert!(
            open_fills >= 1,
            "an open dialog must emit at least one COLOR fill (the backdrop); got {open_fills}",
        );

        // Cross-check: a closed dialog must paint nothing — same
        // contract `permission_prompt` follows when it's not in the
        // tree. This guards against a future refactor that paints the
        // backdrop unconditionally.
        let mut closed = ModalDialog::<TestContent>::new("Title", TestContent)
            .open(false)
            .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
        let closed_fills = color_fill_count(&mut closed);
        assert_eq!(
            closed_fills, 0,
            "a closed dialog must emit zero COLOR fills; got {closed_fills}",
        );
    }

    /// A destructive dialog must paint its primary button with a different
    /// colour than the ordinary accent primary. We drive the real draw path
    /// for both variants (identical but for `with_destructive`) and assert the
    /// encoded fill data differs — the only thing that can change it is the
    /// primary button's fill colour.
    #[test]
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

    #[test]
    fn accessibility_reports_dialog_role_modal_and_title() {
        use crate::a11y::Role;
        let d = ModalDialog::new("Confirm delete", TestContent).open(true);
        let node = Widget::accessibility(&d).expect("dialog is not presentational");
        assert_eq!(node.role, Role::Dialog);
        assert_eq!(node.name.as_deref(), Some("Confirm delete"));
        assert!(node.state.modal, "a backdrop dialog is modal");
        assert!(!node.state.hidden, "an open dialog is visible");
    }

    #[test]
    fn accessibility_closed_dialog_is_hidden() {
        let d = ModalDialog::new("Confirm delete", TestContent).open(false);
        let node = Widget::accessibility(&d).unwrap();
        assert!(node.state.hidden, "a closed dialog is hidden from the tree");
    }

    #[test]
    fn children_exposes_dialog_content() {
        let d = ModalDialog::new("Title", TestContent).open(true);
        assert_eq!(
            Widget::children(&d).len(),
            1,
            "the dialog body must be reachable as a child for the a11y tree",
        );
    }
    // === Modal focus-trap contract ==================================

    /// An OPEN dialog reports its box rect via `modal_capture`; a closed
    /// one reports `None`. This is the signal the App uses to suppress its
    /// own focus shortcuts and swallow click-through.
    #[test]
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
    #[test]
    fn open_dialog_swallows_all_keyboard_events() {
        let (mut d, _events) = laid_out_counting(true);
        let handled = dispatch_handled(&mut d, &char_key_event());
        assert!(handled, "an open dialog must consume every keyboard event");
    }

    /// A press OUTSIDE the box is consumed (swallowed) AND dismisses; it
    /// must not leak to widgets behind the backdrop.
    #[test]
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
    #[test]
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
    #[test]
    fn closed_dialog_is_inert() {
        let (mut d, events) = laid_out_counting(false);
        let handled = dispatch_handled(&mut d, &left_press());
        assert!(!handled, "a closed dialog must not consume events");
        assert_eq!(events.get(), 0, "a closed dialog must not forward events");
    }
}

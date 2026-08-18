//! `SearchField` — a search-styled text box: a rounded pill with a leading
//! magnifier, a real editable [`TextInput`] in the middle, and a trailing "×"
//! that clears the query. A dozen apps (`music`, `mail`, `tube`, `containers`,
//! `software_store`, `monitor`, `file-explorer`, `permissions`, `browser`,
//! `snippets`, `contacts`, …) hand-roll exactly this — a pill `Container` +
//! `Icon::Search` gutter wrapping a `TextInput`, plus a bespoke clear button.
//!
//! `SearchField` composes the toolkit's own [`TextInput`] (so caret, selection,
//! IME, and CRDT binding all come for free) and only adds the search chrome.
//! It works both **local** (the query lives outside the CRDT — the common case
//! for a filter box) and **bound** to a doc path. The query is read back with
//! [`SearchField::value`] or the one-shot [`SearchField::take_dirty`], which
//! also reports a clear as an empty-string edit.
//!
//! ## Launcher mode — search does not live in the app
//!
//! Built with [`SearchField::opens_palette`], the field stops being an inline
//! filter and becomes a *thin affordance* that raises the shared Ctrl+K
//! [`CommandPalette`](crate::widget::CommandPalette) scoped to the hosting app.
//! The pill renders a leading magnifier, a placeholder, and a trailing `Ctrl+K`
//! kbd chip; it is **non-editable** — a click anywhere on it just latches a
//! request the host polls with [`SearchField::take_palette_request`], then opens
//! its own `CommandPalette` seeded with the app's resources. The search UI and
//! ranking thus live in one shared toolkit widget, not in every app's bespoke
//! search bar.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

const HEIGHT: f64 = 38.0;
const ICON_GUTTER: f64 = 34.0;
const CLEAR_GUTTER: f64 = 30.0;
const ICON_D: f64 = 16.0;
const CLEAR_D: f64 = 16.0;

/// A search-styled input: magnifier + editable text + clear button.
pub struct SearchField {
    input: TextInput,
    leading: Icon,
    clearable: bool,
    cleared: bool,
    rect: Rect,
    clear_rect: Rect,
    last_cursor: Point,
    /// Launcher mode: the field opens the shared Ctrl+K palette instead
    /// of editing an inline query. See [`SearchField::opens_palette`].
    opens_palette: bool,
    /// Latched when the (non-editable) launcher pill is activated;
    /// drained by the host via [`SearchField::take_palette_request`].
    palette_requested: bool,
    /// Set once [`apply_layout_external`](Self::apply_layout_external) is used:
    /// the host hand-places the field and passes a throwaway taffy tree, so
    /// `handle_event` must not look the inner input up via taffy.
    standalone: bool,
}

impl SearchField {
    /// A local-mode search field (query kept outside the CRDT). Chain
    /// [`with_placeholder`](Self::with_placeholder) / [`with_id`](Self::with_id)
    /// / [`bound`](Self::bound) to configure it.
    pub fn new() -> Self {
        Self {
            input: TextInput::local().with_plain_chrome().with_placeholder("Search"),
            leading: Icon::Search,
            clearable: true,
            cleared: false,
            rect: Rect::ZERO,
            clear_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            opens_palette: false,
            palette_requested: false,
            standalone: false,
        }
    }

    /// Turn this into a **launcher**: instead of an inline filter, the
    /// pill becomes a non-editable affordance that opens the shared
    /// Ctrl+K [`CommandPalette`](crate::widget::CommandPalette) scoped to
    /// the app. Renders a trailing `Ctrl+K` kbd chip and, on click,
    /// latches a request the host reads with
    /// [`take_palette_request`](Self::take_palette_request).
    ///
    /// This is the "search does not live in the app" contract — the app
    /// stops hand-rolling search UI and delegates to the OS-shared
    /// palette.
    pub fn opens_palette(mut self) -> Self {
        self.opens_palette = true;
        self.clearable = false;
        self
    }

    /// Whether this field is a palette launcher (vs. an inline filter).
    pub fn is_palette_launcher(&self) -> bool {
        self.opens_palette
    }

    /// Mark this field as hand-placed by an immediate-mode host (which drives
    /// it via [`apply_layout_external`](Self::apply_layout_external) and routes
    /// events with a throwaway taffy tree). Set this at construction so the
    /// field is safe to route events to even before its first draw. Retained
    /// hosts never call this.
    pub fn standalone(mut self) -> Self {
        self.standalone = true;
        self
    }

    /// One-shot read: `true` exactly once after the launcher pill was
    /// activated (clicked). The host uses this to open its
    /// [`CommandPalette`](crate::widget::CommandPalette). Always `false`
    /// for a non-launcher field.
    pub fn take_palette_request(&mut self) -> bool {
        std::mem::take(&mut self.palette_requested)
    }

    /// Set the placeholder shown when the query is empty.
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.input.set_placeholder(hint);
        self
    }

    /// Change the placeholder at runtime (e.g. when a palette repurposes the
    /// field to collect a prompted action's argument, the placeholder becomes
    /// the action's prompt).
    pub fn set_placeholder(&mut self, hint: &str) {
        self.input.set_placeholder(hint);
    }

    /// Give the embedded input a stable test/focus id.
    pub fn with_id(mut self, id: &str) -> Self {
        self.input = std::mem::replace(&mut self.input, TextInput::local()).with_id(id);
        self
    }

    /// Bind the query to a CRDT string path so edits sync through the doc,
    /// instead of the default local-only buffer.
    pub fn bound(mut self, path: &str) -> Self {
        self.input = std::mem::replace(&mut self.input, TextInput::local())
            .with_plain_chrome()
            .with_on_change_path(path);
        self
    }

    /// Override the leading icon (default [`Icon::Search`]).
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.leading = icon;
        self
    }

    /// Hide the trailing clear button (it otherwise appears while the query
    /// is non-empty).
    pub fn without_clear(mut self) -> Self {
        self.clearable = false;
        self
    }

    /// The current query text.
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// Seed the query programmatically (e.g. from a selection, a recent search,
    /// or the shell handing off the text that opened the palette). Goes through
    /// the shared text core, so caret/selection stay consistent.
    pub fn set_query(&mut self, text: &str) {
        self.input.set_text(text);
    }

    /// Focus (or blur) the embedded input, so a host can raise the field ready
    /// to type without a click. Never focuses a launcher pill (it is not
    /// editable).
    pub fn set_focused(&mut self, on: bool) {
        if !self.opens_palette {
            self.input.set_focus(on);
        }
    }

    /// Whether the query is empty.
    pub fn is_empty(&self) -> bool {
        self.input.value().is_empty()
    }

    /// The leading (magnifier by default) chrome icon. Exposed so a host — or a
    /// test guarding that the field keeps its magnifier — can read it without
    /// rasterising a frame.
    pub fn leading_icon(&self) -> Icon {
        self.leading
    }

    /// The placeholder hint shown while the query is empty and the field is
    /// unfocused.
    pub fn placeholder(&self) -> &str {
        self.input.placeholder()
    }

    /// Whether the embedded input currently holds focus — lets a host that
    /// paints its own surrounding chrome (focus ring, placeholder gating) read
    /// the field's focus state instead of tracking a parallel boolean.
    pub fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    /// Position the field at an explicit rect, for immediate-mode hosts that
    /// hand-place their widgets (mirrors [`TextInput::apply_layout_external`]).
    /// The inner text input is inset by the magnifier + clear-button gutters.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.standalone = true;
        self.rect = rect;
        let inner = Rect::new(rect.x0 + ICON_GUTTER, rect.y0, rect.x1 - CLEAR_GUTTER, rect.y1);
        self.input.apply_layout_external(inner);
    }

    /// One-shot read of query changes since the last call — typing **and**
    /// clears (a clear reports `Some("")`). Returns `None` when nothing
    /// changed. This is the primary channel a host polls to re-run its filter.
    pub fn take_dirty(&mut self) -> Option<String> {
        if let Some(t) = self.input.take_dirty() {
            return Some(t);
        }
        if self.cleared {
            self.cleared = false;
            return Some(String::new());
        }
        None
    }

    /// The clear-button hit rect (empty until laid out + drawn). Exposed for
    /// tests.
    pub fn clear_rect(&self) -> Rect {
        self.clear_rect
    }

    /// Whether the clear button is currently showing (clearable + non-empty).
    /// Never in launcher mode — the trailing slot holds the `Ctrl+K` chip.
    fn clear_visible(&self) -> bool {
        self.clearable && !self.opens_palette && !self.input.value().is_empty()
    }

    /// Draw the trailing `Ctrl+K` kbd chip shown in launcher mode. A small
    /// rounded pill with a hairline border and dim label, right-aligned
    /// inside the field so it reads as a keyboard hint rather than a
    /// control.
    fn draw_kbd_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        const LABEL: &str = "Ctrl+K";
        const SIZE: f32 = 11.0;
        let (tw, _) = text_renderer.measure(LABEL, SIZE, None);
        let chip_h = 20.0_f64;
        let chip_w = tw as f64 + 14.0;
        let right_inset = 10.0_f64;
        let cy = self.rect.center().y;
        let chip = Rect::new(
            self.rect.x1 - right_inset - chip_w,
            cy - chip_h / 2.0,
            self.rect.x1 - right_inset,
            cy + chip_h / 2.0,
        );
        let rounded = RoundedRect::from_rect(chip, 5.0);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            vello::peniko::Color::from_rgba8(255, 255, 255, 16),
            None,
            &rounded,
        );
        scene.stroke(&Stroke::new(1.0), transform, theme.border_standard, None, &rounded);
        text_renderer.draw_text(
            scene,
            LABEL,
            SIZE,
            theme.text_dim,
            transform * Affine::translate((chip.x0 + 7.0, cy - 6.5)),
            None,
        );
    }
}

impl Default for SearchField {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchField {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.input.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.input.build_layout(taffy, theme, node_map);
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    align_items: Some(taffy::AlignItems::Center),
                    flex_shrink: 0.0,
                    padding: taffy::Rect {
                        left: length(ICON_GUTTER as f32),
                        right: length(CLEAR_GUTTER as f32),
                        top: length(0.0),
                        bottom: length(0.0),
                    },
                    size: taffy::Size { width: percent(1.0), height: length(HEIGHT as f32) },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        let child_node = taffy.children(node).unwrap()[0];
        self.input.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        // Pill chrome, focus-aware.
        let focused = self.input.is_focused();
        let pill = RoundedRect::from_rect(self.rect, theme.tokens.radii.full as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &pill,
        );
        let border = if focused { theme.border_focus } else { theme.border_standard };
        scene.stroke(&Stroke::new(if focused { 1.5 } else { 1.0 }), transform, border, None, &pill);

        // Leading magnifier.
        let icon_rect = Rect::from_center_size(
            Point::new(self.rect.x0 + ICON_GUTTER / 2.0, self.rect.center().y),
            (ICON_D, ICON_D),
        );
        self.leading.draw_with_stroke(scene, transform, icon_rect, theme.text_dim, 1.6);

        // Editable text.
        self.input.draw(scene, transform, text_renderer, theme, presence);

        // Trailing slot: a `Ctrl+K` kbd chip in launcher mode, otherwise
        // the clear button (when there's a query to clear).
        if self.opens_palette {
            self.clear_rect = Rect::ZERO;
            self.draw_kbd_chip(scene, transform, text_renderer, theme);
        } else if self.clear_visible() {
            let cr = Rect::from_center_size(
                Point::new(self.rect.x1 - CLEAR_GUTTER / 2.0, self.rect.center().y),
                (CLEAR_D, CLEAR_D),
            );
            self.clear_rect = cr;
            let hovered = cr.contains(self.last_cursor);
            let col = if hovered { theme.text_primary } else { theme.text_dim };
            Icon::X.draw_with_stroke(scene, transform, cr, col, 1.6);
        } else {
            self.clear_rect = Rect::ZERO;
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
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        // Launcher mode: the pill is non-editable. A left press anywhere on
        // it latches a palette-open request and never reaches the inner
        // input, so the field never takes focus or edits a query.
        if self.opens_palette {
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
            {
                if self.rect.contains(self.last_cursor) {
                    self.palette_requested = true;
                    *handled = true;
                }
            }
            return;
        }
        // Intercept a click on the clear button before the input sees it.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if self.clear_visible() && self.clear_rect.contains(self.last_cursor) {
                self.input.set_text("");
                self.cleared = true;
                *handled = true;
                return;
            }
        }
        // Route to the inner input. Retained hosts pass a taffy tree whose
        // `node_id` has the input as its first child. Immediate-mode hosts
        // (that hand-place the field via `apply_layout_external`) pass a
        // throwaway tree where looking the child up would panic, so pass
        // `node_id` straight through — the input ignores taffy and only uses
        // the node id for focus-target identity, which those hosts sink.
        let child_node = if self.standalone {
            node_id
        } else {
            taffy.children(node_id).unwrap()[0]
        };
        self.input
            .handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, child_node, taffy);
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        // The whole launcher pill is a click target.
        if self.opens_palette {
            if self.rect.contains(point) {
                return Some(winit::window::CursorIcon::Pointer);
            }
            return None;
        }
        if self.clear_visible() && self.clear_rect.contains(point) {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.input.cursor_shape(point)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.input as &dyn Widget]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.input as &mut (dyn Widget + '_)]
    }

    fn needs_commit(&self) -> bool {
        self.input.needs_commit()
    }

    fn on_commit(&mut self) {
        self.input.on_commit();
    }

    fn name(&self) -> &str { "SearchField" }

    fn get_rect(&self) -> Rect { self.rect }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Launcher mode is a button that opens the shared palette, not an
        // editable field — expose it as such so AT surfaces the action.
        if self.opens_palette {
            return Some(
                AccessNode::new(Role::Button)
                    .with_name("Open search (Ctrl+K)")
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        // A search box is a labelled text input; its current query is the value.
        Some(
            AccessNode::new(Role::TextInput)
                .with_name("Search")
                .with_value(self.input.value().to_string())
                .with_state(AccessState::new().focusable(true).focused(self.input.is_focused())),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay_out(w: &mut dyn Widget, vw: f32) -> TaffyTree<()> {
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
                    height: taffy::AvailableSpace::Definite(HEIGHT as f32),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map(|&p| unsafe { (*p).measure(k, a, &mut tr) }).unwrap_or(taffy::Size::ZERO)
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
        taffy
    }

    fn draw(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    #[test]
    fn embedded_input_sits_inside_the_icon_gutter() {
        let mut sf = SearchField::new();
        let _ = lay_out(&mut sf, 320.0);
        // The pill fills the width; the input is inset past the leading gutter.
        assert!((sf.get_rect().width() - 320.0).abs() < 1.0, "pill fills width");
        let input_rect = sf.input.get_rect();
        assert!(
            input_rect.x0 >= sf.get_rect().x0 + ICON_GUTTER - 0.5,
            "the editable text starts past the magnifier gutter ({} vs {})",
            input_rect.x0,
            sf.get_rect().x0 + ICON_GUTTER
        );
    }

    #[test]
    fn clear_button_hidden_when_empty_shown_when_typed() {
        let mut sf = SearchField::new();
        let _ = lay_out(&mut sf, 320.0);
        draw(&mut sf);
        assert_eq!(sf.clear_rect(), Rect::ZERO, "empty query ⇒ no clear button");
        sf.input.set_text("hello");
        draw(&mut sf);
        assert_ne!(sf.clear_rect(), Rect::ZERO, "non-empty query ⇒ a clear button appears");
    }

    #[test]
    fn clicking_clear_empties_query_and_reports_dirty() {
        // Build a tree whose root node IS the SearchField, so handle_event can
        // resolve the child via `taffy.children(node)` exactly as the App does.
        let theme = Theme::dark();
        let mut sf = SearchField::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = sf.build_layout(&mut taffy, &theme, &mut nm);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(HEIGHT as f32),
                },
                |k, a, id, _, _| nm.get(&id).map(|&p| unsafe { (*p).measure(k, a, &mut tr) }).unwrap_or(taffy::Size::ZERO),
            )
            .unwrap();
        sf.apply_layout(&taffy, node, Point::ZERO);
        sf.input.set_text("query text");
        draw(&mut sf);
        assert!(!sf.is_empty());
        let target = sf.clear_rect().center();
        assert!(target.x > 0.0, "clear button resolves after draw");

        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let rootid = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        sf.handle_event(
            &WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(target.x, target.y) },
            1.0, &mods, &mut doc, &rootid, &mut handled, &mut ft, node, &taffy,
        );
        sf.handle_event(
            &WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left },
            1.0, &mods, &mut doc, &rootid, &mut handled, &mut ft, node, &taffy,
        );
        assert!(handled, "clear click consumed");
        assert!(sf.is_empty(), "clear empties the query");
        assert_eq!(sf.take_dirty().as_deref(), Some(""), "clear reports an empty-string edit");
        assert_eq!(sf.take_dirty(), None, "dirty drains after one read");
    }

    #[test]
    fn accessibility_is_a_text_input_with_the_query_as_value() {
        use crate::a11y::Role;
        let mut sf = SearchField::new();
        sf.input.set_text("abc");
        let node = Widget::accessibility(&sf).expect("search field exposes a11y");
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("abc"));
        assert!(node.state.focusable);
    }

    #[test]
    fn launcher_click_latches_a_palette_request_and_stays_non_editable() {
        // A launcher field: build a tree whose root IS the field so
        // handle_event resolves the child via `taffy.children(node)`.
        let theme = Theme::dark();
        let mut sf = SearchField::new().opens_palette().with_placeholder("Search containers…");
        assert!(sf.is_palette_launcher());
        let mut taffy = TaffyTree::<()>::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = sf.build_layout(&mut taffy, &theme, &mut nm);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(HEIGHT as f32),
                },
                |k, a, id, _, _| nm.get(&id).map(|&p| unsafe { (*p).measure(k, a, &mut tr) }).unwrap_or(taffy::Size::ZERO),
            )
            .unwrap();
        sf.apply_layout(&taffy, node, Point::ZERO);
        draw(&mut sf);

        // No pending request before any interaction.
        assert!(!sf.take_palette_request());

        // Click somewhere inside the pill.
        let center = sf.get_rect().center();
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let rootid = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        sf.handle_event(
            &WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(center.x, center.y) },
            1.0, &mods, &mut doc, &rootid, &mut handled, &mut ft, node, &taffy,
        );
        sf.handle_event(
            &WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left },
            1.0, &mods, &mut doc, &rootid, &mut handled, &mut ft, node, &taffy,
        );
        assert!(handled, "the launcher consumes the click");
        // The click did NOT focus the inner input (search never lives in
        // the field itself).
        assert!(!sf.input.is_focused(), "launcher pill stays non-editable");
        // Exactly one request is latched, and it drains once.
        assert!(sf.take_palette_request(), "click latches a palette-open request");
        assert!(!sf.take_palette_request(), "the request drains after one read");
    }

    #[test]
    fn launcher_reports_button_role_for_accessibility() {
        use crate::a11y::Role;
        let sf = SearchField::new().opens_palette();
        let node = Widget::accessibility(&sf).expect("launcher exposes a11y");
        assert_eq!(node.role, Role::Button, "launcher is a button, not a text field");
        assert!(node.state.focusable);
    }
}

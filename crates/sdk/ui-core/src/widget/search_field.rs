//! `SearchField` — a search-styled text box: a rounded pill with a leading
//! magnifier, an editable [`TextInput`] in the middle, and a trailing "×"
//! that clears the query. Composes the toolkit's own [`TextInput`] so caret,
//! selection, IME, and CRDT binding are shared.
//!
//! ## Launcher mode — search does not live in the app
//!
//! Built with [`SearchField::opens_palette`], the field becomes a
//! non-editable affordance that raises the shared command-search shortcut
//! [`CommandPalette`](crate::widget::CommandPalette) instead: a click
//! latches a request the host polls with [`SearchField::take_palette_request`]
//! and opens its own `CommandPalette`. Search UI and ranking thus live in
//! one shared toolkit widget, not in every app's bespoke search bar.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::icons::Icon;
use crate::platform::{PlatformConventions, Shortcut};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{TextInput, TextInputKind, Widget};

const HEIGHT: f64 = 38.0;
const ICON_GUTTER: f64 = 34.0;
const CLEAR_GUTTER: f64 = 30.0;
const ICON_D: f64 = 16.0;
const CLEAR_D: f64 = 16.0;

/// A search-styled input: magnifier + editable text + clear button.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent mode/dirty marker (clearable, cleared, \
              opens_palette, palette_requested) set and read on its own schedule, never \
              combined into a state machine, so a bitflags/enum encoding would not simplify \
              the call sites"
)]
pub struct SearchField {
    input: TextInput,
    leading: Icon,
    clearable: bool,
    cleared: bool,
    rect: Rect,
    clear_rect: Rect,
    last_cursor: Point,
    /// See [`SearchField::opens_palette`].
    opens_palette: bool,
    /// Latched when the (non-editable) launcher pill is activated;
    /// drained by the host via [`SearchField::take_palette_request`].
    palette_requested: bool,
    platform: PlatformConventions,
}

impl SearchField {
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    pub(crate) fn with_language(language: &str) -> Self {
        Self::with_input(TextInput::local_with_language(language))
    }

    fn with_input(input: TextInput) -> Self {
        Self {
            input: input
                .with_plain_chrome()
                .with_kind(TextInputKind::Search)
                .with_placeholder("Search"),
            leading: Icon::Search,
            clearable: true,
            cleared: false,
            rect: Rect::ZERO,
            clear_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            opens_palette: false,
            palette_requested: false,
            platform: PlatformConventions::default(),
        }
    }

    #[must_use]
    pub fn opens_palette(mut self) -> Self {
        self.opens_palette = true;
        self.clearable = false;
        self
    }

    pub fn is_palette_launcher(&self) -> bool {
        self.opens_palette
    }

    /// `true` exactly once after the launcher pill was clicked. Always
    /// `false` for a non-launcher field.
    pub fn take_palette_request(&mut self) -> bool {
        std::mem::take(&mut self.palette_requested)
    }

    /// Keep pointer and semantic activation on one latched request path.
    fn request_palette(&mut self) -> bool {
        if !self.opens_palette {
            return false;
        }
        self.palette_requested = true;
        true
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.input.set_placeholder(hint);
        self
    }

    pub fn set_placeholder(&mut self, hint: &str) {
        self.input.set_placeholder(hint);
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.input = std::mem::replace(&mut self.input, TextInput::local()).with_id(id);
        self
    }

    /// Binds the query to a CRDT string path, instead of the default
    /// local-only buffer.
    #[must_use]
    pub fn bound(mut self, path: &str) -> Self {
        self.input = std::mem::replace(&mut self.input, TextInput::local())
            .with_plain_chrome()
            .with_kind(TextInputKind::Search)
            .with_on_change_path(path);
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.leading = icon;
        self
    }

    #[must_use]
    pub fn without_clear(mut self) -> Self {
        self.clearable = false;
        self
    }

    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// Goes through the shared text core, so caret/selection stay consistent.
    pub fn set_query(&mut self, text: &str) {
        self.input.set_text(text);
    }

    /// Never focuses a launcher pill (it is not editable).
    pub fn set_focused(&mut self, on: bool) {
        if !self.opens_palette {
            self.input.set_focus(on);
        }
    }

    /// Select the current query for conventional Find/Search re-entry.
    pub fn select_all(&mut self) {
        if !self.opens_palette {
            self.input.select_all();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.input.value().is_empty()
    }

    /// Typing **and** clears (a clear reports `Some("")`).
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

    /// Empty until laid out + drawn.
    pub fn clear_rect(&self) -> Rect {
        self.clear_rect
    }

    /// Never in launcher mode — the trailing slot holds the `Ctrl+K` chip.
    fn clear_visible(&self) -> bool {
        self.clearable && !self.opens_palette && !self.input.value().is_empty()
    }

    fn draw_kbd_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        const SIZE: f32 = 11.0;
        let label = Shortcut::command("K").label(self.platform);
        let (tw, _) = text_renderer.measure(&label, SIZE, None);
        let chip_h = 20.0_f64;
        let chip_w = f64::from(tw) + 14.0;
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
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &rounded,
        );
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition is float matrix multiplication; it cannot overflow \
                      or panic"
        )]
        let label_transform = transform * Affine::translate((chip.x0 + 7.0, cy - 6.5));
        text_renderer.draw_text(scene, &label, SIZE, theme.text_dim, label_transform, None);
    }
}

impl Default for SearchField {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchField {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.input.set_platform_conventions(conventions);
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.input.sync(state, path);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "these are widget-geometry constants — small, bounded pixel values — so \
                  narrowing to f32 for Taffy's layout math loses no meaningful precision"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.input.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                align_items: Some(taffy::AlignItems::Center),
                flex_shrink: 0.0,
                padding: taffy::Rect {
                    left: length(ICON_GUTTER as f32),
                    right: length(CLEAR_GUTTER as f32),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(HEIGHT as f32),
                },
                ..Default::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let Some(&child_node) = taffy.children(node).unwrap_or_default().first() {
            self.input
                .apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
        }
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
        let focused = self.input.is_focused();
        let pill = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.full));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &pill,
        );
        let border = if focused {
            theme.border_focus
        } else {
            theme.border_standard
        };
        scene.stroke(
            &Stroke::new(if focused { 1.5 } else { 1.0 }),
            transform,
            border,
            None,
            &pill,
        );

        let icon_rect = Rect::from_center_size(
            Point::new(self.rect.x0 + ICON_GUTTER / 2.0, self.rect.center().y),
            (ICON_D, ICON_D),
        );
        self.leading
            .draw_with_stroke(scene, transform, icon_rect, theme.text_dim, 1.6);

        self.input
            .draw(scene, transform, text_renderer, theme, presence);

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
            let col = if hovered {
                theme.text_primary
            } else {
                theme.text_dim
            };
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
        // A left press anywhere latches a palette-open request and never
        // reaches the inner input.
        if self.opens_palette {
            if let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } = event
                && self.rect.contains(self.last_cursor)
            {
                self.request_palette();
                *handled = true;
            }
            return;
        }
        // Intercept a click on the clear button before the input sees it.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.clear_visible()
            && self.clear_rect.contains(self.last_cursor)
        {
            self.input.set_text("");
            self.cleared = true;
            *handled = true;
            return;
        }
        if let Some(&child_node) = taffy.children(node_id).unwrap_or_default().first() {
            self.input.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                child_node,
                taffy,
            );
        }
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

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Widget::ime_purpose(&self.input)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.input as &dyn Widget]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.input);
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

    fn name(&self) -> &'static str {
        "SearchField"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // A button that opens the shared palette, not an editable field.
        if self.opens_palette {
            let shortcut = Shortcut::command("K").label(self.platform);
            return Some(
                AccessNode::new(Role::Button)
                    .with_name(format!("Open search ({shortcut})"))
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        self.input
            .accessibility()
            .map(|node| node.with_name("Search"))
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> bool {
        if self.opens_palette {
            return matches!(action, crate::a11y::Action::Activate) && self.request_palette();
        }
        Widget::perform_action(&mut self.input, action, state, path)
    }
}

#[cfg(test)]
pub(crate) fn observed_language(field: &SearchField) -> &str {
    field.input.spell_language()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value; f32 precision loss is \
                  immaterial"
    )]
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
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
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
        assert!(
            (sf.get_rect().width() - 320.0).abs() < 1.0,
            "pill fills width"
        );
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
        assert_ne!(
            sf.clear_rect(),
            Rect::ZERO,
            "non-empty query ⇒ a clear button appears"
        );
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value; f32 precision loss is \
                  immaterial"
    )]
    fn clicking_clear_empties_query_and_reports_dirty() {
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
                |k, a, id, _, _| {
                    nm.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
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
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(target.x, target.y),
            },
            1.0,
            &mods,
            &mut doc,
            &rootid,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        sf.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut doc,
            &rootid,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        assert!(handled, "clear click consumed");
        assert!(sf.is_empty(), "clear empties the query");
        assert_eq!(
            sf.take_dirty().as_deref(),
            Some(""),
            "clear reports an empty-string edit"
        );
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
    fn launcher_accessibility_uses_the_platform_shortcut_label() {
        use crate::platform::{Platform, PlatformConventions};

        let mut field = SearchField::new().opens_palette();
        field.set_platform_conventions(PlatformConventions::new(Platform::MacOs));
        let node = Widget::accessibility(&field).expect("launcher exposes a11y");
        assert_eq!(node.name.as_deref(), Some("Open search (⌘K)"));

        field.set_platform_conventions(PlatformConventions::new(Platform::Windows));
        let node = Widget::accessibility(&field).expect("launcher exposes a11y");
        assert_eq!(node.name.as_deref(), Some("Open search (Ctrl+K)"));
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value; f32 precision loss is \
                  immaterial"
    )]
    fn launcher_click_latches_a_palette_request_and_stays_non_editable() {
        let theme = Theme::dark();
        let mut sf = SearchField::new()
            .opens_palette()
            .with_placeholder("Search containers…");
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
                |k, a, id, _, _| {
                    nm.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        sf.apply_layout(&taffy, node, Point::ZERO);
        draw(&mut sf);

        assert!(!sf.take_palette_request());

        let center = sf.get_rect().center();
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let rootid = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        sf.handle_event(
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(center.x, center.y),
            },
            1.0,
            &mods,
            &mut doc,
            &rootid,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        sf.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut doc,
            &rootid,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        assert!(handled, "the launcher consumes the click");
        assert!(!sf.input.is_focused(), "launcher pill stays non-editable");
        assert!(
            sf.take_palette_request(),
            "click latches a palette-open request"
        );
        assert!(
            !sf.take_palette_request(),
            "the request drains after one read"
        );
    }

    #[test]
    fn launcher_reports_button_role_for_accessibility() {
        use crate::a11y::Role;
        let sf = SearchField::new().opens_palette();
        let node = Widget::accessibility(&sf).expect("launcher exposes a11y");
        assert_eq!(
            node.role,
            Role::Button,
            "launcher is a button, not a text field"
        );
        assert!(node.state.focusable);
    }

    #[test]
    fn launcher_semantic_activate_latches_the_same_palette_request_as_click() {
        let mut sf = SearchField::new().opens_palette();
        let mut doc = AutoCommit::new();
        assert!(Widget::perform_action(
            &mut sf,
            &crate::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(sf.take_palette_request());
        assert!(!sf.take_palette_request(), "semantic request drains once");
    }
}

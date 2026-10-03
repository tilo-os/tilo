//! `AddressSuggestions` — the address-bar autocomplete dropdown.
//!
//! A leaf `ui` widget the browser chrome positions directly under the
//! address [`ui::widget::TextInput`]. It renders the ranked
//! [`HistoryEntry`] rows produced by
//! [`History::suggest`](crate::history::History::suggest) as a floating panel
//! of list rows — each row shows the page title with its URL beneath it —
//! and lifts the current selection.
//!
//! Two integration styles are supported, and the browser uses both:
//!  * **Imperative** (primary): the chrome owns the address bar's key
//!    handling, so it calls [`AddressSuggestions::move_selection`] on
//!    Up/Down and reads [`AddressSuggestions::selected`] on Enter to pick
//!    the highlighted entry. The `TextInput` keeps focus throughout.
//!  * **Self-driven** (fallback / standalone): if the widget is given the
//!    keyboard directly its [`Widget::handle_event`] handles Up/Down
//!    (move), Enter (choose → [`AddressSuggestions::take_chosen`]) and
//!    Escape (dismiss), plus mouse hover + click to select a row.
//!
//! The panel paints in [`Widget::draw_overlay`] so it composites above the
//! page surface regardless of sibling order — the same overlay contract
//! the toolkit's `Dropdown` uses. `draw` itself is a no-op.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Interaction, ListRowFace, Widget};

use crate::history::HistoryEntry;

/// Address-bar suggestion dropdown. See the module docs.
pub struct AddressSuggestions {
    /// Ranked suggestions, best first (as returned by `History::suggest`).
    items: Vec<HistoryEntry>,
    /// Index of the highlighted row. Always valid when `items` is
    /// non-empty (clamped on every mutation); meaningless when empty.
    selected: usize,
    /// Row under the pointer, for hover highlight. `None` off the panel.
    hovered: Option<usize>,
    /// Entry the user committed via Enter/click, drained by
    /// [`Self::take_chosen`].
    chosen: Option<HistoryEntry>,
    /// Panel rect in logical coordinates.
    rect: Rect,
    /// Caller-supplied top-left anchor (under the address bar). When set
    /// it overrides taffy's assigned position, like `ContextMenu`.
    anchor: Option<Point>,
    /// Panel width in logical pixels.
    width: f32,
    /// Last cursor position (mouse-move carries it; mouse-press doesn't).
    last_cursor: Point,
    /// The theme the rows are measured in. The overlay is painted outside the
    /// normal widget tree, so it keeps the theme to give its rows their
    /// matching hit targets at every text scale.
    theme: Theme,
    /// Height of one two-line suggestion row in `theme`.
    row_h: f64,
}

impl Default for AddressSuggestions {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSuggestions {
    /// An empty, hidden suggestion overlay.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            hovered: None,
            chosen: None,
            rect: Rect::ZERO,
            anchor: None,
            width: Theme::dark().tokens.metrics.panel_width_sheet,
            last_cursor: Point::ZERO,
            row_h: measured_row_height(&Theme::dark()),
            theme: Theme::dark(),
        }
    }

    /// Replace the suggestion list. Resets the highlight to the first row
    /// (the best-ranked suggestion) and clears any stale hover.
    pub fn set_suggestions(&mut self, items: Vec<HistoryEntry>) {
        self.items = items;
        self.selected = 0;
        self.hovered = None;
    }

    /// Clear all suggestions — the overlay becomes hidden.
    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.hovered = None;
    }

    /// Whether the overlay currently has anything to show.
    pub fn is_visible(&self) -> bool {
        !self.items.is_empty()
    }

    /// The suggestions currently displayed, best first.
    pub fn items(&self) -> &[HistoryEntry] {
        &self.items
    }

    /// Index of the highlighted row, or `None` when empty.
    pub fn selected_index(&self) -> Option<usize> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.selected)
        }
    }

    /// The highlighted entry, or `None` when there are no suggestions.
    /// The chrome reads this on Enter to navigate to the picked page.
    pub fn selected(&self) -> Option<&HistoryEntry> {
        self.items.get(self.selected)
    }

    /// Move the highlight by `delta` rows, wrapping at both ends (Down
    /// from the last row lands on the first, Up from the first on the
    /// last). No-op when there are no suggestions.
    pub fn move_selection(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as i32;
        let idx = (self.selected as i32 + delta).rem_euclid(n);
        self.selected = idx as usize;
    }

    /// Set the top-left anchor (usually the address bar's bottom-left) and
    /// the panel width, so the overlay renders flush under the input.
    pub fn set_anchor(&mut self, top_left: Point, width: f32) {
        self.anchor = Some(top_left);
        if width > 0.0 {
            self.width = width;
        }
        // Recompute the panel rect now: as a floating field (not in the
        // widget tree) this overlay's `apply_layout` never runs, so this is
        // the only place `self.rect` gets set from the anchor.
        self.relayout(top_left);
    }

    /// Measure the rows in the live theme. Rows are as tall as their text,
    /// so pointer hit testing follows them at every text scale.
    pub fn set_theme(&mut self, theme: &Theme) {
        if self.theme.text_scale.to_bits() == theme.text_scale.to_bits()
            && self.theme.ui_scale.to_bits() == theme.ui_scale.to_bits()
        {
            return;
        }
        self.theme = theme.clone();
        self.row_h = measured_row_height(theme);
        if let Some(anchor) = self.anchor {
            self.relayout(anchor);
        }
    }

    /// Take the entry the user committed via Enter or a row click, if any.
    /// Returns `Some` exactly once per commit, then `None`.
    pub fn take_chosen(&mut self) -> Option<HistoryEntry> {
        self.chosen.take()
    }

    /// Route an accessibility action addressed to a virtual suggestion row.
    ///
    /// The overlay is painted above Browser's retained content tree, so the
    /// Browser root owns the final dispatch. Keeping target resolution here
    /// makes that bridge use the same stable URL hash that this widget exports
    /// to AT-SPI, rather than trusting a stale visible-row index.
    pub fn perform_accessibility_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
    ) -> bool {
        let Some(hash) = target.strip_prefix("browser.address-suggestion:") else {
            return false;
        };
        let Some(index) = self
            .items
            .iter()
            .position(|entry| blake3::hash(entry.url.as_bytes()).to_hex().as_str() == hash)
        else {
            return false;
        };
        match action {
            ui::a11y::Action::Focus => {
                self.selected = index;
                true
            }
            ui::a11y::Action::Activate => {
                self.selected = index;
                self.chosen = self.items.get(index).cloned();
                true
            }
            _ => false,
        }
    }

    /// Inset of the rows inside the panel.
    fn pad(&self) -> f64 {
        f64::from(self.theme.tokens.spacing.s2)
    }

    /// Total panel height for the current item count (0 when hidden).
    fn panel_height(&self) -> f64 {
        if self.items.is_empty() {
            0.0
        } else {
            self.pad() * 2.0 + self.items.len() as f64 * self.row_height()
        }
    }

    fn row_height(&self) -> f64 {
        self.row_h
    }

    /// Recompute `self.rect` from the anchor (or a fallback origin) and the
    /// current item count.
    fn relayout(&mut self, origin: Point) {
        let top_left = self.anchor.unwrap_or(origin);
        self.rect = Rect::new(
            top_left.x,
            top_left.y,
            top_left.x + f64::from(self.width),
            top_left.y + self.panel_height(),
        );
    }

    /// Row index under a logical `point`, if it lands on a row.
    fn row_at(&self, point: Point) -> Option<usize> {
        if self.items.is_empty() || !self.rect.contains(point) {
            return None;
        }
        let rel = point.y - (self.rect.y0 + self.pad());
        if rel < 0.0 {
            return None;
        }
        let idx = (rel / self.row_height()) as usize;
        (idx < self.items.len()).then_some(idx)
    }

    /// Paint the panel + rows. Shared by `draw_overlay` (the real path)
    /// and the render test.
    fn render(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.items.is_empty() {
            return;
        }
        // A floating panel: its own material and shadow, its rows inked and
        // lifted for that ground.
        ui::widget::floating_surface::paint(scene, transform, self.rect, theme);
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, &on_panel);
        let pad = self.pad();
        for (idx, entry) in self.items.iter().enumerate() {
            let row_top = self.rect.y0 + pad + idx as f64 * self.row_height();
            let rect = Rect::new(
                self.rect.x0 + pad,
                row_top,
                self.rect.x1 - pad,
                row_top + self.row_height(),
            );
            // Fall back to the URL as the title when a page has none yet.
            let title = if entry.title.trim().is_empty() {
                entry.url.as_str()
            } else {
                entry.title.as_str()
            };
            ListRowFace::new(title)
                .secondary(&entry.url)
                .selected(idx == self.selected)
                .interaction(Interaction {
                    hovered: self.hovered == Some(idx),
                    ..Interaction::default()
                })
                .paint(&mut ctx, rect);
        }
    }
}

/// The height of one two-line suggestion row in `theme`.
fn measured_row_height(theme: &Theme) -> f64 {
    ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        ListRowFace::new("Title")
            .secondary("https://example.test")
            .height(text, theme)
    })
}

impl Widget for AddressSuggestions {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see
        // `ui::widget::TaffyTreeExt`'s doc comment), and `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so there is
        // nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.width),
                    height: Dimension::length(self.panel_height() as f32),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout entry for our own node would be a taffy bug, not a
        // recoverable app condition — skip this frame's rect update rather
        // than panic; the next frame's layout pass will have one.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let taffy_origin = Point::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
        );
        self.relayout(taffy_origin);
    }

    /// The panel is a floating overlay — nothing paints in the normal
    /// pass; the real drawing happens in [`Widget::draw_overlay`] so it
    /// composites above the page surface.
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.set_theme(theme);
        self.render(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.is_visible() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self.row_at(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.row_at(self.last_cursor) {
                    self.selected = idx;
                    self.chosen = self.items.get(idx).cloned();
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                match &key.logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_selection(1);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_selection(-1);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.chosen = self.selected().cloned();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.clear();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "AddressSuggestions"
    }

    fn test_id(&self) -> Option<&str> {
        Some("browser.address_suggestions")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let children = self
            .items
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let title = if entry.title.trim().is_empty() {
                    entry.url.as_str()
                } else {
                    entry.title.as_str()
                };
                let row_top = self.rect.y0 + self.pad() + index as f64 * self.row_height();
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!(
                        "browser.address-suggestion:{}",
                        blake3::hash(entry.url.as_bytes()).to_hex()
                    ))
                    .with_name(title)
                    .with_description(entry.url.clone())
                    .with_bounds(Rect::new(
                        self.rect.x0,
                        row_top,
                        self.rect.x1,
                        row_top + self.row_height(),
                    ))
                    .with_position(index.saturating_add(1), self.items.len())
                    .with_state(AccessState {
                        focusable: true,
                        focused: index == self.selected,
                        selected: index == self.selected,
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name("Address suggestions")
                .with_bounds(self.rect)
                .with_state(AccessState {
                    hidden: self.items.is_empty(),
                    expanded: Some(!self.items.is_empty()),
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else {
            return None;
        };
        (*index < self.items.len()).then(|| {
            let top = self.rect.y0 + self.pad() + *index as f64 * self.row_height();
            Rect::new(self.rect.x0, top, self.rect.x1, top + self.row_height())
        })
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.items.len() {
            return false;
        }
        self.selected = *index;
        true
    }

    /// While the overlay is up it behaves like a transient popover anchored
    /// over the page, so it reports its rect as a modal capture — matching
    /// the toolkit's other dropdown-style overlays.
    fn modal_capture(&self) -> Option<Rect> {
        if self.is_visible() {
            Some(self.rect)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn entry(url: &str, title: &str) -> HistoryEntry {
        HistoryEntry {
            url: url.to_string(),
            title: title.to_string(),
            last_visit: 0,
            visit_count: 1,
        }
    }

    fn three() -> Vec<HistoryEntry> {
        vec![
            entry("https://a.test", "Alpha"),
            entry("https://b.test", "Beta"),
            entry("https://c.test", "Gamma"),
        ]
    }

    fn lay_out(w: &mut AddressSuggestions) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(800.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    /// Selection model of the suggestion overlay.
    #[test]
    fn suggestion_selection_highlights_wraps_and_resets() {
        // Phase `empty_overlay_is_hidden_with_no_selection`.
        {
            let w = AddressSuggestions::new();
            assert!(!w.is_visible());
            assert_eq!(w.selected(), None);
            assert_eq!(w.selected_index(), None);
        }

        // Phase `set_suggestions_highlights_first_and_move_wraps`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            assert!(w.is_visible());
            assert_eq!(
                w.selected().unwrap().url,
                "https://a.test",
                "starts on the best row"
            );

            w.move_selection(1);
            assert_eq!(w.selected().unwrap().url, "https://b.test");
            w.move_selection(1);
            assert_eq!(w.selected().unwrap().url, "https://c.test");
            // Down from the last row wraps to the first.
            w.move_selection(1);
            assert_eq!(w.selected().unwrap().url, "https://a.test");
            // Up from the first row wraps to the last.
            w.move_selection(-1);
            assert_eq!(w.selected().unwrap().url, "https://c.test");
        }

        // Phase `set_suggestions_resets_selection`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            w.move_selection(2); // now on row 2
            w.set_suggestions(three()); // fresh list
            assert_eq!(w.selected_index(), Some(0), "a new list resets to the top");
        }

        // Phase `move_selection_on_empty_is_noop`.
        {
            let mut w = AddressSuggestions::new();
            w.move_selection(1);
            assert_eq!(w.selected(), None);
        }
    }

    /// Rendering, sizing and accessibility of suggestion rows.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn suggestion_rows_render_and_expose_stable_accessible_targets() {
        // Phase `accessibility_target_uses_the_stable_row_identity`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            let target = format!(
                "browser.address-suggestion:{}",
                blake3::hash(b"https://c.test").to_hex()
            );

            assert!(w.perform_accessibility_action(&target, &ui::a11y::Action::Focus));
            assert_eq!(
                w.selected().map(|entry| entry.url.as_str()),
                Some("https://c.test")
            );
            assert!(w.perform_accessibility_action(&target, &ui::a11y::Action::Activate));
            assert_eq!(
                w.take_chosen().map(|entry| entry.url),
                Some("https://c.test".into())
            );
            assert!(
                !w.perform_accessibility_action(
                    "browser.address-suggestion:stale",
                    &ui::a11y::Action::Activate
                ),
                "a stale AT-SPI row must never select a different suggestion"
            );
        }

        // Phase `enlarged_text_expands_rows_and_pointer_hit_targets_together`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            w.set_anchor(Point::new(20.0, 40.0), 320.0);
            let normal_height = w.rect.height();

            w.set_theme(&Theme::dark().with_text_scale(1.5));
            assert!(w.rect.height() > normal_height);
            assert_eq!(
                w.row_at(Point::new(40.0, w.rect.y0 + w.pad() + w.row_height() + 1.0)),
                Some(1),
                "hit testing follows the larger visual row"
            );
        }

        // Phase `suggestions_expose_stable_selectable_screen_reader_rows`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            w.set_anchor(Point::new(20.0, 40.0), 320.0);
            let node = Widget::accessibility(&w).unwrap();
            assert_eq!(node.children.len(), 3);
            assert!(node.children[0].state.selected);
            assert!(
                node.children[0]
                    .action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with("browser.address-suggestion:"))
            );
            assert!(w.focus_synthetic_accessibility(&[2]));
            assert_eq!(w.selected_index(), Some(2));
            assert!(w.synthetic_accessibility_rect(&[2]).is_some());
        }

        // Phase `render_emits_when_visible_and_nothing_when_hidden`.
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();

            // Hidden: no draw ops.
            let mut hidden = AddressSuggestions::new();
            lay_out(&mut hidden);
            let mut empty_scene = Scene::new();
            hidden.render(&mut empty_scene, Affine::IDENTITY, &mut tr, &theme);
            assert_eq!(
                empty_scene.encoding().draw_tags.len(),
                0,
                "a hidden overlay paints nothing"
            );

            // Visible: the panel + rows produce draw ops.
            let mut shown = AddressSuggestions::new();
            shown.set_suggestions(three());
            lay_out(&mut shown);
            let mut scene = Scene::new();
            shown.render(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert!(
                !scene.encoding().draw_tags.is_empty(),
                "a visible overlay paints the panel and rows"
            );
        }
    }

    /// Keyboard, Escape and pointer input on the overlay.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn keyboard_escape_and_mouse_choose_or_dismiss_suggestions() {
        // Phase `keyboard_moves_selection_and_enter_chooses`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());

            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mods = winit::event::Modifiers::default();

            let mut send = |w: &mut AddressSuggestions, named: NamedKey| -> bool {
                let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
                ev.logical_key = Key::Named(named);
                ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                );
                ev.location = winit::keyboard::KeyLocation::Standard;
                ev.state = ElementState::Pressed;
                ev.repeat = false;
                ev.text = None;
                let event = WindowEvent::KeyboardInput {
                    device_id: unsafe { std::mem::zeroed() },
                    event: ev,
                    is_synthetic: false,
                };
                let mut handled = false;
                let mut focus = None;
                w.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                handled
            };

            assert!(send(&mut w, NamedKey::ArrowDown));
            assert_eq!(
                w.selected().unwrap().url,
                "https://b.test",
                "Down advances the highlight"
            );
            assert!(send(&mut w, NamedKey::Enter));
            let chosen = w
                .take_chosen()
                .expect("Enter commits the highlighted entry");
            assert_eq!(chosen.url, "https://b.test");
            assert_eq!(w.take_chosen(), None, "the choice drains exactly once");
        }

        // Phase `escape_dismisses`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mods = winit::event::Modifiers::default();

            let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
            ev.logical_key = Key::Named(NamedKey::Escape);
            ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            );
            ev.location = winit::keyboard::KeyLocation::Standard;
            ev.state = ElementState::Pressed;
            ev.repeat = false;
            ev.text = None;
            let event = WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: ev,
                is_synthetic: false,
            };
            let mut handled = false;
            let mut focus = None;
            w.handle_event(
                &event,
                &mut ui::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(handled);
            assert!(!w.is_visible(), "Escape clears the overlay");
        }

        // Phase `mouse_click_selects_and_chooses_row`.
        {
            let mut w = AddressSuggestions::new();
            w.set_suggestions(three());
            w.set_anchor(Point::new(50.0, 100.0), 400.0);
            lay_out(&mut w);

            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mods = winit::event::Modifiers::default();

            // Aim at the centre of the second row (idx 1).
            let r = w.get_rect();
            let row1_y = r.y0 + w.pad() + w.row_height() * 1.5;
            let mut dispatch = |w: &mut AddressSuggestions, ev: WindowEvent| -> bool {
                let mut handled = false;
                let mut focus = None;
                w.handle_event(
                    &ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                handled
            };
            dispatch(
                &mut w,
                WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: winit::dpi::PhysicalPosition::new(r.x0 + 20.0, row1_y),
                },
            );
            let handled = dispatch(
                &mut w,
                WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
            );
            assert!(handled, "a click on a row is consumed");
            assert_eq!(
                w.selected_index(),
                Some(1),
                "the clicked row becomes selected"
            );
            assert_eq!(w.take_chosen().unwrap().url, "https://b.test");
        }
    }
}

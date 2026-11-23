//! `AddressSuggestions`: the address field's suggestions, when the window draws its
//! own field (outside a Tilo session; in one the window bar lists them, so this list
//! stays empty and there is only ever one).
//!
//! Both are the design system's [`SuggestionList`]: a floating panel under the field,
//! as wide as it, each row the kind's icon, its title and the line under it. The
//! chrome owns the field's keys and calls [`AddressSuggestions::move_selection`] on
//! Up and Down; the widget takes Enter, Escape, the pointer and touch. The panel
//! paints in [`Widget::draw_overlay`], above the page whatever the sibling order.

use std::collections::HashMap;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::i18n::ArgValue;
use ui::widget::TaffyTreeExt;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Suggestion, SuggestionList, Widget};

use crate::omnibox::{Row, RowKind};

pub struct AddressSuggestions {
    items: Vec<Row>,
    list: SuggestionList,
    engine: String,
    /// The row the person committed, drained by [`Self::take_chosen`].
    chosen: Option<Row>,
    last_cursor: Point,
    touched: Option<(u64, usize)>,

    /// Input or a new document value changed what this widget paints.
    dirty: bool,
}

impl Default for AddressSuggestions {
    fn default() -> Self {
        Self::new()
    }
}

impl AddressSuggestions {
    pub fn new() -> Self {
        Self {
            dirty: true,
            items: Vec::new(),
            list: SuggestionList::new(),
            engine: String::new(),
            chosen: None,
            last_cursor: Point::ZERO,
            touched: None,
        }
    }

    /// Replace the list; nothing is chosen in a new one.
    pub fn set_suggestions(&mut self, items: Vec<Row>) {
        let rows = items
            .iter()
            .map(|row| list_row(row, &self.engine))
            .collect();
        self.items = items;
        self.list.set_rows(rows);
        self.list.set_highlight(None);
        self.touched = None;
    }

    pub fn set_search_engine(&mut self, name: &str) {
        if self.engine != name {
            name.clone_into(&mut self.engine);
            let items = std::mem::take(&mut self.items);
            self.set_suggestions(items);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.list.clear();
        self.touched = None;
    }

    pub fn is_visible(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn items(&self) -> &[Row] {
        &self.items
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.list.highlight()
    }

    pub fn selected(&self) -> Option<&Row> {
        self.selected_index()
            .and_then(|index| self.items.get(index))
    }

    /// Choose the next row (`delta > 0`) or the previous one, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        if delta != 0 {
            self.list.move_highlight(delta > 0);
        }
    }

    /// Hang the list under `field`, as wide as it, inside `window`.
    pub fn set_anchor(&mut self, field: Rect, window: Rect) {
        self.list.set_anchor(field);
        if window.area() > 0.0 {
            self.list.set_viewport(window);
        }
    }

    /// Take the row the person committed, once per commit.
    pub fn take_chosen(&mut self) -> Option<Row> {
        self.chosen.take()
    }

    fn choose(&mut self, index: usize) -> bool {
        let Some(row) = self.items.get(index).cloned() else {
            return false;
        };
        self.list.set_highlight(Some(index));
        self.chosen = Some(row);
        true
    }

    /// Route an accessibility action addressed to a virtual suggestion row.
    ///
    /// The list paints above Browser's retained tree, so the Browser root owns the
    /// final dispatch; resolving the target here uses the hash this widget exports to
    /// AT-SPI rather than a stale visible-row index.
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
            .position(|entry| blake3::hash(row_key(entry).as_bytes()).to_hex().as_str() == hash)
        else {
            return false;
        };
        match action {
            ui::a11y::Action::Focus => {
                self.list.set_highlight(Some(index));
                true
            }
            ui::a11y::Action::Activate => self.choose(index),
            _ => false,
        }
    }

    fn panel(&self) -> Option<Rect> {
        if self.items.is_empty() {
            None
        } else {
            self.list.rect()
        }
    }

    fn row_at(&self, point: Point) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        self.list.row_at(point)
    }

    fn over(&self, point: Point) -> bool {
        self.panel().is_some_and(|panel| panel.contains(point))
    }
}

/// One omnibox row as the list shows it: the kind's icon, the title, and the line
/// under it where that says something the title does not.
fn list_row(row: &Row, engine: &str) -> Suggestion {
    use ui::icons::Icon;
    let (icon, subtitle) = match row.kind {
        RowKind::Go => (Icon::Globe, None),
        RowKind::History => (Icon::History, Some(row.detail.clone())),
        // The star is the toolbar's mark for a saved page; the line names that source
        // and the address it opens.
        RowKind::Bookmark => (Icon::Star, Some(bookmark_detail(row))),
        RowKind::Search => (
            Icon::Search,
            (!engine.is_empty()).then(|| {
                crate::resource::CATALOG.t_args(
                    "browser-suggestion-search",
                    &[("engine", ui::i18n::ArgValue::Text(engine.to_owned()))],
                )
            }),
        ),
        RowKind::Suggestion => (Icon::Search, None),
        RowKind::Extension => (Icon::Box, Some(row.detail.clone())),
    };
    let suggestion = Suggestion::labelled(row.url.clone(), row.title.clone()).with_icon(icon);
    match subtitle.filter(|line| !line.is_empty()) {
        Some(line) => suggestion.with_subtitle(line),
        None => suggestion,
    }
}

/// What marks a saved page's row as one, in the person's language: its source and the
/// address it opens.
fn bookmark_detail(row: &Row) -> String {
    crate::resource::CATALOG.t_args(
        "browser-suggestion-bookmark",
        &[("url", ui::i18n::ArgValue::Text(row.detail.clone()))],
    )
}

/// A row's identity across relayouts: what it is and what it opens.
fn row_key(row: &Row) -> String {
    match row.kind {
        RowKind::History | RowKind::Bookmark => row.url.clone(),
        kind => format!("{kind:?}:{}", row.url),
    }
}

fn describe(row: &Row) -> String {
    match row.kind {
        RowKind::History | RowKind::Extension => row.detail.clone(),
        RowKind::Bookmark => bookmark_detail(row),
        RowKind::Go => crate::CATALOG.t_args(
            "browser-open",
            &[("address", ArgValue::Text(row.detail.clone()))],
        ),
        RowKind::Search | RowKind::Suggestion => crate::CATALOG.t_args(
            "browser-search-for",
            &[("query", ArgValue::Text(row.title.clone()))],
        ),
    }
}

impl Widget for AddressSuggestions {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The list floats over the page; it takes no room in the layout.
        let style = Style {
            size: Size::from_lengths(0.0, 0.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if !self.items.is_empty() {
            self.list.paint(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
        if !self.is_visible() {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                self.list.hover_at(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(idx) = self.row_at(self.last_cursor) {
                    self.choose(idx);
                    *ctx.handled = true;
                } else if self.over(self.last_cursor) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let p = ctx.logical(touch.location);
                match touch.phase {
                    TouchPhase::Started => {
                        if let Some(index) = self.row_at(p) {
                            self.touched = Some((touch.id, index));
                            self.list.set_highlight(Some(index));
                            *ctx.handled = true;
                        } else if self.over(p) {
                            *ctx.handled = true;
                        }
                    }
                    TouchPhase::Moved => {
                        if self.touched.is_some_and(|(id, _)| id == touch.id) {
                            *ctx.handled = true;
                        }
                    }
                    TouchPhase::Ended => {
                        if let Some((id, index)) = self.touched.take() {
                            if id == touch.id {
                                // A tap lifts where it went down; a drag off the row
                                // lets it go.
                                if self.row_at(p) == Some(index) {
                                    self.choose(index);
                                } else {
                                    self.list.set_highlight(None);
                                }
                                *ctx.handled = true;
                            } else {
                                self.touched = Some((id, index));
                            }
                        }
                    }
                    TouchPhase::Cancelled => {
                        if self.touched.take().is_some() {
                            self.list.set_highlight(None);
                        }
                    }
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
                        if let Some(index) = self.selected_index() {
                            self.choose(index);
                            *ctx.handled = true;
                        }
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

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "AddressSuggestions"
    }

    fn test_id(&self) -> Option<&str> {
        Some("browser.address_suggestions")
    }

    fn get_rect(&self) -> Rect {
        self.panel().unwrap_or(Rect::ZERO)
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        (0..self.items.len())
            .filter_map(|index| {
                self.list
                    .row_rect(index)
                    .map(|rect| (format!("browser.address-suggestion:{index}"), rect))
            })
            .collect()
    }

    fn accessibility_hidden(&self) -> bool {
        self.items.is_empty()
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        let children = self
            .items
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut node = AccessNode::new(Role::ListItem)
                    .with_action_target(format!(
                        "browser.address-suggestion:{}",
                        blake3::hash(row_key(entry).as_bytes()).to_hex()
                    ))
                    .with_name(entry.title.as_str())
                    .with_description(describe(entry))
                    .with_position(index.saturating_add(1), self.items.len())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.selected_index() == Some(index))
                            .selected(self.selected_index() == Some(index)),
                    );
                if let Some(rect) = self.list.row_rect(index) {
                    node = node.with_bounds(rect);
                }
                node
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name(crate::resource::CATALOG.t("browser-address-suggestions"))
                .with_bounds(self.get_rect())
                .with_state(AccessState {
                    hidden: self.accessibility_hidden(),
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
        self.list.row_rect(*index)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        if *index >= self.items.len() {
            return false;
        }
        self.list.set_highlight(Some(*index));
        true
    }

    /// While the list is up it reports its rect as a modal capture, like the
    /// toolkit's other dropdown overlays.
    fn modal_capture(&self) -> Option<Rect> {
        self.panel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, title: &str) -> Row {
        Row {
            kind: RowKind::History,
            title: title.to_string(),
            detail: url.trim_start_matches("https://").to_string(),
            url: url.to_string(),
        }
    }

    fn three() -> Vec<Row> {
        vec![
            entry("https://a.test", "Alpha"),
            entry("https://b.test", "Beta"),
            entry("https://c.test", "Gamma"),
        ]
    }

    const FIELD: Rect = Rect::new(50.0, 60.0, 450.0, 92.0);
    const WINDOW: Rect = Rect::new(0.0, 0.0, 900.0, 700.0);

    fn shown() -> AddressSuggestions {
        let mut w = AddressSuggestions::new();
        w.set_suggestions(three());
        w.set_anchor(FIELD, WINDOW);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        w.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        w
    }

    #[test]
    fn suggestion_selection_starts_unchosen_wraps_and_resets() {
        let w = AddressSuggestions::new();
        assert!(!w.is_visible());
        assert_eq!(w.selected(), None);

        let mut w = AddressSuggestions::new();
        w.set_suggestions(three());
        assert!(w.is_visible());
        assert_eq!(
            w.selected(),
            None,
            "Enter opens what was typed until a row is chosen"
        );
        w.move_selection(1);
        assert_eq!(w.selected().unwrap().url, "https://a.test");
        w.move_selection(1);
        w.move_selection(1);
        assert_eq!(w.selected().unwrap().url, "https://c.test");
        w.move_selection(1);
        assert_eq!(
            w.selected().unwrap().url,
            "https://a.test",
            "wraps to the first"
        );
        w.move_selection(-1);
        assert_eq!(
            w.selected().unwrap().url,
            "https://c.test",
            "and to the last"
        );
        w.set_suggestions(three());
        assert_eq!(w.selected_index(), None, "a new list chooses nothing");

        let mut empty = AddressSuggestions::new();
        empty.move_selection(1);
        assert_eq!(empty.selected(), None);
    }

    #[test]
    fn suggestions_hang_under_the_field_and_expose_stable_rows() {
        let w = shown();
        let panel = w.get_rect();
        assert!(panel.y0 >= FIELD.y1, "under the field: {panel:?}");
        assert!(
            (panel.x0 - FIELD.x0).abs() < 1.0 && panel.width() >= FIELD.width() - 1.0,
            "as wide as the field: {panel:?}"
        );
        let rows = w.test_rects();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].1.y1 <= pair[1].1.y0 + 0.5)
        );

        let node = Widget::accessibility(&w).unwrap();
        assert_eq!(node.children.len(), 3);
        assert!(
            node.children[0]
                .action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("browser.address-suggestion:"))
        );

        let mut hidden = AddressSuggestions::new();
        hidden.set_anchor(FIELD, WINDOW);
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        hidden.draw_overlay(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
        assert_eq!(
            scene.encoding().draw_tags.len(),
            0,
            "a hidden list paints nothing"
        );
        assert!(hidden.modal_capture().is_none());
    }

    #[test]
    fn accessibility_target_uses_the_stable_row_identity() {
        let mut w = shown();
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
        assert!(w.focus_synthetic_accessibility(&[2]));
        assert_eq!(w.selected_index(), Some(2));
        assert!(w.synthetic_accessibility_rect(&[2]).is_some());
    }

    #[test]
    fn keyboard_pointer_and_touch_choose_or_dismiss_suggestions() {
        let send = |w: &mut AddressSuggestions, named: NamedKey| -> bool {
            let ev = ui::synthetic_input::key_event(Key::Named(named), None, ElementState::Pressed);
            ui::testing::dispatch(w, &ui::synthetic_input::keyboard_input(ev))
        };

        let mut w = shown();
        assert!(!send(&mut w, NamedKey::Enter), "Enter goes on to the field");
        assert_eq!(w.take_chosen(), None);
        assert!(send(&mut w, NamedKey::ArrowDown));
        assert!(send(&mut w, NamedKey::ArrowDown));
        assert!(send(&mut w, NamedKey::Enter));
        assert_eq!(w.take_chosen().unwrap().url, "https://b.test");
        assert_eq!(w.take_chosen(), None, "the choice drains exactly once");
        assert!(send(&mut w, NamedKey::Escape));
        assert!(!w.is_visible());

        let mut w = shown();
        let (_, row) = w.test_rects()[1].clone();
        ui::testing::dispatch(
            &mut w,
            &ui::synthetic_input::cursor_moved(row.center().x, row.center().y),
        );
        assert!(ui::testing::dispatch(
            &mut w,
            &ui::synthetic_input::mouse_press()
        ));
        assert_eq!(w.take_chosen().unwrap().url, "https://b.test");

        let mut w = shown();
        let (_, row) = w.test_rects()[2].clone();
        let touch = |phase, p: Point| {
            WindowEvent::Touch(winit::event::Touch {
                device_id: winit::event::DeviceId::dummy(),
                phase,
                location: winit::dpi::PhysicalPosition::new(p.x, p.y),
                force: None,
                id: 7,
            })
        };
        assert!(ui::testing::dispatch(
            &mut w,
            &touch(TouchPhase::Started, row.center())
        ));
        assert!(ui::testing::dispatch(
            &mut w,
            &touch(TouchPhase::Ended, row.center())
        ));
        assert_eq!(w.take_chosen().unwrap().url, "https://c.test");
        assert!(ui::testing::dispatch(
            &mut w,
            &touch(TouchPhase::Started, row.center())
        ));
        ui::testing::dispatch(&mut w, &touch(TouchPhase::Ended, Point::new(5.0, 5.0)));
        assert_eq!(w.take_chosen(), None, "lifting off the row chooses nothing");
    }
}

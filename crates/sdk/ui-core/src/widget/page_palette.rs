//! `PageJumpPalette` — Cmd-K / Ctrl-K fullscreen-ish page jump
//! affordance used by the notes app.
//!
//! Conceptually a thinner cousin of `crate::widget::CommandPalette`
//! — the `CommandPalette` has a concept-locked filter strip + footer
//! row that doesn't fit the notes app's "just jump to a page" mental
//! model. `PageJumpPalette` ships only the input row + the filtered
//! result list, sized to look like a fullscreen-ish dialog when
//! anchored at a generous offset from the window's top-center.

use crate::widget::ModelPath;
use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::TextRenderer;
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::link_popover::PageEntry;

const WIDTH: f64 = 480.0;
const ROW_HEIGHT: f64 = 36.0;
const INPUT_HEIGHT: f64 = 44.0;
const PADDING: f64 = 12.0;
const MAX_VISIBLE_ROWS: usize = 10;

/// Narrows a visible-row count/slot index (always `<= MAX_VISIBLE_ROWS`)
/// to `f64` for layout math. Bounded far below `f64`'s precision limit,
/// so this never loses precision in practice.
#[allow(
    clippy::cast_precision_loss,
    reason = "row counts/slots are bounded to MAX_VISIBLE_ROWS (10)"
)]
fn row_f64(v: usize) -> f64 {
    v as f64
}

/// Composes `base` with a translation for text placement. Affine
/// composition is floating-point matrix multiply — it cannot overflow or
/// panic the way integer arithmetic can.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine `*` composes floating-point transform matrices, not integer arithmetic"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

pub struct PageJumpPalette {
    items: Vec<PageEntry>,
    navigation_items: Vec<PageEntry>,
    provider: Option<Box<dyn crate::search::SearchProvider>>,
    search_cursor: Option<crate::search::SearchCursor>,
    addresses: HashMap<Uuid, tilo_actions::ActionAddress>,
    subtitles: HashMap<Uuid, String>,
    pending_address: Option<tilo_actions::ActionAddress>,
    viewport: Option<Rect>,
    pointer: Point,
    visible: Vec<usize>,
    cursor: usize,
    query: String,
    query_selection: Selection,
    query_composition: QueryComposition,
    visible_flag: bool,
    /// Top-left anchor in logical coords. Set by the host when
    /// opening the palette — typically `(window_width/2 - WIDTH/2,
    /// 80.0)` for a fullscreen-ish appearance.
    anchor: Point,
    /// Cached from the current theme so the card's row bands grow with its
    /// shaped query text and result labels.
    text_scale: f64,
    pending: Option<PageEntry>,
}

impl Default for PageJumpPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl PageJumpPalette {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            navigation_items: Vec::new(),
            provider: None,
            search_cursor: None,
            addresses: HashMap::new(),
            subtitles: HashMap::new(),
            pending_address: None,
            viewport: None,
            pointer: Point::ZERO,
            visible: Vec::new(),
            cursor: 0,
            query: String::new(),
            query_selection: Selection::collapsed(0),
            query_composition: QueryComposition::new(),
            visible_flag: false,
            anchor: Point::ZERO,
            text_scale: 1.0,
            pending: None,
        }
    }

    pub fn set_items(&mut self, items: Vec<PageEntry>) {
        if self.navigation_items == items {
            return;
        }
        self.navigation_items = items.clone();
        self.items = items;
        self.rebuild_visible();
    }

    /// Reuse the system provider; the palette owns only query/view state.
    pub fn set_search_provider(&mut self, provider: Box<dyn crate::search::SearchProvider>) {
        self.provider = Some(provider);
    }

    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = Some(viewport);
    }

    pub fn take_action_selection(&mut self) -> Option<tilo_actions::ActionAddress> {
        self.pending_address.take()
    }

    fn load_search_page(&mut self) {
        let Some(provider) = self.provider.as_ref() else {
            return;
        };
        if self
            .search_cursor
            .as_ref()
            .is_some_and(|cursor| !provider.action_directory_cursor_is_current(cursor))
        {
            self.items.clear();
            self.addresses.clear();
            self.subtitles.clear();
            self.search_cursor = None;
            self.cursor = 0;
        }
        let page = provider.search_page(
            &crate::search::SearchQuery::new(self.query.as_str()),
            self.search_cursor.as_ref(),
            512,
            64,
        );
        self.items.retain(|entry| !entry.id.is_nil());
        for result in page.results {
            let Some(address) = result.action_address() else {
                continue;
            };
            if self.addresses.values().any(|current| current == &address) {
                continue;
            }
            let id = Uuid::new_v4();
            let (title, subtitle) = match result.subtitle {
                Some(note) => (note, Some(result.title)),
                None => (result.title, None),
            };
            if let Some(subtitle) = subtitle {
                self.subtitles.insert(id, subtitle);
            }
            self.addresses.insert(id, address);
            self.items.push(PageEntry { id, title });
        }
        self.search_cursor = page.next;
        if self.search_cursor.is_some() {
            self.items.push(PageEntry {
                id: Uuid::nil(),
                title: "More results…".into(),
            });
        }
        self.visible = (0..self.items.len()).collect();
        self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
    }

    fn activate_highlighted(&mut self) {
        let Some(entry) = self.highlighted().cloned() else {
            return;
        };
        if entry.id.is_nil() && self.search_cursor.is_some() {
            self.load_search_page();
            return;
        }
        if let Some(address) = self.addresses.get(&entry.id) {
            self.pending_address = Some(address.clone());
        } else {
            self.pending = Some(entry);
        }
        self.set_visible(false);
    }

    fn row_height(&self) -> f64 {
        if self.subtitles.is_empty() {
            ROW_HEIGHT
        } else {
            58.0
        }
    }

    fn visible_capacity(&self) -> usize {
        let Some(viewport) = self.viewport else {
            return MAX_VISIBLE_ROWS;
        };
        let available = viewport.height() - 32.0 - (PADDING * 3.0 + INPUT_HEIGHT) * self.text_scale;
        (1..=MAX_VISIBLE_ROWS)
            .rfind(|rows| row_f64(*rows) * self.row_height() * self.text_scale <= available)
            .unwrap_or(1)
    }

    /// Pointer handling is modal, including the dismissing click.
    pub fn handle_pointer(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
                let rect = self.rect();
                let top = rect.y0 + (PADDING * 2.0 + INPUT_HEIGHT) * self.text_scale;
                for slot in 0..self.visible_capacity() {
                    let y = top + row_f64(slot) * self.row_height() * self.text_scale;
                    if Rect::new(rect.x0, y, rect.x1, y + self.row_height() * self.text_scale)
                        .contains(self.pointer)
                    {
                        self.cursor = self
                            .window_start()
                            .saturating_add(slot)
                            .min(self.visible.len().saturating_sub(1));
                        break;
                    }
                }
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if !self.rect().contains(self.pointer) {
                    self.set_visible(false);
                } else if self.pointer.y
                    >= self.rect().y0 + (PADDING * 2.0 + INPUT_HEIGHT) * self.text_scale
                {
                    self.activate_highlighted();
                }
                true
            }
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => true,
            _ => false,
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible_flag = visible;
        if !visible {
            self.query.clear();
            self.query_selection = Selection::collapsed(0);
            self.query_composition.clear();
            self.cursor = 0;
            self.rebuild_visible();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_flag
    }

    pub fn set_anchor(&mut self, p: Point) {
        self.anchor = p;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn visible_len(&self) -> usize {
        self.visible.len()
    }

    pub fn highlighted(&self) -> Option<&PageEntry> {
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.items.get(i))
    }

    pub fn take_selection(&mut self) -> Option<PageEntry> {
        self.pending.take()
    }

    pub fn width(&self) -> f64 {
        self.viewport.map_or(WIDTH, |viewport| {
            WIDTH.min((viewport.width() - 32.0).max(1.0))
        })
    }

    pub fn height(&self) -> f64 {
        let rows = row_f64(self.visible.len().clamp(1, self.visible_capacity()));
        (PADDING * 3.0 + INPUT_HEIGHT + self.row_height() * rows) * self.text_scale
    }

    pub fn rect(&self) -> Rect {
        let anchor = self.viewport.map_or(self.anchor, |viewport| {
            Point::new(
                f64::midpoint(viewport.x0, viewport.x1) - self.width() / 2.0,
                self.anchor
                    .y
                    .min(viewport.y1 - self.height() - 16.0)
                    .max(viewport.y0 + 16.0),
            )
        });
        Rect::new(
            anchor.x,
            anchor.y,
            anchor.x + self.width(),
            anchor.y + self.height(),
        )
    }

    fn rebuild_visible(&mut self) {
        self.addresses.clear();
        self.subtitles.clear();
        self.search_cursor = None;
        if !self.query.trim().is_empty() && self.provider.is_some() {
            self.items.clear();
            self.load_search_page();
            return;
        }
        self.items = self.navigation_items.clone();
        let needle = self.query.to_lowercase();
        self.visible.clear();
        for (i, e) in self.items.iter().enumerate() {
            if needle.is_empty() || e.title.to_lowercase().contains(&needle) {
                self.visible.push(i);
            }
        }
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// First result kept in the painted window. Keyboard movement can reach
    /// every result without letting the highlighted row disappear below the
    /// fixed-height palette.
    fn window_start(&self) -> usize {
        self.cursor
            .saturating_add(1)
            .saturating_sub(self.visible_capacity())
    }

    pub fn handle_key(&mut self, event: &WindowEvent) -> bool {
        self.handle_key_with_modifiers(event, winit::keyboard::ModifiersState::empty())
    }

    pub fn handle_key_with_modifiers(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::keyboard::ModifiersState,
    ) -> bool {
        if !self.visible_flag {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            let change = self.query_composition.handle_event(
                event,
                &mut self.query,
                &mut self.query_selection,
                modifiers.shift_key(),
                modifiers.control_key() || modifiers.super_key(),
            );
            if change == QueryEventChange::Edited {
                self.cursor = 0;
                self.rebuild_visible();
            }
            return change != QueryEventChange::Ignored;
        };
        match logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                if !self.visible.is_empty() {
                    self.cursor = self
                        .cursor
                        .saturating_add(1)
                        .min(self.visible.len().saturating_sub(1));
                }
                true
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            Key::Named(NamedKey::Enter) => {
                self.activate_highlighted();
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.set_visible(false);
                true
            }
            _ => {
                let change = self.query_composition.handle_event(
                    event,
                    &mut self.query,
                    &mut self.query_selection,
                    modifiers.shift_key(),
                    modifiers.control_key() || modifiers.super_key(),
                );
                if change == QueryEventChange::Edited {
                    self.cursor = 0;
                    self.rebuild_visible();
                }
                change != QueryEventChange::Ignored
            }
        }
    }

    pub fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.visible_flag {
            return;
        }
        self.text_scale = f64::from(theme.text_scale);
        let rect = self.rect();
        Self::draw_card_chrome(scene, transform, theme, rect);
        let input_rect = self.draw_input_field(scene, transform, text_renderer, theme, rect);
        self.draw_result_rows(scene, transform, text_renderer, theme, rect, input_rect);
    }

    /// The palette card's background fill and border.
    fn draw_card_chrome(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        let scale = f64::from(theme.text_scale);
        let radius = 12.0 * scale;
        let shape = RoundedRect::from_rect(rect, radius);
        // Panel role (HIG §9.1): panel ground + the derived panel edge as a
        // crisp inset stroke; a declared `border_standard` was invisible
        // against `raised` on the dark preset.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            crate::chrome::panel_ground(theme),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(1.0 * scale),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }

    /// The query input box and its composed text. Returns the input's
    /// `Rect` so the result list can lay out below it.
    fn draw_input_field(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) -> Rect {
        let tokens = &theme.tokens;
        let scale = f64::from(theme.text_scale);
        let input_rect = Rect::new(
            rect.x0 + PADDING * scale,
            rect.y0 + PADDING * scale,
            rect.x1 - PADDING * scale,
            rect.y0 + (PADDING + INPUT_HEIGHT) * scale,
        );
        let input_shape = RoundedRect::from_rect(input_rect, f64::from(theme.tokens.radii.control));
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &input_shape,
        );
        scene.stroke(
            &Stroke::new(1.0 * scale),
            transform,
            tokens.colors.border_focus,
            None,
            &input_shape,
        );

        self.query_composition.draw(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            QueryView {
                text: &self.query,
                selection: &self.query_selection,
                placeholder: "Search notes…",
                show_caret: true,
            },
            Rect::new(
                input_rect.x0 + 12.0 * scale,
                input_rect.y0,
                input_rect.x1 - 12.0 * scale,
                input_rect.y1,
            ),
            &tokens.typography.style(TextRole::Body),
            QueryInk::of(&tokens.colors),
        );
        input_rect
    }

    /// Either the "No notes match" placeholder, or the visible-and-windowed
    /// slice of result rows with the cursor highlight.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "bounded logical row width is a text renderer f32"
    )]
    fn draw_result_rows(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        input_rect: Rect,
    ) {
        let tokens = &theme.tokens;
        let scale = f64::from(theme.text_scale);
        // The armed row is the panel's strong lift, as in every menu.
        let selected_ink = crate::widget::floating_surface::armed_row(theme).1;
        let list_top = input_rect.y1 + PADDING * scale;
        // Result rows use `TextRenderer`, which owns the global accessibility
        // multiplier. Keep the requested size at the unscaled design token so
        // it isn't enlarged twice while the row band scales once.
        let row_size = 14.0;
        if self.visible.is_empty() {
            let label = "No notes match";
            let (_, label_h) = text_renderer.measure(label, row_size, None);
            let y = list_top + (ROW_HEIGHT * scale - f64::from(label_h)) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                row_size,
                tokens.colors.text_dim,
                translated(transform, rect.x0 + (PADDING + 12.0) * scale, y),
                None,
            );
            return;
        }

        for (slot, (visible_index, &idx)) in self
            .visible
            .iter()
            .enumerate()
            .skip(self.window_start())
            .take(self.visible_capacity())
            .enumerate()
        {
            // `visible` only ever holds indices produced by `rebuild_visible`
            // enumerating `self.items`, so this is structurally in range —
            // checked rather than indexed so that invariant can't silently
            // regress into a panic.
            let Some(row) = self.items.get(idx) else {
                continue;
            };
            let y0 = list_top + row_f64(slot) * self.row_height() * scale;
            let row_rect = Rect::new(
                rect.x0 + PADDING * scale,
                y0,
                rect.x1 - PADDING * scale,
                y0 + self.row_height() * scale,
            );
            if visible_index == self.cursor {
                crate::widget::floating_surface::paint_armed_row(
                    scene,
                    transform,
                    row_rect,
                    row_rect.x0 - rect.x0,
                    theme,
                );
            }
            let subtitle = self.subtitles.get(&row.id);
            let (_, title_h) = text_renderer.measure(&row.title, row_size, None);
            let ty = if subtitle.is_some() {
                row_rect.y0 + 7.0 * scale
            } else {
                row_rect.y0 + (row_rect.height() - f64::from(title_h)) / 2.0
            };
            let available = (row_rect.width() - 24.0 * scale).max(0.0) as f32;
            let title = text_renderer.elide_to_width(&row.title, row_size, available);
            text_renderer.draw_text(
                scene,
                &title,
                row_size,
                if visible_index == self.cursor {
                    selected_ink
                } else {
                    tokens.colors.text_primary
                },
                translated(transform, row_rect.x0 + 12.0 * scale, ty),
                None,
            );
            if let Some(subtitle) = subtitle {
                let subtitle = text_renderer.elide_to_width(subtitle, 12.0, available);
                text_renderer.draw_text(
                    scene,
                    &subtitle,
                    12.0,
                    if visible_index == self.cursor {
                        selected_ink
                    } else {
                        tokens.colors.text_dim
                    },
                    translated(
                        transform,
                        row_rect.x0 + 12.0 * scale,
                        row_rect.y0 + 31.0 * scale,
                    ),
                    None,
                );
            }
        }
    }

    /// Page-id getter for tests that don't want to fish through
    /// `take_selection`.
    pub fn highlighted_id(&self) -> Option<Uuid> {
        self.highlighted().map(|e| e.id)
    }
}

impl Widget for PageJumpPalette {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(Style::default(), node_map, self)
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
        "PageJumpPalette"
    }
    fn test_id(&self) -> Option<&str> {
        Some("page_jump_palette")
    }
    fn get_rect(&self) -> Rect {
        self.rect()
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.query_composition.ime_cursor_rect()
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.query_composition
            .ime_edit_fingerprint(&self.query, &self.query_selection, key)
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.visible_flag {
            return Some(
                AccessNode::new(Role::Dialog)
                    .with_name("Jump to note")
                    .with_state(AccessState {
                        hidden: true,
                        modal: true,
                        ..Default::default()
                    }),
            );
        }
        let count = self.visible.len();
        let mut children: Vec<AccessNode> = self
            .visible
            .iter()
            .enumerate()
            .filter_map(|(position, &item_index)| {
                self.items.get(item_index).map(|item| {
                    AccessNode::new(Role::MenuItem)
                        .with_action_target(format!("notes.search:{}", item.id))
                        .with_name(self.subtitles.get(&item.id).map_or_else(
                            || item.title.clone(),
                            |subtitle| format!("{} — {subtitle}", item.title),
                        ))
                        .with_state(AccessState {
                            selected: position == self.cursor,
                            ..Default::default()
                        })
                        .with_position(position.saturating_add(1), count)
                })
            })
            .collect();
        children.insert(
            0,
            AccessNode::new(Role::TextInput)
                .with_name("Search notes")
                .with_action_target("notes.search.query")
                .with_optional_value(Some(self.query.clone()))
                .with_state(AccessState::new().focusable(true)),
        );
        Some(
            AccessNode::new(Role::Dialog)
                .with_name("Jump to note")
                .with_optional_value((!self.query.is_empty()).then(|| self.query.clone()))
                .with_state(AccessState {
                    modal: true,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !self.visible_flag {
            return false;
        }
        if target == "notes.search.query"
            && let crate::a11y::Action::SetValue(value) = action
        {
            self.query.clone_from(value);
            self.query_selection = Selection::collapsed(self.query.len());
            self.query_composition.clear();
            self.cursor = 0;
            self.rebuild_visible();
            return true;
        }
        if matches!(action, crate::a11y::Action::Activate)
            && let Some(id) = target
                .strip_prefix("notes.search:")
                .and_then(|id| Uuid::parse_str(id).ok())
            && let Some(cursor) = self
                .visible
                .iter()
                .position(|index| self.items.get(*index).is_some_and(|entry| entry.id == id))
        {
            self.cursor = cursor;
            self.activate_highlighted();
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;
    use winit::keyboard::SmolStr;

    fn key_press(logical_key: Key, text: Option<&str>) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            text.map(SmolStr::new),
            ElementState::Pressed,
        ))
    }

    fn char_press(c: &str) -> WindowEvent {
        key_press(Key::Character(SmolStr::new(c)), Some(c))
    }

    fn seed_two() -> PageJumpPalette {
        let mut p = PageJumpPalette::new();
        p.set_items(vec![
            PageEntry {
                id: Uuid::from_u128(1),
                title: "Welcome".into(),
            },
            PageEntry {
                id: Uuid::from_u128(2),
                title: "Roadmap".into(),
            },
        ]);
        p
    }

    #[test]
    fn palette_scales_and_stays_legible_inside_the_viewport() {
        // Case `selected_result_ink_meets_text_contrast_floor`.
        {
            for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
                let colors = &theme.tokens.colors;
                let ratio =
                    crate::a11y::contrast_ratio(colors.intent.on_inverted, colors.intent.inverted);
                let floor = crate::tokens::colors::ON_FILL_MIN_CONTRAST;
                assert!(
                    ratio >= floor,
                    "selected result contrast {ratio:.2}:1 is below {floor}:1"
                );
            }
        }

        // Case `high_text_palette_scales_input_and_result_bands`.
        {
            let mut p = seed_two();
            p.text_scale = 2.0;
            assert_eq!(p.height(), 304.0);
            let rect = p.rect();
            assert_eq!(rect.height(), 304.0);
            assert!(ROW_HEIGHT * p.text_scale >= 72.0);
        }

        // Case `narrow_large_text_palette_keeps_all_visible_rows_inside_viewport`.
        {
            let mut p = seed_two();
            p.set_viewport(Rect::new(0.0, 0.0, 390.0, 600.0));
            p.set_anchor(Point::new(-45.0, 80.0));
            p.text_scale = 2.0;
            p.set_items(
                (1..25)
                    .map(|id| PageEntry {
                        id: Uuid::from_u128(id),
                        title: format!("Note {id}"),
                    })
                    .collect(),
            );
            p.set_visible(true);
            for _ in 0..23 {
                p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            }
            let rect = p.rect();
            assert!(rect.x0 >= 16.0 && rect.x1 <= 374.0);
            assert!(rect.y0 >= 16.0 && rect.y1 <= 584.0);
            assert!(p.cursor >= p.window_start());
            assert!(p.cursor < p.window_start().saturating_add(p.visible_capacity()));
        }
    }

    #[test]
    fn palette_query_edits_filter_and_select_all() {
        // Case `new_starts_empty_and_hidden`.
        {
            let p = PageJumpPalette::new();
            assert!(!p.is_visible());
            assert_eq!(p.visible_len(), 0);
        }

        // Case `typing_filters`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            p.handle_key(&char_press("r"));
            assert_eq!(p.query(), "r");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        }

        // Case `typing_replaces_selection_at_the_caret_instead_of_appending`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            p.query = "wxyz".into();
            p.query_selection = Selection {
                anchor: 1,
                cursor: 3,
            };
            assert!(p.handle_key(&char_press("a")));
            assert_eq!(p.query(), "waz");
            assert_eq!(p.query_selection, Selection::collapsed(2));
        }

        // Case `ime_preedit_draws_without_filtering_until_commit`.
        {
            let mut palette = PageJumpPalette::new();
            palette.set_items(vec![
                PageEntry {
                    id: Uuid::from_u128(1),
                    title: "東京".into(),
                },
                PageEntry {
                    id: Uuid::from_u128(2),
                    title: "London".into(),
                },
            ]);
            palette.set_visible(true);
            palette.set_anchor(Point::new(20.0, 20.0));
            assert!(
                palette.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )))
            );
            assert_eq!(palette.query(), "");
            assert_eq!(palette.visible_len(), 2);

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            palette.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(Widget::ime_cursor_rect(&palette).is_some());

            assert!(
                palette.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("東京".into(),)))
            );
            assert_eq!(palette.query(), "東京");
            assert_eq!(palette.visible_len(), 1);
            assert_eq!(palette.highlighted().unwrap().title, "東京");
        }

        // Case `query_command_select_all_replaces_text_without_typing_shortcut`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            p.handle_key(&char_press("welcome"));
            p.handle_key_with_modifiers(&char_press("a"), winit::keyboard::ModifiersState::CONTROL);
            p.handle_key(&char_press("road"));
            assert_eq!(p.query(), "road");
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        }
    }

    #[test]
    fn palette_keyboard_moves_commits_and_escapes() {
        // Case `enter_commits_and_closes`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            let chosen = p.take_selection().unwrap();
            assert_eq!(chosen.title, "Welcome");
            assert!(!p.is_visible());
        }

        // Case `escape_hides_without_committing`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!p.is_visible());
            assert!(p.take_selection().is_none());
        }

        // Case `arrow_keys_move_cursor`.
        {
            let mut p = seed_two();
            p.set_visible(true);
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
        }

        // Case `long_result_lists_keep_the_keyboard_highlight_in_the_painted_window`.
        {
            let mut p = PageJumpPalette::new();
            p.set_items(
                (0..14)
                    .map(|i| PageEntry {
                        id: Uuid::from_u128(i + 1),
                        title: format!("Page {i}"),
                    })
                    .collect(),
            );
            p.set_visible(true);
            for _ in 0..12 {
                p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            }
            assert_eq!(p.cursor, 12);
            assert_eq!(p.window_start(), 3);
            assert!(p.cursor < p.window_start() + MAX_VISIBLE_ROWS);
        }
    }

    #[test]
    fn palette_exposes_query_and_paged_provider_results() {
        // Case `accessibility_exposes_committed_query_and_selected_result`.
        {
            let mut p = seed_two();
            let hidden = Widget::accessibility(&p).unwrap();
            assert!(hidden.state.hidden);
            p.set_visible(true);
            p.handle_key(&char_press("r"));
            let node = Widget::accessibility(&p).unwrap();
            assert_eq!(node.role, crate::a11y::Role::Dialog);
            assert_eq!(node.value.as_deref(), Some("r"));
            assert_eq!(node.children.len(), 2);
            assert_eq!(node.children[0].role, crate::a11y::Role::TextInput);
            assert!(node.children[1].state.selected);
        }

        // Case `provider_results_continue_in_bounded_pages_and_preserve_action_addresses`.
        {
            use crate::search::{SearchCatalog, SearchProvider, SearchResult};
            struct Provider(SearchCatalog);
            impl SearchProvider for Provider {
                fn name(&self) -> &'static str {
                    "Notes"
                }
                fn search(&self, _: &str) -> Vec<SearchResult> {
                    panic!("palette must use bounded pages");
                }
                fn action_directory_cursor_is_current(
                    &self,
                    cursor: &crate::search::SearchCursor,
                ) -> bool {
                    self.0.cursor_is_current(cursor)
                }
                fn search_page(
                    &self,
                    query: &crate::search::SearchQuery,
                    cursor: Option<&crate::search::SearchCursor>,
                    budget: usize,
                    limit: usize,
                ) -> crate::search::SearchPage {
                    assert_eq!((budget, limit), (512, 64));
                    self.0.query_page(query.text(), cursor, budget, limit)
                }
            }
            let catalog = SearchCatalog::new("Notes");
            for index in 0..70 {
                let resource = format!("note:{index:03}");
                let address = tilo_actions::ActionAddress::new(&resource, "open");
                catalog
                    .upsert(
                        &resource,
                        "matching body",
                        SearchResult::for_action("Notes", format!("Body {index}"), address)
                            .with_subtitle("Research"),
                    )
                    .unwrap();
            }
            let mut p = seed_two();
            p.set_search_provider(Box::new(Provider(catalog)));
            p.set_visible(true);
            p.handle_key(&char_press("matching"));
            assert_eq!(p.visible_len(), 65);
            for _ in 0..64 {
                p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            }
            assert_eq!(p.highlighted().unwrap().title, "More results…");
            p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert!(p.is_visible());
            assert_eq!(p.visible_len(), 70);
            p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert_eq!(
                p.take_action_selection(),
                Some(tilo_actions::ActionAddress::new("note:064", "open"))
            );
            assert!(!p.is_visible());
            assert_eq!(
                p.visible_len(),
                2,
                "closing returns to the navigation catalogue"
            );
        }
    }
}

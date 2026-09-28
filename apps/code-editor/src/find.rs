//! In-buffer find and replace: the match model, and the bar that drives it.
//!
//! The bar is a composition of shared toolkit widgets — two
//! [`SearchField`](ui::widget::SearchField)s for the two queries, and
//! [`Button::paint`] faces for the transport and replace controls — rather than
//! a private text field. That matters beyond tidiness: the shared field is
//! where caret handling, selection, IME, clipboard, focus and the accessibility
//! role live, and a hand-rolled `Key::Character => query.push(c)` has none of
//! them. The HIG's carve-out for document engines is explicit that a code
//! editor may own its *buffer* surface and must still route its **side fields**
//! — find bar included — through the shared widgets.
//!
//! Matching itself is [`crate::search::line_matches`], the same function the
//! project-wide search uses, so "17 matches" in this bar and 17 rows in the
//! search panel can never come from two different definitions of a match.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::style_helpers::length;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, ButtonStyle, SearchField, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::search::{Query, line_matches};

/// Height of the bar. Two 38 px fields plus breathing room, matching the tab
/// bar's visual weight so the editor column reads as one stack.
pub const BAR_HEIGHT: f64 = 52.0;

/// Height when the replace row is showing as well.
pub const BAR_HEIGHT_REPLACE: f64 = 96.0;

/// One match in the active buffer, in the editor's own `(line, column)`
/// character coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub col: usize,
    pub len: usize,
}

/// Every match of `query` in `text`, in document order.
pub fn matches_in(text: &str, query: &Query) -> Vec<Match> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        for (col, len) in line_matches(line, query) {
            out.push(Match {
                line: line_idx,
                col,
                len,
            });
        }
    }
    out
}

/// The index of the first match at or after `(line, col)`, wrapping to 0.
///
/// "Find next" from wherever the caret happens to be is the behaviour every
/// editor has, and getting it from the caret rather than from a stored counter
/// is what makes it still correct after the user clicks somewhere else.
pub fn next_from(matches: &[Match], line: usize, col: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(
        matches
            .iter()
            .position(|m| (m.line, m.col) >= (line, col))
            .unwrap_or(0),
    )
}

/// The index of the last match strictly before `(line, col)`, wrapping to the
/// end.
pub fn prev_from(matches: &[Match], line: usize, col: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(
        matches
            .iter()
            .rposition(|m| (m.line, m.col) < (line, col))
            .unwrap_or(matches.len() - 1),
    )
}

/// What the bar is asking the app to do. The app owns the buffer, so the bar
/// never touches text itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindEvent {
    /// The query or its options changed; recount and re-highlight.
    QueryChanged,
    Next,
    Prev,
    ReplaceCurrent,
    ReplaceAll,
    Close,
}

/// The find / replace bar.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (bar visibility, replace-row visibility, two search options, a redraw flag) rather than a set of options that combine — a bitflag/enum would not read better"
)]
pub struct FindBar {
    visible: bool,
    replace_open: bool,
    find: SearchField,
    replace: SearchField,
    case_sensitive: bool,
    whole_word: bool,
    /// Compact layouts keep match options behind a small overflow button so
    /// the query remains editable without making those power options vanish.
    options_open: bool,

    /// Match count and position, pushed in by the app after each recount.
    total: usize,
    current: Option<usize>,

    rect: Rect,
    node: Option<NodeId>,
    find_node: Option<NodeId>,
    replace_node: Option<NodeId>,
    /// Hit rects recorded during `draw`, in the order of [`Self::buttons`].
    button_rects: Vec<(Btn, Rect)>,
    hover: Point,
    events: Vec<FindEvent>,
    needs_redraw: bool,
}

/// The bar's own controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Prev,
    Next,
    Case,
    Word,
    ToggleReplace,
    Options,
    Close,
    ReplaceOne,
    ReplaceAll,
}

impl FindBar {
    pub fn new() -> Self {
        FindBar {
            visible: false,
            replace_open: false,
            find: SearchField::new()
                .with_id("find-field")
                .with_placeholder("Find in file"),
            replace: SearchField::new()
                .with_id("replace-field")
                .with_placeholder("Replace with")
                .without_clear(),
            case_sensitive: false,
            whole_word: false,
            options_open: false,
            total: 0,
            current: None,
            rect: Rect::ZERO,
            node: None,
            find_node: None,
            replace_node: None,
            button_rects: Vec::new(),
            hover: Point::new(-1.0, -1.0),
            events: Vec::new(),
            needs_redraw: false,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// The height the bar wants right now (zero when hidden).
    pub fn height(&self) -> f64 {
        if !self.visible {
            0.0
        } else if self.replace_open {
            BAR_HEIGHT_REPLACE
        } else {
            BAR_HEIGHT
        }
    }

    /// Open the bar for find (`replace = false`) or find-and-replace.
    /// `seed` pre-fills the query — the editor passes the current selection, so
    /// Ctrl+F on a selected word searches for it, as it should.
    pub fn open(&mut self, replace: bool, seed: Option<&str>) {
        self.visible = true;
        self.replace_open = replace;
        if let Some(s) = seed
            && !s.is_empty()
            && !s.contains('\n')
        {
            self.find.set_query(s);
        }
        self.find.set_focused(true);
        self.replace.set_focused(false);
        self.options_open = false;
        self.needs_redraw = true;
        self.events.push(FindEvent::QueryChanged);
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.find.set_focused(false);
        self.replace.set_focused(false);
        self.needs_redraw = true;
    }

    /// The current query, as the app should search with it.
    pub fn query(&self) -> Query {
        Query::new(self.find.value())
            .case_sensitive(self.case_sensitive)
            .whole_word(self.whole_word)
    }

    /// The replacement text.
    pub fn replacement(&self) -> &str {
        self.replace.value()
    }

    /// Push the recount in: how many matches there are and which one the caret
    /// is on.
    pub fn set_match_state(&mut self, total: usize, current: Option<usize>) {
        if self.total != total || self.current != current {
            self.total = total;
            self.current = current;
            self.needs_redraw = true;
        }
    }

    /// Drain the actions the bar produced this frame.
    pub fn take_events(&mut self) -> Vec<FindEvent> {
        std::mem::take(&mut self.events)
    }

    /// The status label: "3 of 17", "No results", or blank for an empty query.
    pub fn status_label(&self) -> String {
        if self.find.value().is_empty() {
            String::new()
        } else if self.total == 0 {
            "No results".to_string()
        } else {
            match self.current {
                Some(i) => format!("{} of {}", i + 1, self.total),
                None => format!("{} matches", self.total),
            }
        }
    }

    /// Default the unmeasured first layout to compact, so a narrow first frame
    /// never paints controls beyond the viewport.
    fn compact_layout(&self) -> bool {
        self.rect.width() <= 0.0 || self.rect.width() < 520.0
    }

    /// On narrow windows preserve the editable field, navigation and replace
    /// affordances. Case/whole-word stay available from their keyboard/menu
    /// commands without crowding the last visible button off-screen.
    fn buttons(&self) -> Vec<Btn> {
        let mut buttons = Vec::new();
        if !self.compact_layout() {
            buttons.extend([Btn::Case, Btn::Word]);
        }
        buttons.extend([Btn::Prev, Btn::Next, Btn::ToggleReplace]);
        if self.compact_layout() {
            buttons.push(Btn::Options);
        }
        buttons.push(Btn::Close);
        buttons
    }

    fn label_of(b: Btn) -> &'static str {
        match b {
            Btn::Prev => "\u{2191}",
            Btn::Next => "\u{2193}",
            Btn::Case => "Aa",
            Btn::Word => "W",
            Btn::ToggleReplace => "\u{21C4}",
            Btn::Options => "\u{2026}",
            Btn::Close => "\u{2715}",
            Btn::ReplaceOne => "Replace",
            Btn::ReplaceAll => "All",
        }
    }

    fn accessible_name_of(b: Btn) -> &'static str {
        match b {
            Btn::Prev => "Previous match",
            Btn::Next => "Next match",
            Btn::Case => "Match case",
            Btn::Word => "Whole word",
            Btn::ToggleReplace => "Toggle replace",
            Btn::Options => "Find options",
            Btn::Close => "Close find bar",
            Btn::ReplaceOne => "Replace this match",
            Btn::ReplaceAll => "Replace all matches",
        }
    }

    /// Whether a toggle button is currently on.
    fn is_active(&self, b: Btn) -> bool {
        match b {
            Btn::Case => self.case_sensitive,
            Btn::Word => self.whole_word,
            Btn::ToggleReplace => self.replace_open,
            Btn::Options => self.options_open,
            _ => false,
        }
    }

    /// Apply a button press. Returns true if the press was consumed.
    fn press(&mut self, b: Btn) {
        match b {
            Btn::Prev => self.events.push(FindEvent::Prev),
            Btn::Next => self.events.push(FindEvent::Next),
            Btn::Case => {
                self.case_sensitive = !self.case_sensitive;
                self.events.push(FindEvent::QueryChanged);
            }
            Btn::Word => {
                self.whole_word = !self.whole_word;
                self.events.push(FindEvent::QueryChanged);
            }
            Btn::ToggleReplace => self.replace_open = !self.replace_open,
            Btn::Options => self.options_open = !self.options_open,
            Btn::Close => {
                self.close();
                self.events.push(FindEvent::Close);
            }
            Btn::ReplaceOne => self.events.push(FindEvent::ReplaceCurrent),
            Btn::ReplaceAll => self.events.push(FindEvent::ReplaceAll),
        }
        self.needs_redraw = true;
    }

    /// Test hook: press a control by name, exactly as a click on it would.
    #[cfg(test)]
    fn press_named(&mut self, name: &str) {
        let b = match name {
            "prev" => Btn::Prev,
            "next" => Btn::Next,
            "case" => Btn::Case,
            "word" => Btn::Word,
            "replace-toggle" => Btn::ToggleReplace,
            "options" => Btn::Options,
            "close" => Btn::Close,
            "replace-one" => Btn::ReplaceOne,
            "replace-all" => Btn::ReplaceAll,
            other => panic!("unknown find-bar control {other}"),
        };
        self.press(b);
    }

    /// Set the query as if typed, for tests and for the seeded open.
    pub fn set_query(&mut self, q: &str) {
        self.find.set_query(q);
        self.events.push(FindEvent::QueryChanged);
    }

    /// Set the replacement as if typed.
    pub fn set_replacement(&mut self, r: &str) {
        self.replace.set_query(r);
    }

    /// Headless-review hook for the compact overflow menu.
    pub fn show_options_for_shots(&mut self) {
        self.options_open = true;
        // The review state must demonstrate the checked treatment as well as
        // the unchecked labels; ordinary interaction still starts both false.
        self.case_sensitive = true;
        self.needs_redraw = true;
    }

    /// Lay out the find row: the shared `SearchField`, padded on the right
    /// for the status readout and the control cluster (see
    /// [`Widget::build_layout`]).
    fn build_find_row(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        controls_w: f64,
    ) -> NodeId {
        let find_node = self.find.build_layout(taffy, theme, node_map);
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node with a fixed Style and a freshly-created child cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
        let find_row = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    align_items: Some(taffy::AlignItems::Center),
                    size: Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(BAR_HEIGHT as f32),
                    },
                    padding: taffy::Rect {
                        left: length(PAD as f32),
                        right: length(controls_w as f32),
                        top: length(0.0_f32),
                        bottom: length(0.0_f32),
                    },
                    ..Default::default()
                },
                &[find_node],
            )
            .unwrap();
        self.find_node = Some(find_node);
        find_row
    }

    /// Lay out the replace row: the shared `SearchField`, padded on the
    /// right for the "Replace"/"All" buttons that sit where the find row's
    /// control cluster is (see [`Widget::build_layout`]).
    fn build_replace_row(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let replace_node = self.replace.build_layout(taffy, theme, node_map);
        // The replace row's two buttons sit where the find row's cluster is.
        let replace_controls = PAD * 2.0 + REPLACE_ONE_W + GAP + REPLACE_ALL_W;
        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node with a fixed Style and a freshly-created child cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
        let replace_row = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    align_items: Some(taffy::AlignItems::Center),
                    size: Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length((BAR_HEIGHT_REPLACE - BAR_HEIGHT) as f32),
                    },
                    padding: taffy::Rect {
                        left: length(PAD as f32),
                        right: length(replace_controls as f32),
                        top: length(0.0_f32),
                        bottom: length(0.0_f32),
                    },
                    ..Default::default()
                },
                &[replace_node],
            )
            .unwrap();
        self.replace_node = Some(replace_node);
        replace_row
    }
}

impl Default for FindBar {
    fn default() -> Self {
        Self::new()
    }
}

/// Width reserved on the right for the control cluster.
const BTN_W: f64 = 34.0;
const BTN_H: f64 = 28.0;
const GAP: f64 = 6.0;
const PAD: f64 = 10.0;
/// Width reserved for the "3 of 17" readout.
const STATUS_W: f64 = 92.0;
const COMPACT_STATUS_W: f64 = 60.0;
/// Compact enough for a 390px editor while retaining the full, explicit
/// labels. The old 96px/56px pair pushed "All" beyond the narrow viewport.
const REPLACE_ONE_W: f64 = 80.0;
const REPLACE_ALL_W: f64 = 48.0;

impl Widget for FindBar {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.find.sync(state, path);
        self.replace.sync(state, path);
        // A field reports its own edits (including its clear button) here, so
        // typing and clearing both recount through one path.
        if self.find.take_dirty().is_some() {
            self.events.push(FindEvent::QueryChanged);
            self.needs_redraw = true;
        }
        let _ = self.replace.take_dirty();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        if !self.visible {
            #[allow(
                clippy::unwrap_used,
                reason = "Widget::build_layout is infallible by trait contract; inserting a single leaf with a fixed Style cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
            )]
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(0.0),
                    },
                    ..Default::default()
                })
                .unwrap();
            self.node = Some(node);
            self.find_node = None;
            self.replace_node = None;
            return node;
        }

        // The find row leaves room on the right for the status readout and the
        // control cluster; the fields themselves are shared widgets sized by
        // their own layout.
        let status_w = if self.compact_layout() {
            COMPACT_STATUS_W
        } else {
            STATUS_W
        };
        let controls_w = status_w + self.buttons().len() as f64 * (BTN_W + GAP) + PAD * 2.0;
        let find_row = self.build_find_row(taffy, theme, node_map, controls_w);

        let mut rows = vec![find_row];
        if self.replace_open {
            rows.push(self.build_replace_row(taffy, theme, node_map));
        } else {
            self.replace_node = None;
        }

        #[allow(
            clippy::unwrap_used,
            reason = "Widget::build_layout is infallible by trait contract; inserting a node with a fixed Style and freshly-created children cannot fail short of an already-corrupted taffy arena, which nothing here could recover from"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    flex_shrink: 0.0,
                    size: Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::length(self.height() as f32),
                    },
                    ..Default::default()
                },
                &rows,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    #[allow(
        clippy::unwrap_used,
        reason = "Widget::apply_layout is infallible by trait contract; every node looked up here was created by this widget's own build_layout in the same tree, so a lookup failure means the taffy tree is already corrupted and there is no meaningful layout to fall back to"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if !self.visible {
            return;
        }
        let Ok(rows) = taffy.children(node) else {
            return;
        };
        let base = Point::new(self.rect.x0, self.rect.y0);
        if let Some(&find_row) = rows.first() {
            let rl = taffy.layout(find_row).unwrap();
            let row_origin = Point::new(
                base.x + f64::from(rl.location.x),
                base.y + f64::from(rl.location.y),
            );
            if let (Ok(kids), Some(_)) = (taffy.children(find_row), self.find_node)
                && let Some(&k) = kids.first()
            {
                self.find.apply_layout(taffy, k, row_origin);
            }
        }
        if self.replace_open
            && let Some(&replace_row) = rows.get(1)
        {
            let rl = taffy.layout(replace_row).unwrap();
            let row_origin = Point::new(
                base.x + f64::from(rl.location.x),
                base.y + f64::from(rl.location.y),
            );
            if let Ok(kids) = taffy.children(replace_row)
                && let Some(&k) = kids.first()
            {
                self.replace.apply_layout(taffy, k, row_origin);
            }
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
        if !self.visible || self.rect.width() <= 0.0 {
            self.button_rects.clear();
            return;
        }
        let c = &theme.tokens.colors;
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            c.surfaces.raised,
            None,
            &self.rect,
        );
        // Hairline seam under the bar, derived against the raised bar.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            ui::surface::edge_on(c.surfaces.raised),
            None,
            &Rect::new(self.rect.x0, self.rect.y1 - 1.0, self.rect.x1, self.rect.y1),
        );

        self.find
            .draw(scene, transform, text_renderer, theme, presence);
        if self.replace_open {
            self.replace
                .draw(scene, transform, text_renderer, theme, presence);
        }

        self.button_rects.clear();
        self.draw_find_controls(scene, transform, text_renderer, theme);
        if self.replace_open {
            self.draw_replace_buttons(scene, transform, text_renderer, theme);
        }
        if self.compact_layout() && self.options_open {
            self.draw_compact_options(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.visible {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            if self.rect.contains(p) != self.rect.contains(self.hover) || self.rect.contains(p) {
                self.needs_redraw = true;
            }
            self.hover = p;
        }

        // Enter / Shift+Enter step through matches; Escape closes. These are
        // handled before the fields see them so Enter does not just insert.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    self.events.push(FindEvent::Close);
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    if ctx.modifiers.state().shift_key() {
                        self.events.push(FindEvent::Prev);
                    } else {
                        self.events.push(FindEvent::Next);
                    }
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }

        // Control cluster.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && let Some((b, _)) = self
                .button_rects
                .iter()
                .find(|(_, r)| r.contains(self.hover))
                .copied()
        {
            let enabled = !matches!(b, Btn::Prev | Btn::Next | Btn::ReplaceOne | Btn::ReplaceAll)
                || self.total > 0;
            if enabled {
                self.press(b);
            }
            *ctx.handled = true;
            return;
        }

        // Otherwise the fields own it.
        let Ok(rows) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if let Some(&find_row) = rows.first()
            && let Ok(kids) = ctx.taffy.children(find_row)
            && let Some(&k) = kids.first()
        {
            self.find
                .handle_event(event, &mut ctx.reborrow(ctx.path, k));
        }
        if self.replace_open
            && let Some(&replace_row) = rows.get(1)
            && let Ok(kids) = ctx.taffy.children(replace_row)
            && let Some(&k) = kids.first()
        {
            self.replace
                .handle_event(event, &mut ctx.reborrow(ctx.path, k));
        }
    }

    fn name(&self) -> &'static str {
        "FindBar"
    }

    fn test_id(&self) -> Option<&str> {
        Some("find-bar")
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.visible.then(|| {
            let mut node = AccessNode::new(Role::Toolbar)
                .with_name(if self.replace_open {
                    "Find and replace"
                } else {
                    "Find"
                })
                .with_description("Enter searches; Shift+Enter searches backwards; Escape closes")
                .with_value(self.status_label())
                .with_state(AccessState::new().focusable(true));
            if self.compact_layout() && self.options_open {
                node = node.with_children(vec![
                    AccessNode::new(Role::Checkbox)
                        .with_name("Match case")
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .checked(self.case_sensitive),
                        ),
                    AccessNode::new(Role::Checkbox)
                        .with_name("Whole word")
                        .with_state(AccessState::new().focusable(true).checked(self.whole_word)),
                ]);
            }
            node
        })
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.visible {
            let mut children: Vec<&dyn Widget> = vec![&self.find];
            if self.replace_open {
                children.push(&self.replace);
            }
            children
        } else {
            Vec::new()
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.visible {
            let mut children: Vec<&mut dyn Widget> = vec![&mut self.find];
            if self.replace_open {
                children.push(&mut self.replace);
            }
            children
        } else {
            Vec::new()
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.needs_redraw
            || (self.visible && (self.find.needs_redraw() || self.replace.needs_redraw()))
    }

    fn mark_drawn(&mut self) {
        self.needs_redraw = false;
        self.find.mark_drawn();
        self.replace.mark_drawn();
    }
}

impl FindBar {
    /// Draw the status readout and the transport/option/close button
    /// cluster on the find row's right edge, recording each button's hit
    /// rect.
    fn draw_find_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let find_row_mid = self.rect.y0 + BAR_HEIGHT / 2.0;

        // Status readout, immediately left of the control cluster.
        let buttons = self.buttons();
        let cluster_w = buttons.len() as f64 * (BTN_W + GAP);
        let cluster_x0 = self.rect.x1 - PAD - cluster_w;
        let status = self.status_label();
        if !status.is_empty() {
            let color = if self.total == 0 {
                c.semantic.warning
            } else {
                c.text_dim
            };
            let (w, _) = text_renderer.measure(&status, 12.0, None);
            text_renderer.draw_text(
                scene,
                &status,
                12.0,
                color,
                transform
                    * Affine::translate((cluster_x0 - GAP - f64::from(w), find_row_mid - 7.0)),
                None,
            );
        }

        // The control cluster, painted through the shared button face so the
        // hover/press/disabled palette is the OS's, not this app's.
        let mut x = cluster_x0;
        for b in buttons {
            let rect = Rect::new(
                x,
                find_row_mid - BTN_H / 2.0,
                x + BTN_W,
                find_row_mid + BTN_H / 2.0,
            );
            let enabled = match b {
                Btn::Prev | Btn::Next => self.total > 0,
                _ => true,
            };
            let style = if self.is_active(b) {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Ghost
            };
            let face = ButtonFace::new(rect, Self::label_of(b))
                .style(style)
                .radius(6.0)
                .text_size(12.0)
                .disabled(!enabled)
                .hovered(enabled && rect.contains(self.hover))
                .accessible_name(Self::accessible_name_of(b));
            Button::paint(scene, transform, text_renderer, theme, &face);
            self.button_rects.push((b, rect));
            x += BTN_W + GAP;
        }
    }

    /// A real compact overflow menu rather than silently removing case and
    /// whole-word matching. It intentionally floats over the replace row: the
    /// options are transient, and doing so avoids reallocating the editor while
    /// a user is typing a find query.
    fn draw_compact_options(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let w = 172.0;
        let h = BTN_H + PAD * 2.0;
        let x1 = (self.rect.x1 - PAD).min(self.rect.x1);
        let menu = Rect::new(
            x1 - w,
            self.rect.y0 + BAR_HEIGHT - 2.0,
            x1,
            self.rect.y0 + BAR_HEIGHT - 2.0 + h,
        );
        let shape = vello::kurbo::RoundedRect::from_rect(menu, 7.0);
        scene.fill(Fill::NonZero, transform, c.surfaces.overlay, None, &shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(menu, 7.0),
        );
        for (index, button) in [Btn::Case, Btn::Word].into_iter().enumerate() {
            let rect = Rect::new(
                menu.x0 + PAD + index as f64 * 76.0,
                menu.y0 + PAD,
                menu.x0 + PAD + index as f64 * 76.0 + 70.0,
                menu.y1 - PAD,
            );
            let label = match (button, self.is_active(button)) {
                (Btn::Case, true) => "✓ Aa case",
                (Btn::Case, false) => "Aa case",
                (Btn::Word, true) => "✓ Whole",
                (Btn::Word, false) => "Whole",
                _ => "Option",
            };
            let face = ButtonFace::new(rect, label)
                .style(if self.is_active(button) {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Ghost
                })
                .radius(6.0)
                .text_size(11.0)
                .hovered(rect.contains(self.hover))
                .accessible_name(Self::accessible_name_of(button));
            Button::paint(scene, transform, text_renderer, theme, &face);
            self.button_rects.push((button, rect));
        }
    }

    /// Draw the replace row's "Replace"/"All" buttons, recording their hit
    /// rects.
    fn draw_replace_buttons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let mid = self.rect.y0 + BAR_HEIGHT + (BAR_HEIGHT_REPLACE - BAR_HEIGHT) / 2.0;
        let all = Rect::new(
            self.rect.x1 - PAD - REPLACE_ALL_W,
            mid - BTN_H / 2.0,
            self.rect.x1 - PAD,
            mid + BTN_H / 2.0,
        );
        let one = Rect::new(
            all.x0 - GAP - REPLACE_ONE_W,
            mid - BTN_H / 2.0,
            all.x0 - GAP,
            mid + BTN_H / 2.0,
        );
        for (b, rect, style) in [
            (Btn::ReplaceOne, one, ButtonStyle::Neutral),
            (Btn::ReplaceAll, all, ButtonStyle::Neutral),
        ] {
            let enabled = self.total > 0;
            let face = ButtonFace::new(rect, Self::label_of(b))
                .style(style)
                .radius(6.0)
                .text_size(12.0)
                .disabled(!enabled)
                .hovered(enabled && rect.contains(self.hover))
                .accessible_name(Self::accessible_name_of(b));
            Button::paint(scene, transform, text_renderer, theme, &face);
            self.button_rects.push((b, rect));
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    fn q(n: &str) -> Query {
        Query::new(n)
    }

    #[test]
    fn matches_are_in_document_order_and_navigation_wraps() {
        // Each phase computes its own matches; no state is shared.
        matches_are_found_across_lines_in_document_order();
        an_empty_query_matches_nothing();
        next_starts_from_the_caret_and_wraps();
        prev_steps_backwards_and_wraps();
        navigation_on_no_matches_is_none_rather_than_a_panic();
    }

    fn matches_are_found_across_lines_in_document_order() {
        let text = "let foo = 1;\nfn bar() {}\n// foo again foo\n";
        let m = matches_in(text, &q("foo"));
        assert_eq!(m.len(), 3);
        assert_eq!(
            m[0],
            Match {
                line: 0,
                col: 4,
                len: 3
            }
        );
        assert_eq!(
            m[1],
            Match {
                line: 2,
                col: 3,
                len: 3
            }
        );
        assert_eq!(
            m[2],
            Match {
                line: 2,
                col: 13,
                len: 3
            }
        );
    }

    fn an_empty_query_matches_nothing() {
        assert!(matches_in("anything at all", &q("")).is_empty());
    }

    fn next_starts_from_the_caret_and_wraps() {
        let text = "a\nb a\nc a\n";
        let m = matches_in(text, &q("a"));
        assert_eq!(m.len(), 3);
        // From the very top: the first match.
        assert_eq!(next_from(&m, 0, 0), Some(0));
        // From just past the first: the second.
        assert_eq!(next_from(&m, 0, 1), Some(1));
        // From past the last: wraps to the first.
        assert_eq!(next_from(&m, 99, 0), Some(0));
    }

    fn prev_steps_backwards_and_wraps() {
        let text = "a\nb a\nc a\n";
        let m = matches_in(text, &q("a"));
        assert_eq!(prev_from(&m, 2, 2), Some(1));
        assert_eq!(prev_from(&m, 0, 0), Some(2), "wraps to the last match");
    }

    fn navigation_on_no_matches_is_none_rather_than_a_panic() {
        assert_eq!(next_from(&[], 0, 0), None);
        assert_eq!(prev_from(&[], 0, 0), None);
    }

    #[test]
    fn find_bar_reports_its_query_options_and_status() {
        // Each phase builds a fresh bar.
        the_bar_reports_its_query_and_options();
        a_multiline_selection_does_not_seed_the_query();
        the_status_readout_says_what_is_true();
    }

    fn the_bar_reports_its_query_and_options() {
        let mut bar = FindBar::new();
        bar.open(false, Some("needle"));
        assert!(bar.is_visible());
        assert_eq!(bar.query().needle, "needle");
        assert!(!bar.query().case_sensitive);

        bar.press_named("case");
        assert!(bar.query().case_sensitive);
        bar.press_named("word");
        assert!(bar.query().whole_word);
        // Each toggle asks the app to recount.
        assert!(bar.take_events().contains(&FindEvent::QueryChanged));
    }

    fn a_multiline_selection_does_not_seed_the_query() {
        // Ctrl+F with a paragraph selected should open an empty find, not a
        // query containing newlines that can never match a line.
        let mut bar = FindBar::new();
        bar.open(false, Some("two\nlines"));
        assert_eq!(bar.query().needle, "");
    }

    fn the_status_readout_says_what_is_true() {
        let mut bar = FindBar::new();
        bar.open(false, None);
        assert_eq!(bar.status_label(), "", "no query, nothing to report");

        bar.set_query("x");
        bar.set_match_state(0, None);
        assert_eq!(bar.status_label(), "No results");

        bar.set_match_state(17, Some(2));
        assert_eq!(bar.status_label(), "3 of 17", "the readout is 1-based");

        bar.set_match_state(17, None);
        assert_eq!(bar.status_label(), "17 matches");
    }

    #[test]
    fn find_bar_controls_emit_actions_close_and_are_addressable() {
        // Each phase builds a fresh bar.
        the_transport_and_replace_controls_emit_their_actions();
        closing_hides_the_bar_and_collapses_its_height();
        the_bar_is_addressable_by_name_for_headless_driving();
    }

    fn the_transport_and_replace_controls_emit_their_actions() {
        let mut bar = FindBar::new();
        bar.open(true, Some("a"));
        bar.set_match_state(3, Some(0));
        let _ = bar.take_events();

        bar.press_named("next");
        bar.press_named("prev");
        bar.press_named("replace-one");
        bar.press_named("replace-all");
        assert_eq!(
            bar.take_events(),
            vec![
                FindEvent::Next,
                FindEvent::Prev,
                FindEvent::ReplaceCurrent,
                FindEvent::ReplaceAll
            ]
        );
    }

    fn closing_hides_the_bar_and_collapses_its_height() {
        let mut bar = FindBar::new();
        assert_eq!(bar.height(), 0.0, "hidden bar takes no space");
        bar.open(false, None);
        assert_eq!(bar.height(), BAR_HEIGHT);
        bar.press_named("replace-toggle");
        assert_eq!(
            bar.height(),
            BAR_HEIGHT_REPLACE,
            "the replace row adds height"
        );
        bar.press_named("close");
        assert!(!bar.is_visible());
        assert_eq!(bar.height(), 0.0);
        assert!(bar.take_events().contains(&FindEvent::Close));
    }

    fn the_bar_is_addressable_by_name_for_headless_driving() {
        let bar = FindBar::new();
        assert_eq!(Widget::test_id(&bar), Some("find-bar"));
    }
}

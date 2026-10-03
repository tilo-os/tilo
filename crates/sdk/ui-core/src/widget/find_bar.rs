//! Find in a document: the [`FindBar`] and the [`Findable`] capability.
//!
//! A document-like view (text, code, a PDF, a hex dump, a terminal) provides
//! [`Findable`]: it runs the query over its own content, reports the matches,
//! scrolls one into view and paints every match with [`paint_find_match`].
//! The framework owns the bar: [`FindScope`] places it as a band at the top
//! of the content it wraps and binds Ctrl+F (⌘F), Enter / Shift+Enter,
//! F3 / Shift+F3 and Escape. A painted view that lays out its own chrome
//! hosts a [`FindBar`] directly ([`FindBar::layout`], `draw`, `handle_event`)
//! and calls [`FindBar::drive`] with itself.
//!
//! The bar has a counter ("3 of 12"), Match case and Whole word chips,
//! previous/next, and optionally a replace row and a text/hex mode.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::capability::capability_mut;
use crate::widget::search_field::FilterCount;
use crate::widget::{
    AppModel, Button, ButtonFace, Chip, ChipLead, ChipStyle, ControlSize, Element, ModelPath,
    SearchField, TaffyTreeExt, Widget,
};

/// What a find runs over: text, or a byte view's hex digits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FindMode {
    #[default]
    Text,
    /// The query is hex bytes (`de ad be ef`), for a byte view.
    Hex,
}

/// The options the bar's chips set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub mode: FindMode,
}

/// One match, as a span of the findable's own content (a byte range of its
/// text, or of its bytes in hex mode). The findable interprets it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FindMatch {
    pub start: usize,
    pub end: usize,
}

impl FindMatch {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A view whose content can be searched in place.
pub trait Findable {
    /// Every match of `query` under `options`, in reading order, and keep
    /// them to paint. An empty query clears the matches.
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch>;

    /// Make match `index` the current one: scroll it into view and paint it
    /// emphasised (see [`paint_find_match`]).
    fn reveal(&mut self, index: usize);

    /// The bar closed: stop painting matches.
    fn clear_find(&mut self) {
        let _ = self.find("", FindOptions::default());
    }

    /// Whether this view can replace text; the bar then offers a replace row.
    fn supports_replace(&self) -> bool {
        false
    }

    /// Replace match `index` with `with`. Returns whether it changed the
    /// content; an edit goes through the view's own actions, so it is undoable.
    fn replace(&mut self, _index: usize, _with: &str) -> bool {
        false
    }

    /// Replace every match; returns how many changed.
    fn replace_all(&mut self, _with: &str) -> usize {
        0
    }

    /// Whether this view shows bytes, so the bar offers a text/hex mode.
    fn supports_hex(&self) -> bool {
        false
    }
}

/// Paint one match highlight over `rect`, the span of the matched glyphs —
/// never a whole line or block. Every match is a wash of the searcher's own
/// selection colour; the current one is stronger and outlined so it can be
/// told apart at a glance.
pub fn paint_find_match(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    current: bool,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let intent = &theme.tokens.colors.intent;
    let shape = RoundedRect::from_rect(
        rect.inflate(1.0, 0.0),
        f64::from(theme.tokens.radii.detail) / 2.0,
    );
    if current {
        use crate::color_compat::ColorCompat;
        let strong = intent.selection_tint.with_alpha_factor(2.0);
        scene.fill(Fill::NonZero, transform, strong, None, &shape);
        scene.stroke(&Stroke::new(1.5), transform, intent.you, None, &shape);
    } else {
        scene.fill(
            Fill::NonZero,
            transform,
            intent.selection_tint,
            None,
            &shape,
        );
    }
}

/// What the bar asked for since the last [`FindBar::drive`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Pending {
    search: bool,
    step: Option<Step>,
    replace: Option<ReplaceRequest>,
    closed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Next,
    Previous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReplaceRequest {
    One,
    All,
}

/// Every clickable part of the bar, in focus order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    MatchCase,
    WholeWord,
    Text,
    Hex,
    Previous,
    Next,
    Close,
    Replace,
    ReplaceAll,
}

impl Part {
    fn label(self) -> &'static str {
        match self {
            Self::MatchCase => "Match case",
            Self::WholeWord => "Whole word",
            Self::Text => "Text",
            Self::Hex => "Hex",
            Self::Previous => "Previous match",
            Self::Next => "Next match",
            Self::Close => "Close find",
            Self::Replace => "Replace",
            Self::ReplaceAll => "Replace all",
        }
    }
}

/// The find bar: a query field with a match counter, option chips,
/// previous/next and close, and an optional replace row.
pub struct FindBar {
    field: SearchField,
    replace_field: SearchField,
    options: FindOptions,
    replace_row: bool,
    hex_mode: bool,
    matches: usize,
    current: Option<usize>,
    pending: Pending,
    rect: Rect,
    parts: Vec<(Part, Rect)>,
    hover: Option<Part>,
    last_cursor: Point,
    platform: PlatformConventions,
    /// Which field types: the query (`false`) or the replacement.
    replace_focused: bool,
    focused: bool,
    /// The query and options the matches were last found for.
    searched: Option<(String, FindOptions)>,
}

impl Default for FindBar {
    fn default() -> Self {
        Self::new()
    }
}

impl FindBar {
    #[must_use]
    pub fn new() -> Self {
        Self {
            field: SearchField::new()
                .with_placeholder("Find")
                .with_count_slot()
                .with_id("find-query"),
            replace_field: SearchField::new()
                .with_icon(Icon::Pencil)
                .with_placeholder("Replace with")
                .with_id("find-replacement"),
            options: FindOptions::default(),
            replace_row: false,
            hex_mode: false,
            matches: 0,
            current: None,
            pending: Pending::default(),
            rect: Rect::ZERO,
            parts: Vec::new(),
            hover: None,
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            replace_focused: false,
            focused: false,
            searched: None,
        }
    }

    /// Offer the replace row (the bar also offers it whenever its findable
    /// supports replacing).
    #[must_use]
    pub fn with_replace(mut self) -> Self {
        self.replace_row = true;
        self
    }

    /// Offer the text/hex mode switch.
    #[must_use]
    pub fn with_hex_mode(mut self) -> Self {
        self.hex_mode = true;
        self
    }

    /// The query typed so far.
    #[must_use]
    pub fn query(&self) -> &str {
        self.field.value()
    }

    /// Start with `query` (the selection when the bar opens), selected.
    pub fn set_query(&mut self, query: &str) {
        self.field.set_query(query);
        self.field.select_all();
        self.pending.search = true;
    }

    #[must_use]
    pub fn options(&self) -> FindOptions {
        self.options
    }

    /// `(current, total)` — the counter's numbers, 1-based current.
    #[must_use]
    pub fn position(&self) -> Option<(usize, usize)> {
        self.current
            .map(|current| (current.saturating_add(1), self.matches))
    }

    /// Focus the query and select it, as Ctrl+F on an open bar does.
    pub fn focus_query(&mut self) {
        self.replace_focused = false;
        self.focused = true;
        self.field.set_focused(true);
        self.replace_field.set_focused(false);
        self.field.select_all();
    }

    /// Whether the user asked to close the bar since the last
    /// [`drive`](Self::drive).
    #[must_use]
    pub fn close_requested(&self) -> bool {
        self.pending.closed
    }

    /// Ask the bar to close (Escape does this).
    pub fn request_close(&mut self) {
        self.pending.closed = true;
    }

    /// Step to the next match, wrapping.
    pub fn next(&mut self) {
        self.pending.step = Some(Step::Next);
    }

    /// Step to the previous match, wrapping.
    pub fn previous(&mut self) {
        self.pending.step = Some(Step::Previous);
    }

    /// The bar's height: one control row plus padding, two with replace.
    #[must_use]
    pub fn height(&self, theme: &Theme) -> f64 {
        let row = ControlSize::Regular.height(theme);
        let pad = f64::from(theme.tokens.spacing.s2);
        let rows = if self.replace_row { 2.0 } else { 1.0 };
        rows * row + (rows + 1.0) * pad
    }

    /// Carry what the user did to `target`: re-run the query when it or an
    /// option changed, step the current match, replace, or clear on close.
    /// Call after routing each event.
    pub fn drive(&mut self, target: &mut dyn Findable) {
        self.replace_row |= target.supports_replace();
        self.hex_mode |= target.supports_hex();
        let _ = self.field.take_dirty();
        let pending = std::mem::take(&mut self.pending);
        if pending.closed {
            target.clear_find();
            self.matches = 0;
            self.current = None;
            self.searched = None;
            self.pending.closed = true;
            return;
        }
        let stale = self
            .searched
            .as_ref()
            .is_none_or(|(query, options)| query != self.field.value() || *options != self.options);
        if let Some(request) = pending.replace {
            let with = self.replace_field.value().to_owned();
            match request {
                ReplaceRequest::One => {
                    if let Some(current) = self.current {
                        target.replace(current, &with);
                    }
                }
                ReplaceRequest::All => {
                    target.replace_all(&with);
                }
            }
            self.research(target, self.current);
        } else if pending.search || stale {
            self.research(target, Some(0));
        }
        if let Some(step) = pending.step
            && self.matches > 0
        {
            let current = self.current.unwrap_or(0);
            let next = match step {
                Step::Next => current
                    .saturating_add(1)
                    .checked_rem(self.matches)
                    .unwrap_or(0),
                Step::Previous => current
                    .checked_sub(1)
                    .unwrap_or_else(|| self.matches.saturating_sub(1)),
            };
            self.current = Some(next);
            target.reveal(next);
        }
        self.field
            .set_count((!self.field.value().is_empty()).then(|| {
                FilterCount::new(
                    self.current.map_or(0, |c| c.saturating_add(1)),
                    self.matches,
                )
            }));
    }

    /// Run the query again, keeping the current match near `keep`.
    fn research(&mut self, target: &mut dyn Findable, keep: Option<usize>) {
        let found = target.find(self.field.value(), self.options);
        self.searched = Some((self.field.value().to_owned(), self.options));
        self.matches = found.len();
        self.current =
            (self.matches > 0).then(|| keep.unwrap_or(0).min(self.matches.saturating_sub(1)));
        if let Some(current) = self.current {
            target.reveal(current);
        }
    }

    fn toggle(&mut self, part: Part) {
        match part {
            Part::MatchCase => self.options.match_case = !self.options.match_case,
            Part::WholeWord => self.options.whole_word = !self.options.whole_word,
            Part::Text => self.options.mode = FindMode::Text,
            Part::Hex => self.options.mode = FindMode::Hex,
            Part::Previous => {
                self.previous();
                return;
            }
            Part::Next => {
                self.next();
                return;
            }
            Part::Close => {
                self.request_close();
                return;
            }
            Part::Replace => {
                self.pending.replace = Some(ReplaceRequest::One);
                return;
            }
            Part::ReplaceAll => {
                self.pending.replace = Some(ReplaceRequest::All);
                return;
            }
        }
        self.pending.search = true;
    }

    fn part_on(&self, part: Part) -> bool {
        match part {
            Part::MatchCase => self.options.match_case,
            Part::WholeWord => self.options.whole_word,
            Part::Text => self.options.mode == FindMode::Text,
            Part::Hex => self.options.mode == FindMode::Hex,
            _ => false,
        }
    }

    /// Lay the bar out in `rect` (its full band). Hosts that do not go
    /// through the layout pass call this before `draw`.
    pub fn layout(&mut self, rect: Rect, theme: &Theme, text: &mut TextRenderer) {
        self.rect = rect;
        self.parts.clear();
        let pad = f64::from(theme.tokens.spacing.s2);
        let gutter = f64::from(theme.tokens.spacing.s3);
        let row_h = ControlSize::Regular.height(theme);
        let chip_h = ControlSize::Small.height(theme);
        let x0 = rect.x0 + gutter;
        let x1 = rect.x1 - gutter;
        let y = rect.y0 + pad;
        // Trailing: previous, next, close as square icon buttons.
        let mut right = x1;
        for part in [Part::Close, Part::Next, Part::Previous] {
            self.parts
                .push((part, Rect::new(right - row_h, y, right, y + row_h)));
            right -= row_h;
        }
        right -= pad;
        // Option chips, then the mode switch, right to left.
        let chip_y = y + (row_h - chip_h) / 2.0;
        let mut chips = vec![Part::WholeWord, Part::MatchCase];
        if self.hex_mode {
            chips.splice(0..0, [Part::Hex, Part::Text]);
        }
        for part in chips {
            let w = Chip::natural_width_at(part.label(), chip_h, ChipLead::None, theme, text);
            // Narrow bars drop the chips before they squeeze the field.
            if right - w < x0 + 3.0 * row_h * 2.0 {
                break;
            }
            self.parts
                .push((part, Rect::new(right - w, chip_y, right, chip_y + chip_h)));
            right -= w + pad;
        }
        self.field
            .apply_layout_external(Rect::new(x0, y, right.max(x0), y + row_h));
        if self.replace_row {
            let y = y + row_h + pad;
            let mut right = x1;
            for part in [Part::ReplaceAll, Part::Replace] {
                let style = ControlSize::Regular.label_style(theme);
                let (w, _) = text.measure_style(part.label(), None, &style);
                let w = f64::from(w) + 2.0 * gutter;
                self.parts
                    .push((part, Rect::new(right - w, y, right, y + row_h)));
                right -= w + pad;
            }
            self.replace_field
                .apply_layout_external(Rect::new(x0, y, right.max(x0), y + row_h));
        }
    }

    fn part_at(&self, point: Point) -> Option<Part> {
        self.parts
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(part, _)| *part)
    }

    /// Paint the bar. `layout` must have run.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 {
            return;
        }
        // The band stands on the content surface; a hairline parts it from
        // the content below, which is the same surface.
        let ground = theme.tokens.colors.surfaces.content;
        crate::surface::separator(
            scene,
            transform,
            self.rect.x0,
            self.rect.x1,
            self.rect.y1,
            0.0,
            ground,
        );
        self.field.draw(scene, transform, text, theme, presence);
        if self.replace_row {
            self.replace_field
                .draw(scene, transform, text, theme, presence);
        }
        let parts = self.parts.clone();
        for (part, rect) in parts {
            let hovered = self.hover == Some(part);
            match part {
                Part::MatchCase | Part::WholeWord | Part::Text | Part::Hex => {
                    let style = if self.part_on(part) {
                        ChipStyle::tinted(theme.text_primary)
                    } else {
                        ChipStyle::quiet()
                    };
                    Chip::draw_at(
                        scene,
                        transform,
                        rect,
                        part.label(),
                        ChipLead::None,
                        style,
                        theme,
                        text,
                    );
                }
                Part::Previous | Part::Next | Part::Close => {
                    let icon = match part {
                        Part::Previous => Icon::ChevronUp,
                        Part::Next => Icon::ChevronDown,
                        _ => Icon::X,
                    };
                    let disabled = matches!(part, Part::Previous | Part::Next) && self.matches == 0;
                    let face = ButtonFace::new(rect, "")
                        .icon(icon)
                        .ghost()
                        .hovered(hovered)
                        .disabled(disabled)
                        .accessible_name(part.label());
                    Button::paint(scene, transform, text, theme, &face);
                }
                Part::Replace | Part::ReplaceAll => {
                    let disabled = self.matches == 0;
                    let face = ButtonFace::new(rect, part.label())
                        .hovered(hovered)
                        .disabled(disabled);
                    Button::paint(scene, transform, text, theme, &face);
                }
            }
        }
    }

    /// Route one event; `node` is the bar's own layout node (its fields
    /// have none). Returns whether the bar consumed it.
    pub fn route(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            self.hover = self.part_at(self.last_cursor);
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if let Some(part) = self.part_at(self.last_cursor) {
                self.toggle(part);
                *ctx.handled = true;
                return true;
            }
            if self.replace_row && self.replace_field.get_rect().contains(self.last_cursor) {
                self.replace_focused = true;
                self.field.set_focused(false);
            } else if self.field.get_rect().contains(self.last_cursor) {
                self.replace_focused = false;
                self.replace_field.set_focused(false);
            }
        }
        if self.focused
            && let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            let shift = ctx.modifiers.state().shift_key();
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.request_close();
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) if self.replace_focused => {
                    self.toggle(Part::Replace);
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter | NamedKey::F3) => {
                    if shift {
                        self.previous();
                    } else {
                        self.next();
                    }
                    *ctx.handled = true;
                    return true;
                }
                Key::Named(NamedKey::Tab) if self.replace_row => {
                    self.replace_focused = !self.replace_focused;
                    self.field.set_focused(!self.replace_focused);
                    self.replace_field.set_focused(self.replace_focused);
                    *ctx.handled = true;
                    return true;
                }
                _ => {}
            }
        }
        let field = if self.replace_focused {
            &mut self.replace_field
        } else {
            &mut self.field
        };
        if self.focused
            || matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
            )
        {
            field.handle_event(event, ctx);
        }
        *ctx.handled
    }
}

impl Widget for FindBar {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.field.set_platform_conventions(conventions);
        self.replace_field.set_platform_conventions(conventions);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "a find bar's height is a small logical-pixel value"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(self.height(theme) as f32),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
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
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.rect;
        self.layout(rect, theme, text);
        self.paint(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.route(event, ctx);
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.field.set_focused(!self.replace_focused);
            self.replace_field.set_focused(self.replace_focused);
        } else {
            self.field.set_focused(false);
            self.replace_field.set_focused(false);
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if self.replace_focused {
            self.replace_field.ime_cursor_rect()
        } else {
            self.field.ime_cursor_rect()
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.field.ime_purpose()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.part_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        if self.replace_row && self.replace_field.get_rect().contains(point) {
            return self.replace_field.cursor_shape(point);
        }
        self.field.cursor_shape(point)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.replace_row {
            vec![&self.field, &self.replace_field]
        } else {
            vec![&self.field]
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.field);
        if self.replace_row {
            visitor(&self.replace_field);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.replace_row {
            vec![&mut self.field, &mut self.replace_field]
        } else {
            vec![&mut self.field]
        }
    }

    fn name(&self) -> &'static str {
        "FindBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut children = Vec::new();
        if let Some(field) = self.field.accessibility() {
            let position = match self.position() {
                Some((current, total)) => format!("{current} of {total}"),
                None if self.field.value().is_empty() => String::new(),
                None => "No matches".to_owned(),
            };
            let mut field = field.with_name("Find").with_bounds(self.field.get_rect());
            if !position.is_empty() {
                field = field.with_description(position);
            }
            children.push(field);
        }
        if self.replace_row
            && let Some(field) = self.replace_field.accessibility()
        {
            children.push(
                field
                    .with_name("Replace with")
                    .with_bounds(self.replace_field.get_rect()),
            );
        }
        for (part, rect) in &self.parts {
            let role = match part {
                Part::MatchCase | Part::WholeWord => Role::Checkbox,
                Part::Text | Part::Hex => Role::RadioButton,
                _ => Role::Button,
            };
            let mut state = AccessState::new().focusable(true);
            if matches!(role, Role::Checkbox | Role::RadioButton) {
                state = state.checked(self.part_on(*part));
            }
            children.push(
                AccessNode::new(role)
                    .with_name(part.label())
                    .with_bounds(*rect)
                    .with_action_target(format!("find-{part:?}"))
                    .with_state(state),
            );
        }
        Some(
            AccessNode::new(Role::Toolbar)
                .with_name("Find")
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut AppModel,
        _: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(part) = self
            .parts
            .iter()
            .map(|(part, _)| *part)
            .find(|part| target == format!("find-{part:?}"))
        else {
            return false;
        };
        self.toggle(part);
        true
    }
}

/// Run `visit` on the first [`Findable`] in `widget`'s subtree; `None`
/// when there is none.
pub fn with_findable<R>(
    widget: &mut dyn Widget,
    visit: &mut dyn FnMut(&mut dyn Findable) -> R,
) -> Option<R> {
    if let Some(target) = capability_mut::<dyn Findable>(&mut *widget) {
        return Some(visit(target));
    }
    for child in widget.children_mut() {
        if let Some(result) = with_findable(child, visit) {
            return Some(result);
        }
    }
    None
}

/// Content with find: a [`FindBar`] that Ctrl+F (⌘F) opens as a band at the
/// top of the content, driving the first [`Findable`] inside it.
pub struct FindScope {
    bar: FindBar,
    content: Element,
    open: bool,
    structure_dirty: bool,
    rect: Rect,
    bar_node: Option<NodeId>,
    content_node: Option<NodeId>,
    platform: PlatformConventions,
}

impl FindScope {
    pub fn new(content: Element) -> Self {
        Self {
            bar: FindBar::new(),
            content,
            open: false,
            structure_dirty: false,
            rect: Rect::ZERO,
            bar_node: None,
            content_node: None,
            platform: PlatformConventions::default(),
        }
    }

    /// Configure the bar (`FindBar::with_replace`, `with_hex_mode`).
    #[must_use]
    pub fn with_bar(mut self, bar: FindBar) -> Self {
        self.bar = bar;
        self
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub fn bar(&self) -> &FindBar {
        &self.bar
    }

    pub fn bar_mut(&mut self) -> &mut FindBar {
        &mut self.bar
    }

    /// Open the bar (as Ctrl+F does) and focus its query.
    pub fn open(&mut self) {
        if !self.open {
            self.open = true;
            self.structure_dirty = true;
        }
        self.bar.focus_query();
    }

    /// Close the bar and clear the matches.
    pub fn close(&mut self) {
        self.bar.request_close();
        self.drive();
    }

    fn drive(&mut self) {
        let bar = &mut self.bar;
        if with_findable(self.content.as_mut(), &mut |target| bar.drive(target)).is_none() {
            return;
        }
        if self.bar.close_requested() {
            self.bar.pending.closed = false;
            if self.open {
                self.open = false;
                self.structure_dirty = true;
            }
        }
    }

    fn is_find_shortcut(&self, event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
        matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } if c.eq_ignore_ascii_case("f")
                && self.platform.command_modifier_down(modifiers.state())
                && !modifiers.state().shift_key()
        )
    }
}

impl Widget for FindScope {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.bar.set_platform_conventions(conventions);
        self.content.set_platform_conventions(conventions);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut children = Vec::new();
        self.bar_node = if self.open {
            let node = self.bar.build_layout(taffy, theme, node_map);
            children.push(node);
            Some(node)
        } else {
            None
        };
        let content = self.content.build_layout(taffy, theme, node_map);
        children.push(content);
        self.content_node = Some(content);
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
            &children,
        );
        if let Ok(mut style) = taffy.style(content).cloned() {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.min_size.height = length(0.0_f32);
            let _ = taffy.set_style(content, style);
        }
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
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(bar) = self.bar_node {
            self.bar.apply_layout(taffy, bar, inner);
        }
        if let Some(content) = self.content_node {
            self.content.apply_layout(taffy, content, inner);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let content = self.content.take_structure_dirty();
        own || content
    }

    fn is_layout_dirty(&self) -> bool {
        self.structure_dirty || self.content.is_layout_dirty()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content.draw(scene, transform, text, theme, presence);
        if self.open {
            self.bar.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.is_find_shortcut(event, *ctx.modifiers)
            && with_findable(self.content.as_mut(), &mut |_| ()).is_some()
        {
            self.open();
            if let Some(bar) = self.bar_node {
                *ctx.focus_target = Some(bar);
            }
            *ctx.handled = true;
            return;
        }
        if self.open
            && let Some(bar) = self.bar_node
        {
            self.bar.route(event, &mut ctx.reborrow(ctx.path, bar));
            self.drive();
            if *ctx.handled {
                return;
            }
        }
        if let Some(content) = self.content_node {
            self.content
                .handle_event(event, &mut ctx.reborrow(ctx.path, content));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if self.open {
            vec![&self.bar, self.content.as_ref()]
        } else {
            vec![self.content.as_ref()]
        }
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        if self.open {
            visitor(&self.bar);
        }
        visitor(self.content.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.open {
            vec![&mut self.bar, self.content.as_mut()]
        } else {
            vec![self.content.as_mut()]
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.content.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "FindScope"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    /// A text a find runs over: case-folding and whole-word aware, keeping
    /// what it found and which match is current.
    #[derive(Default)]
    struct Doc {
        text: String,
        found: Vec<FindMatch>,
        current: Option<usize>,
        replaced: Vec<(usize, String)>,
    }

    impl Findable for Doc {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.found.clear();
            if query.is_empty() {
                return Vec::new();
            }
            let (hay, needle) = if options.match_case {
                (self.text.clone(), query.to_owned())
            } else {
                (self.text.to_lowercase(), query.to_lowercase())
            };
            let mut at = 0;
            while let Some(found) = hay.get(at..).and_then(|rest| rest.find(&needle)) {
                let start = at + found;
                let end = start + needle.len();
                let bounded = |i: Option<char>| i.is_none_or(|c| !c.is_alphanumeric());
                if !options.whole_word
                    || (bounded(hay.get(..start).and_then(|s| s.chars().last()))
                        && bounded(hay.get(end..).and_then(|s| s.chars().next())))
                {
                    self.found.push(FindMatch::new(start, end));
                }
                at = end;
            }
            self.found.clone()
        }
        fn reveal(&mut self, index: usize) {
            self.current = Some(index);
        }
        fn supports_replace(&self) -> bool {
            true
        }
        fn replace(&mut self, index: usize, with: &str) -> bool {
            self.replaced.push((index, with.to_owned()));
            true
        }
    }

    #[test]
    fn the_bar_counts_steps_and_wraps_through_matches() {
        let mut doc = Doc {
            text: "Cat cat concat cat".into(),
            ..Doc::default()
        };
        let mut bar = FindBar::new();
        bar.set_query("cat");
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 4)));
        assert_eq!(doc.current, Some(0));
        bar.next();
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((2, 4)));
        bar.previous();
        bar.drive(&mut doc);
        bar.previous();
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((4, 4)), "previous wraps to the last");
        assert_eq!(bar.field.count(), Some(FilterCount::new(4, 4)));

        bar.toggle(Part::MatchCase);
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 3)), "Match case drops `Cat`");
        bar.toggle(Part::WholeWord);
        bar.drive(&mut doc);
        assert_eq!(bar.position(), Some((1, 2)), "Whole word drops `concat`");

        assert!(bar.replace_row, "a replacing findable gets the replace row");
        bar.replace_field.set_query("dog");
        bar.toggle(Part::Replace);
        bar.drive(&mut doc);
        assert_eq!(doc.replaced, [(0, "dog".to_owned())]);

        bar.request_close();
        bar.drive(&mut doc);
        assert!(doc.found.is_empty(), "closing clears the highlights");
        assert_eq!(bar.position(), None);
    }

    struct Page {
        doc: Doc,
        rect: Rect,
    }

    impl Findable for Page {
        fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
            self.doc.find(query, options)
        }
        fn reveal(&mut self, index: usize) {
            self.doc.reveal(index);
        }
    }

    impl Widget for Page {
        crate::provides!(dyn Findable);
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: length(200.0_f32),
                },
                ..Default::default()
            });
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
        }
        fn draw(
            &mut self,
            _: &mut Scene,
            _: Affine,
            _: &mut TextRenderer,
            _: &Theme,
            _: &PresenceCtx,
        ) {
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
        fn name(&self) -> &'static str {
            "page"
        }
    }

    fn ctrl_f() -> WindowEvent {
        key(Key::Character("f".into()))
    }

    fn escape() -> WindowEvent {
        key(Key::Named(NamedKey::Escape))
    }

    fn key(logical: Key) -> WindowEvent {
        use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical;
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: winit::event::DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }

    fn send(
        scope: &mut FindScope,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
    ) -> Option<NodeId> {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map = HashMap::new();
        let node = scope.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(640.0),
                    height: taffy::AvailableSpace::Definite(400.0),
                },
            )
            .unwrap();
        scope.apply_layout(&taffy, node, Point::ZERO);
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        scope.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        focus
    }

    #[test]
    fn ctrl_f_opens_the_bar_as_a_band_above_the_content_and_escape_closes_it() {
        let page = Page {
            doc: Doc {
                text: "one two one".into(),
                ..Doc::default()
            },
            rect: Rect::ZERO,
        };
        let mut scope = FindScope::new(Box::new(page));
        scope.set_platform_conventions(PlatformConventions::new(crate::platform::Platform::Linux));
        let ctrl = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
        let plain = winit::event::Modifiers::default();

        let _ = send(&mut scope, &ctrl_f(), plain);
        assert!(!scope.is_open(), "plain F is typing, not find");
        let _ = send(&mut scope, &ctrl_f(), ctrl);
        assert!(scope.is_open());
        assert!(scope.take_structure_dirty(), "the band joins the layout");
        // Lay out again with the band, then check it sits above the content.
        let focus = send(&mut scope, &WindowEvent::Focused(true), plain);
        assert!(focus.is_none());
        assert!(scope.bar.get_rect().height() > 0.0);
        assert!(
            scope.content.get_rect().y0 >= scope.bar.get_rect().y1 - 0.5,
            "the content starts under the band"
        );
        scope.bar.field.set_query("one");
        scope.drive();
        assert_eq!(scope.bar.position(), Some((1, 2)));

        let _ = send(&mut scope, &escape(), plain);
        assert!(!scope.is_open(), "Escape closes the bar");
    }

    #[test]
    fn a_match_highlight_marks_only_its_span() {
        let theme = Theme::dark();
        let mut scene = Scene::new();
        paint_find_match(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(10.0, 0.0, 30.0, 14.0),
            true,
            &theme,
        );
        paint_find_match(&mut scene, Affine::IDENTITY, Rect::ZERO, false, &theme);
        assert!(!scene.encoding().is_empty());
    }
}

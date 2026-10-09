//! `SearchField` — the in-content filter: a field with a leading magnifier,
//! an editable [`TextInput`], a trailing clear button and a reserved slot for
//! the match count.
//!
//! It narrows what a view already shows (a sidebar list, a table, a set of
//! cards); it never searches the app. Searching is the platform's: Enter, or
//! the "Search everywhere" action shown when nothing matches, hands the same
//! query to the platform search ([`SearchRequest`]), and an app's search
//! launcher is [`search_button`](crate::search::search_button), not a field.
//!
//! Bind a field to a list through the [`Filterable`] capability: the list
//! narrows to the query and reports how many rows remain, which the field
//! shows as "3 of 120". [`Filtered`] places a field above any content and
//! wires both, with Ctrl+F focusing the field; a navigation shell's filter
//! slot uses [`sync_filter`] the same way.
//!
//! Composes the toolkit's own [`TextInput`] so caret, selection, IME, and
//! CRDT binding are shared.

use crate::widget::ModelPath;
use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::platform::PlatformConventions;
use crate::presence::PresenceCtx;
use crate::search::{ScopeDecl, SearchOpener, SearchRequest, SearchRequestScope};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::capability::capability_mut;
use crate::widget::text::centered_text_top;
use crate::widget::{ControlSize, TaffyTreeExt};
use crate::widget::{TextInput, TextInputKind, Widget};

/// Leading gutter holding the magnifier, and the trailing clear gutter.
const ICON_GUTTER: f64 = 32.0;
const CLEAR_GUTTER: f64 = 28.0;
const ICON_D: f64 = 16.0;
const CLEAR_D: f64 = 14.0;
/// Space between the trailing slot's text and the clear button or edge.
const SLOT_PAD: f64 = 8.0;

/// How many rows a filtered view shows out of how many it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterCount {
    pub shown: usize,
    pub total: usize,
}

impl FilterCount {
    #[must_use]
    pub const fn new(shown: usize, total: usize) -> Self {
        Self { shown, total }
    }
}

/// A view a [`SearchField`] can narrow: a list, a table, a grid of cards.
pub trait Filterable {
    /// Show only what matches `query`; an empty query shows everything.
    /// Match the way search does ([`crate::search::ListIndex`]) so a filter
    /// and the platform search agree on what "matches" means.
    fn set_filter(&mut self, query: &str);

    /// How many rows the current filter leaves, for the field's counter.
    fn filter_count(&self) -> Option<FilterCount> {
        None
    }

    /// The part of the app the view shows, which "Search everywhere"
    /// narrows the platform search to. `None` searches the whole app.
    fn search_scope(&self) -> Option<ScopeDecl> {
        None
    }
}

/// Narrow the first [`Filterable`] in `widget`'s subtree to `query`, and
/// return its count.
pub fn apply_filter(widget: &mut dyn Widget, query: &str) -> Option<FilterCount> {
    let mut count = None;
    visit_filterable(widget, &mut |target| {
        target.set_filter(query);
        count = target.filter_count();
    });
    count
}

/// Run `visit` on the first [`Filterable`] in `widget`'s subtree; whether
/// there was one.
fn visit_filterable(widget: &mut dyn Widget, visit: &mut dyn FnMut(&mut dyn Filterable)) -> bool {
    if let Some(target) = capability_mut::<dyn Filterable>(&mut *widget) {
        visit(target);
        return true;
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| visit_filterable(child, visit))
}

/// Carry `field`'s edits to the [`Filterable`] in `content`: call after
/// routing each event. The field shows the count the view reports, and its
/// "Search everywhere" escalation narrows to the view's scope.
pub fn sync_filter(field: &mut SearchField, content: &mut dyn Widget) {
    if field.applied.as_deref() != Some(field.value()) {
        let query = field.value().to_owned();
        let count = apply_filter(content, &query);
        field.set_count(count.filter(|_| !query.is_empty()));
        field.applied = Some(query);
    }
    if field.escalation.is_some() {
        let mut scope = None;
        if visit_filterable(content, &mut |target| scope = target.search_scope()) {
            field.escalation =
                Some(scope.map_or(SearchRequestScope::ThisApp, SearchRequestScope::Resource));
        }
    }
}

/// A search-styled input: magnifier + editable text + count slot + clear.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent option or dirty marker (clearable, cleared, \
              reserves the count slot, a removed scope token), set and read on its own \
              schedule, so an enum encoding would not simplify the call sites"
)]
pub struct SearchField {
    input: TextInput,
    leading: Icon,
    clearable: bool,
    cleared: bool,
    rect: Rect,
    clear_rect: Rect,
    /// The "Search everywhere" action's hit rect, when it shows.
    escalate_rect: Rect,
    last_cursor: Point,
    platform: PlatformConventions,
    /// Where Enter and "Search everywhere" send the query; `None` keeps the
    /// field a plain input (the palette's own query field).
    escalation: Option<SearchRequestScope>,
    opener: SearchOpener,
    count: Option<FilterCount>,
    reserve_count: bool,
    /// A scope token drawn before the text ("Only Inbox"), removable.
    scope_token: Option<String>,
    scope_token_rect: Rect,
    scope_cleared: bool,
    size: ControlSize,
    /// The query the bound view was last narrowed to ([`sync_filter`]).
    applied: Option<String>,
    /// The placeholder as written; the input shows it elided to its width.
    placeholder: String,
}

impl SearchField {
    /// A plain search input. Use [`Self::filter`] for a field that narrows a
    /// view.
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    /// The in-content filter: Enter and "Search everywhere" escalate the
    /// query to the platform search of this app, and the count slot is
    /// reserved so the text never runs under the count.
    pub fn filter() -> Self {
        let mut field = Self::new().with_placeholder("Filter");
        field.escalation = Some(SearchRequestScope::ThisApp);
        field.reserve_count = true;
        field
    }

    #[doc(hidden)]
    pub fn with_language(language: &str) -> Self {
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
            escalate_rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            escalation: None,
            opener: SearchOpener::new(),
            count: None,
            reserve_count: false,
            scope_token: None,
            scope_token_rect: Rect::ZERO,
            scope_cleared: false,
            size: ControlSize::Regular,
            applied: None,
            placeholder: "Search".to_owned(),
        }
    }

    /// The field's height: `Regular` in content, `Large` where the field is
    /// the point of the surface (the palette's query field).
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// Escalate to the platform search of one part of the app instead of
    /// the whole app.
    #[must_use]
    pub fn escalates_within(mut self, scope: ScopeDecl) -> Self {
        self.escalation = Some(SearchRequestScope::Resource(scope));
        self
    }

    /// Reserve the trailing count slot even before a count is known.
    #[must_use]
    pub fn with_count_slot(mut self) -> Self {
        self.reserve_count = true;
        self
    }

    /// Show `count` in the trailing slot ("3 of 120"), or nothing.
    pub fn set_count(&mut self, count: Option<FilterCount>) {
        self.count = count;
    }

    #[must_use]
    pub fn count(&self) -> Option<FilterCount> {
        self.count
    }

    /// Show a removable scope token before the text, e.g. "Only Inbox".
    pub fn set_scope_token(&mut self, token: Option<String>) {
        self.scope_token = token;
        self.scope_token_rect = Rect::ZERO;
    }

    #[must_use]
    pub fn scope_token(&self) -> Option<&str> {
        self.scope_token.as_deref()
    }

    /// `true` once after the user removed the scope token (its × or
    /// Backspace in an empty field).
    pub fn take_scope_cleared(&mut self) -> bool {
        std::mem::take(&mut self.scope_cleared)
    }

    #[must_use]
    pub fn with_placeholder(mut self, hint: &str) -> Self {
        self.set_placeholder(hint);
        self
    }

    pub fn set_placeholder(&mut self, hint: &str) {
        hint.clone_into(&mut self.placeholder);
        self.input.set_placeholder(hint);
    }

    /// Give the input the placeholder elided to the room it has, so a long
    /// one ends in an ellipsis instead of running under the trailing slot.
    fn fit_placeholder(&mut self, text_renderer: &mut TextRenderer, theme: &Theme) {
        let room = (self.input.get_rect().width() - SLOT_PAD).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a field's width in logical pixels is far inside f32's range"
        )]
        let fitted = text_renderer.elide_to_width_style(
            &crate::i18n!(&self.placeholder),
            room as f32,
            &theme.text(TextRole::Body),
        );
        if fitted != self.input.placeholder() {
            self.input.set_placeholder(&fitted);
        }
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
        self.input = TextInput::new(vec![crate::presence::PathSegment::Field(path.to_owned())])
            .with_plain_chrome()
            .with_kind(TextInputKind::Search);
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

    pub fn set_focused(&mut self, on: bool) {
        self.input.set_focus(on);
    }

    /// Select the current query for conventional Find/Search re-entry.
    pub fn select_all(&mut self) {
        self.input.select_all();
    }

    /// Whether the query input owns an unfinished composition.
    pub fn is_composing(&self) -> bool {
        self.input.is_composing()
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

    /// Hand the current query to the platform search, as Enter does.
    pub fn escalate(&mut self) -> bool {
        let Some(scope) = self.escalation.clone() else {
            return false;
        };
        self.opener
            .open(SearchRequest { scope, query: None }.with_query(self.input.value()));
        true
    }

    /// Place the field at `rect` for a host that lays out by hand instead of
    /// through the taffy tree.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.input
            .apply_layout_external(self.text_rect_in(rect, 0.0));
    }

    /// Empty until laid out + drawn.
    pub fn clear_rect(&self) -> Rect {
        self.clear_rect
    }

    /// Where the input's text sits inside `rect`, past the leading gutter
    /// (and a scope token `token_w` wide) and before the trailing gutters.
    fn text_rect_in(&self, rect: Rect, token_w: f64) -> Rect {
        let x0 = rect.x0 + ICON_GUTTER + token_w;
        let x1 = (rect.x1 - self.trailing_gutter()).max(x0);
        Rect::new(x0, rect.y0, x1, rect.y1)
    }

    /// The clear button plus the reserved count slot. The slot is measured
    /// at draw time; layout reserves a fixed share so the caret never runs
    /// under the count.
    fn trailing_gutter(&self) -> f64 {
        let slot = if self.reserve_count || self.count.is_some() {
            72.0
        } else {
            0.0
        };
        CLEAR_GUTTER + slot
    }

    fn clear_visible(&self) -> bool {
        self.clearable && !self.input.value().is_empty()
    }

    /// Narrow and idle: draw only the magnifier, as an icon button.
    fn collapsed(&self, theme: &Theme) -> bool {
        let threshold =
            ICON_GUTTER + self.trailing_gutter() + 3.0 * ControlSize::Regular.height(theme);
        self.rect.width() < threshold && !self.input.is_focused() && self.input.value().is_empty()
    }

    fn escalation_shows(&self) -> bool {
        self.escalation.is_some()
            && !self.input.value().is_empty()
            && self.count.is_some_and(|count| count.shown == 0)
    }

    fn draw_trailing_slot(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.escalate_rect = Rect::ZERO;
        let mut right = if self.clear_visible() {
            self.rect.x1 - CLEAR_GUTTER
        } else {
            self.rect.x1 - SLOT_PAD
        };
        // The count stays, "0 of 120" included: it is what tells the person
        // nothing here matches.
        if let Some(count) = self.count {
            let style = theme.text(TextRole::Caption).numeric();
            let text = format!("{} of {}", count.shown, count.total);
            let (w, h) = text_renderer.measure_style(&text, None, &style);
            let x0 = right - f64::from(w);
            if x0 >= self.rect.x0 + ICON_GUTTER {
                let top = centered_text_top(self.rect.y0, self.rect.height(), f64::from(h));
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine composition is float matrix math and cannot overflow"
                )]
                let at = transform * Affine::translate((x0, top));
                text_renderer.draw_style(scene, &text, theme.text_tertiary, at, None, &style);
                right = x0 - SLOT_PAD;
            }
        }
        if self.escalation_shows() {
            // "Search everywhere" is an action, not a hint: a small
            // secondary button with its own face and edge, so it never reads
            // as a label beside the count.
            let label = "Search everywhere";
            let size = ControlSize::Small;
            let face = crate::widget::ButtonFace::new(Rect::ZERO, label)
                .role(crate::widget::ButtonRole::Secondary)
                .control_size(size);
            let width = face.intrinsic_width(text_renderer, theme);
            let height = crate::widget::ButtonFace::height(size, theme).min(self.rect.height());
            let x0 = right - width;
            if x0 < self.rect.x0 + ICON_GUTTER {
                return;
            }
            let cy = self.rect.center().y;
            let rect = Rect::new(x0, cy - height / 2.0, right, cy + height / 2.0);
            let face = crate::widget::ButtonFace::new(rect, label)
                .role(crate::widget::ButtonRole::Secondary)
                .control_size(size);
            let hit = face.hit_rect(theme);
            let hovered = hit.contains(self.last_cursor);
            crate::widget::Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &face.hovered(hovered),
            );
            self.escalate_rect = hit;
        }
    }

    /// Draw the scope token chip and return the width it takes.
    fn draw_scope_token(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let Some(token) = self.scope_token.clone() else {
            self.scope_token_rect = Rect::ZERO;
            return 0.0;
        };
        let height = ControlSize::Small.height(theme) - 6.0;
        let lead = crate::widget::ChipLead::Glyph(Icon::X);
        let natural =
            crate::widget::ChipFace::intrinsic_width(&token, lead, false, text_renderer, theme);
        let max = (self.rect.width() * 0.5).max(height);
        let width = natural.min(max);
        let cy = self.rect.center().y;
        let chip = Rect::new(
            self.rect.x0 + ICON_GUTTER - 4.0,
            cy - height / 2.0,
            self.rect.x0 + ICON_GUTTER - 4.0 + width,
            cy + height / 2.0,
        );
        crate::widget::ChipFace::new(chip, &token).lead(lead).paint(
            scene,
            transform,
            text_renderer,
            theme,
        );
        self.scope_token_rect = chip;
        width + 4.0
    }

    fn press_at(&self, point: Point) -> Press {
        if self.scope_token_rect.contains(point) {
            Press::ScopeToken
        } else if self.escalate_rect.contains(point) {
            Press::Escalate
        } else if self.clear_visible() && self.clear_rect.contains(point) {
            Press::Clear
        } else {
            Press::Field
        }
    }
}

enum Press {
    ScopeToken,
    Escalate,
    Clear,
    Field,
}

impl Default for SearchField {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchField {
    crate::provides!(dyn crate::widget::controls::ActionControls, SearchField);

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.input.set_platform_conventions(conventions);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "these are widget-geometry values — small, bounded pixel values — so \
                  narrowing to f32 for Taffy's layout math loses no meaningful precision"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let child = self.input.build_layout(taffy, theme, node_map);
        let height = self.size.height(theme);
        // A fluid control: it asks for the row's width and, when the row
        // overflows, it is the item that gives — never the fixed targets
        // beside it. At its narrowest it is the magnifier icon button.
        let style = Style {
            display: taffy::Display::Flex,
            align_items: Some(taffy::AlignItems::Center),
            flex_shrink: 1.0,
            min_size: taffy::Size {
                width: length(height as f32),
                height: taffy::Dimension::auto(),
            },
            padding: taffy::Rect {
                left: length(ICON_GUTTER as f32),
                right: length(self.trailing_gutter() as f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(height as f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
        let radius = f64::from(theme.tokens.radii.control);
        if self.collapsed(theme) {
            // The icon button: a square target at the leading edge, lifted
            // on hover like any quiet control.
            let side = self.rect.height();
            let target = Rect::new(
                self.rect.x0,
                self.rect.y0,
                self.rect.x0 + side,
                self.rect.y1,
            );
            if target.contains(self.last_cursor) {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(target, radius),
                );
            }
            let icon = Rect::from_center_size(target.center(), (ICON_D, ICON_D));
            self.leading
                .draw_with_stroke(scene, transform, icon, theme.text_dim, 1.5);
            self.clear_rect = Rect::ZERO;
            self.escalate_rect = Rect::ZERO;
            self.scope_token_rect = Rect::ZERO;
            return;
        }
        let focused = self.input.is_focused();
        let shape = RoundedRect::from_rect(self.rect, radius);
        let fill = theme.tokens.colors.surfaces.raised;
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        // Rest edge derived from the field (HIG §9.5); focus is the user's
        // colour, on one half-pixel-inset outline.
        let border = if focused {
            theme.border_focus
        } else {
            crate::surface::edge_on(fill)
        };
        scene.stroke(
            &Stroke::new(if focused { 1.5 } else { 1.0 }),
            transform,
            border,
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        let icon_rect = Rect::from_center_size(
            Point::new(self.rect.x0 + ICON_GUTTER / 2.0, self.rect.center().y),
            (ICON_D, ICON_D),
        );
        self.leading
            .draw_with_stroke(scene, transform, icon_rect, theme.text_dim, 1.5);

        let token_w = self.draw_scope_token(scene, transform, text_renderer, theme);
        if token_w > 0.0 {
            // The text starts after the token; the input re-lays its line.
            self.input
                .apply_layout_external(self.text_rect_in(self.rect, token_w));
        }
        self.fit_placeholder(text_renderer, theme);
        self.input
            .draw(scene, transform, text_renderer, theme, presence);

        self.draw_trailing_slot(scene, transform, text_renderer, theme);
        if self.clear_visible() {
            let cr = Rect::from_center_size(
                Point::new(self.rect.x1 - CLEAR_GUTTER / 2.0, self.rect.center().y),
                (CLEAR_D, CLEAR_D),
            );
            self.clear_rect = cr.inflate(4.0, 4.0);
            let col = if self.clear_rect.contains(self.last_cursor) {
                theme.text_primary
            } else {
                theme.text_dim
            };
            Icon::X.draw_with_stroke(scene, transform, cr, col, 1.5);
        } else {
            self.clear_rect = Rect::ZERO;
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        // A field placed by `apply_layout_external` has no child node; the
        // input only uses the node to report a focus target.
        let child_node = ctx
            .taffy
            .children(ctx.node_id)
            .unwrap_or_default()
            .first()
            .copied()
            .unwrap_or(ctx.node_id);
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.rect.contains(self.last_cursor)
        {
            match self.press_at(self.last_cursor) {
                Press::ScopeToken => {
                    self.scope_token = None;
                    self.scope_cleared = true;
                    *ctx.handled = true;
                    return;
                }
                Press::Escalate => {
                    self.escalate();
                    *ctx.handled = true;
                    return;
                }
                Press::Clear => {
                    self.input.set_text("");
                    self.cleared = true;
                    *ctx.handled = true;
                    return;
                }
                Press::Field => {
                    // The collapsed icon button focuses the field.
                    if !self.input.is_focused() && self.input.value().is_empty() {
                        *ctx.focus_target = Some(child_node);
                    }
                }
            }
        }
        if self.input.is_focused()
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
            match logical_key {
                Key::Named(NamedKey::Enter) if self.escalate() => {
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Backspace)
                    if self.input.value().is_empty() && self.scope_token.is_some() =>
                {
                    self.scope_token = None;
                    self.scope_cleared = true;
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Escape) if !self.input.value().is_empty() => {
                    self.input.set_text("");
                    self.cleared = true;
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        self.input
            .handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if !self.rect.contains(point) {
            return None;
        }
        match self.press_at(point) {
            Press::ScopeToken | Press::Escalate | Press::Clear => {
                Some(winit::window::CursorIcon::Pointer)
            }
            Press::Field => self.input.cursor_shape(point),
        }
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        Widget::ime_cursor_rect(&self.input)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.input.ime_edit_fingerprint(key)
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

    fn name(&self) -> &'static str {
        "SearchField"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = self
            .input
            .accessibility()?
            .with_name(if self.escalation.is_some() {
                "Filter"
            } else {
                "Search"
            });
        let mut description = Vec::new();
        if !self.placeholder.is_empty() {
            description.push(crate::i18n!(&self.placeholder));
        }
        if let Some(token) = &self.scope_token {
            description.push(token.clone());
        }
        if let Some(count) = self.count {
            description.push(format!("{} of {}", count.shown, count.total));
        }
        if !description.is_empty() {
            node = node.with_description(description.join(", "));
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Widget::perform_action(&mut self.input, action, state, path)
    }
}

impl crate::widget::controls::ActionControls for SearchField {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.opener
            .take()
            .or_else(|| crate::widget::controls::take_control_invocation(&mut self.input))
    }

    fn has_pending_control_invocation(&self) -> bool {
        self.opener.is_pending()
            || crate::widget::controls::has_pending_control_invocation(&self.input)
    }
}

/// Content with a filter: a [`SearchField::filter`] in a band above the
/// content, narrowing the first [`Filterable`] inside it. Ctrl+F (⌘F)
/// focuses the field. A navigation shell's filter slot is the same field
/// wired with [`sync_filter`].
pub struct Filtered {
    field: SearchField,
    content: crate::widget::Element,
    rect: Rect,
    field_node: Option<NodeId>,
    content_node: Option<NodeId>,
    platform: PlatformConventions,
}

impl Filtered {
    pub fn new(content: crate::widget::Element) -> Self {
        Self {
            field: SearchField::filter(),
            content,
            rect: Rect::ZERO,
            field_node: None,
            content_node: None,
            platform: PlatformConventions::default(),
        }
    }

    /// Name what the field filters ("Filter people").
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.field.set_placeholder(placeholder);
        self
    }

    #[must_use]
    pub fn field(&self) -> &SearchField {
        &self.field
    }

    pub fn field_mut(&mut self) -> &mut SearchField {
        &mut self.field
    }
}

impl Widget for Filtered {
    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        self.platform = conventions;
        self.field.set_platform_conventions(conventions);
        self.content.set_platform_conventions(conventions);
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "band paddings are small logical-pixel values"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let field = self.field.build_layout(taffy, theme, node_map);
        let gutter = f64::from(theme.tokens.spacing.s3) as f32;
        let pad = f64::from(theme.tokens.spacing.s2) as f32;
        let band = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_shrink: 0.0,
                padding: taffy::Rect {
                    left: length(gutter),
                    right: length(gutter),
                    top: length(pad),
                    bottom: length(pad),
                },
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &[field],
        );
        let content = self.content.build_layout(taffy, theme, node_map);
        if let Ok(mut style) = taffy.style(content).cloned() {
            style.flex_grow = 1.0;
            style.flex_shrink = 1.0;
            style.min_size.height = length(0.0_f32);
            let _ = taffy.set_style(content, style);
        }
        self.field_node = Some(field);
        self.content_node = Some(content);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            flex_grow: 1.0,
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[band, content], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(field) = self.field_node {
            let band = taffy.parent(field);
            let band_origin =
                band.and_then(|band| taffy.layout(band).ok())
                    .map_or(inner, |layout| {
                        Point::new(
                            inner.x + f64::from(layout.location.x),
                            inner.y + f64::from(layout.location.y),
                        )
                    });
            self.field.apply_layout(taffy, field, band_origin);
        }
        if let Some(content) = self.content_node {
            self.content.apply_layout(taffy, content, inner);
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
        self.field
            .draw(scene, transform, text_renderer, theme, presence);
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
            && c.eq_ignore_ascii_case("f")
            && self.platform.command_modifier_down(ctx.modifiers.state())
            && !ctx.modifiers.state().shift_key()
            && let Some(field) = self.field_node
        {
            let input = ctx
                .taffy
                .children(field)
                .unwrap_or_default()
                .first()
                .copied();
            *ctx.focus_target = Some(input.unwrap_or(field));
            self.field.select_all();
            *ctx.handled = true;
            return;
        }
        if let Some(field) = self.field_node {
            self.field
                .handle_event(event, &mut ctx.reborrow(ctx.path, field));
        }
        sync_filter(&mut self.field, self.content.as_mut());
        if *ctx.handled {
            return;
        }
        if let Some(content) = self.content_node {
            self.content
                .handle_event(event, &mut ctx.reborrow(ctx.path, content));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.field, self.content.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.field);
        visitor(self.content.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.field, self.content.as_mut()]
    }

    fn wants_pointer_capture(&self) -> bool {
        self.content.wants_pointer_capture()
    }

    fn name(&self) -> &'static str {
        "Filtered"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
pub(crate) fn observed_language(field: &SearchField) -> &str {
    field.input.spell_language()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Dispatch;

    fn ctx_event(
        field: &mut SearchField,
        taffy: &TaffyTree<()>,
        node: NodeId,
        event: &WindowEvent,
    ) -> (bool, Option<NodeId>) {
        let sent = Dispatch::new().layout(taffy, node).send(field, event);
        (sent.handled, sent.focus_target)
    }

    fn move_to(point: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        }
    }

    use crate::synthetic_input::mouse_press;

    pub(crate) fn key(named: NamedKey) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Named(named),
            None,
            ElementState::Pressed,
        ))
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value"
    )]
    fn lay_out(w: &mut SearchField, vw: f32) -> (TaffyTree<()>, NodeId) {
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
                    height: taffy::AvailableSpace::Definite(
                        ControlSize::Regular.height(&theme) as f32
                    ),
                },
                |k, a, id, _, _| {
                    node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                        (*p).measure(k, a, &mut tr)
                    })
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    fn draw(w: &mut dyn Widget) -> Scene {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        scene
    }

    #[test]
    fn the_field_is_a_control_with_its_text_inside_the_gutters() {
        let theme = Theme::dark();
        let mut sf = SearchField::new();
        let _ = lay_out(&mut sf, 320.0);
        assert!((sf.get_rect().width() - 320.0).abs() < 1.0, "fills its row");
        assert!(
            (sf.get_rect().height() - ControlSize::Regular.height(&theme)).abs() < 0.5,
            "a regular control's height"
        );
        let input = sf.input.get_rect();
        assert!(input.x0 >= sf.get_rect().x0 + ICON_GUTTER - 0.5);

        let mut filter = SearchField::filter();
        let _ = lay_out(&mut filter, 320.0);
        assert!(
            filter.input.get_rect().x1 <= filter.get_rect().x1 - CLEAR_GUTTER - 60.0,
            "a filter reserves the count slot before any count is known"
        );
    }

    #[test]
    fn clear_appears_with_text_and_empties_the_query() {
        let mut sf = SearchField::new();
        let (taffy, node) = lay_out(&mut sf, 320.0);
        draw(&mut sf);
        assert_eq!(
            sf.clear_rect(),
            Rect::ZERO,
            "empty query has no clear button"
        );
        sf.input.set_text("query text");
        draw(&mut sf);
        let target = sf.clear_rect().center();
        assert!(target.x > 0.0);
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(target));
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        assert!(handled);
        assert!(sf.is_empty());
        assert_eq!(sf.take_dirty().as_deref(), Some(""));
        assert_eq!(sf.take_dirty(), None);
    }

    #[test]
    fn a_narrow_idle_field_is_an_icon_button_that_focuses_the_field() {
        let mut sf = SearchField::filter();
        let (taffy, node) = lay_out(&mut sf, 90.0);
        assert!(sf.collapsed(&Theme::dark()));
        draw(&mut sf);
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(Point::new(10.0, 10.0)));
        let (_, focus) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        let input_node = taffy.children(node).unwrap()[0];
        assert_eq!(focus, Some(input_node), "the icon button focuses the field");
        sf.input.set_focus(true);
        assert!(
            !sf.collapsed(&Theme::dark()),
            "a focused field is a field again"
        );

        let mut wide = SearchField::filter();
        let _ = lay_out(&mut wide, 400.0);
        assert!(!wide.collapsed(&Theme::dark()));
    }

    #[test]
    fn enter_and_search_everywhere_escalate_the_query_to_platform_search() {
        use crate::widget::controls::take_control_invocation;

        let mut plain = SearchField::new();
        plain.input.set_text("x");
        assert!(!plain.escalate(), "the palette's own field never escalates");
        assert!(take_control_invocation(&mut plain).is_none());

        let mut sf =
            SearchField::filter().escalates_within(ScopeDecl::new("folder:inbox", "Inbox"));
        let (taffy, node) = lay_out(&mut sf, 360.0);
        sf.input.set_text("invoice");
        sf.input.set_focus(true);
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &key(NamedKey::Enter));
        assert!(handled);
        let invocation = take_control_invocation(&mut sf).expect("Enter escalates");
        let request = SearchRequest::from_invocation(&invocation).expect("a search request");
        assert_eq!(request.query.as_deref(), Some("invoice"));
        assert_eq!(
            request.resource_scope().map(|s| s.label.as_str()),
            Some("Inbox")
        );
        assert!(
            take_control_invocation(&mut sf).is_none(),
            "one request per Enter"
        );

        // With nothing matching, the count slot offers the same escalation.
        sf.set_count(Some(FilterCount::new(0, 12)));
        sf.input.set_focus(false);
        draw(&mut sf);
        assert_ne!(sf.escalate_rect, Rect::ZERO, "Search everywhere shows");
        let target = sf.escalate_rect.center();
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(target));
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        assert!(handled);
        assert!(take_control_invocation(&mut sf).is_some());
        sf.set_count(Some(FilterCount::new(3, 12)));
        draw(&mut sf);
        assert_eq!(sf.escalate_rect, Rect::ZERO, "a count shows instead");
    }

    #[test]
    fn the_scope_token_is_removed_by_its_cross_or_backspace() {
        let mut sf = SearchField::new();
        let (taffy, node) = lay_out(&mut sf, 400.0);
        sf.set_scope_token(Some("Only Inbox".into()));
        draw(&mut sf);
        assert!(sf.scope_token_rect.width() > 0.0);
        assert!(
            sf.input.get_rect().x0 >= sf.scope_token_rect.x1,
            "the text starts after the token"
        );
        sf.input.set_focus(true);
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &key(NamedKey::Backspace));
        assert!(handled);
        assert!(sf.take_scope_cleared());
        assert!(sf.scope_token().is_none());

        sf.set_scope_token(Some("Only Inbox".into()));
        draw(&mut sf);
        let target = sf.scope_token_rect.center();
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(target));
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        assert!(handled);
        assert!(sf.take_scope_cleared());
    }

    #[test]
    fn the_placeholder_stays_while_focused_and_empty() {
        let mut sf = SearchField::filter().with_placeholder("Filter containers");
        let _ = lay_out(&mut sf, 320.0);
        sf.input.set_focus(true);
        assert!(sf.input.shows_placeholder());
        sf.input.set_text("a");
        assert!(!sf.input.shows_placeholder());
    }

    #[test]
    fn accessibility_names_the_field_and_reads_its_count() {
        use crate::a11y::Role;
        let mut sf = SearchField::filter();
        sf.input.set_text("abc");
        sf.set_count(Some(FilterCount::new(2, 9)));
        let node = Widget::accessibility(&sf).expect("a11y");
        assert_eq!(node.role, Role::TextInput);
        assert_eq!(node.value.as_deref(), Some("abc"));
        assert_eq!(node.name.as_deref(), Some("Filter"));
        assert!(
            node.description
                .as_deref()
                .is_some_and(|d| d.contains("2 of 9"))
        );
    }

    struct Rows {
        all: Vec<&'static str>,
        query: String,
    }

    impl Filterable for Rows {
        fn set_filter(&mut self, query: &str) {
            self.query = query.to_owned();
        }
        fn filter_count(&self) -> Option<FilterCount> {
            let shown = self
                .all
                .iter()
                .filter(|row| row.contains(self.query.as_str()))
                .count();
            Some(FilterCount::new(shown, self.all.len()))
        }
        fn search_scope(&self) -> Option<ScopeDecl> {
            Some(ScopeDecl::new("list:people", "People"))
        }
    }

    impl Widget for Rows {
        crate::provides!(dyn Filterable);
        crate::testing::inert_widget_methods!(leaf_layout, apply_layout, draw, get_rect);
        fn name(&self) -> &'static str {
            "rows"
        }
    }

    #[test]
    fn a_filter_field_narrows_a_filterable_view_and_escalates_to_its_scope() {
        let mut rows = Rows {
            all: vec!["ada", "alan", "grace"],
            query: String::new(),
        };
        let mut sf = SearchField::filter();
        sf.input.set_text("a");
        sync_filter(&mut sf, &mut rows);
        assert_eq!(rows.query, "a");
        assert_eq!(sf.count(), Some(FilterCount::new(3, 3)));
        assert_eq!(
            sf.escalation,
            Some(SearchRequestScope::Resource(ScopeDecl::new(
                "list:people",
                "People"
            )))
        );
        sf.input.set_text("");
        sync_filter(&mut sf, &mut rows);
        assert_eq!(sf.count(), None, "an empty filter shows no count");
    }
}

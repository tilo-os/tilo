//! `SearchField` — the in-content filter: a field with a leading magnifier, an editable
//! [`TextInput`], a trailing clear button and a reserved slot for the match count.

use crate::i18n::widget_text;
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
use crate::search::{ScopeDecl, SearchOpener, SearchRequest};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::DrawChild;
use crate::widget::capability::capability_mut;
use crate::widget::text::centered_text_top;
use crate::widget::{ControlSize, TaffyTreeExt};
use crate::widget::{TextInput, TextInputKind, Widget};

const ICON_GUTTER: f64 = 32.0;
const CLEAR_GUTTER: f64 = 28.0;
const ICON_D: f64 = 16.0;
const CLEAR_D: f64 = 14.0;
const SLOT_PAD: f64 = 8.0;

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

    /// What the field's counter says: "3 of 12", or "No matches" when
    /// nothing matches.
    #[must_use]
    pub fn label(self) -> String {
        if self.shown == 0 {
            crate::i18n::widget_format("search-no-matches", &[])
        } else {
            format!("{} of {}", self.shown, self.total)
        }
    }
}

/// A view a [`SearchField`] can narrow: a list, a table, a grid of cards.
pub trait Filterable {
    /// Show only what matches `query`; an empty query shows everything.
    /// Match the way search does ([`crate::search::ListIndex`]) so a filter
    /// and the platform search agree on what "matches" means.
    fn set_filter(&mut self, query: &str);

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
pub fn with_filterable(
    widget: &mut dyn Widget,
    visit: &mut dyn FnMut(&mut dyn Filterable),
) -> bool {
    visit_filterable(widget, visit)
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

/// Carry `field`'s edits to the [`Filterable`] in `content`: call after routing each event.
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
                Some(scope.map_or_else(SearchRequest::this_app, SearchRequest::within));
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent option or dirty marker (clearable, cleared, \
              reserves the count slot, a removed scope token), set and read on its own \
              schedule, so an enum encoding would not simplify the call sites"
)]
#[crate::component::not_a_component(
    system,
    "no search fields in apps; the search button opens system search (AGENTS.md principle 3)"
)]
pub struct SearchField {
    input: TextInput,
    leading: Icon,
    clearable: bool,
    cleared: bool,
    rect: Rect,
    clear_rect: Rect,
    escalate_rect: Rect,
    escalate_face: Rect,
    last_cursor: Point,
    platform: PlatformConventions,
    /// Where Enter and "Search everywhere" send the query; `None` keeps the
    /// field a plain input (the palette's own query field).
    /// The search this field escalates to, with the query it holds.
    escalation: Option<SearchRequest>,
    opener: SearchOpener,
    count: Option<FilterCount>,
    reserve_count: bool,
    scope_token: Option<String>,
    /// The glyph the token starts with: what it narrowed into, never a
    /// character baked into its text.
    scope_token_icon: Icon,
    scope_token_rect: Rect,
    /// The count's text in the trailing slot, placed by the layout (empty
    /// when there is none, or none fits).
    count_rect: Rect,
    prompt_shown: Option<String>,
    prompt_rect: Rect,
    scope_cleared: bool,
    size: ControlSize,
    /// The query the bound view was last narrowed to ([`sync_filter`]).
    applied: Option<String>,
    /// The placeholder as written; the input shows it elided to its width.
    placeholder: String,
    /// Standing in a floating panel's header: no fill, no outline
    /// ([`crate::widget::primitives::FloatingPanel`]).
    embedded: bool,
    /// What the field is asking for, drawn dim before the text ("New
    /// name:") while a step collects a value in it.
    prompt: Option<String>,

    layout_theme: Theme,
    /// A count, token, prompt or placeholder changed since the layout last
    /// placed the controls. A change of the box or of the value alone shows in
    /// [`Self::is_layout_dirty`].
    layout_dirty: bool,
    /// What the last placement was taken from, besides the box: whether the
    /// field held anything, and whether it stood as its collapsed icon button.
    placed_empty: bool,
    placed_collapsed: bool,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
}

impl SearchField {
    /// A plain search input. Use [`Self::filter`] for a field that narrows a
    /// view.
    pub fn new() -> Self {
        Self::with_input(TextInput::local())
    }

    /// A search input bound to `state`: what the person types updates it, and a change to it
    /// through any clone shows here, as [`TextInput::from_state`] does for an ordinary field.
    #[must_use]
    pub fn from_state(state: &crate::LocalState<String>) -> Self {
        Self::with_input(TextInput::from_state(state))
    }

    /// The in-content filter: Enter and "Search everywhere" escalate the
    /// query to the platform search of this app, and the count slot is
    /// reserved so the text never runs under the count.
    pub fn filter() -> Self {
        let mut field =
            Self::new().with_placeholder(&crate::i18n::widget_format("search-field-filter", &[]));
        field.escalation = Some(SearchRequest::this_app());
        field.reserve_count = true;
        field
    }

    #[doc(hidden)]
    pub fn with_language(language: &str) -> Self {
        Self::with_input(TextInput::local_with_language(language))
    }

    fn with_input(input: TextInput) -> Self {
        Self {
            dirty: true,
            input: input
                .with_plain_chrome()
                .with_kind(TextInputKind::Search)
                .with_placeholder(crate::i18n::widget_format("search-field-placeholder", &[])),
            leading: Icon::Search,
            clearable: true,
            cleared: false,
            rect: Rect::ZERO,
            clear_rect: Rect::ZERO,
            escalate_rect: Rect::ZERO,
            escalate_face: Rect::ZERO,
            last_cursor: Point::ZERO,
            platform: PlatformConventions::default(),
            escalation: None,
            opener: SearchOpener::new(),
            count: None,
            reserve_count: false,
            scope_token: None,
            scope_token_icon: Icon::ChevronLeft,
            scope_token_rect: Rect::ZERO,
            count_rect: Rect::ZERO,
            prompt_shown: None,
            prompt_rect: Rect::ZERO,
            scope_cleared: false,
            size: ControlSize::Regular,
            applied: None,
            placeholder: "Search".to_owned(),
            embedded: false,
            prompt: None,
            layout_theme: Theme::default(),
            layout_dirty: true,
            placed_empty: true,
            placed_collapsed: false,
        }
    }

    /// The field's height: `Regular` in content, `Large` where the field is
    /// the point of the surface (the palette's query field).
    #[must_use]
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub fn escalates_within(mut self, scope: ScopeDecl) -> Self {
        self.escalation = Some(SearchRequest::within(scope));
        self
    }

    #[must_use]
    pub fn with_count_slot(mut self) -> Self {
        self.reserve_count = true;
        self
    }

    /// Show `count` in the trailing slot ("3 of 120"), or nothing.
    pub fn set_count(&mut self, count: Option<FilterCount>) {
        self.dirty = true;
        self.layout_dirty = true;
        self.count = count;
    }

    #[must_use]
    pub fn count(&self) -> Option<FilterCount> {
        self.count
    }

    pub fn set_scope_token(&mut self, token: Option<String>) {
        self.dirty = true;
        self.layout_dirty = true;
        self.scope_token = token;
    }

    /// The glyph the scope token starts with: a chevron by default, since the token names the
    /// scope the query was narrowed into.
    pub fn set_scope_token_icon(&mut self, icon: Icon) {
        self.dirty = true;
        self.layout_dirty = true;
        self.scope_token_icon = icon;
    }

    #[must_use]
    pub fn scope_token(&self) -> Option<&str> {
        self.scope_token.as_deref()
    }

    /// Where the scope token was last drawn; empty without one.
    #[must_use]
    pub fn scope_token_rect(&self) -> Rect {
        self.scope_token_rect
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
        self.dirty = true;
        self.layout_dirty = true;
        hint.clone_into(&mut self.placeholder);
        self.input.set_placeholder(hint);
    }

    /// The words the field shows while it holds nothing, which a step that
    /// repurposes it reads out as what it asks for.
    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// Give the input the placeholder elided to the room it has, so a long one ends in an
    /// ellipsis instead of running under the trailing slot.
    fn fit_placeholder(&mut self, fitted: &str) {
        if fitted != self.input.placeholder() {
            self.input.set_placeholder(fitted);
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.input = std::mem::replace(&mut self.input, TextInput::local()).with_id(id);
        self
    }

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

    /// Stand in a floating panel's header: no fill and no outline of its
    /// own. [`crate::widget::primitives::FloatingPanel`] sets it on a field
    /// placed in its header slot.
    pub fn set_embedded(&mut self, embedded: bool) {
        self.dirty = true;
        self.embedded = embedded;
    }

    /// Ask for a value with `prompt` drawn dim before the text, or `None`
    /// for a plain query.
    pub fn set_prompt(&mut self, prompt: Option<String>) {
        self.dirty = true;
        self.layout_dirty = true;
        self.prompt = prompt;
    }

    #[must_use]
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// What names this field and what its description adds, for a surface that
    /// draws the field itself and builds its own accessible node: the field
    /// named as the search field it is, and what the query covers — the empty
    /// field's words, the token narrowing it, the count it found.
    #[must_use]
    pub fn query_name_and_description(&self) -> (String, Option<String>) {
        let name = if self.escalation.is_some() {
            crate::i18n::widget_format("search-field-filter-name", &[])
        } else {
            crate::i18n::widget_format("search-field-name", &[])
        };
        let mut description = Vec::new();
        if !self.placeholder.is_empty() {
            description.push(widget_text(&self.placeholder));
        }
        if let Some(token) = &self.scope_token {
            description.push(token.clone());
        }
        if let Some(count) = self.count {
            description.push(count.label());
        }
        (
            name,
            (!description.is_empty()).then(|| description.join(", ")),
        )
    }

    /// What the field holds, which decides its keyboard and how it reads
    /// to an input method; a secret is masked, kept off the clipboard and
    /// never read out ([`TextInput::set_secret`]).
    pub fn set_input_kind(&mut self, kind: TextInputKind, secret: bool) {
        self.dirty = true;
        self.input.set_secret(secret);
        self.input.set_kind(if secret {
            TextInputKind::Password
        } else {
            kind
        });
    }

    /// Select `anchor..cursor` (bytes) of the text.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.dirty = true;
        self.input.set_selection(anchor, cursor);
    }

    #[must_use]
    pub fn without_clear(mut self) -> Self {
        self.clearable = false;
        self
    }

    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// Where the caret stands in the text, in bytes, as the input reports it
    /// for review.
    #[must_use]
    pub fn caret_byte(&self) -> Option<usize> {
        self.input.text_review().map(|(caret, _)| caret)
    }

    pub fn set_query(&mut self, text: &str) {
        self.dirty = true;
        self.input.set_text(text);
    }

    /// Insert committed text (an input method's or a dictation's) at the
    /// caret, replacing the selection, as the input method's commit does.
    pub fn insert_at_cursor(&mut self, text: &str) {
        self.input.insert_at_cursor(text);
    }

    pub fn set_focused(&mut self, on: bool) {
        self.dirty = true;
        self.input.set_focus(on);
    }

    /// Whether the field holds the keyboard.
    pub fn is_focused(&self) -> bool {
        self.input.is_focused()
    }

    /// The field's selection, when a chrome surface owning it outside the
    /// retained tree needs to mirror pointer placement back into its own
    /// query model.
    pub fn selection_anchor_focus_public(&self) -> Option<(usize, usize)> {
        self.input.selection_anchor_focus_public()
    }

    /// The selection a host should paint after a pointer event, collapsed to
    /// the caret when nothing is selected.
    pub fn selection_anchor_focus_or_caret(&self) -> (usize, usize) {
        self.input.selection_anchor_focus_or_caret()
    }

    /// Handle a pointer event for a chrome surface that lays this field out
    /// itself. Returns whether the field took it.
    pub fn handle_chrome_pointer(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        self.input
            .handle_chrome_pointer(event, scale_factor, modifiers)
    }

    pub fn select_all(&mut self) {
        self.input.select_all();
    }

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

    pub fn escalate(&mut self) -> bool {
        let Some(request) = self.escalation.clone() else {
            return false;
        };
        self.opener.open(request.with_query(self.input.value()));
        true
    }

    /// Place the field at `rect` in `theme`, for a host that lays out by hand
    /// instead of through the taffy tree: the field and its controls stand
    /// where this puts them, and painting only paints.
    pub fn apply_layout_external(&mut self, rect: Rect, theme: &Theme) {
        self.rect = rect;
        self.layout_theme = theme.clone();
        self.place_controls();
    }

    /// Where the clear button stands, empty until laid out.
    pub fn clear_rect(&self) -> Rect {
        self.clear_rect
    }

    /// The clear button plus the reserved count slot. The slot is measured at
    /// layout time; layout reserves a fixed share so the caret never runs
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

    /// Where the field's controls stand, from its box: the scope token, the prompt, the room
    /// the text has, the count, "Search everywhere" and the clear button.
    fn place_controls(&mut self) {
        let theme = self.layout_theme.clone();
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 || self.collapsed(&theme) {
            self.clear_rect = Rect::ZERO;
            self.escalate_rect = Rect::ZERO;
            self.escalate_face = Rect::ZERO;
            self.scope_token_rect = Rect::ZERO;
            self.count_rect = Rect::ZERO;
            self.prompt_shown = None;
            self.prompt_rect = Rect::ZERO;
        } else {
            let token = self.scope_token.clone();
            let icon = self.scope_token_icon;
            let prompt = self.prompt.clone();
            let count = self.count;
            let placeholder = self.placeholder.clone();
            let clear = self.clear_visible();
            let escalation = self.escalation_shows();
            let trailing = self.trailing_gutter();
            let placed = crate::text_render::with_layout_renderer(&theme, |text| {
                let (token_rect, token_w) = token.as_deref().map_or((Rect::ZERO, 0.0), |token| {
                    place_scope_token(text, &theme, rect, token, icon)
                });
                let (prompt_shown, prompt_rect, prompt_w) = prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty())
                    .map_or((None, Rect::ZERO, 0.0), |prompt| {
                        place_prompt(text, &theme, rect, token_w, prompt)
                    });
                let text_rect = text_rect_in(rect, trailing, token_w + prompt_w);
                let (count_rect, right) = place_count(text, &theme, rect, clear, count);
                let (escalate_face, escalate_rect) = if escalation {
                    place_escalation(text, &theme, rect, right).unwrap_or((Rect::ZERO, Rect::ZERO))
                } else {
                    (Rect::ZERO, Rect::ZERO)
                };
                let clear_rect = if clear {
                    clear_hit_rect(rect)
                } else {
                    Rect::ZERO
                };
                Placed {
                    scope_token: token_rect,
                    prompt_shown,
                    prompt_rect,
                    count_rect,
                    escalate_face,
                    escalate_rect,
                    clear: clear_rect,
                    text_rect,
                    placeholder: fitted_placeholder(text, &theme, text_rect, &placeholder),
                }
            });
            self.scope_token_rect = placed.scope_token;
            self.prompt_shown = placed.prompt_shown;
            self.prompt_rect = placed.prompt_rect;
            self.count_rect = placed.count_rect;
            self.escalate_face = placed.escalate_face;
            self.escalate_rect = placed.escalate_rect;
            self.clear_rect = placed.clear;
            self.input.apply_layout_external(placed.text_rect);
            self.fit_placeholder(&placed.placeholder);
        }
        self.placed_empty = self.input.value().is_empty();
        self.placed_collapsed = self.collapsed(&theme);
        self.layout_dirty = false;
    }

    fn paint_scope_token(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(token) = self.scope_token.as_deref() else {
            return;
        };
        let chip = self.scope_token_rect;
        if chip.width() <= 0.0 {
            return;
        }
        let lead = crate::widget::ChipLead::Glyph(self.scope_token_icon);
        crate::widget::ChipFace::new(chip, token)
            .lead(lead)
            .removable(chip.contains(self.last_cursor))
            .paint(scene, transform, text_renderer, theme);
    }

    fn paint_prompt(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(shown) = self.prompt_shown.as_deref() else {
            return;
        };
        let style = theme.text(TextRole::Body);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine composition is float matrix math and cannot overflow"
        )]
        let at = transform * Affine::translate((self.prompt_rect.x0, self.prompt_rect.y0));
        text_renderer.draw_style(scene, shown, theme.text_dim, at, None, &style);
    }

    fn paint_trailing_slot(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if let Some(count) = self.count
            && !self.count_rect.is_zero_area()
        {
            let style = theme.text(TextRole::Caption).numeric();
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "Affine composition is float matrix math and cannot overflow"
            )]
            let at = transform * Affine::translate((self.count_rect.x0, self.count_rect.y0));
            text_renderer.draw_style(scene, &count.label(), theme.text_tertiary, at, None, &style);
        }
        if !self.escalate_face.is_zero_area() {
            // "Search everywhere" is an action, not a hint: a small
            // secondary button with its own face and edge, so it never reads
            // as a label beside the count.
            let face = crate::widget::ButtonFace::new(self.escalate_face, "Search everywhere")
                .role(crate::widget::ButtonRole::Secondary)
                .control_size(ControlSize::Small);
            let hovered = self.escalate_rect.contains(self.last_cursor);
            crate::widget::Button::paint(
                scene,
                transform,
                text_renderer,
                theme,
                &face.hovered(hovered),
            );
        }
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

struct Placed {
    scope_token: Rect,
    prompt_shown: Option<String>,
    prompt_rect: Rect,
    count_rect: Rect,
    escalate_face: Rect,
    escalate_rect: Rect,
    clear: Rect,
    text_rect: Rect,
    placeholder: String,
}

/// Where the input's text sits inside `rect`, past the leading gutter and a
/// scope token and prompt `lead_w` wide, and before the trailing gutters.
fn text_rect_in(rect: Rect, trailing: f64, lead_w: f64) -> Rect {
    let x0 = rect.x0 + ICON_GUTTER + lead_w;
    let x1 = (rect.x1 - trailing).max(x0);
    Rect::new(x0, rect.y0, x1, rect.y1)
}

/// The clear button's × in `rect`: centred in the trailing gutter.
fn clear_icon_rect(rect: Rect) -> Rect {
    Rect::from_center_size(
        Point::new(rect.x1 - CLEAR_GUTTER / 2.0, rect.center().y),
        (CLEAR_D, CLEAR_D),
    )
}

fn clear_hit_rect(rect: Rect) -> Rect {
    clear_icon_rect(rect).inflate(4.0, 4.0)
}

/// Where the scope token chip stands in `rect`, and the room it takes before
/// the text; empty without a token.
fn place_scope_token(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    token: &str,
    icon: Icon,
) -> (Rect, f64) {
    let height = ControlSize::Small.height(theme) - 6.0;
    let lead = crate::widget::ChipLead::Glyph(icon);
    let natural = crate::widget::ChipFace::intrinsic_width(token, lead, true, text, theme);
    let max = (rect.width() * 0.5).max(height);
    let width = natural.min(max);
    let cy = rect.center().y;
    let chip = Rect::new(
        rect.x0 + ICON_GUTTER - 4.0,
        cy - height / 2.0,
        rect.x0 + ICON_GUTTER - 4.0 + width,
        cy + height / 2.0,
    );
    (chip, width + 4.0)
}

/// The prompt after `lead` px of the text's start, elided to half the field:
/// the words it shows, where it stands, and the room it takes.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a field's width in logical pixels is far inside f32's range"
)]
fn place_prompt(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    lead: f64,
    prompt: &str,
) -> (Option<String>, Rect, f64) {
    let style = theme.text(TextRole::Body);
    let heading = format!("{prompt}:");
    let max = (rect.width() * 0.5).max(0.0);
    let shown = text.elide_to_width_style(&heading, max as f32, &style);
    let (w, h) = text.measure_style(&shown, None, &style);
    let top = centered_text_top(rect.y0, rect.height(), f64::from(h));
    let x0 = rect.x0 + ICON_GUTTER + lead;
    (
        Some(shown),
        Rect::new(x0, top, x0 + f64::from(w), top + f64::from(h)),
        f64::from(w) + f64::from(theme.tokens.spacing.s2),
    )
}

/// The count's text where the trailing slot holds it, and the room left of it
/// for "Search everywhere": empty when it does not fit, so the caret never
/// runs under a count too wide for the slot.
fn place_count(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    clear: bool,
    count: Option<FilterCount>,
) -> (Rect, f64) {
    let right = if clear {
        rect.x1 - CLEAR_GUTTER
    } else {
        rect.x1 - SLOT_PAD
    };
    let Some(count) = count else {
        return (Rect::ZERO, right);
    };
    let style = theme.text(TextRole::Caption).numeric();
    let (w, h) = text.measure_style(&count.label(), None, &style);
    let x0 = right - f64::from(w);
    if x0 < rect.x0 + ICON_GUTTER {
        return (Rect::ZERO, right);
    }
    let top = centered_text_top(rect.y0, rect.height(), f64::from(h));
    (
        Rect::new(x0, top, x0 + f64::from(w), top + f64::from(h)),
        x0 - SLOT_PAD,
    )
}

/// "Search everywhere", its right edge at `right`: its face and its hit
/// target, or nothing when it leaves the field no room for the text.
fn place_escalation(
    text: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    right: f64,
) -> Option<(Rect, Rect)> {
    let label = "Search everywhere";
    let size = ControlSize::Small;
    let width = crate::widget::ButtonFace::new(Rect::ZERO, label)
        .role(crate::widget::ButtonRole::Secondary)
        .control_size(size)
        .intrinsic_width(text, theme);
    let height = crate::widget::ButtonFace::height(size, theme).min(rect.height());
    let x0 = right - width;
    if x0 < rect.x0 + ICON_GUTTER {
        return None;
    }
    let cy = rect.center().y;
    let face = Rect::new(x0, cy - height / 2.0, right, cy + height / 2.0);
    let hit = crate::widget::ButtonFace::new(face, label)
        .role(crate::widget::ButtonRole::Secondary)
        .control_size(size)
        .hit_rect(theme);
    Some((face, hit))
}

/// `placeholder` elided to the room `text_rect` gives it, so a long one ends in
/// an ellipsis instead of running under the trailing slot.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a field's width in logical pixels is far inside f32's range"
)]
fn fitted_placeholder(
    text: &mut TextRenderer,
    theme: &Theme,
    text_rect: Rect,
    placeholder: &str,
) -> String {
    let room = (text_rect.width() - SLOT_PAD).max(0.0);
    text.elide_to_width_style(
        &widget_text(placeholder),
        room as f32,
        &theme.text(TextRole::Body),
    )
}

impl Default for SearchField {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchField {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

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
        self.layout_theme = theme.clone();
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
            overflow: taffy::Point {
                x: taffy::Overflow::Clip,
                y: taffy::Overflow::Clip,
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if let Some(&child_node) = taffy.children(node).unwrap_or_default().first() {
            self.input
                .apply_layout(taffy, child_node, self.rect.origin());
        }
        self.place_controls();
    }

    /// The layout places a field's controls from its box and what it holds: a
    /// count, a token, a prompt, a value that brings the clear button or
    /// "Search everywhere" on, or the width that collapses it to its icon.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
            || self.layout_dirty
            || self.placed_empty != self.input.value().is_empty()
            || self.placed_collapsed != self.collapsed(&self.layout_theme)
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
            return;
        }
        if !self.embedded {
            let shape = RoundedRect::from_rect(self.rect, radius);
            let fill = theme.tokens.colors.surfaces.raised;
            let bordered = crate::surface::control_face(scene, transform, &shape, fill, theme);
            if theme.colors_are_forced() && !bordered {
                let focused = self.input.is_focused();
                scene.stroke(
                    &Stroke::new(if focused { 2.0 } else { 1.0 }),
                    transform,
                    if focused {
                        theme.border_focus
                    } else {
                        crate::surface::edge_on(fill)
                    },
                    None,
                    &crate::surface::edge_outline(self.rect, radius),
                );
            }
        }

        let icon_rect = Rect::from_center_size(
            Point::new(self.rect.x0 + ICON_GUTTER / 2.0, self.rect.center().y),
            (ICON_D, ICON_D),
        );
        self.leading
            .draw_with_stroke(scene, transform, icon_rect, theme.text_dim, 1.5);

        self.paint_scope_token(scene, transform, text_renderer, theme);
        self.paint_prompt(scene, transform, text_renderer, theme);
        self.input
            .draw_child(scene, transform, text_renderer, theme, presence);

        self.paint_trailing_slot(scene, transform, text_renderer, theme);
        if self.clear_visible() && !self.clear_rect.is_zero_area() {
            let col = if self.clear_rect.contains(self.last_cursor) {
                theme.text_primary
            } else {
                theme.text_dim
            };
            Icon::X.draw_with_stroke(scene, transform, clear_icon_rect(self.rect), col, 1.5);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
        }
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
                    self.set_scope_token(None);
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
                    self.set_scope_token(None);
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

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Widget::ime_content(&self.input)
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

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "SearchField"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let (name, description) = self.query_name_and_description();
        let mut node = self.input.accessibility()?.with_name(name);
        if let Some(description) = description {
            node = node.with_description(description);
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        Widget::perform_action(&mut self.input, action, &[], state, path)
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

/// Content with a filter: a [`SearchField::filter`] in a band above the content, narrowing the
/// first [`Filterable`] inside it. Ctrl+F (⌘F) focuses the field.
#[crate::component::not_a_component(
    system,
    "no search fields in apps; the search button opens system search (AGENTS.md principle 3)"
)]
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
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            ..Default::default()
        };
        let style = crate::widget::layout::filling(style, 1.0);
        taffy.widget_node(style, &[band, content], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let Some(field) = self.field_node {
            let band = taffy.parent(field);
            let band_origin = band.map_or(inner, |band| {
                crate::widget::layout_rect(taffy, band, inner).origin()
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
            .draw_child(scene, transform, text_renderer, theme, presence);
        self.content
            .draw_child(scene, transform, text_renderer, theme, presence);
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
        crate::synthetic_input::cursor_moved(point.x, point.y)
    }

    use crate::synthetic_input::mouse_press;

    pub(crate) fn key(named: NamedKey) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            Key::Named(named),
            None,
            ElementState::Pressed,
        ))
    }

    /// `w` laid out `vw` wide in `theme`, as a frame lays it out; it is never
    /// drawn, so only the layout has placed anything.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "test-only layout size is a small pixel value"
    )]
    fn lay_out_in(w: &mut SearchField, vw: f32, theme: &Theme) -> (TaffyTree<()>, NodeId) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut tr = TextRenderer::new();
        let node = w.build_layout(&mut taffy, theme, &mut node_map);
        taffy
            .compute_layout_with_measure(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(vw),
                    height: taffy::AvailableSpace::Definite(
                        ControlSize::Regular.height(theme) as f32
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

    fn lay_out(w: &mut SearchField, vw: f32) -> (TaffyTree<()>, NodeId) {
        lay_out_in(w, vw, &Theme::dark())
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
        let _ = lay_out(&mut sf, 320.0);
        assert_eq!(
            sf.clear_rect(),
            Rect::ZERO,
            "empty query has no clear button"
        );
        sf.input.set_text("query text");
        let (taffy, node) = lay_out(&mut sf, 320.0);
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
        sf.input.set_text("invoice");
        sf.input.set_focus(true);
        let (taffy, node) = lay_out(&mut sf, 360.0);
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &key(NamedKey::Enter));
        assert!(handled);
        let invocation = take_control_invocation(&mut sf).expect("Enter escalates");
        let request = SearchRequest::from_invocation(&invocation).expect("a search request");
        assert_eq!(request.query.as_deref(), Some("invoice"));
        assert_eq!(request.part().map(|s| s.label.as_str()), Some("Inbox"));
        assert!(
            take_control_invocation(&mut sf).is_none(),
            "one request per Enter"
        );

        sf.set_count(Some(FilterCount::new(0, 12)));
        sf.input.set_focus(false);
        let (taffy, node) = lay_out(&mut sf, 360.0);
        assert_ne!(sf.escalate_rect, Rect::ZERO, "Search everywhere shows");
        let target = sf.escalate_rect.center();
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(target));
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        assert!(handled);
        assert!(take_control_invocation(&mut sf).is_some());
        sf.set_count(Some(FilterCount::new(3, 12)));
        lay_out(&mut sf, 360.0);
        assert_eq!(sf.escalate_rect, Rect::ZERO, "a count shows instead");
    }

    #[test]
    fn the_scope_token_is_removed_by_its_cross_or_backspace() {
        let mut sf = SearchField::new();
        let _ = lay_out(&mut sf, 400.0);
        sf.set_scope_token(Some("Only Inbox".into()));
        let (taffy, node) = lay_out(&mut sf, 400.0);
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
        lay_out(&mut sf, 400.0);
        let target = sf.scope_token_rect.center();
        let _ = ctx_event(&mut sf, &taffy, node, &move_to(target));
        let (handled, _) = ctx_event(&mut sf, &taffy, node, &mouse_press());
        assert!(handled);
        assert!(sf.take_scope_cleared());
    }

    /// The layout places every control a field can show: a frame read before
    /// it is drawn — a test, assistive technology, a press on a window that
    /// has laid out and has not yet painted — finds the scope token, the
    /// count, "Search everywhere" and the clear button inside the field,
    /// rather than at the window's origin, where a control nothing placed
    /// stands.
    #[test]
    fn the_layout_places_the_controls_without_a_paint() {
        let mut sf = SearchField::filter();
        sf.set_scope_token(Some("Only Inbox".into()));
        sf.input.set_text("zzz");
        sf.set_count(Some(FilterCount::new(0, 12)));
        // Laid out, never drawn.
        let _ = lay_out(&mut sf, 420.0);
        let field = sf.get_rect();
        for (what, control) in [
            ("the scope token", sf.scope_token_rect),
            ("the count", sf.count_rect),
            ("Search everywhere", sf.escalate_face),
        ] {
            assert!(
                !control.is_zero_area(),
                "{what} stands where the layout put it: {control:?}"
            );
            assert!(
                field.contains(control.center()),
                "{what} {control:?} stands inside the field {field:?}"
            );
        }
        assert!(
            !sf.escalate_rect.is_zero_area(),
            "Search everywhere is a whole target: {:?}",
            sf.escalate_rect
        );
        let clear = clear_icon_rect(field);
        assert!(
            !sf.clear_rect.is_zero_area() && field.contains(clear.center()),
            "the clear button stands in the field: {:?}",
            sf.clear_rect
        );
        let text = sf.input.get_rect();
        assert!(
            text.x0 >= sf.scope_token_rect.x1 - 0.5,
            "the text starts after the token: {text:?} against {:?}",
            sf.scope_token_rect
        );
        assert!(
            text.x1 <= clear.x0 + 0.5,
            "and ends before the clear button: {text:?} against {clear:?}"
        );
    }

    /// The type the person chose — a face whose look-alike letters are told
    /// apart, at a larger text scale — is the type the layout places the
    /// controls in: the scope token is as wide as its words measured in that
    /// type, and wider than the default type would make it.
    #[test]
    fn a_themed_field_places_its_controls_in_the_themes_type() {
        let mut theme = Theme::light().with_text_scale(1.5);
        theme.tokens.typography.distinct_letters = true;
        theme
            .tokens
            .typography
            .set_face(crate::tokens::UiFace::Hyperlegible.name());
        let mut sf = SearchField::new();
        sf.set_scope_token(Some("Only Inbox".into()));
        let (taffy, node) = lay_out_in(&mut sf, 460.0, &theme);
        let field = crate::widget::layout_rect(&taffy, node, Point::ZERO);

        // The same measurement, taken the way a caller outside the layout takes
        // it: in the theme's own type.
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        text.apply_typography(&theme.tokens.typography);
        let lead = crate::widget::ChipLead::Glyph(Icon::ChevronLeft);
        let natural =
            crate::widget::ChipFace::intrinsic_width("Only Inbox", lead, true, &mut text, &theme);
        let height = ControlSize::Small.height(&theme) - 6.0;
        let expected = natural.min((field.width() * 0.5).max(height));
        assert!(
            (sf.scope_token_rect.width() - expected).abs() <= 0.5,
            "the token is as wide as its words in the theme's type: {:?} against {expected}",
            sf.scope_token_rect
        );

        let plain = Theme::light();
        text.set_text_scale(plain.text_scale);
        text.apply_typography(&plain.tokens.typography);
        let default_natural =
            crate::widget::ChipFace::intrinsic_width("Only Inbox", lead, true, &mut text, &plain);
        assert!(
            sf.scope_token_rect.width()
                > default_natural.min((field.width() * 0.5).max(height)) + 1.0,
            "the token is placed in the theme's larger type, not the default's"
        );
        assert!(
            !sf.scope_token_rect.is_zero_area()
                && field.contains(sf.scope_token_rect.center())
                && sf.input.get_rect().x0 >= sf.scope_token_rect.x1 - 0.5,
            "the token and the text after it stand in the field"
        );
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

    #[test]
    fn a_count_with_nothing_matching_says_no_matches() {
        assert_eq!(FilterCount::new(0, 0).label(), "No matches");
        assert_eq!(FilterCount::new(0, 120).label(), "No matches");
        assert_eq!(FilterCount::new(1, 52).label(), "1 of 52");
        let mut sf = SearchField::filter();
        sf.input.set_text("zzz");
        sf.set_count(Some(FilterCount::new(0, 0)));
        let node = Widget::accessibility(&sf).expect("a11y");
        assert!(
            node.description
                .as_deref()
                .is_some_and(|d| d.contains("No matches"))
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
        crate::testing::inert_widget_methods!(leaf_layout);
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
            Some(SearchRequest::within(ScopeDecl::new(
                "list:people",
                "People"
            )))
        );
        sf.input.set_text("");
        sync_filter(&mut sf, &mut rows);
        assert_eq!(sf.count(), None, "an empty filter shows no count");
    }
}

//! The launcher's window: the toolkit's palette card with its filter band,
//! showing what the source answers and putting to it what the person does.
//!
//! The window keeps what the person is doing (the query typed, the kinds
//! chosen, the row selected) and sends every change of it to the source as
//! an [`Ask`]; what the source answers (the rows, the step on screen, the
//! scope) replaces what the card shows. A source lent by the host answers
//! on the host's next frame; without one, the window answers itself from
//! [`FixtureSource`] as soon as it asks. The view answers the launcher's
//! effects on the window and publishes the results it shows.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, Effected, LiveRow, Publishes, ViewCx};
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::search::system::filters::{
    FilterAction, FilterStripSlot, FilterText, KindChip, SearchFilters, toggle_kind,
};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::widget::command_palette::CommandPalette;
use ui::widget::{AppModel, Decorator, Element, EventCtx, ModelPath, Widget};
use ui::winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use ui::winit::keyboard::{Key, NamedKey};

use crate::fixture::FixtureSource;
use crate::i18n::{t, t_args};
use crate::resource::{Launcher, LauncherEffect};
use crate::source::{
    ActivationModifier, Answer, Ask, CardFit, Link, ResourceKind, Search, SearchScope,
};

/// The card's width on its own: the desktop's search card at its widest.
const CARD_WIDTH: f64 = 640.0; // allow-breakpoint: a size, not a layout decision

/// The launcher on screen.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is one independent part of the launcher's state"
)]
pub struct LauncherSurface {
    palette: CommandPalette,
    filters: SearchFilters,
    link: Link,
    /// Answers the link here when no host lent one.
    own: Option<FixtureSource>,
    /// The generation of the answer shown.
    shown: u64,
    open: bool,
    /// What the person searches for, as last asked.
    search: Search,
    scope: SearchScope,
    /// The chips offered, with their labels.
    kinds: Vec<(ResourceKind, String)>,
    band: bool,
    /// Typing narrows the rows here (a step's values, a row's other
    /// actions) rather than asking the source.
    narrows: bool,
    /// The argument step on screen, by its id.
    field: Option<String>,
    /// The row the last answer selected by itself, while the person has
    /// not moved the selection since: a late answer moves it to its own
    /// best row, never a row the person chose.
    auto_selected: Option<String>,
    cursor: Point,
    structure_dirty: bool,
    /// The room the host gives the card: its width and the tallest its
    /// list may grow.
    room: Option<(f64, f64)>,
    /// The scenario to stage on the next sync.
    staged: Rc<RefCell<Option<Scenario>>>,
    /// The results on screen as the view publishes them.
    rows: Rc<RefCell<Vec<LiveRow>>>,
}

impl LauncherSurface {
    /// The launcher over the host's `link`, or over the fixed desktop when
    /// no host lends one.
    pub fn new(link: Option<Link>) -> Self {
        let (link, own) = match link {
            Some(link) => (link, None),
            None => (Link::new(), Some(FixtureSource::new())),
        };
        let mut palette = CommandPalette::new();
        // The source ranks and matches; the card shows its rows verbatim.
        palette.set_bypass_fuzzy(true);
        // The band's chips are the surface's own, painted into the band
        // the card reserves for them.
        palette.set_host_filter_strip(true);
        palette.set_filter_band_shown(false);
        palette.set_query_placeholder(t("launcher-placeholder"));
        palette.close();
        Self {
            palette,
            filters: SearchFilters::default(),
            link,
            own,
            shown: 0,
            open: false,
            search: Search::default(),
            scope: SearchScope::Workspace,
            kinds: Vec::new(),
            band: false,
            narrows: false,
            field: None,
            auto_selected: None,
            cursor: Point::ZERO,
            structure_dirty: true,
            room: None,
            staged: Rc::default(),
            rows: Rc::default(),
        }
    }

    /// The link the launcher and its source talk over.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// The card.
    pub fn palette(&self) -> &CommandPalette {
        &self.palette
    }

    /// Whether the launcher is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Give the card `width` and let its list grow to `list_height`.
    pub fn set_room(&mut self, width: f64, list_height: f64) {
        if self.room != Some((width, list_height)) {
            self.room = Some((width, list_height));
            self.palette.set_max_width(Some(width));
            self.palette.set_max_list_height(list_height);
            self.structure_dirty = true;
        }
    }

    fn ask(&mut self, ask: Ask) {
        self.link.ask(ask);
        if let Some(own) = &mut self.own {
            self.link.serve(own);
        }
    }

    /// Open in `scope`, on `query` or a fresh field.
    pub fn open(&mut self, scope: SearchScope, query: Option<String>) {
        self.open = true;
        self.palette.open();
        self.palette.set_focus(true);
        self.field = None;
        self.narrows = false;
        self.search = Search {
            query: query.clone().unwrap_or_default(),
            kinds: Vec::new(),
        };
        self.palette.set_query(&self.search.query);
        self.auto_selected = None;
        self.structure_dirty = true;
        self.ask(Ask::Open { scope, query });
    }

    /// Open as the host asked, if it did.
    fn take_open_request(&mut self) {
        if let Some(scope) = self.link.take_open_request() {
            self.open(scope, None);
        }
    }

    /// Close without a choice.
    pub fn dismiss(&mut self) {
        if !self.open {
            return;
        }
        self.close();
        self.ask(Ask::Dismissed);
    }

    fn close(&mut self) {
        self.open = false;
        self.field = None;
        self.palette.close();
        self.structure_dirty = true;
    }

    /// Search for `query` with the kinds chosen.
    fn search_for(&mut self, query: String) {
        self.search.query = query;
        self.ask(Ask::Search(self.search.clone()));
    }

    fn filter(&mut self, kinds: Vec<ResourceKind>) {
        self.search.kinds = kinds;
        self.ask(Ask::Search(self.search.clone()));
    }

    /// Choose `row` (the selected one when `None`) with `modifier` held.
    fn choose(&mut self, row: Option<String>, modifier: Option<ActivationModifier>) -> bool {
        let Some(row) = row.or_else(|| self.palette.selected_id().map(str::to_owned)) else {
            return false;
        };
        self.ask(Ask::Choose { row, modifier });
        true
    }

    /// Show what the source answered last, when it is new.
    fn take_answer(&mut self) {
        if self.link.generation() == self.shown {
            return;
        }
        let (generation, answer) = self.link.answer();
        self.shown = generation;
        self.show(answer);
    }

    fn show(&mut self, answer: Answer) {
        if answer.closed {
            self.close();
            return;
        }
        if !self.open {
            return;
        }
        match &answer.field {
            Some((id, spec)) => {
                // The same step asked again (a value refused) keeps what
                // was typed unless the source gives it back corrected.
                if self.field.as_deref() != Some(id.as_str()) || spec.prefill.is_some() {
                    self.palette.open_argument_step(id.clone(), spec.clone());
                } else {
                    self.palette.open_argument_step(
                        id.clone(),
                        ui::widget::command_palette::ArgumentSpec {
                            prefill: self.palette.argument_buffer().map(str::to_owned),
                            ..spec.clone()
                        },
                    );
                }
                self.field = Some(id.clone());
            }
            None => {
                if self.field.take().is_some() || self.palette.in_argument_mode() {
                    self.palette.cancel_argument();
                }
            }
        }
        if let Some(query) = &answer.query {
            self.palette.set_query(query);
            if !answer.narrows {
                self.search.query.clone_from(query);
            }
        }
        self.narrows = answer.narrows;
        self.palette.set_bypass_fuzzy(!answer.narrows);
        self.scope = answer.scope;
        self.palette
            .set_query_placeholder(if answer.placeholder.is_empty() {
                t("launcher-placeholder")
            } else {
                answer.placeholder.clone()
            });
        self.palette.set_scope_token(answer.scope_token.clone());
        self.palette
            .set_scope_token_action(answer.scope_token_action.clone());
        self.palette.set_field_only_while_empty(answer.field_only);
        self.palette.set_searching(answer.searching);
        let kept = self
            .palette
            .selected_id()
            .filter(|id| Some(*id) != self.auto_selected.as_deref())
            .map(str::to_owned);
        self.palette.set_entries(answer.rows);
        // The source's choice is the person's when it keeps it; else the
        // row the person chose stays, as long as it is listed.
        let select = answer.select.or(kept);
        let cursor = select.and_then(|id| {
            self.palette
                .visible_entries()
                .iter()
                .position(|entry| entry.id == id)
        });
        self.palette.set_cursor(cursor.unwrap_or(0));
        self.auto_selected = self.palette.selected_id().map(str::to_owned);
        if let Some((title, detail)) = answer.status {
            self.palette.set_selected_status(title, detail);
        }
        self.kinds = answer.kinds;
        self.band = answer.band && answer.field.is_none() && !self.narrows;
        self.palette.set_filter_band_shown(self.band);
        self.configure_filters();
        self.structure_dirty = true;
    }

    fn configure_filters(&mut self) {
        let kinds = self
            .kinds
            .iter()
            .map(|(kind, label)| KindChip {
                kind: kind.clone(),
                label: label.clone(),
                name: t_args(
                    "launcher-a11y-filter-kind",
                    &[("kind", label.as_str().into())],
                ),
            })
            .collect();
        self.filters.configure(
            kinds,
            self.search.kinds.clone(),
            Vec::new(),
            self.scope,
            FilterText {
                all_label: t("launcher-filter-all"),
                all_name: t("launcher-a11y-filter-all"),
                more_label: t("launcher-filter-more"),
                more_name: t("launcher-a11y-filter-more"),
                filters_name: t("launcher-a11y-filters"),
                kinds_name: t("launcher-a11y-result-kinds"),
                scopes_name: t("launcher-a11y-scope"),
            },
        );
    }

    fn filter_action(&mut self, action: FilterAction) {
        match action {
            FilterAction::AllKinds => {
                if !self.search.kinds.is_empty() {
                    self.filter(Vec::new());
                }
            }
            FilterAction::ToggleKind(kind) => {
                let mut kinds = self.search.kinds.clone();
                toggle_kind(&mut kinds, kind);
                self.filter(kinds);
            }
            FilterAction::More => {
                self.filters.more_open = !self.filters.more_open;
                self.structure_dirty = true;
            }
            FilterAction::Scope(_) => {}
        }
    }

    /// Tell the host what the card makes of its room.
    fn tell_fit(&self) {
        let shown = if self.open {
            self.palette
                .visible_entries()
                .iter()
                .enumerate()
                .filter_map(|(at, entry)| Some((entry.id.clone(), self.palette.row_rect(at)?)))
                .collect()
        } else {
            Vec::new()
        };
        self.link.set_fit(CardFit {
            card: self.palette.get_rect(),
            chrome_height: self.palette.chrome_height(),
            rows: self.palette.entry_row_capacity(),
            shown,
        });
    }

    fn shows_band(&self) -> bool {
        self.band && !self.palette.shows_field_only()
    }

    /// A press at `point`: a chip, a header (which shows only its kind), a
    /// row (chosen at once), or outside the card (which closes it).
    fn press(&mut self, point: Point, modifier: Option<ActivationModifier>) -> bool {
        if self.shows_band()
            && let Some(action) = self.filters.hit(point)
        {
            self.filter_action(action);
            return true;
        }
        if self.field.is_none()
            && !self.narrows
            && let Some(header) = self.palette.header_at(point)
        {
            if let Some((kind, _)) = self.kinds.iter().find(|(_, label)| *label == header) {
                let kind = kind.clone();
                self.filter(vec![kind]);
            }
            return true;
        }
        if let Some(row) = self.palette.row_at(point) {
            self.palette.set_cursor(row);
            self.choose(None, modifier);
            return true;
        }
        if !self.palette.get_rect().contains(point) {
            self.dismiss();
            return true;
        }
        false
    }

    /// What the card asked while it handled an event: a step answered or
    /// left, the scope token taken away, a value stepped, the query
    /// changed.
    fn take_requests(&mut self, query_before: &str) {
        if let Some(text) = self.palette.take_submitted_argument() {
            self.ask(Ask::Argument(text));
        }
        if self.palette.take_argument_cancelled() {
            self.ask(Ask::Back);
        }
        if self.palette.take_scope_token_removed() {
            self.ask(Ask::Widen);
        }
        if let Some((row, delta)) = self.palette.take_adjustment() {
            self.ask(Ask::Adjust { row, delta });
        }
        let query = self.palette.query().to_owned();
        if query != query_before && !self.narrows && self.field.is_none() {
            self.search_for(query);
        }
    }

    fn move_selection(&mut self, by: i64) {
        let count = self.palette.visible_entries().len();
        if count == 0 {
            return;
        }
        let at = i64::try_from(self.palette.cursor()).unwrap_or(0);
        let last = i64::try_from(count.saturating_sub(1)).unwrap_or(0);
        let to = at.saturating_add(by).clamp(0, last);
        self.palette.set_cursor(usize::try_from(to).unwrap_or(0));
        self.selection_moved();
    }

    /// The person moved the selection: it is theirs now, and the source is
    /// told, so rows changing under it keep it on the same result.
    fn selection_moved(&mut self) {
        self.auto_selected = None;
        if let Some(row) = self.palette.selected_id().map(str::to_owned) {
            self.ask(Ask::Select { row });
        }
    }

    /// Stage `scenario`, as the person would get there.
    fn stage_scenario(&mut self, scenario: Option<&str>) {
        if scenario == Some("peek") {
            self.own = Some(FixtureSource::new().field_only());
        }
        let scope = match scenario {
            Some("app") => SearchScope::ActiveApp,
            Some("everywhere") => SearchScope::All,
            _ => SearchScope::Workspace,
        };
        self.open(scope, None);
        let typed = match scenario {
            Some("results" | "filtered" | "narrow" | "app") => "notes",
            Some("everywhere") => "cal",
            Some("step") => "new",
            Some("choices") => "share",
            Some("more") => "q3",
            Some("empty") => "zzz",
            _ => "",
        };
        if !typed.is_empty() {
            self.palette.set_query(typed);
            self.search_for(typed.to_owned());
        }
        match scenario {
            Some("filtered") => self.filter(vec![ResourceKind::File]),
            Some("step") => {
                self.choose(Some("action:new-note".into()), None);
            }
            Some("choices") => {
                self.choose(Some("action:share-notes".into()), None);
            }
            Some("more") => {
                self.choose(Some("file:notes-q3".into()), Some(ActivationModifier::Alt));
            }
            _ => {}
        }
        self.take_answer();
    }
}

/// The modifier `how` names, as a person or agent spells it.
fn modifier_of(how: Option<&str>) -> Result<Option<ActivationModifier>, String> {
    match how.map(str::trim) {
        None | Some("" | "open") => Ok(None),
        Some("here" | "tab") => Ok(Some(ActivationModifier::Shift)),
        Some("beside") => Ok(Some(ActivationModifier::Ctrl)),
        Some("more") => Ok(Some(ActivationModifier::Alt)),
        Some(other) => Err(format!(
            "`{other}` is not a way to open a result: here, beside or more"
        )),
    }
}

/// The scope `name` names.
fn scope_of(name: Option<&str>) -> Result<SearchScope, String> {
    match name.map(|name| name.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "workspace") => Ok(SearchScope::Workspace),
        Some("app" | "this-app" | "active-app") => Ok(SearchScope::ActiveApp),
        Some("everywhere" | "all") => Ok(SearchScope::All),
        Some(other) => Err(format!(
            "`{other}` is not a place to search: app, workspace or everywhere"
        )),
    }
}

impl Decorator for LauncherSurface {
    type Inner = CommandPalette;

    fn inner(&self) -> &CommandPalette {
        &self.palette
    }

    fn inner_mut(&mut self) -> &mut CommandPalette {
        &mut self.palette
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let staged = self.staged.borrow_mut().take();
        if let Some(Scenario(scenario)) = staged {
            self.stage_scenario(scenario.as_deref());
        }
        if let Some(own) = &mut self.own
            && self.link.has_asks()
        {
            self.link.serve(own);
        }
        if let Some((width, list)) = self.link.room() {
            self.set_room(width, list);
        }
        self.take_open_request();
        self.take_answer();
        if self.palette.set_held_modifiers(self.link.held_modifiers()) {
            self.structure_dirty = true;
        }
        self.palette.sync(state, path);
        self.publish_rows();
        self.tell_fit();
    }

    /// The card is as wide as the room the host gives it, or the
    /// desktop's search card on its own, and never wider than its window.
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.palette.build_layout(taffy, theme, node_map);
        let width = self.room.map_or_else(
            || CARD_WIDTH * f64::from(theme.text_scale),
            |(width, _)| width,
        );
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a card is at most a few thousand logical pixels wide"
            )]
            let width = width as f32;
            style.size.width = ui::taffy::Dimension::length(width);
            style.max_size.width = ui::taffy::Dimension::percent(1.0);
            let _ = taffy.set_style(node, style);
        }
        node
    }

    fn take_structure_dirty(&mut self) -> bool {
        let palette = self.palette.take_structure_dirty();
        std::mem::take(&mut self.structure_dirty) || palette
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.tell_fit();
        if !self.open {
            return;
        }
        let card = self.palette.get_rect();
        ui::widget::floating_surface::paint_card_shadow(scene, transform, card, 1.0, theme);
        self.palette
            .draw(scene, transform, text_renderer, theme, presence);
        if self.shows_band() {
            let band = self.palette.filter_strip_rect();
            let before = self.filters.search_chip_rows;
            let rows = self.filters.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                FilterStripSlot {
                    band,
                    row_height: self.palette.filter_strip_row_height(),
                    reserved_rows: self.palette.host_filter_strip_rows(),
                },
                Some(self.cursor),
            );
            if rows != before && self.palette.set_host_filter_strip_rows(rows) {
                self.structure_dirty = true;
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.take_open_request();
        if !self.open {
            return;
        }
        let modifier = ActivationModifier::of(ctx.modifiers.state());
        self.palette.set_held_modifiers(ctx.modifiers.state());
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // Presses on the field are the field's: its caret, its
                // scope token.
                if !self.palette.input_row_rect().contains(self.cursor)
                    && self.press(self.cursor, modifier)
                {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Enter),
                        state: ElementState::Pressed,
                        repeat,
                        ..
                    },
                ..
            } if !self.palette.in_argument_mode() => {
                // One choice per press: a held Enter never repeats one.
                if !repeat && !self.palette.shows_field_only() {
                    self.choose(None, modifier);
                }
                *ctx.handled = true;
                return;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !self.palette.in_argument_mode() => {
                // A typed query is cleared first, so a slip of the finger
                // never costs the whole search. Then a step's values or a
                // row's other actions go back to the results; the results
                // close the launcher.
                if !self.palette.query().is_empty() {
                    let before = self.palette.query().to_owned();
                    self.palette.set_query("");
                    self.take_requests(&before);
                } else if self.narrows {
                    self.ask(Ask::Back);
                } else {
                    self.dismiss();
                }
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        let query_before = self.palette.query().to_owned();
        let cursor_before = self.palette.cursor();
        self.palette.handle_event(event, ctx);
        if self.palette.cursor() != cursor_before {
            self.selection_moved();
        }
        self.take_requests(&query_before);
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if !self.open {
            return None;
        }
        let mut node = self.palette.accessibility()?;
        if self.shows_band() && !node.children.is_empty() {
            node.children.push(self.filters.accessibility());
        }
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if matches!(action, ui::a11y::Action::Activate) && target.starts_with("palette:filter:") {
            let filter = match target {
                "palette:filter:all" => Some(FilterAction::AllKinds),
                "palette:filter:more" => Some(FilterAction::More),
                _ => {
                    let kinds: Vec<ResourceKind> =
                        self.kinds.iter().map(|(kind, _)| kind.clone()).collect();
                    (0..kinds.len())
                        .find(|&at| SearchFilters::search_filter_key(&kinds, at) == target)
                        .and_then(|at| kinds.get(at).cloned())
                        .map(FilterAction::ToggleKind)
                }
            };
            if let Some(filter) = filter {
                self.filter_action(filter);
                return true;
            }
        }
        let query_before = self.palette.query().to_owned();
        let handled = self
            .palette
            .perform_semantic_action(target, action, state, path);
        self.take_requests(&query_before);
        handled
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.open {
            return Vec::new();
        }
        let mut rects = vec![
            ("launcher-card".to_owned(), self.palette.get_rect()),
            ("launcher-field".to_owned(), self.palette.input_row_rect()),
        ];
        if self.shows_band() {
            rects.push((
                "launcher-filters".to_owned(),
                self.palette.filter_strip_rect(),
            ));
        }
        for (at, entry) in self.palette.visible_entries().iter().enumerate() {
            if let Some(rect) = self.palette.row_rect(at) {
                rects.push((format!("launcher-row:{}", entry.id), rect));
            }
        }
        rects
    }
}

/// A screenshot scenario to stage, `None` for a plain capture.
struct Scenario(Option<String>);

/// The launcher window of `cx`'s view, over the host's `link` or over the
/// fixed desktop when no host lends one: it answers the launcher's effects
/// and publishes the results on screen.
pub(crate) fn window(cx: &ViewCx<Launcher>, link: Option<Link>) -> Element {
    let mut window = LauncherSurface::new(link);
    window.link().wake_with(cx.waker());
    // On its own (its own window, a capture) the launcher is the window: it
    // opens at once. A host opens it when the person asks.
    if window.own.is_some() && cx.mode().is_live() {
        window.open(SearchScope::Workspace, None);
    }
    let staged = Rc::clone(&window.staged);
    cx.on_stage(move |scenario| *staged.borrow_mut() = Some(Scenario(scenario.map(str::to_owned))));
    cx.window_effects(
        |window: &mut LauncherSurface, effect: LauncherEffect, _cx| window.answer(effect),
    );
    let rows = Rc::clone(&window.rows);
    cx.publishing(
        Publishes::new().live_rows(move || rows.borrow().clone()),
        Box::new(window),
    )
}

impl LauncherSurface {
    fn answer(&mut self, effect: LauncherEffect) -> EffectAnswer {
        let closed = || Err("search is not open".to_owned());
        match effect {
            LauncherEffect::Open { scope, query } => {
                let scope = scope_of(scope.as_deref())?;
                self.open(scope, query);
            }
            LauncherEffect::Search { query } => {
                if !self.open {
                    return closed();
                }
                self.palette.set_query(&query);
                if self.narrows || self.field.is_some() {
                    return Ok(Effected::done());
                }
                self.search_for(query);
            }
            LauncherEffect::Filter { kinds } => {
                if !self.open {
                    return closed();
                }
                let chosen = kinds
                    .iter()
                    .map(|name| {
                        self.kinds
                            .iter()
                            .map(|(kind, _)| kind)
                            .chain(KNOWN_KINDS.iter())
                            .find(|kind| {
                                kind.id().eq_ignore_ascii_case(name.trim())
                                    || kind.label().eq_ignore_ascii_case(name.trim())
                            })
                            .cloned()
                            .ok_or_else(|| format!("no result is of kind `{name}`"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.filter(chosen);
            }
            LauncherEffect::Widen => {
                if !self.open {
                    return closed();
                }
                self.ask(Ask::Widen);
            }
            LauncherEffect::MoveSelection { by } => {
                if !self.open {
                    return closed();
                }
                self.move_selection(by);
            }
            LauncherEffect::Choose { row, how } => {
                if !self.open {
                    return closed();
                }
                let modifier = modifier_of(how.as_deref())?;
                if !self.choose(row, modifier) {
                    return Err("there is no result to choose".into());
                }
            }
            LauncherEffect::Adjust { row, delta } => {
                if !self.open {
                    return closed();
                }
                let Some(row) = row.or_else(|| self.palette.selected_id().map(str::to_owned))
                else {
                    return Err("there is no result to step".into());
                };
                let delta = if delta < 0 { -1 } else { 1 };
                self.ask(Ask::Adjust { row, delta });
            }
            LauncherEffect::Answer { value } => {
                if self.field.is_none() {
                    return Err("no step is asking for a value".into());
                }
                self.ask(Ask::Argument(value));
            }
            LauncherEffect::Back => {
                if self.field.is_none() && !self.narrows {
                    return Err("there is no step to leave".into());
                }
                self.ask(Ask::Back);
            }
            LauncherEffect::Dismiss => self.dismiss(),
        }
        self.take_answer();
        self.publish_rows();
        Ok(Effected::done())
    }

    /// Publish the results on screen, each chosen through
    /// [`LauncherEffect::Choose`]: what an agent reads to find the row to
    /// choose.
    fn publish_rows(&self) {
        if !self.open {
            self.rows.borrow_mut().clear();
            return;
        }
        *self.rows.borrow_mut() = self
            .palette
            .visible_entries()
            .into_iter()
            .map(|entry| {
                LiveRow::new(
                    format!("launcher.search/{}", entry.id),
                    "launcher.result",
                    entry.label.clone(),
                )
                .effect(&LauncherEffect::Choose {
                    row: Some(entry.id.clone()),
                    how: None,
                })
            })
            .collect();
    }
}

/// The kinds a filter may name before a search found any of them.
const KNOWN_KINDS: &[ResourceKind] = &[
    ResourceKind::Application,
    ResourceKind::Action,
    ResourceKind::File,
    ResourceKind::Folder,
    ResourceKind::Note,
    ResourceKind::Record,
    ResourceKind::Tab,
    ResourceKind::WindowGroup,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(query: &str) -> LauncherSurface {
        let mut surface = LauncherSurface::new(None);
        surface.open(SearchScope::Workspace, None);
        if !query.is_empty() {
            surface.palette.set_query(query);
            surface.search_for(query.to_owned());
        }
        surface.take_answer();
        surface
    }

    fn ids(surface: &LauncherSurface) -> Vec<String> {
        surface
            .palette
            .visible_entries()
            .iter()
            .map(|entry| entry.id.clone())
            .collect()
    }

    #[test]
    fn opening_shows_the_launcher_sections_and_typing_their_results() {
        let surface = opened("");
        assert!(surface.is_open());
        assert_eq!(ids(&surface).last().map(String::as_str), Some("all-apps"));
        let surface = opened("calendar");
        assert_eq!(ids(&surface), ["app:calendar"]);
    }

    #[test]
    fn a_selection_the_person_made_survives_a_late_answer() {
        let mut surface = opened("notes");
        surface.move_selection(2);
        let chosen = surface.palette.selected_id().map(str::to_owned);
        surface.search_for("notes".into());
        surface.take_answer();
        assert_eq!(surface.palette.selected_id().map(str::to_owned), chosen);
    }

    #[test]
    fn a_step_is_answered_and_closes_the_launcher() {
        let mut surface = opened("new");
        assert!(surface.choose(Some("action:new-note".into()), None));
        surface.take_answer();
        assert!(surface.palette.in_argument_mode());
        surface.ask(Ask::Argument("Groceries".into()));
        surface.take_answer();
        assert!(!surface.is_open(), "the action ran");
    }

    #[test]
    fn a_kind_chip_narrows_the_results_to_it() {
        let mut surface = opened("notes");
        assert!(surface.band);
        surface.filter_action(FilterAction::ToggleKind(ResourceKind::File));
        surface.take_answer();
        assert!(ids(&surface).iter().all(|id| id.starts_with("file:")));
        surface.filter_action(FilterAction::AllKinds);
        surface.take_answer();
        assert!(ids(&surface).len() > 1);
    }

    #[test]
    fn ways_to_open_and_places_to_search_read_as_people_name_them() {
        assert_eq!(
            modifier_of(Some("beside")),
            Ok(Some(ActivationModifier::Ctrl))
        );
        assert!(modifier_of(Some("sideways")).is_err());
        assert_eq!(scope_of(Some("everywhere")), Ok(SearchScope::All));
        assert_eq!(scope_of(None), Ok(SearchScope::Workspace));
    }
}

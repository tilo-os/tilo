//! The palette over rows a host searched itself: the desktop's apps, windows
//! and documents, a launcher's service, a window's published actions.
//!
//! The card, the query field, the list, the argument steps and the geometry
//! are the palette's own, shared with an app's Ctrl+K search; what differs is
//! where the rows come from and where a choice goes. A host hands rows in
//! ([`ActionPalette::set_entries`]) and takes back which one the person chose
//! ([`ActionPalette::take_outcome`]).

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::platform::PlatformConventions;
use crate::search::{
    ActivationModifier, CommandRow, FieldWeight, SearchGroup, SearchResult, SearchRow,
};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{SearchField, Widget};

use super::rows::{ArgumentKind, ArgumentSpec, CommandEntry, CommandOutcome};
use super::{ActionPalette, FIELD_SIZE, HostStep};

/// Rows the list shows before it scrolls when the host sets no height.
const DEFAULT_LIST_ROWS: f64 = 10.0;
/// Room the composed zero-state needs: glyph, headline, hint.
const EMPTY_STATE_HEIGHT: f64 = 104.0;
/// The card's width on its own: what a host that sets none gets.
const CARD_WIDTH: f64 = 480.0;
const INPUT_HEIGHT: f64 = 40.0;
const PADDING: f64 = 8.0;
/// One row of host-painted controls between the field and the list.
const BAND_HEIGHT: f64 = 36.0;
/// The accessible target of the scope token when it is a control.
const SCOPE_TOKEN_TARGET: &str = "command-palette:scope-token";
/// The accessible target of the query field.
const QUERY_TARGET: &str = "command-palette:query";

impl ActionPalette {
    /// A palette over rows a host feeds itself, rather than over an app's
    /// published actions.
    #[must_use]
    pub fn rows(entries: Vec<CommandEntry>) -> Self {
        let mut palette = Self::new();
        palette.field = palette.host_field();
        palette.set_entries(entries);
        palette
    }

    /// Replace the rows the host searched. The first is selected.
    pub fn set_entries(&mut self, entries: Vec<CommandEntry>) {
        self.host_rows = Some(entries);
        self.planned_host_rows();
        self.set_cursor(0);
        self.structure_dirty = true;
    }

    /// The rows the host fed that the list is showing, in order.
    #[must_use]
    pub fn visible_entries(&self) -> Vec<&CommandEntry> {
        let Some(rows) = self.host_rows.as_ref() else {
            return Vec::new();
        };
        self.host_visible
            .iter()
            .filter_map(|&at| rows.get(at))
            .collect()
    }

    /// Whether the palette is over host rows.
    #[must_use]
    pub(crate) fn hosts_rows(&self) -> bool {
        self.host_rows.is_some()
    }

    /// The chosen row, with the value it asked for if it asked. Drained by
    /// the host after it handled input.
    pub fn take_outcome(&mut self) -> Option<CommandOutcome> {
        self.outcome.take()
    }

    /// The step ←/→ asked of the selected adjustable row since the last call:
    /// its id and the direction (-1 or 1). The host changes the value and
    /// refreshes the row.
    pub fn take_adjustment(&mut self) -> Option<(String, i8)> {
        self.adjustment.take()
    }

    /// The value a host step was confirmed with, once.
    pub fn take_submitted_argument(&mut self) -> Option<String> {
        self.submitted_argument.take()
    }

    /// Whether a host step was backed out of since the last call.
    pub fn take_argument_cancelled(&mut self) -> bool {
        std::mem::take(&mut self.argument_cancelled)
    }

    /// Ask for one value in the field, for a host that collects an action's
    /// arguments one step at a time. The list stays where it is underneath.
    /// Enter hands the value to [`Self::take_submitted_argument`] and Escape
    /// sets [`Self::take_argument_cancelled`]; the host decides what follows.
    /// Neither runs a row, so a step can't stand in for one.
    pub fn open_argument_step(&mut self, id: impl Into<String>, spec: ArgumentSpec) {
        self.open = true;
        self.results.set_focused(true);
        self.outcome = None;
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.begin_host_step(id.into(), spec);
    }

    /// Leave any host step without running or reporting anything: the host
    /// has moved on (to another step, or back to the results).
    pub fn cancel_host_step(&mut self) {
        self.end_host_step();
        self.submitted_argument = None;
        self.argument_cancelled = false;
    }

    /// The value the open step's field holds.
    #[must_use]
    pub fn argument_buffer(&self) -> Option<&str> {
        self.host_step.as_ref().map(|_| self.field.value())
    }

    /// Show `token` at the start of the field: the part of the source the
    /// query is narrowed to, removable with Backspace.
    pub fn set_scope_token(&mut self, token: Option<String>) {
        self.field.set_scope_token(token);
    }

    /// What pressing the token does, when it is a control: its accessible
    /// name. `None`: Backspace alone removes it.
    pub fn set_scope_token_action(&mut self, action: Option<String>) {
        self.scope_token_action = action;
    }

    /// The token the field shows, when one is set.
    #[must_use]
    pub fn scope_token(&self) -> Option<&str> {
        self.field.scope_token()
    }

    /// What pressing the token does, when it is a control.
    #[must_use]
    pub fn scope_token_action(&self) -> Option<&str> {
        self.scope_token_action.as_deref()
    }

    /// Whether Backspace at the start of the field took the token away since
    /// the last call: the host widens the search.
    pub fn take_scope_token_removed(&mut self) -> bool {
        // The field's own × and Backspace take the token away too.
        let cleared = self.field.take_scope_cleared();
        std::mem::take(&mut self.scope_token_removed) || cleared
    }

    /// Take the scope token away as Backspace would.
    fn remove_scope_token(&mut self) {
        if self.field.scope_token().is_some() {
            self.field.set_scope_token(None);
            self.scope_token_removed = true;
        }
    }

    /// Show the query field alone while nothing is typed: no list and no
    /// band, the card a single row until the person types.
    pub fn set_field_only_while_empty(&mut self, on: bool) {
        self.field_only_while_empty = on;
        self.structure_dirty = true;
    }

    /// Whether the card shows its field alone right now.
    #[must_use]
    pub fn shows_field_only(&self) -> bool {
        self.field_only_while_empty && self.field.is_empty() && self.host_step.is_none()
    }

    /// Whether the card reserves its band of host-painted controls right now.
    #[must_use]
    pub fn filter_band_shown(&self) -> bool {
        !self.shows_field_only() && self.band_shown
    }

    /// Show or hide the band. A host hides it until the results have
    /// something to tell apart, so it isn't standing chrome over every
    /// search. Returns whether that changed.
    pub fn set_filter_band_shown(&mut self, shown: bool) -> bool {
        let changed = self.band_shown != shown;
        self.band_shown = shown;
        self.structure_dirty |= changed;
        self.sync_list();
        changed
    }

    /// The card grows by exactly `rows` of host-painted controls and the
    /// results start below them, rather than a host with too many controls
    /// painting over results. Returns whether the reservation changed.
    pub fn set_host_filter_strip_rows(&mut self, rows: usize) -> bool {
        let rows = rows.max(1);
        let changed = self.band_rows != rows;
        self.band_rows = rows;
        self.structure_dirty |= changed;
        self.sync_list();
        changed
    }

    /// The band rows the card is reserving.
    #[must_use]
    pub fn host_filter_strip_rows(&self) -> usize {
        self.band_rows.max(1)
    }

    /// The band the host paints its own controls into.
    #[must_use]
    pub fn filter_strip_rect(&self) -> Rect {
        let row = self.input_row_rect();
        let y0 = row.y1 + PADDING;
        Rect::new(row.x0, y0, row.x1, y0 + self.band_height())
    }

    /// One band row's height, for a host spacing its controls by it.
    #[must_use]
    pub fn filter_strip_row_height(&self) -> f64 {
        BAND_HEIGHT * self.layout_scale()
    }

    /// The tallest the result list may grow before it scrolls, in logical
    /// pixels. The host sets it from the display it shows the palette on.
    pub fn set_max_list_height(&mut self, height: f64) {
        self.max_list_height = Some(height.max(0.0));
        self.structure_dirty = true;
        self.sync_list();
    }

    /// The widest the card may be, in logical pixels: the host sets it from
    /// the room it shows the palette in. `None` keeps its own width.
    pub fn set_max_width(&mut self, width: Option<f64>) {
        self.max_width = width.map(|width| width.max(0.0));
        self.structure_dirty = true;
    }

    /// The text scale layout and host-painted controls share.
    #[must_use]
    pub fn text_scale(&self) -> f64 {
        self.text_scale
    }

    /// Replace the placeholder the field shows while a step is not using it.
    pub fn set_query_placeholder(&mut self, placeholder: impl Into<String>) {
        self.query_placeholder = placeholder.into();
        if self.host_step.is_none() {
            self.field.set_placeholder(&self.query_placeholder);
        }
    }

    /// Whether more results are still on their way (an index whose code is
    /// still loading): an empty list then says "Searching…" instead of
    /// "No results".
    pub fn set_searching(&mut self, searching: bool) {
        self.searching = searching;
        self.sync_empty_state();
    }

    /// Every row the host fed shows unless the host lets the palette narrow
    /// by the query. A host doing its own matching turns narrowing off, so
    /// hits whose labels don't literally spell the query aren't hidden by
    /// fuzzy scoring.
    pub fn set_bypass_fuzzy(&mut self, on: bool) {
        self.bypass_fuzzy = on;
        self.planned_host_rows();
        self.structure_dirty = true;
    }

    /// Tell the palette which modifiers are down, for a host that routes
    /// modifier changes itself.
    pub fn set_held_modifiers(&mut self, held: winit::keyboard::ModifiersState) -> bool {
        let changed = self.held != held;
        self.held = held;
        self.results.set_held_modifier(ActivationModifier::of(held));
        changed
    }

    /// The modifiers held now that name one of a row's alternatives.
    #[must_use]
    pub fn held_modifier(&self) -> Option<ActivationModifier> {
        ActivationModifier::of(self.held)
    }

    /// The selected row's id; `None` while nothing is shown.
    #[must_use]
    pub fn selected_id(&self) -> Option<&str> {
        let rows = self.host_rows.as_ref()?;
        self.host_visible
            .get(self.results.cursor())
            .and_then(|&at| rows.get(at))
            .map(|entry| entry.id.as_str())
    }

    /// Show host feedback on the selected row using the shared status pill and
    /// description, keeping the query, the order and the selection so an
    /// unsuccessful invocation can be corrected or retried.
    pub fn set_selected_status(
        &mut self,
        badge: impl Into<String>,
        description: impl Into<String>,
    ) {
        let Some(&at) = self.host_visible.get(self.results.cursor()) else {
            return;
        };
        let (badge, description) = (badge.into(), description.into());
        if let Some(entry) = self.host_rows.as_mut().and_then(|rows| rows.get_mut(at)) {
            entry.badge = Some(badge);
            entry.description = Some(description);
        }
        self.results.replace_groups(self.host_groups());
        self.sync_list();
    }

    /// Scroll the list by `dy` logical px (down for positive), keeping the
    /// cursor on a row in view so the list stays where it was put.
    fn scroll_list(&mut self, dy: f64) {
        self.sync_list();
        if !self.results.scroll_by(dy) {
            return;
        }
        let shown = self.results.shown_rows();
        if shown.is_empty() {
            return;
        }
        let cursor = self
            .cursor()
            .clamp(shown.start, shown.end.saturating_sub(1));
        self.results.set_cursor(cursor);
        self.sync_list();
    }

    /// The row `i` of the list, in window coordinates; `None` when it is
    /// scrolled out of view.
    #[must_use]
    pub fn row_rect(&self, i: usize) -> Option<Rect> {
        if i >= self.host_visible.len() || self.shows_field_only() {
            return None;
        }
        self.results.row_rect(i)
    }

    /// The group header at `p`, if one is there.
    #[must_use]
    pub fn header_at(&self, p: Point) -> Option<String> {
        if self.shows_field_only() || !self.rect.contains(p) {
            return None;
        }
        self.results.header_at(p)
    }

    /// The row at `p`, if one is there.
    #[must_use]
    pub fn row_at(&self, p: Point) -> Option<usize> {
        if self.shows_field_only() {
            return None;
        }
        self.results
            .row_at(p)
            .filter(|&row| row < self.host_visible.len())
    }

    /// The field's row, inside the card's padding.
    #[must_use]
    pub fn input_row_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.rect.y0 + PADDING,
            self.rect.x1 - PADDING,
            self.rect.y0 + PADDING + INPUT_HEIGHT * self.layout_scale(),
        )
    }

    /// Hand the card a rect, for a host that positions its overlays itself.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
        self.sync_list();
    }

    /// The size the card asks for: the rows it holds, up to the tallest list
    /// the host allows.
    #[must_use]
    pub fn desired_size(&self) -> (f64, f64) {
        if self.shows_field_only() {
            return (
                self.card_width(),
                PADDING * 2.0 + INPUT_HEIGHT * self.layout_scale(),
            );
        }
        let list = if self.host_visible.is_empty() {
            EMPTY_STATE_HEIGHT * self.layout_scale()
        } else {
            self.fitted_list_height()
        };
        (self.card_width(), self.chrome_height() + list)
    }

    /// The card's height less its result list: the field, the band and the
    /// padding around them.
    #[must_use]
    pub fn chrome_height(&self) -> f64 {
        PADDING + (INPUT_HEIGHT * self.layout_scale()) + PADDING + self.band_block() + PADDING
    }

    /// How many result rows a full list holds: what a host asks its sources
    /// for, so the first screen is full.
    #[must_use]
    pub fn entry_row_capacity(&self) -> usize {
        let row = self.row_height().max(1.0);
        crate::num::usize_of_f64((self.list_cap() / row).floor()).max(1)
    }

    /// The result list's rect (the card under its field and band, with the
    /// rows' padding), from the card's.
    pub(crate) fn sync_list(&mut self) {
        // Before the first layout there is nothing to place the rows in.
        if self.rect.height() > 0.0 {
            self.results.set_rect(self.list_rect());
        }
    }

    /// The result list's rect, inside the card's padding.
    fn list_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + PADDING,
            self.list_top_y(),
            self.rect.x1 - PADDING,
            self.list_bottom_y(),
        )
    }

    fn list_top_y(&self) -> f64 {
        self.input_row_rect().y1 + PADDING + self.band_block()
    }

    fn list_bottom_y(&self) -> f64 {
        self.rect.y1 - PADDING
    }

    /// Theme scales and list metrics, before layout or paint.
    pub(crate) fn host_prepare(&mut self, theme: &Theme) {
        self.text_scale = f64::from(theme.text_scale);
        self.ui_scale = f64::from(theme.ui_scale);
        self.results.resolve_metrics(theme);
        self.results
            .set_group_gap(f64::from(theme.tokens.spacing.rhythm.section));
        let generation = crate::search::engine_generation();
        if self.ranked_generation != generation {
            self.ranked_generation = generation;
            if !self.bypass_fuzzy {
                self.planned_host_rows();
            }
        }
        self.sync_list();
    }

    fn layout_scale(&self) -> f64 {
        self.text_scale * self.ui_scale
    }

    fn card_width(&self) -> f64 {
        let width = CARD_WIDTH * self.layout_scale();
        self.max_width.map_or(width, |max| width.min(max))
    }

    /// The list's cap: what the host allows, else ten rows.
    fn list_cap(&self) -> f64 {
        self.max_list_height
            .unwrap_or_else(|| DEFAULT_LIST_ROWS * self.row_height())
    }

    /// The whole rows from the top that fit under the cap: a cap that cut a
    /// row in half would leave a sliver nothing paints.
    fn fitted_list_height(&self) -> f64 {
        self.results
            .fitted_height(self.list_cap().max(self.row_height()))
    }

    /// The list's nominal row: what capacity, the default cap and a wheel
    /// notch count in. Each row is as tall as its own lines.
    fn row_height(&self) -> f64 {
        let any_subtitle = self
            .visible_entries()
            .iter()
            .any(|entry| entry.description.is_some());
        self.results.row_band(any_subtitle)
    }

    fn band_height(&self) -> f64 {
        BAND_HEIGHT * self.layout_scale() * usize_to_f64(self.host_filter_strip_rows())
    }

    /// Room the band takes in the card: its rows and the padding under them.
    fn band_block(&self) -> f64 {
        if self.shows_field_only() || !self.band_shown {
            return 0.0;
        }
        self.band_height() + PADDING
    }

    /// The host's rows as the result list's groups: a run of rows sharing a
    /// group under its header with the group's count, rows in a section under
    /// the section's header with their groups as captions, and a row with
    /// neither running on under the rows above it.
    fn host_groups(&self) -> Vec<SearchGroup> {
        let mut groups: Vec<SearchGroup> = Vec::new();
        let mut current: Option<&str> = None;
        let mut section: Option<&str> = None;
        let Some(rows) = self.host_rows.as_ref() else {
            return groups;
        };
        for (entry_i, &at) in self.host_visible.iter().enumerate() {
            let Some(entry) = rows.get(at) else {
                continue;
            };
            let mut opened: Option<SearchGroup> = None;
            if let Some(name) = entry.section.as_deref() {
                if section != Some(name) {
                    section = Some(name);
                    current = Some(name);
                    opened = Some(SearchGroup {
                        section: Some(name.to_owned()),
                        ..SearchGroup::new("", Vec::new())
                    });
                }
                if let Some(group) = entry.group.as_deref()
                    && current != Some(group)
                {
                    current = Some(group);
                    opened = Some(SearchGroup {
                        section: Some(name.to_owned()),
                        ..SearchGroup::new(group, Vec::new())
                    });
                }
            } else {
                section = None;
                if let Some(group) = entry.group.as_deref()
                    && current != Some(group)
                {
                    current = Some(group);
                    // Fall back to the contiguous run when the host gives no
                    // uncapped total.
                    let total = entry.group_total.unwrap_or_else(|| {
                        self.host_visible.get(entry_i..).map_or(0, |rest| {
                            rest.iter()
                                .take_while(|&&j| {
                                    rows.get(j)
                                        .is_some_and(|e| e.group.as_deref() == Some(group))
                                })
                                .count()
                        })
                    });
                    opened = Some(SearchGroup {
                        total: Some(total),
                        ..SearchGroup::new(group, Vec::new())
                    });
                }
            }
            let row = self.host_result(entry);
            match (opened, groups.last_mut()) {
                (Some(mut group), _) => {
                    group.results.push(row);
                    groups.push(group);
                }
                (None, Some(last)) if last.section.as_deref() == entry.section.as_deref() => {
                    last.results.push(row);
                }
                (None, _) => groups.push(SearchGroup {
                    section: entry.section.clone(),
                    ..SearchGroup::new("", vec![row])
                }),
            }
        }
        groups
    }

    /// A host row as a result list row.
    fn host_result(&self, entry: &CommandEntry) -> SearchResult {
        let mut result = SearchResult::new(
            entry.group.clone().unwrap_or_default(),
            entry.label.clone(),
            entry.id.clone(),
        );
        result.subtitle.clone_from(&entry.description);
        result.icon = entry.icon;
        result.row = Some(SearchRow::Command(Box::new(CommandRow {
            description_is_path: entry.description_is_path,
            facts: entry.description_facts.clone(),
            image: entry.image.clone(),
            avatar: entry.avatar.clone(),
            badge: entry.badge.clone(),
            keycaps: entry.keycap_labels(self.platform),
            faces: entry.faces.clone(),
            outcome: entry.outcome.clone(),
            alternates: entry.alternates.clone(),
        })));
        result
    }

    /// Which of the host's rows the query lets through, and the list they make.
    fn planned_host_rows(&mut self) {
        self.host_visible.clear();
        let rows = self.host_rows.as_deref().unwrap_or_default();
        let matching = (!self.bypass_fuzzy).then(|| {
            self.host_index.ranks(
                self.query(),
                rows.iter().map(|entry| {
                    [
                        (entry.label.as_str(), FieldWeight::TITLE),
                        (
                            entry.description.as_deref().unwrap_or_default(),
                            FieldWeight::SUBTITLE,
                        ),
                    ]
                }),
            )
        });
        for (i, _) in rows.iter().enumerate() {
            if matching
                .as_ref()
                .is_some_and(|ranks| ranks.get(i).is_none_or(Option::is_none))
            {
                continue;
            }
            self.host_visible.push(i);
        }
        let groups = self.host_groups();
        self.results.set_groups(groups);
        let query = self.query().to_owned();
        self.results.set_highlight(&query);
        self.sync_empty_state();
        self.sync_list();
    }

    /// What the empty list says: searching, nothing yet, or no match for the
    /// query with what to try instead.
    fn sync_empty_state(&mut self) {
        self.results.set_empty_text(self.empty_results_title());
        self.results.set_empty_hint(
            (!self.is_searching()).then(|| crate::i18n!("palette-no-results-hint")),
        );
    }

    fn is_searching(&self) -> bool {
        // Rows this palette ranks itself wait for the index's code too.
        (self.searching || (!self.bypass_fuzzy && !crate::search::engine_ready()))
            && !self.query().is_empty()
    }

    fn empty_results_title(&self) -> String {
        if self.is_searching() {
            crate::i18n!("palette-searching")
        } else if self.query().is_empty() {
            crate::i18n!("palette-empty")
        } else {
            crate::i18n!("palette-no-results", "query" => self.query())
        }
    }

    /// Choosing row `id`: a row that asks for a value collects it first; any
    /// other hands its id back to the host.
    pub(crate) fn resolve_host_row(&mut self, id: &str) {
        let Some(entry) = self
            .host_rows
            .as_ref()
            .and_then(|rows| rows.iter().find(|entry| entry.id == id))
            .cloned()
        else {
            return;
        };
        if let Some(spec) = entry.argument.clone() {
            self.begin_host_step(entry.id, spec);
            return;
        }
        self.outcome = Some(CommandOutcome {
            id: entry.id,
            argument: None,
        });
        self.end_host_step();
    }

    /// Activate the selected row, exactly as Enter does.
    pub(crate) fn activate_host_selected(&mut self) {
        if self.host_step.is_some() {
            return;
        }
        if let Some(id) = self.selected_id().map(str::to_owned) {
            self.resolve_host_row(&id);
        }
    }

    /// Repurpose the field to collect `spec`'s value for host row `id`.
    fn begin_host_step(&mut self, id: String, spec: ArgumentSpec) {
        if self.host_step.is_none() {
            self.saved_host_query = self.field.value().to_owned();
        }
        let prefill = spec.prefill.clone().unwrap_or_default();
        let (kind, secret) = match spec.kind {
            ArgumentKind::Number { .. } => (crate::widget::TextInputKind::Number, false),
            ArgumentKind::Text => (crate::widget::TextInputKind::Prose, false),
            ArgumentKind::Secret => (crate::widget::TextInputKind::Prose, true),
        };
        self.field.set_input_kind(kind, secret);
        self.field.set_query(&prefill);
        self.field.set_selection(0, prefill.len());
        self.field.set_placeholder(&spec.placeholder);
        self.field
            .set_prompt((!spec.prompt.is_empty()).then(|| spec.prompt.clone()));
        let _ = self.field.take_dirty();
        self.field.set_focused(true);
        self.host_step = Some(HostStep {
            id,
            kind: spec.kind,
        });
        self.structure_dirty = true;
    }

    /// Leave the open host step, putting the query back in the field.
    pub(crate) fn end_host_step(&mut self) {
        if self.host_step.take().is_none() {
            return;
        }
        // A secret is wiped while the field still knows it is one.
        self.field.set_query("");
        self.field
            .set_input_kind(crate::widget::TextInputKind::Search, false);
        let saved = std::mem::take(&mut self.saved_host_query);
        self.field.set_query(&saved);
        self.field.set_placeholder(&self.query_placeholder);
        self.field.set_prompt(None);
        let _ = self.field.take_dirty();
        self.planned_host_rows();
        self.structure_dirty = true;
    }

    /// Whether a host step is collecting a value.
    #[must_use]
    pub(crate) fn in_host_step(&self) -> bool {
        self.host_step.is_some()
    }

    /// The keys a host step owns: Enter confirms, Escape backs out, a number
    /// steps, and everything else edits the value.
    fn handle_host_arg_key(&mut self, key: &Key, handled: &mut bool) {
        *handled = true;
        match key {
            Key::Named(NamedKey::Enter) => {
                let Some(step) = self.host_step.take() else {
                    return;
                };
                let value = self.field.value().to_owned();
                self.submitted_argument = Some(value.clone());
                self.outcome = Some(CommandOutcome {
                    id: step.id,
                    argument: Some(value),
                });
                self.end_host_step();
            }
            Key::Named(NamedKey::Escape) => {
                self.argument_cancelled = true;
                self.end_host_step();
            }
            // No list navigation while typing an argument; a number steps.
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown) => {
                let up = matches!(key, Key::Named(NamedKey::ArrowUp));
                let Some(kind) = self.host_step.as_ref().map(|step| step.kind) else {
                    return;
                };
                if let Some(next) = kind.stepped(self.field.value(), if up { 1.0 } else { -1.0 }) {
                    self.field.set_query(&next);
                    let _ = self.field.take_dirty();
                }
            }
            _ => {}
        }
    }

    /// ←/→ on a selected adjustable row with the caret at the query's end
    /// steps its value instead of moving the caret.
    fn step_selected(&mut self, key: &Key) -> bool {
        let direction: i8 = match key {
            Key::Named(NamedKey::ArrowRight) => 1,
            Key::Named(NamedKey::ArrowLeft) => -1,
            _ => return false,
        };
        let end = self.field.value().len();
        if self.field.selection_anchor_focus_or_caret() != (end, end) {
            return false;
        }
        let Some(id) = self.selected_id().map(str::to_owned) else {
            return false;
        };
        let adjustable = self
            .host_rows
            .as_ref()
            .and_then(|rows| rows.iter().find(|entry| entry.id == id))
            .is_some_and(|entry| entry.adjustable);
        if !adjustable {
            return false;
        }
        self.adjustment = Some((id, direction));
        true
    }

    /// The field holds the keyboard exactly while the palette does.
    pub(crate) fn sync_field_focus(&mut self) {
        if self.field.is_focused() != self.is_presented() {
            self.field.set_focused(self.is_presented());
        }
    }

    /// Paint the card over the host's rows: the field, the band its host
    /// paints into, and the list.
    pub(crate) fn draw_host(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        // Theme typography is already scaled. Keep all legacy text paint and
        // metric calls in that same space, restoring the child renderer after.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        self.host_prepare(theme);
        // The one floating card: the opaque panel ground and its edges.
        // Opaque, as the card is rows of small text: a tinted backdrop let the
        // window under it read through them. Only the scrim around it lets the
        // desktop show.
        crate::widget::floating_surface::paint_card_body(scene, transform, self.rect, theme);
        // A floating panel whose header slot holds the query field, embedded:
        // the card's own first row, not a field boxed inside it. Results, when
        // there are any, are set off from it by the panel's hairline under the
        // header.
        let row_rect = self.input_row_rect();
        let panel = crate::widget::FloatingPanelFace::new()
            .header(row_rect.height())
            .body_empty(self.shows_field_only())
            .layout(self.rect, theme);
        if let Some(rule) = panel.rule {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::surface::edge_on(crate::chrome::panel_ground(theme)),
                None,
                &rule,
            );
        }
        self.sync_field_focus();
        self.field.apply_layout_external(row_rect);
        self.field
            .draw(scene, transform, text_renderer, theme, presence);
        if self.shows_field_only() {
            text_renderer.set_text_scale(renderer_scale);
            return;
        }
        if self.host_visible.is_empty() {
            self.results
                .draw_empty(scene, transform, text_renderer, theme);
        } else {
            self.results
                .draw_list(scene, transform, text_renderer, theme);
        }
        text_renderer.set_text_scale(renderer_scale);
    }

    /// Route one input event into the host-driven card.
    pub(crate) fn route_host_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if !self.is_presented() {
            return false;
        }
        self.sync_field_focus();
        self.sync_list();
        // Read, never consumed: every other surface needs modifiers too.
        if let WindowEvent::ModifiersChanged(modifiers) = event {
            self.set_held_modifiers(modifiers.state());
            return false;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && self.rect.contains(self.last_cursor)
            && self.host_step.is_none()
        {
            // A notch moves the list a row.
            let dy = match *delta {
                MouseScrollDelta::LineDelta(_, y) => -f64::from(y) * self.row_height(),
                MouseScrollDelta::PixelDelta(p) => -p.y / ctx.scale_factor,
            };
            self.scroll_list(dy);
            *ctx.handled = true;
            return true;
        }
        if self.host_pointer_event(event, ctx) {
            return true;
        }
        if matches!(
            event,
            WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
        ) {
            // Presses on the field place its caret or take the scope token.
            let pressed = matches!(event, WindowEvent::MouseInput { .. });
            if !pressed || self.input_row_rect().contains(self.last_cursor) {
                self.edit_host_field(event, ctx);
            }
            return true;
        }
        if matches!(event, WindowEvent::Ime(_)) {
            self.edit_host_field(event, ctx);
            *ctx.handled = true;
            return true;
        }
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key,
                    state: element_state,
                    repeat,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if *element_state != ElementState::Pressed {
            return false;
        }
        // Submission needs a fresh press, including after loading or opening
        // a prompt.
        if *repeat && matches!(logical_key, Key::Named(NamedKey::Enter)) {
            *ctx.handled = true;
            return true;
        }
        let mods = ctx.modifiers.state();
        let shift = mods.shift_key();
        let ctrl = mods.control_key() || mods.super_key();
        if self.host_step.is_some() {
            self.handle_host_arg_key(logical_key, ctx.handled);
            // Enter, Escape and a number's arrows belong to the step alone;
            // everything else edits the value in the field.
            if !matches!(
                logical_key,
                Key::Named(
                    NamedKey::Enter | NamedKey::Escape | NamedKey::ArrowUp | NamedKey::ArrowDown
                )
            ) {
                self.edit_host_field(event, ctx);
            }
            return true;
        }
        if self.handle_list_key(logical_key, shift, ctrl) {
            *ctx.handled = true;
            return true;
        }
        if !shift && !ctrl && self.step_selected(logical_key) {
            *ctx.handled = true;
            return true;
        }
        match logical_key {
            Key::Named(NamedKey::Enter) => {
                self.activate_host_selected();
                *ctx.handled = true;
            }
            // A typed query is cleared first; only an empty field closes, so
            // a slip of the finger never costs the whole search.
            Key::Named(NamedKey::Escape) => {
                if self.field.is_empty() {
                    self.close();
                } else {
                    self.set_query("");
                }
                *ctx.handled = true;
            }
            _ => self.edit_host_field(event, ctx),
        }
        true
    }

    /// Moves through the results; `true` when the key moved the selection or
    /// belonged to the list.
    pub fn handle_list_key(&mut self, key: &Key, shift: bool, ctrl: bool) -> bool {
        let caret = self.field.selection_anchor_focus_or_caret();
        let end = self.field.value().len();
        let owned =
            self.results
                .handle_list_key(key, shift, ctrl, (caret == (0, 0), caret == (end, end)));
        if owned {
            self.sync_list();
        }
        owned
    }

    /// Pointer moves pick the row under them; a press chooses it.
    fn host_pointer_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) -> bool {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            // The pointer picks the row it is over where it stands: a row
            // half in view is not scrolled out from under it.
            if let Some(row) = self.row_at(self.last_cursor) {
                self.results.set_cursor_in_place(row);
                *ctx.handled = true;
            }
            return false;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && let Some(row) = self.row_at(self.last_cursor)
        {
            self.set_cursor(row);
            self.activate_host_selected();
            *ctx.handled = true;
            return true;
        }
        false
    }

    /// Hand `event` to the field; a query it edited narrows the list.
    fn edit_host_field(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let Ok(node) = taffy.new_leaf(taffy::Style::default()) else {
            return;
        };
        let mut focus = None;
        self.field.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: ctx.state.reborrow(),
                path: ctx.path,
                handled: ctx.handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        if self.field.take_scope_cleared() {
            self.scope_token_removed = true;
        }
        if self.field.take_dirty().is_some() && self.host_step.is_none() {
            self.planned_host_rows();
            self.structure_dirty = true;
        }
    }

    /// Narrow the host's rows by what the field holds now, after text was
    /// committed into it from outside the keyboard: the same the person's
    /// typing does.
    pub(crate) fn committed_host_text(&mut self) {
        if self.host_step.is_none() {
            self.planned_host_rows();
        }
        self.structure_dirty = true;
    }

    /// The field a host's palette asks in: the card's own first row, embedded
    /// in its header, with no clear button (Escape clears).
    fn host_field(&self) -> SearchField {
        let mut field = self
            .field_source
            .build()
            .control_size(FIELD_SIZE)
            .without_clear()
            .with_placeholder(&self.query_placeholder);
        field.set_embedded(true);
        field
    }

    /// Open over the host's rows: an empty query, the first row selected.
    pub(crate) fn open_host(&mut self) {
        self.open = true;
        self.results.set_focused(true);
        self.host_step = None;
        self.arg = None;
        self.confirm = None;
        self.outcome = None;
        self.adjustment = None;
        self.submitted_argument = None;
        self.argument_cancelled = false;
        self.saved_host_query.clear();
        self.field.set_query("");
        let _ = self.field.take_dirty();
        self.sync_field_focus();
        self.planned_host_rows();
        self.set_cursor(0);
        self.structure_dirty = true;
    }

    /// Close over the host's rows, keeping them for the next time it opens.
    pub(crate) fn close_host(&mut self) {
        self.end_host_step();
        self.open = false;
        self.field.set_focused(false);
        self.results.set_focused(false);
    }

    /// The query the host's rows were narrowed by, which a step keeps aside
    /// while it uses the field.
    pub(crate) fn host_query(&self) -> &str {
        if self.host_step.is_some() {
            &self.saved_host_query
        } else {
            self.field.value()
        }
    }

    /// Replace the query the host's rows are narrowed by.
    pub(crate) fn set_host_query(&mut self, query: &str) {
        if self.host_step.is_some() {
            query.clone_into(&mut self.saved_host_query);
        } else {
            self.field.set_query(query);
            let _ = self.field.take_dirty();
        }
        self.planned_host_rows();
        self.structure_dirty = true;
    }

    /// The accessibility tree of the host-driven card: the query as a text
    /// field, the results, and the scope token when it is a control.
    pub(crate) fn host_accessibility(&self) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let open = self.is_open();
        let mut node = AccessNode::new(Role::Dialog)
            .with_name(crate::i18n!("palette-name"))
            .with_state(AccessState {
                focusable: true,
                focused: self.is_presented(),
                modal: open,
                expanded: Some(open),
                hidden: !open,
                ..Default::default()
            });
        if !open {
            return node;
        }
        let name = match &self.host_step {
            Some(_) => self.field.prompt().unwrap_or_default().to_owned(),
            None => self.query_placeholder.clone(),
        };
        // The caret makes the field a text field for assistive technology
        // (AT-SPI Text and EditableText), not only a labelled value.
        let (anchor, caret) = self.field.selection_anchor_focus_or_caret();
        let secret = self
            .host_step
            .as_ref()
            .is_some_and(|step| step.kind == ArgumentKind::Secret);
        let mut query_node = AccessNode::new(Role::TextInput)
            .with_action_target(QUERY_TARGET)
            .with_name(name)
            .with_bounds(self.input_row_rect())
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.is_presented())
                    .protected(secret),
            );
        if !secret {
            query_node = query_node
                .with_value(self.field.value().to_owned())
                .with_text_selection(anchor, caret);
        }
        let mut list = self
            .results
            .accessibility()
            .unwrap_or_else(|| AccessNode::new(Role::List));
        if self.host_visible.is_empty() && self.host_step.is_some() {
            list.children.clear();
        }
        let mut children = vec![
            query_node,
            list.with_name(crate::i18n!("palette-results"))
                .with_bounds(self.list_rect()),
        ];
        if let (Some(token), Some(action)) = (self.field.scope_token(), &self.scope_token_action) {
            children.push(
                AccessNode::new(Role::Button)
                    .with_action_target(SCOPE_TOKEN_TARGET)
                    .with_name(action.clone())
                    .with_description(token.to_owned())
                    .with_bounds(self.field.scope_token_rect())
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        node = node.with_children(children);
        node
    }

    /// Answer an assistive action on the host-driven card.
    pub(crate) fn host_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        doc: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, path) = crate::doc::document(doc, path);
        if target == SCOPE_TOKEN_TARGET {
            return match action {
                Action::Activate if self.scope_token_action.is_some() => {
                    self.remove_scope_token();
                    true
                }
                _ => false,
            };
        }
        if target == QUERY_TARGET {
            return match action {
                Action::Focus | Action::Activate => true,
                // Assistive technology and agents set the query as a value,
                // exactly as typing it would.
                Action::SetValue(value) if self.host_step.is_none() => {
                    self.set_query(value);
                    true
                }
                _ => false,
            };
        }
        if !self
            .results
            .perform_semantic_action(target, action, state, path)
        {
            return false;
        }
        if self.results.take_activated().is_some() {
            self.activate_host_selected();
        }
        true
    }

    /// The rect an assistive tree path names.
    pub(crate) fn host_synthetic_rect(&self, path: &[usize]) -> Option<Rect> {
        let mut node = self.host_accessibility();
        for &index in path {
            node = node.children.into_iter().nth(index)?;
        }
        node.bounds.map(crate::a11y::AccessBounds::to_rect)
    }

    /// Focus what an assistive tree path names.
    pub(crate) fn host_focus_synthetic(&mut self, path: &[usize]) -> bool {
        match path {
            [0] => true,
            [1, ..] => {
                let target = (|| {
                    let mut node = self.host_accessibility();
                    for &index in path {
                        node = node.children.into_iter().nth(index)?;
                    }
                    node.action_target
                })();
                let Some(target) = target else {
                    return false;
                };
                self.results.select_action(&target)
            }
            _ => false,
        }
    }

    /// The IME fingerprint of a step's value, scoped to the step so assistive
    /// input cannot leak into the query.
    pub(crate) fn host_ime_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let field = self.field.ime_edit_fingerprint(key)?;
        Some(match &self.host_step {
            Some(step) => crate::ime::scoped_fingerprint(step.id.as_bytes(), field, key),
            None => field,
        })
    }

    /// The keycap notation rows are drawn with.
    pub(crate) fn set_host_platform(&mut self, conventions: PlatformConventions) {
        if self.platform != conventions {
            self.platform = conventions;
            // Keycaps follow the platform's notation.
            self.results.replace_groups(self.host_groups());
        }
    }
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

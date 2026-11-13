//! [`Dropdown`] — a reusable, CRDT-backed single-select control.

use crate::i18n::widget_text;
use crate::num::f32_of_f64;
use crate::widget::autocomplete::{Suggestion, SuggestionList};
use crate::widget::{AppModel, EventCtx, ModelPath};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[path = "dropdown_component.rs"]
mod component;
use crate::widget::TaffyTreeExt;
use crate::widget::binding::{BoxedBinding, Pulled};
pub use component::DropdownProps;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::query_text::{QueryComposition, QueryEventChange, QueryInk, QueryView};
use crate::text_render::{TextRenderer, TextStyle};
use crate::text_selection::Selection;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::{ActionEmitter, ControlBindingMetadata};

/// Height of the closed control before the first layout: a Regular control.
const CONTROL_HEIGHT: f64 = crate::tokens::Metrics::BASE.min_pointer_target as f64;
/// Tallest the rows of the open list get before it scrolls.
const MAX_LIST_HEIGHT: f64 = 240.0;
const PAD_X: f64 = 10.0;
const CHEVRON_W: f64 = 30.0;

fn menu_min_width() -> f64 {
    f64::from(crate::tokens::Metrics::BASE.panel_width_utility)
}

type SelectionChangeCallback = Box<dyn FnMut(&str)>;

const CHANGE_DROPDOWN: &str = "tilo.ui.dropdown.change";

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (open/hover/focus/enabled/etc), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct Dropdown {
    id: String,
    accessible_name: Option<String>,
    values: Vec<String>,
    options: Vec<String>,
    groups: Vec<Option<String>>,
    shortcuts: Vec<(String, String)>,
    binding: Option<BoxedBinding<String>>,
    on_change: Option<SelectionChangeCallback>,
    action: Option<ActionEmitter<String>>,
    change_verb: Option<super::control_verb::ControlVerb<String>>,
    placeholder: String,

    selected: String,
    open: bool,
    interactive_open: bool,
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    typing: crate::type_ahead::TypeAhead,
    list: SuggestionList,
    rows_at: Vec<usize>,
    query_rect: Rect,

    align_highlight_on_sync: bool,

    rect: Rect,
    outer: Rect,
    caption: Option<String>,
    caption_height: f64,
    control_size: crate::widget::ControlSize,
    is_focused: bool,
    is_hovered: bool,
    last_cursor: Point,
    /// The host viewport from the latest paint, so the open list can flip
    /// above the control instead of running off the bottom of the window.
    /// `None` until a paint has reported one (bare harnesses never do).
    viewport: Option<Rect>,
    theme: Option<Rc<Theme>>,

    enabled: bool,
    menu: Option<crate::icons::Icon>,
    drawn: Option<u64>,

    fill: bool,
    leading_icon: Option<crate::icons::Icon>,
    name_resolved: bool,
    field_height: f64,
    label_style: TextStyle,
    caption_style: TextStyle,
    theme_caption_height: f64,
    measured: f64,
    column: Option<ChooserColumn>,
    chooser_cap: f64,
    interaction: crate::interaction::InteractionEnvironment,
    layout_dirty: bool,
    appear: crate::motion::Appearing,
    list_pose: crate::motion::Pose,
}

/// Lists shorter than this never show a filter: a typed letter jumps to the
/// next option starting with it instead.
pub const FILTER_MIN_OPTIONS: usize = 8;

/// The one width a section's choosers share.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChooserColumn(Rc<Cell<f64>>);

impl ChooserColumn {
    /// Forget the offers of the previous layout, so a chooser whose options got shorter
    /// lets the column narrow.
    pub(crate) fn reset(&self) {
        self.0.set(0.0);
    }

    fn offer(&self, width: f64) -> f64 {
        let shared = self.0.get().max(width);
        self.0.set(shared);
        shared
    }

    /// Enrol `widget` or the value choosers inside it: dropdowns that hug their options,
    /// without a caption of their own.
    pub(crate) fn join(&self, widget: &mut dyn Widget) {
        if let Some(any) = widget.as_any_mut() {
            if any.is::<crate::widget::Section>() || any.is::<crate::widget::FormSection>() {
                return;
            }
            if let Some(dropdown) = any.downcast_mut::<Dropdown>() {
                dropdown.join_column(self);
                return;
            }
        }
        for child in widget.children_mut() {
            self.join(child);
        }
    }
}

impl Dropdown {
    /// Take part in a section's shared chooser width, unless the dropdown
    /// spans its row, is a menu button, or is a captioned filter.
    fn join_column(&mut self, column: &ChooserColumn) {
        self.column =
            (!self.fill && self.menu.is_none() && self.caption.is_none()).then(|| column.clone());
    }

    fn field_width(&self, own: f64) -> f64 {
        let own = own.max(PAD_X + CHEVRON_W);
        self.column.as_ref().map_or(own, |column| {
            column
                .offer(own)
                .min(self.chooser_cap)
                .max(PAD_X + CHEVRON_W)
        })
    }

    fn effectively_enabled(&self) -> bool {
        self.enabled && self.action.as_ref().is_none_or(ActionEmitter::is_available)
    }
    /// Construct a dropdown over `options`, keeping the chosen value as text under the
    /// document key `value_path`.
    pub fn new(options: Vec<String>, value_path: &str) -> Self {
        Self {
            id: String::new(),
            accessible_name: None,
            values: options.clone(),
            options,
            groups: Vec::new(),
            shortcuts: Vec::new(),
            binding: Some(crate::doc_source::text_key(value_path)),
            on_change: None,
            change_verb: None,
            action: None,
            placeholder: String::new(),
            selected: String::new(),
            open: false,
            interactive_open: false,
            filter: String::new(),
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
            typing: crate::type_ahead::TypeAhead::default(),
            list: {
                let mut list = SuggestionList::new()
                    .with_scrollbar()
                    .with_max_visible(usize::MAX);
                list.set_max_height(Some(MAX_LIST_HEIGHT));
                list
            },
            rows_at: Vec::new(),
            query_rect: Rect::ZERO,
            align_highlight_on_sync: false,
            rect: Rect::ZERO,
            outer: Rect::ZERO,
            caption: None,
            caption_height: 0.0,
            control_size: crate::widget::ControlSize::Regular,
            is_focused: false,
            is_hovered: false,
            last_cursor: Point::ZERO,
            viewport: None,
            theme: None,
            enabled: true,
            menu: None,
            fill: false,
            leading_icon: None,
            name_resolved: false,
            field_height: CONTROL_HEIGHT,
            label_style: crate::tokens::typography::stock().style(TextRole::Label),
            caption_style: crate::tokens::typography::stock().style(TextRole::Caption),
            theme_caption_height: 0.0,
            measured: 0.0,
            column: None,
            chooser_cap: f64::from(crate::tokens::Metrics::BASE.chooser_width),
            interaction: crate::interaction::InteractionEnvironment::default(),
            layout_dirty: true,
            drawn: None,
            appear: crate::motion::Appearing::closed(),
            list_pose: crate::motion::Pose::REST,
        }
    }

    fn visual_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            &self.selected,
            &self.options,
            &self.placeholder,
            self.open,
            self.list.highlight(),
            &self.filter,
            self.list.scroll_offset().to_bits(),
            self.is_focused,
            self.is_hovered,
            self.effectively_enabled(),
        )
            .hash(&mut hasher);
        [self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y1]
            .map(f64::to_bits)
            .hash(&mut hasher);
        self.list.is_scrollbar_active().hash(&mut hasher);
        (self.filter_selection.anchor, self.filter_selection.cursor).hash(&mut hasher);
        crate::i18n::I18nManager::revision().hash(&mut hasher);
        hasher.finish()
    }

    /// Create a dropdown bound to typed process-local selected state.
    ///
    /// User selection updates `state`; mutations through any clone update
    /// this dropdown on the next synchronization pass. The selected string is
    /// intentionally not restricted to `options`, so asynchronously loaded
    /// choices can preserve their model value while options catch up.
    pub fn from_state(options: Vec<String>, state: &crate::LocalState<String>) -> Self {
        let (selected, binding) = super::binding::local(state);
        Self {
            selected,
            binding: Some(binding),
            ..Self::new(options, "")
        }
    }

    /// Observe selections produced by user interaction.
    #[must_use]
    pub fn on_change(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        if self.action.is_none() {
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Route every committed selection through an exported resource action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.action = Some(ActionEmitter::new(binding));
        self.on_change = None;
        self
    }

    /// Run `callback` with each committed selection, the chooser as control
    /// `tilo.control:<id>`, publishing it as its view action `change`: a person's choice, a
    /// screen reader's, a script's and an agent's arrive through the same action.
    #[must_use]
    pub fn on_change_view(mut self, callback: impl FnMut(&str) + 'static) -> Self {
        if self.action.is_none() {
            self.change_verb = Some(super::control_verb::ControlVerb::new(
                self.test_id().unwrap_or("dropdown"),
                "Change",
                "change",
                CHANGE_DROPDOWN,
                super::control_verb::VerbArgument::Text,
            ));
            self.on_change = Some(Box::new(callback));
        }
        self
    }

    /// Stable identifier for the e2e harness (`Widget::test_id`).
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    /// Name the dropdown for assistive technology.
    ///
    /// The selected option remains the accessible value, so changing the
    /// selection never changes the label.
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.name_resolved = false;
        self
    }

    /// Name the dropdown with text that is already resolved (a label the
    /// app localized itself, a user's own name for a thing): spoken as
    /// given, never looked up as a message id.
    #[must_use]
    pub fn with_resolved_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self.name_resolved = true;
        self
    }

    /// Span the parent's width.
    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self.layout_dirty = true;
        self
    }

    /// A glyph before the value that says what the choice is.
    #[must_use]
    pub fn leading_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.leading_icon = Some(icon);
        self.layout_dirty = true;
        self
    }

    /// A menu button: the control is `icon` alone, its list opens at least
    /// as wide as a utility panel, aligned to the control's trailing edge, and
    /// choosing an option reports it (to [`Self::on_change`] or
    /// [`Self::on_action`]) without the control keeping it as its value.
    /// Name it with [`Self::with_accessible_name`]: it shows no text.
    #[must_use]
    pub fn as_menu(mut self, icon: crate::icons::Icon) -> Self {
        self.menu = Some(icon);
        self
    }

    /// Text shown (dimmed) when no value is selected yet.
    #[must_use]
    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self.layout_dirty = true;
        self
    }

    /// Update the localized empty-value label without replacing the control.
    pub fn set_placeholder(&mut self, placeholder: &str) {
        if self.placeholder != placeholder {
            placeholder.clone_into(&mut self.placeholder);
            self.drawn = None;
        }
    }

    /// Set the enabled state at build time; see the `enabled` field.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Imperatively flip the enabled state.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.close();
        }
    }

    /// Start opened.
    #[must_use]
    pub fn open(mut self) -> Self {
        self.open = true;
        self.appear.snap(true);
        self.align_highlight_on_sync = true;
        self
    }

    /// Conditionally start opened — convenience for callers that decide at build time (e.g.
    /// a shot seed).
    #[must_use]
    pub fn open_if(self, cond: bool) -> Self {
        if cond { self.open() } else { self }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The bounds the open list must stay inside — normally the window. A
    /// paint that sees the presence viewport keeps this in sync on its own;
    /// a host whose tree does not carry it down to the control (`Row` and
    /// `Column` clear it for their children, and the App pops it before the
    /// overlay pass) hands it over here so a control near the bottom edge
    /// still opens upward.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.note_viewport(viewport);
    }

    /// Builder form of [`Self::set_viewport`].
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.set_viewport(viewport);
        self
    }

    /// Replace the available options while preserving the current value when possible.
    pub fn set_options(&mut self, options: Vec<String>) {
        if self.options != options || self.values != options {
            self.layout_dirty = true;
        }
        self.values.clone_from(&options);
        self.options = options;
        self.groups.clear();
    }

    /// What the list offers, in order.
    #[must_use]
    pub fn options(&self) -> &[String] {
        &self.options
    }

    /// Set stable values with separate presentation labels. This keeps a
    /// duplicate label selectable and searchable without exposing ids in UI.
    pub fn set_value_options(&mut self, options: Vec<(String, String)>) {
        let (values, labels): (Vec<_>, Vec<_>) = options.into_iter().unzip();
        if self.options != labels {
            self.layout_dirty = true;
        }
        self.values = values;
        self.options = labels;
        self.groups.clear();
    }

    /// Set stable values with presentation labels, each under a heading: the
    /// heading stands above its group's options in the open list, small and
    /// dim like a menu's section, and is never chosen itself. Heads run in
    /// order; an empty heading's options stand with no heading at all.
    pub fn set_grouped_value_options(&mut self, groups: Vec<(String, Vec<(String, String)>)>) {
        self.values.clear();
        self.options.clear();
        self.groups.clear();
        for (heading, options) in groups {
            for (value, label) in options {
                self.values.push(value);
                self.options.push(label);
                self.groups
                    .push((!heading.is_empty()).then(|| heading.clone()));
            }
        }
        self.layout_dirty = true;
    }

    /// The keys that do what each option does, by value: shown at the end of its row
    /// (`Ctrl+S`).
    pub fn set_option_shortcuts<'a>(
        &mut self,
        shortcuts: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) {
        self.layout_dirty = true;
        self.shortcuts = shortcuts
            .into_iter()
            .filter(|(_, keys)| !keys.is_empty())
            .map(|(value, keys)| (value.to_owned(), keys.to_owned()))
            .collect();
    }

    /// Open the shared searchable, scrollable menu and align its highlight to
    /// the current selected value on the next sync.
    pub fn open_menu(&mut self) {
        self.open = true;
        self.appear.show(true);
        self.align_highlight_on_sync = true;
    }

    pub fn selected(&self) -> &str {
        &self.selected
    }

    /// Start on `selected`: the value the field shows, in the value's ink, until its
    /// binding reads a stored value.
    #[must_use]
    pub fn with_selected(mut self, selected: impl Into<String>) -> Self {
        self.selected = selected.into();
        self
    }

    /// Set the selected model value programmatically.
    pub fn set_selected(&mut self, selected: impl Into<String>) {
        let selected = selected.into();
        if selected != self.selected {
            self.layout_dirty = true;
        }
        self.selected = selected;
        if self.action.is_none() {
            self.publish_local_state();
        }
    }

    fn publish_local_state(&mut self) {
        if let Some(binding) = &mut self.binding {
            binding.publish(&self.selected);
        }
    }

    /// Place the dropdown at an absolute rect without going through taffy —
    /// the counterpart to `TextInput::apply_layout_external`. The open
    /// list still paints in the overlay pass, so only the closed control's
    /// box is caller-driven; focus / a11y / CRDT-bound selection are unaffected.
    ///
    /// With a caption, `rect` holds the caption and the field below it (see
    /// [`DropdownFace::outer_height`]); the caption's height comes from the
    /// theme, so the field never covers it, even when the host never ran a
    /// layout pass.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.caption_height = if self.caption.is_some() {
            if self.theme_caption_height > 0.0 {
                self.theme_caption_height
            } else {
                caption_height_in(
                    &crate::tokens::typography::stock().style(TextRole::Caption),
                    f64::from(crate::tokens::Spacing::BASE.s1),
                )
            }
        } else {
            0.0
        };
        self.place(rect);
        self.place_list();
    }

    /// Stack a short caption above the field ("Level", "Source") in the
    /// `Caption` role and secondary ink. It also names the control to
    /// assistive technology unless an accessible name was given.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self.layout_dirty = true;
        self
    }

    /// The field's height and label role.
    #[must_use]
    pub fn control_size(mut self, size: crate::widget::ControlSize) -> Self {
        self.control_size = size;
        self.layout_dirty = true;
        self
    }

    fn adopt_theme(&mut self, theme: &Theme) {
        let changed = self.theme.as_ref().is_none_or(|held| {
            held.text_scale.to_bits() != theme.text_scale.to_bits() || held.tokens != theme.tokens
        });
        if changed {
            self.theme = Some(Rc::new(theme.clone()));
            self.list.forget_width();
        }
        self.field_height = self.control_size.height(theme);
        let touch_minimum =
            if self.interaction.pointer_precision == crate::interaction::PointerPrecision::Coarse {
                f64::from(self.interaction.minimum_target())
            } else {
                0.0
            };
        self.field_height = self.field_height.max(touch_minimum);
        self.label_style = self.control_size.label_style(theme);
        self.caption_style = theme.text(TextRole::Caption);
        self.theme_caption_height =
            caption_height_in(&self.caption_style, f64::from(theme.tokens.spacing.s1));
        self.chooser_cap = f64::from(theme.tokens.metrics.chooser_width * theme.text_growth());
        self.list.set_min_row_height(touch_minimum);
        self.list.set_theme(theme);
    }

    /// The narrowest field that shows every option, the placeholder and the
    /// value without eliding, beside the leading icon and the chevron; at
    /// least as wide as the caption above it.
    fn measured_width(&self, text: &mut TextRenderer) -> f64 {
        let labels = self
            .options
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(self.placeholder.as_str()))
            .chain(std::iter::once(self.selected_label()));
        let field = field_width_for(labels, self.leading_icon.is_some(), &self.label_style, text);
        let caption = self.caption.as_deref().map_or(0.0, |caption| {
            f64::from(text.measure_style(caption, None, &self.caption_style).0).ceil()
        });
        let search = if self.supports_filter() {
            field_width_for(
                [widget_text("dropdown-search-placeholder").as_str()],
                true,
                &self.label_style,
                text,
            )
        } else {
            0.0
        };
        field.max(caption).max(search)
    }

    fn supports_filter(&self) -> bool {
        self.menu.is_none() && self.options.len() >= FILTER_MIN_OPTIONS
    }

    fn place(&mut self, outer: Rect) {
        self.outer = outer;
        let field_y0 = (outer.y0 + self.caption_height).min(outer.y1);
        self.rect = Rect::new(outer.x0, field_y0, outer.x1, outer.y1);
    }
    /// Position (0-based) of the highlighted row in the filtered list.
    pub fn highlight_index(&self) -> usize {
        self.list.highlight().unwrap_or(0)
    }
    /// Current filter query (empty when not filtering).
    pub fn filter_query(&self) -> &str {
        &self.filter
    }

    fn filtered(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            (0..self.options.len()).collect()
        } else {
            let q = self.filter.to_lowercase();
            self.options
                .iter()
                .enumerate()
                .filter(|(_, o)| o.to_lowercase().contains(&q))
                .map(|(i, _)| i)
                .collect()
        }
    }

    /// Number of options currently visible (after filtering).
    pub fn visible_count(&self) -> usize {
        self.filtered().len()
    }

    fn selected_pos(&self, filtered: &[usize]) -> usize {
        filtered
            .iter()
            .position(|&i| {
                self.values
                    .get(i)
                    .is_some_and(|value| *value == self.selected)
            })
            .unwrap_or(0)
    }

    fn selected_row(&self, filtered: &[usize]) -> usize {
        self.rows_at
            .get(self.selected_pos(filtered))
            .copied()
            .unwrap_or(0)
    }

    fn has_selected_value(&self) -> bool {
        !self.selected.is_empty() || self.values.contains(&self.selected)
    }

    fn selected_label(&self) -> &str {
        self.values
            .iter()
            .position(|value| *value == self.selected)
            .and_then(|index| self.options.get(index))
            .map_or(self.selected.as_str(), String::as_str)
    }

    fn refresh_list(&mut self) {
        let (rows, rows_at) = self.rows_for(&self.filtered());
        self.rows_at = rows_at;
        let hint =
            (rows.is_empty() || self.supports_filter()).then(|| widget_text("dropdown-no-matches"));
        self.list.set_rows(rows);
        self.list.set_status(hint);
        self.list.set_anchor(self.rect);
        if let Some(viewport) = self.viewport {
            self.list.set_viewport(viewport);
        }
        // A menu opens at least a menu's width, hanging from the trigger's
        // trailing edge.
        let menu = self.menu.is_some();
        self.list
            .set_min_width(if menu { menu_min_width() } else { 0.0 });
        self.list.set_align(if menu {
            crate::widget::PanelAlign::Trailing
        } else {
            crate::widget::PanelAlign::Leading
        });
    }

    /// The open list's rows for the options `filtered` shows, and the row in
    /// them each of those options stands in: a group's heading takes a row
    /// above its first shown option, so a filter that hides the rest of a
    /// group never leaves its heading standing alone.
    fn rows_for(&self, filtered: &[usize]) -> (Vec<Suggestion>, Vec<usize>) {
        let mut rows: Vec<Suggestion> = Vec::with_capacity(filtered.len());
        let mut at: Vec<usize> = Vec::with_capacity(filtered.len());
        let mut standing: Option<&str> = None;
        for index in filtered {
            let (Some(label), Some(value)) = (self.options.get(*index), self.values.get(*index))
            else {
                continue;
            };
            let heading = self.groups.get(*index).and_then(Option::as_deref);
            if heading != standing {
                if let Some(heading) = heading {
                    rows.push(Suggestion::heading(heading));
                }
                standing = heading;
            }
            at.push(rows.len());
            let mut row = Suggestion::labelled(value.clone(), label.clone())
                .with_checked(self.menu.is_none() && *value == self.selected);
            if let Some((_, keys)) = self.shortcuts.iter().find(|(of, _)| of == value) {
                row = row.with_shortcut(keys.clone());
            }
            rows.push(row);
        }
        (rows, at)
    }

    fn list_rect(&self) -> Rect {
        self.list.rect().unwrap_or(Rect::ZERO)
    }

    fn place_list(&mut self) {
        let filtering = self.open && self.supports_filter();
        self.query_rect = if filtering {
            content_rect(self.rect, true, &self.label_style)
        } else {
            Rect::ZERO
        };
        if !self.open {
            return;
        }
        self.refresh_list();
        let Some(theme) = self.theme.clone() else {
            return;
        };
        if !self.list.is_measured() {
            crate::text_render::with_layout_renderer(&theme, |text| {
                self.list.measure(text, &theme);
            });
        }
        self.list.place_scroll();
    }

    fn option_key(value: &str) -> u64 {
        crate::a11y::hash_key(&("dropdown-option", value))
    }

    fn options_access(&self) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let filtered = self.filtered();
        let total = filtered.len();
        let options = filtered
            .iter()
            .enumerate()
            .filter_map(|(pos, &index)| {
                let label = self.options.get(index)?;
                let value = self.values.get(index)?;
                let row = self.rows_at.get(pos).copied().unwrap_or(pos);
                let mut option = AccessNode::new(Role::ListBoxOption)
                    .with_name(label.clone())
                    .with_key(Self::option_key(value))
                    .with_position(row.saturating_add(1), total)
                    .with_state(
                        AccessState::new()
                            .selected(self.menu.is_none() && *value == self.selected)
                            .active_descendant(
                                self.is_focused() && Some(row) == self.list.highlight(),
                            ),
                    );
                if let Some(drawn) = self.list.row_rect(row) {
                    option = option.with_bounds(drawn);
                }
                Some(option)
            })
            .collect();
        AccessNode::new(Role::ListBox)
            .with_bounds(self.list_rect())
            .with_children(options)
    }

    /// Arm the option whose label begins with what has been typed: letters
    /// in quick succession spell a name, one letter again steps to the next
    /// option it begins, wrapping round; nothing when none begins so. A
    /// space only continues a name being typed.
    fn type_ahead(&mut self, typed: &str) {
        let now = crate::time::now();
        if typed.trim().is_empty() && !self.typing.is_typing(now) {
            return;
        }
        self.typing.type_text(typed, now);
        let armed = self
            .list
            .highlight()
            .and_then(|row| self.rows_at.iter().position(|at| *at == row));
        let next = self
            .typing
            .target(self.options.iter().map(String::as_str), armed);
        if let Some(pos) = next {
            let row = self.rows_at.get(pos).copied().unwrap_or(pos);
            self.list.set_highlight(Some(row));
        }
    }

    fn do_open(&mut self) {
        self.open = true;
        self.appear.show(true);
        self.interactive_open = true;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.typing.reset();
        self.list.clear();
        self.place_list();
        let filtered = self.filtered();
        self.list.set_highlight(Some(self.selected_row(&filtered)));
    }

    /// A menu button's face: a Plain icon-only button, its hover, open and
    /// focus each visible.
    fn draw_menu_trigger(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        icon: crate::icons::Icon,
    ) {
        let name = self.accessible_name.as_deref().unwrap_or_default();
        let face = crate::widget::ButtonFace::icon_only(self.rect, icon, name)
            .role(crate::widget::ButtonRole::Plain)
            .control_size(self.control_size)
            .toggled(self.open)
            .hovered(self.is_hovered)
            .focused(self.is_focused && !self.open)
            .disabled(!self.effectively_enabled());
        crate::widget::Button::paint(scene, transform, text_renderer, theme, &face);
    }

    /// Close the options overlay without changing its selected value.
    pub fn close(&mut self) {
        self.open = false;
        self.appear.show(false);
        self.interactive_open = false;
        self.list.pointer_left();
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.place_list();
    }

    fn commit_value(&mut self, value: &str, _state: &AppModel, _root: &ModelPath) {
        if self.menu.is_some() {
            // A menu's choice is a command: reported, never kept. A menu
            // bound to view state reports it there, for the view to act on.
            if let Some(action) = &mut self.action {
                let _ = action.emit(value.to_string());
            } else if let Some(verb) = &mut self.change_verb {
                verb.emit(value.to_string());
            } else if let Some(callback) = self.on_change.as_mut() {
                callback(value);
            } else if let Some(binding) = &mut self.binding {
                binding.publish(&value.to_string());
            }
            return;
        }
        if let Some(action) = &mut self.action {
            if let Err(error) = action.emit(value.to_string()) {
                tracing::warn!(%error, "the chooser's action refused its value");
            }
        } else if let Some(binding) = &mut self.binding {
            binding.publish(&value.to_string());
        }
        // Bound to the value its action writes, the chooser shows what the
        // store holds once it holds it, never an unconfirmed choice.
        if self.action.is_none() || self.binding.is_none() {
            self.selected = value.to_string();
        }
        if self.action.is_none() {
            self.publish_local_state();
        }
        if self.action.is_none() {
            if let Some(verb) = &mut self.change_verb {
                verb.emit(value.to_string());
            } else if let Some(callback) = self.on_change.as_mut() {
                callback(value);
            }
        }
    }

    fn select_highlighted(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(value) = self.list.highlighted().map(|row| row.value.clone()) {
            self.commit_value(&value, state, root);
        }
        self.close();
    }

    fn on_cursor_moved(&mut self, event: &WindowEvent, pos: Point, ctx: &mut EventCtx<'_>) {
        self.last_cursor = pos;
        // A hover change is a repaint, not consumed input (see `Button`): a
        // parent that stops at the first handled child would otherwise never
        // deliver this move to the sibling the pointer went to, and the
        // sibling's hover — or this one's — would stick.
        self.is_hovered = self.rect.contains(pos);
        if self.open {
            if self.scroll_event(event, ctx) || *ctx.handled {
                return;
            }
            let pos = self.list_pose.to_local(pos);
            if self.list.arm_at(pos) {
                *ctx.handled = true;
            }
        }
    }

    fn scroll_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        self.open && self.list.scroll_event(event, ctx)
    }

    fn on_left_press(
        &mut self,
        state: &AppModel,
        root: &ModelPath,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
    ) {
        let pos = self.last_cursor;
        if self.open {
            let at = self.list_pose.to_local(pos);
            if self.list_rect().contains(at) {
                if self.list.row_at(at).is_some() {
                    self.list.arm_at(at);
                    self.select_highlighted(state, root);
                }
            } else if self.rect.contains(pos) {
                if self.supports_filter() && self.query_rect.contains(pos) {
                    *focus_target = Some(node_id);
                    if let Some(selection) =
                        self.filter_composition.pointer_selection(&self.filter, pos)
                    {
                        self.filter_selection = selection;
                    }
                } else {
                    self.close();
                }
            } else {
                self.close();
            }
            *handled = true;
        } else if self.rect.contains(pos) {
            *focus_target = Some(node_id);
            self.do_open();
            *handled = true;
        }
    }

    fn on_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * -self.list.row_height(),
            MouseScrollDelta::PixelDelta(p) => -p.y,
        };
        self.list.scroll_px(dy);
        *handled = true;
    }

    /// `WindowEvent::KeyboardInput`, guarded by the caller to only fire
    /// while focused and on key-down: open/closed navigation, select,
    /// dismiss, and the filter-query fallback.
    fn on_key(
        &mut self,
        event: &WindowEvent,
        ke: &KeyEvent,
        modifiers: Modifiers,
        state: &AppModel,
        root: &ModelPath,
        handled: &mut bool,
    ) {
        let logical_key = &ke.logical_key;
        if self.open {
            let mods = modifiers.state();
            let shift = mods.shift_key();
            let ctrl = mods.control_key() || mods.super_key();
            match logical_key {
                Key::Named(key @ (NamedKey::ArrowDown | NamedKey::ArrowUp)) => {
                    self.list.step_highlight(*key == NamedKey::ArrowDown, false);
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.select_highlighted(state, root);
                    *handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *handled = true;
                }
                _ if self.menu.is_some() => {}
                _ if self.options.len() < FILTER_MIN_OPTIONS => {
                    // A short list never filters: a typed letter arms the
                    // next option starting with it.
                    if let Some(typed) = ke.text.as_deref()
                        && !ctrl
                    {
                        self.type_ahead(typed);
                        *handled = true;
                    }
                }
                _ => {
                    let change = self.filter_composition.handle_event(
                        event,
                        &mut self.filter,
                        &mut self.filter_selection,
                        shift,
                        ctrl,
                    );
                    if change == QueryEventChange::Edited {
                        self.filter_edited();
                    }
                    if change != QueryEventChange::Ignored {
                        *handled = true;
                    }
                }
            }
        } else if let Key::Named(
            NamedKey::Enter | NamedKey::Space | NamedKey::ArrowDown | NamedKey::ArrowUp,
        ) = logical_key
        {
            self.do_open();
            *handled = true;
        }
    }
}

impl Dropdown {
    fn note_viewport(&mut self, viewport: Rect) {
        if self.viewport == Some(viewport) {
            return;
        }
        self.viewport = Some(viewport);
        self.layout_dirty |= self.open;
    }

    fn filter_edited(&mut self) {
        self.place_list();
        self.list.set_highlight(Some(0));
        self.list.scroll_px(f64::NEG_INFINITY);
    }
}

impl Widget for Dropdown {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations
    );

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.is_hovered &= environment.supports_hover();
            self.layout_dirty = true;
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn needs_redraw(&self) -> bool {
        self.drawn != Some(self.visual_signature())
    }

    fn mark_drawn(&mut self) {
        self.drawn = Some(self.visual_signature());
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(binding) = &mut self.binding
            && let Pulled::Value(selected) = binding.pull(state, root)
            && selected != self.selected
        {
            self.selected = selected;
            self.place_list();
        }
        if self.align_highlight_on_sync {
            self.align_highlight_on_sync = false;
            self.place_list();
            let filtered = self.filtered();
            self.list.set_highlight(Some(self.selected_row(&filtered)));
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.adopt_theme(theme);
        if self.menu.is_some() {
            // A menu button is an icon-only control of its size.
            self.caption_height = 0.0;
            let side = f32_of_f64(self.control_size.height(theme));
            let node = taffy.leaf_node(Style {
                size: Size::from_lengths(side, side),
                flex_shrink: 0.0,
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        self.caption_height = if self.caption.is_some() {
            self.theme_caption_height
        } else {
            0.0
        };
        let width = if self.fill {
            Dimension::percent(1.0)
        } else if self.measured > 0.0 {
            Dimension::length(f32_of_f64(self.field_width(self.measured)))
        } else {
            Dimension::auto()
        };
        let style = Style {
            size: Size {
                width,
                height: Dimension::length(f32_of_f64(self.field_height + self.caption_height)),
            },
            min_size: Size {
                width: Dimension::length(f32_of_f64(PAD_X + CHEVRON_W)),
                height: Dimension::auto(),
            },
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.place(crate::widget::layout_rect(taffy, node, origin));
        self.place_list();
    }

    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        if self.menu.is_some() {
            let side = f32_of_f64(self.field_height);
            return Size {
                width: known.width.unwrap_or(side),
                height: known.height.unwrap_or(side),
            };
        }
        let width = if self.fill {
            PAD_X + CHEVRON_W
        } else {
            self.measured = self.measured_width(text);
            self.field_width(self.measured)
        };
        Size {
            width: known.width.unwrap_or(f32_of_f64(width)),
            height: known
                .height
                .unwrap_or(f32_of_f64(self.field_height + self.caption_height)),
        }
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut TaffyTree<()>,
        node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        if self.fill || self.menu.is_some() {
            return false;
        }
        self.measured = self.measured_width(text);
        let Ok(current) = tree.style(node) else {
            return false;
        };
        let mut style = current.clone();
        style.size.width = Dimension::length(f32_of_f64(self.field_width(self.measured)));
        if style == *current {
            return false;
        }
        tree.set_style(node, style).is_ok()
    }

    /// The width follows the options, the placeholder, the caption, the
    /// icon and the size, and nothing else: an idle dropdown never asks for
    /// a layout pass.
    fn is_layout_dirty(&self) -> bool {
        self.outer == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(viewport) = presence.viewport.get() {
            self.note_viewport(viewport);
        }
        self.adopt_theme(theme);
        if let Some(icon) = self.menu {
            self.draw_menu_trigger(scene, transform, text_renderer, theme, icon);
            return;
        }
        if self.caption.is_some() && (self.caption_height - self.theme_caption_height).abs() > 0.01
        {
            self.caption_height = self.theme_caption_height;
            self.place(self.outer);
            self.place_list();
        }
        let disabled = !self.effectively_enabled();
        let filtering = self.open && self.supports_filter();
        let (label, placeholder) = if self.has_selected_value() {
            (self.selected_label().to_string(), false)
        } else {
            (self.placeholder.clone(), true)
        };
        paint_face(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            &FacePaint {
                outer: self.outer,
                field: self.rect,
                caption: self.caption.as_deref(),
                icon: if filtering {
                    Some(crate::icons::Icon::Search)
                } else {
                    self.leading_icon
                },
                label: (!filtering).then_some((label.as_str(), placeholder)),
                size: self.control_size,
                open: self.open,
                hovered: self.is_hovered,
                focused: self.is_focused,
                disabled,
            },
        );
        if filtering {
            self.filter_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.filter,
                    selection: &self.filter_selection,
                    placeholder: &widget_text("dropdown-search-placeholder"),
                    show_caret: self.is_focused,
                },
                self.query_rect,
                &self.control_size.label_style(theme),
                QueryInk::of(&theme.tokens.colors),
            );
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.appear.follow(self.open, theme.motion());
        if !self.appear.is_drawn() {
            self.list_pose = crate::motion::Pose::REST;
            return;
        }
        if let Some(viewport) = presence.viewport.get() {
            self.note_viewport(viewport);
        }
        let list = self.list_rect();
        let pose = self.appear.pose(list, self.rect.center());
        self.list_pose = pose;
        let bounds = crate::widget::floating_surface::painted_bounds(list, theme);
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.list.paint(scene, transform, text_renderer, theme);
        });
    }

    fn needs_animation_frame(&self) -> bool {
        self.appear.is_moving() || self.list.needs_animation_frame()
    }

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.list.advance();
        false
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.effectively_enabled() {
            return;
        }
        if self.open {
            self.refresh_list();
        }
        if self.open
            && self.is_focused
            && self.options.len() >= FILTER_MIN_OPTIONS
            && let WindowEvent::Ime(_) = event
        {
            let change = self.filter_composition.handle_event(
                event,
                &mut self.filter,
                &mut self.filter_selection,
                false,
                false,
            );
            if change == QueryEventChange::Edited {
                self.filter_edited();
            }
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = ctx.logical(position);
                self.on_cursor_moved(event, pos, ctx);
            }
            // The pointer left the window: no `CursorMoved` follows, so the
            // resting hover veneer would otherwise stay painted.
            WindowEvent::CursorLeft { .. } => {
                self.is_hovered = false;
                self.list.pointer_left();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.scroll_event(event, ctx);
                if *ctx.handled {
                    return;
                }
                self.on_left_press(
                    ctx.state.doc(),
                    ctx.path,
                    ctx.handled,
                    ctx.focus_target,
                    ctx.node_id,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.open => {
                self.scroll_event(event, ctx);
            }
            WindowEvent::MouseWheel { delta, .. } if self.open => {
                if !self.scroll_event(event, ctx) {
                    self.on_wheel(delta, ctx.handled);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    ke @ KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.on_key(
                    event,
                    ke,
                    *ctx.modifiers,
                    ctx.state.doc(),
                    ctx.path,
                    ctx.handled,
                );
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "Dropdown"
    }

    fn test_id(&self) -> Option<&str> {
        if self.id.is_empty() {
            Some("dropdown")
        } else {
            Some(self.id.as_str())
        }
    }

    fn get_rect(&self) -> Rect {
        self.outer
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        if !focused {
            self.close();
        }
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.open
            .then(|| self.filter_composition.ime_cursor_rect())
            .flatten()
    }
    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.ime_cursor_rect()?;
        self.filter_composition
            .ime_edit_fingerprint(&self.filter, &self.filter_selection, key)
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        Some(crate::ime::InputContent::TEXT)
    }
    fn is_enabled(&self) -> bool {
        self.effectively_enabled()
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Explicit names are Fluent ids. Keep the legacy value/placeholder
        // fallback for unlabeled standalone dropdowns.
        let name = match &self.accessible_name {
            Some(name) => crate::i18n::spoken_name(name, self.name_resolved),
            None => self.caption.clone(),
        };
        // What the chooser shows stands in for a name where nothing named it,
        // so a reader hears what it is set to; it is the chooser's own value,
        // never the words for what it sets, so a row that holds those says
        // them instead.
        let found = name.is_none().then(|| {
            if self.has_selected_value() {
                self.selected_label().to_string()
            } else {
                self.placeholder.clone()
            }
        });
        let value = if self.open && !self.filter.is_empty() {
            Some(self.filter.clone())
        } else {
            self.has_selected_value()
                .then(|| self.selected_label().to_string())
        };
        let role = if self.menu.is_some() {
            Role::Button
        } else {
            Role::ComboBox
        };
        let value = if self.menu.is_some() { None } else { value };
        let mut node = match found.filter(|found| !found.is_empty()) {
            Some(found) => AccessNode::new(role).with_found_name(found),
            None => AccessNode::new(role).with_optional_name(name),
        }
        .with_optional_value(value)
        .with_state(
            AccessState::new()
                .focusable(self.is_focusable())
                .focused(self.is_focused())
                .disabled(!self.is_enabled())
                .expanded(self.open),
        );
        if let Some(idx) = self.values.iter().position(|value| *value == self.selected) {
            node = node.with_position(idx.saturating_add(1), self.options.len());
        }
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("dropdown"));
        }
        if self.open && self.supports_filter() {
            node = node.with_description(widget_text("dropdown-search-description"));
        }
        if self.open {
            node = node
                .with_children(vec![self.options_access()])
                .with_keyed_action_routes(self.test_id().unwrap_or("dropdown"));
        }
        Some(node)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
        }
        if self.open {
            self.refresh_list();
        }
        if let Some(&key) = key_path.last()
            && let Some(row) = self.filtered().iter().position(|&index| {
                self.values
                    .get(index)
                    .is_some_and(|value| Self::option_key(value) == key)
            })
        {
            if !self.open || !matches!(action, Action::Activate) {
                return false;
            }
            let row = self.rows_at.get(row).copied().unwrap_or(row);
            self.list.set_highlight(Some(row));
            self.select_highlighted(state.doc(), root);
            return true;
        }
        match action {
            Action::Expand => {
                self.do_open();
                true
            }
            Action::Collapse => {
                self.close();
                true
            }
            Action::Activate => {
                if self.open {
                    self.close();
                } else {
                    self.do_open();
                }
                true
            }
            Action::SetValue(value) => {
                let Some(index) = self
                    .values
                    .iter()
                    .position(|option| option == value)
                    .or_else(|| self.options.iter().position(|option| option == value))
                else {
                    return false;
                };
                let Some(value) = self.values.get(index).cloned() else {
                    return false;
                };
                self.commit_value(&value, state.doc(), root);
                self.close();
                true
            }
            _ => false,
        }
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.open.then(|| self.rect.union(self.list_rect()))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.open
    }
}

impl crate::resources::PublishesResources for Dropdown {
    fn resources(&self, _doc: &crate::automerge::AutoCommit) -> Vec<tilo_actions::ResourceSpec> {
        self.change_verb
            .iter()
            .map(|verb| verb.resource())
            .collect()
    }
}

impl crate::widget::ResourceOperations for Dropdown {
    fn start_operation(
        &mut self,
        op: &super::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &crate::automerge::ObjId,
    ) -> Result<Option<super::ResourceOperationStart>, String> {
        let changed = self
            .change_verb
            .as_ref()
            .map(|verb| verb.start(op))
            .transpose()?
            .flatten();
        let Some(value) = changed else {
            return crate::widget::resource_operation::start_by_default(self, op, doc, path);
        };
        // A menu's choice is a command, reported and never kept, as the
        // press path reports it; a chooser keeps the value it was told.
        if self.menu.is_none() {
            self.selected.clone_from(&value);
        }
        if let Some(callback) = self.on_change.as_mut() {
            callback(&value);
        }
        Ok(Some(super::ResourceOperationStart::Completed(
            crate::tilo_actions::ActionOutput::default(),
        )))
    }
}

impl crate::widget::controls::ActionControls for Dropdown {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &mut self.action {
            let _ = action.install_custody(domain, attachment);
        }
        if let Some(verb) = &mut self.change_verb {
            verb.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_mut()
            .and_then(ActionEmitter::take)
            .or_else(|| {
                self.change_verb
                    .as_mut()
                    .and_then(super::control_verb::ControlVerb::take)
            })
    }
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let control_id = self.test_id().unwrap_or("dropdown");
        if let Some(verb) = &self.change_verb {
            return vec![verb.metadata(control_id)];
        }
        crate::widget::controls::value_control_bindings(
            control_id,
            self.action.as_ref(),
            self.binding.as_ref().and_then(|b| b.document_key()),
            self.on_change.is_some(),
        )
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        if let Some(action) = &mut self.action {
            action.refresh_availability(allowed);
        }
        if let Some(verb) = &mut self.change_verb {
            verb.refresh_availability(allowed);
        }
        if self.interactive_open && !self.effectively_enabled() {
            self.close();
        }
    }
}

fn caption_height_in(style: &TextStyle, gap: f64) -> f64 {
    f64::from(style.line_height_px()) + gap
}

fn leading_icon_edge(style: &TextStyle) -> f64 {
    f64::from(style.line_height_px()).round()
}

fn leading_icon_gap(style: &TextStyle) -> f64 {
    (leading_icon_edge(style) / 3.0).round()
}

fn field_width_for<'s>(
    labels: impl IntoIterator<Item = &'s str>,
    icon: bool,
    style: &TextStyle,
    text: &mut TextRenderer,
) -> f64 {
    let widest = labels
        .into_iter()
        .filter(|label| !label.is_empty())
        .map(|label| f64::from(text.measure_style(label, None, style).0))
        .fold(0.0, f64::max);
    let lead = if icon {
        leading_icon_edge(style) + leading_icon_gap(style)
    } else {
        0.0
    };
    // Half a pixel of slack, so a field sized to its label never elides it
    // over shaping round-off.
    (PAD_X + lead + widest + 0.5 + CHEVRON_W).ceil()
}

fn content_rect(field: Rect, icon: bool, style: &TextStyle) -> Rect {
    let lead = if icon {
        leading_icon_edge(style) + leading_icon_gap(style)
    } else {
        0.0
    };
    Rect::new(
        (field.x0 + PAD_X + lead).min(field.x1),
        field.y0,
        (field.x1 - CHEVRON_W).max(field.x0),
        field.y1,
    )
}

struct FacePaint<'a> {
    outer: Rect,
    field: Rect,
    caption: Option<&'a str>,
    icon: Option<crate::icons::Icon>,
    label: Option<(&'a str, bool)>,
    size: crate::widget::ControlSize,
    open: bool,
    hovered: bool,
    focused: bool,
    disabled: bool,
}

fn paint_face(ctx: &mut PaintCtx<'_>, face: &FacePaint<'_>) {
    let theme = ctx.theme;
    let style = face.size.label_style(theme);
    if let Some(caption) = face.caption {
        let caption_style = theme.text(TextRole::Caption);
        let shown = ctx.text.elide_to_width_style(
            caption,
            f32_of_f64(face.outer.width().max(1.0)),
            &caption_style,
        );
        ctx.text(
            &shown,
            &caption_style,
            theme.text_dim,
            Point::new(face.outer.x0, face.outer.y0),
            None,
        );
    }
    let radius = f64::from(theme.tokens.radii.control);
    let shape = RoundedRect::from_rect(face.field, radius);
    if face.disabled {
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            crate::widget::DISABLED_ALPHA,
            ctx.transform,
            &shape,
        );
    }
    let raised = theme.tokens.colors.surfaces.raised;
    let active = (face.open || face.focused) && !face.disabled;
    let bg = if active {
        theme.tokens.colors.lift_on(raised, 1.0)
    } else if face.hovered && !face.disabled {
        theme.tokens.colors.lift_on(raised, 0.5)
    } else {
        raised
    };
    let bordered = crate::surface::control_face(ctx.scene, ctx.transform, &shape, bg, theme);
    let (bw, bc) = if active {
        (2.0, theme.tokens.colors.intent.accent_ink)
    } else {
        (1.0, crate::surface::edge_on(raised))
    };
    if active || !bordered {
        ctx.scene.stroke(
            &Stroke::new(bw),
            ctx.transform,
            bc,
            None,
            &crate::surface::edge_outline(face.field, radius),
        );
    }

    let content = content_rect(face.field, face.icon.is_some(), &style);
    if let Some(icon) = face.icon {
        let edge = leading_icon_edge(&style).min(face.field.height());
        let x = face.field.x0 + PAD_X;
        let cy = face.field.center().y;
        crate::icons::draw_icon(
            ctx.scene,
            ctx.transform,
            Rect::new(x, cy - edge / 2.0, x + edge, cy + edge / 2.0),
            icon,
            theme.text_dim,
            crate::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
    }
    if let Some((label, placeholder)) = face.label
        && !label.is_empty()
    {
        let color = if placeholder {
            theme.tokens.colors.text_tertiary
        } else {
            theme.text_primary
        };
        let room = f32_of_f64(content.width().max(0.0) + 0.5);
        let shown = ctx.text.elide_to_width_style(label, room, &style);
        let (_, th) = ctx.text.measure_style(&shown, None, &style);
        let ty = face.field.y0 + (face.field.height() - f64::from(th)) / 2.0;
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &shape,
        );
        ctx.text(&shown, &style, color, Point::new(content.x0, ty), None);
        ctx.scene.pop_layer();
    }

    let edge = (f64::from(theme.tokens.metrics.icon_sm) * f64::from(theme.text_scale.max(1.0)))
        .min(CHEVRON_W - 2.0);
    let slot_center = Point::new(face.field.x1 - CHEVRON_W / 2.0, face.field.center().y);
    crate::icons::draw_icon(
        ctx.scene,
        ctx.transform,
        Rect::from_center_size(slot_center, (edge, edge)),
        if face.open {
            crate::icons::Icon::ChevronUp
        } else {
            crate::icons::Icon::ChevronDown
        },
        theme.text_dim,
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
    if face.disabled {
        ctx.scene.pop_layer();
    }
}

/// An immediate-mode dropdown: the closed [`Dropdown`] look for a painted
/// host (a camera's controls, a reader's settings) that owns its own
/// hit-testing. Pair it with an option [`crate::widget::Popover`]
/// (`Popover::new().with_items(..)`, opened with
/// `open_at(face.field_rect(theme), Some(selected))`) for the list.
///
/// Size the rect from the face, so rect and glyphs agree at every scale:
/// [`DropdownFace::outer_height`] (the field, plus a caption when it has
/// one) and [`DropdownFace::intrinsic_width`] for the widest option.
pub struct DropdownFace<'a> {
    rect: Rect,
    value: &'a str,
    placeholder: &'a str,
    caption: Option<&'a str>,
    icon: Option<crate::icons::Icon>,
    size: crate::widget::ControlSize,
    open: bool,
    hovered: bool,
    focused: bool,
    disabled: bool,
}

impl<'a> DropdownFace<'a> {
    /// A closed dropdown at `rect` showing `value`.
    #[must_use]
    pub fn new(rect: Rect, value: &'a str) -> Self {
        Self {
            rect,
            value,
            placeholder: "",
            caption: None,
            icon: None,
            size: crate::widget::ControlSize::Regular,
            open: false,
            hovered: false,
            focused: false,
            disabled: false,
        }
    }
    /// Shown, in the weaker placeholder ink, while `value` is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }
    /// A short label stacked above the field ("Level", "Source").
    #[must_use]
    pub fn caption(mut self, caption: &'a str) -> Self {
        self.caption = Some(caption);
        self
    }
    /// A glyph before the value that says what the choice is.
    #[must_use]
    pub fn leading_icon(mut self, icon: crate::icons::Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// The field's height and label role.
    #[must_use]
    pub fn control_size(mut self, size: crate::widget::ControlSize) -> Self {
        self.size = size;
        self
    }
    /// Its option list is showing: the field lifts and the chevron turns.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }
    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    #[must_use]
    pub fn height(size: crate::widget::ControlSize, theme: &Theme) -> f64 {
        size.height(theme)
    }

    /// This face's whole height: the field, and its caption above it.
    #[must_use]
    pub fn outer_height(&self, theme: &Theme) -> f64 {
        Self::height(self.size, theme) + self.caption_height(theme)
    }

    fn caption_height(&self, theme: &Theme) -> f64 {
        if self.caption.is_some() {
            caption_height_in(
                &theme.text(TextRole::Caption),
                f64::from(theme.tokens.spacing.s1),
            )
        } else {
            0.0
        }
    }

    /// The narrowest rect that shows every one of `options` (and the placeholder) beside
    /// the icon and the chevron, and the caption above.
    #[must_use]
    pub fn intrinsic_width<'s>(
        &self,
        options: impl IntoIterator<Item = &'s str>,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> f64 {
        let style = self.size.label_style(theme);
        let mut labels: Vec<&str> = options.into_iter().collect();
        labels.push(self.placeholder);
        labels.push(self.value);
        let field = field_width_for(labels, self.icon.is_some(), &style, text);
        let caption = self.caption.map_or(0.0, |caption| {
            f64::from(
                text.measure_style(caption, None, &theme.text(TextRole::Caption))
                    .0,
            )
            .ceil()
        });
        field.max(caption)
    }

    /// The field below the caption: what the option list anchors to and what a pointer
    /// opens.
    #[must_use]
    pub fn field_rect(&self, theme: &Theme) -> Rect {
        let caption = self.caption_height(theme);
        Rect::new(
            self.rect.x0,
            (self.rect.y0 + caption).min(self.rect.y1),
            self.rect.x1,
            self.rect.y1,
        )
    }

    /// Whether `point` opens the list: the field, grown to the minimum pointer target.
    #[must_use]
    pub fn contains(&self, theme: &Theme, point: Point) -> bool {
        let field = self.field_rect(theme);
        let target = f64::from(theme.tokens.metrics.min_pointer_target);
        let grow_y = ((target - field.height()) / 2.0).max(0.0);
        field.inflate(0.0, grow_y).contains(point)
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let (label, placeholder) = if self.value.is_empty() {
            (self.placeholder, true)
        } else {
            (self.value, false)
        };
        paint_face(
            &mut PaintCtx::new(scene, transform, text, theme),
            &FacePaint {
                outer: self.rect,
                field: self.field_rect(theme),
                caption: self.caption,
                icon: self.icon,
                label: Some((label, placeholder)),
                size: self.size,
                open: self.open,
                hovered: self.hovered,
                focused: self.focused,
                disabled: self.disabled,
            },
        );
    }
}

#[cfg(test)]
mod tests;

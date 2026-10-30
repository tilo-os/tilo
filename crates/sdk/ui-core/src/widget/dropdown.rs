//! [`Dropdown`] — a reusable, CRDT-backed single-select control.
//!
//! Closed it reads like a button: the current selection plus a chevron. Open
//! it reveals a scrollable list that composites *on top of* following content
//! via the App's top-layer [`Widget::draw_overlay`] pass — a real overlay,
//! not an inline expander that shoves siblings down.
//!
//! Like [`crate::widget::TextInput`], the chosen value can live in either an
//! explicit durable or process-local model. [`Dropdown::new`] binds a
//! document key; [`Dropdown::from_state`] binds a typed
//! [`crate::LocalState<String>`]. In both forms `sync` mirrors model changes
//! back without app-owned fan-out.
//!
//! Keyboard (while focused): Enter/Space/Down open; Up/Down move the
//! highlight; Enter selects + closes; Esc dismisses. In a list of
//! [`FILTER_MIN_OPTIONS`] or more, printable characters drive an
//! incremental, case-insensitive substring filter (Backspace edits it) so
//! hundred-entry lists stay usable without endless scrolling. Opening a
//! searchable list shows its search prompt and caret immediately. A
//! shorter list never filters: letters typed in quick succession arm the
//! first option their name begins, one letter again steps on through the
//! options it begins ([`crate::type_ahead`], as every list answers typing).
//!
//! Closed, the control is as wide as its widest option (`fill` spans the
//! parent). Inside a [`crate::widget::Section`] or
//! [`crate::widget::FormSection`] the section's choosers share one width:
//! the widest option among them, capped at the theme's chooser width, so a
//! column of trailing dropdowns has two straight edges. [`DropdownFace`]
//! paints the same closed control for a painted host, paired with an option
//! [`crate::widget::Popover`].
//!
//! [`Dropdown::as_menu`] makes it a menu button instead: an icon that opens
//! a list of choices, each one a command rather than a value it keeps.
//!
//! While open the widget reports [`Widget::wants_pointer_capture`] so its
//! container routes all events to it exclusively — how the out-of-layout
//! list becomes hit-testable and click-outside dismissal works.

use crate::num::f32_of_f64;
use crate::widget::autocomplete::{Suggestion, SuggestionList};
use crate::widget::{AppModel, EventCtx, ModelPath};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::widget::TaffyTreeExt;
use crate::widget::binding::{BoxedBinding, Pulled};
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

/// The narrowest a menu button's list opens, so its choices read in full
/// beside an icon-sized trigger: a utility panel's width.
fn menu_min_width() -> f64 {
    f64::from(crate::tokens::Metrics::BASE.panel_width_utility)
}

type SelectionChangeCallback = Box<dyn FnMut(&str)>;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent widget-configuration or ephemeral-UI-state flag (open/hover/focus/enabled/etc), not a state machine; grouping them would misrepresent orthogonal concerns and ripple through every accessor in this file"
)]
pub struct Dropdown {
    id: String,
    /// Fluent message id (or, with `name_resolved`, resolved text) spoken
    /// independently from the selected value.
    accessible_name: Option<String>,
    /// Stable machine values parallel to the human-readable option labels.
    /// The legacy constructor uses equal value/label pairs.
    values: Vec<String>,
    options: Vec<String>,
    /// The keys that do what an option does, by its value, shown at the
    /// row's end so a menu teaches its shortcuts.
    shortcuts: Vec<(String, String)>,
    /// Where the selected value lives: a document key or process-local state.
    binding: Option<BoxedBinding<String>>,
    on_change: Option<SelectionChangeCallback>,
    action: Option<ActionEmitter<String>>,
    placeholder: String,

    /// Mirror of the bound value. Empty when unset.
    selected: String,
    open: bool,
    /// An accepted interaction opened this popup, rather than a shot seed.
    interactive_open: bool,
    /// Incremental substring filter; empty shows every option.
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    /// What has been typed into an open short list.
    typing: crate::type_ahead::TypeAhead,
    /// The open list: the options the filter shows, the highlighted one
    /// armed, scrolled to keep it in view.
    list: SuggestionList,
    query_rect: Rect,

    /// One-shot: align `highlight` to the selected option on the next
    /// `sync`. Set by the `open()` builder, where `selected` isn't known
    /// until `sync` reads the document.
    align_highlight_on_sync: bool,

    /// The field: the closed control, below its caption when it has one.
    rect: Rect,
    /// The whole widget: the caption (if any) above the field.
    outer: Rect,
    /// A short label stacked above the field ("Level", "Source").
    caption: Option<String>,
    /// Height the caption takes above the field in the current theme.
    caption_height: f64,
    /// The field's height and label role.
    control_size: crate::widget::ControlSize,
    is_focused: bool,
    is_hovered: bool,
    last_cursor: Point,
    /// The host viewport from the latest paint, so the open list can flip
    /// above the control instead of running off the bottom of the window.
    /// `None` until a paint has reported one (bare harnesses never do).
    viewport: Option<Rect>,

    /// When `false`: skipped by focus, rejects input, renders dimmed.
    /// Default `true`.
    enabled: bool,
    /// A menu button's icon: the control is that icon, and a choice is a
    /// command it reports rather than a value it shows.
    menu: Option<crate::icons::Icon>,
    /// What the last painted frame showed, so an unchanged control reports
    /// no damage.
    drawn: Option<u64>,

    /// Span the parent's width instead of hugging the widest option.
    fill: bool,
    /// A glyph before the value: what kind of thing the choice is.
    leading_icon: Option<crate::icons::Icon>,
    /// `accessible_name` is already resolved text, spoken as given.
    name_resolved: bool,
    /// The field's height in the theme of the latest layout or paint; what
    /// [`Self::apply_layout_external`] stacks the caption above.
    field_height: f64,
    /// The label and caption styles of the latest layout, for measuring.
    label_style: TextStyle,
    caption_style: TextStyle,
    theme_caption_height: f64,
    /// The width measured for the options in the latest layout.
    measured: f64,
    /// The width the choosers of the enclosing section share, when this
    /// dropdown is one of them.
    column: Option<ChooserColumn>,
    /// The widest a shared chooser grows: the theme's chooser width, grown
    /// with the text.
    chooser_cap: f64,
    /// Host-owned pointer precision controls direct-touch target geometry.
    interaction: crate::interaction::InteractionEnvironment,
    /// An option, the placeholder, the caption or the icon changed since the
    /// last layout, so the measured width may have.
    layout_dirty: bool,
    /// The list's coming from the field and going back (`motion::Appear`).
    appear: crate::motion::Appearing,
    /// The pose the list was last drawn in: rows are hit where they are
    /// drawn while it moves.
    list_pose: crate::motion::Pose,
}

/// Lists shorter than this never show a filter: a typed letter jumps to the
/// next option starting with it instead.
pub const FILTER_MIN_OPTIONS: usize = 8;

/// The one width a section's choosers share. Each dropdown offers the width
/// its own options need; all of them take the widest offer, capped at the
/// theme's chooser width (a longer value elides). Trailing in their rows,
/// they then line up on both edges.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChooserColumn(Rc<Cell<f64>>);

impl ChooserColumn {
    /// Forget the offers of the previous layout, so a chooser whose options
    /// got shorter lets the column narrow.
    pub(crate) fn reset(&self) {
        self.0.set(0.0);
    }

    fn offer(&self, width: f64) -> f64 {
        let shared = self.0.get().max(width);
        self.0.set(shared);
        shared
    }

    /// Enrol `widget` or the value choosers inside it: dropdowns that hug
    /// their options, without a caption of their own. A nested section keeps
    /// its own column.
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

    /// The width the control takes for options that need `own`: its own, or
    /// the section's shared chooser width.
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
    /// Construct a dropdown over `options`, keeping the chosen value as text
    /// under the document key `value_path`.
    pub fn new(options: Vec<String>, value_path: &str) -> Self {
        Self {
            id: String::new(),
            accessible_name: None,
            values: options.clone(),
            options,
            shortcuts: Vec::new(),
            binding: Some(crate::doc_source::text_key(value_path)),
            on_change: None,
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

    /// Everything the control's paint depends on.
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
    ///
    /// Model synchronization and [`Self::set_selected`] do not invoke this
    /// callback.
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

    /// Span the parent's width. Without it the control is as wide as its
    /// widest option and the chevron need, like a [`crate::widget::NumberInput`],
    /// so it sits in a row or a header's actions beside other controls.
    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self.layout_dirty = true;
        self
    }

    /// A glyph before the value that says what the choice is (a source, a
    /// scope, a calendar).
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

    /// Imperatively flip the enabled state. Disabling an open dropdown
    /// also closes it.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.close();
        }
    }

    /// Start opened. Used for headless `--shots` of the open state; the
    /// highlight is aligned to the selected option on the first `sync`.
    #[must_use]
    pub fn open(mut self) -> Self {
        self.open = true;
        // Open from its first paint: shown at rest, not arriving.
        self.appear.snap(true);
        self.align_highlight_on_sync = true;
        self
    }

    /// Conditionally start opened — convenience for callers that decide
    /// at build time (e.g. a shot seed).
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
        self.viewport = Some(viewport);
    }

    /// Builder form of [`Self::set_viewport`].
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.set_viewport(viewport);
        self
    }

    /// Replace the available options while preserving the current value when
    /// possible. Useful for embedded hosts whose choices are model-derived.
    pub fn set_options(&mut self, options: Vec<String>) {
        if self.options != options || self.values != options {
            self.layout_dirty = true;
        }
        self.values.clone_from(&options);
        self.options = options;
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
    }

    /// The keys that do what each option does, by value: shown at the
    /// end of its row (`Ctrl+S`). Options without one show none.
    pub fn set_option_shortcuts<'a>(
        &mut self,
        shortcuts: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) {
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

    /// Start on `selected`: the value the field shows, in the value's ink,
    /// until its binding reads a stored value. Use this, not a placeholder,
    /// for a setting's current or default value; a placeholder is a prompt
    /// for a field with no value.
    #[must_use]
    pub fn with_selected(mut self, selected: impl Into<String>) -> Self {
        // Shown, not published: the binding's stored value still wins, and
        // starting on the current value is not an edit.
        self.selected = selected.into();
        self
    }

    /// Set the selected model value programmatically.
    ///
    /// A bound [`crate::LocalState`] is updated immediately; this does not
    /// invoke [`Self::on_change`].
    pub fn set_selected(&mut self, selected: impl Into<String>) {
        let selected = selected.into();
        // A value outside the options (loaded ahead of them) is measured too.
        if selected != self.selected && !self.values.contains(&selected) {
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

    /// Adopt the theme's geometry: the field height, the caption height and
    /// the styles the width is measured with.
    fn adopt_theme(&mut self, theme: &Theme) {
        self.field_height = self.control_size.height(theme);
        // Preserve compact pointer controls; coarse direct input needs real
        // non-overlapping targets, not inflated neighbouring hit rectangles.
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
                [crate::i18n!("dropdown-search-placeholder").as_str()],
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

    /// Lay the widget out in `outer`: the caption on top, the field below.
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

    /// Indices into `self.options` matching the current filter, in order.
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

    /// Position of the selected option within `filtered`, or 0.
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

    fn has_selected_value(&self) -> bool {
        !self.selected.is_empty() || self.values.contains(&self.selected)
    }

    /// Presentation label for the selected stable value.
    fn selected_label(&self) -> &str {
        self.values
            .iter()
            .position(|value| *value == self.selected)
            .and_then(|index| self.options.get(index))
            .map_or(self.selected.as_str(), String::as_str)
    }

    /// Hand the open list its rows (the options the filter shows, the
    /// applied one checked, each with its keys), its anchor and its bounds.
    fn refresh_list(&mut self) {
        let rows: Vec<Suggestion> = self
            .filtered()
            .into_iter()
            .filter_map(|index| {
                let label = self.options.get(index)?;
                let value = self.values.get(index)?;
                let mut row = Suggestion::labelled(value.clone(), label.clone())
                    .with_checked(self.menu.is_none() && *value == self.selected);
                if let Some((_, keys)) = self.shortcuts.iter().find(|(of, _)| of == value) {
                    row = row.with_shortcut(keys.clone());
                }
                Some(row)
            })
            .collect();
        // A list that filters keeps room for its empty-result line, so it
        // keeps its width while a query narrows it.
        let hint = (rows.is_empty() || self.supports_filter())
            .then(|| crate::i18n!("dropdown-no-matches"));
        self.list.set_suggestions(rows);
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

    /// The open list's panel.
    fn list_rect(&self) -> Rect {
        self.list.rect().unwrap_or(Rect::ZERO)
    }

    /// The stable key of the option whose value is `value`.
    fn option_key(value: &str) -> u64 {
        crate::a11y::hash_key(&("dropdown-option", value))
    }

    /// The open list as assistive technology and agents read it: each
    /// option the filter shows, where it is drawn (rows scrolled out of
    /// the list have no geometry), the chosen one selected and the
    /// highlighted one the combobox's active descendant, each choosable through
    /// [`Widget::perform_action`] by its key.
    fn options_access(&self) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let filtered = self.filtered();
        let total = filtered.len();
        let options = filtered
            .iter()
            .enumerate()
            .filter_map(|(row, &index)| {
                let label = self.options.get(index)?;
                let value = self.values.get(index)?;
                let mut option = AccessNode::new(Role::ListBoxOption)
                    .with_name(label.clone())
                    .with_key(Self::option_key(value))
                    .with_position(row.saturating_add(1), total)
                    // The keyboard stays on the combobox while its list is
                    // open: the highlighted option is its active descendant,
                    // announced without taking the tree's focus from it.
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
        let next = self.typing.target(
            self.options.iter().map(String::as_str),
            self.list.highlight(),
        );
        if let Some(index) = next {
            self.list.set_highlight(Some(index));
        }
    }

    /// Open the list and align highlight + scroll to the current value.
    fn do_open(&mut self) {
        self.open = true;
        self.appear.show(true);
        self.interactive_open = true;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.typing.reset();
        self.list.clear();
        self.refresh_list();
        let filtered = self.filtered();
        self.list.set_highlight(Some(self.selected_pos(&filtered)));
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
    /// A leaving list fades with the rows it showed.
    pub fn close(&mut self) {
        self.open = false;
        self.appear.show(false);
        self.interactive_open = false;
        self.list.pointer_left();
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
    }

    /// Store `value` through the binding and mirror it locally.
    fn commit_value(&mut self, value: &str, _state: &AppModel, _root: &ModelPath) {
        if self.menu.is_some() {
            // A menu's choice is a command: reported, never kept. A menu
            // bound to view state reports it there, for the view to act on.
            if let Some(action) = &mut self.action {
                let _ = action.emit(value.to_string());
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
        if self.action.is_none()
            && let Some(callback) = self.on_change.as_mut()
        {
            callback(value);
        }
    }

    /// Select the highlighted option (if any) and close.
    fn select_highlighted(&mut self, state: &AppModel, root: &ModelPath) {
        if let Some(value) = self.list.highlighted().map(|row| row.value.clone()) {
            self.commit_value(&value, state, root);
        }
        self.close();
    }

    /// `WindowEvent::CursorMoved` — hover tracking, plus (while open)
    /// following the highlight to the row under the pointer.
    fn on_cursor_moved(&mut self, event: &WindowEvent, pos: Point, ctx: &mut EventCtx<'_>) {
        self.last_cursor = pos;
        // A hover change is a repaint, not consumed input (see `Button`): a
        // parent that stops at the first handled child would otherwise never
        // deliver this move to the sibling the pointer went to, and the
        // sibling's hover — or this one's — would stick.
        self.is_hovered = self.rect.contains(pos);
        // While open, hovering a row moves the highlight; a held bar thumb follows.
        if self.open {
            if self.scroll_event(event, ctx) || *ctx.handled {
                return;
            }
            // The list is hit where it is drawn while it arrives.
            let pos = self.list_pose.to_local(pos);
            if self.list.arm_at(pos) {
                *ctx.handled = true;
            }
        }
    }

    /// Hand `event` to the open list's scroll region.
    fn scroll_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        self.open && self.list.scroll_event(event, ctx)
    }

    /// `WindowEvent::MouseInput { state: Pressed, button: Left }` — select a
    /// clicked row, close on a click outside the list, or open when closed.
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
            // The list is hit where it is drawn while it arrives.
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
                // Click-outside dismissal.
                self.close();
            }
            *handled = true;
        } else if self.rect.contains(pos) {
            *focus_target = Some(node_id);
            self.do_open();
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel` off the list: an open dropdown scrolls from the field too.
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
                // A menu shows no query to type into.
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
        } else {
            // Closed: Enter / Space / Down (or Up) open the list.
            if let Key::Named(
                NamedKey::Enter | NamedKey::Space | NamedKey::ArrowDown | NamedKey::ArrowUp,
            ) = logical_key
            {
                self.do_open();
                *handled = true;
            }
        }
    }
}

impl Dropdown {
    /// The filter changed: the list shows what it matches, from the top.
    fn filter_edited(&mut self) {
        self.refresh_list();
        self.list.set_highlight(Some(0));
        self.list.scroll_px(f64::NEG_INFINITY);
    }
}

impl Widget for Dropdown {
    crate::provides!(dyn crate::widget::controls::ActionControls);

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
        {
            self.selected = selected;
        }
        if self.align_highlight_on_sync {
            self.align_highlight_on_sync = false;
            self.refresh_list();
            let filtered = self.filtered();
            self.list.set_highlight(Some(self.selected_pos(&filtered)));
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The open list is intentionally NOT a taffy child: it paints in the
        // overlay pass, so it doesn't reserve layout space.
        //
        // The control is as wide as its widest option (measured in
        // `adjust_layout_style`), or the row's width with `fill`. In
        // both cases it is the item that gives when the row is short of
        // room, like `TextInput` and unlike fixed-size leaves such as
        // `Toggle`: with `flex_shrink: 0` a dropdown claimed the whole row
        // and pushed every later sibling past the edge to zero width. The
        // floor keeps the chevron slot and padding so a squeezed control is
        // still a control.
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
        // Before its options are measured the width is left to `measure`,
        // which the layout pass asks; afterwards it is pinned to them.
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
        self.layout_dirty = false;
    }

    /// The width every option needs, so a parent that measures its children
    /// (a form row placing a trailing control) reserves exactly that.
    fn measure(
        &mut self,
        known: Size<Option<f32>>,
        _available: Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> Size<f32> {
        if self.menu.is_some() {
            // A menu button is the square of its size laid out in
            // `build_layout`.
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        // An externally placed dropdown learns the theme here: its caption
        // takes the theme's height from now on.
        self.adopt_theme(theme);
        if let Some(icon) = self.menu {
            self.draw_menu_trigger(scene, transform, text_renderer, theme, icon);
            return;
        }
        if self.caption.is_some() && (self.caption_height - self.theme_caption_height).abs() > 0.01
        {
            self.caption_height = self.theme_caption_height;
            self.place(self.outer);
        }
        let disabled = !self.effectively_enabled();
        let filtering = self.open && self.supports_filter();
        let (label, placeholder) = if self.has_selected_value() {
            (self.selected_label().to_string(), false)
        } else {
            (self.placeholder.clone(), true)
        };
        let content = paint_face(
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
        self.query_rect = if filtering { content } else { Rect::ZERO };
        if filtering {
            self.filter_composition.draw(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                QueryView {
                    text: &self.filter,
                    selection: &self.filter_selection,
                    placeholder: &crate::i18n!("dropdown-search-placeholder"),
                    show_caret: self.is_focused,
                },
                content,
                &self.control_size.label_style(theme),
                QueryInk::of(&theme.tokens.colors),
            );
        }
    }

    /// Top-layer pass: paint the open list above all later-drawn
    /// siblings. The App composites this after the normal `draw` pass.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The list comes from the field and goes back (`motion::Appear`); a
        // leaving list is drawn as it was until it has gone, and takes no
        // input.
        self.appear.follow(self.open, theme.motion());
        if !self.appear.is_drawn() {
            self.list_pose = crate::motion::Pose::REST;
            return;
        }
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        // A leaving list is drawn with the rows it closed with.
        if self.open {
            self.refresh_list();
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

    /// A glide after a flick scrolls the list as the wheel does.
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        self.list.advance();
        false
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Disabled: reject all input — won't open, no selection, no hover.
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
            // A wheel over the list scrolls it as every painted list scrolls;
            // one elsewhere over an open dropdown goes on scrolling it too.
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
        // Losing focus (e.g. Tab away) dismisses the open list.
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
        let name = if let Some(name) = &self.accessible_name {
            Some(if self.name_resolved {
                name.clone()
            } else {
                crate::i18n!(name)
            })
        } else if let Some(caption) = &self.caption {
            Some(caption.clone())
        } else if self.has_selected_value() {
            Some(self.selected_label().to_string())
        } else if !self.placeholder.is_empty() {
            Some(self.placeholder.clone())
        } else {
            None
        };
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
        let mut node = AccessNode::new(role)
            .with_optional_name(name)
            .with_optional_value(value)
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled())
                    .expanded(self.open),
            );
        // Surface the chosen option's ordinal so the reader can say "2 of 5".
        if let Some(idx) = self.values.iter().position(|value| *value == self.selected) {
            node = node.with_position(idx.saturating_add(1), self.options.len());
        }
        if self.action.is_none() && self.on_change.is_none() {
            node = node.with_action_target(self.test_id().unwrap_or("dropdown"));
        }
        if self.open && self.supports_filter() {
            node = node.with_description(crate::i18n!("dropdown-search-description"));
        }
        if self.open {
            node = node
                .with_children(vec![self.options_access()])
                .with_keyed_action_routes(self.test_id().unwrap_or("dropdown"));
        }
        Some(node)
    }

    /// Accessibility and headless control of the combobox.
    ///
    /// Expand/collapse mirrors pointer opening, Activate toggles the popup,
    /// and `SetValue` selects one exact option through the same model mutation
    /// path as keyboard or pointer selection.
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
        // One of the open list's options, by its key: chosen as a click on
        // its row chooses it.
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

    // An open selector is a foreground query/list, so navigation containers
    // defer Escape and other global shortcuts to it before going back.
    fn modal_capture(&self) -> Option<Rect> {
        self.open.then(|| self.rect.union(self.list_rect()))
    }

    /// While open, grabs every event — see module docs.
    fn wants_pointer_capture(&self) -> bool {
        self.open
    }
}

impl crate::widget::controls::ActionControls for Dropdown {
    crate::widget::controls::optional_emitter_controls!(custody);
    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        crate::widget::controls::value_control_bindings(
            self.test_id().unwrap_or("dropdown"),
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
        if self.interactive_open && !self.effectively_enabled() {
            self.close();
        }
    }
}

/// The height of a caption stacked above a field: its line and `spacing.s1`.
fn caption_height_in(style: &TextStyle, gap: f64) -> f64 {
    f64::from(style.line_height_px()) + gap
}

/// The edge of a leading icon: the label's line, so it grows with the text
/// scale and stays the label's size.
fn leading_icon_edge(style: &TextStyle) -> f64 {
    f64::from(style.line_height_px()).round()
}

/// Between a leading icon and the value.
fn leading_icon_gap(style: &TextStyle) -> f64 {
    (leading_icon_edge(style) / 3.0).round()
}

/// The narrowest field that shows each of `labels` beside the padding, the
/// optional leading icon and the chevron slot.
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

/// Where the value sits inside `field`: after the padding and a leading
/// icon, before the chevron slot.
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

/// One closed dropdown, painted: the shared body of the retained
/// [`Dropdown`] and the immediate-mode [`DropdownFace`].
struct FacePaint<'a> {
    outer: Rect,
    field: Rect,
    caption: Option<&'a str>,
    icon: Option<crate::icons::Icon>,
    /// The value or the placeholder (`true`), or `None` while a filter
    /// query takes the field.
    label: Option<(&'a str, bool)>,
    size: crate::widget::ControlSize,
    open: bool,
    hovered: bool,
    focused: bool,
    disabled: bool,
}

/// Paint the caption, the field, its leading icon, its value and its
/// chevron; returns the rect the value occupies.
fn paint_face(ctx: &mut PaintCtx<'_>, face: &FacePaint<'_>) -> Rect {
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
    // Disabled widgets paint their (forced-idle) look under a reduced
    // group opacity so they read as non-interactive.
    if face.disabled {
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            crate::widget::DISABLED_ALPHA,
            ctx.transform,
            &shape,
        );
    }
    // The field is a Secondary control: the raised face, its lift under the
    // pointer and while open (HIG §9.2), the derived rest edge, and your
    // colour as the focus ring.
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
    // Rest edge and focus ring share one half-pixel-inset outline; the
    // theme's border stands in for the rest edge.
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
        // The placeholder is the tertiary ink, clearly weaker than a value.
        let color = if placeholder {
            theme.tokens.colors.text_tertiary
        } else {
            theme.text_primary
        };
        // One line, elided to the room left of the chevron: a max width
        // handed to `draw_text` wraps instead, and a wrapped second line
        // lands under the control's bottom edge.
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

    // The trailing chevron, centred in its slot, from the one icon set; it
    // grows with the text.
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
    content
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

    /// The narrowest rect that shows every one of `options` (and the
    /// placeholder) beside the icon and the chevron, and the caption above.
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

    /// The field below the caption: what the option list anchors to and
    /// what a pointer opens.
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

    /// Whether `point` opens the list: the field, grown to the minimum
    /// pointer target.
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

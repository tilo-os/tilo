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
//! shorter list never filters: a typed letter arms the next option starting
//! with it.
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
use crate::num::f64_of_usize;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{AppModel, ModelPath};
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
/// One row of the open list: a menu row.
const ITEM_HEIGHT: f64 = crate::tokens::Metrics::BASE.menu_row_height as f64;
/// Tallest the rows of the open list get before it scrolls.
const MAX_LIST_HEIGHT: f64 = 240.0;
const LIST_GAP: f64 = 4.0;
/// The open list's inset round its rows: the menu inset every menu shares
/// (`floating_surface::menu_inset`), so the armed row runs concentric with
/// the panel.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const LIST_PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const PAD_X: f64 = 10.0;
const CHEVRON_W: f64 = 30.0;
const LINE_PX: f32 = 60.0;

/// The narrowest a menu button's list opens, so its choices read in full
/// beside an icon-sized trigger: a utility panel's width.
fn menu_min_width() -> f64 {
    f64::from(crate::tokens::Metrics::BASE.panel_width_utility)
}

type SelectionChangeCallback = Box<dyn FnMut(&str)>;

/// Composes a paint transform with a translation. `Affine`'s `Mul` is pure
/// `f64` matrix math with no overflow trap, so it cannot panic regardless of
/// operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

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
    /// Position (0-based) into the *filtered* list of the highlighted row.
    highlight: usize,
    /// Incremental substring filter; empty shows every option.
    filter: String,
    filter_selection: Selection,
    filter_composition: QueryComposition,
    scroll: f32,
    scrollbar: Scrollbar,
    scrollbar_gutter: f64,
    scrollbar_track: Rect,
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
    /// The theme's caption height from the latest layout or paint.
    theme_caption_height: f64,
    /// The width measured for the options in the latest layout.
    measured: f64,
    /// The width the choosers of the enclosing section share, when this
    /// dropdown is one of them.
    column: Option<ChooserColumn>,
    /// The widest a shared chooser grows: the theme's chooser width, grown
    /// with the text.
    chooser_cap: f64,
    /// The open list's width for its options, from the latest paint.
    list_width: f64,
    /// One row of the open list: a menu row, grown with its text.
    item_h: f64,
    /// Host-owned pointer precision controls direct-touch target geometry.
    interaction: crate::interaction::InteractionEnvironment,
    /// An option, the placeholder, the caption or the icon changed since the
    /// last layout, so the measured width may have.
    layout_dirty: bool,
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
            highlight: 0,
            filter: String::new(),
            filter_selection: Selection::collapsed(0),
            filter_composition: QueryComposition::new(),
            scroll: 0.0,
            scrollbar: Scrollbar::vertical(),
            scrollbar_gutter: 0.0,
            scrollbar_track: Rect::ZERO,
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
            list_width: 0.0,
            item_h: ITEM_HEIGHT,
            interaction: crate::interaction::InteractionEnvironment::default(),
            layout_dirty: true,
            drawn: None,
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
            self.highlight,
            &self.filter,
            self.scroll.to_bits(),
            self.is_focused,
            self.is_hovered,
            self.effectively_enabled(),
        )
            .hash(&mut hasher);
        [self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y1]
            .map(f64::to_bits)
            .hash(&mut hasher);
        self.item_h.to_bits().hash(&mut hasher);
        self.scrollbar.is_active().hash(&mut hasher);
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
        self.highlight = self.highlight.min(self.options.len().saturating_sub(1));
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
        self.highlight = self.highlight.min(self.options.len().saturating_sub(1));
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
        self.item_h = f64::from(theme.tokens.metrics.menu_row_height)
            .max(
                (f64::from(theme.text(TextRole::Body).line_height_px())
                    + 2.0 * f64::from(theme.tokens.spacing.s1))
                .ceil(),
            )
            .max(touch_minimum);
        self.scroll = self.scroll.min(self.max_scroll(self.visible_count()));
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
        self.highlight
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

    /// Height occupied by the closed control and its open options overlay.
    ///
    /// External-layout hosts can use this to make a surrounding popover hug
    /// the visible rows without duplicating the dropdown's sizing constants.
    pub fn open_overlay_height(&self) -> f64 {
        // The laid-out control height (a `ui_scale`d `metrics.bar_height`)
        // when there is one; the reference height before the first layout.
        let control = if self.rect.height() > 0.0 {
            self.rect.height()
        } else {
            CONTROL_HEIGHT
        };
        control + LIST_GAP + self.list_box_height(self.visible_count())
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

    /// Height of the open list box for `n` filtered rows (>=1 so the
    /// "no matches" row has a box to live in).
    fn list_box_height(&self, n: usize) -> f64 {
        let inset = self.viewport.map_or(2.0 * LIST_PAD, |viewport| {
            [
                crate::widget::PopoverSide::Above,
                crate::widget::PopoverSide::Below,
            ]
            .into_iter()
            .map(|side| {
                crate::widget::floating_surface::anchored_room(self.rect, side, LIST_GAP, viewport)
            })
            .fold(0.0, f64::max)
            .min(2.0 * LIST_PAD)
        });
        self.rows_height(n) + inset
    }

    /// Height of the visible rows of the open list, inside its inset.
    fn rows_height(&self, n: usize) -> f64 {
        let authored = (f64_of_usize(n.max(1)) * self.item_h).min(MAX_LIST_HEIGHT.max(self.item_h));
        let Some(viewport) = self.viewport else {
            return authored;
        };
        let room = [
            crate::widget::PopoverSide::Above,
            crate::widget::PopoverSide::Below,
        ]
        .into_iter()
        .map(|side| {
            crate::widget::floating_surface::anchored_room(self.rect, side, LIST_GAP, viewport)
        })
        .fold(0.0, f64::max);
        authored.min((room - 2.0 * LIST_PAD).max(0.0))
    }

    /// The rows' box inside the open `list`.
    fn rows_rect(list: Rect) -> Rect {
        Rect::new(
            list.x0 + LIST_PAD,
            list.y0 + LIST_PAD.min(list.height() / 2.0),
            list.x1 - LIST_PAD,
            list.y1 - LIST_PAD.min(list.height() / 2.0),
        )
    }

    /// Absolute rect of the open list: below the control, or above it when
    /// opening below would leave the host viewport and there is more room
    /// above, placed by `floating_surface::place_anchored` like every
    /// anchored Panel.
    /// Hit-testing and paint share this, so a flipped list is clickable
    /// where it is drawn.
    fn list_rect(&self, n: usize) -> Rect {
        let h = self.list_box_height(n);
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        // At least as wide as the field, and as its widest option with the
        // check beside it; a menu at least a menu's width.
        let mut width = self.rect.width().max(self.list_width);
        if self.menu.is_some() {
            width = width.max(menu_min_width());
        }
        let width = width.min(viewport.width().max(1.0));
        let placed = crate::widget::floating_surface::place_anchored(
            self.rect,
            (width, h),
            crate::widget::PopoverSide::Below,
            LIST_GAP,
            viewport,
        )
        .0;
        if self.menu.is_some() {
            // Hangs from the trigger's trailing edge, kept inside the window.
            let x0 = (self.rect.x1 - width)
                .min(viewport.x1 - width)
                .max(viewport.x0);
            return Rect::new(x0, placed.y0, x0 + width, placed.y1);
        }
        placed
    }

    /// The open list's width for its options: each row's label, the check,
    /// the row insets and the menu inset either side.
    fn measure_list(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let style = theme.text(TextRole::Body);
        let widest = self
            .options
            .iter()
            .map(|option| f64::from(text.measure_style(option, None, &style).0))
            .fold(0.0, f64::max);
        let hint = crate::i18n!("dropdown-no-matches");
        let widest = widest.max(f64::from(text.measure_style(&hint, None, &style).0));
        let check = f64::from(theme.tokens.metrics.icon_sm);
        self.list_width = (widest + 3.0 * LIST_PAD + check + 2.0 * LIST_PAD).ceil();
    }

    fn max_scroll(&self, n: usize) -> f32 {
        let content = f64_of_usize(n) * self.item_h;
        f32_of_f64((content - self.rows_height(n)).max(0.0))
    }

    fn scroll_metrics(&self, n: usize) -> ScrollMetrics {
        ScrollMetrics::new(
            f64_of_usize(n) * self.item_h,
            self.rows_height(n),
            f64::from(self.scroll),
        )
    }

    fn option_rows(&self, list: Rect, n: usize) -> Rect {
        let mut rows = Self::rows_rect(list);
        if self.scroll_metrics(n).overflows() {
            rows.x1 = (rows.x1 - self.scrollbar_gutter).max(rows.x0);
        }
        rows
    }

    /// Filtered-list row index under `pos`, or `None` outside `list`/`n` rows.
    fn row_at(&self, pos: Point, list: Rect, n: usize) -> Option<usize> {
        let rows = self.option_rows(list, n);
        if !rows.contains(pos) {
            return None;
        }
        let rel = pos.y - rows.y0 + f64::from(self.scroll);
        if rel < 0.0 {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rel >= 0.0 is checked above; this cast is the intended \
                      floor-to-row-index conversion"
        )]
        let vi = (rel / self.item_h) as usize;
        (vi < n).then_some(vi)
    }

    /// Scroll so the row at filtered-position `vi` is fully visible.
    fn ensure_visible(&mut self, vi: usize, n: usize) {
        let box_h = f32_of_f64(self.rows_height(n));
        let top = f32_of_f64(f64_of_usize(vi) * self.item_h);
        let bottom = top + f32_of_f64(self.item_h);
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + box_h {
            self.scroll = bottom - box_h;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(n));
    }

    /// Arm the next option after the highlighted one whose label starts
    /// with `typed`, wrapping round; nothing when none does.
    fn type_ahead(&mut self, typed: &str) {
        let typed = typed.trim().to_lowercase();
        if typed.is_empty() {
            return;
        }
        let n = self.options.len();
        let next = (1..=n)
            .map(|step| self.highlight.saturating_add(step) % n)
            .find(|&index| {
                self.options
                    .get(index)
                    .is_some_and(|label| label.to_lowercase().starts_with(&typed))
            });
        if let Some(index) = next {
            self.highlight = index;
            self.ensure_visible(index, n);
        }
    }

    /// Open the list and align highlight + scroll to the current value.
    fn do_open(&mut self) {
        self.open = true;
        self.interactive_open = true;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        let filtered = self.filtered();
        self.highlight = self.selected_pos(&filtered);
        self.scroll = 0.0;
        self.ensure_visible(self.highlight, filtered.len());
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
        self.interactive_open = false;
        self.scrollbar.release();
        self.scrollbar_track = Rect::ZERO;
        self.filter.clear();
        self.filter_selection = Selection::collapsed(0);
        self.filter_composition.clear();
        self.scroll = 0.0;
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
            let _ = action.emit(value.to_string());
        } else if let Some(binding) = &mut self.binding {
            binding.publish(&value.to_string());
        }
        self.selected = value.to_string();
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
        let filtered = self.filtered();
        if let Some(&opt_idx) = filtered.get(self.highlight)
            && let Some(value) = self.values.get(opt_idx).cloned()
        {
            self.commit_value(&value, state, root);
        }
        self.close();
    }

    /// `WindowEvent::CursorMoved` — hover tracking, plus (while open)
    /// following the highlight to the row under the pointer.
    fn on_cursor_moved(&mut self, pos: Point, handled: &mut bool) {
        self.last_cursor = pos;
        // A hover change is a repaint, not consumed input (see `Button`): a
        // parent that stops at the first handled child would otherwise never
        // deliver this move to the sibling the pointer went to, and the
        // sibling's hover — or this one's — would stick.
        self.is_hovered = self.rect.contains(pos);
        // While open, hovering a row moves the highlight.
        if self.open {
            let filtered = self.filtered();
            let list = self.list_rect(filtered.len());
            let metrics = self.scroll_metrics(filtered.len());
            if let Some(offset) = self.scrollbar.drag_to(self.scrollbar_track, metrics, pos) {
                self.scroll = f32_of_f64(offset);
                *handled = true;
                return;
            }
            self.scrollbar
                .update_hover(self.scrollbar_track, metrics, pos);
            if let Some(vi) = self.row_at(pos, list, filtered.len())
                && vi != self.highlight
            {
                self.highlight = vi;
                *handled = true;
            }
        }
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
            let filtered = self.filtered();
            let list = self.list_rect(filtered.len());
            if list.contains(pos) {
                match self.scrollbar.press(
                    self.scrollbar_track,
                    self.scroll_metrics(filtered.len()),
                    pos,
                ) {
                    PressOutcome::Thumb => {
                        *handled = true;
                        return;
                    }
                    PressOutcome::Page(offset) => {
                        self.scroll = f32_of_f64(offset);
                        *handled = true;
                        return;
                    }
                    PressOutcome::Miss => {}
                }
                if let Some(vi) = self.row_at(pos, list, filtered.len()) {
                    self.highlight = vi;
                    self.select_highlighted(state, root);
                }
                *handled = true;
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
                *handled = true;
            } else {
                // Click-outside dismissal.
                self.close();
                *handled = true;
            }
        } else if self.rect.contains(pos) {
            *focus_target = Some(node_id);
            self.do_open();
            *handled = true;
        }
    }

    /// `WindowEvent::MouseWheel`, only routed here while open.
    fn on_wheel(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => *y * -LINE_PX,
            MouseScrollDelta::PixelDelta(p) => f32_of_f64(-p.y),
        };
        let n = self.filtered().len();
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll(n));
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
                Key::Named(NamedKey::ArrowDown) => {
                    let n = self.filtered().len();
                    if n > 0 {
                        self.highlight = self.highlight.saturating_add(1).min(n.saturating_sub(1));
                        self.ensure_visible(self.highlight, n);
                    }
                    *handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let n = self.filtered().len();
                    self.highlight = self.highlight.saturating_sub(1);
                    self.ensure_visible(self.highlight, n);
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
                        self.highlight = 0;
                        self.scroll = 0.0;
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
            let filtered = self.filtered();
            self.highlight = self.selected_pos(&filtered);
            self.ensure_visible(self.highlight, filtered.len());
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
                size: Size {
                    width: Dimension::length(side),
                    height: Dimension::length(side),
                },
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
        if !self.open {
            return;
        }
        if let Some(viewport) = presence.viewport.get() {
            self.viewport = Some(viewport);
        }
        self.measure_list(text_renderer, theme);
        let filtered = self.filtered();
        let list = self.list_rect(filtered.len());
        // The open list is a floating Panel (HIG §9.1 / §9.5): the panel
        // radius, the floating shadow, and the floating material with its
        // derived edge, so it separates from the field it drops from.
        let radius = f64::from(theme.tokens.radii.panel);
        crate::surface::shadow(
            scene,
            transform,
            list,
            radius,
            &theme.tokens.elevation.floating,
        );
        crate::surface::panel(scene, transform, list, radius, theme);

        // Rows sit inside the menu inset and scroll inside it.
        self.scrollbar_gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let inset = Self::rows_rect(list);
        let rows = self.option_rows(list, filtered.len());
        self.scrollbar_track = if self.scroll_metrics(filtered.len()).overflows() {
            self.scrollbar
                .track_in(Rect::new(rows.x1, inset.y0, inset.x1, inset.y1), theme)
        } else {
            Rect::ZERO
        };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &rows);

        if filtered.is_empty() {
            // Centre on the measured line, not the font size (the face's
            // ascent/descent otherwise sits the label low in the row).
            let hint = crate::i18n!("dropdown-no-matches");
            let (_, hint_h) = text_renderer.measure_style(&hint, None, &theme.text(TextRole::Body));
            let ty = rows.y0 + (self.item_h - f64::from(hint_h)) / 2.0;
            text_renderer.draw_style(
                scene,
                &hint,
                theme.text_dim,
                translated(transform, rows.x0 + LIST_PAD, ty),
                None,
                &theme.text(TextRole::Body),
            );
        } else {
            for (vi, &opt_idx) in filtered.iter().enumerate() {
                let row_top = rows.y0 + (f64_of_usize(vi) * self.item_h) - f64::from(self.scroll);
                let row_bottom = row_top + self.item_h;
                // Cull rows scrolled out of the box (hundreds of zones).
                if row_bottom < rows.y0 || row_top > rows.y1 {
                    continue;
                }
                let Some(opt) = self.options.get(opt_idx) else {
                    continue;
                };
                let row = Rect::new(rows.x0, row_top, rows.x1, row_bottom);
                let is_selected = self.menu.is_none()
                    && self
                        .values
                        .get(opt_idx)
                        .is_some_and(|value| *value == self.selected);
                // The armed row is the panel's strong lift, as in every
                // menu (HIG §9.3);
                // the applied value carries a trailing check.
                let color = if vi == self.highlight {
                    crate::widget::floating_surface::paint_armed_row(
                        scene, transform, row, LIST_PAD, theme,
                    )
                } else {
                    theme.text_primary
                };
                let check = f64::from(theme.tokens.metrics.icon_sm);
                if is_selected {
                    crate::icons::draw_icon(
                        scene,
                        transform,
                        Rect::from_center_size(
                            Point::new(row.x1 - LIST_PAD - check / 2.0, row.center().y),
                            (check, check),
                        ),
                        crate::icons::Icon::Check,
                        color,
                        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                // The keys that do the same at the row's end, quieter than
                // the label unless the row is highlighted.
                let mut label_end = row.x1 - LIST_PAD * 2.0 - check;
                let keys = self.values.get(opt_idx).and_then(|value| {
                    self.shortcuts
                        .iter()
                        .find(|(of, _)| of == value)
                        .map(|(_, keys)| keys.as_str())
                });
                if let Some(keys) = keys {
                    let (keys_w, keys_h) =
                        text_renderer.measure_style(keys, None, &theme.text(TextRole::Body));
                    let keys_x = label_end - f64::from(keys_w);
                    let keys_color = if vi == self.highlight {
                        color
                    } else {
                        theme.text_dim
                    };
                    text_renderer.draw_style(
                        scene,
                        keys,
                        keys_color,
                        translated(
                            transform,
                            keys_x,
                            row_top + (self.item_h - f64::from(keys_h)) / 2.0,
                        ),
                        None,
                        &theme.text(TextRole::Body),
                    );
                    label_end = keys_x - LIST_PAD;
                }
                // Rows are one `ITEM_HEIGHT` tall, so a long option elides
                // rather than wrapping into the row beneath it.
                let shown = text_renderer.elide_to_width_style(
                    opt,
                    f32_of_f64((label_end - row.x0 - LIST_PAD).max(0.0)),
                    &theme.text(TextRole::Body),
                );
                let (_, opt_h) =
                    text_renderer.measure_style(&shown, None, &theme.text(TextRole::Body));
                let ty = row_top + (self.item_h - f64::from(opt_h)) / 2.0;
                text_renderer.draw_style(
                    scene,
                    &shown,
                    color,
                    translated(transform, row.x0 + LIST_PAD, ty),
                    None,
                    &theme.text(TextRole::Body),
                );
            }
        }
        scene.pop_layer();
        self.scrollbar.draw(
            scene,
            transform,
            self.scrollbar_track,
            self.scroll_metrics(filtered.len()),
            theme,
            false,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // Disabled: reject all input — won't open, no selection, no hover.
        if !self.effectively_enabled() {
            return;
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
                self.highlight = 0;
                self.scroll = 0.0;
            }
            *ctx.handled = true;
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.on_cursor_moved(pos, ctx.handled);
            }
            // The pointer left the window: no `CursorMoved` follows, so the
            // resting hover veneer would otherwise stay painted.
            WindowEvent::CursorLeft { .. } => {
                self.is_hovered = false;
                self.scrollbar
                    .update_hover(Rect::ZERO, self.scroll_metrics(0), self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
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
                *ctx.handled |= self.scrollbar.release();
            }
            WindowEvent::MouseWheel { delta, .. } if self.open => {
                self.on_wheel(delta, ctx.handled);
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

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        Some(winit::window::ImePurpose::Normal)
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
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                expanded: Some(self.open),
                ..Default::default()
            });
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
        _key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !self.effectively_enabled() {
            return false;
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
        self.open
            .then(|| self.rect.union(self.list_rect(self.visible_count())))
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
    ctx.scene
        .fill(Fill::NonZero, ctx.transform, bg, None, &shape);
    let (bw, bc) = if active {
        (2.0, theme.tokens.colors.intent.you)
    } else {
        (1.0, crate::surface::edge_on(raised))
    };
    // Rest edge and focus ring share one half-pixel-inset outline.
    ctx.scene.stroke(
        &Stroke::new(bw),
        ctx.transform,
        bc,
        None,
        &crate::surface::edge_outline(face.field, radius),
    );

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

    /// The field's height at `size` in `theme`.
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

    /// Paint the face.
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
mod tests {
    use super::*;
    use crate::testing::Dispatch;
    use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, transaction::Transactable};

    fn opts() -> Vec<String> {
        ["UTC", "Europe/Amsterdam", "America/New_York", "Asia/Tokyo"]
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// A list long enough to filter: `opts` and four zones with no `a`.
    fn zones() -> Vec<String> {
        let mut zones = opts();
        zones.extend(
            ["Europe/Berlin", "Europe/Oslo", "Europe/Zurich", "Etc/GMT+1"]
                .iter()
                .map(ToString::to_string),
        );
        zones
    }

    /// `first`, then numbered fillers up to a list that filters.
    fn padded(first: &[&str]) -> Vec<String> {
        let mut options: Vec<String> = first.iter().map(ToString::to_string).collect();
        let mut n = 1;
        while options.len() < FILTER_MIN_OPTIONS {
            options.push(format!("Filler {n}"));
            n += 1;
        }
        options
    }

    #[test]
    fn dropdown_touch_choices_use_host_targets_and_select_stable_values() {
        for theme in [Theme::light(), Theme::dark(), Theme::high_contrast()] {
            for scale in [1.0, 1.5] {
                let selected = crate::LocalState::new(String::new());
                let mut frame = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(Dropdown::from_state(zones(), &selected).with_id("language-picker")),
                    (420, 320),
                    theme.clone().with_ui_scale(scale),
                );
                let pointer_geometry = frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        (dropdown.rect.height(), dropdown.item_h)
                    })
                    .unwrap();
                frame.tap_named("language-picker").unwrap();
                let point = frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert!(dropdown.is_open());
                        let minimum = f64::from(crate::InteractionEnvironment::TOUCH.minimum_target());
                        assert!(dropdown.rect.height() >= minimum,
                            "the actual touch field must use the host minimum target");
                        assert!(dropdown.item_h >= minimum,
                            "each actual painted and hit-tested choice must use the host touch target");
                        let rows = dropdown.option_rows(dropdown.list_rect(dropdown.visible_count()), dropdown.visible_count());
                        Point::new(rows.center().x, rows.y0 + dropdown.item_h * 1.5)
                    })
                    .unwrap();
                frame.tap_at(point.x, point.y);
                frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert!(!dropdown.is_open());
                        assert_eq!(dropdown.selected(), "Europe/Amsterdam");
                    })
                    .unwrap();
                assert_eq!(selected.get(), "Europe/Amsterdam");
                frame.set_interaction_environment(crate::InteractionEnvironment::default());
                frame
                    .with_widget_mut::<Dropdown, _>(|dropdown| {
                        assert_eq!(
                            (dropdown.rect.height(), dropdown.item_h),
                            pointer_geometry,
                            "returning to a fine pointer restores the theme's compact geometry"
                        );
                        assert_eq!(dropdown.selected(), "Europe/Amsterdam");
                    })
                    .unwrap();
            }
        }
    }

    #[test]
    fn dropdown_language_popup_stays_visible_in_a_short_scaled_window() {
        let mut frame = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(
                Dropdown::from_state(zones(), &crate::LocalState::new(String::new()))
                    .with_id("language-picker"),
            ),
            (420, 180),
            Theme::light().with_ui_scale(1.5),
        );
        frame.click_named("language-picker").unwrap();
        frame.render_png(
            &std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("dropdown-review/shared-short.png"),
        );
        let popup = frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(dropdown.is_open());
                dropdown.list_rect(dropdown.visible_count())
            })
            .unwrap();
        assert!(
            popup.y0 >= 0.0 && popup.y1 <= 180.0,
            "the actual painted and hit-tested popup must fit its viewport: {popup:?}"
        );
    }

    #[test]
    fn dropdown_language_shared_scrollbar_and_search_field_are_interactive() {
        let mut frame = crate::headless::FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(
                Dropdown::from_state(zones(), &crate::LocalState::new(String::new()))
                    .with_id("language-picker"),
            ),
            (420, 180),
            Theme::dark().with_ui_scale(1.5),
        );
        frame.click_named("language-picker").unwrap();
        let (thumb, track) = frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                let metrics = dropdown.scroll_metrics(dropdown.visible_count());
                (
                    dropdown
                        .scrollbar
                        .thumb_rect(dropdown.scrollbar_track, metrics)
                        .unwrap(),
                    dropdown.scrollbar_track,
                )
            })
            .unwrap();
        frame.drag_at(
            (thumb.center().x, thumb.center().y),
            (track.center().x, track.y1),
        );
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(
                    dropdown.scroll > 0.0,
                    "the actual shared scrollbar must scroll the list"
                );
                assert!(!dropdown.scrollbar.is_dragging());
                assert!(dropdown.is_open(), "dragging must not select an option");
            })
            .unwrap();
        frame.type_text("Europe");
        let query = frame
            .with_widget_mut::<Dropdown, _>(|dropdown| dropdown.query_rect)
            .unwrap();
        frame.click_at(query.x0 + 1.0, query.center().y);
        frame.type_text("X");
        let mut text = TextRenderer::new();
        let hint_width = f64::from(
            text.measure_style(
                &crate::i18n!("dropdown-no-matches"),
                None,
                &frame.theme().text(TextRole::Body),
            )
            .0,
        );
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(
                    dropdown.is_open(),
                    "the painted search field remains editable when clicked"
                );
                assert_eq!(
                    dropdown.filter, "XEurope",
                    "pointer caret uses the shaped query layout"
                );
                assert_eq!(dropdown.visible_count(), 0);
                let rows = dropdown.option_rows(dropdown.list_rect(0), 0);
                assert!(
                    rows.width() - LIST_PAD >= hint_width,
                    "the localized empty-result text must fit its actual painted clip"
                );
            })
            .unwrap();
    }

    #[test]
    fn dropdown_language_accessibility_selects_stable_ids_with_duplicate_labels() {
        let state = crate::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(Vec::new(), &state);
        dropdown.set_value_options(vec![
            ("first".into(), "Same label".into()),
            ("second".into(), "Same label".into()),
        ]);
        let (mut document, root) = doc_with_tz("");
        assert!(Widget::perform_action(
            &mut dropdown,
            &crate::a11y::Action::SetValue("second".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &root
        ));
        assert_eq!(state.get(), "second");
        assert!(!Widget::perform_action(
            &mut dropdown,
            &crate::a11y::Action::SetValue("foreign".into()),
            &[],
            &mut crate::widget::Edits::lend(&mut document),
            &root
        ));
        assert_eq!(state.get(), "second");
    }

    #[test]
    fn dropdown_language_empty_stable_value_keeps_its_human_label() {
        let state = crate::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(Vec::new(), &state);
        dropdown.set_value_options(vec![
            (String::new(), "Session language".into()),
            ("de_DE".into(), "German".into()),
        ]);
        let mut frame =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(dropdown), (420, 180));
        frame
            .with_widget_mut::<Dropdown, _>(|dropdown| {
                assert!(dropdown.has_selected_value());
                assert_eq!(dropdown.selected_label(), "Session language");
                let node = Widget::accessibility(dropdown).unwrap();
                assert_eq!(node.value.as_deref(), Some("Session language"));
            })
            .unwrap();
    }

    #[test]
    fn dropdown_language_revoked_action_releases_modal_capture() {
        let mut dropdown =
            Dropdown::new(opts(), "legacy").on_action(crate::widget::test_string_action_binding());
        let allowed = std::collections::HashSet::from([tilo_actions::ActionAddress::new(
            "test:control",
            "set",
        )]);
        crate::widget::controls::refresh_control_action_availability(&mut dropdown, &allowed);
        let mut doc = AutoCommit::new();
        assert!(Widget::perform_action(
            &mut dropdown,
            &crate::a11y::Action::Expand,
            &[],
            &mut crate::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert!(Widget::modal_capture(&dropdown).is_some());
        crate::widget::controls::refresh_control_action_availability(
            &mut dropdown,
            &std::collections::HashSet::new(),
        );
        assert!(!Widget::is_enabled(&dropdown));
        assert!(!dropdown.is_open());
        assert!(Widget::modal_capture(&dropdown).is_none());
        assert!(crate::widget::controls::take_control_invocation(&mut dropdown).is_none());

        // Unavailable capture-only fixtures retain their authored open state.
        let mut capture = Dropdown::new(opts(), "legacy")
            .on_action(crate::widget::test_string_action_binding())
            .open();
        crate::widget::controls::refresh_control_action_availability(
            &mut capture,
            &std::collections::HashSet::new(),
        );
        assert!(capture.is_open());
    }

    /// The open list's rows sit inside the menu inset every menu shares,
    /// so its armed row runs concentric with the panel: the inset is not a
    /// row, and the first row starts after it.
    #[test]
    fn the_open_list_insets_its_rows_like_every_menu() {
        let mut dd = Dropdown::new(opts(), "tz");
        dd.rect = Rect::new(0.0, 0.0, 240.0, 32.0);
        let n = dd.options.len();
        let list = dd.list_rect(n);
        let theme = Theme::dark();
        assert!(
            (LIST_PAD - crate::widget::floating_surface::menu_inset(&theme)).abs() < 1e-9,
            "the dropdown's inset is the shared menu inset"
        );
        let x = list.center().x;
        assert_eq!(dd.row_at(Point::new(x, list.y0 + 2.0), list, n), None);
        assert_eq!(dd.row_at(Point::new(4.0, list.y0 + 20.0), list, n), None);
        assert_eq!(
            dd.row_at(Point::new(x, list.y0 + LIST_PAD + 1.0), list, n),
            Some(0)
        );
        assert!(
            (list.height() - (f64_of_usize(n) * ITEM_HEIGHT + 2.0 * LIST_PAD)).abs() < 1e-9,
            "the list hugs its rows plus the inset: {list:?}"
        );
    }

    #[test]
    fn updating_placeholder_does_not_select_or_replace_a_private_value() {
        let selection = crate::LocalState::new(String::new());
        let mut dropdown = Dropdown::from_state(opts(), &selection);
        dropdown.set_placeholder("Waiting for tasks");
        assert_eq!(
            dropdown.accessibility().unwrap().name.as_deref(),
            Some("Waiting for tasks")
        );
        assert!(selection.with(String::is_empty));
        dropdown.set_selected("UTC");
        dropdown.set_placeholder("Choose a task");
        assert_eq!(dropdown.selected(), "UTC");
        assert_eq!(selection.with(Clone::clone), "UTC");
    }

    /// CRDT doc with `tz` seeded as a Text object holding `value`.
    fn doc_with_tz(value: &str) -> (AutoCommit, ObjId) {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let obj = doc.put_object(&root, "tz", ObjType::Text).unwrap();
        let _ = doc.splice_text(&obj, 0, 0, value);
        (doc, root)
    }

    use crate::synthetic_input::key_press;

    /// Drive a key through a focused dropdown with a throwaway taffy node.
    fn feed_key(dd: &mut Dropdown, ev: &WindowEvent, doc: &mut AutoCommit, root: &ObjId) -> bool {
        Dispatch::new().doc(doc).path(root).send(dd, ev).handled
    }

    #[test]
    fn dropdown_keyboard_opens_navigates_selects_and_escapes() {
        fn starts_closed_and_reads_value_on_sync() {
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(!dd.is_open());
            let (doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&doc, &root);
            assert_eq!(dd.selected(), "Asia/Tokyo");
        }

        fn down_opens_when_closed_and_focused() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root
            ));
            assert!(dd.is_open());
            // Highlight lands on the currently-selected option.
            assert_eq!(dd.highlight_index(), 0);
        }

        fn arrows_move_highlight_while_open() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.highlight_index(), 2);
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowUp), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.highlight_index(), 1);
        }

        fn enter_emits_selection_action_and_closes() {
            let mut dd =
                Dropdown::new(opts(), "tz").on_action(crate::widget::test_string_action_binding());
            crate::widget::controls::refresh_control_action_availability(
                &mut dd,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            // Move to "America/New_York" (index 2) and select it.
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Enter), None),
                &mut doc,
                &root
            ));
            assert!(!dd.is_open(), "selecting closes the list");
            assert_eq!(dd.selected(), "America/New_York");
            assert_eq!(
                crate::widget::controls::take_control_invocation(&mut dd)
                    .unwrap()
                    .arguments["value"],
                "America/New_York"
            );
            assert_eq!(
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("UTC"),
                "selecting submits an action and preserves the read model"
            );
        }

        fn escape_closes_without_changing_value() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Escape), None),
                &mut doc,
                &root
            ));
            assert!(!dd.is_open());
            assert_eq!(dd.selected(), "UTC", "Esc must not change the value");
            assert_eq!(
                crate::doc::read_str_or_text(&doc, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

        fn open_builder_aligns_highlight_to_selection_on_sync() {
            let mut dd = Dropdown::new(opts(), "tz").open();
            let (doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&doc, &root);
            assert!(dd.is_open());
            assert_eq!(
                dd.highlight_index(),
                3,
                "highlight tracks the selected option"
            );
        }

        starts_closed_and_reads_value_on_sync();
        down_opens_when_closed_and_focused();
        arrows_move_highlight_while_open();
        enter_emits_selection_action_and_closes();
        escape_closes_without_changing_value();
        open_builder_aligns_highlight_to_selection_on_sync();
    }

    #[test]
    fn combobox_filter_editing_handles_case_spaces_graphemes_and_ime() {
        fn typing_filters_options_case_insensitively() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            assert_eq!(dd.visible_count(), 8);
            // "ame" matches only "America/New_York".
            feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("m".into()), Some("m")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("e".into()), Some("e")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "ame");
            assert_eq!(dd.visible_count(), 1);
            // Enter selects the sole filtered match.
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Enter), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.selected(), "America/New_York");
        }

        fn space_is_part_of_an_open_combobox_query() {
            let mut dd = Dropdown::new(padded(&["New York", "New Delhi"]), "city");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            dd.set_focus(true);
            dd.do_open();
            for (key_value, text) in [
                (Key::Character("N".into()), Some("N")),
                (Key::Character("e".into()), Some("e")),
                (Key::Character("w".into()), Some("w")),
                (Key::Named(NamedKey::Space), None),
                (Key::Character("Y".into()), Some("Y")),
            ] {
                feed_key(&mut dd, &key_press(key_value, text), &mut doc, &root);
            }
            assert_eq!(dd.filter_query(), "New Y");
            assert_eq!(dd.visible_count(), 1);
        }

        fn backspace_edits_the_filter() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            );
            feed_key(
                &mut dd,
                &key_press(Key::Character("z".into()), Some("z")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.visible_count(), 0); // "az" matches nothing
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Backspace), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "a");
            // "a" matches Amsterdam, America, Asia.
            assert_eq!(dd.visible_count(), 3);
        }

        fn backspace_deletes_one_extended_grapheme_from_the_filter() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            let cluster = "👩🏽‍💻";
            feed_key(
                &mut dd,
                &key_press(Key::Character(cluster.into()), Some(cluster)),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), cluster);
            feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::Backspace), None),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "");
            assert_eq!(dd.filter_selection, Selection::collapsed(0));
        }

        fn typing_replaces_the_selected_filter_range() {
            let mut dd = Dropdown::new(zones(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            dd.filter = "Europe".into();
            dd.filter_selection = Selection {
                anchor: 0,
                cursor: dd.filter.len(),
            };
            feed_key(
                &mut dd,
                &key_press(Key::Character("亜".into()), Some("亜")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.filter_query(), "亜");
            assert_eq!(dd.filter_selection, Selection::collapsed("亜".len()));
        }

        fn ime_preedit_is_visible_but_filters_only_after_commit() {
            let mut dd = Dropdown::new(padded(&["東京", "London"]), "city");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            dd.apply_layout_external(Rect::new(10.0, 10.0, 260.0, 50.0));
            dd.set_focus(true);
            dd.do_open();

            assert!(feed_key(
                &mut dd,
                &WindowEvent::Ime(winit::event::Ime::Preedit(
                    "東京".into(),
                    Some(("東".len(), "東".len())),
                )),
                &mut doc,
                &root,
            ));
            assert_eq!(dd.filter_query(), "");
            assert_eq!(dd.visible_count(), 8);
            assert_eq!(
                Widget::accessibility(&dd).unwrap().value,
                None,
                "preedit must stay out of the accessibility value"
            );

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            Widget::draw(
                &mut dd,
                &mut scene,
                Affine::IDENTITY,
                &mut renderer,
                &Theme::dark(),
                &PresenceCtx::empty(),
            );
            let candidate =
                Widget::ime_cursor_rect(&dd).expect("open combobox publishes its caret");
            assert!(dd.get_rect().contains(candidate.center()));

            assert!(feed_key(
                &mut dd,
                &WindowEvent::Ime(winit::event::Ime::Commit("東京".into())),
                &mut doc,
                &root,
            ));
            assert_eq!(dd.filter_query(), "東京");
            assert_eq!(dd.visible_count(), 1);
            assert_eq!(
                Widget::accessibility(&dd).unwrap().value.as_deref(),
                Some("東京")
            );
        }

        typing_filters_options_case_insensitively();
        space_is_part_of_an_open_combobox_query();
        backspace_edits_the_filter();
        backspace_deletes_one_extended_grapheme_from_the_filter();
        typing_replaces_the_selected_filter_range();
        ime_preedit_is_visible_but_filters_only_after_commit();
    }

    #[test]
    fn open_list_pointer_capture_focus_and_placement() {
        fn losing_focus_closes_the_list() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            assert!(dd.is_open());
            dd.set_focus(false);
            assert!(!dd.is_open(), "Tab-away dismisses the dropdown");
        }

        fn open_dropdown_captures_pointer() {
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(!dd.wants_pointer_capture());
            dd.do_open();
            assert!(dd.wants_pointer_capture());
        }

        /// Hover is a repaint, not consumed input (the rule `Button` documents):
        /// the move that changes it is left for siblings, and leaving the window
        /// clears it because no `CursorMoved` will follow.
        fn hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
            let (mut doc, root) = doc_with_tz("UTC");
            let enter = WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(20.0, 20.0),
            };
            assert!(
                !feed_key(&mut dd, &enter, &mut doc, &root),
                "a hover change must not mark the move handled"
            );
            assert!(dd.is_hovered, "the pointer is over the control");
            let leave = WindowEvent::CursorLeft {
                device_id: winit::event::DeviceId::dummy(),
            };
            feed_key(&mut dd, &leave, &mut doc, &root);
            assert!(!dd.is_hovered, "CursorLeft clears the resting hover");
        }

        /// The open list flips above the control when opening below would leave
        /// the host viewport and there is room above; hit-testing shares the
        /// rect, so the flipped list is clickable where it is painted. The
        /// viewport arrives either through the presence context at paint or
        /// from the host via `set_viewport`; both routes are exercised.
        fn open_list_flips_above_when_it_would_leave_the_viewport() {
            let mut dd = Dropdown::new(opts(), "tz");
            dd.rect = Rect::new(0.0, 700.0, 200.0, 740.0);
            let n = opts().len();
            let below = dd.list_rect(n);
            assert!(
                below.y0 >= dd.rect.y1,
                "without a viewport the list opens below"
            );
            // Route 1: the paint pass sees the App's pushed window viewport.
            let presence = PresenceCtx::default();
            presence
                .viewport
                .set(Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            let mut scene = Scene::new();
            dd.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                &Theme::dark(),
                &presence,
            );
            assert_eq!(dd.viewport, Some(Rect::new(0.0, 0.0, 800.0, 760.0)));
            // Route 2: a host inside a Row/Column hands it over explicitly.
            let mut hosted =
                Dropdown::new(opts(), "tz").with_viewport(Rect::new(0.0, 0.0, 800.0, 760.0));
            hosted.rect = dd.rect;
            assert_eq!(hosted.list_rect(n), dd.list_rect(n));
            let flipped = dd.list_rect(n);
            assert!(
                flipped.y1 <= dd.rect.y0,
                "the list opens above the control: {flipped:?} vs {:?}",
                dd.rect
            );
            assert!(flipped.y0 >= 0.0, "and stays inside the viewport");
            assert!((flipped.height() - below.height()).abs() < 1e-6);
            assert_eq!(
                dd.row_at(Point::new(10.0, flipped.y0 + ITEM_HEIGHT / 2.0), flipped, n),
                Some(0),
                "the first row is hit where it is painted"
            );
            // No room above either: stay below rather than run off the top.
            dd.set_viewport(Rect::new(0.0, 690.0, 800.0, 760.0));
            assert!((dd.list_rect(n).y0 - below.y0).abs() < 1e-6);
        }

        /// The dropdown is the fluid item in a row (F2-16): laid beside fixed
        /// controls in a row too narrow for its `percent(100%)`, it gives the
        /// room back, so its siblings keep their size and the trailing one is
        /// still on screen instead of being pushed to zero width past the edge.
        fn dropdown_in_a_row_yields_width_to_fixed_siblings() {
            use crate::widget::{Button, Row};
            let mut row = Row::new()
                .no_wrap()
                .push(Box::new(Button::new("left", "Left")))
                .push(Box::new(Dropdown::new(opts(), "tz")))
                .push(Box::new(Button::new("right", "Right")));
            let theme = Theme::dark();
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = row.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(600.0),
                        height: taffy::AvailableSpace::Definite(80.0),
                    },
                )
                .unwrap();
            row.apply_layout(&taffy, node, Point::ZERO);
            let rects: Vec<Rect> = row.children().iter().map(|c| c.get_rect()).collect();
            assert_eq!(rects.len(), 3);
            let (left, dd, right) = (rects[0], rects[1], rects[2]);
            assert!(
                (left.width() - right.width()).abs() < 0.5 && left.width() > 0.0,
                "the fixed buttons keep their size: {left:?} / {right:?}"
            );
            assert!(
                dd.width() >= PAD_X + CHEVRON_W - 0.5,
                "the dropdown keeps at least its chevron slot: {dd:?}"
            );
            assert!(
                right.x1 <= 600.0 + 0.5,
                "the trailing button stays inside the 600 px row: {right:?}"
            );
            assert!(
                left.x1 <= dd.x0 + 0.5 && dd.x1 <= right.x0 + 0.5,
                "row order without overlap: {left:?} {dd:?} {right:?}"
            );
            assert!(
                (dd.height() - crate::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
                "the control is a Regular control: {dd:?}"
            );
        }

        fn a_caption_stacks_above_the_field_and_names_it() {
            let theme = Theme::dark();
            let mut dd = Dropdown::new(opts(), "level").caption("Level");
            let mut taffy = TaffyTree::new();
            let mut node_map = HashMap::new();
            let node = dd.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(300.0),
                        height: taffy::AvailableSpace::Definite(200.0),
                    },
                )
                .unwrap();
            dd.apply_layout(&taffy, node, Point::ZERO);
            let field = crate::widget::ControlSize::Regular.height(&theme);
            assert!((dd.rect.height() - field).abs() < 0.5, "{:?}", dd.rect);
            assert!(dd.rect.y0 > dd.outer.y0, "the caption sits above the field");
            assert_eq!(Widget::get_rect(&dd), dd.outer);
            let node = Widget::accessibility(&dd).expect("dropdown semantics");
            assert_eq!(node.name.as_deref(), Some("Level"));
        }

        fn external_overlay_height_hugs_short_lists_and_caps_long_ones() {
            let short = Dropdown::new(opts(), "tz");
            assert_eq!(
                short.open_overlay_height(),
                CONTROL_HEIGHT + LIST_GAP + 4.0 * ITEM_HEIGHT + 2.0 * LIST_PAD
            );

            let long = Dropdown::new((0..100).map(|index| index.to_string()).collect(), "value");
            assert_eq!(
                long.open_overlay_height(),
                CONTROL_HEIGHT + LIST_GAP + MAX_LIST_HEIGHT + 2.0 * LIST_PAD
            );
        }

        losing_focus_closes_the_list();
        open_dropdown_captures_pointer();
        hover_changes_do_not_consume_the_move_and_cursor_left_clears_hover();
        open_list_flips_above_when_it_would_leave_the_viewport();
        dropdown_in_a_row_yields_width_to_fixed_siblings();
        a_caption_stacks_above_the_field_and_names_it();
        external_overlay_height_hugs_short_lists_and_caps_long_ones();
    }

    #[test]
    fn committed_selection_binds_process_local_state() {
        fn local_state_binding_is_two_way_across_dropdowns() {
            let state = crate::LocalState::new(String::from("UTC"));
            let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let captured = std::rc::Rc::clone(&changes);
            let mut first = Dropdown::from_state(opts(), &state)
                .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
            let mut second = Dropdown::from_state(opts(), &state);
            let mut document = AutoCommit::new();

            state.set(String::from("Asia/Tokyo"));
            Widget::sync(&mut first, &document, &automerge::ROOT);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(first.selected(), "Asia/Tokyo");
            assert_eq!(second.selected(), "Asia/Tokyo");
            assert!(changes.borrow().is_empty(), "model sync is not user intent");

            assert!(Widget::perform_action(
                &mut first,
                &crate::a11y::Action::SetValue(String::from("Europe/Amsterdam")),
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            ));
            assert_eq!(state.get(), "Europe/Amsterdam");
            assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
            Widget::sync(&mut second, &document, &automerge::ROOT);
            assert_eq!(second.selected(), "Europe/Amsterdam");

            second.set_selected("America/New_York");
            assert_eq!(state.get(), "America/New_York");
            Widget::sync(&mut first, &document, &automerge::ROOT);
            assert_eq!(first.selected(), "America/New_York");
            assert_eq!(&*changes.borrow(), &["Europe/Amsterdam"]);
        }

        local_state_binding_is_two_way_across_dropdowns();
    }

    #[test]
    fn a_menu_button_reports_a_choice_without_keeping_it() {
        let state = crate::LocalState::new(String::new());
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let captured = std::rc::Rc::clone(&chosen);
        let mut menu = Dropdown::from_state(opts(), &state)
            .as_menu(crate::icons::Icon::Plus)
            .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
        menu.rect = Rect::new(560.0, 100.0, 592.0, 132.0);
        let mut document = AutoCommit::new();
        let root = automerge::ROOT;

        // The list hangs from the icon's trailing edge, wide enough to read.
        let list = menu.list_rect(opts().len());
        assert!((list.x1 - menu.rect.x1).abs() < 1e-6, "{list:?}");
        assert!(list.width() >= menu_min_width());
        menu.set_viewport(Rect::new(0.0, 0.0, 400.0, 800.0));
        let kept = menu.list_rect(opts().len());
        assert!(
            kept.x1 <= 400.0 && kept.x0 >= 0.0,
            "kept in the window: {kept:?}"
        );

        // Typing does not filter: a menu shows no query.
        menu.set_focus(true);
        assert!(feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::Enter), None),
            &mut document,
            &root
        ));
        assert!(menu.is_open());
        feed_key(
            &mut menu,
            &key_press(Key::Character("a".into()), Some("a")),
            &mut document,
            &root,
        );
        assert_eq!(menu.visible_count(), opts().len());
        feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::ArrowDown), None),
            &mut document,
            &root,
        );
        feed_key(
            &mut menu,
            &key_press(Key::Named(NamedKey::Enter), None),
            &mut document,
            &root,
        );

        assert_eq!(&*chosen.borrow(), &["Europe/Amsterdam"]);
        assert!(!menu.is_open());
        assert_eq!(menu.selected(), "", "a command is not kept as a value");
        assert_eq!(state.get(), "");
        let node = menu.accessibility().expect("a menu button is announced");
        assert_eq!(node.role, crate::a11y::Role::Button);
    }

    #[test]
    fn dropdown_accessibility_semantics_and_disabled_state() {
        fn semantic_actions_expand_collapse_and_reject_unknown_values() {
            use crate::a11y::Action;

            let mut dropdown = Dropdown::new(opts(), "tz");
            let (mut document, root) = doc_with_tz("UTC");
            Widget::sync(&mut dropdown, &document, &root);

            assert!(Widget::perform_action(
                &mut dropdown,
                &Action::Expand,
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &root,
            ));
            assert!(dropdown.is_open());
            assert!(Widget::perform_action(
                &mut dropdown,
                &Action::Collapse,
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &root,
            ));
            assert!(!dropdown.is_open());
            assert!(!Widget::perform_action(
                &mut dropdown,
                &Action::SetValue(String::from("not an option")),
                &[],
                &mut crate::widget::Edits::lend(&mut document),
                &root,
            ));
            assert_eq!(dropdown.selected(), "UTC");
            assert_eq!(
                crate::doc::read_str_or_text(&document, &root, "tz").as_deref(),
                Some("UTC")
            );
        }

        fn with_enabled_false_is_reported_by_is_enabled() {
            let dd = Dropdown::new(opts(), "tz").with_enabled(false);
            assert!(!Widget::is_enabled(&dd));
            assert!(
                Widget::is_enabled(&Dropdown::new(opts(), "tz")),
                "default is enabled"
            );
        }

        fn disabled_dropdown_does_not_open() {
            let mut dd = Dropdown::new(opts(), "tz").with_enabled(false);
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            let handled = feed_key(
                &mut dd,
                &key_press(Key::Named(NamedKey::ArrowDown), None),
                &mut doc,
                &root,
            );
            assert!(!dd.is_open(), "disabled dropdown must not open");
            assert!(!handled, "disabled dropdown must not swallow input");
            assert_eq!(
                dd.selected(),
                "UTC",
                "disabled dropdown must not change the value"
            );
        }

        fn accessibility_reports_combobox_role_value_and_collapsed() {
            use crate::a11y::Role;
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("Asia/Tokyo");
            dd.sync(&doc, &root);
            let node = Widget::accessibility(&dd).expect("dropdown is not presentational");
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(node.name.as_deref(), Some("Asia/Tokyo"));
            assert_eq!(node.value.as_deref(), Some("Asia/Tokyo"));
            assert_eq!(
                node.state.expanded,
                Some(false),
                "closed dropdown is collapsed"
            );
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
        }

        fn accessibility_unselected_uses_placeholder_name_and_no_value() {
            use crate::a11y::Role;
            let dd = Dropdown::new(opts(), "tz").with_placeholder("Pick a zone");
            let node = Widget::accessibility(&dd).unwrap();
            assert_eq!(node.role, Role::ComboBox);
            assert_eq!(
                node.name.as_deref(),
                Some("Pick a zone"),
                "placeholder is the name when unset"
            );
            assert_eq!(node.value, None, "no value until something is selected");
        }

        fn accessibility_reflects_open_and_disabled() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            let node = Widget::accessibility(&dd).unwrap();
            assert_eq!(node.state.expanded, Some(true), "open dropdown is expanded");
            assert!(node.state.focused);

            let dd2 = Dropdown::new(opts(), "tz").with_enabled(false);
            let node2 = Widget::accessibility(&dd2).unwrap();
            assert!(node2.state.disabled);
        }

        fn action_binding_owns_accessibility_selection() {
            let mut dropdown = Dropdown::new(opts(), "legacy")
                .on_change(|_| panic!("legacy callback must be suppressed"))
                .on_action(crate::widget::test_string_action_binding());
            let mut doc = AutoCommit::new();
            crate::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::new(),
            );
            assert!(!Widget::is_enabled(&dropdown));
            crate::widget::controls::refresh_control_action_availability(
                &mut dropdown,
                &std::collections::HashSet::from([tilo_actions::ActionAddress::new(
                    "test:control",
                    "set",
                )]),
            );
            assert!(Widget::is_enabled(&dropdown));
            assert!(Widget::perform_action(
                &mut dropdown,
                &crate::a11y::Action::SetValue("Asia/Tokyo".into()),
                &[],
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            assert!(doc.get(&automerge::ROOT, "legacy").unwrap().is_none());
            assert_eq!(
                crate::widget::controls::take_control_invocation(&mut dropdown)
                    .unwrap()
                    .arguments["value"],
                "Asia/Tokyo"
            );
        }

        semantic_actions_expand_collapse_and_reject_unknown_values();
        with_enabled_false_is_reported_by_is_enabled();
        disabled_dropdown_does_not_open();
        accessibility_reports_combobox_role_value_and_collapsed();
        accessibility_unselected_uses_placeholder_name_and_no_value();
        accessibility_reflects_open_and_disabled();
        action_binding_owns_accessibility_selection();
    }

    #[test]
    fn a_sections_choosers_share_one_width_and_line_up_on_both_edges() {
        use crate::widget::{Element, Row, Section, Spacer, Text};
        fn row(label: &str, dropdown: Dropdown) -> Element {
            Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Text::new(label)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(dropdown)),
            )
        }
        let short = || vec!["A".to_string(), "B".to_string()];
        let section = Section::titled(
            "Formats",
            vec![
                row("Zone", Dropdown::new(opts(), "tz").with_id("zone")),
                row("Size", Dropdown::new(short(), "ab").with_id("size")),
                row(
                    "Filter",
                    Dropdown::new(short(), "f")
                        .caption("Level")
                        .with_id("filter"),
                ),
                Box::new(Section::new(vec![row(
                    "Nested",
                    Dropdown::new(short(), "n").with_id("nested"),
                )])),
            ],
        );
        let app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(section), (900, 400));
        let targets = app.targets();
        let target = |id: &str| *targets.get(id).expect(id);
        let (zone, size) = (target("zone"), target("size"));
        assert!(
            (zone.width() - size.width()).abs() < 0.5 && (zone.x1 - size.x1).abs() < 0.5,
            "the section's choosers share the widest one's width and edges: {zone:?} / {size:?}"
        );
        assert!(
            target("filter").width() < zone.width() - 1.0,
            "a captioned filter keeps its own width"
        );
        assert!(
            target("nested").width() < zone.width() - 1.0,
            "a nested section shares its own width, not the outer one's"
        );

        let cap = f64::from(Theme::dark().tokens.metrics.chooser_width);
        let section = Section::new(vec![
            row("Size", Dropdown::new(short(), "ab").with_id("size")),
            row(
                "Device",
                Dropdown::new(vec!["Built-in speakers and headphone jack".repeat(3)], "d")
                    .with_id("device"),
            ),
        ]);
        let app =
            crate::headless::FrameHarness::new(crate::State::new(), Box::new(section), (900, 400));
        let targets = app.targets();
        let target = |id: &str| *targets.get(id).expect(id);
        for id in ["size", "device"] {
            assert!(
                (target(id).width() - cap).abs() < 0.5,
                "a long option caps the shared width at the chooser width: {id} {:?}",
                target(id)
            );
        }
    }

    #[test]
    fn dropdown_sizes_to_its_options_and_keeps_its_value_while_open() {
        fn hugs_its_widest_option_unless_it_fills() {
            use crate::widget::Row;
            let row = Row::new()
                .push(Box::new(Dropdown::new(opts(), "tz").with_id("hug")))
                .push(Box::new(
                    Dropdown::new(vec!["A".into(), "B".into()], "ab").with_id("tiny"),
                ));
            let app =
                crate::headless::FrameHarness::new(crate::State::new(), Box::new(row), (900, 200));
            let targets = app.targets();
            let hug = targets.get("hug").expect("hugging dropdown");
            let tiny = targets.get("tiny").expect("short dropdown");
            assert!(
                hug.width() < 400.0 && hug.width() > tiny.width(),
                "a dropdown is as wide as its options, not the row: {hug:?} / {tiny:?}"
            );
            let mut text = TextRenderer::new();
            let theme = Theme::dark();
            let widest = opts()
                .iter()
                .map(|o| f64::from(text.measure_style(o, None, &theme.text(TextRole::Label)).0))
                .fold(0.0, f64::max);
            assert!(
                hug.width() >= widest + PAD_X + CHEVRON_W,
                "the widest option fits beside the chevron: {hug:?} vs {widest}"
            );

            let filled = crate::widget::Row::new()
                .push(Box::new(Dropdown::new(opts(), "tz").with_id("fill").fill()));
            let app = crate::headless::FrameHarness::new(
                crate::State::new(),
                Box::new(filled),
                (900, 200),
            );
            let fill = *app.targets().get("fill").expect("filling dropdown");
            assert!(fill.width() > 600.0, "fill spans the row: {fill:?}");
        }

        fn a_short_list_jumps_instead_of_filtering() {
            let mut dd = Dropdown::new(opts(), "tz");
            let (mut doc, root) = doc_with_tz("UTC");
            dd.sync(&doc, &root);
            dd.set_focus(true);
            dd.do_open();
            assert_eq!(dd.highlight_index(), 0);
            assert!(feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            ));
            assert_eq!(dd.filter_query(), "", "a short list never filters");
            assert_eq!(dd.visible_count(), 4);
            assert_eq!(dd.highlight_index(), 2, "the next option starting with a");
            feed_key(
                &mut dd,
                &key_press(Key::Character("a".into()), Some("a")),
                &mut doc,
                &root,
            );
            assert_eq!(dd.highlight_index(), 3, "typing again moves on");
            let node = Widget::accessibility(&dd).expect("semantics");
            assert_eq!(
                node.value.as_deref(),
                Some("UTC"),
                "the value stays while the list is open"
            );
        }

        fn a_caption_placed_externally_never_covers_the_field() {
            let mut dd = Dropdown::new(opts(), "tz").caption("Time zone");
            let theme = Theme::dark();
            let face = DropdownFace::new(Rect::ZERO, "").caption("Time zone");
            let height = face.outer_height(&theme);
            dd.apply_layout_external(Rect::new(0.0, 0.0, 200.0, height));
            assert!(
                (dd.rect.height() - crate::widget::ControlSize::Regular.height(&theme)).abs() < 0.5,
                "the field keeps its height below the caption: {:?}",
                dd.rect
            );
            assert!(dd.rect.y0 > 0.0);
        }

        fn a_resolved_name_is_spoken_as_given() {
            let dd = Dropdown::new(opts(), "tz").with_resolved_accessible_name("Zeitzone");
            let node = Widget::accessibility(&dd).expect("semantics");
            assert_eq!(node.name.as_deref(), Some("Zeitzone"));
        }

        fn an_idle_dropdown_needs_no_layout() {
            let theme = Theme::dark();
            let mut dd = Dropdown::new(opts(), "tz");
            assert!(Widget::is_layout_dirty(&dd));
            let mut taffy = TaffyTree::new();
            let mut node_map = HashMap::new();
            let node = dd.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(300.0),
                        height: taffy::AvailableSpace::Definite(200.0),
                    },
                )
                .unwrap();
            dd.apply_layout(&taffy, node, Point::ZERO);
            assert!(!Widget::is_layout_dirty(&dd));
            dd.set_options(zones());
            assert!(
                Widget::is_layout_dirty(&dd),
                "new options may change the width"
            );
        }

        fn the_face_matches_the_retained_control() {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let face = DropdownFace::new(Rect::ZERO, "UTC").leading_icon(crate::icons::Icon::Globe);
            let width = face.intrinsic_width(opts().iter().map(String::as_str), &mut text, &theme);
            let mut dd = Dropdown::new(opts(), "tz").leading_icon(crate::icons::Icon::Globe);
            dd.adopt_theme(&theme);
            dd.set_selected("UTC");
            assert!((dd.measured_width(&mut text) - width).abs() < 0.5);
            assert!(
                (DropdownFace::height(crate::widget::ControlSize::Small, &theme)
                    - crate::widget::ControlSize::Small.height(&theme))
                .abs()
                    < 1e-9
            );
            let captioned =
                DropdownFace::new(Rect::new(0.0, 0.0, 120.0, 60.0), "UTC").caption("Zone");
            let field = captioned.field_rect(&theme);
            assert!(field.y0 > 0.0 && field.y1 == 60.0);
            assert!(captioned.contains(&theme, field.center()));
            assert!(!captioned.contains(&theme, Point::new(60.0, 1.0)));
            let mut scene = Scene::new();
            captioned.paint(&mut scene, Affine::IDENTITY, &mut text, &theme);
        }

        hugs_its_widest_option_unless_it_fills();
        a_short_list_jumps_instead_of_filtering();
        a_caption_placed_externally_never_covers_the_field();
        a_resolved_name_is_spoken_as_given();
        an_idle_dropdown_needs_no_layout();
        the_face_matches_the_retained_control();
    }
}

//! Accessibility semantic model for the qOS `ui` toolkit.
//!
//! This is the **data layer** of accessibility. It
//! defines the toolkit-ergonomic semantic types — [`Role`], [`AccessNode`],
//! [`AccessState`], and [`Action`] — that every
//! [`Widget`](crate::widget::Widget) describes itself with via
//! [`Widget::accessibility`](crate::widget::Widget::accessibility).
//!
//! These types are deliberately a **thin convenience layer**. The export
//! layer maps them onto AccessKit's far richer schema (182 roles,
//! 22 actions) and exports the assembled tree over AT-SPI2 so standard ATs
//! (Orca, braille) work, while an in-process channel feeds the same nodes to
//! the built-in screen reader. Keeping our surface small means a widget
//! author implements semantics once with a handful of variants, yet the
//! export layer still emits a fully-detailed AT tree.
//!
//! Everything here is **pure data, no behaviour** — no D-Bus, no AccessKit
//! dependency yet. It is intentionally *AccessKit-compatible in spirit*: each
//! [`Role`]/[`Action`]/[`AccessState`] field maps cleanly onto an AccessKit
//! concept (e.g. our [`Role::Switch`] → `accesskit::Role::Switch`, our
//! [`AccessState::multiline`] + [`Role::TextInput`] →
//! `accesskit::Role::MultilineTextInput`).

use vello::peniko::Color;

use crate::color_compat::ColorCompat;

/// Semantic role of an accessible element. A thin subset of AccessKit's 182
/// roles covering the widgets the toolkit actually ships, plus the structural
/// containers needed for landmark/heading navigation. Map liberally onto
/// AccessKit when exporting: unknown/rare cases fall back to
/// [`Role::Generic`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    // --- Structural / containers ---
    /// Top-level window / surface.
    Window,
    /// Modal or non-modal dialog.
    Dialog,
    /// A semantic grouping (e.g. a labelled form section).
    Group,
    /// A non-semantic grouping that carries no meaning of its own. Prefer
    /// returning `None` from `accessibility()` for pure layout widgets so they
    /// collapse out of the tree entirely; `Generic` is for the rare container
    /// that must appear but has no richer role.
    Generic,
    /// A toolbar of controls.
    Toolbar,
    /// The tab strip of a tabbed surface.
    TabBar,
    /// A single tab within a [`Role::TabBar`].
    Tab,
    /// The panel a selected [`Role::Tab`] reveals.
    TabPanel,
    /// A list container.
    List,
    /// A single item within a [`Role::List`].
    ListItem,
    /// A data table.
    Table,
    /// A row within a [`Role::Table`] or [`Role::Tree`].
    Row,
    /// A cell within a [`Role::Row`].
    Cell,
    /// A tree / outline container.
    Tree,
    /// A node within a [`Role::Tree`] (use [`AccessState::expanded`]).
    TreeItem,
    /// A menu container.
    Menu,
    /// A single item within a [`Role::Menu`].
    MenuItem,

    // --- Controls ---
    /// A push button.
    Button,
    /// A button with a persistent pressed/unpressed state
    /// (use [`AccessState::checked`]).
    ToggleButton,
    /// A checkbox (use [`AccessState::checked`]).
    Checkbox,
    /// A radio button within a group (use [`AccessState::checked`]).
    RadioButton,
    /// An on/off switch / toggle (use [`AccessState::checked`]).
    Switch,
    /// A value slider over a continuous range.
    Slider,
    /// A spin button (numeric stepper).
    SpinButton,
    /// A combo box / dropdown selector (use [`AccessState::expanded`]).
    ComboBox,
    /// A single-line editable text field.
    TextInput,
    /// A multi-line editable text field. (Equivalent to a [`Role::TextInput`]
    /// with [`AccessState::multiline`] set; provided as its own role because
    /// AccessKit distinguishes them.)
    MultilineTextInput,
    /// A hyperlink.
    Link,
    /// A progress indicator (determinate or, with [`AccessState::busy`],
    /// indeterminate).
    ProgressBar,
    /// A busy / loading spinner.
    Spinner,
    /// A tooltip.
    Tooltip,

    // --- Content ---
    /// A (possibly decorative) image; supply an alt text via `name`.
    Image,
    /// A short text label that names another control.
    Label,
    /// Non-editable, non-interactive text.
    StaticText,
    /// A section heading at the given outline `level` (1 = top level), used
    /// for heading navigation. Mirrors `accesskit::Role::Heading` plus the
    /// AT-SPI `level` attribute.
    Heading {
        /// Heading depth, 1-based (`<h1>` == `level: 1`).
        level: u8,
    },
    /// A paragraph of running text.
    Paragraph,
    /// A document root (e.g. the block editor).
    Document,
    /// An arbitrary drawing canvas.
    Canvas,
    /// Role could not be determined. Maps to `accesskit::Role::Unknown`.
    Unknown,
}

/// Boolean / tri-state flags describing the live state of an accessible
/// element. Tri-state fields use `Option<bool>` where `None` means "the state
/// does not apply" (e.g. a [`Role::Button`] is never `checked`) — this matches
/// ARIA's `aria-checked="false"` vs. absent distinction and AccessKit's
/// `Toggled`/`Expanded` optionality.
///
/// State is **derived, never stored**: a widget computes it from its existing
/// fields (`is_focused()`, `is_enabled()`, selection indices, …) at
/// `accessibility()` time, so it can never drift from what `draw` paints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessState {
    /// The element currently holds keyboard focus.
    pub focused: bool,
    /// The element can receive keyboard focus.
    pub focusable: bool,
    /// The element is selected within a multi-selectable container.
    pub selected: bool,
    /// Tri-state checked: `None` = not checkable, `Some(true|false)` =
    /// checkable and currently on/off. (A future "mixed" tri-state can be
    /// represented by a dedicated [`AccessState::mixed`] flag.)
    pub checked: Option<bool>,
    /// "Partially checked" / indeterminate, for tri-state checkboxes.
    pub mixed: bool,
    /// Tri-state expanded: `None` = not expandable, `Some(true|false)` =
    /// expandable and currently open/closed.
    pub expanded: Option<bool>,
    /// The element is disabled / inert (not actionable).
    pub disabled: bool,
    /// The element's value cannot be edited by the user.
    pub read_only: bool,
    /// The element must be filled in / acted on before submission.
    pub required: bool,
    /// The element's current value fails validation.
    pub invalid: bool,
    /// The element (or its subtree) is busy / loading.
    pub busy: bool,
    /// The element is a modal that traps focus.
    pub modal: bool,
    /// The element is hidden from view (and should be from the tree).
    pub hidden: bool,
    /// A text field that accepts multiple lines.
    pub multiline: bool,
    /// A sensitive value (password) whose contents the AT must mask. When set,
    /// widgets must NOT place the secret in [`AccessNode::value`].
    pub protected: bool,
}

impl AccessState {
    /// A blank state with every flag cleared (same as [`Default`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// Set [`focused`](Self::focused).
    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
    /// Set [`focusable`](Self::focusable).
    pub fn focusable(mut self, v: bool) -> Self {
        self.focusable = v;
        self
    }
    /// Set [`selected`](Self::selected).
    pub fn selected(mut self, v: bool) -> Self {
        self.selected = v;
        self
    }
    /// Set [`checked`](Self::checked) to `Some(v)`.
    pub fn checked(mut self, v: bool) -> Self {
        self.checked = Some(v);
        self
    }
    /// Set [`expanded`](Self::expanded) to `Some(v)`.
    pub fn expanded(mut self, v: bool) -> Self {
        self.expanded = Some(v);
        self
    }
    /// Set [`disabled`](Self::disabled).
    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }
    /// Set [`read_only`](Self::read_only).
    pub fn read_only(mut self, v: bool) -> Self {
        self.read_only = v;
        self
    }
    /// Set [`multiline`](Self::multiline).
    pub fn multiline(mut self, v: bool) -> Self {
        self.multiline = v;
        self
    }
    /// Set [`protected`](Self::protected).
    pub fn protected(mut self, v: bool) -> Self {
        self.protected = v;
        self
    }
    /// Set [`modal`](Self::modal).
    pub fn modal(mut self, v: bool) -> Self {
        self.modal = v;
        self
    }
    /// Set [`busy`](Self::busy).
    pub fn busy(mut self, v: bool) -> Self {
        self.busy = v;
        self
    }
}

/// An action that can be requested on an accessible element by an assistive
/// technology or the built-in screen reader. Consumed by the future
/// `Widget::perform_action` hook, which routes it through the same
/// CRDT-mutating path as `handle_event`. Maps onto `accesskit::Action` and
/// the AT-SPI action interface.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Default activation — a button press, a link follow, a menu-item
    /// invoke. (ARIA/AT-SPI "click".)
    Activate,
    /// Move keyboard focus to the element.
    Focus,
    /// Toggle a checkbox / switch / toggle button.
    ToggleCheck,
    /// Step a range value up (slider, spin button).
    Increment,
    /// Step a range value down (slider, spin button).
    Decrement,
    /// Expand a collapsible (tree item, combo box, disclosure).
    Expand,
    /// Collapse a collapsible.
    Collapse,
    /// Replace the element's value (text field, spin button).
    SetValue(String),
    /// Set a text selection by UTF-8 byte offsets into the value.
    SetSelection {
        /// Selection anchor (where the selection started).
        anchor: usize,
        /// Selection focus / caret (where it ends).
        focus: usize,
    },
    /// Scroll the element into the visible viewport.
    ScrollIntoView,
    /// Open the element's context menu.
    ShowContextMenu,
}

/// How urgently a [live region](AccessNode::live) announces a change.
///
/// A live region narrates its own updates *without moving focus* — a toast
/// appearing, a status line changing, a progress step. The marker tells the
/// reader whether an update may wait for a pause in speech or must cut in
/// immediately. Maps onto ARIA `aria-live="polite"`/`"assertive"` and
/// AccessKit's live politeness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Politeness {
    /// Wait for a natural pause before speaking — the default for unobtrusive
    /// status updates that shouldn't interrupt what the user is doing.
    Polite,
    /// Speak at once, interrupting any in-flight narration — for time-critical
    /// messages (errors, the toast a user just triggered).
    Assertive,
}

/// The per-widget *local* accessibility semantics returned by
/// [`Widget::accessibility`](crate::widget::Widget::accessibility).
///
/// Geometry comes from [`Widget::get_rect`](crate::widget::Widget::get_rect)
/// and the subtree from [`Widget::children`](crate::widget::Widget::children);
/// the `App` assembles the global tree from these. `name`/`description`
/// must already be **FTL-resolved** strings (via [`i18n!`](crate::i18n)), not
/// translation keys, because AccessKit/AT-SPI consume final text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessNode {
    /// The element's semantic role.
    pub role: Role,
    /// Accessible name (the primary label a screen reader announces).
    pub name: Option<String>,
    /// Accessible description (supplementary detail beyond the name).
    pub description: Option<String>,
    /// The element's current value as text (a text field's contents, a
    /// slider's formatted value). Never populated for
    /// [`protected`](AccessState::protected) elements.
    pub value: Option<String>,
    /// Live boolean/tri-state flags.
    pub state: AccessState,
    /// When set, this node is a **live region**: the `App` watches its spoken
    /// text each frame and announces changes at the given politeness *without*
    /// moving focus. `None` (the default) means the node is not a live region.
    pub live: Option<Politeness>,
    /// Total number of items in the owning collection (a tab strip's tab
    /// count, a list's length). Pairs with [`pos_in_set`](Self::pos_in_set) to
    /// let the reader say "2 of 4". `None` when the node isn't a positioned
    /// collection item.
    pub set_size: Option<usize>,
    /// This item's 1-based position within its collection. See
    /// [`set_size`](Self::set_size).
    pub pos_in_set: Option<usize>,
    /// Hierarchy depth, 1-based (a top-level tree row is `1`). Read out as
    /// "level N" for nested structures. `None` for flat elements.
    pub level: Option<usize>,
    /// Stable identity of THIS node within its parent's
    /// [`children`](Self::children) list, for nodes that are **synthetic** (no
    /// backing [`Widget`](crate::widget::Widget)).
    ///
    /// The export layer derives a synthetic node's assistive-technology id by
    /// chaining these keys from the owning widget downwards, so an AT that
    /// holds a reference to "the clip named `intro`" keeps pointing at that
    /// clip across repaints — and the same key path is what comes back on
    /// [`Widget::perform_action_at`](crate::widget::Widget::perform_action_at)
    /// when the AT operates the node.
    ///
    /// `None` (the default) means "key me by my ordinal position among my
    /// siblings". That is the right choice for **position-addressed**
    /// structures — a table's rows and cells, where "row 3" is the identity the
    /// user means even after a re-sort. Set an explicit key for
    /// **identity-addressed** structures — a clip on a timeline, a layer, a
    /// note — whose position moves but which stay the same object; use
    /// [`with_hashed_key`](Self::with_hashed_key) to derive one from a string
    /// id.
    ///
    /// Do not mix keyed and unkeyed siblings in one list: an explicit key of
    /// `3` and an ordinal of `3` are indistinguishable downstream.
    pub key: Option<u64>,
    /// Synthetic accessible children that have no backing
    /// [`Widget`](crate::widget::Widget) of their own. A widget that owns a
    /// structure too fine-grained to model as separate widgets — a table's
    /// rows and cells, a virtualized list's items — describes that structure
    /// here. The export layer emits these as the node's children, nested as
    /// deeply as needed (rows containing cells). Empty (the default) for the
    /// common case where the subtree comes entirely from real child widgets.
    pub children: Vec<AccessNode>,
}

impl AccessNode {
    /// A bare node with the given `role` and no name/description/value and a
    /// default (all-clear) [`AccessState`].
    pub fn new(role: Role) -> Self {
        Self {
            role,
            name: None,
            description: None,
            value: None,
            state: AccessState::default(),
            live: None,
            set_size: None,
            pos_in_set: None,
            level: None,
            key: None,
            children: Vec::new(),
        }
    }

    /// Set the accessible [`name`](Self::name).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the accessible name from an already-`Option`-wrapped string (handy
    /// when an empty label should collapse to `None`).
    pub fn with_optional_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// Set the [`description`](Self::description).
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the [`value`](Self::value).
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Set the [`value`](Self::value) from an `Option` (e.g. `None` for an
    /// empty or protected field).
    pub fn with_optional_value(mut self, value: Option<String>) -> Self {
        self.value = value;
        self
    }

    /// Set the [`state`](Self::state).
    pub fn with_state(mut self, state: AccessState) -> Self {
        self.state = state;
        self
    }

    /// Mark this node a [live region](Self::live) at the given politeness, so
    /// the reader announces its changes without moving focus.
    pub fn with_live(mut self, politeness: Politeness) -> Self {
        self.live = Some(politeness);
        self
    }

    /// Set this item's position within its collection: 1-based
    /// [`pos_in_set`](Self::pos_in_set) within a total of `set_size`. Read out
    /// as "N of M".
    pub fn with_position(mut self, pos_in_set: usize, set_size: usize) -> Self {
        self.pos_in_set = Some(pos_in_set);
        self.set_size = Some(set_size);
        self
    }

    /// Set the hierarchy [`level`](Self::level) (1-based depth), read out as
    /// "level N" for nested structures like trees.
    pub fn with_level(mut self, level: usize) -> Self {
        self.level = Some(level);
        self
    }

    /// Set this node's stable [`key`](Self::key) within its sibling list.
    ///
    /// Use this for a synthetic node whose *identity* outlives its *position* —
    /// a clip that can be dragged along a track, a layer that can be
    /// reordered. The key must be unique among the node's siblings and stable
    /// for as long as the element it describes exists.
    pub fn with_key(mut self, key: u64) -> Self {
        self.key = Some(key);
        self
    }

    /// Set this node's stable [`key`](Self::key) by hashing any hashable value
    /// — the ergonomic form of [`with_key`](Self::with_key) for elements
    /// identified by a string (`clip.id`), a tuple (`(track, clip)`), or a
    /// UUID.
    ///
    /// ```
    /// # use ui::a11y::{AccessNode, Role};
    /// let clip = AccessNode::new(Role::ListItem).with_hashed_key("clip-intro");
    /// assert_eq!(clip.key, Some(ui::a11y::hash_key(&"clip-intro")));
    /// ```
    pub fn with_hashed_key(mut self, key: impl std::hash::Hash) -> Self {
        self.key = Some(hash_key(&key));
        self
    }

    /// Attach synthetic [`children`](Self::children) — accessible elements
    /// with no backing widget (a table's rows/cells). The export layer
    /// emits them as this node's subtree.
    pub fn with_children(mut self, children: Vec<AccessNode>) -> Self {
        self.children = children;
        self
    }
}

/// Hash any hashable value into the `u64` key space used by
/// [`AccessNode::key`].
///
/// This is the *inverse-less* half of the key contract: a widget hashes its
/// element ids on the way out, and compares an incoming key path element back
/// against `hash_key(&id)` on the way in (see
/// [`Widget::perform_action_at`](crate::widget::Widget::perform_action_at)).
/// The hash is a plain FNV-1a with a fixed basis, so it is deterministic within
/// and across processes — unlike [`std::collections::hash_map::RandomState`],
/// which would hand an assistive technology a different id every launch.
pub fn hash_key(value: &impl std::hash::Hash) -> u64 {
    let mut h = FnvHasher::new();
    std::hash::Hash::hash(value, &mut h);
    std::hash::Hasher::finish(&h)
}

/// FNV-1a, 64-bit. Small, dependency-free and — the part that matters here —
/// **fixed-seed**, so an id derived from it is reproducible.
#[derive(Clone, Copy)]
pub(crate) struct FnvHasher(u64);

impl FnvHasher {
    pub(crate) const fn new() -> Self {
        // FNV offset basis.
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            // FNV prime.
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// User-facing accessibility preferences. Stored in the per-user settings CRDT
/// and mirrored into spawned sandboxes via `QOS_A11Y_*` env vars, so every
/// `ui` app can read a consistent set. The shell owns theming and broadcasts
/// changes; apps consult these to pick a [`Theme`](crate::theme::Theme) preset
/// and to gate motion.
///
/// Defaults are all-off so existing single-user setups are unaffected until a
/// user opts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct A11ySettings {
    /// Use a high-contrast theme track (see [`Theme::high_contrast`]).
    ///
    /// [`Theme::high_contrast`]: crate::theme::Theme::high_contrast
    pub high_contrast: bool,
    /// Prefer reduced / no motion: animations jump to their end state instead
    /// of easing. Consulted by `ui/src/animation` and the magnifier pan.
    pub reduce_motion: bool,
    /// A screen reader is active. Apps/shell can use this to enable extra
    /// narration hooks and keep idle frames flowing for live regions.
    pub screen_reader: bool,
}

impl Default for A11ySettings {
    fn default() -> Self {
        Self {
            high_contrast: false,
            reduce_motion: false,
            screen_reader: false,
        }
    }
}

/// WCAG 2.1 relative luminance of an sRGB colour, in `[0.0, 1.0]`. Alpha is
/// ignored (colours are assumed opaque for contrast purposes). Used by the
/// high-contrast theme tests and, later, by an automated contrast-ratio audit
/// over theme token pairs.
pub fn relative_luminance(c: Color) -> f64 {
    fn channel(v: u8) -> f64 {
        let s = v as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG 2.1 contrast ratio between two colours, in `[1.0, 21.0]` (1.0 = same
/// luminance, 21.0 = black on white). WCAG AA body text wants ≥ 4.5:1; AAA
/// wants ≥ 7:1.
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_state_default_is_all_clear() {
        let s = AccessState::default();
        assert!(!s.focused);
        assert!(!s.focusable);
        assert!(!s.selected);
        assert_eq!(s.checked, None);
        assert_eq!(s.expanded, None);
        assert!(!s.disabled);
        assert!(!s.read_only);
        assert!(!s.protected);
        assert!(!s.multiline);
        assert_eq!(s, AccessState::new());
    }

    #[test]
    fn access_state_builders_set_the_right_fields() {
        let s = AccessState::new()
            .focusable(true)
            .focused(true)
            .checked(true)
            .disabled(true);
        assert!(s.focusable);
        assert!(s.focused);
        assert_eq!(s.checked, Some(true));
        assert!(s.disabled);
        // Builders are independent — untouched fields stay default.
        assert_eq!(s.expanded, None);
        assert!(!s.read_only);

        let unchecked = AccessState::new().checked(false);
        assert_eq!(unchecked.checked, Some(false), "checked(false) is Some(false), not None");

        let collapsed = AccessState::new().expanded(false);
        assert_eq!(collapsed.expanded, Some(false));
    }

    #[test]
    fn access_node_new_has_no_text_and_default_state() {
        let n = AccessNode::new(Role::Button);
        assert_eq!(n.role, Role::Button);
        assert_eq!(n.name, None);
        assert_eq!(n.description, None);
        assert_eq!(n.value, None);
        assert_eq!(n.state, AccessState::default());
    }

    #[test]
    fn access_node_builders_compose() {
        let n = AccessNode::new(Role::Slider)
            .with_name("Volume")
            .with_description("Output volume")
            .with_value("50%")
            .with_state(AccessState::new().focusable(true).focused(true));
        assert_eq!(n.role, Role::Slider);
        assert_eq!(n.name.as_deref(), Some("Volume"));
        assert_eq!(n.description.as_deref(), Some("Output volume"));
        assert_eq!(n.value.as_deref(), Some("50%"));
        assert!(n.state.focusable && n.state.focused);
    }

    #[test]
    fn access_node_live_and_position_builders() {
        let bare = AccessNode::new(Role::Group);
        assert_eq!(bare.live, None);
        assert_eq!(bare.set_size, None);
        assert_eq!(bare.pos_in_set, None);
        assert_eq!(bare.level, None);

        let live = AccessNode::new(Role::Group).with_live(Politeness::Assertive);
        assert_eq!(live.live, Some(Politeness::Assertive));

        let item = AccessNode::new(Role::Tab).with_position(2, 4);
        assert_eq!(item.pos_in_set, Some(2));
        assert_eq!(item.set_size, Some(4));

        let row = AccessNode::new(Role::TreeItem).with_level(3);
        assert_eq!(row.level, Some(3));
    }

    #[test]
    fn access_node_optional_helpers() {
        let named = AccessNode::new(Role::StaticText).with_optional_name(Some("Hi".into()));
        assert_eq!(named.name.as_deref(), Some("Hi"));
        let anon = AccessNode::new(Role::Image).with_optional_name(None);
        assert_eq!(anon.name, None);
        let masked = AccessNode::new(Role::TextInput).with_optional_value(None);
        assert_eq!(masked.value, None);
    }

    #[test]
    fn heading_role_carries_level() {
        let h2 = Role::Heading { level: 2 };
        assert_eq!(h2, Role::Heading { level: 2 });
        assert_ne!(h2, Role::Heading { level: 3 });
    }

    #[test]
    fn action_equality_and_payloads() {
        assert_eq!(Action::Activate, Action::Activate);
        assert_ne!(Action::Increment, Action::Decrement);
        assert_eq!(Action::SetValue("a".into()), Action::SetValue("a".into()));
        assert_ne!(Action::SetValue("a".into()), Action::SetValue("b".into()));
        assert_eq!(
            Action::SetSelection { anchor: 0, focus: 3 },
            Action::SetSelection { anchor: 0, focus: 3 },
        );
        assert_ne!(
            Action::SetSelection { anchor: 0, focus: 3 },
            Action::SetSelection { anchor: 1, focus: 3 },
        );
    }

    #[test]
    fn a11y_settings_default_is_all_off() {
        let s = A11ySettings::default();
        assert!(!s.high_contrast);
        assert!(!s.reduce_motion);
        assert!(!s.screen_reader);
    }

    #[test]
    fn a11y_settings_roundtrips_through_json() {
        let s = A11ySettings {
            high_contrast: true,
            reduce_motion: false,
            screen_reader: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: A11ySettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn contrast_ratio_extremes() {
        let white = Color::from_rgb8(255, 255, 255);
        let black = Color::from_rgb8(0, 0, 0);
        // Black-on-white is the WCAG maximum, 21:1.
        let r = contrast_ratio(white, black);
        assert!((r - 21.0).abs() < 0.01, "black/white contrast should be ~21:1, got {r}");
        // Symmetric.
        assert!((contrast_ratio(black, white) - r).abs() < 1e-9);
        // A colour against itself is 1:1.
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn relative_luminance_bounds() {
        let white = relative_luminance(Color::from_rgb8(255, 255, 255));
        let black = relative_luminance(Color::from_rgb8(0, 0, 0));
        assert!((white - 1.0).abs() < 1e-6);
        assert!(black.abs() < 1e-6);
        // Green contributes more luminance than red or blue (WCAG weights).
        let red = relative_luminance(Color::from_rgb8(255, 0, 0));
        let green = relative_luminance(Color::from_rgb8(0, 255, 0));
        let blue = relative_luminance(Color::from_rgb8(0, 0, 255));
        assert!(green > red && red > blue);
    }
}

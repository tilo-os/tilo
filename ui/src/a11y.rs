//! Accessibility semantic model: the **data layer**. Defines the
//! toolkit-ergonomic semantic types — [`Role`], [`AccessNode`],
//! [`AccessState`], and [`Action`] — that every
//! [`Widget`](crate::widget::Widget) describes itself with via
//! [`Widget::accessibility`](crate::widget::Widget::accessibility).
//!
//! Deliberately a **thin convenience layer**: the export layer maps these
//! onto AccessKit's far richer schema (182 roles, 22 actions) and exports
//! the assembled tree over AT-SPI2, while an in-process channel feeds the
//! same nodes to the built-in screen reader. A widget author implements
//! semantics once with a handful of variants.
//!
//! Everything here is **pure data, no behaviour** — no D-Bus, no AccessKit
//! dependency. Each field maps cleanly onto an AccessKit concept (e.g. our
//! [`AccessState::multiline`] + [`Role::TextInput`] →
//! `accesskit::Role::MultilineTextInput`).

use vello::peniko::Color;

use crate::color_compat::ColorCompat;

/// A thin subset of AccessKit's 182 roles covering the widgets the toolkit
/// actually ships; unknown/rare cases fall back to [`Role::Generic`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Window,
    Dialog,
    Group,
    /// Prefer returning `None` from `accessibility()` for pure layout widgets
    /// so they collapse out of the tree; `Generic` is for a rare container
    /// with no richer role that must still appear.
    Generic,
    Toolbar,
    TabBar,
    Tab,
    /// The panel a selected [`Role::Tab`] reveals.
    TabPanel,
    List,
    ListItem,
    Table,
    /// Within a [`Role::Table`] or [`Role::Tree`].
    Row,
    Cell,
    Tree,
    /// Use [`AccessState::expanded`].
    TreeItem,
    Menu,
    MenuItem,

    Button,
    /// Persistent pressed/unpressed state (use [`AccessState::checked`]).
    ToggleButton,
    /// Use [`AccessState::checked`].
    Checkbox,
    /// Use [`AccessState::checked`].
    RadioButton,
    /// Use [`AccessState::checked`].
    Switch,
    Slider,
    SpinButton,
    /// Use [`AccessState::expanded`].
    ComboBox,
    TextInput,
    /// Own role because AccessKit distinguishes it from [`Role::TextInput`].
    MultilineTextInput,
    Link,
    /// Determinate, or with [`AccessState::busy`], indeterminate.
    ProgressBar,
    Spinner,
    Tooltip,

    /// Supply alt text via `name`.
    Image,
    /// A short text label that names another control.
    Label,
    StaticText,
    /// Outline `level` (1 = top level), for heading navigation.
    Heading {
        level: u8,
    },
    Paragraph,
    /// E.g. the block editor.
    Document,
    Canvas,
    Unknown,
}

/// Tri-state fields use `Option<bool>` where `None` means "the state does
/// not apply" (e.g. a [`Role::Button`] is never `checked`) — matching ARIA's
/// `aria-checked="false"` vs. absent distinction.
///
/// State is **derived, never stored**: a widget computes it from its
/// existing fields at `accessibility()` time, so it can never drift from
/// what `draw` paints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field mirrors one independent ARIA state attribute; they're set \
              by name through builder methods below, never positionally, so there's \
              no risk of two flags being swapped by accident"
)]
pub struct AccessState {
    pub focused: bool,
    pub focusable: bool,
    pub selected: bool,
    /// `None` = not checkable; tri-state goes through [`AccessState::mixed`], not a third value here.
    pub checked: Option<bool>,
    /// "Partially checked" / indeterminate, for tri-state checkboxes.
    pub mixed: bool,
    /// `None` = not expandable.
    pub expanded: Option<bool>,
    pub disabled: bool,
    pub read_only: bool,
    pub required: bool,
    pub invalid: bool,
    /// The element or its subtree.
    pub busy: bool,
    /// Traps focus.
    pub modal: bool,
    /// Hidden from view, and should be from the tree.
    pub hidden: bool,
    pub multiline: bool,
    /// When set, widgets must NOT place the secret in [`AccessNode::value`].
    pub protected: bool,
}

impl AccessState {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
    #[must_use]
    pub fn focusable(mut self, v: bool) -> Self {
        self.focusable = v;
        self
    }
    #[must_use]
    pub fn selected(mut self, v: bool) -> Self {
        self.selected = v;
        self
    }
    /// Sets [`checked`](Self::checked) to `Some(v)`.
    #[must_use]
    pub fn checked(mut self, v: bool) -> Self {
        self.checked = Some(v);
        self
    }
    /// Sets [`expanded`](Self::expanded) to `Some(v)`.
    #[must_use]
    pub fn expanded(mut self, v: bool) -> Self {
        self.expanded = Some(v);
        self
    }
    #[must_use]
    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }
    #[must_use]
    pub fn read_only(mut self, v: bool) -> Self {
        self.read_only = v;
        self
    }
    #[must_use]
    pub fn multiline(mut self, v: bool) -> Self {
        self.multiline = v;
        self
    }
    #[must_use]
    pub fn protected(mut self, v: bool) -> Self {
        self.protected = v;
        self
    }
    #[must_use]
    pub fn modal(mut self, v: bool) -> Self {
        self.modal = v;
        self
    }
    #[must_use]
    pub fn busy(mut self, v: bool) -> Self {
        self.busy = v;
        self
    }
}

/// Requested by an assistive technology or the built-in screen reader.
/// Consumed by the future `Widget::perform_action` hook, routed through the
/// same CRDT-mutating path as `handle_event`.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A button press, a link follow, a menu-item invoke (ARIA/AT-SPI "click").
    Activate,
    Focus,
    ToggleCheck,
    Increment,
    Decrement,
    /// Tree item, combo box, disclosure.
    Expand,
    Collapse,
    /// Text field, spin button.
    SetValue(String),
    /// Extended-grapheme indices into the accessible `TextRun`, matching
    /// AccessKit's selectable-character model rather than Unicode scalars.
    SetSelection {
        /// Where the selection started.
        anchor: usize,
        /// Where it ends.
        focus: usize,
    },
    ScrollIntoView,
    ShowContextMenu,
}

/// How urgently a [live region](AccessNode::live) narrates its own update
/// *without moving focus* — a toast appearing, a status line changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Politeness {
    /// Wait for a natural pause before speaking.
    Polite,
    /// Speak at once, interrupting any in-flight narration.
    Assertive,
}

/// The per-widget *local* accessibility semantics returned by
/// [`Widget::accessibility`](crate::widget::Widget::accessibility);
/// `name`/`description` must already be **FTL-resolved** strings (via
/// [`i18n!`](crate::i18n)), not translation keys, since AccessKit/AT-SPI
/// consume final text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessNode {
    pub role: Role,
    /// The primary label a screen reader announces.
    pub name: Option<String>,
    pub description: Option<String>,
    /// Never populated for [`protected`](AccessState::protected) elements.
    pub value: Option<String>,
    /// Direction-preserving UTF-8 byte endpoints for editable text. The
    /// AccessKit bridge converts these to grapheme indices in a `TextRun`.
    pub text_selection: Option<AccessibleTextSelection>,
    pub state: AccessState,
    /// The `App` watches its spoken text each frame and announces changes at
    /// the given politeness *without* moving focus. `None` = not a live region.
    pub live: Option<Politeness>,
    /// Total items in the owning collection, pairs with
    /// [`pos_in_set`](Self::pos_in_set) to let the reader say "2 of 4".
    pub set_size: Option<usize>,
    /// 1-based position within the collection. See [`set_size`](Self::set_size).
    pub pos_in_set: Option<usize>,
    /// 1-based hierarchy depth, read out as "level N".
    pub level: Option<usize>,
    /// Accessible children with no backing [`Widget`](crate::widget::Widget)
    /// of their own — a table's rows and cells, a virtualized list's items.
    pub children: Vec<AccessNode>,
}

impl AccessNode {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            name: None,
            description: None,
            value: None,
            text_selection: None,
            state: AccessState::default(),
            live: None,
            set_size: None,
            pos_in_set: None,
            level: None,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Handy when an empty label should collapse to `None`.
    #[must_use]
    pub fn with_optional_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    #[must_use]
    pub fn with_optional_value(mut self, value: Option<String>) -> Self {
        self.value = value;
        self
    }

    /// Publish an editable caret (`anchor == focus`) or selection. Endpoints
    /// are UTF-8 bytes and are clamped to extended grapheme boundaries by the
    /// bridge, matching the toolkit editing contract.
    #[must_use]
    pub fn with_text_selection(mut self, anchor: usize, focus: usize) -> Self {
        self.text_selection = Some(AccessibleTextSelection { anchor, focus });
        self
    }

    #[must_use]
    pub fn with_state(mut self, state: AccessState) -> Self {
        self.state = state;
        self
    }

    #[must_use]
    pub fn with_live(mut self, politeness: Politeness) -> Self {
        self.live = Some(politeness);
        self
    }

    /// `pos_in_set` is 1-based within a total of `set_size`. Read out as "N of M".
    #[must_use]
    pub fn with_position(mut self, pos_in_set: usize, set_size: usize) -> Self {
        self.pos_in_set = Some(pos_in_set);
        self.set_size = Some(set_size);
        self
    }

    #[must_use]
    pub fn with_level(mut self, level: usize) -> Self {
        self.level = Some(level);
        self
    }

    #[must_use]
    pub fn with_children(mut self, children: Vec<AccessNode>) -> Self {
        self.children = children;
        self
    }
}

/// Editable-text selection in the widget's native UTF-8 document space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibleTextSelection {
    pub anchor: usize,
    pub focus: usize,
}

/// Stored in the per-user settings CRDT and mirrored into spawned sandboxes
/// via `QOS_A11Y_*` env vars. Defaults are all-off so existing single-user
/// setups are unaffected until a user opts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct A11ySettings {
    /// Use a high-contrast theme track (see [`Theme::high_contrast`]).
    ///
    /// [`Theme::high_contrast`]: crate::theme::Theme::high_contrast
    pub high_contrast: bool,
    /// Animations jump to their end state instead of easing. Consulted by
    /// `ui/src/animation` and the magnifier pan.
    pub reduce_motion: bool,
    /// Lets apps/shell enable extra narration hooks and keep idle frames
    /// flowing for live regions.
    pub screen_reader: bool,
}

/// WCAG 2.1 relative luminance of an sRGB colour, in `[0.0, 1.0]`. Alpha is
/// ignored (colours are assumed opaque for contrast purposes).
pub fn relative_luminance(c: Color) -> f64 {
    fn channel(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
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
        assert_eq!(
            unchecked.checked,
            Some(false),
            "checked(false) is Some(false), not None"
        );

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
            Action::SetSelection {
                anchor: 0,
                focus: 3
            },
            Action::SetSelection {
                anchor: 0,
                focus: 3
            },
        );
        assert_ne!(
            Action::SetSelection {
                anchor: 0,
                focus: 3
            },
            Action::SetSelection {
                anchor: 1,
                focus: 3
            },
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
        assert!(
            (r - 21.0).abs() < 0.01,
            "black/white contrast should be ~21:1, got {r}"
        );
        assert!((contrast_ratio(black, white) - r).abs() < 1e-9);
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

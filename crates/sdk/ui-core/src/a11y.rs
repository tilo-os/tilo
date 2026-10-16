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

use vello::kurbo::Rect;

/// A thin subset of AccessKit's 182 roles covering the widgets the toolkit
/// supports; unknown/rare cases fall back to [`Role::Generic`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
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
    /// A focusable divider between two panes that moves with the arrow
    /// keys; its value is the first pane's share in percent.
    Separator,
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
    /// A terminal transcript and input surface. Unlike a text input, its
    /// exposed value is not replaced by [`Action::SetValue`]; input is a
    /// stream owned by the terminal's resource/action contract.
    Terminal,
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
#[must_use]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this mirrors independent ARIA/AccessKit state flags; combining them into an enum would prevent valid simultaneous states"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct AccessState {
    pub focused: bool,
    pub focusable: bool,
    /// The control owns a standard context menu.  The bridge exposes
    /// AccessKit/AT-SPI's `ShowContextMenu` verb only when this is true, so
    /// canvas-style apps can make a menu discoverable without claiming that
    /// every focusable list row has one.
    pub context_menu: bool,
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

    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
    pub fn focusable(mut self, v: bool) -> Self {
        self.focusable = v;
        self
    }
    /// Advertise a standard context-menu action for this control.
    pub fn context_menu(mut self, v: bool) -> Self {
        self.context_menu = v;
        self
    }
    pub fn selected(mut self, v: bool) -> Self {
        self.selected = v;
        self
    }
    /// Sets [`checked`](Self::checked) to `Some(v)`.
    pub fn checked(mut self, v: bool) -> Self {
        self.checked = Some(v);
        self
    }
    /// Sets [`expanded`](Self::expanded) to `Some(v)`.
    pub fn expanded(mut self, v: bool) -> Self {
        self.expanded = Some(v);
        self
    }
    pub fn disabled(mut self, v: bool) -> Self {
        self.disabled = v;
        self
    }
    pub fn read_only(mut self, v: bool) -> Self {
        self.read_only = v;
        self
    }
    pub fn multiline(mut self, v: bool) -> Self {
        self.multiline = v;
        self
    }
    pub fn protected(mut self, v: bool) -> Self {
        self.protected = v;
        self
    }
    pub fn modal(mut self, v: bool) -> Self {
        self.modal = v;
        self
    }
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

/// A change to the person's accessibility preferences that a control asked
/// for, such as the sign-in screen's larger-text or high-contrast toggle.
/// The host applies it to the whole window, exactly as the matching system
/// preference would.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AccessibilityRequest {
    /// Multiply text by this factor (1.0 is the default size).
    TextScale(f32),
    HighContrast(bool),
    ScreenReader(bool),
}

/// How urgently a [live region](AccessNode::live) narrates its own update
/// *without moving focus* — a toast appearing, a status line changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Politeness {
    /// Wait for a natural pause before speaking.
    Polite,
    /// Speak at once, interrupting any in-flight narration.
    Assertive,
}

/// The per-widget *local* accessibility semantics returned by
/// [`Widget::accessibility`](crate::widget::Widget::accessibility);
/// `name`/`description` must already be **FTL-resolved** strings (via
/// [`i18n!`](macro@crate::i18n)), not translation keys, since AccessKit/AT-SPI
/// consume final text.
#[must_use]
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AccessNode {
    pub role: Role,
    /// Stable identity of a retained actionable widget within its semantic
    /// parent. Unlike a layout id, this survives rebuilds and sibling moves.
    pub semantic_key: Option<String>,
    /// Opaque stable identity used to route actions on a synthetic child back
    /// to the widget that produced it. It has meaning only to that widget.
    pub action_target: Option<String>,
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
    /// Stable identity of THIS node within its parent's
    /// [`children`](Self::children) list, for nodes that are **synthetic** (no
    /// backing [`Widget`](crate::widget::Widget)).
    ///
    /// The export layer derives a synthetic node's assistive-technology id by
    /// chaining these keys from the owning widget downwards, so an AT that
    /// holds a reference to "the clip named `intro`" keeps pointing at that
    /// clip across repaints — and the same key path is what comes back on
    /// [`Widget::perform_action`](crate::widget::Widget::perform_action)
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
    /// Absolute logical-pixel bounds for a synthetic node. Retained widgets
    /// receive their bounds from [`Widget::get_rect`](crate::widget::Widget::get_rect);
    /// virtual rows and cells use this field because they have no widget of
    /// their own. The AccessKit bridge applies the window scale factor.
    pub bounds: Option<AccessBounds>,
    /// Accessible children with no backing [`Widget`](crate::widget::Widget)
    /// of their own — a table's rows and cells, a virtualized list's items.
    /// The export layer emits them as this node's subtree, nested as deeply
    /// as needed (rows containing cells).
    pub children: Vec<AccessNode>,
}

/// A widget's [`AccessNode`] with its synthetic children shared rather than
/// owned, from [`Widget::accessibility_shared`](crate::widget::Widget::accessibility_shared).
///
/// A widget that keeps its children's nodes between frames (an editor's
/// paragraphs) hands each over as the very `Arc` it handed over last frame
/// when that child did not change. The semantic tree then knows the child is
/// unchanged without copying or comparing it, so a frame that changes one
/// child costs that child, not the widget's whole subtree.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedAccessNode {
    /// The widget's node. Its own `children` are empty: they are in
    /// [`Self::children`].
    pub node: AccessNode,
    pub children: Vec<std::sync::Arc<AccessNode>>,
}

impl SharedAccessNode {
    /// The node as [`Widget::accessibility`](crate::widget::Widget::accessibility)
    /// answers it, the children copied out where something else still holds
    /// them.
    pub fn into_owned(self) -> AccessNode {
        let Self { mut node, children } = self;
        node.children
            .extend(children.into_iter().map(std::sync::Arc::unwrap_or_clone));
        node
    }
}

impl AccessNode {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            semantic_key: None,
            action_target: None,
            name: None,
            description: None,
            value: None,
            text_selection: None,
            state: AccessState::default(),
            live: None,
            set_size: None,
            pos_in_set: None,
            level: None,
            key: None,
            bounds: None,
            children: Vec::new(),
        }
    }

    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        self.semantic_key = Some(key.into());
        self
    }

    /// Attach an opaque, widget-local target for actions on this synthetic
    /// node. Prefer stable model keys over visible indices.
    pub fn with_action_target(mut self, target: impl Into<String>) -> Self {
        let target = target.into();
        // An actionable synthetic node necessarily needs stable identity for
        // agents and AT to hold across frames. Derive the existing compact key
        // from the same opaque target so authors cannot accidentally provide
        // routability without durability.
        self.key = Some(hash_key(&target));
        self.semantic_key = Some(target.clone());
        self.action_target = Some(target);
        self
    }

    /// Route every keyed actionable descendant through its existing
    /// [`Widget::perform_action`](crate::widget::Widget::perform_action)
    /// implementation. This is the high-complexity custom-canvas adapter:
    /// timeline/editor authors keep one stable keyed semantic tree instead of
    /// duplicating an action id and handler for every painted affordance.
    pub fn with_keyed_action_routes(mut self, namespace: &str) -> Self {
        fn actionable(role: Role) -> bool {
            matches!(
                role,
                Role::Button
                    | Role::ToggleButton
                    | Role::Checkbox
                    | Role::RadioButton
                    | Role::Switch
                    | Role::ComboBox
                    | Role::Slider
                    | Role::SpinButton
                    | Role::TextInput
                    | Role::MultilineTextInput
            )
        }

        fn visit(node: &mut AccessNode, namespace: &str, parent: &[u64]) {
            let mut path = parent.to_vec();
            if let Some(key) = node.key {
                path.push(key);
            }
            if actionable(node.role) && node.action_target.is_none() && !path.is_empty() {
                let encoded = path
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join("/");
                let target = format!("tilo.key-action:{namespace}:{encoded}");
                node.semantic_key = Some(target.clone());
                node.action_target = Some(target);
            }
            for child in &mut node.children {
                visit(child, namespace, &path);
            }
        }

        visit(&mut self, namespace, &[]);
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Handy when an empty label should collapse to `None`.
    pub fn with_optional_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn with_optional_value(mut self, value: Option<String>) -> Self {
        self.value = value;
        self
    }

    /// Publish an editable caret (`anchor == focus`) or selection. Endpoints
    /// are UTF-8 bytes and are clamped to extended grapheme boundaries by the
    /// bridge, matching the toolkit editing contract.
    pub fn with_text_selection(mut self, anchor: usize, focus: usize) -> Self {
        self.text_selection = Some(AccessibleTextSelection { anchor, focus });
        self
    }

    pub fn with_state(mut self, state: AccessState) -> Self {
        self.state = state;
        self
    }

    pub fn with_live(mut self, politeness: Politeness) -> Self {
        self.live = Some(politeness);
        self
    }

    /// `pos_in_set` is 1-based within a total of `set_size`. Read out as "N of M".
    pub fn with_position(mut self, pos_in_set: usize, set_size: usize) -> Self {
        self.pos_in_set = Some(pos_in_set);
        self.set_size = Some(set_size);
        self
    }

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
    /// # use tilo_ui::a11y::{AccessNode, Role};
    /// let clip = AccessNode::new(Role::ListItem).with_hashed_key("clip-intro");
    /// assert_eq!(clip.key, Some(tilo_ui::a11y::hash_key(&"clip-intro")));
    /// ```
    pub fn with_hashed_key(mut self, key: impl std::hash::Hash) -> Self {
        self.key = Some(hash_key(&key));
        self
    }

    /// Attach absolute logical-pixel geometry to a synthetic semantic node.
    pub fn with_bounds(mut self, bounds: Rect) -> Self {
        self.bounds = Some(bounds.into());
        self
    }

    /// Attach synthetic [`children`](Self::children) — accessible elements
    /// with no backing widget (a table's rows/cells).
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
/// [`Widget::perform_action`](crate::widget::Widget::perform_action)).
/// The hash is a plain FNV-1a with a fixed basis, so it is deterministic within
/// and across processes — unlike [`std::collections::hash_map::RandomState`],
/// which would hand an assistive technology a different id every launch.
pub fn hash_key(value: &impl std::hash::Hash) -> u64 {
    let mut h = fnv::FnvHasher::default();
    std::hash::Hash::hash(value, &mut h);
    std::hash::Hasher::finish(&h)
}

/// Every name and value `root` and its descendants expose to assistive
/// technology, depth first.
pub fn texts(root: &dyn crate::widget::Widget) -> Vec<String> {
    fn node(access: &AccessNode, out: &mut Vec<String>) {
        out.extend(access.name.clone());
        out.extend(access.value.clone());
        for child in &access.children {
            node(child, out);
        }
    }
    fn visit(widget: &dyn crate::widget::Widget, out: &mut Vec<String>) {
        if let Some(access) = widget.accessibility() {
            node(&access, out);
        }
        widget.for_each_child(&mut |child| visit(child, out));
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out
}

/// Serializable absolute logical-pixel geometry for a synthetic
/// [`AccessNode`]. This mirrors [`Rect`] without requiring widget authors to
/// depend on its serialization feature.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct AccessBounds {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl AccessBounds {
    #[must_use]
    pub fn to_rect(self) -> Rect {
        Rect::new(self.x0, self.y0, self.x1, self.y1)
    }
}

impl From<Rect> for AccessBounds {
    fn from(value: Rect) -> Self {
        Self {
            x0: value.x0,
            y0: value.y0,
            x1: value.x1,
            y1: value.y1,
        }
    }
}

/// Editable-text selection in the widget's native UTF-8 document space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AccessibleTextSelection {
    pub anchor: usize,
    pub focus: usize,
}

/// Stored in the per-user settings CRDT and mirrored into spawned sandboxes
/// via `TILO_A11Y_*` env vars. Defaults are all-off so existing single-user
/// setups are unaffected until a user opts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct A11ySettings {
    /// Use a high-contrast theme track (see [`Theme::high_contrast`]).
    ///
    /// [`Theme::high_contrast`]: crate::theme::Theme::high_contrast
    pub high_contrast: bool,
    /// Animations jump to their end state instead of easing. Consulted by
    /// `crates/sdk/ui-core/src/animation` and the magnifier pan.
    pub reduce_motion: bool,
    /// Lets apps/shell enable extra narration hooks and keep idle frames
    /// flowing for live regions.
    pub screen_reader: bool,
}

/// WCAG 2.1 luminance and contrast live in [`crate::color`]; re-exported
/// here, where accessibility code has always found them.
pub use crate::color::{contrast_ratio, relative_luminance};

#[cfg(test)]
mod tests {
    use super::*;
    use vello::peniko::Color;

    #[test]
    fn access_state_and_settings_default_clear_and_builders_set_only_their_field() {
        let s = AccessState::default();
        for (field, set) in [
            ("focused", s.focused),
            ("focusable", s.focusable),
            ("selected", s.selected),
            ("disabled", s.disabled),
            ("read_only", s.read_only),
            ("protected", s.protected),
            ("multiline", s.multiline),
        ] {
            assert!(!set, "default {field} is clear");
        }
        assert_eq!(s.checked, None);
        assert_eq!(s.expanded, None);
        assert_eq!(s, AccessState::new());

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
        assert_eq!(
            AccessState::new().checked(false).checked,
            Some(false),
            "checked(false) is Some(false), not None"
        );
        assert_eq!(AccessState::new().expanded(false).expanded, Some(false));

        // Settings default to all off and round-trip through JSON.
        let s = A11ySettings::default();
        assert!(!s.high_contrast);
        assert!(!s.reduce_motion);
        assert!(!s.screen_reader);
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
    fn access_node_builders_compose_text_live_position_and_level() {
        let n = AccessNode::new(Role::Button);
        assert_eq!(n.role, Role::Button);
        assert_eq!(n.name, None);
        assert_eq!(n.description, None);
        assert_eq!(n.value, None);
        assert_eq!(n.state, AccessState::default());

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

        // Optional helpers.
        let named = AccessNode::new(Role::StaticText).with_optional_name(Some("Hi".into()));
        assert_eq!(named.name.as_deref(), Some("Hi"));
        let anon = AccessNode::new(Role::Image).with_optional_name(None);
        assert_eq!(anon.name, None);
        let masked = AccessNode::new(Role::TextInput).with_optional_value(None);
        assert_eq!(masked.value, None);

        // The heading role carries its level.
        let h2 = Role::Heading { level: 2 };
        assert_eq!(h2, Role::Heading { level: 2 });
        assert_ne!(h2, Role::Heading { level: 3 });
    }

    #[test]
    fn contrast_ratio_and_relative_luminance_follow_wcag() {
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

        assert!((relative_luminance(white) - 1.0).abs() < 1e-6);
        assert!(relative_luminance(black).abs() < 1e-6);
        // Green contributes more luminance than red or blue (WCAG weights).
        let red = relative_luminance(Color::from_rgb8(255, 0, 0));
        let green = relative_luminance(Color::from_rgb8(0, 255, 0));
        let blue = relative_luminance(Color::from_rgb8(0, 0, 255));
        assert!(green > red && red > blue);
    }
}

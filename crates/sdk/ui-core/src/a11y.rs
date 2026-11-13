//! Accessibility semantic model: the **data layer**. Defines the toolkit-ergonomic semantic types —
//! [`Role`], [`AccessNode`], [`AccessState`], and [`Action`] — that every
//! [`Widget`](crate::widget::Widget) describes itself with via
//! [`Widget::accessibility`](crate::widget::Widget::accessibility).

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
    /// The options a [`Role::ComboBox`] offers while it is open.
    ListBox,
    /// One option of a [`Role::ListBox`]; use [`AccessState::selected`].
    ListBoxOption,

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
    /// A focusable divider between two panes that moves with the arrow keys; its value is the first
    /// pane's share in percent.
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
    /// The item the keyboard is on inside a focused control that keeps the
    /// focus itself: an open combo box's highlighted option. Assistive
    /// technology announces it while focus stays on the control (AccessKit's
    /// `active_descendant`, ARIA's `aria-activedescendant`). Unlike
    /// [`focused`](Self::focused) it never moves the tree's focus.
    pub active_descendant: bool,
    pub focusable: bool,
    /// The control owns a standard context menu.
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
    /// What a text field asks for, so a browser or password manager can fill it in and a page can offer
    /// passkeys on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofill: Option<Autofill>,
}

/// What a text field asks for, in the terms browsers and password managers fill fields by (HTML's
/// `autocomplete` tokens).
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Autofill {
    /// The name the person signs in with; a page offers passkeys here too.
    Username,
    Email,
    /// The password the person signs in with.
    CurrentPassword,
    /// A password the person is choosing.
    NewPassword,
    /// A code sent to the person or shown by their authenticator.
    OneTimeCode,
    /// The person full name.
    Name,
    /// A telephone number.
    Tel,
}

impl AccessState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn focused(mut self, v: bool) -> Self {
        self.focused = v;
        self
    }
    /// Sets [`active_descendant`](Self::active_descendant).
    pub fn active_descendant(mut self, v: bool) -> Self {
        self.active_descendant = v;
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
    /// Sets [`autofill`](Self::autofill).
    pub fn autofill(mut self, purpose: Option<Autofill>) -> Self {
        self.autofill = purpose;
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
    /// Extended-grapheme indices into the accessible `TextRun`, matching AccessKit's
    /// selectable-character model rather than Unicode scalars.
    SetSelection {
        /// Where the selection started.
        anchor: usize,
        /// Where it ends.
        focus: usize,
    },
    ScrollIntoView,
    ShowContextMenu,
}

/// Whether activating a control of `role` has a meaning of it, so an assistive technology is offered
/// the activation: the roles a person presses.
#[must_use]
pub(crate) fn activation_role(role: Role) -> bool {
    pressable_role(crate::accesskit_bridge::map_role(role))
}

/// [`activation_role`] of a role as the exported tree names it: the same roles, read back from a
/// node a widget described ([`crate::accesskit_bridge::map_role`]).
#[must_use]
pub(crate) const fn pressable_role(role: crate::accesskit::Role) -> bool {
    matches!(
        role,
        crate::accesskit::Role::Button
            | crate::accesskit::Role::CheckBox
            | crate::accesskit::Role::RadioButton
            | crate::accesskit::Role::Switch
            | crate::accesskit::Role::Tab
            | crate::accesskit::Role::MenuItem
            | crate::accesskit::Role::ListItem
            | crate::accesskit::Role::TreeItem
            | crate::accesskit::Role::Link
    )
}

/// A change to the person's accessibility preferences that a control asked for, such as the sign-in
/// screen's larger-text or high-contrast toggle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AccessibilityRequest {
    /// Multiply text by this factor (1.0 is the default size).
    TextScale(f32),
    HighContrast(bool),
    ScreenReader(bool),
}

/// How urgently a [live region](AccessNode::live) narrates its update *without moving focus* — a toast
/// appearing, a status line changing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Politeness {
    /// Wait for a natural pause before speaking.
    Polite,
    /// Speak at once, interrupting any in-flight narration.
    Assertive,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn not(value: &bool) -> bool {
    !*value
}

/// The per-widget *local* accessibility semantics returned by
/// [`Widget::accessibility`](crate::widget::Widget::accessibility);
/// `name`/`description` must already be **FTL-resolved** strings (via
/// [`widget_text`](crate::i18n::widget_text)), not translation keys, since
/// AccessKit/AT-SPI consume final text.
#[must_use]
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AccessNode {
    pub role: Role,
    /// Stable identity of a retained actionable widget within its semantic parent.
    pub semantic_key: Option<String>,
    /// Opaque stable identity used to route actions on a synthetic child back to the widget that
    /// produced it.
    pub action_target: Option<String>,
    /// The primary label a screen reader announces.
    pub name: Option<String>,
    /// `name` is a fallback the widget found for itself (the value it shows) rather than words given
    /// for it.
    #[serde(default, skip_serializing_if = "not")]
    pub name_found: bool,
    pub description: Option<String>,
    /// Never populated for [`protected`](AccessState::protected) elements.
    pub value: Option<String>,
    /// How many UTF-16 code units a [`protected`](AccessState::protected)
    /// value has, which is what the characters a person typed into it
    /// measure in a page field. A host that shows the interface as
    /// HTML keeps the person's text in that field and never receives the
    /// characters back, so this is how it learns the app cleared or replaced
    /// them and its field must let them go too. A masked field shows one
    /// dot per character, so the count of them is already on screen.
    pub value_length: Option<usize>,
    /// Direction-preserving UTF-8 byte endpoints for editable text. The
    /// AccessKit bridge converts these to grapheme indices in a `TextRun`.
    pub text_selection: Option<AccessibleTextSelection>,
    pub state: AccessState,
    /// The `App` watches its spoken text each frame and announces changes at the given politeness
    /// *without* moving focus.
    pub live: Option<Politeness>,
    /// Total items in the owning collection, pairs with
    /// [`pos_in_set`](Self::pos_in_set) to let the reader say "2 of 4".
    pub set_size: Option<usize>,
    pub pos_in_set: Option<usize>,
    pub level: Option<usize>,
    /// Stable identity of THIS node within its parent's
    /// [`children`](Self::children) list, for nodes that are **synthetic** (no
    /// backing [`Widget`](crate::widget::Widget)).
    ///
    /// The export layer derives a synthetic node's assistive-technology id by
    /// chaining these keys from the owning widget downwards, so an AT that
    /// has a reference to "the clip named `intro`" keeps pointing at that
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
    /// Accessible children with no backing [`Widget`](crate::widget::Widget) of their own — a table's
    /// rows and cells, a virtualized list's items.
    pub children: Vec<AccessNode>,
    /// Whether [`name`](Self::name) is the app fixed, localized copy (a
    /// button's label, a heading), the same for everyone, rather than
    /// something a person or a resource put there. Unset means content: a
    /// feedback report hides it until the reporter shows it.
    #[serde(skip)]
    pub interface_name: bool,
    /// What kind of thing this is in the app words, where its role
    /// says too little ("Message" for a chat row that is a list item), in
    /// the reader's language. Always the app copy, never content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_description: Option<String>,
    /// The item of its app's collection this node shows: a note's block id, a task's issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// What this control does, as a stable identity: the record kind it acts on and the operation
    /// (`so.tilo.lang.notes.Notebook` and `new_note`, a field's `title` for a control that edits one),
    /// written as [`tilo_actions::ActionAddress`] writes an address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// A widget's [`AccessNode`] with its synthetic children shared rather than owned, from
/// [`Widget::accessibility_shared`](crate::widget::Widget::accessibility_shared).
#[derive(Clone, Debug, PartialEq)]
pub struct SharedAccessNode {
    /// The widget's node. Its own `children` are empty: they are in [`Self::children`].
    pub node: AccessNode,
    pub children: Vec<std::sync::Arc<AccessNode>>,
}

impl SharedAccessNode {
    /// The node as [`Widget::accessibility`](crate::widget::Widget::accessibility) answers it, the
    /// children copied out where something else still has them.
    pub fn into_owned(self) -> AccessNode {
        let Self { mut node, children } = self;
        node.children
            .extend(children.into_iter().map(std::sync::Arc::unwrap_or_clone));
        node
    }
}

impl AccessNode {
    /// This node and every node under it, in tree order.
    #[must_use]
    pub fn nodes(&self) -> Vec<&Self> {
        let mut out = Vec::new();
        let mut next = vec![self];
        while let Some(node) = next.pop() {
            out.push(node);
            next.extend(node.children.iter().rev());
        }
        out
    }

    pub fn new(role: Role) -> Self {
        Self {
            role,
            semantic_key: None,
            action_target: None,
            name: None,
            name_found: false,
            description: None,
            value: None,
            value_length: None,
            text_selection: None,
            state: AccessState::default(),
            live: None,
            set_size: None,
            pos_in_set: None,
            level: None,
            key: None,
            bounds: None,
            children: Vec::new(),
            interface_name: false,
            role_description: None,
            item: None,
            action: None,
        }
    }

    /// Say what this control does ([`Self::action`]): the identity of the app action it runs, or of the
    /// field it edits, in the language of ids.
    pub fn with_action(mut self, action: impl Into<String>) -> Self {
        self.action = Some(action.into()).filter(|action| !action.is_empty());
        self
    }

    /// Say which item of its collection this node shows ([`Self::item`]).
    pub fn with_item(mut self, item: impl Into<String>) -> Self {
        self.item = Some(item.into()).filter(|item| !item.is_empty());
        self
    }

    /// Mark the name as the app fixed, localized copy ([`Self::interface_name`]).
    pub fn with_interface_name(mut self, interface: bool) -> Self {
        self.interface_name = interface;
        self
    }

    /// Say what kind of thing this is in the app words ([`Self::role_description`]).
    pub fn with_role_description(mut self, kind: impl Into<String>) -> Self {
        self.role_description = Some(kind.into()).filter(|kind| !kind.trim().is_empty());
        self
    }

    pub fn with_semantic_key(mut self, key: impl Into<String>) -> Self {
        self.semantic_key = Some(key.into());
        self
    }

    /// Whether this node's id stays the same control across a rebuild: it has a semantic key of its
    /// own, or the key it hashed, rather than its place among its siblings.
    pub(crate) fn is_keyed(&self) -> bool {
        self.semantic_key.is_some() || self.key.is_some()
    }

    /// Attach an opaque, widget-local target for actions on this synthetic node.
    pub fn with_action_target(self, target: impl Into<String>) -> Self {
        self.with_owned_action_target(target.into())
    }

    fn with_owned_action_target(mut self, target: String) -> Self {
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
    /// [`Widget::perform_action`](crate::widget::Widget::perform_action) implementation.
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
                    | Role::ListBoxOption
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

    /// [`Self::with_name`] for a name the widget found for itself, the value it shows: a parent that
    /// has the control's words says its ([`Self::name_found`]).
    pub fn with_found_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self.name_found = true;
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set or drop the description, for a node whose default one would only
    /// repeat what its name already says.
    pub fn with_optional_description(mut self, description: Option<String>) -> Self {
        self.description = description;
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

    /// Publish the length of a [`protected`](AccessState::protected) value without its characters (see
    /// [`Self::value_length`]).
    pub fn with_value_length(mut self, value_length: usize) -> Self {
        self.value_length = Some(value_length);
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

    /// Attach synthetic [`children`](Self::children) — accessible elements with no backing widget (a
    /// table's rows/cells).
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

/// Every name and value `root` and its descendants expose to assistive technology, depth first.
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

/// Serializable absolute logical-pixel geometry for a synthetic [`AccessNode`].
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

/// Where a synthetic node (one of `owner`'s accessibility children, at `path` below its node) is drawn:
/// its bounds, else the rect the owner reports for it.
pub(crate) fn synthetic_bounds(
    access: &AccessNode,
    owner: &dyn crate::widget::Widget,
    path: &[usize],
) -> Option<AccessBounds> {
    access.bounds.or_else(|| {
        owner
            .synthetic_accessibility_rect(path)
            .filter(|rect| rect.width() > 0.0 || rect.height() > 0.0)
            .map(Into::into)
    })
}

/// `owner`'s accessibility node with every synthetic descendant's
/// geometry filled in by [`synthetic_bounds`], as the native accessibility
/// tree reads it: a node exported without bounds would otherwise be placed
/// over its owner's whole rect, and a pointer sent to its centre would miss.
pub(crate) fn with_synthetic_bounds(
    mut access: AccessNode,
    owner: &dyn crate::widget::Widget,
) -> AccessNode {
    fn fill(node: &mut AccessNode, owner: &dyn crate::widget::Widget, path: &mut Vec<usize>) {
        for (index, child) in node.children.iter_mut().enumerate() {
            path.push(index);
            child.bounds = synthetic_bounds(child, owner, path);
            fill(child, owner, path);
            path.pop();
        }
    }
    fill(&mut access, owner, &mut Vec::new());
    access
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
    pub high_contrast: bool,
    /// Animations jump to their end state instead of easing.
    pub reduce_motion: bool,
    /// Lets apps/shell enable extra narration hooks and keep idle frames flowing for live regions.
    pub screen_reader: bool,
}

/// WCAG 2.1 luminance and contrast live in [`crate::color`]; re-exported
/// here, where accessibility code has always found them.
pub use crate::color::{contrast_ratio, relative_luminance};

/// The node `widget` states, and what a row of one is called: its, or the first node a widget it wraps
/// names itself with.
pub(crate) fn stated_node(widget: &dyn crate::widget::Widget) -> Option<AccessNode> {
    if let Some(node) = widget.accessibility()
        && node.name.is_some()
    {
        return Some(node);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| stated_node(child))
}

/// The widget that states `widget`'s row ([`stated_node`]), to act on: the one whose node an owner
/// publishing the row hands over, so the one an action on that node reaches.
pub(crate) fn stated_widget_mut(
    widget: &mut dyn crate::widget::Widget,
) -> Option<&mut dyn crate::widget::Widget> {
    if widget
        .accessibility()
        .is_some_and(|node| node.name.is_some())
    {
        return Some(widget);
    }
    for child in widget.children_mut() {
        if let Some(stated) = stated_widget_mut(child) {
            return Some(stated);
        }
    }
    None
}

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
        assert_eq!(s.expanded, None);
        assert!(!s.read_only);
        assert_eq!(
            AccessState::new().checked(false).checked,
            Some(false),
            "checked(false) is Some(false), not None"
        );
        assert_eq!(AccessState::new().expanded(false).expanded, Some(false));

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

        let named = AccessNode::new(Role::StaticText).with_optional_name(Some("Hi".into()));
        assert_eq!(named.name.as_deref(), Some("Hi"));
        let anon = AccessNode::new(Role::Image).with_optional_name(None);
        assert_eq!(anon.name, None);
        let masked = AccessNode::new(Role::TextInput).with_optional_value(None);
        assert_eq!(masked.value, None);

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
        let red = relative_luminance(Color::from_rgb8(255, 0, 0));
        let green = relative_luminance(Color::from_rgb8(0, 255, 0));
        let blue = relative_luminance(Color::from_rgb8(0, 0, 255));
        assert!(green > red && red > blue);
    }
}

//! The runtime's view tree and keyed patches as a UI graph (implementation design 7.6).
//!
//! A tilolang view evaluates to a **concrete** tree: loops are already expanded and every node
//! has a stable key. This module translates that tree into a [`UiDocument`] (node id = node key,
//! text literal) and the runtime's keyed patches into [`UiGraphEdit`]s, which the host applies
//! to its live graph; the toolkit reconciles by id and keeps focus.
//!
//! Controls do not write the document. A button reports its click under its node key, and a
//! bound input or checkbox shows a value the host keeps current and routes each edit through a
//! published action. [`Control`] tells the host which of those a key is, so the host can register
//! the routes before the node is built.

use std::collections::BTreeMap;

use serde_json::Value as Json;
use tilo_ui_graph::{
    UiButtonRole, UiDocument, UiGraphEdit, UiGraphError, UiNode, UiTextRole, UiTextSource,
};

/// The component name of an embedded block (`embed`), which the host builds.
pub const EMBED_COMPONENT: &str = "tl-embed";

/// A resource, by app, type and id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceRef {
    pub app: String,
    pub type_id: String,
    pub id: String,
}

/// A two-way binding of an input or checkbox to a field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bind {
    pub resource: ResourceRef,
    pub field: String,
}

/// An action reference stored in a view node: what a click invokes.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionRef {
    pub app: String,
    pub action_id: String,
    /// The subject resource, when the action takes one.
    pub resource: Option<ResourceRef>,
    /// The remaining arguments by parameter name.
    pub args: serde_json::Map<String, Json>,
}

/// One node of the runtime's concrete view tree.
#[derive(Clone, Debug, PartialEq)]
pub struct UiElem {
    pub key: String,
    /// `column`, `row`, `text`, `heading`, `caption`, `button`, `input`, `checkbox`, `section`,
    /// `spacer` or `embed`.
    pub kind: String,
    pub props: BTreeMap<String, Json>,
    pub bind: Option<Bind>,
    /// Event name (`press`) to the action it invokes.
    pub on: BTreeMap<String, ActionRef>,
    pub anchor: Option<String>,
    pub children: Vec<UiElem>,
}

impl UiElem {
    #[must_use]
    pub fn new(key: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            kind: kind.into(),
            props: BTreeMap::new(),
            bind: None,
            on: BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        }
    }

    #[must_use]
    pub fn prop(mut self, name: impl Into<String>, value: impl Into<Json>) -> Self {
        self.props.insert(name.into(), value.into());
        self
    }

    #[must_use]
    pub fn child(mut self, child: Self) -> Self {
        self.children.push(child);
        self
    }

    #[must_use]
    pub fn on_press(mut self, action: ActionRef) -> Self {
        self.on.insert("press".to_owned(), action);
        self
    }

    #[must_use]
    pub fn bound(mut self, bind: Bind) -> Self {
        self.bind = Some(bind);
        self
    }

    fn find(&self, key: &str) -> Option<&Self> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(key))
    }

    fn find_mut(&mut self, key: &str) -> Option<&mut Self> {
        if self.key == key {
            return Some(self);
        }
        self.children
            .iter_mut()
            .find_map(|child| child.find_mut(key))
    }

    /// Detach the descendant `key`.
    fn detach(&mut self, key: &str) -> Option<Self> {
        if let Some(position) = self.children.iter().position(|child| child.key == key) {
            return Some(self.children.remove(position));
        }
        self.children.iter_mut().find_map(|child| child.detach(key))
    }

    fn text_prop(&self, name: &str) -> Option<String> {
        self.props.get(name).map(value_text)
    }

    fn flag_prop(&self, name: &str) -> bool {
        self.props
            .get(name)
            .and_then(Json::as_bool)
            .unwrap_or(false)
    }
}

/// How a value of a text prop reads: text as itself, numbers and flags as their literals.
fn value_text(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

/// One change of the runtime's tree.
#[derive(Clone, Debug, PartialEq)]
pub enum UiPatch {
    Insert {
        parent: String,
        index: usize,
        node: UiElem,
    },
    Remove {
        key: String,
    },
    /// `index` is the position in the parent after the node was detached.
    Move {
        key: String,
        parent: String,
        index: usize,
    },
    SetProp {
        key: String,
        name: String,
        value: Json,
    },
}

/// What a keyed node does when the person uses it.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// A button: its click invokes `action`.
    Press(Option<ActionRef>),
    /// A text input showing `shown`, each edit replacing the bound field.
    Text { bind: Bind, shown: String },
    /// A checkbox showing `shown`, each toggle setting the bound field.
    Flag { bind: Bind, shown: bool },
}

#[derive(Debug, thiserror::Error)]
pub enum TranslateError {
    #[error("the view node `{key}` is of the unknown kind `{kind}`")]
    UnknownKind { key: String, kind: String },
    #[error("there is no view node `{0}`")]
    UnknownKey(String),
    #[error("the view node `{0}` cannot hold children")]
    NotAContainer(String),
    #[error("the view node `{0}` needs a label, or has nothing to name it for assistive tools")]
    MissingLabel(String),
    #[error("the view node `{0}` is a bound control with no binding")]
    Unbound(String),
    #[error("the view node `{0}` cannot be moved into itself")]
    MoveIntoSelf(String),
    #[error("the translated graph is invalid: {0}")]
    Graph(#[from] UiGraphError),
}

/// The graph of a whole tree and the controls in it.
#[derive(Debug)]
pub struct Rendered {
    pub document: UiDocument,
    pub controls: Vec<(String, Control)>,
}

/// The change one batch of patches makes.
#[derive(Debug, Default, PartialEq)]
pub struct Update {
    /// Register these before applying `edits`: a node is built with its route already in place.
    pub registered: Vec<(String, Control)>,
    pub edits: Vec<UiGraphEdit>,
    /// Keys whose controls no longer exist.
    pub unregistered: Vec<String>,
    /// New text to show in the input of a key.
    pub text: Vec<(String, String)>,
    /// New state to show in the checkbox of a key.
    pub flags: Vec<(String, bool)>,
}

/// The id of the node that holds a node's children.
fn items_id(elem: &UiElem) -> String {
    if elem.kind == "section" {
        format!("{}~items", elem.key)
    } else {
        elem.key.clone()
    }
}

fn is_container(kind: &str) -> bool {
    matches!(kind, "column" | "row" | "section")
}

fn text_node(id: String, text: String, role: UiTextRole) -> UiNode {
    UiNode::Text {
        id,
        text: UiTextSource::Static { value: text },
        role,
    }
}

fn accessible_label(elem: &UiElem) -> Result<String, TranslateError> {
    elem.text_prop("label")
        .filter(|label| !label.trim().is_empty())
        .or_else(|| {
            elem.text_prop("placeholder")
                .filter(|label| !label.trim().is_empty())
        })
        .ok_or_else(|| TranslateError::MissingLabel(elem.key.clone()))
}

/// Translate `elem` and everything below it, recording the controls on the way.
fn to_node(elem: &UiElem, controls: &mut Vec<(String, Control)>) -> Result<UiNode, TranslateError> {
    let id = elem.key.clone();
    let mut children = |elem: &UiElem| -> Result<Vec<UiNode>, TranslateError> {
        elem.children
            .iter()
            .map(|child| to_node(child, controls))
            .collect()
    };
    Ok(match elem.kind.as_str() {
        "column" => UiNode::Column {
            id,
            children: children(elem)?,
            gap: None,
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        },
        "row" => UiNode::Row {
            id,
            children: children(elem)?,
            no_wrap: false,
            fill_remaining_height: false,
        },
        "section" => {
            let body = vec![
                text_node(
                    format!("{id}~title"),
                    elem.text_prop("title").unwrap_or_default(),
                    UiTextRole::Heading,
                ),
                UiNode::Column {
                    id: format!("{id}~items"),
                    children: children(elem)?,
                    gap: None,
                    no_padding: true,
                    intrinsic_height: false,
                    fill_remaining_height: false,
                },
            ];
            UiNode::Container {
                id: id.clone(),
                child: Box::new(UiNode::Column {
                    id: format!("{id}~body"),
                    children: body,
                    gap: None,
                    no_padding: true,
                    intrinsic_height: false,
                    fill_remaining_height: false,
                }),
                background: true,
                no_padding: false,
                fill_height: false,
                width: None,
                height: None,
            }
        }
        "text" => text_node(
            id,
            elem.text_prop("text").unwrap_or_default(),
            UiTextRole::Body,
        ),
        "heading" => text_node(
            id,
            elem.text_prop("text").unwrap_or_default(),
            UiTextRole::Heading,
        ),
        "caption" => text_node(
            id,
            elem.text_prop("text").unwrap_or_default(),
            UiTextRole::Caption,
        ),
        "spacer" => UiNode::Spacer { id },
        "button" => {
            let label = elem.text_prop("label").unwrap_or_default();
            if label.trim().is_empty() {
                return Err(TranslateError::MissingLabel(id));
            }
            let action = elem.on.get("press").cloned();
            let enabled = action.is_some() && !elem.flag_prop("disabled");
            controls.push((id.clone(), Control::Press(action)));
            UiNode::Button {
                id: id.clone(),
                label,
                accessible_name: None,
                tooltip: None,
                on_click_path: Some(id),
                click_value: None,
                navigate: None,
                enabled,
                role: if elem.flag_prop("primary") {
                    UiButtonRole::Primary
                } else {
                    UiButtonRole::Secondary
                },
            }
        }
        "input" => {
            let bind = elem
                .bind
                .clone()
                .ok_or_else(|| TranslateError::Unbound(id.clone()))?;
            let label = accessible_label(elem)?;
            controls.push((
                id.clone(),
                Control::Text {
                    bind,
                    shown: elem.text_prop("value").unwrap_or_default(),
                },
            ));
            UiNode::TextInput {
                id: id.clone(),
                label: Some(label),
                accessible_name: String::new(),
                placeholder: elem.text_prop("placeholder").unwrap_or_default(),
                value_path: Some(id),
                initial_value: String::new(),
                multiline: elem.flag_prop("multiline"),
                disabled: elem.flag_prop("disabled"),
            }
        }
        "checkbox" => {
            let bind = elem
                .bind
                .clone()
                .ok_or_else(|| TranslateError::Unbound(id.clone()))?;
            let label = accessible_label(elem)?;
            controls.push((
                id.clone(),
                Control::Flag {
                    bind,
                    shown: elem.flag_prop("checked"),
                },
            ));
            UiNode::Checkbox {
                id: id.clone(),
                label: Some(label),
                accessible_name: String::new(),
                value_path: id,
                enabled: !elem.flag_prop("disabled"),
            }
        }
        "embed" => {
            let mut props = serde_json::Map::new();
            for name in ["source", "author"] {
                if let Some(value) = elem.props.get(name) {
                    props.insert(name.to_owned(), value.clone());
                }
            }
            UiNode::Component {
                id,
                name: EMBED_COMPONENT.to_owned(),
                props,
            }
        }
        other => {
            return Err(TranslateError::UnknownKind {
                key: id,
                kind: other.to_owned(),
            });
        }
    })
}

/// Every control below `elem` (itself included), by key, as a fresh translation finds them.
fn controls_of(elem: &UiElem) -> Result<Vec<(String, Control)>, TranslateError> {
    let mut controls = Vec::new();
    to_node(elem, &mut controls)?;
    Ok(controls)
}

fn keys_below(elem: &UiElem, out: &mut Vec<String>) {
    out.push(elem.key.clone());
    for child in &elem.children {
        keys_below(child, out);
    }
}

/// Keeps the runtime's tree and turns its patches into graph edits.
#[derive(Debug, Default)]
pub struct Translator {
    root: Option<UiElem>,
    controls: BTreeMap<String, Control>,
}

impl Translator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace everything with `tree`.
    ///
    /// # Errors
    /// The tree has a node that is no primitive, lacks a label, or does not form a valid graph.
    pub fn load(&mut self, tree: UiElem) -> Result<Rendered, TranslateError> {
        let mut controls = Vec::new();
        let node = to_node(&tree, &mut controls)?;
        let document = UiDocument::new(node);
        document.validate()?;
        self.controls = controls.iter().cloned().collect();
        self.root = Some(tree);
        Ok(Rendered { document, controls })
    }

    /// What the node `key` does when used.
    #[must_use]
    pub fn control(&self, key: &str) -> Option<&Control> {
        self.controls.get(key)
    }

    /// The tree as the patches so far made it.
    #[must_use]
    pub const fn tree(&self) -> Option<&UiElem> {
        self.root.as_ref()
    }

    /// Apply one batch of patches to the kept tree and return the graph edits that make the
    /// same change. A property no graph property stands for replaces its node whole, which is
    /// always correct.
    ///
    /// # Errors
    /// A patch names a node that is not there, or produces an invalid node. The kept tree is
    /// then unchanged.
    pub fn apply(&mut self, patches: &[UiPatch]) -> Result<Update, TranslateError> {
        let Some(root) = self.root.clone() else {
            return Err(TranslateError::UnknownKey("root".to_owned()));
        };
        let mut working = root;
        let mut update = Update::default();
        for patch in patches {
            Self::apply_one(&mut working, patch, &mut update)?;
        }
        for key in &update.unregistered {
            self.controls.remove(key);
        }
        for (key, control) in &update.registered {
            self.controls.insert(key.clone(), control.clone());
        }
        for (key, text) in &update.text {
            if let Some(Control::Text { shown, .. }) = self.controls.get_mut(key) {
                shown.clone_from(text);
            }
        }
        for (key, flag) in &update.flags {
            if let Some(Control::Flag { shown, .. }) = self.controls.get_mut(key) {
                *shown = *flag;
            }
        }
        self.root = Some(working);
        Ok(update)
    }

    fn apply_one(
        tree: &mut UiElem,
        patch: &UiPatch,
        update: &mut Update,
    ) -> Result<(), TranslateError> {
        match patch {
            UiPatch::Insert {
                parent,
                index,
                node,
            } => {
                let holder = tree
                    .find_mut(parent)
                    .ok_or_else(|| TranslateError::UnknownKey(parent.clone()))?;
                if !is_container(&holder.kind) {
                    return Err(TranslateError::NotAContainer(parent.clone()));
                }
                let at = (*index).min(holder.children.len());
                let target = items_id(holder);
                let mut controls = Vec::new();
                let translated = to_node(node, &mut controls)?;
                holder.children.insert(at, node.clone());
                update.registered.extend(controls);
                update.edits.push(UiGraphEdit::Insert {
                    parent: target,
                    index: at,
                    node: translated,
                });
            }
            UiPatch::Remove { key } => {
                let removed = tree
                    .detach(key)
                    .ok_or_else(|| TranslateError::UnknownKey(key.clone()))?;
                keys_below(&removed, &mut update.unregistered);
                update.edits.push(UiGraphEdit::Remove { id: key.clone() });
            }
            UiPatch::Move { key, parent, index } => {
                let moved = tree
                    .detach(key)
                    .ok_or_else(|| TranslateError::UnknownKey(key.clone()))?;
                if moved.find(parent).is_some() {
                    return Err(TranslateError::MoveIntoSelf(key.clone()));
                }
                let holder = tree
                    .find_mut(parent)
                    .ok_or_else(|| TranslateError::UnknownKey(parent.clone()))?;
                if !is_container(&holder.kind) {
                    return Err(TranslateError::NotAContainer(parent.clone()));
                }
                let at = (*index).min(holder.children.len());
                let target = items_id(holder);
                holder.children.insert(at, moved);
                update.edits.push(UiGraphEdit::Move {
                    id: key.clone(),
                    parent: target,
                    index: at,
                });
            }
            UiPatch::SetProp { key, name, value } => {
                let elem = tree
                    .find_mut(key)
                    .ok_or_else(|| TranslateError::UnknownKey(key.clone()))?;
                elem.props.insert(name.clone(), value.clone());
                Self::prop_changed(elem, name, value, update)?;
            }
        }
        Ok(())
    }

    /// The edits one changed property makes.
    fn prop_changed(
        elem: &UiElem,
        name: &str,
        value: &Json,
        update: &mut Update,
    ) -> Result<(), TranslateError> {
        let id = elem.key.clone();
        let set = |id: String, property: &str, value: Json| UiGraphEdit::SetProperty {
            id,
            property: property.to_owned(),
            value,
        };
        match (elem.kind.as_str(), name) {
            ("text" | "heading" | "caption", "text") => {
                update
                    .edits
                    .push(set(id, "text.value", Json::String(value_text(value))));
            }
            ("button", "label") if !value_text(value).trim().is_empty() => {
                update
                    .edits
                    .push(set(id, "label", Json::String(value_text(value))));
            }
            ("button", "disabled") => {
                let enabled = elem.on.contains_key("press") && !value.as_bool().unwrap_or(false);
                update.edits.push(set(id, "enabled", Json::Bool(enabled)));
            }
            ("section", "title") => update.edits.push(set(
                format!("{id}~title"),
                "text.value",
                Json::String(value_text(value)),
            )),
            ("input", "value") => update.text.push((id, value_text(value))),
            ("checkbox", "checked") => update.flags.push((id, value.as_bool().unwrap_or(false))),
            _ => {
                // No graph property stands for it: rebuild the node.
                let mut controls = Vec::new();
                let node = to_node(elem, &mut controls)?;
                let mut gone = Vec::new();
                keys_below(elem, &mut gone);
                update.unregistered.extend(gone);
                update.registered.extend(controls);
                update.edits.push(UiGraphEdit::Replace { id, node });
            }
        }
        Ok(())
    }
}

/// An event on a keyed node, as the host reads it from the toolkit.
#[derive(Clone, Debug, PartialEq)]
pub enum UiEvent {
    /// The button `key` was clicked.
    Press { key: String },
    /// The input `key` now holds `text`.
    Edit { key: String, text: String },
    /// The checkbox `key` is now `checked`.
    Toggle { key: String, checked: bool },
}

/// What an event asks of the runtime.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Invoke an action reference stored in the view.
    Invoke(ActionRef),
    /// Replace the text of a bound field.
    SetText { bind: Bind, text: String },
    /// Set a bound flag.
    SetFlag { bind: Bind, checked: bool },
}

impl Translator {
    /// The runtime request an event on a node stands for; `None` when the node is not a control
    /// of that kind (a press on a disabled button, an edit of a key that went away).
    #[must_use]
    pub fn request(&self, event: &UiEvent) -> Option<Request> {
        match (
            event,
            self.controls.get(match event {
                UiEvent::Press { key }
                | UiEvent::Edit { key, .. }
                | UiEvent::Toggle { key, .. } => key,
            })?,
        ) {
            (UiEvent::Press { .. }, Control::Press(Some(action))) => {
                Some(Request::Invoke(action.clone()))
            }
            (UiEvent::Edit { text, .. }, Control::Text { bind, .. }) => Some(Request::SetText {
                bind: bind.clone(),
                text: text.clone(),
            }),
            (UiEvent::Toggle { checked, .. }, Control::Flag { bind, .. }) => {
                Some(Request::SetFlag {
                    bind: bind.clone(),
                    checked: *checked,
                })
            }
            _ => None,
        }
    }
}

/// Which keys of `tree` are controls: a convenience for hosts that rebuild everything.
#[must_use]
pub fn control_keys(tree: &UiElem) -> Vec<String> {
    controls_of(tree)
        .map(|controls| controls.into_iter().map(|(key, _)| key).collect())
        .unwrap_or_default()
}

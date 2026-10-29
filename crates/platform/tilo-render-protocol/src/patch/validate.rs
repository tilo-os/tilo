//! The check every batch an app sends passes before the UI host touches a widget.
//!
//! The UI host builds every window of a room from what each app sends. A batch that makes it
//! build a widget wrongly, or work without bound, takes the room's windows down with it. So
//! every message is checked, once, in the host's own copy:
//!
//! - **Valid by type.** A described component's node (`ui.ListRow`) names a component of the
//!   toolkit's catalogue; each of its props is one the description declares (or a sizing
//!   argument every node takes), holding a value of the declared type; each event and binding
//!   is one it declares. A primitive of the language (`stack`, `list`) is one the host builds.
//! - **Bounded.** Strings, keys, paths, props, depth and the rows of one message are limited,
//!   and each window's trees together hold at most [`PatchLimits::nodes`] nodes (the host
//!   counts them as it applies patches: one window's budget is its own).
//! - **Only its own trees.** A connection is one window of one app; its patches name nodes by
//!   key paths in the trees it sent, and a path that names no node is refused when applied.

use tilo_ui_graph::component::{Catalogue, Description, Type};

use super::{Binding, Event, Node, Patch, ResourceRef, RowChange, ToApp, ToUi, Value, Words};

/// Why a batch was refused: what was wrong, and where.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{what}{at}")]
pub struct Refused {
    pub what: &'static str,
    /// Where, from the outermost part in, such as `` in `stack` s in prop `gap` ``.
    pub at: String,
}

impl Refused {
    #[must_use]
    pub fn invalid(what: &'static str) -> Self {
        Self {
            what,
            at: String::new(),
        }
    }

    /// The same refusal, found in `part`.
    #[must_use]
    pub fn within(mut self, part: impl std::fmt::Display) -> Self {
        self.at = format!(" in {part}{}", self.at);
        self
    }
}

/// What one window may make its UI host hold and do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PatchLimits {
    /// Nodes of one window's trees together, rendered rows included.
    pub nodes: u32,
    /// How deep a tree nests.
    pub depth: u32,
    /// Bytes of one string: a prop's text, a bound value, a splice.
    pub string_bytes: u32,
    /// Bytes of one key or name.
    pub key_bytes: u32,
    /// Keys in one path.
    pub path: u32,
    /// Props of one node.
    pub props: u32,
    /// Rows or keys in one message.
    pub rows: u32,
    /// Messages in one batch.
    pub messages: u32,
}

impl Default for PatchLimits {
    fn default() -> Self {
        Self {
            nodes: 200_000,
            depth: 128,
            string_bytes: 8 << 20,
            key_bytes: 4096,
            path: 128,
            props: 128,
            rows: 8192,
            messages: 65_536,
        }
    }
}

/// The sizing and placement arguments every node takes (`fill`, `min: 12.rem`, `at`).
pub const NODE_ARGS: &[&str] = &[
    "fill", "min", "max", "aspect", "span", "lines", "truncate", "at",
];

/// Every primitive of the language a view may send: the layouts, controls and blocks the host
/// builds itself, and `slot`, a component's named slot. A primitive's props are the language's
/// arguments, which the host reads by name, each as the type it takes, ignoring what it does
/// not read; so what is checked of them is that the primitive is one the host builds and that
/// every value is bounded.
pub const PRIMITIVES: &[&str] = &[
    "stack",
    "column",
    "fragment",
    "primary",
    "secondary",
    "item",
    "row",
    "grid",
    "split",
    "page",
    "layers",
    "scroll",
    "spacer",
    "divider",
    "progress",
    "form",
    "field",
    "toggle",
    "choice",
    "list",
    "checkbox",
    "date",
    "image",
    "embed",
    "invocation",
    "error",
    "text",
    SLOT,
];

/// The kind of a node holding the content of a component's named slot.
pub const SLOT: &str = "slot";

/// The prefix of a described component's kind: `ui.ListRow`.
pub const COMPONENT_PREFIX: &str = "ui.";

/// Whether `kind` is a primitive the host builds.
#[must_use]
pub fn primitive(kind: &str) -> bool {
    PRIMITIVES.contains(&kind)
}

/// Checks batches against a toolkit's catalogue and a window's limits.
pub struct Validator<'a> {
    catalogue: &'a Catalogue,
    limits: PatchLimits,
}

impl<'a> Validator<'a> {
    #[must_use]
    pub fn new(catalogue: &'a Catalogue, limits: PatchLimits) -> Self {
        Self { catalogue, limits }
    }

    #[must_use]
    pub fn limits(&self) -> &PatchLimits {
        &self.limits
    }

    /// Check one batch an app sent its window.
    ///
    /// # Errors
    /// The first thing in it that is not valid.
    pub fn to_ui(&self, batch: &[ToUi]) -> Result<(), Refused> {
        if batch.len() > self.limits.messages as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "messages in a batch"
            )));
        }
        for message in batch {
            self.message(message)?;
        }
        Ok(())
    }

    fn message(&self, message: &ToUi) -> Result<(), Refused> {
        match message {
            ToUi::Loading
            | ToUi::Close
            | ToUi::Front(_)
            | ToUi::Ask(None)
            | ToUi::Form(None)
            | ToUi::Activate
            | ToUi::Took { .. } => Ok(()),
            ToUi::Open { target, tree, .. } => {
                self.key(&target.id)?;
                self.key(&target.ty)?;
                self.node(tree, 0)
            }
            ToUi::Patch { patches, .. } => {
                if patches.len() > self.limits.messages as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "patches in a message"
                    )));
                }
                patches.iter().try_for_each(|patch| self.patch(patch))
            }
            ToUi::Bound { key, value, .. } => {
                self.key(key)?;
                self.value(value, 0)
            }
            ToUi::Rows {
                list, keys, rows, ..
            } => {
                self.key(list)?;
                if keys.len() > self.limits.rows as usize || rows.len() > self.limits.rows as usize
                {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "rows in a message"
                    )));
                }
                keys.iter().try_for_each(|key| self.key(key))?;
                rows.iter().try_for_each(|row| self.node(row, 1))
            }
            ToUi::RowsChanged { list, changes, .. } => {
                self.key(list)?;
                if changes.len() > self.limits.rows as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "rows in a message"
                    )));
                }
                changes.iter().try_for_each(|change| match change {
                    RowChange::Inserted { key, .. } => self.key(key),
                    RowChange::Changed { row } => self.node(row, 1),
                    RowChange::Removed { .. } | RowChange::Moved { .. } | RowChange::Reset => {
                        Ok(())
                    }
                })
            }
            ToUi::Notice { words, .. } => self.words(words),
            ToUi::Ask(Some(ask)) => {
                self.string(&ask.action)?;
                self.string(&ask.invoker)
            }
            ToUi::Form(Some(form)) => {
                self.string(&form.title)?;
                self.string(&form.submit)?;
                if let Some(error) = &form.error {
                    self.string(error)?;
                }
                if let Some((_, key)) = &form.from {
                    self.key(key)?;
                }
                if form.fields.len() > self.limits.messages as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "fields in a form"
                    )));
                }
                form.fields.iter().try_for_each(|(key, text)| {
                    self.key(key)?;
                    self.string(text)
                })?;
                self.node(&form.tree, 1)
            }
        }
    }

    fn patch(&self, patch: &Patch) -> Result<(), Refused> {
        match patch {
            Patch::SetRoot(node) => self.node(node, 0),
            Patch::Insert { parent, node, .. } => {
                self.path(parent)?;
                self.node(node, parent.len().saturating_add(1))
            }
            Patch::Remove { parent, key } | Patch::Move { parent, key, .. } => {
                self.path(parent)?;
                self.key(key)
            }
            // A prop, binding or event set on its own is checked against its node's kind when
            // applied ([`Self::node`] of the node it changed).
            Patch::SetProp { path, name, value } => {
                self.path(path)?;
                self.key(name)?;
                value
                    .as_ref()
                    .map_or(Ok(()), |value| self.value(value, path.len()))
            }
            Patch::SetBinding { path, binding } => {
                self.path(path)?;
                binding
                    .as_ref()
                    .map_or(Ok(()), |binding| self.binding(binding))
            }
            Patch::SetEvent {
                path,
                event,
                action,
            } => {
                self.path(path)?;
                self.key(event)?;
                action.as_ref().map_or(Ok(()), |event| self.event(event))
            }
            Patch::SetAnchor { path, anchor } => {
                self.path(path)?;
                anchor.as_ref().map_or(Ok(()), |anchor| self.key(anchor))
            }
            Patch::SpliceProp {
                path, name, insert, ..
            } => {
                self.path(path)?;
                self.key(name)?;
                if insert.len() > self.limits.rows as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "items of a list value"
                    )));
                }
                insert
                    .iter()
                    .try_for_each(|item| self.value(item, path.len().saturating_add(1)))
            }
        }
    }

    /// Check `node`, at `depth`, and everything under it: its kind, and each prop, binding,
    /// event and child against it.
    ///
    /// # Errors
    /// The first part of it that is not valid.
    pub fn node(&self, node: &Node, depth: usize) -> Result<(), Refused> {
        if depth > self.limits.depth as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "tree depth"
            )));
        }
        self.key(&node.key)?;
        if node.props.len() > self.limits.props as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "props of a node"
            )));
        }
        if let Some(anchor) = &node.anchor {
            self.key(anchor)?;
        }
        self.shape(node, depth)
            .map_err(|refused| refused.within(format_args!("`{}` {}", node.kind, node.key)))?;
        node.children
            .iter()
            .try_for_each(|child| self.node(child, depth.saturating_add(1)))
    }

    /// Check the node's own parts, not its children, against its kind.
    ///
    /// # Errors
    /// A part of it its kind does not take.
    pub fn shape(&self, node: &Node, depth: usize) -> Result<(), Refused> {
        if let Some(name) = node.kind.strip_prefix(COMPONENT_PREFIX) {
            let description = self
                .catalogue
                .get(name)
                .ok_or_else(|| Refused::invalid("a component the toolkit does not describe"))?;
            return self.component(node, description, depth);
        }
        if !primitive(&node.kind) {
            return Err(Refused::invalid("a kind of node the host does not build"));
        }
        for (name, value) in &node.props {
            self.key(name)?;
            self.value(value, depth)
                .map_err(|refused| refused.within(format_args!("prop `{name}`")))?;
        }
        for (name, event) in &node.events {
            self.key(name)?;
            self.event(event)?;
        }
        node.binding
            .as_ref()
            .map_or(Ok(()), |binding| self.binding(binding))
    }

    fn component(
        &self,
        node: &Node,
        description: &Description,
        depth: usize,
    ) -> Result<(), Refused> {
        for (name, value) in &node.props {
            self.prop(name, value, description, depth)
                .map_err(|refused| refused.within(format_args!("prop `{name}`")))?;
        }
        self.parts(node, description)
    }

    fn prop(
        &self,
        name: &str,
        value: &Value,
        description: &Description,
        depth: usize,
    ) -> Result<(), Refused> {
        self.value(value, depth)?;
        {
            let declared = description
                .prop(name)
                .map(|prop| &prop.ty)
                .or_else(|| description.state(name).map(|state| &state.ty));
            match declared {
                Some(ty) => {
                    if !fits(value, ty) {
                        return Err(Refused::invalid(
                            "a prop holding a value of another type than its component declares",
                        ));
                    }
                }
                None if node_arg(name) => {}
                // Which binding a node binds, of a component with several.
                None if name == "bind" => {
                    let Value::Text(bound) = value else {
                        return Err(Refused::invalid("a binding named by other than text"));
                    };
                    if description.binding(bound).is_none() {
                        return Err(Refused::invalid("a binding its component does not declare"));
                    }
                }
                // What kind of text a text prop holds (`value:as` is `date` or `time`).
                None if name.strip_suffix(":as").is_some_and(|prop| {
                    description.prop(prop).is_some() || description.state(prop).is_some()
                }) => {}
                // A text field that is a document's body.
                None if name == "field_role" && !description.bindings.is_empty() => {}
                None => {
                    return Err(Refused::invalid("a prop its component does not declare"));
                }
            }
        }
        Ok(())
    }

    fn parts(&self, node: &Node, description: &Description) -> Result<(), Refused> {
        for (name, event) in &node.events {
            if description.event(name).is_none()
                && description.binding(name).is_none()
                && !offers(description, name)
            {
                return Err(Refused::invalid("an event its component does not declare")
                    .within(format_args!("event `{name}`")));
            }
            self.event(event)?;
        }
        if let Some(binding) = &node.binding {
            if description.bindings.is_empty() {
                return Err(Refused::invalid("a binding on a component that takes none"));
            }
            self.binding(binding)?;
        }
        Ok(())
    }

    fn binding(&self, binding: &Binding) -> Result<(), Refused> {
        self.string(&binding.name)?;
        if let Some((target, field)) = &binding.field {
            self.key(target)?;
            self.key(field)?;
        }
        Ok(())
    }

    fn event(&self, event: &Event) -> Result<(), Refused> {
        if let Some((target, field)) = &event.toggles {
            self.key(target)?;
            self.key(field)?;
        }
        if let Some((target, action)) = &event.invokes {
            self.key(target)?;
            self.key(action)?;
        }
        Ok(())
    }

    fn value(&self, value: &Value, depth: usize) -> Result<(), Refused> {
        if depth > self.limits.depth as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "value depth"
            )));
        }
        match value {
            Value::Unit | Value::Int(_) | Value::Bool(_) => Ok(()),
            Value::Float(number) if number.is_finite() => Ok(()),
            Value::Float(_) => Err(Refused::invalid("a number that is not finite")),
            Value::Text(text) | Value::Error(text) => self.string(text),
            Value::Enum(case) => self.key(case),
            Value::List(items) => {
                if items.len() > self.limits.rows as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "items of a list value"
                    )));
                }
                items
                    .iter()
                    .try_for_each(|item| self.value(item, depth.saturating_add(1)))
            }
            Value::Opt(held) => held
                .as_deref()
                .map_or(Ok(()), |held| self.value(held, depth.saturating_add(1))),
            Value::Ref(resource) => {
                self.key(&resource.id)?;
                self.key(&resource.ty)
            }
            Value::Principal(who) => {
                self.key(&who.id)?;
                self.string(&who.name)
            }
            Value::Map(entries) => {
                if entries.len() > self.limits.rows as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "entries of a map value"
                    )));
                }
                entries.iter().try_for_each(|(key, value)| {
                    self.value(key, depth.saturating_add(1))?;
                    self.value(value, depth.saturating_add(1))
                })
            }
            Value::Action(offered) => {
                self.string(&offered.label)?;
                self.key(&offered.event)?;
                offered.icon.as_ref().map_or(Ok(()), |icon| self.key(icon))
            }
            Value::Rich(rich) => {
                if rich.blocks.len() > self.limits.rows as usize
                    || rich.embeds.len() > self.limits.rows as usize
                {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "paragraphs of rich text"
                    )));
                }
                rich.strings().try_for_each(|text| self.string(text))
            }
        }
    }

    fn words(&self, words: &Words) -> Result<(), Refused> {
        match words {
            Words::Plain(text) => self.string(text),
            Words::Phrase { id, args } => {
                self.key(id)?;
                if args.len() > self.limits.props as usize {
                    return Err(Refused::invalid(concat!(
                        "over the limit of ",
                        "arguments of a phrase"
                    )));
                }
                args.iter().try_for_each(|(name, value)| {
                    self.key(name)?;
                    self.string(value)
                })
            }
        }
    }

    fn path(&self, path: &[String]) -> Result<(), Refused> {
        if path.len() > self.limits.path as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "path length"
            )));
        }
        path.iter().try_for_each(|key| self.key(key))
    }

    fn key(&self, key: &str) -> Result<(), Refused> {
        if key.len() > self.limits.key_bytes as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "key length"
            )));
        }
        Ok(())
    }

    fn resource(&self, resource: &ResourceRef) -> Result<(), Refused> {
        self.key(&resource.id)?;
        self.key(&resource.ty)
    }

    fn string(&self, text: &str) -> Result<(), Refused> {
        if text.len() > self.limits.string_bytes as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "string length"
            )));
        }
        Ok(())
    }

    /// Check one batch the host sent an app: in an app host, what the window says it did is
    /// as hostile as what an app sends its window.
    ///
    /// # Errors
    /// The first thing in it that is not valid.
    pub fn to_app(&self, batch: &[ToApp]) -> Result<(), Refused> {
        if batch.len() > self.limits.messages as usize {
            return Err(Refused::invalid(concat!(
                "over the limit of ",
                "messages in a batch"
            )));
        }
        for message in batch {
            match message {
                ToApp::Fire {
                    key, event, value, ..
                } => {
                    self.key(key)?;
                    self.key(event)?;
                    if let Some(value) = value {
                        self.value(value, 0)?;
                    }
                }
                ToApp::Toggle { target, key, .. } => {
                    self.resource(target)?;
                    self.key(key)?;
                }
                ToApp::Resolve { key, .. } => self.key(key)?,
                ToApp::Splice {
                    target,
                    key,
                    insert,
                    ..
                } => {
                    self.resource(target)?;
                    self.key(key)?;
                    self.string(insert)?;
                }
                ToApp::Edit {
                    target, key, text, ..
                } => {
                    self.resource(target)?;
                    self.key(key)?;
                    self.string(text)?;
                }
                ToApp::EditRich {
                    target, key, rich, ..
                } => {
                    self.resource(target)?;
                    self.key(key)?;
                    self.value(&Value::Rich(rich.clone()), 0)?;
                }
                ToApp::Open { resource } => self.resource(resource)?,
                ToApp::OpenReference { reference } => self.string(reference)?,
                ToApp::Rows {
                    list, start, end, ..
                } => {
                    self.key(list)?;
                    if end < start || end.saturating_sub(*start) > self.limits.rows {
                        return Err(Refused::invalid(concat!(
                            "over the limit of ",
                            "rows in a message"
                        )));
                    }
                }
                ToApp::Focus { at } => {
                    if let Some((_, key)) = at {
                        self.key(key)?;
                    }
                }
                ToApp::Names { me, others } => {
                    self.string(me)?;
                    if others.len() > self.limits.messages as usize {
                        return Err(Refused::invalid(concat!(
                            "over the limit of ",
                            "names in a message"
                        )));
                    }
                    for (id, name) in others {
                        self.key(id)?;
                        self.string(name)?;
                    }
                }
                ToApp::FormEdit { field, text, .. } => {
                    self.key(field)?;
                    self.string(text)?;
                }
                ToApp::Back
                | ToApp::Answer(_)
                | ToApp::Undo
                | ToApp::Composing(_)
                | ToApp::FormSubmit { .. }
                | ToApp::FormCancel { .. } => {}
            }
        }
        Ok(())
    }
}

/// The event an action offered in the prop `prop` fires: `actions.2` for the third of
/// `actions`. Its node gives it what the action runs, as it gives its declared events.
#[must_use]
pub fn offered_event(prop: &str, index: usize) -> String {
    format!("{prop}.{index}")
}

/// Whether `event` is one an action offered in a prop of the component fires
/// ([`offered_event`]).
fn offers(description: &Description, event: &str) -> bool {
    fn holds_actions(ty: &Type) -> bool {
        match ty {
            Type::Action => true,
            Type::Option(inner) | Type::List(inner) | Type::Set(inner) => holds_actions(inner),
            Type::Map { value, .. } => holds_actions(value),
            _ => false,
        }
    }
    event
        .rsplit_once('.')
        .filter(|(_, index)| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|(prop, _)| description.prop(prop))
        .is_some_and(|prop| holds_actions(&prop.ty))
}

/// Whether `name` is a sizing or placement argument every node takes, with its unit
/// (`min.rem`).
fn node_arg(name: &str) -> bool {
    let base = name
        .strip_suffix(".rem")
        .or_else(|| name.strip_suffix(".dp"))
        .unwrap_or(name);
    NODE_ARGS.contains(&base)
}

/// Whether `value` is one of the type `ty`, as the host reads it into a widget: text shows any
/// value as text, a whole number is a number, an `Option` holds nothing or a value.
#[must_use]
pub fn fits(value: &Value, ty: &Type) -> bool {
    match (ty, value) {
        (Type::Option(_), Value::Opt(None) | Value::Unit)
        | (Type::Int, Value::Int(_))
        | (Type::Float, Value::Float(_) | Value::Int(_))
        | (Type::Bool, Value::Bool(_) | Value::Int(_))
        | (Type::Principal, Value::Principal(_))
        | (Type::Resource, Value::Ref(_))
        | (Type::Icon | Type::Time, Value::Text(_))
        | (Type::RichText, Value::Rich(_))
        | (Type::Action, Value::Action(_)) => true,
        (Type::Option(inner), Value::Opt(Some(held))) => fits(held, inner),
        (Type::Option(inner), other) => fits(other, inner),
        (_, Value::Opt(Some(held))) => fits(held, ty),
        (
            Type::Text,
            Value::List(_) | Value::Ref(_) | Value::Rich(_) | Value::Map(_) | Value::Action(_),
        ) => false,
        (Type::Text, _) => true,
        (Type::Choice { cases, .. }, Value::Text(case) | Value::Enum(case)) => cases.contains(case),
        (Type::List(inner) | Type::Set(inner), Value::List(items)) => {
            items.iter().all(|item| fits(item, inner))
        }
        (Type::Map { key, value }, Value::Map(entries)) => {
            entries.iter().all(|(k, v)| fits(k, key) && fits(v, value))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::Lane;
    use super::*;

    fn validator() -> Validator<'static> {
        Validator::new(
            tilo_ui_graph::component::catalogue(),
            PatchLimits::default(),
        )
    }

    fn component(name: &str, key: &str, props: &[(&str, Value)]) -> Node {
        let mut node = Node::empty(&format!("{COMPONENT_PREFIX}{name}"), key);
        for (prop, value) in props {
            node.props.insert((*prop).to_owned(), value.clone());
        }
        node
    }

    fn open(tree: Node) -> Vec<ToUi> {
        vec![ToUi::Open {
            lane: Lane::Root,
            target: ResourceRef {
                id: "doc".to_owned(),
                ty: "notes.Notebook".to_owned(),
            },
            tree,
            front: None,
        }]
    }

    #[test]
    fn a_tree_of_described_components_and_primitives_passes() {
        let mut stack = Node::empty("stack", "s");
        stack.props.insert("gap".to_owned(), Value::text("section"));
        stack.props.insert("min.rem".to_owned(), Value::Float(12.0));
        let mut button = component("Button", "b", &[("label", Value::text("Send"))]);
        button.events.insert("press".to_owned(), Event::default());
        stack.children = vec![
            component(
                "Text",
                "t",
                &[
                    ("value", Value::text("2026-10-24")),
                    ("value:as", Value::text("date")),
                    ("role", Value::text("title")),
                ],
            ),
            button,
        ];
        assert_eq!(validator().to_ui(&open(stack)), Ok(()));
    }

    #[test]
    fn what_a_component_does_not_declare_is_refused() {
        let unknown = component("Nothing", "x", &[]);
        assert!(validator().to_ui(&open(unknown)).is_err());
        let prop = component("Text", "t", &[("color", Value::text("red"))]);
        assert!(validator().to_ui(&open(prop)).is_err());
        let typed = component("Text", "t", &[("role", Value::text("shouting"))]);
        assert!(
            validator().to_ui(&open(typed)).is_err(),
            "a case not declared"
        );
        let mut event = component("Text", "t", &[("value", Value::text("x"))]);
        event.events.insert("press".to_owned(), Event::default());
        assert!(validator().to_ui(&open(event)).is_err());
        assert!(
            validator()
                .to_ui(&open(Node::empty("script", "s")))
                .is_err()
        );
    }

    #[test]
    fn work_is_bounded() {
        let limits = PatchLimits {
            depth: 3,
            key_bytes: 16,
            ..PatchLimits::default()
        };
        let checked = Validator::new(tilo_ui_graph::component::catalogue(), limits);
        let mut deep = Node::empty("stack", "0");
        for level in 1..6 {
            deep = Node {
                children: vec![deep],
                ..Node::empty("stack", &level.to_string())
            };
        }
        assert_eq!(
            checked.to_ui(&open(deep)).map_err(|refused| refused.what),
            Err("over the limit of tree depth")
        );
        assert_eq!(
            checked
                .to_ui(&open(Node::empty(
                    "stack",
                    "a key far longer than sixteen bytes"
                )))
                .map_err(|refused| refused.what),
            Err("over the limit of key length")
        );
        assert!(
            validator()
                .to_ui(&open(component(
                    "Text",
                    "t",
                    &[("value", Value::Float(f64::NAN))]
                )))
                .is_err()
        );
    }
}

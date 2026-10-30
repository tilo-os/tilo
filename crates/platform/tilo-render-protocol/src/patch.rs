//! Component patches: how a tilolang app's window reaches the room's UI host, and what the
//! person did comes back.
//!
//! The scene protocol carries pixels an app's toolkit recorded. This one carries what a
//! tilolang app's views *are*: trees of the toolkit's described components
//! (`tilo_ui_graph::component`) and the language's layout primitives, keyed by stable keys,
//! and the changes the runtime planned to them. The UI host (the room's render service) holds
//! the widgets, lays them out, shapes text, draws and takes input; the app (its view
//! instances, in an app host) holds the data. So:
//!
//! - **The app sends its views** ([`ToUi`]): a lane's tree when it opens, the runtime's
//!   patches to it after, the rows of virtual lists the host asked for, what bound controls
//!   show, and what the window says (a notice, the Ask bar).
//! - **The host sends what the person did** ([`ToApp`]): events of components (an action
//!   reference fired by key), edits through bindings (text as splices, so concurrent edits
//!   merge), the rows a list shows now, focus and composition. Typing shows in the host's text
//!   field on the next frame; the app hears of it as a splice and never draws it.
//!
//! **Transport-agnostic.** A side sends at most one batch per frame: its messages, in order,
//! encoded as one byte string ([`encode_batch`], [`decode_batch`]). What carries the bytes is
//! the transport's: a socket between processes with a sealed memfd for a large batch
//! ([`unix`]), a `postMessage` between a page and a worker in the browser (the bytes are an
//! `ArrayBuffer` transferred, not copied), or a queue in one process ([`local`]). Nothing here
//! needs a socket, a thread or a clock, so both ends build for `wasm32`.
//!
//! **Hostile until validated.** A batch the UI host receives is decoded once into its own
//! memory and checked ([`validate`]) against the component descriptions and the window's
//! budget before any widget is touched; a batch that fails is refused whole and the window
//! shows the platform's error state, while the host and every other window keep running.
//! A connection is one window of one app: a patch can only name nodes of the trees that
//! window's app sent.

/// Rich text as a window shows it ([`Value::Rich`]).
pub use tilo_ui_graph::rich;

use serde::{Deserialize, Serialize};

use crate::WireError;

pub mod local;
#[cfg(target_os = "linux")]
pub mod unix;
pub mod validate;

/// The version both ends name. A newer end must still read this one's messages.
pub const VERSION: u16 = 1;

/// Whether a node of `kind` is a virtual list on the wire: a `ui.VirtualList`, whose rows the
/// app sends apart from the tree ([`ToUi::Rows`]) as the list shows them. A `list` in a view's
/// content is drawn whole (a card's is its preview), so its rows travel in the tree.
#[must_use]
pub fn is_virtual_list(kind: &str) -> bool {
    kind == "ui.VirtualList"
}

/// One window's two views: the app's root, and the view of the resource `open(r)` (or a
/// row) went to, shown after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Lane {
    Root,
    Detail,
}

/// A node of a view's tree: a described component (`ui.ListRow`), a primitive of the language
/// (`stack`, `row`, `list`), a `slot` holding a component's named slot, or a `fragment`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub kind: String,
    /// Stable for as long as the node shows the same thing: widgets are kept by key.
    pub key: String,
    pub props: std::collections::BTreeMap<String, Value>,
    pub binding: Option<Binding>,
    /// What each event does, by the event's name.
    pub events: std::collections::BTreeMap<String, Event>,
    /// The resource or part a link to this node names.
    pub anchor: Option<String>,
    pub children: Vec<Node>,
}

impl Node {
    /// A node of `kind` with `key` and nothing else.
    #[must_use]
    pub fn empty(kind: &str, key: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            key: key.to_owned(),
            ..Self::default()
        }
    }

    /// The node at `path` (keys from this node's children down), if there is one.
    #[must_use]
    pub fn at(&self, path: &[String]) -> Option<&Self> {
        let mut node = self;
        for key in path {
            node = node.children.iter().find(|child| child.key == *key)?;
        }
        Some(node)
    }

    /// The node at `path`, to change it.
    pub fn at_mut(&mut self, path: &[String]) -> Option<&mut Self> {
        let mut node = self;
        for key in path {
            node = node.children.iter_mut().find(|child| child.key == *key)?;
        }
        Some(node)
    }

    /// The node whose key is `key`, anywhere under this one (this one included); the first
    /// found, depth first.
    #[must_use]
    pub fn find(&self, key: &str) -> Option<&Self> {
        if self.key == key {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(key))
    }

    /// How many nodes the tree holds, this one included.
    #[must_use]
    pub fn count(&self) -> usize {
        1usize.saturating_add(self.children.iter().map(Self::count).sum::<usize>())
    }
}

/// A resource a node shows or an event carries: its id in the room, and its type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    pub id: String,
    pub ty: String,
}

impl ResourceRef {
    /// The id the resource is named by.
    #[must_use]
    pub fn id_string(&self) -> String {
        self.id.clone()
    }
}

/// A participant: their identity, and what the room calls them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub name: String,
}

/// A value a node holds: the language's values as a view shows them. Formatting (a date the
/// person's way, a number with their separators) is the host's, so values travel as they are.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
    List(Vec<Value>),
    Opt(Option<Box<Value>>),
    /// A case of an enum, by name.
    Enum(String),
    Ref(ResourceRef),
    Principal(Principal),
    /// Rich text, with how each reference in it reads for the person reading it.
    Rich(rich::RichText),
    /// What a failed or refused part of a view shows.
    Error(String),
    /// A map's entries, each key once, in key order.
    Map(Vec<(Value, Value)>),
    /// An action offered to the person: how a control shows it, and the event of its node
    /// that choosing it fires.
    Action(Offered),
}

/// An action a node offers ([`Value::Action`]), as the toolkit holds one.
pub use tilo_ui_graph::component::Offered;

impl Value {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    #[must_use]
    pub fn some(value: Self) -> Self {
        Self::Opt(Some(Box::new(value)))
    }
}

/// What a bound control edits, as the host needs to show and announce it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The resource and field it edits; `None` for the view's own state.
    pub field: Option<(String, String)>,
    /// What it is called, for assistive technology.
    pub name: String,
}

impl Binding {
    /// The resource (by id) and field it edits, when it edits one.
    #[must_use]
    pub fn field(&self) -> Option<(&str, &str)> {
        self.field
            .as_ref()
            .map(|(target, field)| (target.as_str(), field.as_str()))
    }
}

/// An event a node's view gave it. What it does is the app's: the host reports the event by
/// the node's key and name, and the app applies the action reference its view attached. The
/// host knows only, when that action toggles a field of a resource (`pin` toggles `pinned`),
/// which, so a header does not repeat a control that stands for it; and which action on which
/// resource it runs, so a menu the system completes does not offer it twice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub toggles: Option<(String, String)>,
    /// The resource (by id) and the action the event runs, when it runs an action on one.
    pub invokes: Option<(String, String)>,
}

/// One change of a lane's tree, against the tree the ones before it left. Paths are keys from
/// the root's children down; the root is the empty path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Patch {
    /// The root itself changed kind or key: the tree anew.
    SetRoot(Node),
    Insert {
        parent: Vec<String>,
        index: u32,
        node: Node,
    },
    Remove {
        parent: Vec<String>,
        key: String,
    },
    Move {
        parent: Vec<String>,
        key: String,
        to: u32,
    },
    SetProp {
        path: Vec<String>,
        name: String,
        value: Option<Value>,
    },
    SetBinding {
        path: Vec<String>,
        binding: Option<Binding>,
    },
    SetEvent {
        path: Vec<String>,
        event: String,
        action: Option<Event>,
    },
    SetAnchor {
        path: Vec<String>,
        anchor: Option<String>,
    },
    /// Part of a list prop replaced in place: `remove` items from `at` on taken out and
    /// `insert` put there. A long list (a collection's items) changes by what changed, never
    /// sent whole again.
    SpliceProp {
        path: Vec<String>,
        name: String,
        at: u32,
        remove: u32,
        insert: Vec<Value>,
    },
}

/// Why `apply` could not take a patch: it names a node the tree does not have.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a patch names a node the tree does not have")]
pub struct Unapplied;

impl Patch {
    /// Apply this patch to `root`.
    ///
    /// # Errors
    /// The patch names a node `root` does not have.
    pub fn apply(self, root: &mut Node) -> Result<(), Unapplied> {
        match self {
            Self::SetRoot(node) => *root = node,
            Self::Insert {
                parent,
                index,
                node,
            } => {
                let parent = root.at_mut(&parent).ok_or(Unapplied)?;
                let index = usize::try_from(index)
                    .unwrap_or(usize::MAX)
                    .min(parent.children.len());
                parent.children.insert(index, node);
            }
            Self::Remove { parent, key } => {
                let parent = root.at_mut(&parent).ok_or(Unapplied)?;
                let at = parent
                    .children
                    .iter()
                    .position(|child| child.key == key)
                    .ok_or(Unapplied)?;
                parent.children.remove(at);
            }
            Self::Move { parent, key, to } => {
                let parent = root.at_mut(&parent).ok_or(Unapplied)?;
                let at = parent
                    .children
                    .iter()
                    .position(|child| child.key == key)
                    .ok_or(Unapplied)?;
                let moved = parent.children.remove(at);
                let to = usize::try_from(to)
                    .unwrap_or(usize::MAX)
                    .min(parent.children.len());
                parent.children.insert(to, moved);
            }
            Self::SetProp { path, name, value } => {
                let node = root.at_mut(&path).ok_or(Unapplied)?;
                match value {
                    Some(value) => {
                        node.props.insert(name, value);
                    }
                    None => {
                        node.props.remove(&name);
                    }
                }
            }
            Self::SetBinding { path, binding } => {
                root.at_mut(&path).ok_or(Unapplied)?.binding = binding;
            }
            Self::SetEvent {
                path,
                event,
                action,
            } => {
                let node = root.at_mut(&path).ok_or(Unapplied)?;
                match action {
                    Some(action) => {
                        node.events.insert(event, action);
                    }
                    None => {
                        node.events.remove(&event);
                    }
                }
            }
            Self::SetAnchor { path, anchor } => {
                root.at_mut(&path).ok_or(Unapplied)?.anchor = anchor;
            }
            Self::SpliceProp {
                path,
                name,
                at,
                remove,
                insert,
            } => {
                let node = root.at_mut(&path).ok_or(Unapplied)?;
                let Some(Value::List(items)) = node.props.get_mut(&name) else {
                    return Err(Unapplied);
                };
                let at = usize::try_from(at).map_err(|_| Unapplied)?;
                let end = at
                    .checked_add(usize::try_from(remove).map_err(|_| Unapplied)?)
                    .filter(|end| *end <= items.len())
                    .ok_or(Unapplied)?;
                items.splice(at..end, insert);
            }
        }
        Ok(())
    }
}

/// A change of a virtual list's rows, against the rows the ones before it left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RowChange {
    Inserted {
        index: u32,
        key: String,
    },
    Removed {
        index: u32,
    },
    Moved {
        from: u32,
        to: u32,
    },
    /// A rendered row shows something else now.
    Changed {
        row: Node,
    },
    /// The list was replaced as a whole: its rows are read anew.
    Reset,
}

/// Words the window says: a phrase of the platform's catalogue (the host says it in the
/// person's language), or text the app made (an error, an action's label).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Words {
    Phrase {
        id: String,
        args: Vec<(String, String)>,
    },
    Plain(String),
}

impl Words {
    #[must_use]
    pub fn phrase(id: &str) -> Self {
        Self::Phrase {
            id: id.to_owned(),
            args: Vec::new(),
        }
    }
}

/// What kind of notice the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeKind {
    Info,
    Error,
}

/// What brings the window's content column to the front of a narrow window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Front {
    /// What the person did last: a click slides it in, a key lands it.
    Input,
    /// An agent, a script or a search hit: it lands at once.
    Action,
}

/// The Ask bar: trusted chrome naming who asks for what.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    /// The action, as its package names it.
    pub action: String,
    /// Who asks.
    pub invoker: String,
}

/// A person's answer to the Ask bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Allow,
    Decline,
    Always,
}

/// An action waiting for its arguments: the form the window shows for what the invocation still
/// needs. The app derives its fields from the action's parameters (each control from the
/// parameter's type), keeps what the person typed as their lens state, and checks each value as
/// the action takes it; the window presents the fields (a sheet over the window, a popover at the
/// control it came from, a sheet from the foot of a narrow window) with the title, submit and
/// cancel, and reports edits by field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Form {
    /// Which form: edits and answers name it, so ones for a form that closed are dropped.
    pub id: u64,
    /// The action, as its label names it ("Share…").
    pub title: String,
    /// The submit button's label: the action's label without its ellipsis ("Share").
    pub submit: String,
    /// The action takes something away: the submit button is destructive.
    pub destructive: bool,
    /// Every argument parses as its type: submit can be taken.
    pub ready: bool,
    /// Why the action refused the last submit, when that names no field.
    pub error: Option<String>,
    /// The control that ran the action, by lane and key: a popover form is anchored to it.
    pub from: Option<(Lane, String)>,
    /// Each field's control in `tree` by key, in order, with the text it holds now (what the
    /// person typed, else the default): the window shows it when the form opens.
    pub fields: Vec<(String, String)>,
    /// Every field is short (one line, a switch or a choice): a popover can hold them.
    pub short: bool,
    /// The fields, laid out: a tree of the same components and content nodes views send.
    pub tree: Node,
}

/// What an app sends its window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToUi {
    /// The window shows the platform's loading state until a lane opens.
    Loading,
    /// A lane opened (or opened anew): its view's tree, without the rows of its virtual lists,
    /// and the resource it shows. A detail opening with `front` comes to the front of a narrow
    /// window.
    Open {
        lane: Lane,
        target: ResourceRef,
        tree: Node,
        front: Option<Front>,
    },
    /// The detail lane closed.
    Close,
    /// The detail lane, already open, comes to the front.
    Front(Front),
    /// The runtime's patches to a lane's tree.
    Patch { lane: Lane, patches: Vec<Patch> },
    /// What the bound control `key` shows now. `acked` is the last of its splices the app took:
    /// the host keeps what the person typed after it.
    Bound {
        lane: Lane,
        key: String,
        value: Value,
        acked: u64,
    },
    /// Rows of the virtual list `list` (its key in the lane): how many it has, the keys of
    /// those from `start` on, and those of them rendered.
    Rows {
        lane: Lane,
        list: String,
        len: u32,
        start: u32,
        keys: Vec<String>,
        rows: Vec<Node>,
    },
    /// What changed of a virtual list's rows, and how many it has now.
    RowsChanged {
        lane: Lane,
        list: String,
        len: u32,
        changes: Vec<RowChange>,
    },
    Notice {
        words: Words,
        kind: NoticeKind,
        /// The notice offers Undo.
        undo: bool,
    },
    /// The Ask bar, or none.
    Ask(Option<Ask>),
    /// The form of an action waiting for its arguments, or none: it closed (submitted,
    /// cancelled, or the action ran).
    Form(Option<Box<Form>>),
    /// Someone asked to see the window ("Show this window"): it comes forward.
    Activate,
    /// The app ran the first `fired` presses the window sent (`ToApp::Fire`), and sent what
    /// they opened before this. What the person typed after a press goes to the app once it
    /// ran it, into the field focused then.
    Took { fired: u64 },
}

/// What the person did, as the window tells its app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToApp {
    /// The event `event` of the node `key` happened, carrying `value` when it carries one.
    Fire {
        lane: Lane,
        key: String,
        event: String,
        value: Option<Value>,
    },
    /// The checkbox or switch `key` of the view of `target` is now `checked`.
    ///
    /// An edit names the resource the lane showed when the person made it: it applies to that
    /// resource, whatever the lane shows by the time the app takes it.
    Toggle {
        lane: Lane,
        target: ResourceRef,
        key: String,
        checked: bool,
    },
    /// Text typed into the bound control `key` of the view of `target`: `delete` bytes at byte
    /// `at` replaced by `insert`, against what the control showed. `seq` counts the control's
    /// splices.
    Splice {
        lane: Lane,
        target: ResourceRef,
        key: String,
        seq: u64,
        at: u32,
        delete: u32,
        insert: String,
    },
    /// The bound control `key` of the view of `target`, which edits a value other than text (a
    /// number, a date), now holds `text`.
    Edit {
        lane: Lane,
        target: ResourceRef,
        key: String,
        text: String,
    },
    /// The bound rich-text control `key` of the view of `target` now holds `rich`: its
    /// paragraphs, marks and references (the runtime brings the field to it by splices and
    /// marks).
    EditRich {
        lane: Lane,
        target: ResourceRef,
        key: String,
        rich: rich::RichText,
    },
    /// The suggestion in the invocation card `key` was allowed or declined.
    Resolve {
        lane: Lane,
        key: String,
        approve: bool,
    },
    /// A component presenting `resource` was activated: show it.
    Open {
        resource: ResourceRef,
    },
    /// A reference in rich text was activated: open what it refers to, `reference` being the
    /// `mention` mark's value (`tilo_reference::embed`).
    OpenReference {
        reference: String,
    },
    /// Back from the detail lane.
    Back,
    /// The virtual list `list` shows the rows `start..end` now.
    Rows {
        lane: Lane,
        list: String,
        start: u32,
        end: u32,
    },
    Answer(Answer),
    /// The field `field` (a key of [`Form::fields`]) of the form `form` now holds `text`: what
    /// was typed, a case or a resource chosen by its name or id, `true` or `false`.
    FormEdit {
        form: u64,
        field: String,
        text: String,
    },
    /// The form `form` was submitted.
    FormSubmit {
        form: u64,
    },
    /// The form `form` was closed without submitting: what was typed stays for next time.
    FormCancel {
        form: u64,
    },
    /// The person asked to take back their last step: Undo on a notice, or the window's undo
    /// (Ctrl+Z, Edit › Undo). The record's undo, the one undo there is: a field the person typed
    /// into keeps no history of its own beside it.
    Undo,
    /// The keyboard focus is in the bound control `key` of `lane`, or in none.
    Focus {
        at: Option<(Lane, String)>,
    },
    /// An input method is composing in the focused control, or stopped.
    Composing(bool),
    /// What the window calls the room's participants now: `me` for the person at the window,
    /// and each of `others` by id. An app whose instance runs out of the window's process names
    /// people only by these.
    Names {
        me: String,
        others: Vec<(String, String)>,
    },
    /// The view of `lane` has `width` by `height` of the window, in `rem` (the body text size
    /// after the person's setting): what its `space` reads (layout.md, "Responding to space").
    Space {
        lane: Lane,
        width: f32,
        height: f32,
    },
    /// The person asked to bring back what their last undo took back: the window's redo
    /// (Ctrl+Shift+Z, Edit › Redo). The mirror of [`ToApp::Undo`].
    Redo,
}

/// Largest batch either side sends or accepts.
pub const MAX_BATCH_BYTES: usize = 16 << 20;

/// One frame's messages as the bytes a transport carries.
///
/// # Errors
/// The messages encode to more than [`MAX_BATCH_BYTES`].
pub fn encode_batch<T: Serialize>(messages: &[T]) -> Result<Vec<u8>, WireError> {
    let mut bytes = Vec::with_capacity(64);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    let bytes = postcard::to_extend(messages, bytes).map_err(|_| WireError::Malformed("batch"))?;
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(WireError::OverLimit("batch size"));
    }
    Ok(bytes)
}

/// The messages of a batch, read once into the receiver's memory.
///
/// # Errors
/// The bytes are no batch of this version, or are over [`MAX_BATCH_BYTES`].
pub fn decode_batch<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, WireError> {
    if bytes.len() > MAX_BATCH_BYTES {
        return Err(WireError::OverLimit("batch size"));
    }
    let (version, body) = bytes.split_at_checked(2).ok_or(WireError::Truncated)?;
    if version != VERSION.to_le_bytes() {
        return Err(WireError::Malformed("batch version"));
    }
    let (messages, rest) =
        postcard::take_from_bytes(body).map_err(|_| WireError::Malformed("batch"))?;
    if !rest.is_empty() {
        return Err(WireError::Malformed("bytes after a batch"));
    }
    Ok(messages)
}

/// What carries one side's batches to the other: a queue in one process, a socket and ring
/// between processes, `postMessage` between a page and a worker. At most one batch a frame.
pub trait Transport {
    /// Hand `batch` to the other side.
    ///
    /// # Errors
    /// The other side is gone.
    fn send(&mut self, batch: Vec<u8>) -> Result<(), WireError>;

    /// The next batch the other side sent, when one waits.
    ///
    /// # Errors
    /// The other side is gone, or what arrived is no batch.
    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(key: &str, value: &str) -> Node {
        let mut node = Node::empty("ui.Text", key);
        node.props.insert("value".to_owned(), Value::text(value));
        node
    }

    #[test]
    fn patches_apply_by_key_and_refuse_a_node_the_tree_lacks() {
        let mut root = Node::empty("stack", "s");
        root.children = vec![text("a", "1"), text("b", "2")];
        Patch::Insert {
            parent: Vec::new(),
            index: 0,
            node: text("n", "0"),
        }
        .apply(&mut root)
        .expect("applies");
        Patch::Move {
            parent: Vec::new(),
            key: "b".to_owned(),
            to: 1,
        }
        .apply(&mut root)
        .expect("applies");
        Patch::SetProp {
            path: vec!["a".to_owned()],
            name: "value".to_owned(),
            value: Some(Value::text("3")),
        }
        .apply(&mut root)
        .expect("applies");
        let keys: Vec<&str> = root.children.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, ["n", "b", "a"]);
        assert_eq!(
            root.find("a").and_then(|a| a.props.get("value")),
            Some(&Value::text("3"))
        );
        assert_eq!(
            Patch::Remove {
                parent: vec!["nowhere".to_owned()],
                key: "a".to_owned()
            }
            .apply(&mut root),
            Err(Unapplied)
        );
    }

    #[test]
    fn a_batch_reads_back_as_it_was_sent_and_scribbles_are_refused() {
        let batch = vec![
            ToUi::Open {
                lane: Lane::Root,
                target: ResourceRef {
                    id: "doc".to_owned(),
                    ty: "notes.Notebook".to_owned(),
                },
                tree: text("t", "Notes"),
                front: None,
            },
            ToUi::Patch {
                lane: Lane::Root,
                patches: vec![Patch::SetProp {
                    path: Vec::new(),
                    name: "value".to_owned(),
                    value: Some(Value::text("Ideas")),
                }],
            },
        ];
        let bytes = encode_batch(&batch).expect("encodes");
        let back: Vec<ToUi> = decode_batch(&bytes).expect("decodes");
        assert_eq!(back, batch);
        for cut in 0..bytes.len() {
            let truncated: Result<Vec<ToUi>, _> = decode_batch(&bytes[..cut]);
            assert!(truncated.is_err() || cut == bytes.len());
        }
        let mut other = bytes;
        other[0] = 9;
        assert!(decode_batch::<ToUi>(&other).is_err(), "another version");
    }

    #[test]
    fn rich_text_reads_back_as_it_was_sent_with_or_without_its_parts() {
        let reference = r#"{"app":"a.b","id":"t1","kind":"a.task"}"#;
        let rich = |marks: Vec<rich::Mark>, embeds: Vec<rich::Embed>| rich::RichText {
            blocks: vec![rich::Block {
                kind: "paragraph".to_owned(),
                text: "see Fix it".to_owned(),
                marks,
                attrs: std::collections::BTreeMap::new(),
            }],
            embeds,
        };
        let batch: Vec<ToUi> = [
            rich(Vec::new(), Vec::new()),
            rich(
                vec![rich::Mark {
                    start: 4,
                    end: 10,
                    name: "mention".to_owned(),
                    value: rich::Attr::Text(reference.to_owned()),
                }],
                vec![
                    rich::Embed {
                        reference: reference.to_owned(),
                        view: rich::InlineView::Resource {
                            title: "Fix the build".to_owned(),
                            icon: None,
                            detail: None,
                        },
                    },
                    rich::Embed {
                        reference: "u-ada".to_owned(),
                        view: rich::InlineView::Unreadable,
                    },
                ],
            ),
        ]
        .into_iter()
        .map(|value| ToUi::Patch {
            lane: Lane::Root,
            patches: vec![Patch::SetProp {
                path: Vec::new(),
                name: "value".to_owned(),
                value: Some(Value::Rich(value)),
            }],
        })
        .collect();
        let bytes = encode_batch(&batch).expect("encodes");
        assert_eq!(decode_batch::<ToUi>(&bytes).expect("decodes"), batch);
    }

    #[test]
    fn a_rich_inputs_edit_round_trips_empty_or_not() {
        let plain = rich::RichText {
            blocks: vec![rich::Block {
                kind: "paragraph".into(),
                text: "hi".into(),
                ..rich::Block::default()
            }],
            embeds: Vec::new(),
        };
        for rich in [rich::RichText::default(), plain] {
            let edit = vec![ToApp::EditRich {
                lane: Lane::Root,
                target: ResourceRef {
                    id: "n1".into(),
                    ty: "Note".into(),
                },
                key: "draft".into(),
                rich,
            }];
            let bytes = encode_batch(&edit).expect("encodes");
            assert_eq!(decode_batch::<ToApp>(&bytes).expect("decodes"), edit);
        }
    }

    #[test]
    fn maps_offered_actions_and_list_splices_round_trip_and_apply() {
        let who = |id: &str| {
            Value::Principal(Principal {
                id: id.to_owned(),
                name: id.to_owned(),
            })
        };
        let reactions = Value::Map(vec![
            (Value::text("👍"), Value::List(vec![who("ada"), who("leo")])),
            (Value::text("🎉"), Value::List(vec![who("mia")])),
        ]);
        let offered = Value::List(vec![Value::Action(Offered {
            label: "Pin".to_owned(),
            icon: Some("pin".to_owned()),
            on: Some(false),
            event: "actions.0".to_owned(),
            ..Offered::default()
        })]);
        let item = |id: &str| {
            Value::Ref(ResourceRef {
                id: id.to_owned(),
                ty: "chat.Message".to_owned(),
            })
        };
        let mut node = Node::empty("ui.Reactions", "r");
        node.props.insert("reactions".to_owned(), reactions);
        node.props.insert("actions".to_owned(), offered);
        node.props.insert(
            "items".to_owned(),
            Value::List(vec![item("m1"), item("m3")]),
        );
        let splice = Patch::SpliceProp {
            path: Vec::new(),
            name: "items".to_owned(),
            at: 1,
            remove: 0,
            insert: vec![item("m2")],
        };
        let batch = vec![
            ToUi::Open {
                lane: Lane::Root,
                target: ResourceRef {
                    id: "c".to_owned(),
                    ty: "chat.Conversation".to_owned(),
                },
                tree: node.clone(),
                front: None,
            },
            ToUi::Patch {
                lane: Lane::Root,
                patches: vec![splice.clone()],
            },
        ];
        let bytes = encode_batch(&batch).expect("encodes");
        assert_eq!(decode_batch::<ToUi>(&bytes).expect("decodes"), batch);
        splice.apply(&mut node).expect("applies");
        assert_eq!(
            node.props.get("items"),
            Some(&Value::List(vec![item("m1"), item("m2"), item("m3")]))
        );
        let past_the_end = Patch::SpliceProp {
            path: Vec::new(),
            name: "items".to_owned(),
            at: 3,
            remove: 1,
            insert: Vec::new(),
        };
        assert_eq!(past_the_end.apply(&mut node), Err(Unapplied));
        let not_a_list = Patch::SpliceProp {
            path: Vec::new(),
            name: "reactions".to_owned(),
            at: 0,
            remove: 0,
            insert: Vec::new(),
        };
        assert_eq!(not_a_list.apply(&mut node), Err(Unapplied));
    }
}

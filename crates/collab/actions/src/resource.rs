//! The nested **Resource** tree an app publishes, and the **ActionSpec**s each
//! resource exports.
//!
//! A [`Resource`] is something the app currently has *open / presented* — a
//! playing video, an open book — and resources **nest**: a video owns its
//! comments; a book owns its annotations. Each resource exports
//! [`ActionSpec`]s: the operations Ctrl+K offers for it.
//!
//! The split that matters: a [`Resource`]/[`ActionSpec`] is **presentation** —
//! a local, per-frame description with human labels and (for a prompted action)
//! a closure. The [`EncodedAction`] an `ActionSpec` yields is the **action** —
//! the serializable, replayable value. The presentation never crosses a
//! boundary; only the action does.

use std::sync::Arc;

use crate::action::{Action, ActionError, EncodedAction, ExecutionLocus};

/// Stable identity of a resource within the publishing app instance (e.g.
/// `tube.video:dQw4w9WgXcQ`, `ebook.annotation:<uuid>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResourceId(pub String);

impl ResourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One open/presented resource and the actions it exports. Resources nest via
/// [`children`](Resource::children).
#[derive(Clone)]
pub struct Resource {
    pub id: ResourceId,
    /// A stable kind tag (e.g. `"tube.video"`, `"tube.comment"`,
    /// `"ebook.book"`, `"ebook.annotation"`). Shared with the global-search
    /// type registry so the same kind can be enumerated OS-wide.
    pub kind: String,
    /// Human, localisable label shown in the palette.
    pub label: String,
    pub children: Vec<Resource>,
    pub actions: Vec<ActionSpec>,
}

impl Resource {
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            id: ResourceId::new(id),
            kind: kind.into(),
            label: label.into(),
            children: Vec::new(),
            actions: Vec::new(),
        }
    }

    pub fn with_action(mut self, action: ActionSpec) -> Self {
        self.actions.push(action);
        self
    }

    pub fn with_child(mut self, child: Resource) -> Self {
        self.children.push(child);
        self
    }

    pub fn with_children(mut self, children: impl IntoIterator<Item = Resource>) -> Self {
        self.children.extend(children);
        self
    }
}

/// What kind of thing an action's one text argument names.
///
/// Both arrive as strings; what differs is where a usable one can come from.
/// Words are what the asker says outright, so an instruction asking for them
/// contains them. A content hash addresses bytes already in this machine's blob
/// store — nobody types one and nothing can invent one, so it exists only
/// because some earlier operation produced the content it names.
///
/// Declared on the spec rather than inferred from the prompt, because it is
/// what lets a caller that cannot see the screen tell "give me a new name" from
/// "give me the image to place".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Collects {
    /// Free text, in the words of whoever is asking.
    #[default]
    Words,
    /// The content hash of bytes already in this machine's blob store.
    Content,
}

/// How an [`ActionSpec`] produces its replayable [`EncodedAction`] when chosen.
#[derive(Clone)]
pub enum ActionInput {
    /// Fires immediately with this pre-encoded action.
    Immediate(EncodedAction),
    /// Collects one free-text argument first (the palette shows an argument
    /// step), then builds the action from it. The closure is presentation-side
    /// state — it never crosses a boundary; only its output [`EncodedAction`]
    /// does.
    Prompted {
        prompt: String,
        placeholder: String,
        /// What that argument names. See [`Collects`].
        collects: Collects,
        build: Arc<dyn Fn(&str) -> EncodedAction + Send + Sync>,
    },
}

/// One action a resource exports to the palette.
#[derive(Clone)]
pub struct ActionSpec {
    /// Stable id within the resource (e.g. `"like"`, `"reply"`).
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// Declared execution locus (mirrors the concrete action's
    /// [`Action::locus`], so the palette can show / route it before the action
    /// is even built).
    pub locus: ExecutionLocus,
    pub input: ActionInput,
}

impl ActionSpec {
    /// An action that fires immediately. `type_id` must match the app's
    /// registry registration for `A`.
    pub fn immediate<A>(
        id: impl Into<String>,
        label: impl Into<String>,
        type_id: impl Into<String>,
        action: &A,
    ) -> Result<Self, ActionError>
    where
        A: Action + serde::Serialize,
    {
        let locus = action.locus();
        Ok(Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            input: ActionInput::Immediate(EncodedAction::encode(type_id, action)?),
        })
    }

    /// An action that first collects a free-text argument, then builds its
    /// encoded action from it.
    pub fn prompted(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            locus,
            input: ActionInput::Prompted {
                prompt: prompt.into(),
                placeholder: placeholder.into(),
                collects: Collects::Words,
                build: Arc::new(build),
            },
        }
    }

    /// An action whose one argument is the content hash of bytes this machine
    /// already holds, rather than words somebody types.
    ///
    /// A separate constructor rather than a parameter on [`Self::prompted`], so
    /// every action that already exists keeps saying exactly what it said: an
    /// argument is words unless the app declares otherwise.
    pub fn prompted_content(
        id: impl Into<String>,
        label: impl Into<String>,
        prompt: impl Into<String>,
        placeholder: impl Into<String>,
        locus: ExecutionLocus,
        build: impl Fn(&str) -> EncodedAction + Send + Sync + 'static,
    ) -> Self {
        let mut spec = Self::prompted(id, label, prompt, placeholder, locus, build);
        if let ActionInput::Prompted { collects, .. } = &mut spec.input {
            *collects = Collects::Content;
        }
        spec
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// An app instance implements this to publish the resource tree it currently
/// has open. The host (`ui::App`) collects the focused instance's tree each
/// frame; the CRDT `doc` is passed so a provider can enumerate resources that
/// live *in* the document (e.g. a book's annotations).
pub trait ResourceProvider {
    fn resources(&self, doc: &automerge::AutoCommit) -> Vec<Resource>;
}

/// Field separator inside a flattened composite key. A unit-separator control
/// char never appears in a resource or action id, so splitting is unambiguous.
pub const KEY_SEP: char = '\u{1f}';

/// One (resource, action) pair produced by [`flatten`], addressable by a
/// composite [`key`](FlatAction::key) the palette uses as its row id.
pub struct FlatAction<'a> {
    /// `"<resource_id>\u{1f}<action_id>"` — the palette row id.
    pub key: String,
    pub resource_id: ResourceId,
    pub resource_kind: String,
    pub resource_label: String,
    pub action: &'a ActionSpec,
}

/// Depth-first flatten of a resource forest into every exported action, parent
/// resources before their children. This is what the palette lists.
pub fn flatten(resources: &[Resource]) -> Vec<FlatAction<'_>> {
    let mut out = Vec::new();
    for r in resources {
        flatten_into(r, &mut out);
    }
    out
}

fn flatten_into<'a>(resource: &'a Resource, out: &mut Vec<FlatAction<'a>>) {
    for action in &resource.actions {
        out.push(FlatAction {
            key: format!("{}{}{}", resource.id.0, KEY_SEP, action.id),
            resource_id: resource.id.clone(),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action,
        });
    }
    for child in &resource.children {
        flatten_into(child, out);
    }
}

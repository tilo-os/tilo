//! A resource an app publishes: its identity, what it holds and how it nests.

use crate::action::OperationKind;
use crate::api::ActionAddress;

use super::{
    ActionSpec, ResourceId, ResourcePresentation, ResourceSemanticIdentity, ResourceType,
    ResourceViewSpec, listed_for_people,
};

/// One open/presented resource and the actions it exports. Resources
/// nest via [`children`](ResourceSpec::children).
pub struct ResourceSpec {
    pub id: ResourceId,
    /// A stable kind tag (e.g. `"tube.video"`, `"ebook.annotation"`),
    /// shared with the global-search type registry for OS-wide enumeration.
    pub kind: String,
    /// Human, localisable label shown in the palette.
    pub label: String,
    /// Optional shell-rendered metadata used by system search and resource
    /// browsers. Invalid or oversized values are omitted at the boundary.
    pub presentation: Option<ResourcePresentation>,
    /// Exact original typed identity retained before host namespacing.
    pub semantic_identity: Option<ResourceSemanticIdentity>,
    /// The app that publishes it, as the [`ActionKey`](crate::ActionKey)s
    /// of its actions name it: the one whose resource this is, not the
    /// provider keeping it in a workspace. `None` for a resource published
    /// outside an app.
    pub app_id: Option<String>,
    pub children: Vec<ResourceSpec>,
    pub actions: Vec<ActionSpec>,
    /// Explicit presentations of this resource. Actions remain invocation
    /// mechanisms; only entries in this collection are resource views.
    pub views: Vec<ResourceViewSpec>,
    /// Whether the reading is the person's own rather than everyone's: their clipboard, their
    /// notifications. Only a program the session runs itself reads it; an installed app reads
    /// what its own records hold.
    pub personal: bool,
    /// The workspace that holds the record, when it is not the one the window publishing it is
    /// open in: a person's profile record, held in their own workspace. That workspace decides
    /// the record's changes; `None` is the window's.
    pub workspace: Option<String>,
    /// What it is, as people name it, when its type says ([`ResourceType`]).
    pub resource_type: Option<ResourceType>,
}

/// Written out rather than derived: a derived `clone` is inlined into every
/// app that copies a resource, this one is compiled once.
impl Clone for ResourceSpec {
    #[inline(never)]
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            kind: self.kind.clone(),
            label: self.label.clone(),
            presentation: self.presentation.clone(),
            semantic_identity: self.semantic_identity.clone(),
            app_id: self.app_id.clone(),
            children: self.children.clone(),
            actions: self.actions.clone(),
            views: self.views.clone(),
            personal: self.personal,
            workspace: self.workspace.clone(),
            resource_type: self.resource_type.clone(),
        }
    }
}

impl ResourceSpec {
    /// A resource with a stable `id`, a `kind` and a human `label`.
    /// Framework-internal: a declaration builds a resource's tree, never an
    /// app.
    #[doc(hidden)]
    pub fn new(id: impl Into<String>, kind: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: ResourceId::new(id),
            kind: kind.into(),
            label: label.into(),
            presentation: None,
            resource_type: None,
            semantic_identity: None,
            app_id: None,
            children: Vec::new(),
            actions: Vec::new(),
            views: Vec::new(),
            personal: false,
            workspace: None,
        }
    }

    /// The app that publishes it, as an [`ActionKey`](crate::ActionKey) of
    /// one of its actions names it ([`Self::app_id`]).
    #[must_use]
    pub fn app(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    /// The resource's own id, not the transport alias a window published it
    /// under ([`Self::semantic_identity`]): what its app and the directory
    /// naming it offer it at, whatever alias the broker delivers it under.
    #[must_use]
    pub fn domain_name(&self) -> &str {
        match &self.semantic_identity {
            Some(identity) if identity.is_valid() => identity.key.as_str(),
            _ => self.id.as_str(),
        }
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: ResourcePresentation) -> Self {
        self.presentation = presentation.is_valid().then_some(presentation);
        self
    }

    /// Saying what it is, as people name it.
    #[must_use]
    pub fn with_type(mut self, resource_type: ResourceType) -> Self {
        self.resource_type = Some(resource_type);
        self
    }

    #[must_use]
    pub fn with_action(mut self, action: ActionSpec) -> Self {
        self.actions.push(action);
        self
    }

    #[must_use]
    pub fn with_view(mut self, view: ResourceViewSpec) -> Self {
        self.views.push(view);
        self
    }

    /// Publish this resource's reading to the session's own programs alone: what the person's state
    /// needs, when it is theirs and not an app's.
    #[must_use]
    pub fn with_personal_reading(mut self) -> Self {
        self.personal = true;
        self
    }

    /// The record is held in `workspace` rather than in the window's ([`Self::workspace`]).
    #[must_use]
    pub fn held_in(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Keep the actions `keep` admits, and only the views still opened by
    /// one of them: a view whose open action is not offered cannot be
    /// offered either, and the action contract refuses the whole surface
    /// that carries one.
    pub fn retain_actions(&mut self, keep: impl FnMut(&ActionSpec) -> bool) {
        self.actions.retain(keep);
        let actions = &self.actions;
        self.views
            .retain(|view| actions.iter().any(|action| action.id == view.open_action));
    }

    #[must_use]
    pub fn with_child(mut self, child: ResourceSpec) -> Self {
        self.children.push(child);
        self
    }

    /// Whether a palette may list it: a titled resource that offers a
    /// person at least one action of its own. An untitled node, a control
    /// mirror, and a node left with only agent actions are all addressing
    /// machinery, and a list of what a person may act on never shows them.
    #[must_use]
    pub fn listed_for_people(&self) -> bool {
        listed_for_people(&self.kind, &self.label)
            && self
                .actions
                .iter()
                .any(|action| action.contract.offered_to_people())
    }

    /// How many of its actions a person may take.
    #[must_use]
    pub fn person_action_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| action.contract.offered_to_people())
            .count()
    }

    /// The action that presents it, when a person may take it: what a
    /// palette lists the resource for.
    #[must_use]
    pub fn open_action(&self) -> Option<&ActionSpec> {
        self.actions
            .iter()
            .filter(|action| action.contract.offered_to_people())
            .find(|action| action.kind == OperationKind::View)
            .or_else(|| {
                self.actions
                    .iter()
                    .find(|action| action.contract.offered_to_people())
            })
    }

    #[must_use]
    pub fn with_children(mut self, children: impl IntoIterator<Item = ResourceSpec>) -> Self {
        self.children.extend(children);
        self
    }

    /// The address of the action `action` this resource publishes, or `None`
    /// when it publishes none by that id. A caller holding a published
    /// resource (a survey's, a search hit's, a bound tree's) addresses its
    /// actions from it rather than writing an address of its own, so an
    /// action the resource stopped publishing fails here, at the caller.
    #[must_use]
    pub fn action_address(&self, action: &str) -> Option<ActionAddress> {
        self.actions
            .iter()
            .any(|spec| spec.id == action)
            .then(|| ActionAddress::new(self.id.as_str(), action))
    }
}

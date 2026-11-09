//! What an application publishes of its actions: the inert, typed shape one
//! action has on the wire, and what its invocation must carry.
//!
//! One shape holds an action: the application builds it from its typed
//! [`ActionDescriptor`](crate::ActionDescriptor), the wire carries it as this,
//! and the broker admits it as this.

use crate::{
    ActionAddress, ActionContract, ActionDescriptor, ActionParam, Collects, ExecutionLocus,
    InputSpec, OperationKind, ResourceSemanticIdentity, ResourceViewDescriptor,
};

/// What one published action's invocation must carry: the transport-side
/// twin of [`InputSpec`], which says what kind of text it collects.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PublishedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        /// Whether that text names content already in this machine's store
        /// rather than words somebody says.
        wants_content: bool,
    },
}

impl From<&InputSpec> for PublishedInput {
    fn from(input: &InputSpec) -> Self {
        match input {
            InputSpec::None => Self::None,
            InputSpec::Text {
                prompt,
                placeholder,
                collects,
            } => Self::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                wants_content: *collects == Collects::Content,
            },
        }
    }
}

impl From<&PublishedInput> for InputSpec {
    fn from(input: &PublishedInput) -> Self {
        match input {
            PublishedInput::None => Self::None,
            PublishedInput::Text {
                prompt,
                placeholder,
                wants_content,
            } => Self::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: if *wants_content {
                    Collects::Content
                } else {
                    Collects::Words
                },
            },
        }
    }
}

/// One inert operation an application publishes, without execution authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublishedAction {
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_identity: Option<ResourceSemanticIdentity>,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    /// The other words people use for the action, published with its label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The action's own picture, by its Lucide name, published with its label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub description: Option<String>,
    /// What the action's row shows of the state it acts on.
    #[serde(default, skip_serializing_if = "crate::ActionState::is_empty")]
    pub state: crate::ActionState,
    pub operation_kind: OperationKind,
    /// The canonical typed parameter descriptors.
    pub params: Vec<ActionParam>,
    /// The complete safety/lifecycle/result contract.
    pub contract: ActionContract,
    pub view: Option<ResourceViewDescriptor>,
    pub input: PublishedInput,
}

impl PublishedAction {
    /// Whether this action, read from the app that published it, names a
    /// semantic identity only of its own kind and in bounds.
    pub fn check(&self) -> Result<(), String> {
        if self.semantic_identity.as_ref().is_some_and(|identity| {
            identity.kind.len() > ResourceSemanticIdentity::MAX_KIND_BYTES
                || identity.key.len() > ResourceSemanticIdentity::MAX_KEY_BYTES
                || !identity.is_valid()
                || identity.kind != self.resource_kind
        }) {
            return Err(format!(
                "action `{}`/`{}`: invalid resource semantic identity",
                self.resource, self.action
            ));
        }
        Ok(())
    }

    /// The typed descriptor this action publishes, run at `locus`: the wire
    /// carries no locus of its own.
    pub fn descriptor(&self, locus: ExecutionLocus) -> ActionDescriptor {
        ActionDescriptor {
            semantic_identity: self.semantic_identity.clone(),
            address: ActionAddress::new(&self.resource, &self.action),
            resource_kind: self.resource_kind.clone(),
            resource_label: self.resource_label.clone(),
            action_label: self.action_label.clone(),
            keywords: self.keywords.clone(),
            icon: self.icon.clone(),
            description: self.description.clone(),
            state: self.state.clone(),
            locus,
            kind: self.operation_kind,
            contract: self.contract.clone(),
            input: (&self.input).into(),
            params: self.params.clone(),
            view: self.view.clone(),
        }
    }
}

impl From<&ActionDescriptor> for PublishedAction {
    fn from(descriptor: &ActionDescriptor) -> Self {
        Self {
            resource: descriptor.address.resource.clone(),
            semantic_identity: descriptor.semantic_identity.clone(),
            action: descriptor.address.action.clone(),
            resource_kind: descriptor.resource_kind.clone(),
            resource_label: descriptor.resource_label.clone(),
            action_label: descriptor.action_label.clone(),
            keywords: descriptor.keywords.clone(),
            icon: descriptor.icon.clone(),
            description: descriptor.description.clone(),
            state: descriptor.state.clone(),
            operation_kind: descriptor.kind,
            params: descriptor.params.clone(),
            contract: descriptor.contract.clone(),
            view: descriptor.view.clone(),
            input: (&descriptor.input).into(),
        }
    }
}

/// The fields of a resource every one of its actions carries.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceFields {
    pub resource_kind: String,
    pub resource_label: String,
    pub semantic_identity: Option<ResourceSemanticIdentity>,
}

impl PublishedAction {
    /// The catalogue entry of this action: everything but its resource's own
    /// fields, which a [`ResourceFields`] carries.
    #[must_use]
    pub fn shape(&self) -> Self {
        Self {
            resource: String::new(),
            semantic_identity: None,
            resource_kind: String::new(),
            resource_label: String::new(),
            ..self.clone()
        }
    }

    #[must_use]
    pub fn resource_fields(&self) -> ResourceFields {
        ResourceFields {
            resource_kind: self.resource_kind.clone(),
            resource_label: self.resource_label.clone(),
            semantic_identity: self.semantic_identity.clone(),
        }
    }
}

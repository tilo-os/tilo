//! Pure, versioned schema and structural editor for Tilo serialized UI graphs.

mod edit;

pub use edit::{UiGraphEdit, parse_ui_graph_edits};

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const UI_GRAPH_VERSION: u32 = 1;
const MAX_GRAPH_DEPTH: usize = 64;
const MAX_GRAPH_NODES: usize = 4096;
const MAX_ID_LEN: usize = 128;
const MAX_BINDING_LEN: usize = 512;
const MAX_COMPONENT_NAME_LEN: usize = 128;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDocument {
    pub version: u32,
    pub root: UiNode,
}

impl UiDocument {
    #[must_use]
    pub fn new(root: UiNode) -> Self {
        Self {
            version: UI_GRAPH_VERSION,
            root,
        }
    }

    /// Parse and validate a graph before it can reach the widget tree.
    ///
    /// # Errors
    ///
    /// Returns a JSON decoding or graph-validation error.
    pub fn from_json(source: &str) -> Result<Self, UiGraphError> {
        let document: Self = serde_json::from_str(source)?;
        document.validate()?;
        Ok(document)
    }

    /// Serialize the validated graph in a stable, human-readable form.
    ///
    /// # Errors
    ///
    /// Returns a graph-validation or JSON encoding error.
    pub fn to_json_pretty(&self) -> Result<String, UiGraphError> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Check version, size, identity, accessibility, binding, and geometry
    /// invariants for the complete graph.
    ///
    /// # Errors
    ///
    /// Returns the first violated graph invariant.
    pub fn validate(&self) -> Result<(), UiGraphError> {
        if self.version != UI_GRAPH_VERSION {
            return Err(UiGraphError::UnsupportedVersion {
                found: self.version,
                supported: UI_GRAPH_VERSION,
            });
        }
        let mut ids = HashSet::new();
        let mut count = 0_usize;
        self.root.validate("root", 0, &mut count, &mut ids)
    }

    #[must_use]
    pub fn node(&self, id: &str) -> Option<&UiNode> {
        self.root.find(id)
    }

    /// Every source node in depth-first builder order.
    #[must_use]
    pub fn nodes(&self) -> Vec<&UiNode> {
        let mut nodes = Vec::new();
        self.root.collect(&mut nodes);
        nodes
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut UiNode> {
        self.root.find_mut(id)
    }

    /// Insert `node` into a row or column at `index`. The graph is changed
    /// only after the complete candidate validates.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/non-collection parent, an out-of-range
    /// index, or any graph invariant violated by the inserted subtree.
    pub fn insert_child(
        &mut self,
        parent_id: &str,
        index: usize,
        node: UiNode,
    ) -> Result<(), UiGraphError> {
        self.transaction(|candidate| {
            let parent = candidate
                .node_mut(parent_id)
                .ok_or_else(|| UiGraphError::UnknownNode(parent_id.to_owned()))?;
            let children = parent
                .collection_children_mut()
                .ok_or_else(|| UiGraphError::CannotHaveChildren(parent_id.to_owned()))?;
            if index > children.len() {
                return Err(UiGraphError::ChildIndexOutOfBounds {
                    parent: parent_id.to_owned(),
                    index,
                    len: children.len(),
                });
            }
            children.insert(index, node);
            Ok(())
        })
    }

    /// Remove a non-root node and return its complete subtree.
    ///
    /// A container's single required child cannot be removed; replace that
    /// child or remove the container instead.
    ///
    /// # Errors
    ///
    /// Returns an error for the root, an unknown node, or a container's
    /// required child. A failure leaves the graph unchanged.
    pub fn remove_node(&mut self, id: &str) -> Result<UiNode, UiGraphError> {
        if self.root.id() == id {
            return Err(UiGraphError::CannotRemoveRoot);
        }
        let mut candidate = self.clone();
        let removed = candidate
            .root
            .detach_descendant(id)?
            .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))?;
        candidate.validate()?;
        *self = candidate;
        Ok(removed)
    }

    /// Replace a node in place, including the root, with a validated subtree.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown target or any invariant violated by
    /// the replacement. A failure leaves the graph unchanged.
    pub fn replace_node(&mut self, id: &str, replacement: UiNode) -> Result<(), UiGraphError> {
        self.transaction(|candidate| {
            if candidate.root.id() == id {
                candidate.root = replacement;
                return Ok(());
            }
            if candidate.root.replace_descendant(id, replacement) {
                Ok(())
            } else {
                Err(UiGraphError::UnknownNode(id.to_owned()))
            }
        })
    }

    /// Reparent a non-root subtree into a row or column. `index` names the
    /// position in the destination after the source has been detached.
    ///
    /// # Errors
    ///
    /// Returns an error when moving the root, moving into the subtree itself,
    /// removing a container's required child, selecting an invalid parent or
    /// index, or violating another graph invariant. A failure is atomic.
    pub fn move_node(
        &mut self,
        id: &str,
        new_parent_id: &str,
        index: usize,
    ) -> Result<(), UiGraphError> {
        if self.root.id() == id {
            return Err(UiGraphError::CannotMoveRoot);
        }
        if self
            .node(id)
            .is_some_and(|node| node.find(new_parent_id).is_some())
        {
            return Err(UiGraphError::MoveIntoDescendant {
                node: id.to_owned(),
                parent: new_parent_id.to_owned(),
            });
        }
        self.transaction(|candidate| {
            let moved = candidate
                .root
                .detach_descendant(id)?
                .ok_or_else(|| UiGraphError::UnknownNode(id.to_owned()))?;
            let parent = candidate
                .node_mut(new_parent_id)
                .ok_or_else(|| UiGraphError::UnknownNode(new_parent_id.to_owned()))?;
            let children = parent
                .collection_children_mut()
                .ok_or_else(|| UiGraphError::CannotHaveChildren(new_parent_id.to_owned()))?;
            if index > children.len() {
                return Err(UiGraphError::ChildIndexOutOfBounds {
                    parent: new_parent_id.to_owned(),
                    index,
                    len: children.len(),
                });
            }
            children.insert(index, moved);
            Ok(())
        })
    }

    fn transaction(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<(), UiGraphError>,
    ) -> Result<(), UiGraphError> {
        let mut candidate = self.clone();
        edit(&mut candidate)?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiButtonRole {
    Primary,
    #[default]
    Secondary,
    Destructive,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UiTextRole {
    Display,
    LargeTitle,
    Title,
    Heading,
    Subheading,
    #[default]
    Body,
    Label,
    Caption,
    Reading,
    Code,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiTextSource {
    Static { value: String },
    Document { path: String },
}

/// Process-local navigation requested by a button.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiNavigation {
    Push { route: String },
    Replace { route: String },
    Back,
    Forward,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum UiNode {
    Column {
        id: String,
        #[serde(default)]
        children: Vec<UiNode>,
        #[serde(default)]
        gap: Option<f32>,
        #[serde(default)]
        no_padding: bool,
        #[serde(default)]
        intrinsic_height: bool,
        #[serde(default)]
        fill_remaining_height: bool,
    },
    Row {
        id: String,
        #[serde(default)]
        children: Vec<UiNode>,
        #[serde(default)]
        no_wrap: bool,
        #[serde(default)]
        fill_remaining_height: bool,
    },
    Container {
        id: String,
        child: Box<UiNode>,
        #[serde(default)]
        background: bool,
        #[serde(default)]
        no_padding: bool,
        #[serde(default)]
        fill_height: bool,
        #[serde(default)]
        width: Option<f32>,
        #[serde(default)]
        height: Option<f32>,
    },
    Text {
        id: String,
        text: UiTextSource,
        #[serde(default)]
        role: UiTextRole,
    },
    Button {
        id: String,
        #[serde(default)]
        label: String,
        #[serde(default)]
        accessible_name: Option<String>,
        #[serde(default)]
        tooltip: Option<String>,
        #[serde(default)]
        on_click_path: Option<String>,
        #[serde(default)]
        click_value: Option<String>,
        #[serde(default)]
        navigate: Option<UiNavigation>,
        #[serde(default = "default_true")]
        enabled: bool,
        #[serde(default)]
        role: UiButtonRole,
    },
    TextInput {
        id: String,
        /// Optional visible Fluent label. When present it is also the
        /// control's accessible name, so serialized forms have one source of
        /// truth for both presentations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        accessible_name: String,
        #[serde(default)]
        placeholder: String,
        #[serde(default)]
        value_path: Option<String>,
        #[serde(default)]
        initial_value: String,
        #[serde(default)]
        multiline: bool,
        #[serde(default)]
        disabled: bool,
    },
    Checkbox {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        accessible_name: String,
        value_path: String,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    Toggle {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        accessible_name: String,
        value_path: String,
        #[serde(default = "default_true")]
        enabled: bool,
    },
    /// App-owned retained widget built by a process-local component factory.
    Component {
        id: String,
        /// Stable kebab-case factory name registered by the app.
        name: String,
        /// Serializable app-defined input passed unchanged to the factory.
        #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
        props: serde_json::Map<String, serde_json::Value>,
    },
    Spacer {
        id: String,
    },
}

const fn default_true() -> bool {
    true
}

impl UiNode {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Column { id, .. }
            | Self::Row { id, .. }
            | Self::Container { id, .. }
            | Self::Text { id, .. }
            | Self::Button { id, .. }
            | Self::TextInput { id, .. }
            | Self::Checkbox { id, .. }
            | Self::Toggle { id, .. }
            | Self::Component { id, .. }
            | Self::Spacer { id } => id,
        }
    }

    pub fn children(&self) -> Vec<&UiNode> {
        match self {
            Self::Column { children, .. } | Self::Row { children, .. } => children.iter().collect(),
            Self::Container { child, .. } => vec![child],
            _ => Vec::new(),
        }
    }

    fn children_mut(&mut self) -> Vec<&mut UiNode> {
        match self {
            Self::Column { children, .. } | Self::Row { children, .. } => {
                children.iter_mut().collect()
            }
            Self::Container { child, .. } => vec![child],
            _ => Vec::new(),
        }
    }

    fn collection_children_mut(&mut self) -> Option<&mut Vec<UiNode>> {
        match self {
            Self::Column { children, .. } | Self::Row { children, .. } => Some(children),
            _ => None,
        }
    }

    fn detach_descendant(&mut self, id: &str) -> Result<Option<Self>, UiGraphError> {
        match self {
            Self::Column { children, .. } | Self::Row { children, .. } => {
                if let Some(index) = children.iter().position(|child| child.id() == id) {
                    return Ok(Some(children.remove(index)));
                }
                for child in children {
                    if let Some(removed) = child.detach_descendant(id)? {
                        return Ok(Some(removed));
                    }
                }
                Ok(None)
            }
            Self::Container {
                id: parent, child, ..
            } => {
                if child.id() == id {
                    Err(UiGraphError::RequiredContainerChild {
                        parent: parent.clone(),
                        child: id.to_owned(),
                    })
                } else {
                    child.detach_descendant(id)
                }
            }
            _ => Ok(None),
        }
    }

    fn replace_descendant(&mut self, id: &str, replacement: Self) -> bool {
        match self {
            Self::Column { children, .. } | Self::Row { children, .. } => {
                if let Some(child) = children.iter_mut().find(|child| child.id() == id) {
                    *child = replacement;
                    return true;
                }
                for child in children {
                    if child.replace_descendant(id, replacement.clone()) {
                        return true;
                    }
                }
                false
            }
            Self::Container { child, .. } => {
                if child.id() == id {
                    **child = replacement;
                    true
                } else {
                    child.replace_descendant(id, replacement)
                }
            }
            _ => false,
        }
    }

    fn find(&self, id: &str) -> Option<&Self> {
        if self.id() == id {
            return Some(self);
        }
        self.children().into_iter().find_map(|child| child.find(id))
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Self> {
        if self.id() == id {
            return Some(self);
        }
        self.children_mut()
            .into_iter()
            .find_map(|child| child.find_mut(id))
    }

    fn collect<'a>(&'a self, nodes: &mut Vec<&'a Self>) {
        nodes.push(self);
        for child in self.children() {
            child.collect(nodes);
        }
    }

    fn validate(
        &self,
        path: &str,
        depth: usize,
        count: &mut usize,
        ids: &mut HashSet<String>,
    ) -> Result<(), UiGraphError> {
        if depth > MAX_GRAPH_DEPTH {
            return Err(UiGraphError::TooDeep {
                max: MAX_GRAPH_DEPTH,
            });
        }
        *count = count.saturating_add(1);
        if *count > MAX_GRAPH_NODES {
            return Err(UiGraphError::TooManyNodes {
                max: MAX_GRAPH_NODES,
            });
        }
        let id = self.id();
        if id.trim().is_empty() || id.len() > MAX_ID_LEN {
            return Err(UiGraphError::InvalidId {
                path: path.to_owned(),
                id: id.to_owned(),
            });
        }
        if !ids.insert(id.to_owned()) {
            return Err(UiGraphError::DuplicateId(id.to_owned()));
        }
        match self {
            Self::Column { gap, .. } => validate_non_negative(*gap, id, "gap")?,
            Self::Container { width, height, .. } => {
                validate_positive(*width, id, "width")?;
                validate_positive(*height, id, "height")?;
            }
            Self::Text { text, .. } => {
                if let UiTextSource::Document { path } = text {
                    validate_binding(path, id)?;
                }
            }
            Self::Button {
                label,
                accessible_name,
                on_click_path,
                navigate,
                ..
            } => {
                if label.trim().is_empty()
                    && accessible_name
                        .as_deref()
                        .is_none_or(|name| name.trim().is_empty())
                {
                    return Err(UiGraphError::MissingAccessibleName(id.to_owned()));
                }
                if let Some(binding) = on_click_path {
                    validate_binding(binding, id)?;
                }
                if let Some(UiNavigation::Push { route } | UiNavigation::Replace { route }) =
                    navigate
                {
                    validate_binding(route, id)?;
                }
            }
            Self::TextInput {
                label,
                accessible_name,
                value_path,
                ..
            } => {
                validate_form_label(label.as_deref(), accessible_name, id)?;
                if let Some(binding) = value_path {
                    validate_binding(binding, id)?;
                }
            }
            Self::Checkbox {
                label,
                accessible_name,
                value_path,
                ..
            }
            | Self::Toggle {
                label,
                accessible_name,
                value_path,
                ..
            } => {
                validate_form_label(label.as_deref(), accessible_name, id)?;
                validate_binding(value_path, id)?;
            }
            Self::Component { name, .. } => validate_component_name(name, id)?,
            Self::Row { .. } | Self::Spacer { .. } => {}
        }
        for (index, child) in self.children().into_iter().enumerate() {
            child.validate(
                &format!("{path}/{}[{index}]", child.id()),
                depth.saturating_add(1),
                count,
                ids,
            )?;
        }
        Ok(())
    }
}

fn validate_component_name(name: &str, id: &str) -> Result<(), UiGraphError> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_COMPONENT_NAME_LEN
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && !bytes.windows(2).any(|pair| pair == b"--");
    if valid {
        Ok(())
    } else {
        Err(UiGraphError::InvalidComponentName {
            id: id.to_owned(),
            name: name.to_owned(),
        })
    }
}

fn validate_accessible_name(name: &str, id: &str) -> Result<(), UiGraphError> {
    if name.trim().is_empty() {
        Err(UiGraphError::MissingAccessibleName(id.to_owned()))
    } else {
        Ok(())
    }
}

fn validate_form_label(
    label: Option<&str>,
    accessible_name: &str,
    id: &str,
) -> Result<(), UiGraphError> {
    validate_accessible_name(label.unwrap_or(accessible_name), id)?;
    if label.is_some() && !accessible_name.trim().is_empty() {
        Err(UiGraphError::ConflictingLabels(id.to_owned()))
    } else {
        Ok(())
    }
}

fn validate_binding(binding: &str, id: &str) -> Result<(), UiGraphError> {
    if binding.trim().is_empty() || binding.len() > MAX_BINDING_LEN {
        Err(UiGraphError::InvalidBinding {
            id: id.to_owned(),
            binding: binding.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_positive(value: Option<f32>, id: &str, property: &str) -> Result<(), UiGraphError> {
    if value.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        Err(UiGraphError::InvalidDimension {
            id: id.to_owned(),
            property: property.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn validate_non_negative(value: Option<f32>, id: &str, property: &str) -> Result<(), UiGraphError> {
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        Err(UiGraphError::InvalidDimension {
            id: id.to_owned(),
            property: property.to_owned(),
        })
    } else {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UiGraphError {
    #[error("UI graph revision space is exhausted; refusing to reuse a prior live state")]
    RevisionExhausted,
    #[error("invalid UI graph JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("UI graph version {found} is unsupported; this toolkit supports {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("invalid UI node id {id:?} at {path}")]
    InvalidId { path: String, id: String },
    #[error("duplicate UI node id {0:?}")]
    DuplicateId(String),
    #[error("UI graph exceeds the maximum nesting depth of {max}")]
    TooDeep { max: usize },
    #[error("UI graph exceeds the maximum node count of {max}")]
    TooManyNodes { max: usize },
    #[error("UI node {0:?} needs a visible label or accessible name")]
    MissingAccessibleName(String),
    #[error("UI node {0:?} must use either label or accessible_name, not both")]
    ConflictingLabels(String),
    #[error("UI node {id:?} has invalid model binding {binding:?}")]
    InvalidBinding { id: String, binding: String },
    #[error("UI node {id:?} has invalid {property}")]
    InvalidDimension { id: String, property: String },
    #[error("UI component node {id:?} has invalid component name {name:?}")]
    InvalidComponentName { id: String, name: String },
    #[error("UI component node {id:?} references unregistered component {name:?}")]
    UnknownComponent { id: String, name: String },
    #[error("UI component {name:?} at node {id:?} could not be built: {message}")]
    ComponentBuild {
        id: String,
        name: String,
        message: String,
    },
    #[error("UI node {0:?} does not exist")]
    UnknownNode(String),
    #[error("UI node {0:?} is not a row or column and cannot accept children")]
    CannotHaveChildren(String),
    #[error("child index {index} is outside {parent:?}, which has {len} children")]
    ChildIndexOutOfBounds {
        parent: String,
        index: usize,
        len: usize,
    },
    #[error("the UI graph root cannot be removed")]
    CannotRemoveRoot,
    #[error("the UI graph root cannot be moved; replace it instead")]
    CannotMoveRoot,
    #[error("cannot remove {child:?}; it is the required child of container {parent:?}")]
    RequiredContainerChild { parent: String, child: String },
    #[error("cannot move {node:?} into its own descendant {parent:?}")]
    MoveIntoDescendant { node: String, parent: String },
    #[error("UI graph lock was poisoned")]
    Poisoned,
    #[error("the UI graph changed while this edit was being prepared; retry against the new graph")]
    ConcurrentEdit,
    #[error("UI graph I/O error: {0}")]
    Io(String),
    #[error("UI graph edit batch must contain at least one edit")]
    EmptyEditBatch,
    #[error("UI graph edit {index} failed: {message}")]
    EditFailed { index: usize, message: String },
    #[error("UI node {id:?} has invalid property path {property:?}")]
    InvalidPropertyPath { id: String, property: String },
    #[error("UI node {id:?} has no property {property:?}")]
    UnknownProperty { id: String, property: String },
    #[error("property {property:?} is not valid for UI node {id:?}: {message}")]
    InvalidPropertyValue {
        id: String,
        property: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UiDocument {
        UiDocument::new(UiNode::Column {
            id: "root".into(),
            children: vec![
                UiNode::Text {
                    id: "title".into(),
                    text: UiTextSource::Static {
                        value: "Projects".into(),
                    },
                    role: UiTextRole::Title,
                },
                UiNode::Row {
                    id: "actions".into(),
                    children: Vec::new(),
                    no_wrap: true,
                    fill_remaining_height: false,
                },
            ],
            gap: Some(8.0),
            no_padding: false,
            intrinsic_height: false,
            fill_remaining_height: false,
        })
    }

    #[test]
    fn structural_edits_and_components_round_trip_or_leave_the_document_intact() {
        // Phase `structural_edits_round_trip_through_the_wire_format`.
        {
            let mut document = sample();
            document
                .insert_child(
                    "actions",
                    0,
                    UiNode::Button {
                        id: "create".into(),
                        label: "Create".into(),
                        accessible_name: None,
                        tooltip: None,
                        on_click_path: Some("create-requested".into()),
                        click_value: None,
                        navigate: None,
                        enabled: true,
                        role: UiButtonRole::Primary,
                    },
                )
                .unwrap();
            document.move_node("create", "root", 1).unwrap();
            let json = document.to_json_pretty().unwrap();
            let reopened = UiDocument::from_json(&json).unwrap();
            assert_eq!(reopened, document);
            assert_eq!(
                reopened
                    .nodes()
                    .iter()
                    .map(|node| node.id())
                    .collect::<Vec<_>>(),
                ["root", "title", "create", "actions"]
            );
        }
        // Phase `app_component_nodes_round_trip_props_and_validate_factory_names`.
        {
            let document = UiDocument::from_json(
                r#"{"version":1,"root":{"type":"component","id":"profile","name":"profile-card","props":{"account_id":42,"compact":true}}}"#,
            )
            .expect("valid app component");
            assert!(matches!(
                document.node("profile"),
                Some(UiNode::Component { name, props, .. })
                    if name == "profile-card"
                        && props.get("account_id") == Some(&serde_json::json!(42))
                        && props.get("compact") == Some(&serde_json::json!(true))
            ));
            assert_eq!(
                UiDocument::from_json(&document.to_json_pretty().unwrap()).unwrap(),
                document
            );

            for name in [
                "",
                "ProfileCard",
                "profile_card",
                "-profile",
                "profile--card",
            ] {
                let source = format!(
                    r#"{{"version":1,"root":{{"type":"component","id":"profile","name":{name:?}}}}}"#
                );
                assert!(matches!(
                    UiDocument::from_json(&source),
                    Err(UiGraphError::InvalidComponentName { .. })
                ));
            }
        }
        // Phase `every_failed_structural_edit_preserves_the_document`.
        {
            let mut document = sample();
            let original = document.clone();
            assert!(
                document
                    .insert_child("root", 0, UiNode::Spacer { id: "title".into() })
                    .is_err()
            );
            assert_eq!(document, original);
            assert!(document.move_node("root", "actions", 0).is_err());
            assert_eq!(document, original);
            assert!(document.remove_node("root").is_err());
            assert_eq!(document, original);
        }
    }

    #[test]
    fn navigation_and_form_labels_validate_their_targets_and_sources() {
        // Phase `navigation_round_trips_and_rejects_blank_destinations`.
        {
            let mut document = sample();
            document
                .insert_child(
                    "actions",
                    0,
                    UiNode::Button {
                        id: "settings".into(),
                        label: "Settings".into(),
                        accessible_name: None,
                        tooltip: None,
                        on_click_path: None,
                        click_value: None,
                        navigate: Some(UiNavigation::Push {
                            route: "settings".into(),
                        }),
                        enabled: true,
                        role: UiButtonRole::Secondary,
                    },
                )
                .unwrap();
            let reopened = UiDocument::from_json(&document.to_json_pretty().unwrap()).unwrap();
            assert_eq!(reopened, document);

            if let Some(UiNode::Button { navigate, .. }) = document.node_mut("settings") {
                *navigate = Some(UiNavigation::Replace { route: "  ".into() });
            }
            assert!(matches!(
                document.validate(),
                Err(UiGraphError::InvalidBinding { .. })
            ));
        }
        // Phase `form_labels_are_single_visible_and_accessible_sources`.
        {
            let mut document = sample();
            document
                .insert_child(
                    "root",
                    1,
                    UiNode::TextInput {
                        id: "name".into(),
                        label: Some("profile-name-label".into()),
                        accessible_name: String::new(),
                        placeholder: String::new(),
                        value_path: None,
                        initial_value: String::new(),
                        multiline: false,
                        disabled: false,
                    },
                )
                .unwrap();
            document
                .insert_child(
                    "root",
                    2,
                    UiNode::Checkbox {
                        id: "remember".into(),
                        label: Some("profile-remember-label".into()),
                        accessible_name: String::new(),
                        value_path: "remember".into(),
                        enabled: true,
                    },
                )
                .unwrap();

            let json = document.to_json_pretty().unwrap();
            assert!(json.contains(r#""label": "profile-name-label""#));
            assert!(json.contains(r#""label": "profile-remember-label""#));
            assert!(!json.contains("accessible_name"));
            assert_eq!(UiDocument::from_json(&json).unwrap(), document);

            if let Some(UiNode::TextInput {
                accessible_name, ..
            }) = document.node_mut("name")
            {
                *accessible_name = "duplicate-name".into();
            }
            assert!(matches!(
                document.validate(),
                Err(UiGraphError::ConflictingLabels(id)) if id == "name"
            ));

            if let Some(UiNode::TextInput {
                accessible_name, ..
            }) = document.node_mut("name")
            {
                accessible_name.clear();
            }
            if let Some(UiNode::Checkbox {
                accessible_name, ..
            }) = document.node_mut("remember")
            {
                *accessible_name = "duplicate-check-name".into();
            }
            assert!(matches!(
                document.validate(),
                Err(UiGraphError::ConflictingLabels(id)) if id == "remember"
            ));
        }
    }
}

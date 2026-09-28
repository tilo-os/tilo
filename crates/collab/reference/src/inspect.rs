//! The wire vocabulary of **inspection**: asking an app what its semantic
//! tree holds, and its answer.
//!
//! The session asks one application connection, over the collaboration
//! broker, for the node at a surface point, a node by id, or a depth-bounded
//! subtree. The app answers from its retained semantic tree (`ui::SemanticTree`,
//! which also drives AccessKit, the `ui:` action resources and `Ctrl+K`), so a
//! pick, a recorded step and a screen-reader focus name one node by one id.
//!
//! These types live here rather than in the toolkit so that everything that
//! consumes an answer without drawing UI — the session recorder, the frame
//! masker, a relay — reads the same shapes, and an answer that names document
//! content carries the same [`ResourceRef`] a remark does.
//!
//! Every answer is redacted for its [`Audience`] by each node's declared
//! [`Sensitivity`] ([`redact_node`]): a secret never crosses, personal content
//! crosses only as a shape-preserving placeholder when it leaves the device.
//! Geometry is never redacted, so a secret field can still be masked in a
//! captured frame.

use serde::{Deserialize, Serialize};
use tilo_actions::{Audience, Sensitivity};

use crate::ResourceRef;

/// Wire version of [`InspectRequest`]/[`InspectReply`].
pub const INSPECT_VERSION: u16 = 1;
/// Upper bound on the nodes one reply carries, whatever depth was asked.
pub const MAX_INSPECT_NODES: usize = 2048;
/// Upper bound on the depth a subtree query walks.
pub const MAX_INSPECT_DEPTH: u16 = 64;

/// What to inspect. Node ids are the semantic tree's stable ids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspectQuery {
    /// The deepest on-screen node under this point, in the app surface's
    /// logical coordinates, and the path of nodes above it.
    At { x: f64, y: f64 },
    /// One node by its stable id.
    Node { id: String },
    /// `root` (the window root when `None`) and its descendants down to
    /// `depth` edges below it, breadth first. `depth` is clamped to
    /// [`MAX_INSPECT_DEPTH`].
    Subtree { root: Option<String>, depth: u16 },
}

/// One inspection request, as it reaches the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectRequest {
    pub version: u16,
    pub query: InspectQuery,
    /// Who will read the answer. The session sets it from the caller, never
    /// the caller itself.
    pub audience: Audience,
}

impl InspectRequest {
    pub fn new(query: InspectQuery, audience: Audience) -> Self {
        Self {
            version: INSPECT_VERSION,
            query,
            audience,
        }
    }
}

/// A rectangle in the app surface's **logical** pixels. Multiply by the
/// surface scale for device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl SurfaceRect {
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// How long an [`InspectedNode::id`] stays meaningful.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// Declared by the app (a resource, a unique test id, a semantic key):
    /// it may be kept and resolved later.
    Durable,
    /// A structural path: it survives a rebuild of the same structure but
    /// shifts when a sibling is inserted before it.
    Positional,
}

/// Where a node presents its content, which is what redaction applies to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    /// In its value; the label is only the field's name.
    Value,
    /// In its label: a plain label or a list row *is* its name.
    Label,
}

/// Whether the semantic tree accounts for a node's pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pixels {
    /// The node describes what it paints.
    Described,
    /// A canvas or an unknown role: no node describes these pixels, so a
    /// frame comparison cannot explain them.
    Undescribed,
    /// The node paints feedback this device produces on its own clock and
    /// from its own local state (a status pill that fades after a save), not
    /// anything a session carries. A replay cannot reproduce these pixels,
    /// so a frame comparison leaves them out rather than blame them.
    Transient,
}

/// The node's interactive state, as a screen reader would announce it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeState {
    pub focused: bool,
    pub disabled: bool,
}

/// One node of an inspection answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InspectedNode {
    /// The node's stable semantic id.
    pub id: String,
    pub identity: Identity,
    /// The accessibility role, snake case (`button`, `text_input`, `heading`).
    pub role: String,
    /// The accessible name; redacted like content when it is the content.
    pub label: Option<String>,
    /// The node's value, redacted for the audience. Never set for a secret.
    pub value: Option<String>,
    pub sensitivity: Sensitivity,
    pub content: Content,
    /// Full bounds, already scrolled. Kept for every sensitivity.
    pub bounds: Option<SurfaceRect>,
    /// The on-screen part of [`bounds`](Self::bounds) after every ancestor
    /// clip; `None` when the node is not on screen.
    pub visible: Option<SurfaceRect>,
    pub pixels: Pixels,
    pub state: NodeState,
    /// Every child's id, in reading order, whether or not the reply carries
    /// the child itself.
    pub children: Vec<String>,
    /// The document content this node stands for.
    pub reference: Option<ResourceRef>,
}

impl InspectedNode {
    /// The text that is this node's content: its value or its label.
    pub fn content_text(&self) -> Option<&str> {
        match self.content {
            Content::Value => self.value.as_deref(),
            Content::Label => self.label.as_deref(),
        }
    }
}

/// The answer to one [`InspectRequest`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InspectReply {
    pub version: u16,
    /// The node that answers the query — the node under the point, the node
    /// asked for, the subtree root — or `None` when nothing matched.
    pub target: Option<String>,
    /// For [`InspectQuery::At`], the path from the window root down to the
    /// target; for [`InspectQuery::Node`], the target alone; for
    /// [`InspectQuery::Subtree`], the subtree breadth first.
    pub nodes: Vec<InspectedNode>,
    /// Some node's children were left out because of the depth or the
    /// [`MAX_INSPECT_NODES`] bound.
    pub truncated: bool,
    /// Why the app could not answer at all, in its own words. `None` for an
    /// answer, even one that matched nothing.
    pub refused: Option<String>,
}

impl InspectReply {
    /// An answer that matched nothing yet.
    pub fn empty() -> Self {
        Self {
            version: INSPECT_VERSION,
            ..Self::default()
        }
    }

    /// A refusal carrying `reason`.
    pub fn refused(reason: impl Into<String>) -> Self {
        Self {
            refused: Some(reason.into()),
            ..Self::empty()
        }
    }

    /// Decode an answer as it crosses the broker (`completeInspection`'s
    /// `replyJson`) or as a recorder stored it.
    ///
    /// # Errors
    ///
    /// When `bytes` is not an encoded [`InspectReply`].
    pub fn decode(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// The encoded form [`decode`](Self::decode) reads.
    ///
    /// # Errors
    ///
    /// Only if serialization itself fails, which these types never do.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// The answering node.
    pub fn target_node(&self) -> Option<&InspectedNode> {
        let target = self.target.as_ref()?;
        self.nodes.iter().find(|node| &node.id == target)
    }

    /// Re-apply redaction for `audience`. Idempotent, so a relay may apply it
    /// to an answer that was already redacted at its source.
    #[must_use]
    pub fn redacted_for(mut self, audience: Audience) -> Self {
        for node in &mut self.nodes {
            redact_node(node, audience);
        }
        self
    }
}

/// Apply `node.sensitivity` to its content for `audience`.
///
/// The content is the value when the node presents one (the label is then
/// only the field's name); otherwise it is the label. A reference's label is
/// what a chip shows of the content, so it follows the same rule; its address
/// is kept. Geometry is never touched.
pub fn redact_node(node: &mut InspectedNode, audience: Audience) {
    let sensitivity = node.sensitivity;
    node.value = node
        .value
        .take()
        .and_then(|value| sensitivity.present(&value, audience));
    if node.content == Content::Label {
        node.label = node
            .label
            .take()
            .and_then(|label| sensitivity.present(&label, audience));
    }
    if let Some(reference) = &mut node.reference {
        reference.label = sensitivity
            .present(&reference.label, audience)
            .unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contact() -> InspectedNode {
        InspectedNode {
            id: "t:contact".into(),
            identity: Identity::Durable,
            role: "list_item".into(),
            label: Some("Ada Lovelace".into()),
            value: None,
            sensitivity: Sensitivity::Personal,
            content: Content::Label,
            bounds: Some(SurfaceRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
            visible: None,
            pixels: Pixels::Described,
            state: NodeState::default(),
            children: Vec::new(),
            reference: Some(ResourceRef::whole(
                "contacts.person",
                "contacts.person:1",
                "Ada Lovelace",
            )),
        }
    }

    #[test]
    fn a_label_that_is_personal_content_is_redacted_as_content_and_keeps_its_address() {
        let mut node = contact();
        redact_node(&mut node, Audience::BeyondDevice);
        assert_eq!(node.label.as_deref(), Some("▮▮▮ ▮▮▮▮▮▮▮▮"));
        let reference = node.reference.as_ref().expect("reference kept");
        assert_eq!(reference.id, "contacts.person:1");
        assert_eq!(reference.label, "▮▮▮ ▮▮▮▮▮▮▮▮");
        assert!(node.bounds.is_some(), "geometry is never redacted");
    }

    #[test]
    fn a_secret_field_keeps_its_name_and_geometry_but_never_its_value() {
        let mut node = InspectedNode {
            role: "text_input".into(),
            label: Some("Password".into()),
            value: Some("hunter2".into()),
            sensitivity: Sensitivity::Secret,
            content: Content::Value,
            reference: None,
            ..contact()
        };
        redact_node(&mut node, Audience::LocalSession);
        assert_eq!(node.value, None);
        assert_eq!(node.label.as_deref(), Some("Password"));
        assert_eq!(node.content_text(), None);
        assert!(node.bounds.is_some());
    }

    #[test]
    fn an_answer_round_trips_through_its_encoding_and_redaction_is_idempotent() {
        let reply = InspectReply {
            target: Some("t:contact".into()),
            nodes: vec![contact()],
            ..InspectReply::empty()
        };
        let decoded = InspectReply::decode(&reply.encode().unwrap()).unwrap();
        assert_eq!(decoded, reply);
        let once = decoded.redacted_for(Audience::BeyondDevice);
        assert_eq!(once.clone().redacted_for(Audience::BeyondDevice), once);
        assert_eq!(once.target_node().unwrap().id, "t:contact");
    }
}

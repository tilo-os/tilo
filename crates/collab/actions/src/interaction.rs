//! Semantic, transient presence for an action invocation.
//!
//! An agent does not have a mouse pointer to mirror.  This vocabulary names
//! the resource it is working on and the application-native selection that
//! resource understands.  Receivers resolve those stable anchors against
//! their own layout; pixels, viewport positions, and widget indices never
//! cross the wire.

use serde::{Deserialize, Serialize};

use crate::{ActionApp, ActionGroup, ActionResource, InvocationId};

/// Wire version understood by this crate.
pub const INTERACTION_VERSION: u16 = 1;
/// Hard ceiling for one JSON-encoded update at a trust boundary.
pub const MAX_INTERACTION_BYTES: usize = 32 * 1024;
pub const MAX_ANCHOR_ITEMS: usize = 64;
pub const MAX_ID_BYTES: usize = 512;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_DETAIL_BYTES: usize = 2 * 1024;

/// What kind of actor did something: in presence, in history and at every
/// door. Assigned by the host that authenticated the actor, never inferred
/// from a display name or an `agent:` string prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    #[default]
    Person,
    Agent,
    /// A Shortcut running steps for a person.
    Shortcut,
    /// An application acting under its own identity.
    App,
    /// An installed bot acting as itself, for its installer.
    Bot,
}

impl ActorKind {
    /// Software acting for someone (an agent, a Shortcut, an app), never a
    /// person at their own keyboard: shown with the agent mark and named
    /// with whom it acts for.
    #[must_use]
    pub const fn acts_for_someone(self) -> bool {
        !matches!(self, Self::Person)
    }
}

/// Identity attached by the broker to the normalized interaction event.
/// Friendly names and colours remain roster-owned presentation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionActor {
    pub id: String,
    pub kind: ActorKind,
    /// The person an agent, a Shortcut or an app acts for, as the host
    /// admitted the delegation: what presence names as "Claude, for Ada".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_person: Option<String>,
}

impl InteractionActor {
    /// `id`, of `kind`, acting for nobody but itself.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: ActorKind) -> Self {
        Self {
            id: id.into(),
            kind,
            for_person: None,
        }
    }

    /// The same actor, acting for `person`.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<String>) -> Self {
        self.for_person = Some(person.into());
        self
    }

    /// The actor a receiver shows for an update a peer sent as `sender`:
    /// the actor the update names when it is software acting for exactly
    /// the person the transport certified the sender as (`certified`), so
    /// a device can say its own person's agent acted, never that someone
    /// else did; otherwise the sender itself, as a person.
    #[must_use]
    pub fn from_peer(&self, sender: &str, certified: Option<&str>) -> Self {
        let vouched = self.kind.acts_for_someone()
            && certified.is_some_and(|person| self.for_person.as_deref() == Some(person));
        if vouched {
            self.clone()
        } else {
            Self::new(sender, ActorKind::Person)
        }
    }
}

/// A stable character range within a document block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionTextRange {
    pub block: String,
    pub start: u64,
    pub end: u64,
}

/// A stable inclusive cell rectangle. `sheet` lives on the enclosing anchor
/// so a multi-range selection cannot accidentally span workbooks/sheets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionGridRange {
    pub start_col: u32,
    pub start_row: u32,
    pub end_col: u32,
    pub end_row: u32,
}

/// Application-native target of an interaction.  Every variant is semantic:
/// receivers turn it into their current geometry and may fall back to the
/// enclosing resource when an anchor is off-screen or no longer exists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "kind", rename_all = "snake_case")]
pub enum InteractionAnchor {
    /// The whole resource is the narrowest truthful target available.
    Resource,
    /// A durable node in the app's semantic/accessibility tree.
    SemanticControl { id: String },
    /// One or more ranges in stable document blocks.
    Text {
        region: String,
        ranges: Vec<InteractionTextRange>,
    },
    /// One or more cell rectangles in a stable sheet identity.
    Grid {
        sheet: String,
        ranges: Vec<InteractionGridRange>,
    },
    /// Stable object ids, e.g. shapes, slides, clips, or document blocks.
    Objects { ids: Vec<String> },
    /// Stable rows/items, e.g. filesystem resource addresses.
    ListItems { ids: Vec<String> },
    /// A semantic media interval; times are content time, never screen space.
    Timeline {
        track: Option<String>,
        start_ms: u64,
        end_ms: u64,
    },
}

read_through_json_value!(InteractionAnchor);

/// What the invocation is visibly doing.  Terminal details complement the
/// durable invocation result; they do not replace it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", tag = "phase", rename_all = "snake_case")]
pub enum InteractionPhase {
    Locating,
    Applying,
    Progress {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    Waiting {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Committed,
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        message: String,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

read_through_json_value!(InteractionPhase);

impl InteractionPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Committed | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

/// Action identity and human label from the application's current catalogue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionAction {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<ActionApp>,
}

/// One broker-normalized, transient semantic interaction event.
///
/// `sequence` is monotonic within an invocation. The broker owns the actor,
/// sequence and legal lifecycle transitions; an application contributes only
/// the target it can resolve from the freshly encoded action and live model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticInteraction {
    pub version: u16,
    pub invocation: InvocationId,
    /// Opaque host-minted correlation to this exact original admission.
    /// It grants no authority; hosts resolve it against retained custody,
    /// never against the actor's current task or an application claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paint_origin: Option<uuid::Uuid>,
    pub sequence: u64,
    pub actor: InteractionActor,
    pub phase: InteractionPhase,
    pub action: InteractionAction,
    pub resource: ActionResource,
    pub anchor: InteractionAnchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<ActionGroup>,
}

/// Transport/UI spelling for a semantic interaction lifecycle update.
///
/// The alias keeps the data model's descriptive name while letting stream and
/// event APIs use `InteractionUpdate` alongside `InvocationUpdate`.
pub type InteractionUpdate = SemanticInteraction;

impl SemanticInteraction {
    /// Replace attribution with the identity authenticated by the broker.
    ///
    /// `actor` in decoded application/agent input is an untrusted claim. A
    /// broker must call this (and assign its own monotonic `sequence`) before
    /// validating or broadcasting the normalized event. Presence consumers
    /// may trust attribution only on broker-originated streams.
    #[must_use]
    pub fn normalized_by_broker(mut self, actor: InteractionActor, sequence: u64) -> Self {
        self.actor = actor;
        // Application-authored presence cannot mint admission provenance.
        self.paint_origin = None;
        self.sequence = sequence;
        self
    }

    /// Validate size and semantic bounds before accepting an untrusted update.
    /// Unknown additive object fields remain serde-compatible; a semantic
    /// change uses a new `version` and is rejected here by older readers.
    ///
    /// # Errors
    ///
    /// Returns the first bounded-field, range, version, or encoded-size
    /// violation found in the update.
    pub fn validate(&self) -> Result<(), InteractionViolation> {
        if self.version != INTERACTION_VERSION {
            return Err(issue("version", "unsupported interaction version"));
        }
        validate_id("actor.id", &self.actor.id)?;
        if let Some(person) = &self.actor.for_person {
            validate_id("actor.for_person", person)?;
        }
        validate_id("action.id", &self.action.id)?;
        validate_label("action.label", &self.action.label)?;
        validate_resource(&self.resource)?;
        if let Some(app) = &self.action.app {
            validate_id("action.app.id", &app.id)?;
            validate_label("action.app.label", &app.label)?;
        }
        if let Some(group) = &self.group {
            validate_id("group.id", &group.id)?;
            validate_label("group.label", &group.label)?;
            validate_id("group.kind", &group.kind)?;
        }
        validate_phase(&self.phase)?;
        validate_anchor(&self.anchor)?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| issue("interaction", "interaction could not be encoded"))?;
        if encoded.len() > MAX_INTERACTION_BYTES {
            return Err(issue("interaction", "encoded interaction is too large"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionViolation {
    pub field: &'static str,
    pub reason: &'static str,
}

impl std::fmt::Display for InteractionViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for InteractionViolation {}

const fn issue(field: &'static str, reason: &'static str) -> InteractionViolation {
    InteractionViolation { field, reason }
}

fn validate_nonempty(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), InteractionViolation> {
    if value.trim().is_empty() {
        return Err(issue(field, "must not be empty"));
    }
    if value.len() > maximum {
        return Err(issue(field, "is too long"));
    }
    Ok(())
}

fn validate_id(field: &'static str, value: &str) -> Result<(), InteractionViolation> {
    validate_nonempty(field, value, MAX_ID_BYTES)
}

fn validate_label(field: &'static str, value: &str) -> Result<(), InteractionViolation> {
    validate_nonempty(field, value, MAX_LABEL_BYTES)
}

fn validate_resource(resource: &ActionResource) -> Result<(), InteractionViolation> {
    validate_id("resource.id", &resource.id)?;
    validate_id("resource.kind", &resource.kind)?;
    validate_label("resource.label", &resource.label)
}

fn validate_detail(field: &'static str, value: &str) -> Result<(), InteractionViolation> {
    if value.len() > MAX_DETAIL_BYTES {
        return Err(issue(field, "is too long"));
    }
    Ok(())
}

fn validate_phase(phase: &InteractionPhase) -> Result<(), InteractionViolation> {
    match phase {
        InteractionPhase::Waiting {
            reason: Some(reason),
        } => validate_detail("phase.reason", reason),
        InteractionPhase::Progress {
            label,
            completed,
            total,
        } => {
            validate_nonempty("phase.label", label, MAX_LABEL_BYTES)?;
            if let (Some(completed), Some(total)) = (completed, total)
                && completed > total
            {
                return Err(issue("phase.progress", "completed exceeds total"));
            }
            Ok(())
        }
        InteractionPhase::Failed { code, message } => {
            if let Some(code) = code {
                validate_id("phase.code", code)?;
            }
            validate_nonempty("phase.message", message, MAX_DETAIL_BYTES)
        }
        InteractionPhase::Cancelled {
            message: Some(message),
        } => validate_detail("phase.message", message),
        InteractionPhase::Locating
        | InteractionPhase::Applying
        | InteractionPhase::Waiting { reason: None }
        | InteractionPhase::Committed
        | InteractionPhase::Cancelled { message: None } => Ok(()),
    }
}

fn validate_anchor(anchor: &InteractionAnchor) -> Result<(), InteractionViolation> {
    match anchor {
        InteractionAnchor::Resource => Ok(()),
        InteractionAnchor::SemanticControl { id } => validate_id("anchor.id", id),
        InteractionAnchor::Text { region, ranges } => {
            validate_id("anchor.region", region)?;
            validate_count("anchor.ranges", ranges.len())?;
            for range in ranges {
                validate_id("anchor.range.block", &range.block)?;
                if range.end < range.start {
                    return Err(issue("anchor.range", "range end precedes start"));
                }
            }
            Ok(())
        }
        InteractionAnchor::Grid { sheet, ranges } => {
            validate_id("anchor.sheet", sheet)?;
            validate_count("anchor.ranges", ranges.len())?;
            if ranges
                .iter()
                .any(|range| range.end_col < range.start_col || range.end_row < range.start_row)
            {
                return Err(issue("anchor.range", "range end precedes start"));
            }
            Ok(())
        }
        InteractionAnchor::Objects { ids } | InteractionAnchor::ListItems { ids } => {
            validate_count("anchor.ids", ids.len())?;
            for id in ids {
                validate_id("anchor.id", id)?;
            }
            Ok(())
        }
        InteractionAnchor::Timeline {
            track,
            start_ms,
            end_ms,
        } => {
            if let Some(track) = track {
                validate_id("anchor.track", track)?;
            }
            if end_ms < start_ms {
                return Err(issue("anchor.range", "range end precedes start"));
            }
            Ok(())
        }
    }
}

fn validate_count(field: &'static str, count: usize) -> Result<(), InteractionViolation> {
    if count == 0 {
        return Err(issue(field, "must not be empty"));
    }
    if count > MAX_ANCHOR_ITEMS {
        return Err(issue(field, "has too many items"));
    }
    Ok(())
}

#[cfg(test)]
mod peer_actor_tests {
    use super::*;

    #[test]
    fn a_peer_can_vouch_only_for_its_own_persons_software() {
        let agent = InteractionActor::new("agent:claude", ActorKind::Agent).acting_for("ada");
        assert_eq!(agent.from_peer("peer@7", Some("ada")), agent);
        assert_eq!(
            agent.from_peer("peer@7", Some("bob")),
            InteractionActor::new("peer@7", ActorKind::Person),
            "Bob's device cannot say Ada's agent acted"
        );
        assert_eq!(
            agent.from_peer("peer@7", None),
            InteractionActor::new("peer@7", ActorKind::Person)
        );
        let impostor = InteractionActor::new("carol", ActorKind::Person).acting_for("ada");
        assert_eq!(
            impostor.from_peer("peer@7", Some("ada")),
            InteractionActor::new("peer@7", ActorKind::Person),
            "a person is only ever the sender"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(anchor: InteractionAnchor) -> SemanticInteraction {
        SemanticInteraction {
            paint_origin: None,
            version: INTERACTION_VERSION,
            invocation: InvocationId::new(),
            sequence: 2,
            actor: InteractionActor {
                id: "agent:scribe".into(),
                kind: ActorKind::Agent,
                for_person: None,
            },
            phase: InteractionPhase::Applying,
            action: InteractionAction {
                id: "format-range".into(),
                label: "Format range".into(),
                app: Some(ActionApp {
                    id: "org.tilo.Sheets".into(),
                    label: "Sheets".into(),
                }),
            },
            resource: ActionResource {
                id: "sheets.sheet:budget".into(),
                kind: "sheets.sheet".into(),
                label: "Budget".into(),
            },
            anchor,
            group: Some(ActionGroup {
                id: "task:42".into(),
                label: "Prepare budget".into(),
                kind: "task".into(),
            }),
        }
    }

    /// Versioned grid interactions round-trip, additive fields are ignored but unknown versions are
    /// refused, and the broker replaces claimed attribution and sequence.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_interaction_wire_is_versioned_and_broker_normalised() {
        versioned_grid_interaction_round_trips();
        additive_fields_are_ignored_but_unknown_versions_are_refused();
        broker_normalization_replaces_claimed_attribution_and_sequence();
    }

    fn versioned_grid_interaction_round_trips() {
        let interaction = example(InteractionAnchor::Grid {
            sheet: "sheet:budget".into(),
            ranges: vec![InteractionGridRange {
                start_col: 1,
                start_row: 2,
                end_col: 4,
                end_row: 8,
            }],
        });
        interaction.validate().expect("valid interaction");
        let json = serde_json::to_string(&interaction).expect("encode");
        let decoded: SemanticInteraction = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, interaction);
        assert!(!json.contains("pixel"));
        assert!(!json.contains("x\""));
    }

    fn additive_fields_are_ignored_but_unknown_versions_are_refused() {
        let interaction = example(InteractionAnchor::Resource);
        let mut value = serde_json::to_value(&interaction).expect("encode");
        value
            .as_object_mut()
            .expect("object")
            .insert("future_hint".into(), serde_json::json!(true));
        let decoded: SemanticInteraction = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded, interaction);

        let mut future = interaction;
        future.version = INTERACTION_VERSION + 1;
        assert_eq!(future.validate().unwrap_err().field, "version");
    }

    fn broker_normalization_replaces_claimed_attribution_and_sequence() {
        let mut claimed = example(InteractionAnchor::Resource);
        claimed.paint_origin = Some(uuid::Uuid::new_v4());
        let normalized = claimed.normalized_by_broker(
            InteractionActor {
                id: "agent:reviewer".into(),
                kind: ActorKind::Agent,
                for_person: None,
            },
            17,
        );
        assert_eq!(normalized.actor.id, "agent:reviewer");
        assert_eq!(normalized.sequence, 17);
        assert_eq!(
            normalized.paint_origin, None,
            "presence claims cannot mint original run custody"
        );
    }

    /// Anchors are non-empty, ordered and bounded; labels and terminal details are bounded; terminal
    /// phases are explicit; progress is labelled and in range.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn interaction_fields_are_explicit_and_bounded() {
        anchors_are_nonempty_ordered_and_bounded();
        labels_and_terminal_details_are_bounded();
        terminal_phases_are_explicit();
        progress_is_labelled_and_in_range();
    }

    fn anchors_are_nonempty_ordered_and_bounded() {
        let backwards = example(InteractionAnchor::Text {
            region: "body".into(),
            ranges: vec![InteractionTextRange {
                block: "block:1".into(),
                start: 9,
                end: 3,
            }],
        });
        assert_eq!(backwards.validate().unwrap_err().field, "anchor.range");

        let too_many = example(InteractionAnchor::Objects {
            ids: (0..=MAX_ANCHOR_ITEMS)
                .map(|n| format!("shape:{n}"))
                .collect(),
        });
        assert_eq!(too_many.validate().unwrap_err().field, "anchor.ids");

        let empty = example(InteractionAnchor::ListItems { ids: Vec::new() });
        assert_eq!(empty.validate().unwrap_err().field, "anchor.ids");
    }

    fn labels_and_terminal_details_are_bounded() {
        let mut oversized = example(InteractionAnchor::Resource);
        oversized.phase = InteractionPhase::Failed {
            code: Some("action_failed".into()),
            message: "x".repeat(MAX_DETAIL_BYTES + 1),
        };
        assert_eq!(oversized.validate().unwrap_err().field, "phase.message");

        oversized.phase = InteractionPhase::Committed;
        oversized.resource.label = "x".repeat(MAX_LABEL_BYTES + 1);
        assert_eq!(oversized.validate().unwrap_err().field, "resource.label");
    }

    fn terminal_phases_are_explicit() {
        assert!(!InteractionPhase::Locating.is_terminal());
        assert!(!InteractionPhase::Applying.is_terminal());
        assert!(
            !InteractionPhase::Progress {
                label: "Uploading".into(),
                completed: Some(3),
                total: Some(10),
            }
            .is_terminal()
        );
        assert!(InteractionPhase::Committed.is_terminal());
        assert!(
            InteractionPhase::Failed {
                code: None,
                message: "no".into()
            }
            .is_terminal()
        );
        assert!(InteractionPhase::Cancelled { message: None }.is_terminal());
    }

    fn progress_is_labelled_and_in_range() {
        let mut interaction = example(InteractionAnchor::Resource);
        interaction.phase = InteractionPhase::Progress {
            label: String::new(),
            completed: None,
            total: None,
        };
        assert_eq!(interaction.validate().unwrap_err().field, "phase.label");

        interaction.phase = InteractionPhase::Progress {
            label: "Indexing".into(),
            completed: Some(11),
            total: Some(10),
        };
        assert_eq!(interaction.validate().unwrap_err().field, "phase.progress");
    }
}

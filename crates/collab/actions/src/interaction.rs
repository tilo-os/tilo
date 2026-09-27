//! Semantic, transient presence for an action invocation.
//!
//! An agent does not have a mouse pointer to mirror.  This vocabulary names
//! the resource it is working on and the application-native selection that
//! resource understands.  Receivers resolve those stable anchors against
//! their own layout; pixels, viewport positions, and widget indices never
//! cross the wire.

use serde::{Deserialize, Serialize};

use crate::{ActionApp, ActionGroup, ActionResource, DoorError, InvocationId, InvocationUpdate};

/// Wire version understood by this crate.
pub const INTERACTION_VERSION: u16 = 2;
/// Hard ceiling for one JSON-encoded update at a trust boundary.
pub const MAX_INTERACTION_BYTES: usize = 32 * 1024;
pub const MAX_ANCHOR_ITEMS: usize = 64;
pub const MAX_ID_BYTES: usize = 512;
pub const MAX_LABEL_BYTES: usize = 512;
pub const MAX_DETAIL_BYTES: usize = 2 * 1024;

/// Authenticated principal kind.  This is assigned by the broker, never
/// inferred from a display name or an `agent:` string prefix.
///
/// `Person` is the human kind: it covers a person's pointer, keyboard and
/// touch activations of a bound control as well as anything they drive
/// through the action door, because the broker attributes both to the same
/// authenticated principal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionActorKind {
    Person,
    Agent,
}

/// Why an attempt ended without running. A rejection is terminal and left no
/// document change, which is exactly why it is carried as presence: nothing
/// else records that it was tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionRejection {
    /// The actor's permissions do not include this action here.
    PermissionDenied,
    /// The action is advertised but its precondition is unmet: the control
    /// is disabled.
    Unavailable,
    /// The supplied input failed the action's declared contract.
    Invalid,
    /// The action ran and changed nothing.
    NoChange,
}

/// Identity attached by the broker to the normalized interaction event.
/// Friendly names and colours remain roster-owned presentation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionActor {
    pub id: String,
    pub kind: InteractionActorKind,
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
#[serde(tag = "kind", rename_all = "snake_case")]
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

/// What the invocation is visibly doing.  Terminal details complement the
/// durable invocation result; they do not replace it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
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
    /// Refused before, or instead of, having any effect.
    Rejected {
        reason: InteractionRejection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

impl InteractionPhase {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Committed | Self::Failed { .. } | Self::Cancelled { .. } | Self::Rejected { .. }
        )
    }

    /// Whether the attempt ended without doing what was asked.
    pub fn is_unsuccessful(&self) -> bool {
        matches!(self, Self::Failed { .. } | Self::Rejected { .. })
    }

    /// The presence phase an invocation lifecycle update corresponds to.
    ///
    /// This is the one projection from an invocation's durable lifecycle onto
    /// its transient presence. An agent observing its broker invocation and an
    /// app observing a person's control activation report the same phase for
    /// the same update.
    pub fn from_update(update: &InvocationUpdate) -> Self {
        match update {
            InvocationUpdate::Accepted { .. } | InvocationUpdate::Item { .. } => Self::Applying,
            InvocationUpdate::Progress {
                phase,
                completed,
                total,
                ..
            } => Self::Progress {
                label: phase.clone(),
                completed: *completed,
                total: *total,
            },
            InvocationUpdate::Succeeded { .. } => Self::Committed,
            InvocationUpdate::Failed { error, .. } => Self::Failed {
                code: Some(error.code.clone()),
                message: error.message.clone(),
            },
            InvocationUpdate::Cancelled { message, .. } => Self::Cancelled {
                message: message.clone(),
            },
        }
    }

    /// The presence phase for an attempt the action door refused.
    ///
    /// A refusal that means "this was never going to run for you" (outside the
    /// actor's permission-filtered surface, precondition unmet, bad input)
    /// becomes [`Self::Rejected`]. A confirmation request is a wait, not a
    /// refusal. An action that ran and failed stays [`Self::Failed`].
    pub fn from_door_error(error: &DoorError) -> Self {
        let rejected = |reason, message: String| Self::Rejected {
            reason,
            message: Some(message),
        };
        match error {
            DoorError::Unknown(_) | DoorError::UnregisteredType(_) => {
                rejected(InteractionRejection::PermissionDenied, error.to_string())
            }
            DoorError::Unavailable { reason, .. } => {
                rejected(InteractionRejection::Unavailable, reason.clone())
            }
            DoorError::MissingArgument(_)
            | DoorError::UnexpectedArgument(_)
            | DoorError::InvalidArguments(_) => {
                rejected(InteractionRejection::Invalid, error.to_string())
            }
            DoorError::ConfirmationRequired => Self::Waiting {
                reason: Some(error.to_string()),
            },
            DoorError::Failed(message) => Self::Failed {
                code: Some(error.code().into()),
                message: message.clone(),
            },
        }
    }

    /// The presence phase for the session action host's receipt, the same
    /// lifecycle reached across the broker instead of through the local door.
    pub fn from_receipt(status: &crate::remote::ReceiptStatus) -> Self {
        use crate::remote::{ActionHostError, ReceiptStatus};
        match status {
            ReceiptStatus::Accepted => Self::Applying,
            ReceiptStatus::Completed { .. } => Self::Committed,
            ReceiptStatus::Failed { error } => Self::Failed {
                code: Some(error.code.clone()),
                message: error.message.clone(),
            },
            ReceiptStatus::Cancelled { message } => Self::Cancelled {
                message: message.clone(),
            },
            ReceiptStatus::Rejected { error } => {
                let reason = match error {
                    ActionHostError::ConfirmationRequired => {
                        return Self::Waiting {
                            reason: Some(error.to_string()),
                        };
                    }
                    ActionHostError::UnknownAction | ActionHostError::NotPermitted => {
                        InteractionRejection::PermissionDenied
                    }
                    ActionHostError::InvalidRequest { .. }
                    | ActionHostError::UnknownInvocation
                    | ActionHostError::InvocationConflict => InteractionRejection::Invalid,
                    ActionHostError::Unavailable { .. }
                    | ActionHostError::StaleRevision
                    | ActionHostError::NotCancellable { .. }
                    | ActionHostError::Refused { .. } => InteractionRejection::Unavailable,
                };
                Self::Rejected {
                    reason,
                    message: Some(error.to_string()),
                }
            }
        }
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
    /// This interaction as `audience` may receive it: the resource label is
    /// redacted by its declared sensitivity.
    #[must_use]
    pub fn redacted_for(mut self, audience: crate::Audience) -> Self {
        self.resource = self.resource.redacted_for(audience);
        self
    }

    /// Replace attribution with the identity authenticated by the broker.
    ///
    /// `actor` in decoded application/agent input is an untrusted claim. A
    /// broker must call this (and assign its own monotonic `sequence`) before
    /// validating or broadcasting the normalized event. Presence consumers
    /// may trust attribution only on broker-originated streams.
    #[must_use]
    pub fn normalized_by_broker(mut self, actor: InteractionActor, sequence: u64) -> Self {
        self.actor = actor;
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
        }
        | InteractionPhase::Rejected {
            message: Some(message),
            ..
        } => validate_detail("phase.message", message),
        InteractionPhase::Locating
        | InteractionPhase::Applying
        | InteractionPhase::Waiting { reason: None }
        | InteractionPhase::Committed
        | InteractionPhase::Cancelled { message: None }
        | InteractionPhase::Rejected { message: None, .. } => Ok(()),
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
mod tests {
    use super::*;

    fn example(anchor: InteractionAnchor) -> SemanticInteraction {
        SemanticInteraction {
            version: INTERACTION_VERSION,
            invocation: InvocationId::new(),
            sequence: 2,
            actor: InteractionActor {
                id: "agent:scribe".into(),
                kind: InteractionActorKind::Agent,
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
                sensitivity: crate::Sensitivity::Public,
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
        let claimed = example(InteractionAnchor::Resource);
        let normalized = claimed.normalized_by_broker(
            InteractionActor {
                id: "agent:reviewer".into(),
                kind: InteractionActorKind::Agent,
            },
            17,
        );
        assert_eq!(normalized.actor.id, "agent:reviewer");
        assert_eq!(normalized.sequence, 17);
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
        let rejected = InteractionPhase::Rejected {
            reason: InteractionRejection::Unavailable,
            message: None,
        };
        assert!(rejected.is_terminal());
        assert!(rejected.is_unsuccessful());
        assert!(!InteractionPhase::Committed.is_unsuccessful());
    }

    /// Door refusals are the attempts that leave no document trace; each one
    /// must surface as a terminal presence phase with its reason, and a
    /// confirmation request must not be mistaken for one.
    #[test]
    fn door_refusals_project_onto_rejections_with_their_reason() {
        let address = crate::ActionAddress::new("notes.page:1", "delete");
        let cases = [
            (
                DoorError::Unknown(address.clone()),
                Some(InteractionRejection::PermissionDenied),
            ),
            (
                DoorError::Unavailable {
                    code: "locked".into(),
                    reason: "The page is locked".into(),
                },
                Some(InteractionRejection::Unavailable),
            ),
            (
                DoorError::InvalidArguments("title: must not be empty".into()),
                Some(InteractionRejection::Invalid),
            ),
            (
                DoorError::MissingArgument(address),
                Some(InteractionRejection::Invalid),
            ),
        ];
        for (error, expected) in cases {
            let phase = InteractionPhase::from_door_error(&error);
            match (&phase, expected) {
                (
                    InteractionPhase::Rejected {
                        reason,
                        message: Some(message),
                    },
                    Some(expected),
                ) => {
                    assert_eq!(*reason, expected, "{error:?}");
                    assert!(!message.is_empty(), "the reason travels with {error:?}");
                }
                other => panic!("{error:?} projected to {other:?}"),
            }
            assert!(phase.is_terminal());
            let mut interaction = example(InteractionAnchor::Resource);
            interaction.phase = phase;
            interaction
                .validate()
                .expect("a rejection is a valid update");
        }
        assert!(matches!(
            InteractionPhase::from_door_error(&DoorError::ConfirmationRequired),
            InteractionPhase::Waiting { reason: Some(_) }
        ));
        assert!(matches!(
            InteractionPhase::from_door_error(&DoorError::Failed("disk full".into())),
            InteractionPhase::Failed { code: Some(code), message } if code == "action_failed" && message == "disk full"
        ));
    }

    /// A refusal from the session action host is the same kind of attempt
    /// as one the local door refuses, and carries its reason the same way.
    #[test]
    fn host_receipts_project_onto_the_same_rejections_as_the_door() {
        use crate::remote::{ActionHostError, ReceiptStatus};
        assert_eq!(
            InteractionPhase::from_receipt(&ReceiptStatus::Accepted),
            InteractionPhase::Applying
        );
        for (error, reason) in [
            (
                ActionHostError::NotPermitted,
                InteractionRejection::PermissionDenied,
            ),
            (
                ActionHostError::UnknownAction,
                InteractionRejection::PermissionDenied,
            ),
            (
                ActionHostError::InvalidRequest {
                    reason: "bad".into(),
                },
                InteractionRejection::Invalid,
            ),
            (
                ActionHostError::Unavailable {
                    reason: "offline".into(),
                },
                InteractionRejection::Unavailable,
            ),
        ] {
            let phase = InteractionPhase::from_receipt(&ReceiptStatus::Rejected {
                error: error.clone(),
            });
            assert_eq!(
                phase,
                InteractionPhase::Rejected {
                    reason,
                    message: Some(error.to_string()),
                }
            );
        }
        assert!(matches!(
            InteractionPhase::from_receipt(&ReceiptStatus::Rejected {
                error: ActionHostError::ConfirmationRequired,
            }),
            InteractionPhase::Waiting { .. }
        ));
    }

    #[test]
    fn invocation_updates_project_onto_the_same_phases_for_every_caller() {
        let invocation = InvocationId::new();
        assert_eq!(
            InteractionPhase::from_update(&InvocationUpdate::Accepted { invocation }),
            InteractionPhase::Applying
        );
        assert_eq!(
            InteractionPhase::from_update(&InvocationUpdate::Succeeded {
                invocation,
                output: crate::ActionOutput::default(),
            }),
            InteractionPhase::Committed
        );
        assert_eq!(
            InteractionPhase::from_update(&InvocationUpdate::Failed {
                invocation,
                error: crate::ActionFailure {
                    code: "timeout".into(),
                    message: "late".into(),
                    details: None,
                    retryable: true,
                },
            }),
            InteractionPhase::Failed {
                code: Some("timeout".into()),
                message: "late".into(),
            }
        );
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

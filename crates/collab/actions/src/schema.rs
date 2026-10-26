//! Participant-independent catalogue schema.
//!
//! A multi-participant app (Writer) builds a different resource tree for each
//! participant's caret, selection and Find hits. What it publishes to the
//! broker, and what grants are checked against, must not be one caret's
//! snapshot: that would churn the publication revision on every caret move
//! and would hide operations that another participant could use.
//!
//! Instead an app publishes its catalogue schema: stable addresses whose
//! context-dependent preconditions are declared as flags
//! ([`ContextRequirement`], carried in [`ActionContract::tags`]). At invoke
//! time the host rebuilds the invoker's own tree; an operation whose
//! precondition is not met there is refused with
//! [`ContextRequirement::refusal`].
//!
//! [`catalogue_schema`] reduces a surveyed catalogue to entries keyed by
//! resource kind rather than resource id, so volatile, document-derived ids
//! (a search hit, a selection range) never change the schema.

use serde::{Deserialize, Serialize};

use crate::{
    ActionContract, ActionDescriptor, ActionParam, ActionSpec, DoorError, ExecutionLocus,
    OperationKind,
};

/// Prefix of the contract tags that declare a context precondition.
pub const REQUIRES_TAG_PREFIX: &str = "tilo.requires.";

/// Contract tag of a View operation that changes only the invoking
/// participant's own view (its caret, selection or Find state). Only such an
/// operation may be invoked by a remote participant: every other View
/// operation moves the local person's private view and stays refused.
pub const PARTICIPANT_VIEW_TAG: &str = "tilo.participant-view";

/// A precondition on the invoking participant's view. Declared on the
/// published schema as an availability flag; checked against the invoker's
/// own view at invoke time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextRequirement {
    /// The participant has a caret in an editable region.
    Caret,
    /// The participant has a non-empty selection.
    Selection,
    /// The participant has live Find results.
    FindHits,
}

impl ContextRequirement {
    pub const ALL: [Self; 3] = [Self::Caret, Self::Selection, Self::FindHits];

    /// The contract tag declaring this requirement.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Caret => "tilo.requires.caret",
            Self::Selection => "tilo.requires.selection",
            Self::FindHits => "tilo.requires.find-hits",
        }
    }

    /// Stable refusal code, e.g. `needs-selection`.
    pub const fn refusal_code(self) -> &'static str {
        match self {
            Self::Caret => "needs-caret",
            Self::Selection => "needs-selection",
            Self::FindHits => "needs-find-hits",
        }
    }

    /// The refusal for an invocation whose invoker's view does not meet
    /// this requirement.
    pub fn refusal(self) -> DoorError {
        let reason = match self {
            Self::Caret => "Place the caret in the document first",
            Self::Selection => "Select some text first",
            Self::FindHits => "Find something first",
        };
        DoorError::Unavailable {
            code: self.refusal_code().into(),
            reason: reason.into(),
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|requirement| requirement.tag() == tag)
    }
}

impl ActionContract {
    /// The context preconditions declared on this contract, in order.
    pub fn requirements(&self) -> Vec<ContextRequirement> {
        let mut requirements: Vec<_> = self
            .tags
            .iter()
            .filter_map(|tag| ContextRequirement::from_tag(tag))
            .collect();
        requirements.sort_unstable();
        requirements.dedup();
        requirements
    }

    /// Whether this View contract changes only the invoking participant's
    /// view ([`PARTICIPANT_VIEW_TAG`]).
    pub fn is_participant_view(&self) -> bool {
        self.tags.iter().any(|tag| tag == PARTICIPANT_VIEW_TAG)
    }

    fn add_tag(&mut self, tag: &str) {
        if !self.tags.iter().any(|existing| existing == tag) {
            self.tags.push(tag.to_owned());
        }
    }
}

impl ActionSpec {
    /// Declare a precondition on the invoker's view. The operation stays at
    /// its stable address in every participant's tree and in the published
    /// schema; the invoker's tree refuses it when the precondition fails.
    #[must_use]
    pub fn requires(mut self, requirement: ContextRequirement) -> Self {
        self.type_mut().contract.add_tag(requirement.tag());
        self
    }

    /// Declare that this View operation changes only the invoking
    /// participant's own view, so a remote participant may invoke it.
    #[must_use]
    pub fn participant_view(mut self) -> Self {
        self.type_mut().contract.add_tag(PARTICIPANT_VIEW_TAG);
        self
    }
}

/// One participant-independent catalogue entry. Keyed by resource kind and
/// action id, never by the (possibly document- or view-derived) resource id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueSchemaEntry {
    pub resource_kind: String,
    pub action: String,
    pub kind: OperationKind,
    pub locus: ExecutionLocus,
    pub contract: ActionContract,
    pub params: Vec<ActionParam>,
}

impl CatalogueSchemaEntry {
    pub fn of(descriptor: &ActionDescriptor) -> Self {
        Self {
            resource_kind: descriptor.resource_kind.clone(),
            action: descriptor.address.action.clone(),
            kind: descriptor.kind,
            locus: descriptor.locus,
            contract: descriptor.contract.clone(),
            params: descriptor.params.clone(),
        }
    }

    fn sort_key(&self) -> (String, String, String) {
        (
            self.resource_kind.clone(),
            self.action.clone(),
            serde_json::to_string(self).unwrap_or_default(),
        )
    }
}

/// Reduce a surveyed catalogue to its participant-independent schema:
/// entries keyed by resource kind, sorted, duplicates removed. Two
/// catalogues that differ only in volatile resource ids, labels or order
/// have the same schema.
pub fn catalogue_schema(descriptors: &[ActionDescriptor]) -> Vec<CatalogueSchemaEntry> {
    let mut entries: Vec<_> = descriptors
        .iter()
        .map(CatalogueSchemaEntry::of)
        .map(|entry| (entry.sort_key(), entry))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup_by(|a, b| a.0 == b.0);
    entries.into_iter().map(|(_, entry)| entry).collect()
}

/// Canonical bytes of a schema, for a host to digest into a stable
/// publication revision or a frozen grant schema digest.
pub fn catalogue_schema_bytes(entries: &[CatalogueSchemaEntry]) -> Vec<u8> {
    serde_json::to_vec(entries).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EncodedAction, Resource, door};

    fn encoded(tag: &str) -> EncodedAction {
        EncodedAction {
            type_id: "fixture.action".into(),
            payload: tag.as_bytes().to_vec(),
        }
    }

    /// Minimal multi-participant provider: each participant has its own
    /// caret block and optional selection.
    struct TwoViews {
        views: std::collections::HashMap<String, (String, bool)>,
        own: (String, bool),
    }

    fn tree(caret: &str, selected: bool, schema: bool) -> Vec<Resource> {
        let mut root = Resource::new("doc", "fixture.document", "Document").with_action(
            ActionSpec::immediate_encoded(
                "type-text",
                "Type",
                ExecutionLocus::ClientLocal,
                encoded(&format!("type@{caret}")),
            )
            .requires(ContextRequirement::Caret),
        );
        if schema || selected {
            root = root.with_action(
                ActionSpec::immediate_encoded(
                    "toggle-mark",
                    "Bold",
                    ExecutionLocus::ClientLocal,
                    encoded(&format!("bold@{caret}")),
                )
                .requires(ContextRequirement::Selection),
            );
        }
        if !schema {
            // Volatile, view-derived resource ids never enter the schema.
            root = root.with_child(
                Resource::new(format!("doc.caret:{caret}"), "fixture.caret", "Caret").with_action(
                    ActionSpec::immediate_encoded(
                        "go-to",
                        "Go to",
                        ExecutionLocus::ClientLocal,
                        encoded(caret),
                    ),
                ),
            );
        }
        vec![root]
    }

    impl TwoViews {
        /// The tree `participant` sees; `None` is the local window's.
        fn resources_for(&self, participant: Option<&crate::ParticipantId>) -> Vec<Resource> {
            let Some(participant) = participant else {
                return tree(&self.own.0, self.own.1, false);
            };
            self.views
                .get(participant.as_str())
                .map_or_else(Vec::new, |(caret, selected)| tree(caret, *selected, false))
        }
    }

    /// The participant-independent schema every view publishes.
    fn schema_tree() -> Vec<Resource> {
        let mut schema = tree("", false, true);
        // The schema's go-to lives on the caret resource kind.
        schema.push(
            Resource::new("doc.caret", "fixture.caret", "Caret").with_action(
                ActionSpec::immediate_encoded(
                    "go-to",
                    "Go to",
                    ExecutionLocus::ClientLocal,
                    encoded(""),
                ),
            ),
        );
        schema
    }

    fn provider(agent_caret: &str, agent_selected: bool) -> TwoViews {
        TwoViews {
            views: [(
                "agent-task:t".to_string(),
                (agent_caret.to_string(), agent_selected),
            )]
            .into(),
            own: ("b1".into(), false),
        }
    }

    #[test]
    fn requirement_flags_round_trip_through_contract_tags() {
        let spec =
            ActionSpec::immediate_encoded("x", "X", ExecutionLocus::ClientLocal, encoded("x"))
                .requires(ContextRequirement::Selection)
                .requires(ContextRequirement::Selection)
                .requires(ContextRequirement::Caret)
                .participant_view();
        assert_eq!(
            spec.contract.requirements(),
            vec![ContextRequirement::Caret, ContextRequirement::Selection]
        );
        assert!(spec.contract.is_participant_view());
        assert_eq!(spec.contract.tags.len(), 3, "tags are not duplicated");
        assert!(matches!(
            ContextRequirement::Selection.refusal(),
            DoorError::Unavailable { ref code, .. } if code == "needs-selection"
        ));
        let json = serde_json::to_value(&spec.contract).unwrap();
        let back: ActionContract = serde_json::from_value(json).unwrap();
        assert_eq!(back.requirements(), spec.contract.requirements());
    }

    #[test]
    fn schema_is_stable_across_caret_moves_and_selection() {
        let before = provider("b1", false);
        let after = provider("b7", true);
        let participant = crate::ParticipantId::new("agent-task:t").unwrap();
        // The participant's own trees differ (caret-derived ids, selection).
        assert_ne!(
            door::survey(&before.resources_for(Some(&participant))).len(),
            door::survey(&after.resources_for(Some(&participant))).len()
        );
        let schema = catalogue_schema(&door::survey(&schema_tree()));
        // Every operation a participant can reach has an entry in the schema,
        // whatever its caret: grants checked against the schema cover it.
        for provider in [&before, &after] {
            for entry in
                catalogue_schema(&door::survey(&provider.resources_for(Some(&participant))))
            {
                assert!(
                    schema
                        .iter()
                        .any(|schema| schema.resource_kind == entry.resource_kind
                            && schema.action == entry.action
                            && schema.contract == entry.contract),
                    "{} {} missing from schema",
                    entry.resource_kind,
                    entry.action
                );
            }
        }
    }

    #[test]
    fn schema_ignores_volatile_ids_labels_and_order() {
        let a = door::survey(&tree("b1", false, false));
        let mut b = door::survey(&tree("b99", false, false));
        b.reverse();
        for descriptor in &mut b {
            descriptor.resource_label = "Renamed".into();
        }
        assert_eq!(catalogue_schema(&a), catalogue_schema(&b));
    }

    #[test]
    fn resolution_is_per_invoker() {
        let provider = provider("b7", true);
        let agent = crate::ParticipantId::new("agent-task:t").unwrap();
        let address = door::ActionAddress::new("doc", "type-text");
        let agent_tree = provider.resources_for(Some(&agent));
        let own_tree = provider.resources_for(None);
        assert_eq!(
            door::encode(&agent_tree, &address, None).unwrap().payload,
            b"type@b7".to_vec()
        );
        assert_eq!(
            door::encode(&own_tree, &address, None).unwrap().payload,
            b"type@b1".to_vec()
        );
        // Selection-scoped ops exist only in the tree of the participant
        // that has a selection.
        let bold = door::ActionAddress::new("doc", "toggle-mark");
        assert!(door::resolve(&agent_tree, &bold).is_some());
        assert!(door::resolve(&own_tree, &bold).is_none());
        // An unknown participant gets no tree at all, not the default view.
        let stranger = crate::ParticipantId::new("agent-task:other").unwrap();
        assert!(provider.resources_for(Some(&stranger)).is_empty());
    }
}

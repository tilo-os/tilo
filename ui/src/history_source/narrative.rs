//! Grounded prose for passive and active history surfaces.
//!
//! The deterministic digest remains authoritative. A local or remote language
//! model may rewrite it, but must return the exact source ids it used; results
//! that cite anything outside the request are rejected rather than displayed.

use std::collections::HashSet;

use crate::{ActivityDigest, HistoryEntryId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarrativeRequest {
    pub deterministic_text: String,
    pub source_ids: Vec<HistoryEntryId>,
}

impl NarrativeRequest {
    pub fn from_digest(digest: &ActivityDigest) -> Option<Self> {
        let deterministic_text = digest.headline()?;
        let source_ids = digest
            .briefs
            .iter()
            .flat_map(|brief| brief.entry_ids.iter().copied())
            .collect();
        Some(Self {
            deterministic_text,
            source_ids,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroundedNarrative {
    pub text: String,
    pub source_ids: Vec<HistoryEntryId>,
    pub generator: String,
}

pub trait ActivityNarrator {
    fn summarize(&self, request: &NarrativeRequest) -> Option<GroundedNarrative>;
}

pub fn validate_narrative(
    request: &NarrativeRequest,
    narrative: GroundedNarrative,
) -> Option<GroundedNarrative> {
    let allowed: HashSet<_> = request.source_ids.iter().copied().collect();
    (!narrative.text.trim().is_empty()
        && !narrative.source_ids.is_empty()
        && narrative.source_ids.iter().all(|id| allowed.contains(id)))
    .then_some(narrative)
}

/// Privacy-preserving fallback used when no model is configured or when a
/// generated result fails grounding validation.
pub fn deterministic_narrative(request: &NarrativeRequest) -> GroundedNarrative {
    GroundedNarrative {
        text: request.deterministic_text.clone(),
        source_ids: request.source_ids.clone(),
        generator: "qos-deterministic".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_ungrounded_generated_claims() {
        let request = NarrativeRequest {
            deterministic_text: "Ada updated the brief".into(),
            source_ids: vec![HistoryEntryId([1; 16])],
        };
        let invented = GroundedNarrative {
            text: "The launch was approved".into(),
            source_ids: vec![HistoryEntryId([2; 16])],
            generator: "test-model".into(),
        };
        assert!(validate_narrative(&request, invented).is_none());
    }
}

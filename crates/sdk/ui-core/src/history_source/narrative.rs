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
    /// Factual, already-grouped evidence supplied to a model. The model is a
    /// prose renderer, never an authority over the event graph.
    pub sources: Vec<NarrativeSource>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarrativeSource {
    pub ids: Vec<HistoryEntryId>,
    pub title: String,
    pub detail: String,
    pub authors: Vec<String>,
    pub changes: usize,
}

impl NarrativeRequest {
    pub fn from_digest(digest: &ActivityDigest) -> Option<Self> {
        let deterministic_text = digest.headline()?;
        let source_ids = digest
            .briefs
            .iter()
            .flat_map(|brief| brief.entry_ids.iter().copied())
            .collect();
        let sources = digest
            .briefs
            .iter()
            .map(|brief| NarrativeSource {
                ids: brief.entry_ids.clone(),
                title: brief.title.clone(),
                detail: brief.detail.clone(),
                authors: brief
                    .authors
                    .iter()
                    .map(|author| author.name.clone())
                    .collect(),
                changes: brief.changes,
            })
            .collect();
        Some(Self {
            deterministic_text,
            source_ids,
            sources,
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
        && narrative.text.chars().count() <= 320
        && !narrative.text.chars().any(char::is_control)
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
            sources: Vec::new(),
        };
        let invented = GroundedNarrative {
            text: "The launch was approved".into(),
            source_ids: vec![HistoryEntryId([2; 16])],
            generator: "test-model".into(),
        };
        assert!(validate_narrative(&request, invented).is_none());
    }
}

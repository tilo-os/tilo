//! Grounded prose for passive and active history surfaces.
//!
//! The deterministic digest remains authoritative. A local or remote language
//! model may rewrite it, but must return the exact source ids it used; results
//! that cite anything outside the supplied prompt are rejected. Citation membership
//! is not factual verification; generated prose is never history evidence.

use std::collections::HashSet;

use crate::{ActivityDigest, HistoryEntryId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NarrativeRequest {
    pub deterministic_text: String,
    /// Projection identity (scope, committed frontiers and coverage), never raw documents.
    pub projection: String,
    pub locale: String,
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
    /// The prompt and validator share this bound: omitted briefs cannot be cited.
    pub fn supplied_ids(&self) -> HashSet<HistoryEntryId> {
        let allowed: HashSet<_> = self.source_ids.iter().copied().collect();
        self.sources
            .iter()
            .filter(|source| source.ids.iter().any(|id| allowed.contains(id)))
            .take(24)
            .flat_map(|source| {
                source
                    .ids
                    .iter()
                    .copied()
                    .filter(|id| allowed.contains(id))
                    .take(64)
            })
            .filter(|id| allowed.contains(id))
            .collect()
    }

    pub fn from_digest(digest: &ActivityDigest) -> Option<Self> {
        let first = digest.briefs.first()?;
        let deterministic_text = match digest.briefs.as_slice() {
            [_] => format!("Work centered on {}.", first.title),
            [first, second] => {
                format!("Work centered on {} and {}.", first.title, second.title)
            }
            [first, second, rest @ ..] => format!(
                "Work centered on {}, {}, and {} more.",
                first.title,
                second.title,
                rest.len()
            ),
            [] => return None,
        };
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
            projection: String::new(),
            locale: crate::i18n::I18nManager::current_locale(),
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
    /// Exact request consumed by the runner; required for generated output.
    pub request: Option<Box<NarrativeRequest>>,
}

pub trait ActivityNarrator {
    fn summarize(&self, request: &NarrativeRequest) -> Option<GroundedNarrative>;
}

pub fn validate_narrative(
    request: &NarrativeRequest,
    narrative: GroundedNarrative,
) -> Option<GroundedNarrative> {
    let allowed = request.supplied_ids();
    (!narrative.text.trim().is_empty()
        && narrative.text.chars().count() <= 320
        && !narrative.text.chars().any(char::is_control)
        && !narrative.generator.trim().is_empty()
        && narrative.generator.chars().count() <= 256
        && !narrative.generator.chars().any(char::is_control)
        && narrative.request.as_deref() == Some(request)
        && !narrative.source_ids.is_empty()
        && narrative.source_ids.len() <= 6
        && narrative
            .source_ids
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            == narrative.source_ids.len()
        && narrative.source_ids.iter().all(|id| allowed.contains(id)))
    .then_some(narrative)
}

/// Privacy-preserving fallback used when no model is configured or when a
/// generated result fails grounding validation.
pub fn deterministic_narrative(request: &NarrativeRequest) -> GroundedNarrative {
    GroundedNarrative {
        text: request.deterministic_text.clone(),
        source_ids: request.source_ids.clone(),
        generator: "tilo-deterministic".to_string(),
        request: Some(Box::new(request.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_source::digest::{ActivityBrief, ActivityDigest};
    use crate::time::SystemTime;
    use crate::widget::history::HistoryAuthor;

    fn brief(id: u8, title: &str) -> ActivityBrief {
        ActivityBrief {
            title: title.into(),
            detail: "2 changes".into(),
            authors: vec![HistoryAuthor::new("Ada", 120.0, "ada")],
            apps: vec!["Notes".into()],
            resources: vec!["Launch brief".into()],
            changes: 2,
            actions: Vec::new(),
            first_at: SystemTime::UNIX_EPOCH,
            last_at: SystemTime::UNIX_EPOCH,
            group_id: None,
            work_session_id: None,
            entry_ids: vec![HistoryEntryId([id; 16])],
        }
    }

    #[test]
    fn citations_check_membership_not_truth_and_reject_stale_or_omitted_sources() {
        let digest = ActivityDigest {
            briefs: (1..=25).map(|id| brief(id, "Recorded work")).collect(),
            total_changes: 50,
            people: 1,
        };
        let request = NarrativeRequest::from_digest(&digest).unwrap();
        let narrative = GroundedNarrative {
            text: "A model claim that membership alone cannot verify".into(),
            source_ids: vec![HistoryEntryId([1; 16])],
            generator: "configured model".into(),
            request: Some(Box::new(request.clone())),
        };
        assert!(validate_narrative(&request, narrative.clone()).is_some());
        let mut omitted = narrative.clone();
        omitted.source_ids = vec![HistoryEntryId([25; 16])];
        assert!(validate_narrative(&request, omitted).is_none());
        let mut duplicate = narrative.clone();
        duplicate.source_ids.push(HistoryEntryId([1; 16]));
        assert!(validate_narrative(&request, duplicate).is_none());
        let mut excessive = narrative.clone();
        excessive.source_ids = (1..=7).map(|id| HistoryEntryId([id; 16])).collect();
        assert!(validate_narrative(&request, excessive).is_none());
        for field in ["scope", "coverage", "frontier"] {
            let mut changed = request.clone();
            changed.projection = field.into();
            assert!(validate_narrative(&changed, narrative.clone()).is_none());
        }
        let mut changed = request;
        changed.locale = "de-DE".into();
        assert!(validate_narrative(&changed, narrative).is_none());
    }

    #[test]
    fn narratives_summarise_work_and_refuse_ungrounded_claims() {
        // phase: deterministic_copy_summarizes_work_instead_of_repeating_a_glance_label
        {
            let digest = ActivityDigest {
                briefs: vec![
                    brief(1, "Onboard Sarah"),
                    brief(2, "Write Q3 financial report"),
                ],
                total_changes: 4,
                people: 2,
            };
            let request = NarrativeRequest::from_digest(&digest).expect("two briefs");
            assert_eq!(
                request.deterministic_text,
                "Work centered on Onboard Sarah and Write Q3 financial report."
            );
        }
        // phase: refuses_ungrounded_generated_claims
        {
            let request = NarrativeRequest {
                deterministic_text: "Ada updated the brief".into(),
                projection: String::new(),
                locale: "en-US".into(),
                source_ids: vec![HistoryEntryId([1; 16])],
                sources: Vec::new(),
            };
            let invented = GroundedNarrative {
                text: "The launch was approved".into(),
                source_ids: vec![HistoryEntryId([2; 16])],
                generator: "test-model".into(),
                request: Some(Box::new(request.clone())),
            };
            assert!(validate_narrative(&request, invented).is_none());
        }
    }
}

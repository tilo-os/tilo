//! Laying a model's proposals over the deterministic floor.
//!
//! The floor is never weakened: a model can add redactions and replace a
//! step's title, but it cannot remove or lower a floor proposal. Titles are
//! masked last, so no title shows text that one of the final proposals
//! redacts, whoever wrote the title.

use std::collections::BTreeMap;

use crate::detect::scan;
use crate::{
    MASK, MAX_TITLE_CHARS, RedactionBasis, RedactionProposal, ReviewProposals, ReviewStep,
    ReviewTraceRequest, Span, StepKind, StepOutcome, StepTitle, TitleBasis,
};

/// The walk-through title of `step` built from its kind, outcome and label.
#[must_use]
pub fn label_title(step: &ReviewStep) -> String {
    let action = step.action.trim();
    let title = match (step.kind, step.outcome) {
        (StepKind::Opened, _) => format!("You opened {}", step.app.trim()),
        (StepKind::Invoked, StepOutcome::Done) => format!("You pressed {action}"),
        (StepKind::Invoked, StepOutcome::Rejected) => {
            format!("You pressed {action}; it was refused")
        }
        (StepKind::Invoked, StepOutcome::Failed) => format!("You pressed {action}; it failed"),
        (StepKind::Edited, _) => format!("You edited \u{201C}{action}\u{201D}"),
        (StepKind::Appeared, _) => format!("\u{201C}{action}\u{201D} appeared"),
    };
    if title.chars().nth(MAX_TITLE_CHARS).is_none() {
        title
    } else {
        let mut short: String = title
            .chars()
            .take(MAX_TITLE_CHARS.saturating_sub(1))
            .collect();
        short.push('\u{2026}');
        short
    }
}

fn mask_ranges(title: &str, mut ranges: Vec<(usize, usize)>) -> String {
    ranges.sort_unstable();
    let mut masked = String::with_capacity(title.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        let start = start.max(cursor);
        if start >= end {
            continue;
        }
        masked.push_str(title.get(cursor..start).unwrap_or_default());
        let hidden = title.get(start..end).unwrap_or_default().chars().count();
        masked.extend(std::iter::repeat_n(MASK, hidden));
        cursor = end;
    }
    masked.push_str(title.get(cursor..).unwrap_or_default());
    masked
}

/// `title` with every redacted value, and anything the pattern detectors
/// find in the title itself, replaced by [`MASK`] per character.
fn masked_title(title: &str, redacted: &[&str]) -> String {
    let mut ranges: Vec<(usize, usize)> = redacted
        .iter()
        .filter(|value| value.chars().nth(1).is_some())
        .flat_map(|value| {
            title
                .match_indices(value)
                .map(|(start, found)| (start, start.saturating_add(found.len())))
        })
        .collect();
    ranges.extend(scan(title).into_iter().map(|hit| (hit.start, hit.end)));
    mask_ranges(title, ranges)
}

/// Merge `floor` and an optional model answer into review's proposals.
///
/// Identical spans collapse into one proposal that keeps the strongest
/// basis (declared, then pattern, then model) and the highest confidence.
/// Anything inside a declared value is already covered by it and is dropped.
/// Every step gets a title: the model's where it wrote one, otherwise one
/// built from the step's label.
#[must_use]
pub fn merge(
    request: &ReviewTraceRequest,
    floor: Vec<RedactionProposal>,
    model: Option<ReviewProposals>,
) -> ReviewProposals {
    let (model_redactions, model_titles) =
        model.map_or_else(Default::default, |model| (model.redactions, model.titles));

    let mut unique: BTreeMap<(u32, String, Span), RedactionProposal> = BTreeMap::new();
    for proposal in floor.into_iter().chain(model_redactions) {
        if request.redacted(&proposal).is_none() {
            continue;
        }
        let key = (proposal.step, proposal.node.clone(), proposal.span);
        match unique.get_mut(&key) {
            Some(existing) => {
                let confidence = existing.confidence.max(proposal.confidence);
                if proposal.basis < existing.basis {
                    *existing = proposal;
                }
                existing.confidence = confidence;
            }
            None => {
                unique.insert(key, proposal);
            }
        }
    }
    let declared: Vec<(u32, String, Span)> = unique
        .values()
        .filter(|proposal| proposal.basis == RedactionBasis::Declared)
        .map(|proposal| (proposal.step, proposal.node.clone(), proposal.span))
        .collect();
    let redactions: Vec<RedactionProposal> = unique
        .into_values()
        .filter(|proposal| {
            proposal.basis == RedactionBasis::Declared
                || !declared.iter().any(|(step, node, span)| {
                    *step == proposal.step && *node == proposal.node && span.contains(proposal.span)
                })
        })
        .collect();

    let mut redacted: Vec<&str> = redactions
        .iter()
        .filter_map(|proposal| request.redacted(proposal))
        .collect();
    // Longest first, so a value is masked whole before a part of it is.
    redacted.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    redacted.dedup();

    let mut written: BTreeMap<u32, String> = model_titles
        .into_iter()
        .filter(|title| title.basis == TitleBasis::Model)
        .map(|title| (title.step, title.title))
        .collect();
    let titles = request
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| {
            let index = u32::try_from(index).ok()?;
            let (title, basis) = written.remove(&index).map_or_else(
                || (label_title(step), TitleBasis::ActionLabel),
                |title| (title, TitleBasis::Model),
            );
            Some(StepTitle {
                step: index,
                title: masked_title(&title, &redacted),
                basis,
            })
        })
        .collect();

    ReviewProposals { redactions, titles }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{request, step, text};
    use crate::{Confidence, DeclaredSensitivity, RedactionCategory, floor, review_without_model};

    fn model_redaction(
        step: u32,
        node: &str,
        start: u32,
        end: u32,
        confidence: u8,
    ) -> RedactionProposal {
        RedactionProposal {
            step,
            node: node.into(),
            span: Span { start, end },
            category: RedactionCategory::Name,
            confidence: Confidence::new(confidence).unwrap(),
            basis: RedactionBasis::Model,
        }
    }

    fn mail_trace() -> ReviewTraceRequest {
        let mut failed = step(StepKind::Invoked, "Send", Vec::new());
        failed.outcome = StepOutcome::Failed;
        request(vec![
            step(StepKind::Opened, "Open", Vec::new()),
            step(
                StepKind::Edited,
                "To",
                vec![
                    text("r:to", DeclaredSensitivity::Personal, "Jana Novák"),
                    text(
                        "r:body",
                        DeclaredSensitivity::Public,
                        "Hi Petr, see jana@example.cz",
                    ),
                ],
            ),
            failed,
            step(StepKind::Appeared, "Couldn\u{2019}t send", Vec::new()),
        ])
    }

    #[test]
    fn without_a_model_every_step_is_titled_from_its_label() {
        let review = review_without_model(&mail_trace());
        let titles: Vec<&str> = review
            .titles
            .iter()
            .map(|title| title.title.as_str())
            .collect();
        assert_eq!(
            titles,
            [
                "You opened Mail",
                "You edited \u{201C}To\u{201D}",
                "You pressed Send; it failed",
                "\u{201C}Couldn\u{2019}t send\u{201D} appeared",
            ]
        );
        assert!(
            review
                .titles
                .iter()
                .all(|title| title.basis == TitleBasis::ActionLabel)
        );
        assert_eq!(review.redactions.len(), 2);
    }

    #[test]
    fn a_model_adds_to_the_floor_and_never_removes_or_lowers_it() {
        let trace = mail_trace();
        let floor = floor(&trace);
        let email = floor
            .iter()
            .find(|p| p.category == RedactionCategory::Email)
            .cloned()
            .unwrap();
        let mut weaker = email.clone();
        weaker.basis = RedactionBasis::Model;
        weaker.confidence = Confidence::new(10).unwrap();
        weaker.category = RedactionCategory::FreeFormPersonal;
        let model = ReviewProposals {
            redactions: vec![
                // "Petr" in the public body: only the model can know.
                model_redaction(1, "r:body", 3, 7, 80),
                // Inside the declared value: already covered.
                model_redaction(1, "r:to", 0, 4, 90),
                weaker,
            ],
            titles: Vec::new(),
        };
        let merged = merge(&trace, floor, Some(model));
        assert_eq!(merged.redactions.len(), 3);
        let same = merged
            .redactions
            .iter()
            .find(|p| p.span == email.span && p.node == "r:body")
            .unwrap();
        assert_eq!(same.basis, RedactionBasis::Pattern);
        assert_eq!(same.category, RedactionCategory::Email);
        assert_eq!(same.confidence, email.confidence);
        assert!(
            merged
                .redactions
                .iter()
                .any(|p| p.basis == RedactionBasis::Model && trace.redacted(p) == Some("Petr"))
        );
        assert!(
            !merged
                .redactions
                .iter()
                .any(|p| p.node == "r:to" && p.basis == RedactionBasis::Model)
        );
    }

    #[test]
    fn model_titles_replace_label_titles_and_are_masked() {
        let trace = mail_trace();
        let model = ReviewProposals {
            redactions: vec![model_redaction(1, "r:body", 3, 7, 80)],
            titles: vec![
                StepTitle {
                    step: 1,
                    title: "You wrote to Jana Novák and Petr".into(),
                    basis: TitleBasis::Model,
                },
                StepTitle {
                    step: 2,
                    title: "You pressed Send (+49 30 1234 5678)".into(),
                    basis: TitleBasis::Model,
                },
            ],
        };
        let merged = merge(&trace, floor(&trace), Some(model));
        let title = |step: u32| merged.titles.iter().find(|t| t.step == step).unwrap();
        assert_eq!(
            title(1).title,
            "You wrote to \u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE} and \u{25AE}\u{25AE}\u{25AE}\u{25AE}"
        );
        assert_eq!(title(1).basis, TitleBasis::Model);
        assert!(!title(2).title.contains("1234"));
        assert_eq!(title(3).basis, TitleBasis::ActionLabel);
    }

    #[test]
    fn a_proposal_outside_the_request_is_dropped() {
        let trace = mail_trace();
        let model = ReviewProposals {
            redactions: vec![
                model_redaction(9, "r:body", 0, 2, 80),
                model_redaction(1, "r:body", 0, 400, 80),
            ],
            titles: Vec::new(),
        };
        assert_eq!(
            merge(&trace, Vec::new(), Some(model)).redactions,
            Vec::new()
        );
    }
}

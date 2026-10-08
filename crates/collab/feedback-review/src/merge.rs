//! Laying a model's proposals over the deterministic floor.
//!
//! The floor is never weakened: a model can add redactions and replace a
//! step's title, but it cannot remove or lower a floor proposal. Titles are
//! masked last, so no title shows text that one of the final proposals
//! redacts, whoever wrote the title.

use std::collections::BTreeMap;

use crate::detect::scan;
use crate::{
    LabelTitle, MASK, RedactionBasis, RedactionProposal, ReviewProposals, ReviewStep,
    ReviewTraceRequest, Span, StepKind, StepOutcome, StepTitle, TitleText,
};

/// The walk-through title of `step` built from its kind, outcome and label,
/// for the caller to localize.
#[must_use]
pub fn label_title(step: &ReviewStep) -> LabelTitle {
    let action = step.action.trim().to_owned();
    match (step.kind, step.outcome) {
        (StepKind::Opened, _) => LabelTitle::Opened {
            app: step.app.trim().to_owned(),
        },
        (StepKind::Invoked, StepOutcome::Done) => LabelTitle::Invoked { action },
        (StepKind::Invoked, StepOutcome::Rejected) => LabelTitle::Refused { action },
        (StepKind::Invoked, StepOutcome::Failed) => LabelTitle::Failed { action },
        (StepKind::Edited, _) => LabelTitle::Edited { field: action },
        (StepKind::Appeared, _) => LabelTitle::Appeared { label: action },
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

    let redactions = merge_redactions(request, floor, model_redactions);

    let mut redacted: Vec<&str> = redactions
        .iter()
        .filter_map(|proposal| request.redacted(proposal))
        .collect();
    // Longest first, so a value is masked whole before a part of it is.
    redacted.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
    redacted.dedup();

    // Only a model's own text replaces a step's title; a model cannot
    // submit a label title.
    let mut written: BTreeMap<u32, String> = model_titles
        .into_iter()
        .filter_map(|title| match title.title {
            TitleText::Model { text } => Some((title.step, text)),
            TitleText::Label(_) => None,
        })
        .collect();
    let titles = request
        .steps
        .iter()
        .enumerate()
        .filter_map(|(index, step)| {
            let index = u32::try_from(index).ok()?;
            let title = if let Some(text) = written.remove(&index) {
                TitleText::Model {
                    text: masked_title(&text, &redacted),
                }
            } else {
                let mut label = label_title(step);
                let masked = masked_title(label.argument_mut(), &redacted);
                *label.argument_mut() = masked;
                TitleText::Label(label)
            };
            Some(StepTitle { step: index, title })
        })
        .collect();

    ReviewProposals { redactions, titles }
}

/// Collapse and validate proposal addresses identically for text-only review
/// and a recording review with model proposals and walkthrough titles.
pub(crate) fn merge_redactions(
    request: &ReviewTraceRequest,
    floor: Vec<RedactionProposal>,
    model_redactions: Vec<RedactionProposal>,
) -> Vec<RedactionProposal> {
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
    unique
        .into_values()
        .filter(|proposal| {
            proposal.basis == RedactionBasis::Declared
                || !declared.iter().any(|(step, node, span)| {
                    *step == proposal.step && *node == proposal.node && span.contains(proposal.span)
                })
        })
        .collect()
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
        let review = review_without_model(&mail_trace()).unwrap();
        let titles: Vec<TitleText> = review
            .titles
            .iter()
            .map(|title| title.title.clone())
            .collect();
        assert_eq!(
            titles,
            [
                TitleText::Label(LabelTitle::Opened { app: "Mail".into() }),
                TitleText::Label(LabelTitle::Edited { field: "To".into() }),
                TitleText::Label(LabelTitle::Failed {
                    action: "Send".into()
                }),
                TitleText::Label(LabelTitle::Appeared {
                    label: "Couldn\u{2019}t send".into()
                }),
            ]
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
                    title: TitleText::Model {
                        text: "You wrote to Jana Novák and Petr".into(),
                    },
                },
                StepTitle {
                    step: 2,
                    title: TitleText::Model {
                        text: "You pressed Send (+49 30 1234 5678)".into(),
                    },
                },
                // A model cannot pass its title off as a localized one.
                StepTitle {
                    step: 3,
                    title: TitleText::Label(LabelTitle::Invoked {
                        action: "Delete everything".into(),
                    }),
                },
            ],
        };
        let merged = merge(&trace, floor(&trace), Some(model));
        let title = |step: u32| merged.titles.iter().find(|t| t.step == step).unwrap();
        let text = |step: u32| match &title(step).title {
            TitleText::Model { text } => text.clone(),
            TitleText::Label(label) => panic!("step {step} kept its label title {label:?}"),
        };
        assert_eq!(
            text(1),
            "You wrote to \u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE}\u{25AE} and \u{25AE}\u{25AE}\u{25AE}\u{25AE}"
        );
        assert!(!text(2).contains("1234"));
        assert_eq!(
            title(3).title,
            TitleText::Label(LabelTitle::Appeared {
                label: "Couldn\u{2019}t send".into()
            })
        );
    }

    #[test]
    fn a_label_title_argument_is_masked_too() {
        let mut trace = mail_trace();
        trace.steps[2].action = "Reply to jana@example.cz".into();
        trace.steps[2].texts.push(text(
            "r:subject",
            DeclaredSensitivity::Public,
            "jana@example.cz",
        ));
        let review = review_without_model(&trace).unwrap();
        let TitleText::Label(label) = &review.titles[2].title else {
            panic!("label title");
        };
        assert_eq!(
            label.args()[0].1,
            format!("Reply to {}", "\u{25AE}".repeat(15))
        );
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

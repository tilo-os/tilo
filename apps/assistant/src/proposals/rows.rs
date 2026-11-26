//! The person's proposals and standing instructions, as the initiative service returned them.

use ui::prelude::*;
use ui::tilo_actions::remote::workspace_events::SetUp;
use ui::tilo_actions::remote::workspace_initiative::{Kind, Proposal, Proposals, State};
use ui::tokens::Rhythm;
use ui::widget::{Section, SectionHeader, When};

use super::{Decision, Review};
use crate::tr;

pub(super) fn rows(review: &Review, list: &Proposals) -> Vec<Element> {
    let mut waiting = Vec::new();
    let mut decided = Vec::new();
    for proposal in &list.proposals {
        let row = proposal_row(review, proposal);
        if proposal.state.waits() {
            waiting.push(row);
        } else {
            decided.push(row);
        }
    }
    let mut parts = Vec::new();
    if waiting.is_empty() {
        parts.push(
            Text::new("assistant-proposals-none-waiting")
                .dimmed()
                .into_widget(),
        );
    } else {
        parts.push(section("assistant-proposals-waiting", waiting));
    }
    if !list.kinds.is_empty() {
        let kinds = list
            .kinds
            .iter()
            .map(|kind| preference(review, kind))
            .collect();
        parts.push(section("assistant-proposals-preferences", kinds));
    }
    if !decided.is_empty() {
        parts.push(section("assistant-proposals-decided", decided));
    }
    parts
}

fn section(label: &str, rows: Vec<Element>) -> Element {
    let count = rows.len();
    Box::new(Section::new(rows).with_header(SectionHeader::group(tr(label)).count(count)))
}

fn column() -> Column {
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Related)
}

/// Keep the provider's words as content, even when the words happen to match a message key.
fn words(value: &str) -> Text {
    Text::with_args("assistant-proposal-value", &[("value", value.into())])
}

fn proposal_row(review: &Review, proposal: &Proposal) -> Element {
    let mut row = column()
        .with_id(format!("assistant-proposal-{}", proposal.id))
        .push(
            words(&proposal.title)
                .with_role(TextRole::Subheading)
                .into_widget(),
        );
    if !proposal.why.is_empty() {
        row = row.push(words(&proposal.why).into_widget());
    }
    let kind = if proposal.event.is_some() {
        "assistant-proposal-event-kind"
    } else {
        "assistant-proposal-task-kind"
    };
    row = row.push(
        Text::with_args(kind, &[("kind", proposal.kind.as_str().into())])
            .with_role(TextRole::Caption)
            .dimmed()
            .into_widget(),
    );
    if let Some(event) = &proposal.event {
        row = row.push(event_details(event));
    }
    let status = match proposal.state {
        State::Proposed => return Box::new(row.push(choices(review, proposal))),
        State::Accepted => "assistant-proposal-accepted",
        State::Declined => "assistant-proposal-declined",
        State::Started => "assistant-proposal-started",
    };
    Box::new(row.push(Text::new(status).with_role(TextRole::Label).into_widget()))
}

fn event_details(event: &SetUp) -> Element {
    let every = event.every.map(|n| n.to_string());
    let fields = [
        ("assistant-proposal-event-name", Some(event.name.as_str())),
        ("assistant-proposal-event-when", Some(event.when.as_str())),
        (
            "assistant-proposal-event-condition",
            event.only_if.as_deref(),
        ),
        ("assistant-proposal-event-every", every.as_deref()),
        (
            "assistant-proposal-event-instruction",
            Some(event.instruction.as_str()),
        ),
        (
            "assistant-proposal-event-delivery",
            event.told_as.as_deref(),
        ),
        ("assistant-proposal-event-purpose", event.purpose.as_deref()),
        ("assistant-proposal-event-style", event.style.as_deref()),
        (
            "assistant-proposal-event-guidance",
            event.guidance.as_deref(),
        ),
    ];
    let mut details = column();
    for (label, value) in fields {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            details = details.push(Box::new(
                column()
                    .with_rhythm(Rhythm::Tight)
                    .push(
                        Text::new(label)
                            .with_role(TextRole::Label)
                            .dimmed()
                            .into_widget(),
                    )
                    .push(words(value).into_widget()),
            ));
        }
    }
    Box::new(details)
}

fn choices(review: &Review, proposal: &Proposal) -> Element {
    let reason = LocalState::new(String::new());
    let field = TextInput::from_state(&reason)
        .with_id(&format!("assistant-proposal-why-{}", proposal.id))
        .with_accessible_name(tr("assistant-proposal-reason"))
        .with_placeholder("assistant-proposal-reason");
    let id = proposal.id.clone();
    let label = if proposal.event.is_some() {
        "assistant-proposal-set-up"
    } else {
        "assistant-proposal-start"
    };
    let accept = decision_button(
        review,
        &format!("assistant-proposal-accept-{id}"),
        label,
        move || Decision::Accept {
            id: id.clone(),
            always: false,
        },
    );
    let id = proposal.id.clone();
    let decline = decision_button(
        review,
        &format!("assistant-proposal-decline-{id}"),
        "assistant-proposal-decline",
        move || {
            let reason = reason.get();
            let reason = reason.trim();
            Decision::Decline {
                id: id.clone(),
                why: (!reason.is_empty()).then(|| reason.to_owned()),
            }
        },
    );
    let actions = Row::new()
        .with_rhythm(Rhythm::Related)
        .push(accept)
        .push(decline);
    let mut content = column().push(Box::new(field)).push(Box::new(actions));
    if proposal.event.is_none() {
        content = content.push(standing_approval(review, proposal));
    }
    Box::new(content)
}

fn standing_approval(review: &Review, proposal: &Proposal) -> Element {
    let confirming = LocalState::new(false);
    let open = confirming.clone();
    let ask = Button::new(
        &format!("assistant-proposal-always-{}", proposal.id),
        "assistant-proposal-always",
    )
    .on_view(move || open.set(true));
    let id = proposal.id.clone();
    let confirm = decision_button(
        review,
        &format!("assistant-proposal-confirm-always-{id}"),
        "assistant-proposal-confirm-always",
        move || Decision::Accept {
            id: id.clone(),
            always: true,
        },
    );
    let close = confirming.clone();
    let cancel = Button::new(
        &format!("assistant-proposal-cancel-always-{}", proposal.id),
        "assistant-job-cancel",
    )
    .on_view(move || close.set(false));
    let scope = Text::with_args(
        "assistant-proposal-always-detail",
        &[("kind", proposal.kind.as_str().into())],
    );
    let confirm = column().push(scope.into_widget()).push(Box::new(
        Row::new()
            .with_rhythm(Rhythm::Related)
            .push(confirm)
            .push(cancel.into_widget()),
    ));
    Box::new(
        column()
            .push(Box::new(When::from_state(
                &confirming,
                |on| !on,
                ask.into_widget(),
            )))
            .push(Box::new(When::from_state(
                &confirming,
                |on| *on,
                Box::new(confirm),
            ))),
    )
}

fn preference(review: &Review, kind: &Kind) -> Element {
    let mut row = column().push(words(&kind.kind).with_role(TextRole::Label).into_widget());
    if kind.always {
        let name = kind.kind.clone();
        row = row
            .push(Text::new("assistant-kind-always").into_widget())
            .push(decision_button(
                review,
                &format!("assistant-kind-ask-first-{name}"),
                "assistant-kind-ask-first",
                move || Decision::AskFirst(name.clone()),
            ));
    }
    if kind.quiet {
        let name = kind.kind.clone();
        row = row
            .push(Text::new("assistant-kind-quiet").into_widget())
            .push(decision_button(
                review,
                &format!("assistant-kind-propose-again-{name}"),
                "assistant-kind-suggest-again",
                move || Decision::ProposeAgain(name.clone()),
            ));
    }
    if !kind.always && !kind.quiet {
        row = row.push(Text::new("assistant-kind-asks").dimmed().into_widget());
    }
    Box::new(row)
}

fn decision_button(
    review: &Review,
    id: &str,
    label: &str,
    decision: impl Fn() -> Decision + 'static,
) -> Element {
    let review = review.clone();
    Button::new(id, label)
        .on_own_device_effect(move || review.decide(decision()))
        .into_widget()
}

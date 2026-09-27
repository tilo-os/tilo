//! A forge issue drafted from a report, with nothing personal in it.
//!
//! A draft is made only of what the report showed the room, and even that
//! passes the review floor ([`tilo_feedback_review`]) on the way in:
//!
//! - the people it names are declared `Personal` and replaced wherever they
//!   occur, the way review replaces a declared value in a recording;
//! - pattern detectors catch an address, a phone number, an account number
//!   or a credential that slipped into the words;
//! - who reported it is never part of it: the issue links the report, and
//!   the report says who wrote it to those allowed to see it.
//!
//! What stays is the report's own words, the address it was about, and the
//! recording's summary facts: the apps, the number of steps, the layer the
//! problem is in and the reporter's image generation.

use std::fmt::Write as _;

use chat::feedback::IssueDraft;
use chat::model::{Attachment, Message};
use tilo_feedback_review::{
    DeclaredSensitivity, ReviewStep, ReviewText, ReviewTraceRequest, StepKind, StepOutcome,
    TextOrigin,
};
use tilo_reference::SessionLayer;

/// Longest title a template draft gives an issue, in characters.
const TITLE_CHARS: usize = 80;

/// The node the redacted text is read back from.
const TEXT_NODE: &str = "text";

/// `text` with every declared `personal` value and every pattern the review
/// floor detects replaced by a shape-preserving placeholder.
pub fn redact(text: &str, personal: &[String]) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let mut texts = vec![ReviewText {
        node: TEXT_NODE.into(),
        sensitivity: DeclaredSensitivity::Public,
        kind: None,
        origin: TextOrigin::Semantic,
        text: text.to_string(),
    }];
    texts.extend(
        personal
            .iter()
            .filter(|value| !value.trim().is_empty())
            .enumerate()
            .map(|(index, value)| ReviewText {
                node: format!("personal-{index}"),
                sensitivity: DeclaredSensitivity::Personal,
                kind: None,
                origin: TextOrigin::Semantic,
                text: value.clone(),
            }),
    );
    let request = ReviewTraceRequest {
        steps: vec![ReviewStep {
            kind: StepKind::Edited,
            outcome: StepOutcome::Done,
            app: String::new(),
            action: String::new(),
            resource_kind: String::new(),
            texts,
        }],
    };
    let mut spans: Vec<(usize, usize)> = tilo_feedback_review::floor(&request)
        .into_iter()
        .filter(|proposal| proposal.node == TEXT_NODE)
        .filter_map(|proposal| {
            let start = usize::try_from(proposal.span.start).ok()?;
            let end = usize::try_from(proposal.span.end).ok()?;
            (start < end && text.is_char_boundary(start) && text.is_char_boundary(end))
                .then_some((start, end))
        })
        .collect();
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for (start, end) in spans {
        let start = start.max(at);
        if start >= end {
            continue;
        }
        out.push_str(text.get(at..start).unwrap_or_default());
        out.push_str(&tilo_actions::placeholder(
            text.get(start..end).unwrap_or_default(),
        ));
        at = end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

/// The values in `report` that are about a person rather than the problem:
/// the people it mentions, as they were named in its text.
pub fn personal_values(report: &Message) -> Vec<String> {
    report
        .mentions
        .iter()
        .filter_map(|mention| report.text.get(mention.start..mention.end()))
        .map(|run| run.trim_start_matches('@').to_string())
        .filter(|name| !name.trim().is_empty())
        .collect()
}

/// The address a report is linked by in an issue: the message's resource in
/// the room's conversation.
pub fn report_link(room: &str, report: &Message) -> String {
    format!("tilo:room/{room}/chat.message:{}", report.id)
}

/// The report's words, redacted, for a model or a draft.
pub fn redacted_text(report: &Message) -> String {
    redact(&report.text, &personal_values(report))
}

fn layer_label(layer: SessionLayer) -> &'static str {
    match layer {
        SessionLayer::Logic => "logic or UX (the replay matches what was recorded)",
        SessionLayer::BelowToolkit => {
            "below the toolkit (renderer, fonts, driver, scaling or compositor)"
        }
    }
}

/// A draft for `report` in room `room`: the model's title and summary when
/// one was asked, else a template from the report's own words.
pub fn draft(
    room: &str,
    report: &Message,
    title: Option<&str>,
    summary: Option<&str>,
) -> IssueDraft {
    let personal = personal_values(report);
    let text = redact(&report.text, &personal);
    let title = title
        .map(|title| redact(title, &personal))
        .filter(|title| !title.trim().is_empty() && !title.contains(['\n', '\r']))
        .unwrap_or_else(|| template_title(&text));
    let mut body = String::new();
    if let Some(summary) = summary.map(|summary| redact(summary, &personal))
        && !summary.trim().is_empty()
    {
        body.push_str(summary.trim());
        body.push_str("\n\n");
    }
    body.push_str("## Report\n\n");
    for line in text.lines() {
        body.push_str("> ");
        body.push_str(line);
        body.push('\n');
    }
    body.push_str("\n## Facts\n\n");
    let _ = writeln!(body, "- Report: {}", report_link(room, report));
    if let Some(reference) = &report.reference {
        let _ = writeln!(
            body,
            "- About: `{}` ({})",
            redact(&reference.id, &personal),
            redact(&reference.kind, &personal)
        );
    }
    let session = report
        .attachment
        .as_ref()
        .and_then(Attachment::record)
        .and_then(|record| record.session.as_ref());
    if let Some(summary) = session {
        let _ = writeln!(body, "- Apps: {}", summary.apps.join(", "));
        let _ = writeln!(body, "- Steps: {}", summary.steps);
        let _ = writeln!(body, "- Layer: {}", layer_label(summary.layer));
        if let Some(generation) = summary.generation {
            let _ = writeln!(body, "- Generation: {generation}");
        }
        if summary.redactions.total() > 0 {
            let _ = writeln!(
                body,
                "- Redacted before sending: {} text, {} regions",
                summary.redactions.text, summary.redactions.regions
            );
        }
    }
    IssueDraft {
        report: report.id.clone(),
        title,
        body,
    }
}

fn template_title(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Feedback report");
    if first.chars().count() <= TITLE_CHARS {
        return first.to_string();
    }
    let mut cut: String = first.chars().take(TITLE_CHARS - 1).collect();
    if let Some(space) = cut.rfind(' ') {
        cut.truncate(space);
    }
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chat::mention::Mention;
    use tilo_reference::{RecordAttachment, Redactions, ResourceRef, SessionSummary};

    fn report(text: &str) -> Message {
        Message {
            id: "m_report".into(),
            channel: tilo_reference::FEEDBACK_CHANNEL.into(),
            author: "u_1f4c3a9b2d7e5061".into(),
            ts: 10,
            text: text.into(),
            edited: false,
            history: vec![],
            parent: None,
            reactions: vec![],
            mentions: vec![],
            reference: None,
            also: Vec::new(),
            attachment: None,
            attachment_withdrawn: false,
            pinned: false,
            work: None,
            report: None,
            approved: None,
        }
    }

    #[test]
    fn a_draft_keeps_the_facts_and_nothing_personal() {
        let mut m =
            report("Send to ada.lovelace@example.com fails after @Ada Byron edits the To field");
        let at = m.text.find("@Ada").unwrap();
        m.mentions = vec![Mention {
            user: "u_ada".into(),
            start: at,
            len: "@Ada Byron".len(),
            is_agent: false,
        }];
        m.reference = Some(ResourceRef::whole("mail.field", "mail.field:to", "To"));
        m.attachment = Some(Attachment::Record(Box::new(RecordAttachment::session(
            "uuid:rec",
            "Recording",
            "personal-room",
            SessionSummary {
                apps: vec!["so.tilo.mail".into()],
                duration_ms: 42_000,
                steps: 5,
                layer: SessionLayer::Logic,
                redactions: Redactions {
                    text: 2,
                    regions: 1,
                },
                generation: Some(121),
            },
        ))));
        let draft = draft("room-1", &m, None, None);
        let all = format!("{}\n{}", draft.title, draft.body);
        for personal in [
            "ada.lovelace@example.com",
            "Ada Byron",
            "u_1f4c3a9b2d7e5061",
        ] {
            assert!(!all.contains(personal), "{personal} leaked into {all}");
        }
        assert!(draft.is_valid());
        for fact in [
            "so.tilo.mail",
            "Steps: 5",
            "logic or UX",
            "Generation: 121",
            "mail.field:to",
            "tilo:room/room-1/chat.message:m_report",
        ] {
            assert!(
                draft.body.contains(fact),
                "{fact} missing from {}",
                draft.body
            );
        }
        assert!(draft.title.starts_with("Send to "));
    }

    #[test]
    fn a_model_title_and_summary_pass_the_same_floor() {
        let m = report("It fails for bob@example.org");
        let draft = draft(
            "room-1",
            &m,
            Some("Fails for bob@example.org"),
            Some("Mail refuses to send to bob@example.org."),
        );
        assert!(!format!("{}{}", draft.title, draft.body).contains("bob@example.org"));
        assert!(draft.body.starts_with("Mail refuses to send to "));
    }
}

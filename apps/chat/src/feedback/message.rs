//! A filed report as its channel shows it: a message by whoever filed it,
//! read out from the report's structured entries in the reader's language,
//! with its masked picture as the message's image and the reactions people
//! answered it with. Nothing of this is stored: the record keeps the
//! reviewed submission, and this is how it reads.

use tilo_document::ClipBlock;
use ui::feedback::{
    FrameTimings, Hidden, Interaction, Outcome, ReviewedSelection, ReviewedText, TextKind,
};
use ui::tilo_resource::Id;

use crate::record::FeedbackReport;

/// The file name a report's picture goes by where one shows (a bridge's
/// upload).
pub const PICTURE_NAME: &str = "report.png";

/// The most of one pick's texts the message lists.
const SHOWN_TEXTS: usize = 6;
/// The most of the reporter's last actions the message lists, newest last.
const SHOWN_ACTIONS: usize = 5;

/// `report` as the message its channel shows, `None` for a report whose
/// submission is missing.
pub(crate) fn message(report: &FeedbackReport) -> Option<crate::record::Message> {
    let submission = report.submission.as_ref()?;
    let feedback = &submission.feedback;
    let lines = lines(
        &feedback.text,
        &feedback.selections,
        &feedback.interactions,
        feedback.timings.as_ref(),
    );
    let body = lines
        .iter()
        .map(|line| ClipBlock {
            kind: "paragraph".into(),
            text: line.clone(),
            marks: Vec::new(),
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect();
    let attachment = feedback
        .screenshot
        .as_ref()
        .map(|picture| crate::model::Attachment::Image {
            name: PICTURE_NAME.into(),
            blob: picture.hash.to_ascii_lowercase(),
            w: picture.width,
            h: picture.height,
        })
        .filter(crate::record::valid_attachment);
    Some(crate::record::Message {
        channel: Id::from_raw(submission.destination.channel.clone()),
        author: report.author.clone(),
        ts: report.ts,
        text: lines.join("\n"),
        body,
        reactions: report.reactions.clone(),
        attachment,
        ..crate::record::Message::default()
    })
}

/// The report's lines: the reporter's summary, each pick with what it
/// said and their note on it, what they did last and how slow it was.
fn lines(
    summary: &str,
    selections: &[ReviewedSelection],
    interactions: &[Interaction],
    timings: Option<&FrameTimings>,
) -> Vec<String> {
    let mut lines = Vec::new();
    if !summary.trim().is_empty() {
        lines.push(summary.trim().to_owned());
    }
    for (index, selection) in selections.iter().enumerate() {
        lines.extend(pick(index, selection));
    }
    if let Some(recent) = recent(interactions) {
        lines.push(recent);
    }
    if let Some(timings) = timings.filter(|timings| !timings.frames.is_empty()) {
        lines.push(crate::i18n::t_args(
            "chat-feedback-slow",
            &[
                ("slow", timings.slow_frames().into()),
                ("frames", timings.frames.len().into()),
                ("budget", (timings.budget / 1_000).into()),
                ("worst", (timings.worst() / 1_000).into()),
            ],
        ));
    }
    lines
}

/// One pick: what it was in which app ("A message in Chat", "The “Send
/// message” button in Chat"), the reporter's note on it, and the texts it
/// showed. Only the app's own words name a part; a name that is content is
/// one of its texts.
fn pick(index: usize, selection: &ReviewedSelection) -> Vec<String> {
    let label = selection
        .label
        .as_ref()
        .map(|label| (label.kind, read(label)))
        .filter(|(_, label)| !label.trim().is_empty());
    let role = selection.role.clone();
    let what = match (&selection.kind, &label) {
        (Some(kind), _) => {
            crate::i18n::t_args("chat-feedback-part-kind", &[("kind", kind.clone().into())])
        }
        (None, Some((TextKind::Interface, name))) => crate::i18n::t_args(
            "chat-feedback-part-named",
            &[("name", name.clone().into()), ("role", role.into())],
        ),
        (None, _) => crate::i18n::t_args("chat-feedback-part-role", &[("role", role.into())]),
    };
    // The app by the name it was picked under; never its id, which only
    // routes. A pick with no name known is the part alone.
    let app = selection.app_name.trim();
    let mut lines = vec![if app.is_empty() {
        crate::i18n::t_args(
            "chat-feedback-pick-alone",
            &[
                ("index", index.saturating_add(1).into()),
                ("what", what.into()),
            ],
        )
    } else {
        crate::i18n::t_args(
            "chat-feedback-pick",
            &[
                ("index", index.saturating_add(1).into()),
                ("what", what.into()),
                ("app", app.to_owned().into()),
            ],
        )
    }];
    let named_by_content = label
        .filter(|(kind, _)| *kind != TextKind::Interface || selection.kind.is_some())
        .map(|(_, label)| label);
    if let Some(note) = selection
        .note
        .as_ref()
        .map(read)
        .filter(|note| !note.trim().is_empty())
    {
        lines.push(crate::i18n::t_args(
            "chat-feedback-note",
            &[("note", note.into())],
        ));
    }
    // One line per text: a text may hold any punctuation, so no separator
    // between them can be told apart from the text itself.
    lines.extend(
        named_by_content
            .into_iter()
            .chain(selection.texts.iter().map(read))
            .filter(|text| !text.trim().is_empty())
            .take(SHOWN_TEXTS)
            .map(|text| crate::i18n::t_args("chat-feedback-text", &[("text", text.into())])),
    );
    lines
}

/// The reporter's last actions, newest last.
fn recent(interactions: &[Interaction]) -> Option<String> {
    let skip = interactions.len().saturating_sub(SHOWN_ACTIONS);
    let actions: Vec<String> = interactions
        .iter()
        .skip(skip)
        .map(|interaction| {
            let outcome = match interaction.outcome {
                Outcome::Done => "done",
                Outcome::Refused => "refused",
                Outcome::Failed => "failed",
            };
            let label = interaction
                .label
                .clone()
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| {
                    crate::i18n::t_args(
                        "chat-feedback-action-unnamed",
                        &[
                            ("action", interaction.action.clone().into()),
                            ("resource", interaction.resource.clone().into()),
                        ],
                    )
                });
            crate::i18n::t_args(
                "chat-feedback-action",
                &[("label", label.into()), ("outcome", outcome.into())],
            )
        })
        .collect();
    (!actions.is_empty()).then(|| {
        crate::i18n::t_args(
            "chat-feedback-recent",
            &[("actions", actions.join(", ").into())],
        )
    })
}

/// A reviewed text with each hidden span as its kind's placeholder.
fn read(text: &ReviewedText) -> String {
    text.parts
        .iter()
        .map(|part| match part.hidden {
            Some(hidden) => crate::i18n::t_args(
                "chat-feedback-hidden",
                &[("kind", hidden_kind(hidden).into())],
            ),
            None => part.text.clone(),
        })
        .collect()
}

const fn hidden_kind(hidden: Hidden) -> &'static str {
    match hidden {
        Hidden::Name => "name",
        Hidden::Email => "email",
        Hidden::Phone => "phone",
        Hidden::Address => "address",
        Hidden::Account => "account",
        Hidden::Credential => "credential",
        Hidden::Personal => "personal",
    }
}

#[cfg(test)]
mod tests;

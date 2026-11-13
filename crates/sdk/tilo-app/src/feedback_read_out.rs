//! A filed feedback report read out in the reader's language: the reporter's
//! summary, each part they picked with what it said and their note on it,
//! what they did last, how slow it was, and the recording it carries. A
//! report's channel shows these lines as its message, and a bridge posts the
//! same lines where it mirrors the channel: the report keeps its structured
//! entries, and this is how they read.

use crate::feedback::{
    FrameTimings, Hidden, Interaction, Outcome, RecordingRef, ReviewedFeedback, ReviewedSelection,
    ReviewedText, TextKind,
};

use crate::copy_catalogue::t_args;

/// The file name a report's picture goes by where it shows as an image (a
/// report's message, a bridge's upload).
pub const PICTURE_NAME: &str = "report.png";

/// The most of one pick's texts the read-out lists.
const SHOWN_TEXTS: usize = 6;
/// The most of the reporter's last actions the read-out lists, newest last.
const SHOWN_ACTIONS: usize = 5;

/// `feedback`'s lines: the reporter's summary, each pick with what it said
/// and their note on it, what they did last, how slow it was and its
/// recording.
#[must_use]
pub fn lines(feedback: &ReviewedFeedback) -> Vec<String> {
    let mut lines = summary_lines(
        &feedback.text,
        &feedback.selections,
        &feedback.interactions,
        feedback.timings.as_ref(),
    );
    lines.extend(feedback.recording.as_ref().map(recording));
    lines
}

fn summary_lines(
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
        lines.push(t_args(
            "feedback-report-slow",
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
        (Some(kind), _) => t_args(
            "feedback-report-part-kind",
            &[("kind", kind.clone().into())],
        ),
        (None, Some((TextKind::Interface, name))) => t_args(
            "feedback-report-part-named",
            &[("name", name.clone().into()), ("role", role.into())],
        ),
        (None, _) => t_args("feedback-report-part-role", &[("role", role.into())]),
    };
    // The app by the name it was picked under; never its id, which only
    // routes. A pick with no name known is the part alone.
    let app = selection.app_name.trim();
    let mut lines = vec![if app.is_empty() {
        t_args(
            "feedback-report-pick-alone",
            &[
                ("index", index.saturating_add(1).into()),
                ("what", what.into()),
            ],
        )
    } else {
        t_args(
            "feedback-report-pick",
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
        lines.push(t_args("feedback-report-note", &[("note", note.into())]));
    }
    // One line per text: a text may hold any punctuation, so no separator
    // between them can be told apart from the text itself.
    lines.extend(
        named_by_content
            .into_iter()
            .chain(selection.texts.iter().map(read))
            .filter(|text| !text.trim().is_empty())
            .take(SHOWN_TEXTS)
            .map(|text| t_args("feedback-report-text", &[("text", text.into())])),
    );
    lines
}

/// What the report's recording holds: whose record, over which time, in how
/// many steps.
fn recording(recording: &RecordingRef) -> String {
    let at = |ms: i64| crate::format::time_of(&crate::format::UnixSecs(ms.div_euclid(1000)));
    t_args(
        "feedback-report-recording",
        &[
            ("app", recording.app_name.clone().into()),
            ("from", at(recording.from_ms).into()),
            ("to", at(recording.to_ms).into()),
            ("steps", recording.steps.into()),
        ],
    )
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
                    t_args(
                        "feedback-report-action-unnamed",
                        &[
                            ("action", interaction.action.clone().into()),
                            ("resource", interaction.resource.clone().into()),
                        ],
                    )
                });
            t_args(
                "feedback-report-action",
                &[("label", label.into()), ("outcome", outcome.into())],
            )
        })
        .collect();
    (!actions.is_empty()).then(|| {
        t_args(
            "feedback-report-recent",
            &[("actions", actions.join(", ").into())],
        )
    })
}

/// A reviewed text with each hidden span as its kind's placeholder.
fn read(text: &ReviewedText) -> String {
    text.parts
        .iter()
        .map(|part| match part.hidden {
            Some(hidden) => t_args(
                "feedback-report-hidden",
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

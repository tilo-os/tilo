//! The review sheet of a report about picked parts of the screen: a note
//! per part and one about the whole, "this was slow", then exactly what
//! will be sent, with every hidden span listed to show or hide again, and
//! Send.

use std::cell::RefCell;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::feedback::report::{
    Exposure, ModelText, Redaction, ReportReview, model_prompt, model_proposals,
};
use crate::feedback::{Composer, ModelCheck, ReportWords};
use crate::models::{CompletionFuture, Content, Message, ModelRequest, Models, Role, ToolChoice};
use crate::prelude::*;

use super::{FeedbackDraft, FeedbackEffect, Presentation, group};

/// What the sheet shows of the review; built again whenever it changes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReviewShown {
    review: Option<ReportReview>,
    check: ModelCheck,
    sent: bool,
    picture: Option<Vec<u8>>,
}

impl ReviewShown {
    pub(super) fn of(composer: &Composer) -> Option<Self> {
        composer.picked()?;
        Some(Self {
            review: composer.report_review().cloned(),
            check: composer
                .model_check()
                .cloned()
                .unwrap_or(ModelCheck::Unavailable),
            sent: composer.report_sent(),
            picture: composer
                .report_picture()
                .map(|picture| picture.png().to_vec()),
        })
    }
}

/// What the sheet calls each picked part: what the person saw it called
/// while picking it ("Message in Chat").
pub(super) fn titles(composer: &Composer) -> Option<Vec<String>> {
    let draft = composer.picked()?;
    Some(
        draft
            .picks
            .iter()
            .map(|pick| pick.caption.clone())
            .collect(),
    )
}

/// The words the person wrote, as the effects read them.
pub(super) fn words(
    draft: &FeedbackDraft,
    presentation: &Presentation,
    picks: usize,
) -> ReportWords {
    ReportWords {
        summary: draft.issue.clone(),
        notes: presentation
            .notes
            .iter()
            .take(picks)
            .map(LocalState::get)
            .collect(),
        slow: draft.slow,
    }
}

/// The sheet for the picked parts called `titles`.
pub(super) fn sheet(
    cx: &ViewCx<FeedbackDraft>,
    presentation: &Presentation,
    titles: &[String],
) -> Element {
    let mut sheet = crate::column![
        Text::new("feedback-report-title").with_role(TextRole::Title),
        Text::new("feedback-report-intro").with_id("feedback-report-intro"),
    ]
    .with_gap(12.0);
    for (index, (title, note)) in titles.iter().zip(presentation.notes.iter()).enumerate() {
        sheet = sheet.push(Box::new(
            group()
                .with_gap(4.0)
                .push(Box::new(
                    Text::new("feedback-pick-title")
                        .with_arg("number", index.saturating_add(1).to_string())
                        .with_arg("part", title.clone())
                        .with_role(TextRole::Subheading),
                ))
                .push(Box::new(
                    TextInput::from_state(note)
                        .with_id(&format!("feedback-note-{index}"))
                        .with_placeholder("feedback-pick-note"),
                )),
        ));
    }
    let shown = presentation.review.clone();
    let sheet = sheet
        .push(Box::new(
            cx.input(FeedbackDraft::paths().issue)
                .with_id("feedback-issue")
                .with_placeholder("feedback-report-describe")
                .multiline(true),
        ))
        .push(Box::new(
            cx.checkbox("feedback-slow", FeedbackDraft::paths().slow),
        ))
        .push(Box::new(cx.effect_button(
            "feedback-report-check",
            &FeedbackEffect::Review,
        )))
        .push(cx.switch(&shown, |part, shown| outgoing(part, shown.as_ref())))
        // The report says what to do next itself; the status line only
        // speaks once there is progress or a problem to report.
        .push(Box::new(ui_core::widget::When::from_state(
            &presentation.status,
            |status| status != "feedback-review-first",
            Box::new(
                Text::from_state(&presentation.status, Clone::clone)
                    .with_id("feedback-status")
                    .with_live(crate::a11y::Politeness::Polite),
            ),
        )));
    Box::new(sheet)
}

/// Exactly what will be sent, what was hidden from it, and Send.
fn outgoing(cx: &ViewCx<FeedbackDraft>, shown: Option<&ReviewShown>) -> Element {
    let Some(shown) = shown else {
        return Box::new(group());
    };
    let Some(review) = &shown.review else {
        return Box::new(Text::new("feedback-report-check-first"));
    };
    let mut part = group().with_gap(8.0).push(Box::new(
        Text::new(if shown.sent {
            "feedback-report-was-sent"
        } else {
            "feedback-report-outgoing"
        })
        .with_role(TextRole::Heading),
    ));
    if let Some(image) = shown
        .picture
        .as_ref()
        .and_then(|png| crate::widget::Image::from_encoded(png).ok())
    {
        part = part.push(Box::new(image.with_alt("feedback-report-picture")));
    }
    part = part.push(Box::new(
        Text::new("feedback-reviewed-text")
            .with_arg("value", review.feedback.plain())
            .with_id("feedback-preview"),
    ));
    if !review.redactions.is_empty() {
        part = part.push(Box::new(
            Text::new("feedback-report-hidden").with_role(TextRole::Subheading),
        ));
        for (index, redaction) in review.redactions.iter().enumerate() {
            part = part.push(hidden_row(cx, index, redaction, shown.sent));
        }
    }
    let check = Text::new(match shown.check {
        ModelCheck::Unavailable => "feedback-check-rules",
        ModelCheck::Pending { .. } => "feedback-check-pending",
        ModelCheck::Applied { added: 0, .. } => "feedback-check-model-clear",
        ModelCheck::Applied { .. } => "feedback-check-model-added",
        ModelCheck::Failed { .. } => "feedback-check-model-failed",
    });
    let check = match shown.check.by() {
        Some(model) => check.with_arg("model", model.to_owned()),
        None => check,
    };
    part = part.push(Box::new(check.with_id("feedback-check")));
    let last = if shown.sent {
        cx.effect_button("feedback-retry", &FeedbackEffect::Retry)
    } else {
        cx.effect_button("feedback-report-send", &FeedbackEffect::Submit)
    };
    Box::new(part.push(Box::new(last)))
}

/// One hidden span: what it is, what it says (here, on this device), and
/// the choice to show it in the report or hide it again.
fn hidden_row(
    cx: &ViewCx<FeedbackDraft>,
    index: usize,
    redaction: &Redaction,
    sent: bool,
) -> Element {
    let kind = redaction.hidden_as.placeholder();
    let label = Text::new(if redaction.hidden {
        "feedback-redaction-hidden"
    } else {
        "feedback-redaction-shown"
    })
    .with_arg("kind", kind.to_owned())
    .with_arg("value", ellipsized(redaction.value(), 80))
    .with_id(format!("feedback-redaction-{index}"));
    let row = crate::row![label].with_gap(8.0);
    if sent {
        return Box::new(row);
    }
    if !redaction.revealable {
        return Box::new(row.push(Box::new(Text::new("feedback-redaction-always"))));
    }
    Box::new(row.push(Box::new(cx.effect_button(
        if redaction.hidden {
            "feedback-redaction-show"
        } else {
            "feedback-redaction-hide"
        },
        &FeedbackEffect::Toggle { index },
    ))))
}

fn ellipsized(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(chars).collect();
    out.push('\u{2026}');
    out
}

/// A model looking over a review of `words`: its prompt's numbered texts
/// and the answer on its way.
pub(super) struct ModelPass {
    words: ReportWords,
    view: Vec<ModelText>,
    answer: CompletionFuture<'static>,
}

impl ModelPass {
    /// Ask `models`, which the review calls `name`, over the review the
    /// composer shows. A model outside this device sees what the person
    /// keeps hidden only as placeholders; so does one on it, which the
    /// review does not tell apart.
    pub(super) fn start(
        models: &Models,
        name: &str,
        composer: &mut Composer,
        words: &ReportWords,
    ) -> Option<Self> {
        if !models.is_available() {
            return None;
        }
        let request = composer.report_request()?;
        let (prompt, view) = model_prompt(&request, Exposure::Placeholders);
        let ask = ModelRequest {
            messages: vec![Message::text(Role::User, prompt)],
            tools: Vec::new(),
            tool_choice: ToolChoice::None,
            max_output_tokens: 1024,
        };
        let models = models.clone();
        composer.model_pending(words, name);
        Some(Self {
            words: words.clone(),
            view,
            answer: Box::pin(async move { models.complete(&ask).await }),
        })
    }

    /// Lay the answer over the review once it is in; whether it is.
    pub(super) fn poll(&mut self, composer: &mut Composer) -> bool {
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(answer) = self.answer.as_mut().poll(&mut cx) else {
            return false;
        };
        let proposals = answer.ok().and_then(|response| {
            let text: String = response
                .message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            model_proposals(&self.view, &text).ok()
        });
        composer.apply_model(&self.words, proposals);
        true
    }
}

/// Lays a pass's answer over the review once it is in; whether it is.
pub(super) type PassPoll = dyn FnMut(&mut Composer) -> bool;

/// The pass in flight, shared by the services and the effects. The
/// services poll it only through [`PassPoll`], so the review it runs stays
/// with the composer's window.
pub(super) type Pass = Rc<RefCell<Option<Box<PassPoll>>>>;

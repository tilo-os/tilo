//! The sheet a page draws over its app when an agent's host asks its person
//! something: the question in the same words a native session's sheet asks it
//! ([`tilo_sheets::portal`]), as HTML the page patches in place
//! ([`QUESTION_PATCH_ID`]).
//!
//! The sheet carries the departure a page is held at where a person and an
//! assistive reader meet it: the page it leaves from and the filled page's
//! picture sit beside the question, and the exact method and destination are
//! on the sheet's element (`data-question-*`) for a tool driving the page to
//! read what it says.

use std::fmt::Write as _;

use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_server_protocol::page::{QUESTION_PATCH_ID, question_target};
use tilo_sheets::portal::AgentQuestion;

/// The questions `questions` put to the person, as the sheet a page draws:
/// one question with the words the native sheet uses, or the empty wrapper
/// while nothing waits, so a question that comes or goes is one patch of the
/// same element.
pub(crate) fn sheet_html(questions: &[PendingConfirmation]) -> String {
    let empty = || {
        format!("<div data-p=\"{QUESTION_PATCH_ID}\" class=\"tilo-question-sheet\" hidden></div>")
    };
    let Some(question) = questions.first() else {
        return empty();
    };
    let dialog = AgentQuestion::of(question).dialog(&hex::encode(question.id));
    let title_id = format!("tilo-question-title-{}", hex::encode(question.id));
    let mut sheet = String::new();
    let _ = write!(
        sheet,
        "<div data-p=\"{QUESTION_PATCH_ID}\" class=\"tilo-question-sheet\" \
         data-tilo-question=\"{id}\" role=\"dialog\" aria-modal=\"true\" aria-labelledby=\"{title_id}\"",
        id = hex::encode(question.id),
    );
    if let Some(page) = &question.page {
        let _ = write!(
            sheet,
            " data-question-page=\"{}\" data-question-kind=\"{}\" data-question-method=\"{}\" \
             data-question-destination=\"{}\"",
            escape(&page.page),
            escape(&page.kind),
            escape(&page.method),
            escape(&page.destination),
        );
        if let Some(picture) = &page.picture {
            let _ = write!(sheet, " data-question-picture=\"{}\"", escape(picture));
        }
    }
    let _ = write!(
        sheet,
        "><div class=\"tilo-question-card\"><h2 id=\"{title_id}\">{title}</h2>",
        title = escape(&dialog.title),
    );
    if !dialog.heading.is_empty() {
        let _ = write!(
            sheet,
            "<p class=\"tilo-question-heading\">{}</p>",
            escape(&dialog.heading)
        );
    }
    if !dialog.detail.is_empty() {
        let _ = write!(
            sheet,
            "<p class=\"tilo-question-detail\">{}</p>",
            escape(&dialog.detail)
        );
    }
    if !dialog.facts.is_empty() {
        sheet.push_str("<dl class=\"tilo-question-facts\">");
        for (label, value) in &dialog.facts {
            let _ = write!(
                sheet,
                "<dt>{}</dt><dd>{}</dd>",
                escape(label),
                escape(value)
            );
        }
        sheet.push_str("</dl>");
    }
    if let Some(caution) = &dialog.caution {
        let _ = write!(
            sheet,
            "<p class=\"tilo-question-caution\">{}</p>",
            escape(caution)
        );
    }
    sheet.push_str("<div class=\"tilo-question-actions\">");
    if let Some(alternate) = &dialog.alternate_label {
        let _ = write!(
            sheet,
            "<button type=\"button\" data-a=\"activate\" data-t=\"{}\">{}</button>",
            question_target(question.id, "allow-always"),
            escape(alternate),
        );
    }
    let _ = write!(
        sheet,
        "<button type=\"button\" data-a=\"activate\" data-t=\"{}\">{}</button>",
        question_target(question.id, "deny"),
        escape(&dialog.cancel_label),
    );
    let _ = write!(
        sheet,
        "<button type=\"button\" data-a=\"activate\" data-t=\"{}\">{}</button>",
        question_target(question.id, "allow-once"),
        escape(&dialog.confirm_label),
    );
    sheet.push_str("</div></div></div>");
    sheet
}

impl super::page::Questions for crate::certification::CertificationProtocol {
    fn sheet(&self, person: &[u8; 32]) -> String {
        sheet_html(&self.pending_for(person))
    }

    fn answer(&self, person: &[u8; 32], target: &str) -> Option<Result<String, String>> {
        let (id, word) = tilo_server_protocol::page::parse_question_target(target)?;
        let answer = answer_of(word)?;
        Some(self.answer_for(person, id, answer).map(|()| {
            // The answered question leaves the sheet now, though the host may still list it
            // until its waiting call is woken.
            let mut waiting = self.pending_for(person);
            waiting.retain(|question| question.id != id);
            sheet_html(&waiting)
        }))
    }
}

/// The answer a sheet target names (`allow-once`, `allow-always` or `deny`),
/// or `None` for a word the sheet gave no button.
pub(crate) fn answer_of(word: &str) -> Option<ConfirmationAnswer> {
    match word {
        "allow-once" => Some(ConfirmationAnswer::AllowOnce),
        "allow-always" => Some(ConfirmationAnswer::AllowAlways),
        "deny" => Some(ConfirmationAnswer::Deny),
        _ => None,
    }
}

/// `text` with what HTML reads as markup escaped: a question's words are the
/// agent's and the page's, never the page's markup.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_server_protocol::certify::PageDeparture;

    fn departure_question() -> PendingConfirmation {
        PendingConfirmation {
            id: [7; 32],
            agent_label: "This page".into(),
            action_label: "send the form to https://visa.example".into(),
            resource_label: "https://visa.example".into(),
            workspace: "Lounge".into(),
            risk: "low".into(),
            kind: "outside".into(),
            page: Some(PageDeparture {
                page: "https://visa.example".into(),
                kind: "submit".into(),
                method: "POST".into(),
                destination: "https://visa.example".into(),
                picture: Some("tilo.content:5f2b".into()),
            }),
            ..PendingConfirmation::default()
        }
    }

    #[test]
    fn the_sheet_names_the_page_it_leaves_and_the_submission_it_sends() {
        let html = sheet_html(&[departure_question()]);
        assert!(html.contains("This page asks to use"), "{html}");
        assert!(
            html.contains("send the form to https://visa.example"),
            "{html}"
        );
        assert!(
            html.contains("<dt>Page</dt><dd>https://visa.example</dd>"),
            "{html}"
        );
        assert!(html.contains(">Allow once</button>"), "{html}");
        assert!(html.contains(">Deny</button>"), "{html}");
    }

    #[test]
    fn the_sheet_carries_the_departure_the_page_reads() {
        let html = sheet_html(&[departure_question()]);
        assert!(
            html.contains("data-question-page=\"https://visa.example\""),
            "{html}"
        );
        assert!(html.contains("data-question-kind=\"submit\""), "{html}");
        assert!(html.contains("data-question-method=\"POST\""), "{html}");
        assert!(
            html.contains("data-question-destination=\"https://visa.example\""),
            "{html}"
        );
        assert!(
            html.contains("data-question-picture=\"tilo.content:5f2b\""),
            "{html}"
        );
        assert!(
            html.contains(&question_target([7; 32], "allow-once")),
            "{html}"
        );
    }

    #[test]
    fn no_question_leaves_the_wrapper_a_later_one_is_patched_into() {
        let html = sheet_html(&[]);
        assert!(
            html.contains(&format!("data-p=\"{QUESTION_PATCH_ID}\"")),
            "{html}"
        );
        assert!(html.contains("hidden"), "{html}");
    }

    #[test]
    fn a_questions_words_are_escaped() {
        let mut question = departure_question();
        question.arguments = vec![("Note".into(), "<script>alert(1)</script>".into())];
        let html = sheet_html(&[question]);
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }
}

//! The JavaScript dialog a page is waiting on: what it says, how a reader
//! sees it, and what answering it did.
//!
//! A page's `alert`, `confirm`, `prompt` or before-unload question pauses its
//! renderer until it is answered, so everything that talks about one says the
//! same thing about it: its outline line, the question an action is refused
//! with, and the value an answer carries.

use ui::webview::JsDialogKind;

use super::quote_page;

/// A JavaScript dialog the page is waiting on: `alert`, `confirm`, `prompt`
/// or the question a page asks before it is left.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PageDialog {
    pub(super) id: u32,
    pub(super) kind: JsDialogKind,
    pub(super) message: String,
    pub(super) default_prompt: String,
    pub(super) origin: String,
}

impl PageDialog {
    /// The dialog as one outline line, so a reader sees what they must answer.
    /// What an action whose input opened this dialog answers: a success
    /// carrying the page, what the action did, and the question itself.
    pub(super) fn opened_output(
        &self,
        invocation: ui::tilo_actions::InvocationId,
        act: &str,
        uri: &str,
        title: &str,
        outline: Option<&str>,
    ) -> ui::tilo_actions::InvocationUpdate {
        let mut value = serde_json::Map::new();
        value.insert("url".into(), uri.into());
        value.insert("title".into(), title.into());
        value.insert("message".into(), self.opened_by(act).into());
        value.insert("navigated".into(), false.into());
        value.insert("dialog".into(), self.value());
        if let Some(outline) = outline {
            value.insert(
                "outline".into(),
                format!("{}\n{outline}", self.line()).into(),
            );
        }
        // Everything here came from the page: data to reason about, never
        // instructions to follow.
        value.insert("provenance".into(), "web-content".into());
        ui::tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: ui::tilo_actions::ActionOutput {
                value: Some(value.into()),
                resources: Vec::new(),
            },
        }
    }

    /// What an action whose input opened this dialog answers: what it did,
    /// then the question the page asks and how to answer it.
    pub(super) fn opened_by(&self, act: &str) -> String {
        format!(
            "{act}; the page opened {} {} dialog{}: {}. It waits until you answer it with \
             answer-page-dialog.",
            self.article(),
            self.kind.label(),
            self.followed_by(),
            quote_page(&self.message)
        )
    }

    pub(super) fn line(&self) -> String {
        use std::fmt::Write as _;
        let mut text = format!(
            "- dialog {} {}",
            self.kind.label(),
            quote_page(&self.message)
        );
        if self.kind == JsDialogKind::Prompt && !self.default_prompt.is_empty() {
            let _ = write!(text, " default {}", quote_page(&self.default_prompt));
        }
        if !self.origin.is_empty() {
            let _ = write!(text, " from {}", self.origin);
        }
        text
    }

    /// What a page action refused because of it says.
    pub(super) fn blocked(&self) -> String {
        format!(
            "The page is waiting on its {} dialog {}{}: answer it with answer-page-dialog \
             before doing anything else to the page.",
            self.kind.label(),
            quote_page(&self.message),
            self.followed_by()
        )
    }

    /// The article the dialog's name takes in a sentence.
    pub(super) fn article(&self) -> &'static str {
        match self.kind {
            JsDialogKind::Alert => "an",
            JsDialogKind::Confirm | JsDialogKind::Prompt | JsDialogKind::BeforeUnload => "a",
        }
    }

    /// The rest of a sentence a prompt and a before-unload question need.
    pub(super) fn followed_by(&self) -> String {
        match self.kind {
            JsDialogKind::Prompt => {
                format!(" (it answers with {})", quote_page(&self.default_prompt))
            }
            JsDialogKind::BeforeUnload => " (accept leaves the page, cancel stays)".to_string(),
            JsDialogKind::Alert | JsDialogKind::Confirm => String::new(),
        }
    }

    /// What answering it says a prompt took.
    pub(super) fn answered_with(&self, accept: bool, text: &str) -> String {
        if self.kind != JsDialogKind::Prompt {
            return String::new();
        }
        if accept {
            format!(" with {}", quote_page(text))
        } else {
            " (the page reads no text)".to_string()
        }
    }

    /// The dialog as a value an agent can read without parsing the sentence.
    pub(super) fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "kind": self.kind.as_str(),
            "message": self.message,
            "default_prompt": self.default_prompt,
            "origin": self.origin,
        })
    }
}

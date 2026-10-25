//! The page's own question, in the words the agent reads.
//!
//! A page's `alert`, `confirm`, `prompt` or "leave this page?" question stops
//! the page until it is answered, so everything that talks about one says the
//! same thing about it: the line a reader sees, the question an action that
//! opened it answers with, the refusal another action gets while it is open,
//! and what answering it did.
//!
//! A question an action's input opened is not a failure of that action: the
//! click happened and the page only asks before it goes on. The action
//! succeeds, and its answer carries the question, so the agent answers it
//! without another read.

use ui::webview::{PageDialog, PageDialogKind};

/// A page string as one outline token, with its quotes and newlines made
/// visible so a dialog's text can never look like outline structure.
fn quote_page(text: &str) -> String {
    const MAX: usize = 512;
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for (index, character) in text.chars().enumerate() {
        if index >= MAX {
            quoted.push('…');
            break;
        }
        match character {
            '"' => quoted.push('\\'),
            '\n' | '\r' => quoted.push(' '),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// The page's kind of question, named for a sentence: `alert`, `confirm`,
/// `prompt`, or the question asked before the page is left.
pub(super) fn kind_label(dialog: &PageDialog) -> &'static str {
    match dialog.kind {
        PageDialogKind::Alert => "alert",
        PageDialogKind::Confirm => "confirm",
        PageDialogKind::Prompt => "prompt",
        PageDialogKind::Leave { reload: false } => "before-unload",
        PageDialogKind::Leave { reload: true } => "reload",
    }
}

/// The dialog as one outline token: the question, and what a prompt
/// suggests as its answer.
pub(super) fn line(dialog: &PageDialog) -> String {
    use std::fmt::Write as _;
    let mut text = format!(
        "- dialog {} {}",
        kind_label(dialog),
        quote_page(&dialog.message)
    );
    if dialog.kind == PageDialogKind::Prompt && !dialog.default_text.is_empty() {
        let _ = write!(text, " default {}", quote_page(&dialog.default_text));
    }
    if !dialog.origin.is_empty() {
        let _ = write!(text, " from {}", dialog.origin);
    }
    text
}

/// The article the question's name takes in a sentence.
fn article(dialog: &PageDialog) -> &'static str {
    match dialog.kind {
        PageDialogKind::Alert => "an",
        PageDialogKind::Confirm | PageDialogKind::Prompt | PageDialogKind::Leave { .. } => "a",
    }
}

/// The rest of a sentence a prompt and a leave question need.
fn followed_by(dialog: &PageDialog) -> String {
    match dialog.kind {
        PageDialogKind::Prompt => {
            format!(" (it answers with {})", quote_page(&dialog.default_text))
        }
        PageDialogKind::Leave { reload: true } => {
            " (accept reloads the page, cancel stays)".to_string()
        }
        PageDialogKind::Leave { reload: false } => {
            " (accept leaves the page, cancel stays)".to_string()
        }
        PageDialogKind::Alert | PageDialogKind::Confirm => String::new(),
    }
}

/// What an action whose input opened this question answers: what it did,
/// then the question the page asks and how to answer it.
pub(super) fn opened_by(dialog: &PageDialog, act: &str) -> String {
    format!(
        "{act}; the page opened {} {} dialog{}: {}. It waits until you answer it on the \
         {} row, with accept-page-dialog or dismiss-page-dialog.",
        article(dialog),
        kind_label(dialog),
        followed_by(dialog),
        quote_page(&dialog.message),
        crate::page_dialogs::ROW
    )
}

/// What an action whose input opened the page's question answers: a success
/// saying what the action did, carrying the question as both the sentence an
/// agent reads and the value it can act on, with the question's outline line.
pub(super) fn opened_output(
    invocation: ui::tilo_actions::InvocationId,
    act: &str,
    uri: &str,
    title: &str,
    dialog: &PageDialog,
) -> ui::tilo_actions::InvocationUpdate {
    let mut value = serde_json::Map::new();
    value.insert("url".into(), uri.into());
    value.insert("title".into(), title.into());
    value.insert("navigated".into(), false.into());
    value.insert("message".into(), opened_by(dialog, act).into());
    value.insert("dialog".into(), self::value(dialog));
    value.insert("outline".into(), line(dialog).into());
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

/// The question as the value an action that opened it answers with, so an
/// agent reads it without parsing the sentence.
pub(super) fn value(dialog: &PageDialog) -> serde_json::Value {
    serde_json::json!({
        "kind": match dialog.kind {
            PageDialogKind::Leave { reload } => {
                if reload { "reload" } else { "beforeunload" }
            }
            PageDialogKind::Alert => "alert",
            PageDialogKind::Confirm => "confirm",
            PageDialogKind::Prompt => "prompt",
        },
        "message": dialog.message,
        "default_text": dialog.default_text,
        "origin": dialog.origin,
    })
}

/// What a page action refused because of the question says: the question
/// itself, so the agent knows what it is answering.
pub(super) fn blocked(dialog: &PageDialog) -> String {
    format!(
        "The page is waiting on its {} dialog {}{}: answer it on the {} row, with \
         accept-page-dialog or dismiss-page-dialog, before doing anything else to the page.",
        kind_label(dialog),
        quote_page(&dialog.message),
        followed_by(dialog),
        crate::page_dialogs::ROW
    )
}

/// What answering it says a prompt took.
fn answered_with(dialog: &PageDialog, accept: bool, text: &str) -> String {
    if dialog.kind != PageDialogKind::Prompt {
        return String::new();
    }
    if accept {
        format!(" with {}", quote_page(text))
    } else {
        " (the page reads no text)".to_string()
    }
}

/// What answering the question did, and what to do next: the page goes on
/// from here, so reading it again is what shows where it went.
pub(super) fn answered(dialog: &PageDialog, accept: bool, text: &str) -> String {
    let answer = match (dialog.kind, accept) {
        (PageDialogKind::Leave { reload: true }, true) => ": reload the page".to_string(),
        (PageDialogKind::Leave { .. }, true) => ": leave the page".to_string(),
        (_, true) => format!(": OK{}", answered_with(dialog, true, text)),
        (_, false) => format!(": Cancel{}", answered_with(dialog, false, text)),
    };
    format!(
        "Answered the page's {} dialog{answer}. The page goes on from here: read-page shows what \
         it says now, and the {} row asks again if the page has another question.",
        kind_label(dialog),
        crate::page_dialogs::ROW
    )
}

/// What a page action answers while the page waits on its own question: the
/// page runs nothing until the question is answered, so it says what the page
/// asks and how to answer it rather than leaving the action to time out.
pub(super) fn blocked_output(
    invocation: ui::tilo_actions::InvocationId,
    dialog: Option<&PageDialog>,
) -> ui::tilo_actions::InvocationUpdate {
    let message = dialog.map_or_else(
        || {
            format!(
                "The page waits for an answer to its question: answer it on {}",
                crate::page_dialogs::ROW
            )
        },
        blocked,
    );
    ui::tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: ui::tilo_actions::ActionFailure {
            code: "browser.page.question".into(),
            message,
            details: dialog.map(value),
            retryable: false,
        },
    }
}

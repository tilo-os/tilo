//! What a person's agents on a host ask them, as the page is asked it: the
//! export JavaScript calls once a page is in as someone (holding their root,
//! or paired to them), and the sheet an answer is given on.
//!
//! Everything behind them — the approver session that asks the host, the
//! question sheet the session's questions are drawn on, and the workspace
//! transport that session reaches the host over — is reached only through
//! [`QuestionsCode`], declared with `ui::lazy_code!`, so the browser desktop
//! keeps it out of its start module (`deferred-questions`; linked otherwise).
//! A page that is asked nothing never fetches it; one that is asked answers
//! once the code is in, from the requests kept here.

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

use crate::over_page::OverSheet;
use tilo_user_identity::DeviceCert;

/// The agent questions' code: the sheet the questions are drawn on, and the
/// session that asks a host for them.
#[derive(Clone, Copy)]
pub struct QuestionsCode {
    pub(crate) sheet: fn() -> Box<dyn OverSheet>,
    pub(crate) answer: fn(u32, &JsValue, &DeviceCert) -> Result<(), JsError>,
}

/// The agent questions' code this build links.
#[must_use]
pub fn builtin_questions() -> QuestionsCode {
    QuestionsCode {
        sheet: crate::agent_questions::sheet,
        answer: crate::agent_questions::answer_as,
    }
}

ui::lazy_code! {
    /// The agent questions' code.
    pub static QUESTIONS: QuestionsCode = builtin_questions, feature = "deferred-questions";
}

thread_local! {
    /// The sessions whose host's questions the page was asked to answer
    /// while the code was still to come: the session (`WasmPeer::id`), the
    /// key the page answers with and the session's device certificate.
    static ASKED: RefCell<Vec<(u32, JsValue, DeviceCert)>> = const { RefCell::new(Vec::new()) };
    /// Whether the page was asked to answer at all, under any host: only
    /// then is the sheet worth drawing, and its code worth fetching.
    static ANSWERING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this page answers what a host's agents ask.
pub(crate) fn answering() -> bool {
    ANSWERING.with(std::cell::Cell::get)
}

/// Answer what the person's agents on the host of session `peer`
/// (`WasmPeer::id`, whose device `cert` certifies) ask them, over that
/// session, with `custody`: the person's root where the page holds it, else
/// the session's own device key ([`crate::peer::WasmPeer`]'s
/// `answerAgentQuestions`). Once per session; asking again does nothing. The
/// code is fetched if it is not in yet, and the requests kept meanwhile are
/// answered as it arrives.
///
/// # Errors
/// `custody` is no custody.
pub(crate) fn answer_agent_questions(
    peer: u32,
    custody: &JsValue,
    cert: &DeviceCert,
) -> Result<(), JsError> {
    ANSWERING.with(|answering| answering.set(true));
    if let Some(code) = QUESTIONS.get() {
        return (code.answer)(peer, custody, cert);
    }
    ASKED.with_borrow_mut(|asked| asked.push((peer, custody.clone(), cert.clone())));
    Ok(())
}

/// The code is in: answer what the page was asked until then.
pub fn code_arrived() {
    let Some(code) = QUESTIONS.get() else {
        return;
    };
    let asked = ASKED.with_borrow_mut(std::mem::take);
    for (peer, custody, cert) in asked {
        if let Err(error) = (code.answer)(peer, &custody, &cert) {
            tracing::debug!(?error, "agent questions: this page cannot answer them");
        }
    }
}

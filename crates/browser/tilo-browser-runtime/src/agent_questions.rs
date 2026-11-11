//! What a person's agents on a workspace host ask them, on a page holding
//! their root: the approver session a native session keeps
//! ([`tilo_relay_client::questions`]), signed by the root the page holds, and
//! each question on the native question sheet over the desktop
//! ([`tilo_sheets::portal`]), in its words. Allow once runs that one
//! invocation, Deny refuses it; a question the host withdraws, or that
//! expires, takes the sheet away. While the tab is out of view, a
//! notification says an agent waits (the person did not cause it).
//!
//! This is the page's code for it, reached only through
//! [`crate::questions::QuestionsCode`] so that a page which holds no root
//! never fetches the session or the workspace transport it reaches the host
//! over.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};

use tilo_relay_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_sheets::Seat as _;
use tilo_sheets::portal::{AgentQuestion, Dialog, DrawnSheet, PortalEffect, PortalSeat};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;

use crate::browser::sleep_ms;
use crate::over_page::OverSheet;
use crate::peer::PageSigner;

/// How often the page asks a host for new questions.
const POLL_MS: u32 = 2_000;
/// The longest wait before a host that failed is asked again.
const RETRY_MAX_MS: u32 = 30_000;

/// A question waiting for the person, and where its answer goes.
struct Waiting {
    question: PendingConfirmation,
    answer: mpsc::UnboundedSender<Answered>,
}

thread_local! {
    /// The questions waiting, oldest first: the sheet shows the first.
    static WAITING: RefCell<VecDeque<Waiting>> = RefCell::default();
    /// The sessions (`WasmPeer::id`) whose host this page answers.
    static PEERS: RefCell<HashSet<u32>> = RefCell::default();
}

/// Answer what the person's agents on the host of the page's session `peer`
/// (`WasmPeer::id`) ask them, as the person whose root custody the page holds
/// (`root`) — the same rule as a native device's: a page without the root is
/// no approver, and the host refuses it. Every question and answer goes over
/// that session's own endpoint; the page dials nothing else for them. Once
/// per session, until it closes; asking again does nothing.
///
/// # Errors
/// `root` is no custody.
pub(crate) fn answer_for_root(peer: u32, root: &JsValue) -> Result<(), JsError> {
    let root = PageSigner::from_js(root)?;
    if crate::peer_sync::handle_of(peer).is_none()
        || !PEERS.with_borrow_mut(|peers| peers.insert(peer))
    {
        return Ok(());
    }
    wasm_bindgen_futures::spawn_local(async move {
        let mut retry = POLL_MS;
        // A session the page closed is asked nothing more.
        while let Some(session) = crate::peer_sync::handle_of(peer) {
            let began = web_time::Instant::now();
            let (answered, mut answers) = mpsc::unbounded_channel();
            let member_host = tilo_session_client::member_host(&session);
            let mut host = MemberQuestions {
                host: &member_host,
                root: &root,
            };
            let error =
                answer_questions(&mut host, &mut PageSheet { answered }, &mut answers, || {
                    sleep_ms(POLL_MS)
                })
                .await;
            drop(member_host);
            drop(session);
            tracing::debug!(%error, retry_in_ms = retry, "agent questions: the host was not reached");
            // A session that ran a while starts its backoff afresh.
            if began.elapsed().as_millis() > u128::from(RETRY_MAX_MS) {
                retry = POLL_MS;
            }
            sleep_ms(retry).await;
            retry = retry.saturating_mul(2).min(RETRY_MAX_MS);
        }
        PEERS.with_borrow_mut(|peers| peers.remove(&peer));
    });
    Ok(())
}

/// The page's side of a session: each question waits for the sheet.
struct PageSheet {
    answered: mpsc::UnboundedSender<Answered>,
}

impl QuestionSheet for PageSheet {
    fn ask(&mut self, question: &PendingConfirmation) {
        notify(question);
        WAITING.with_borrow_mut(|waiting| {
            waiting.push_back(Waiting {
                question: question.clone(),
                answer: self.answered.clone(),
            });
        });
        crate::browser::wake_desktops();
    }

    fn withdraw(&mut self, id: [u8; 32]) {
        WAITING.with_borrow_mut(|waiting| waiting.retain(|asked| asked.question.id != id));
        unnotify(&id);
        crate::browser::wake_desktops();
    }
}

/// The sheet `question` is asked on, as a native session words it.
fn dialog(question: &PendingConfirmation) -> Dialog {
    AgentQuestion {
        agent: question.agent_label.clone(),
        run: question.run.clone(),
        action: question.action_label.clone(),
        resource: question.resource_label.clone(),
        arguments: question.arguments.clone(),
        risk: question.risk.clone(),
        kind: question.kind.clone(),
        undoable: question.undoable,
        workspace: question.workspace.clone(),
    }
    .dialog(&format!("agent-question:{}", hex::encode(question.id)))
}

/// The notification's tag: at most 15 characters, as every platform keeps.
fn tag(id: &[u8; 32]) -> String {
    format!("agentq-{}", hex::encode(&id[..4]))
}

/// Tell a person whose tab is out of view that an agent waits on them.
fn notify(question: &PendingConfirmation) {
    if !crate::host_services::notifies_browser() {
        return;
    }
    let asked = dialog(question);
    let words = asked.words();
    let notice = ui::notify::Notification::new(tag(&question.id), asked.title, words);
    if let Err(error) = ui::host::current().notifications.show(&notice) {
        tracing::debug!(%error, "agent questions: no notification");
    }
}

fn unnotify(id: &[u8; 32]) {
    let _ = ui::host::current().notifications.withdraw(&tag(id));
}

/// Hand the person's answer to question `id` back to its session.
fn answer(id: [u8; 32], answer: ConfirmationAnswer) {
    let answered = WAITING.with_borrow_mut(|waiting| {
        let at = waiting.iter().position(|asked| asked.question.id == id)?;
        waiting.remove(at)
    });
    if let Some(asked) = answered {
        let _ = asked.answer.send((id, answer));
    }
    unnotify(&id);
}

/// The question sheet over the page: the first waiting question, until the
/// person answers it or the host takes it back.
pub(crate) struct PageQuestions {
    seat: PortalSeat,
    sheet: DrawnSheet,
    /// The question the sheet asks now.
    up: Option<[u8; 32]>,
    /// The page holds a built sheet.
    built: bool,
}

impl PageQuestions {
    pub(crate) fn new() -> Self {
        let seat = PortalSeat::default();
        let sheet = DrawnSheet::new(&seat);
        Self {
            seat,
            sheet,
            up: None,
            built: false,
        }
    }

    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = false;
        // The seat took only presses naming the question up.
        for effect in self.seat.take_asked() {
            let given = match effect {
                PortalEffect::Confirm { .. } => ConfirmationAnswer::AllowOnce,
                PortalEffect::Cancel { .. } => ConfirmationAnswer::Deny,
                _ => continue,
            };
            if let Some(id) = self.up.take() {
                answer(id, given);
                self.seat.presentation().close();
                changed = true;
            }
        }
        let first =
            WAITING.with_borrow(|waiting| waiting.front().map(|asked| asked.question.clone()));
        match first {
            Some(question) if self.up != Some(question.id) => {
                self.seat.show(Some(dialog(&question)));
                self.seat.presentation().open(false);
                self.up = Some(question.id);
                changed = true;
            }
            None if self.up.take().is_some() => {
                self.seat.taken_down();
                changed = true;
            }
            _ => {}
        }
        let presentation = self.seat.presentation();
        // Its card leaves until nothing of it is drawn; then the sheet is down.
        if presentation.phase() == tilo_sheets::Phase::Leaving
            && !presentation.appearance().is_drawn()
        {
            presentation.gone();
        }
        if !presentation.is_drawn() {
            if std::mem::take(&mut self.built) {
                show(None);
                changed = true;
            }
            return changed;
        }
        if let Some(sheet) = self.sheet.rebuilt(missing || !self.built) {
            show(Some(sheet));
            self.built = true;
            changed = true;
        }
        changed
    }
}

/// The sheet an agent's question is drawn on, as the page lays it out.
pub(crate) fn sheet() -> Box<dyn OverSheet> {
    Box::new(PageQuestions::new())
}

impl OverSheet for PageQuestions {
    fn follow_sheet(
        &mut self,
        show: &mut dyn FnMut(Option<ui::widget::Element>),
        missing: bool,
    ) -> bool {
        self.follow(show, missing)
    }

    fn drawn(&self) -> bool {
        self.seat.presentation().is_open()
    }

    fn sheet_name(&self) -> &'static str {
        "PageQuestions"
    }
}

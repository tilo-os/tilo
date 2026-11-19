//! What a person's agents on a workspace host ask them, on a page of theirs:
//! the approver session a native session keeps
//! ([`tilo_server_client::questions`]), signed by the root the page holds or,
//! on a page paired to them, by the page's own device key, and
//! each question on the native question sheet over the desktop
//! ([`tilo_sheets::portal`]), in its words. Allow once runs that one
//! invocation, Deny refuses it; a question the agent words itself is answered
//! with one of its choices or what the person types, and Not now leaves it
//! unanswered. A question the server withdraws, or that expires, takes the
//! sheet away. While the tab is out of view, a
//! notification says an agent waits (the person did not cause it).
//!
//! This is the page's code for it, reached only through
//! [`crate::questions::QuestionsCode`] so that a page which is asked nothing
//! never fetches the session or the workspace transport it reaches the host
//! over.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};

use tilo_server_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_sheets::SheetState as _;
use tilo_sheets::portal::{
    AgentAsk, AgentQuestion, Dialog, DrawnSheet, Field, PortalEffect, PortalState,
};
use tokio::sync::mpsc;
use wasm_bindgen::prelude::*;

use crate::browser::sleep_ms;
use crate::over_page::OverSheet;
use crate::peer::PageSigner;
use tilo_user_identity::DeviceCert;

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
/// (`WasmPeer::id`, whose device `cert` certifies) ask them, with `device key`:
/// the person's root where the page holds it, else the session's own device
/// key, as any device of theirs answers — the person is asked wherever they
/// are. Every question and answer goes over that session's own endpoint; the
/// page dials nothing else for them. Once per session, until it closes;
/// asking again does nothing.
///
/// # Errors
/// `device key` is no device key, or neither the person's root nor the session's
/// device key.
pub(crate) fn answer_as(peer: u32, device_key: &JsValue, cert: &DeviceCert) -> Result<(), JsError> {
    use tilo_server_client::DeviceSigner as _;
    let signer = PageSigner::from_js(device_key)?;
    let key = signer.device_public_key();
    let device = if key == cert.user_public_key {
        None
    } else if key == cert.device_public_key {
        Some(cert.clone())
    } else {
        return Err(JsError::new(
            "the page answers with the person's root or its own device key",
        ));
    };
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
            let member_server = tilo_session_client::member_server(&session);
            let mut host = MemberQuestions {
                server: &member_server,
                root: &signer,
                device: device.as_ref(),
            };
            let error =
                answer_questions(&mut host, &mut PageSheet { answered }, &mut answers, || {
                    sleep_ms(POLL_MS)
                })
                .await;
            drop(member_server);
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

/// The agents waiting for the person to answer them, each once, by the
/// name their client goes by ("Claude Code" for "Claude Code #2"): what the
/// tab says while the person is elsewhere.
pub(crate) fn waiting_agents() -> Vec<String> {
    WAITING.with_borrow(|waiting| {
        let mut agents: Vec<String> = Vec::new();
        for asked in waiting {
            let client = ui::presence::agent_client(&asked.question.agent_label).to_owned();
            if !agents.contains(&client) {
                agents.push(client);
            }
        }
        agents
    })
}

/// The sheet `question` is asked on, as a native session words it.
fn dialog(question: &PendingConfirmation) -> Dialog {
    AgentQuestion {
        agent: question.agent_label.clone(),
        title: question.title.clone(),
        action: question.action_label.clone(),
        resource: question.resource_label.clone(),
        arguments: question.arguments.clone(),
        risk: question.risk.clone(),
        kind: question.kind.clone(),
        undoable: question.undoable,
        workspace: question.workspace.clone(),
        page: question.page_origin(),
        picture: question.page.as_ref().and_then(|page| page.picture.clone()),
        ask: asked_words(question),
    }
    .dialog(&format!("agent-question:{}", hex::encode(question.id)))
}

/// `dialog`, saying how many questions wait behind it: every open question
/// is in this one place, answered one after the other.
fn with_more(mut dialog: Dialog, more: usize) -> Dialog {
    let label = tilo_sheets::word("agent-question-more-label");
    dialog.facts.retain(|(said, _)| *said != label);
    if more > 0 {
        dialog.facts.push((
            label,
            tilo_sheets::words_with(
                "agent-question-more",
                &[(
                    "count",
                    ui::i18n::ArgValue::Number(f64::from(u32::try_from(more).unwrap_or(u32::MAX))),
                )],
            ),
        ));
    }
    dialog
}

/// What `question` asks in its agent's own words, as the sheet asks it.
fn asked_words(question: &PendingConfirmation) -> Option<AgentAsk> {
    question.ask.as_ref().map(|ask| AgentAsk {
        question: ask.question.clone(),
        choices: ask.choices.clone(),
        typed: ask.typed,
    })
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
    if let Err(error) = ui::process::current().notifications.show(&notice) {
        tracing::debug!(%error, "agent questions: no notification");
    }
}

fn unnotify(id: &[u8; 32]) {
    let _ = ui::process::current().notifications.withdraw(&tag(id));
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
    state: PortalState,
    sheet: DrawnSheet,
    /// The question the sheet asks now.
    up: Option<[u8; 32]>,
    /// The page holds a built sheet.
    built: bool,
}

impl PageQuestions {
    pub(crate) fn new() -> Self {
        let state = PortalState::default();
        let sheet = DrawnSheet::new(&state);
        Self {
            state,
            sheet,
            up: None,
            built: false,
        }
    }

    fn follow(&mut self, show: &mut dyn FnMut(Option<ui::widget::Element>), missing: bool) -> bool {
        let mut changed = false;
        // The state took only presses naming the question up.
        let asking = self.up.and_then(|id| {
            WAITING.with_borrow(|waiting| {
                waiting
                    .iter()
                    .find(|asked| asked.question.id == id)
                    .and_then(|asked| asked_words(&asked.question))
            })
        });
        for effect in self.state.take_asked() {
            let given = match (effect, &asking) {
                (PortalEffect::Cancel { .. }, _) => ConfirmationAnswer::Deny,
                (PortalEffect::Confirm { .. }, None) => ConfirmationAnswer::AllowOnce,
                (PortalEffect::Select { index, .. }, Some(_)) => {
                    // A choice above the field is picked, not yet sent.
                    if let Some(mut shown) = self.state.dialog() {
                        shown.selected = usize::try_from(index).unwrap_or(usize::MAX);
                        shown.error = None;
                        self.state.show(Some(shown));
                        changed = true;
                    }
                    continue;
                }
                (PortalEffect::Answer { index, .. }, Some(ask)) => {
                    match ask.reply(usize::try_from(index).ok(), "") {
                        Some(reply) => ConfirmationAnswer::Reply(reply),
                        None => continue,
                    }
                }
                (PortalEffect::Confirm { .. }, Some(ask)) => {
                    let chosen = self.state.dialog().map(|shown| shown.selected);
                    let Some(reply) = ask.reply(chosen, &self.state.text(Field::Answer)) else {
                        if let Some(mut shown) = self.state.dialog() {
                            shown.error = Some(tilo_sheets::word("agent-ask-pick-or-type"));
                            self.state.show(Some(shown));
                            changed = true;
                        }
                        continue;
                    };
                    ConfirmationAnswer::Reply(reply)
                }
                _ => continue,
            };
            if let Some(id) = self.up.take() {
                answer(id, given);
                self.state.presentation().close();
                changed = true;
            }
        }
        let (first, more) = WAITING.with_borrow(|waiting| {
            (
                waiting.front().map(|asked| asked.question.clone()),
                waiting.len().saturating_sub(1),
            )
        });
        match first {
            Some(question) if self.up != Some(question.id) => {
                self.state.show(Some(with_more(dialog(&question), more)));
                self.state.presentation().open(false);
                self.up = Some(question.id);
                changed = true;
            }
            // Another question came, or one behind it went: the sheet says
            // how many more wait, and keeps what the person picked.
            Some(_) => {
                if let Some(shown) = self.state.dialog() {
                    let counted = with_more(shown.clone(), more);
                    if counted != shown {
                        self.state.show(Some(counted));
                        changed = true;
                    }
                }
            }
            None if self.up.take().is_some() => {
                self.state.taken_down();
                changed = true;
            }
            _ => {}
        }
        let presentation = self.state.presentation();
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
        self.state.presentation().is_open()
    }

    fn sheet_name(&self) -> &'static str {
        "PageQuestions"
    }
}

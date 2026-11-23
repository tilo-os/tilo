//! Questions this person's agents on their workspace hosts put to them.
//! An agent a host runs for the person never confirms an action its grants leave to them by its own
//! word: the host asks the person's devices about that one invocation. This device, when it is one
//! of the person's own (no agent's key), keeps an approver session with each host it reaches: the
//! hosts the peer network connects to, and the hosts of the workspaces placed away from it,
//! puts every question on the session's question sheet, and sends the answer back: the person is
//! asked on whichever of their devices they are at. A question that is withdrawn or expires, or
//! that another of their devices answered, takes its sheet away.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_lock::MutexRecover as _;
use tilo_network_actor::DeviceIdentity;
use tilo_network_actor::services::Approvers;
use tilo_server_client::Approver;
use tilo_server_client::questions::{Answered, QuestionSheet, answer_questions};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation, TypedSecret};
use tilo_service::{Question, Questions};
use tilo_sheets::portal::{ALLOW_ALWAYS, AgentAsk, AgentQuestion};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

/// The kind the session's sheet reads an agent's question as.
pub(crate) const KIND: &str = "network.agent-question";

/// How often an open session asks its host for new questions.
const POLL: Duration = Duration::from_millis(250);
/// The longest wait before an approver session that failed is opened again.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// The approver sessions this device keeps, one per host: the one set the peer network's hosts
/// and the hosts of the workspaces placed away from this device share, each host answered on one
/// session however this device came to reach it. Two sessions with one host would put each of its
/// questions on the sheet twice, the second taking the first's place and answering it no.
#[derive(Clone, Default)]
pub(crate) struct Answering {
    questions: Option<Questions>,
    sessions: Arc<Mutex<HashMap<EndpointId, tokio::task::JoinHandle<()>>>>,
}

impl Answering {
    pub(crate) fn new(questions: Option<Questions>) -> Self {
        Self {
            questions,
            sessions: Arc::default(),
        }
    }

    /// Answer `host` 's questions from now on, on `endpoint`, when this device may: an agent's key
    /// or a stand-in is no device of the person's and answers nothing for them. A host already
    /// answered keeps its session. A host this device's person has nothing at refuses the session,
    /// which is tried again later. Called inside the runtime the session runs on.
    pub(crate) fn answer(
        &self,
        endpoint: &Endpoint,
        host: EndpointAddr,
        identity: &DeviceIdentity,
    ) {
        let capabilities = &identity.device_cert.capabilities;
        if capabilities.agent.is_some() || capabilities.stand_in {
            return;
        }
        let Some(questions) = self.questions.clone() else {
            return;
        };
        let mut sessions = self.sessions.lock_recover();
        if sessions
            .get(&host.id)
            .is_some_and(|session| !session.is_finished())
        {
            return;
        }
        let key = Arc::clone(&identity.signer);
        let id = host.id;
        let session = tokio::spawn(answer_host(
            endpoint.clone(),
            host,
            identity.device_cert.clone(),
            key,
            questions,
        ));
        sessions.insert(id, session);
    }
}

impl Approvers for Answering {
    fn start(&mut self, endpoint: &Endpoint, host: EndpointAddr, identity: &DeviceIdentity) {
        self.answer(endpoint, host, identity);
    }

    fn stop(&mut self, host: EndpointId) {
        let ended = self.sessions.lock_recover().remove(&host);
        if let Some(session) = ended {
            session.abort();
        }
    }
}

/// Keep an approver session with `host` open and answer its questions,
/// reopening it with backoff whenever it fails.
async fn answer_host(
    endpoint: Endpoint,
    host: EndpointAddr,
    cert: DeviceCert,
    key: Arc<dyn collab_types::signer::DeviceSigner>,
    questions: Questions,
) {
    let mut wait = POLL;
    loop {
        let opened = Approver::connect(
            &endpoint,
            host.clone(),
            cert.clone(),
            key.as_ref(),
            &RevocationList::new(),
        )
        .await;
        let error = match opened {
            Ok(mut approver) => {
                wait = POLL;
                let (answered, mut answers) = mpsc::unbounded_channel();
                let mut sheet = SessionSheet {
                    questions: questions.clone(),
                    answered,
                    shown: HashMap::new(),
                };
                answer_questions(&mut approver, &mut sheet, &mut answers, || {
                    tokio::time::sleep(POLL)
                })
                .await
            }
            Err(error) => error,
        };
        tracing::debug!(host = %host.id, %error, retry_in = ?wait, "agent questions: approver session ended");
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2).min(RETRY_MAX);
    }
}

/// What a host's question asks in words, as the sheet asks it.
fn sheet_ask(ask: &tilo_server_protocol::certify::AgentAsk) -> AgentAsk {
    AgentAsk {
        question: ask.question.clone(),
        choices: ask.choices.clone(),
        secret: ask.secret,
    }
}

/// The session's question sheet, as an approver session shows its questions on it.
struct SessionSheet {
    questions: Questions,
    answered: mpsc::UnboundedSender<Answered>,
    /// The questions up, each waiting on the person's answer.
    shown: HashMap<[u8; 32], tokio::task::JoinHandle<()>>,
}

impl QuestionSheet for SessionSheet {
    fn ask(&mut self, question: &PendingConfirmation) {
        let id = question.id;
        let asked = self.questions.ask(Question {
            handle: format!("agent-question:{}", hex::encode(id)),
            kind: KIND.to_owned(),
            app_id: None,
            label: question.agent_label.clone(),
            details: serde_json::to_value(AgentQuestion::of(question)).unwrap_or_default(),
        });
        let answered = self.answered.clone();
        let allow_always = question.allow_always;
        let ask = question.ask.as_ref().map(sheet_ask);
        let task = tokio::spawn(async move {
            let answer = match (asked.await, ask) {
                (Some(given), Some(ask)) if ask.secret => given
                    .values
                    .into_iter()
                    .next()
                    .filter(|typed| !typed.is_empty())
                    .map_or(ConfirmationAnswer::Deny, |typed| {
                        ConfirmationAnswer::Secret(TypedSecret::new(String::clone(&typed)))
                    }),
                (Some(given), Some(ask)) => ask
                    .reply(given.choice)
                    .map_or(ConfirmationAnswer::Deny, ConfirmationAnswer::Reply),
                (Some(given), None) if allow_always => match given.choice {
                    Some(0) => ConfirmationAnswer::AllowOnce,
                    Some(ALLOW_ALWAYS) => ConfirmationAnswer::AllowAlways,
                    _ => ConfirmationAnswer::Deny,
                },
                (Some(_), None) => ConfirmationAnswer::AllowOnce,
                (None, _) => ConfirmationAnswer::Deny,
            };
            let _ = answered.send((id, answer));
        });
        self.shown.insert(id, task);
    }

    fn withdraw(&mut self, id: [u8; 32]) {
        if let Some(task) = self.shown.remove(&id) {
            task.abort();
        }
    }
}

//! Questions this person's agents on their workspace hosts put to them.
//! An agent a host runs for the person never confirms an action its grants leave to them by its own
//! word: the host asks the person's approving devices about that one invocation. This device, when
//! it may admit devices (it holds the person's root custody), keeps an approver session with each
//! host it connects to, puts every question on the session's question sheet, and sends the answer
//! back. A question that is withdrawn or expires takes its sheet away.

use std::collections::HashMap;
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_network_actor::HostIdentity;
use tilo_network_actor::services::Approvers;
use tilo_relay_client::Approver;
use tilo_relay_client::questions::{Answered, QuestionSheet, answer_questions};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_service::{Question, Questions};
use tilo_sheets::portal::AgentQuestion;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

/// The kind the session's sheet reads an agent's question as.
pub(crate) const KIND: &str = "network.agent-question";

/// How often an open session asks its host for new questions.
const POLL: Duration = Duration::from_millis(250);
/// The longest wait before an approver session that failed is opened again.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// The approver sessions this device keeps, one per host.
#[derive(Default)]
pub(crate) struct Answering {
    questions: Option<Questions>,
    sessions: HashMap<EndpointId, tokio::task::JoinHandle<()>>,
}

impl Answering {
    pub(crate) fn new(questions: Option<Questions>) -> Self {
        Self {
            questions,
            sessions: HashMap::new(),
        }
    }
}

impl Approvers for Answering {
    /// Answer `host` 's questions from now on, when this device may: a device that cannot admit
    /// devices is no approver, and its host refuses it.
    fn start(&mut self, endpoint: &Endpoint, host: EndpointAddr, identity: &HostIdentity) {
        if !identity.device_cert.capabilities.can_admit_devices
            || self
                .sessions
                .get(&host.id)
                .is_some_and(|session| !session.is_finished())
        {
            return;
        }
        let Some(questions) = self.questions.clone() else {
            return;
        };
        let Ok(key) = identity.signing_key() else {
            return;
        };
        let id = host.id;
        let session = tokio::spawn(answer_host(
            endpoint.clone(),
            host,
            identity.device_cert.clone(),
            key,
            questions,
        ));
        self.sessions.insert(id, session);
    }

    fn stop(&mut self, host: EndpointId) {
        if let Some(session) = self.sessions.remove(&host) {
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
    key: ed25519_dalek::SigningKey,
    questions: Questions,
) {
    let mut wait = POLL;
    loop {
        let opened = Approver::connect(
            &endpoint,
            host.clone(),
            cert.clone(),
            &key,
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
            details: serde_json::to_value(AgentQuestion {
                agent: question.agent_label.clone(),
                run: question.run.clone(),
                action: question.action_label.clone(),
                resource: question.resource_label.clone(),
                arguments: question.arguments.clone(),
                risk: question.risk.clone(),
                kind: question.kind.clone(),
                undoable: question.undoable,
                workspace: question.workspace.clone(),
            })
            .unwrap_or_default(),
        });
        let answered = self.answered.clone();
        let task = tokio::spawn(async move {
            let answer = match asked.await {
                Some(_) => ConfirmationAnswer::AllowOnce,
                None => ConfirmationAnswer::Deny,
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

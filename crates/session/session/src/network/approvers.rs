//! Questions this person's agents on their workspace hosts put to them.
//! An agent a host runs for the person never confirms an action its grants leave to them by its own
//! word: the host asks the person's approving devices about that one invocation. This device, when
//! it may admit devices (it holds the person's root custody), keeps an approver session with each
//! host it connects to, puts every question on the session's question sheet, and sends the answer
//! back. A question that is withdrawn or expires takes its sheet away.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use tilo_network_actor::HostIdentity;
use tilo_network_actor::services::Approvers;
use tilo_relay_client::{Approver, CertifyError};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_service::{Question, Questions};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

/// The kind the session's sheet reads an agent's question as.
pub(crate) const KIND: &str = "network.agent-question";

/// How often an open session asks its host for new questions.
const POLL: Duration = Duration::from_millis(250);
/// The longest wait before an approver session that failed is opened again.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// An agent's question, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct AgentQuestion {
    /// The agent, as the host names it.
    pub agent: String,
    /// What it wants to do.
    pub action: String,
    /// What it acts on.
    pub resource: String,
    /// What the host says the action risks.
    pub risk: String,
    /// The workspace it acts in.
    pub workspace: String,
}

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
            Ok(approver) => {
                wait = POLL;
                serve(approver, &questions).await
            }
            Err(error) => error,
        };
        tracing::debug!(host = %host.id, %error, retry_in = ?wait, "agent questions: approver session ended");
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2).min(RETRY_MAX);
    }
}

/// Show every question the host lists on the sheet, once, and hand each answer back, until the
/// session fails. Questions that are gone are withdrawn from the sheet.
async fn serve(mut approver: Approver, questions: &Questions) -> CertifyError {
    let (answered, mut answers) = mpsc::unbounded_channel::<([u8; 32], ConfirmationAnswer)>();
    let mut shown: HashMap<[u8; 32], tokio::task::JoinHandle<()>> = HashMap::new();
    // The questions the person has answered, kept until the host stops listing them: a list read
    // while the answer is on its way does not put the question back on their sheet.
    let mut done: HashSet<[u8; 32]> = HashSet::new();
    loop {
        let listed = match approver.list_confirmations().await {
            Ok(listed) => listed,
            Err(error) => {
                withdraw(&mut shown);
                return error;
            }
        };
        let live: HashSet<[u8; 32]> = listed.iter().map(|question| question.id).collect();
        done.retain(|id| live.contains(id));
        let gone: Vec<[u8; 32]> = shown
            .keys()
            .filter(|id| !live.contains(*id))
            .copied()
            .collect();
        for id in gone {
            if let Some(task) = shown.remove(&id) {
                task.abort();
            }
        }
        for question in listed {
            if shown.contains_key(&question.id) || done.contains(&question.id) {
                continue;
            }
            let id = question.id;
            let asked = questions.ask(Question {
                handle: handle(&id),
                kind: KIND.to_owned(),
                app_id: None,
                label: question.agent_label.clone(),
                details: details(&question),
            });
            let answered = answered.clone();
            let task = tokio::spawn(async move {
                let answer = match asked.await {
                    Some(_) => ConfirmationAnswer::AllowOnce,
                    None => ConfirmationAnswer::Deny,
                };
                let _ = answered.send((id, answer));
            });
            shown.insert(id, task);
        }
        tokio::select! {
            () = tokio::time::sleep(POLL) => {}
            Some((id, answer)) = answers.recv() => {
                if let Some(task) = shown.remove(&id) {
                    task.abort();
                }
                done.insert(id);
                if let Err(error) = approver.answer(id, answer).await {
                    withdraw(&mut shown);
                    return error;
                }
            }
        }
    }
}

/// The question's handle on the sheet.
fn handle(id: &[u8; 32]) -> String {
    format!("agent-question:{}", hex::encode(id))
}

/// Withdraw every question still shown: the session that asked them ended.
fn withdraw(shown: &mut HashMap<[u8; 32], tokio::task::JoinHandle<()>>) {
    for (_, task) in shown.drain() {
        task.abort();
    }
}

/// The question as the sheet reads it.
fn details(question: &PendingConfirmation) -> serde_json::Value {
    serde_json::to_value(AgentQuestion {
        agent: question.agent_label.clone(),
        action: question.action_label.clone(),
        resource: question.resource_label.clone(),
        risk: question.risk.clone(),
        workspace: question.workspace.clone(),
    })
    .unwrap_or_default()
}

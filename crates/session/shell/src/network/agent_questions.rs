//! Questions this person's agents on their hosts put to them.
//!
//! An agent a host runs for the person never confirms an action its grants
//! leave to them by its own word: the host asks the person's approving
//! devices about that one invocation. This device, when it may admit
//! devices (it holds the person's root custody), keeps an approver session
//! with each host it connects to, puts every question on the compositor's
//! confirmation sheet — the same "… wants to act for you" sheet an agent or
//! app acting for the person on this machine is confirmed on, Allow once or
//! Deny — and sends the answer back. The sheet appears over whatever the
//! person is doing; the agent meanwhile shows it is waiting in the room's
//! presence. A question that is withdrawn or expires takes its sheet away.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_action_broker::app_actions::{AppActionConsent, AppActionConsentRequest};
use tilo_relay_client::{Approver, CertifyError};
use tilo_relay_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::mpsc;

use crate::network::actor::HostIdentity;
use crate::shell::portal_dialog::PortalDialogQueue;

/// How often an open session asks its host for new questions.
const POLL: Duration = Duration::from_millis(250);
/// The longest wait before an approver session that failed is opened again.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Where questions are shown, and whether anyone is there to answer them.
#[derive(Clone)]
pub(crate) struct Sheet {
    queue: PortalDialogQueue,
    present: fn() -> bool,
}

impl Sheet {
    /// This session's sheet, answered only while someone is at its
    /// unlocked screen.
    pub(crate) fn session() -> Self {
        Self {
            queue: PortalDialogQueue::global(),
            present: crate::shell::app_action_consent::session_attended,
        }
    }
}

/// The approver sessions this device keeps, one per host.
#[derive(Default)]
pub(crate) struct Answering {
    sheet: Option<Sheet>,
    sessions: HashMap<EndpointId, tokio::task::JoinHandle<()>>,
}

impl Answering {
    /// Answer `host`'s questions from now on, when this device may: a device
    /// that cannot admit devices is no approver, and its host refuses it.
    pub(crate) fn start(
        &mut self,
        endpoint: &Endpoint,
        host: EndpointAddr,
        identity: &HostIdentity,
    ) {
        if !identity.device_cert.capabilities.can_admit_devices
            || self
                .sessions
                .get(&host.id)
                .is_some_and(|session| !session.is_finished())
        {
            return;
        }
        let sheet = self.sheet.get_or_insert_with(Sheet::session).clone();
        let id = host.id;
        let session = tokio::spawn(answer_host(
            endpoint.clone(),
            host,
            identity.device_cert.clone(),
            identity.device_signing_key.clone(),
            sheet,
        ));
        self.sessions.insert(id, session);
    }

    /// Stop answering `host`'s questions.
    pub(crate) fn stop(&mut self, host: EndpointId) {
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
    sheet: Sheet,
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
                serve(approver, &sheet, &hex::encode(cert.user_public_key)).await
            }
            Err(error) => error,
        };
        tracing::debug!(host = %host.id, %error, retry_in = ?wait, "agent questions: approver session ended");
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2).min(RETRY_MAX);
    }
}

/// Show every question the host lists on the sheet, once, and hand each
/// answer back, until the session fails. Sheets of questions that are gone
/// are withdrawn.
async fn serve(mut approver: Approver, sheet: &Sheet, person: &str) -> CertifyError {
    let (answered, mut answers) = mpsc::unbounded_channel::<([u8; 32], ConfirmationAnswer)>();
    let mut shown: HashSet<[u8; 32]> = HashSet::new();
    loop {
        let asked = match approver.list_confirmations().await {
            Ok(asked) => asked,
            Err(error) => {
                withdraw(sheet, shown.drain());
                return error;
            }
        };
        let gone: Vec<[u8; 32]> = shown
            .iter()
            .filter(|id| !asked.iter().any(|question| question.id == **id))
            .copied()
            .collect();
        for id in &gone {
            shown.remove(id);
        }
        withdraw(sheet, gone.into_iter());
        for question in asked {
            if shown.contains(&question.id) || !(sheet.present)() {
                continue;
            }
            let Ok(answer) = sheet.queue.enqueue_action_grant(request(&question, person)) else {
                continue;
            };
            shown.insert(question.id);
            let answered = answered.clone();
            tokio::spawn(async move {
                let answer = match answer.await {
                    Ok(AppActionConsent::Deny) | Err(_) => ConfirmationAnswer::Deny,
                    Ok(_) => ConfirmationAnswer::AllowOnce,
                };
                let _ = answered.send((question.id, answer));
            });
        }
        tokio::select! {
            () = tokio::time::sleep(POLL) => {}
            Some((id, answer)) = answers.recv() => {
                shown.remove(&id);
                if let Err(error) = approver.answer(id, answer).await {
                    withdraw(sheet, shown.drain());
                    return error;
                }
            }
        }
    }
}

fn handle(id: &[u8; 32]) -> String {
    format!("agent-question:{}", hex::encode(id))
}

fn withdraw(sheet: &Sheet, ids: impl Iterator<Item = [u8; 32]>) {
    for id in ids {
        sheet.queue.cancel(&handle(&id));
    }
}

/// The sheet's question: the agent confirms one invocation for `person`,
/// which grants nothing.
fn request(question: &PendingConfirmation, person: &str) -> AppActionConsentRequest {
    AppActionConsentRequest {
        handle: handle(&question.id),
        room: question.room.clone(),
        person: person.to_owned(),
        app_id: question.agent_label.clone(),
        target_app: String::new(),
        resource: String::new(),
        action: String::new(),
        resource_label: question.resource_label.clone(),
        action_label: question.action_label.clone(),
        // A risk the host could not name is read as the gravest it may be.
        risk: tilo_actions::RiskLevel::from_name(&question.risk)
            .unwrap_or(tilo_actions::RiskLevel::Destructive),
        needs_confirmation: true,
        confirming: true,
        moderation: None,
    }
}

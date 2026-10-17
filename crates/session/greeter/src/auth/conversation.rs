//! Private transient responses for the existing greetd authentication worker.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use ui::app::UserEvent;
use winit::event_loop::EventLoopProxy;
use zeroize::Zeroizing;

use crate::greetd_client::{
    AuthError, AuthMessageType, ConversationControl, authenticate_conversation,
};

use super::{AuthUpdate, Failure};
use tilo_lock::MutexRecover as _;

/// A public challenge identity and its response kind; never the response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub id: u64,
    pub secret: bool,
}

enum Response {
    Answer(Zeroizing<String>),
    Cancel,
}

struct PendingChallenge {
    identity: Challenge,
    question: String,
}

/// Control of one actual worker. All response values stay process-local and
/// zeroized, including rejected responses and abandoned channel messages.
#[derive(Clone)]
pub struct AuthSession {
    conversation: ConversationControl,
    pending: Arc<Mutex<Option<PendingChallenge>>>,
    responses: mpsc::SyncSender<Response>,
}

impl AuthSession {
    pub fn challenge(&self) -> Option<Challenge> {
        if self.is_cancelling() {
            return None;
        }
        self.pending
            .lock_recover()
            .as_ref()
            .map(|pending| pending.identity)
    }

    /// The exact waiting challenge and its bounded, redacted normal question.
    /// Raw error details and private responses are never part of this value.
    pub fn question(&self) -> Option<(Challenge, String)> {
        if self.is_cancelling() {
            return None;
        }
        self.pending
            .lock_recover()
            .as_ref()
            .map(|pending| (pending.identity, pending.question.clone()))
    }

    /// Supply one private response to the currently waiting challenge.
    ///
    /// # Errors
    /// Refuses oversized, stale or cancelled responses without echoing values.
    pub fn respond(&self, id: u64, response: Zeroizing<String>) -> Result<(), &'static str> {
        if response.len() > 4096 {
            return Err("The authentication response exceeds its limit");
        }
        let mut pending = self.pending.lock_recover();
        if pending.as_ref().map(|pending| pending.identity.id) != Some(id)
            || self.conversation.is_cancelled()
        {
            return Err("This authentication challenge is no longer waiting for a response");
        }
        self.responses
            .try_send(Response::Answer(response))
            .map_err(|_| "The authentication response channel is unavailable")?;
        pending.take();
        drop(pending);
        Ok(())
    }

    /// Cancel this normal greetd attempt and wake its private response waiter.
    ///
    /// # Errors
    /// Refuses cancellation after actual start/completion or socket failure.
    pub fn cancel(&self) -> Result<(), &'static str> {
        self.conversation
            .cancel()
            .map_err(|_| "The authentication attempt cannot be cancelled now")?;
        self.pending.lock_recover().take();
        // Wake a worker waiting for a private response. If an answer already
        // occupies the one slot, the shared cancellation check rejects it.
        let _ = self.responses.try_send(Response::Cancel);
        Ok(())
    }

    pub fn is_cancelling(&self) -> bool {
        self.conversation.is_cancelled()
    }

    pub fn can_cancel(&self) -> bool {
        self.conversation.can_cancel()
    }
}

struct Attempt {
    sock: String,
    username: String,
    password: Zeroizing<String>,
    cmd: Vec<String>,
    env: Vec<String>,
}

fn session() -> (AuthSession, mpsc::Receiver<Response>) {
    let (responses, receiver) = mpsc::sync_channel(1);
    (
        AuthSession {
            conversation: ConversationControl::default(),
            pending: Arc::new(Mutex::new(None)),
            responses,
        },
        receiver,
    )
}

/// Run normal authentication without blocking the UI. The monotonically
/// increasing challenge identity belongs to the greeter process lifetime,
/// so a response to an old attempt cannot settle a later attempt's prompt.
pub fn spawn_auth(
    proxy: EventLoopProxy<UserEvent>,
    sock: String,
    username: String,
    password: Zeroizing<String>,
    cmd: Vec<String>,
    env: Vec<String>,
    sequence: Arc<AtomicU64>,
) -> AuthSession {
    spawn_auth_updates(
        sock,
        username,
        password,
        cmd,
        env,
        sequence,
        move |update| {
            let _ = proxy.send_event(UserEvent::External(update.encode()));
        },
    )
}

pub(crate) fn spawn_auth_updates<F: Fn(AuthUpdate) + Send + 'static>(
    sock: String,
    username: String,
    password: Zeroizing<String>,
    cmd: Vec<String>,
    env: Vec<String>,
    sequence: Arc<AtomicU64>,
    update: F,
) -> AuthSession {
    let (session, receiver) = session();
    let worker_session = session.clone();
    std::thread::spawn(move || {
        run_worker(
            Attempt {
                sock,
                username,
                password,
                cmd,
                env,
            },
            &worker_session,
            &receiver,
            &sequence,
            update,
        );
    });
    session
}

fn run_worker<F: Fn(AuthUpdate)>(
    attempt: Attempt,
    session: &AuthSession,
    receiver: &mpsc::Receiver<Response>,
    sequence: &AtomicU64,
    update: F,
) {
    let mut first_secret = true;
    // Keep at most 32 private answers while this attempt runs, solely to
    // remove literal credential echoes from the ordinary local PAM prompt.
    // No raw prompt is published through console state or resource values.
    let mut secrets = vec![attempt.password];
    let result = authenticate_conversation(
        &attempt.sock,
        &attempt.username,
        &attempt.cmd,
        &attempt.env,
        &session.conversation,
        |kind, mut text| {
            for secret in &secrets {
                if !secret.is_empty() {
                    text = text.replace(secret.as_str(), "");
                }
            }
            // The same bounded redacted text serves the local UI and the
            // waiting challenge's semantic question (at most 4096 UTF-8 bytes).
            let text: String = text.chars().take(1024).collect();
            if matches!(kind, AuthMessageType::Info | AuthMessageType::Error) {
                update(AuthUpdate::Prompt {
                    text,
                    is_error: matches!(kind, AuthMessageType::Error),
                });
                return Ok(None);
            }
            if matches!(kind, AuthMessageType::Secret) && first_secret {
                first_secret = false;
                return Ok(Some(
                    secrets
                        .first()
                        .map_or_else(String::new, |secret| secret.to_string()),
                ));
            }
            if secrets.len() >= 32 {
                return Err(AuthError::Greetd(
                    "The authentication conversation exceeded its response limit".into(),
                ));
            }
            let id = sequence
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .map_err(|_| {
                    AuthError::Greetd("Authentication challenge identities are exhausted".into())
                })?;
            let challenge = Challenge {
                id,
                secret: matches!(kind, AuthMessageType::Secret),
            };
            *session.pending.lock_recover() = Some(PendingChallenge {
                identity: challenge,
                question: text.clone(),
            });
            update(AuthUpdate::Prompt {
                text,
                is_error: false,
            });
            update(AuthUpdate::Challenge(challenge));
            match receiver.recv() {
                Ok(Response::Answer(answer)) if !session.conversation.is_cancelled() => {
                    let reply = answer.to_string();
                    secrets.push(answer);
                    Ok(Some(reply))
                }
                Ok(Response::Answer(_) | Response::Cancel) => Err(AuthError::Cancelled),
                Err(_) => Err(AuthError::Transport(
                    "The authentication response channel closed".into(),
                )),
            }
        },
        || update(AuthUpdate::Accepted),
    );
    session.pending.lock_recover().take();
    if let Err(error) = result {
        let failure = Failure::from(&error);
        tracing::warn!(?failure, "sign-in did not start a session");
        update(AuthUpdate::Failed(failure));
    }
}

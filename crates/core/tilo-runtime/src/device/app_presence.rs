//! An app's place in its workspace's live presence, on the connection its launcher handed it.
//!
//! A sandboxed app holds no live session: its views say where their person is — the resource
//! they show, the caret in it, their pointer, that they are composing a message, the steps of an
//! action as they run — as one word under a presence key ([`collab_types::presence::Said`]), and
//! they are shown the other members' words the same way ([`collab_types::presence::Heard`]). The
//! device's live presence says the app's words for the connection's app, workspace and member in
//! the workspace's live session, exactly as the shell's own words go, and hands back what the
//! session hears from the workspace's other devices. A connection that ends leaves every key it
//! said, so nobody is shown at a caret whose window closed.
//!
//! Words are checked and attributed here, never taken from the app: a word is the app's own in
//! its own workspace, a status is trimmed and bounded, an interaction is attributed to the
//! connection's member and numbered by this lane, and the desktop's own word (its availability)
//! is the shell's, never an app's.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use collab_types::MemberId;
use collab_types::presence::{Heard, PresenceKey, PresencePayload, Said};
use tilo_actions::remote::{ActionCallError, CallLanes, RemoteActionMessage, Take, frame_line};
use tilo_lock::MutexRecover as _;

use super::live_relay::Live;
use super::live_speaking::Watch;
use super::services::Daemon;
use crate::seats::AppAt;

/// Where the lane writes what the session hears: the app connection's own writer.
pub(crate) type Out = Arc<dyn Fn(String) + Send + Sync>;

/// What an app's connection needs to say and hear presence: the device's live session, the app
/// and workspace the launcher started it in, the member it speaks for, and whether it is still
/// that process.
#[derive(Clone)]
pub(crate) struct Setup {
    live: Arc<Live>,
    app: String,
    workspace: String,
    member: MemberId,
    current: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl Setup {
    /// The presence of the app process `at`, which `daemon` started: `None` while this device
    /// runs no live session (then the lane refuses each word with the reason).
    pub(crate) fn of(daemon: &Arc<Daemon>, at: &AppAt) -> Option<Self> {
        let live = daemon.live_session()?;
        let held = Arc::downgrade(daemon);
        let (app, workspace, pid) = (at.app.clone(), at.workspace.clone(), at.pid);
        let current: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::new(move || {
            let daemon = held
                .upgrade()
                .ok_or_else(|| "this computer's daemon is stopping".to_string())?;
            daemon
                .app_of(pid)
                .filter(|(named, in_workspace)| *named == app && *in_workspace == workspace)
                .map(|_| ())
                .ok_or_else(|| "the app's process ended".to_string())
        });
        Some(Self {
            live: Arc::clone(live),
            app: at.app.clone(),
            workspace: at.workspace.clone(),
            member: MemberId::person(daemon.person_id()),
            current,
        })
    }
}

/// The presence lane an app's connection carries: the words its views say, and the words the
/// workspace's live session hears for them.
pub(crate) struct PresenceLane {
    setup: Setup,
    /// The app hears its workspace's words until this is dropped.
    watch: Watch,
    /// How far this connection got with each invocation it narrates.
    sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
    /// The keys this connection said a word under, which it leaves when it goes.
    said: Mutex<HashSet<PresenceKey>>,
}

impl PresenceLane {
    #[must_use]
    pub(crate) fn new(setup: Setup, out: Out) -> Self {
        let workspace = setup.workspace.clone();
        let watch = setup
            .live
            .speaking
            .watch_app(&workspace, move |member, frame| {
                let heard = Heard {
                    member,
                    key: frame.key,
                    payload: frame.payload,
                };
                match serde_json::to_string(&heard) {
                    Ok(heard) => out(frame_line(RemoteActionMessage::PresenceHeard { heard })),
                    Err(error) => tracing::warn!(%error, "a presence word does not write"),
                }
            });
        Self {
            setup,
            watch,
            sequences: Mutex::default(),
            said: Mutex::default(),
        }
    }

    /// One word of the app's person, answered whether the live presence said it or refused it.
    fn take(&self, correlation: u64, said: &str) -> Take {
        let result = self.say(said);
        Take::Answer(frame_line(RemoteActionMessage::PresenceSaidResult {
            correlation,
            result,
        }))
    }

    fn say(&self, said: &str) -> Result<(), ActionCallError> {
        (self.setup.current)().map_err(|reason| ActionCallError::Refused { reason })?;
        let Said { key, payload } =
            serde_json::from_str(said).map_err(|error| ActionCallError::InvalidRequest {
                reason: format!("a presence word does not read: {error}"),
            })?;
        let payload = PresencePayload::from_live(payload)
            .map_err(|reason| ActionCallError::InvalidRequest { reason })?
            .filter(PresencePayload::reaches_apps)
            .ok_or_else(|| ActionCallError::Refused {
                reason: "the desktop's own word is the shell's, never an app's".to_owned(),
            })?;
        let payload = self.attributed(payload)?;
        let live = payload
            .to_live()
            .ok_or_else(|| ActionCallError::InvalidRequest {
                reason: "a presence word the live session cannot carry".to_owned(),
            })?;
        {
            let mut keys = self.said.lock_recover();
            if matches!(live, collab_types::presence::Payload::Left) {
                keys.remove(&key);
            } else {
                keys.insert(key.clone());
            }
        }
        self.setup.live.speaking.say_app(
            &self.setup.app,
            &self.setup.workspace,
            &self.setup.member,
            key,
            live,
            Some(self.watch.id()),
        );
        Ok(())
    }

    /// `payload` as this connection may say it: a status trimmed and bounded, an interaction
    /// attributed to the connection's member and numbered here.
    fn attributed(&self, payload: PresencePayload) -> Result<PresencePayload, ActionCallError> {
        match payload {
            PresencePayload::Status { text, automatic } => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > collab_types::presence::MAX_STATUS_CHARS {
                    return Err(ActionCallError::InvalidRequest {
                        reason: "a status exceeds the presence bound".to_owned(),
                    });
                }
                Ok(PresencePayload::Status { text, automatic })
            }
            PresencePayload::Interaction(update) => {
                let sequence = {
                    let mut sequences = self.sequences.lock_recover();
                    let current = sequences.entry(update.invocation).or_insert(0);
                    *current = current.saturating_add(1);
                    let sequence = *current;
                    drop(sequences);
                    sequence
                };
                let update = update.normalized_by_broker(
                    tilo_actions::Acting::member(self.setup.member.clone()),
                    sequence,
                );
                update
                    .validate()
                    .map_err(|error| ActionCallError::InvalidRequest {
                        reason: format!("invalid interaction: {error}"),
                    })?;
                Ok(PresencePayload::Interaction(Box::new(update)))
            }
            other => Ok(other),
        }
    }
}

impl Drop for PresenceLane {
    fn drop(&mut self) {
        let keys: Vec<PresenceKey> = self.said.lock_recover().iter().cloned().collect();
        if !keys.is_empty() {
            self.setup.live.speaking.left_app(
                &self.setup.app,
                &self.setup.workspace,
                &self.setup.member,
                &keys,
            );
        }
    }
}

impl CallLanes for PresenceLane {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        match message {
            RemoteActionMessage::PresenceSaid { correlation, said } => {
                self.take(*correlation, said)
            }
            _ => Take::NotCall,
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "app_presence_tests.rs"]
mod tests;

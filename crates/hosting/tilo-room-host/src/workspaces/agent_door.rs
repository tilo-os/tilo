//! The agent door of the host's own person: the devices they run an agent on
//! (`tilo login`, then `tilo mcp`), and each session of one open now.
//!
//! A device says it runs an agent when its session opens
//! ([`tilo_relay_protocol::transport::Envelope::AgentRun`]), naming the agent
//! as History names its runs; the host keeps that only for a device of its own
//! person. Each of the device's sessions is one run, ended at once by
//! [`AgentDoor::stop`] while the device stays the person's, so the agent can
//! come back. A revoked device leaves the door ([`AgentDoor::forget`]); its
//! sessions end through the access audit that refuses it everywhere.
//!
//! The door is served as a live read
//! ([`tilo_actions::remote::agent_door::DOOR`]): [`AgentDoor::changes`]
//! moves whenever what it reads would read differently.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::agent_door::{Door, Grant, Reach, Run};
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;
use tokio::sync::{Notify, watch};

/// One session of an agent's device, open now.
struct OpenRun {
    started_ms: i64,
    /// Ends the session ([`AgentDoor::stop`]).
    stop: Arc<Notify>,
}

/// A device of the person's that runs an agent.
struct Agent {
    name: String,
    granted_ms: i64,
    last_ms: i64,
    runs: Vec<OpenRun>,
}

/// The agent door of the host's own person.
pub(crate) struct AgentDoor {
    agents: Mutex<HashMap<[u8; 32], Agent>>,
    change: watch::Sender<u64>,
}

impl Default for AgentDoor {
    fn default() -> Self {
        Self {
            agents: Mutex::default(),
            change: watch::Sender::new(0),
        }
    }
}

fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX)
}

impl AgentDoor {
    /// `cert`'s device, on the session `stop` ends, runs the agent `name`.
    pub(crate) fn note(&self, cert: &DeviceCert, name: &str, stop: &Arc<Notify>) {
        let now = now_ms();
        let mut agents = self.agents.lock_recover();
        let agent = agents
            .entry(cert.device_public_key)
            .or_insert_with(|| Agent {
                name: name.to_owned(),
                granted_ms: i64::try_from(cert.issued_at_ns / 1_000_000).unwrap_or(i64::MAX),
                last_ms: now,
                runs: Vec::new(),
            });
        name.clone_into(&mut agent.name);
        agent.last_ms = now;
        if !agent.runs.iter().any(|run| Arc::ptr_eq(&run.stop, stop)) {
            agent.runs.push(OpenRun {
                started_ms: now,
                stop: Arc::clone(stop),
            });
        }
        drop(agents);
        self.moved();
    }

    /// The session of `device` that `stop` ends has ended.
    pub(crate) fn ended(&self, device: &[u8; 32], stop: &Arc<Notify>) {
        let mut agents = self.agents.lock_recover();
        let Some(agent) = agents.get_mut(device) else {
            return;
        };
        let before = agent.runs.len();
        agent.runs.retain(|run| !Arc::ptr_eq(&run.stop, stop));
        let moved = agent.runs.len() != before;
        drop(agents);
        if moved {
            self.moved();
        }
    }

    /// `device` is no agent of the person's any longer: it was revoked.
    pub(crate) fn forget(&self, device: &[u8; 32]) {
        let gone = self.agents.lock_recover().remove(device);
        if let Some(agent) = gone {
            for run in agent.runs {
                run.stop.notify_one();
            }
            self.moved();
        }
    }

    /// Whether `device` runs an agent.
    pub(crate) fn is_agent(&self, device: &[u8; 32]) -> bool {
        self.agents.lock_recover().contains_key(device)
    }

    /// End the open runs named `run` of the agent `grant` names. Whether
    /// one was open.
    pub(crate) fn stop(&self, grant: &str, run: &str) -> bool {
        let Some(device) = grant_device(grant) else {
            return false;
        };
        let stops = self
            .agents
            .lock_recover()
            .get(&device)
            .filter(|agent| agent.name == run)
            .map(|agent| {
                agent
                    .runs
                    .iter()
                    .map(|open| Arc::clone(&open.stop))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for stop in &stops {
            stop.notify_one();
        }
        !stops.is_empty()
    }

    /// The door as the person reads it: every agent, the latest to act first.
    pub(crate) fn door(&self) -> Door {
        let agents = self.agents.lock_recover();
        let mut grants = agents
            .iter()
            .map(|(device, agent)| Grant {
                id: hex::encode(device),
                agent: agent.name.clone(),
                reach: Reach::Account,
                granted_ms: agent.granted_ms,
                last_ms: Some(agent.last_ms),
                runs: agent
                    .runs
                    .iter()
                    .map(|open| Run {
                        name: agent.name.clone(),
                        started_ms: open.started_ms,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        drop(agents);
        grants.sort_by(|a, b| b.last_ms.cmp(&a.last_ms).then_with(|| a.id.cmp(&b.id)));
        Door {
            grants,
            ..Door::default()
        }
    }

    /// Moves whenever [`Self::door`] may read differently.
    pub(crate) fn changes(&self) -> watch::Receiver<u64> {
        self.change.subscribe()
    }

    fn moved(&self) {
        self.change
            .send_modify(|count| *count = count.wrapping_add(1));
    }
}

impl super::Workspaces {
    /// The device of `cert`, on the session `stop` ends, runs the agent
    /// `name`: kept for a device of the host's own person only.
    pub(crate) fn note_agent(
        &self,
        cert: &DeviceCert,
        name: &str,
        stop: &Arc<tokio::sync::Notify>,
    ) {
        if cert.user_public_key == self.inner.host.cert.user_public_key
            && cert.device_public_key != self.inner.host.cert.device_public_key
        {
            self.inner.agent_door.note(cert, name, stop);
        }
    }

    /// The session of `device` that `stop` ends has ended.
    pub(crate) fn agent_session_ended(&self, device: &[u8; 32], stop: &Arc<tokio::sync::Notify>) {
        self.inner.agent_door.ended(device, stop);
    }
}

/// The device a grant id ([`Grant::id`]) names.
fn grant_device(grant: &str) -> Option<[u8; 32]> {
    hex::decode(grant).ok()?.try_into().ok()
}

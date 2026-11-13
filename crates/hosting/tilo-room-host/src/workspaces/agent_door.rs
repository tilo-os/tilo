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
use tilo_user_identity::{AgentScope, DeviceCert};
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
    /// What its key's certificate says of it.
    cert: DeviceCert,
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
                cert: cert.clone(),
            });
        name.clone_into(&mut agent.name);
        agent.last_ms = now;
        // The key it is set up with may be replaced by another of the
        // person's keys between sessions: what it may do is the certificate
        // it came in with now.
        agent.cert = cert.clone();
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
                reach: match agent.cert.capabilities.agent {
                    Some(AgentScope::Workspace(id)) => {
                        let id = AgentScope::Workspace(id).name();
                        Reach::Workspace {
                            name: id.clone(),
                            id,
                        }
                    }
                    Some(AgentScope::AllWorkspaces) | None => Reach::Account,
                },
                runs_on: agent.cert.device_label.clone(),
                expires_ms: agent
                    .cert
                    .expires_at_ns
                    .map(|ns| i64::try_from(ns / 1_000_000).unwrap_or(i64::MAX)),
                browser_pages: agent.cert.capabilities.browser_pages,
                script_key: agent.cert.capabilities.server_key,
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
    /// Whether `device` still runs the person's agent: a stop that leaves it so is the person
    /// stopping a run, and one that does not is the device being revoked.
    pub(crate) fn runs_agent(&self, device: &[u8; 32]) -> bool {
        self.inner.agent_door.is_agent(device)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{AgentScope, DeviceCapabilities};

    fn cert(capabilities: DeviceCapabilities) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: "Claude Code".to_owned(),
            capabilities,
            issued_at_ns: 1_000_000_000,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    fn grant_cert(key: u8, scope: AgentScope, browser_pages: bool, server_key: bool) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [key; 32],
            device_label: "Framework 16".into(),
            capabilities: DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
                agent: Some(scope),
                stand_in: false,
                server_key,
                browser_pages,
            },
            issued_at_ns: 5_000_000_000,
            expires_at_ns: Some(9_000_000_000),
            signature: [0; 64],
            issuer: None,
        }
    }

    /// The door says of each agent whether the key it came in with operates
    /// web pages in Browser, as that key's certificate carries it: a key
    /// without it is asked about every page action.
    #[test]
    fn an_agents_key_says_whether_it_operates_web_pages() {
        let door = AgentDoor::default();
        let stop = Arc::new(Notify::new());
        let operating = cert(
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces).with_browser_pages(true),
        );
        door.note(&operating, "Claude Code", &stop);
        let read = door.door();
        assert_eq!(read.grants.len(), 1);
        assert!(read.grants[0].browser_pages);

        let asked = cert(DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces));
        door.note(&asked, "Claude Code", &stop);
        assert!(!door.door().grants[0].browser_pages);
    }

    /// The door reads each grant as its key's certificate says it: where the
    /// agent runs, the one workspace it reaches, until when, whether it may
    /// operate web pages in Browser, and whether it is a script's key.
    #[test]
    fn a_grant_reads_as_its_certificate_says() {
        let door = AgentDoor::default();
        let workspace = [7; 16];
        let stop = Arc::new(Notify::new());
        door.note(
            &grant_cert(2, AgentScope::Workspace(workspace), true, false),
            "Claude Code #1",
            &stop,
        );
        door.note(
            &grant_cert(3, AgentScope::AllWorkspaces, false, true),
            "Nightly export #1",
            &Arc::new(Notify::new()),
        );
        let read = door.door();
        let agent = read
            .grants
            .iter()
            .find(|grant| grant.id == hex::encode([2u8; 32]))
            .expect("the agent's grant");
        let id = AgentScope::Workspace(workspace).name();
        assert_eq!(
            agent.reach,
            Reach::Workspace {
                id: id.clone(),
                name: id
            }
        );
        assert_eq!(agent.runs_on, "Framework 16");
        assert_eq!(agent.expires_ms, Some(9_000));
        assert!(agent.browser_pages);
        assert!(!agent.script_key);
        assert_eq!(agent.runs.len(), 1);
        let script = read
            .grants
            .iter()
            .find(|grant| grant.id == hex::encode([3u8; 32]))
            .expect("the script's key");
        assert_eq!(script.reach, Reach::Account);
        assert!(script.script_key);
        assert!(!script.browser_pages);
    }
}

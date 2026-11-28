//! The hosts an API server may forward a call to, and which one serves it.
//!
//! An API server keeps no workspace: which host holds the workspace, and which
//! of its holders can read it, is the workspace's. Every host that answers is
//! a candidate, and the API server picks among the ones that answer the grant
//! as the workspace's own devices pick among themselves
//! ([`neighbour_wire::placement`]): the measured round trip, then the
//! host's own key, so equal candidates always resolve the same way. A host
//! that does not answer is ranked as away.
//!
//! Answering is not holding: a server that keeps workspaces for the people it
//! lets in takes any certified device's session, also of a person it keeps
//! nothing for. So each one that answers is asked what it lists for the key,
//! and one that lists nothing is ranked as away while another lists a
//! workspace. When none lists any (a person's first workspace is still to be
//! made), every one that answered stays a candidate.
//!
//! When no host answers at all, nothing that can read the workspace is
//! online, and the caller gets the typed refusal
//! ([`crate::server::ServerError::NoHolder`]) rather than a call tried again
//! forever.

use crate::grant_key::GrantKey;
use crate::server::{Connection, Server, ServerError};
use neighbour_wire::placement::{self, Answer, Decline, Power, Rank, Work};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tilo_server_protocol::web::{PageHost, ServerInfo};

/// How long one host may take to answer with a session.
const ANSWERING: Duration = Duration::from_secs(12);

/// The hosts `value` names, each as the host writes it ([`ServerInfo::web_host_json`]): one
/// object, or a JSON array of them.
///
/// # Errors
/// `value` is not that, or a host in it names no https origin, which names the host.
pub fn read(value: &str) -> Result<Vec<ServerInfo>, String> {
    let value: serde_json::Value =
        serde_json::from_str(value).map_err(|error| error.to_string())?;
    let hosts: Vec<PageHost> = match value {
        serde_json::Value::Array(_) => serde_json::from_value(value),
        one => serde_json::from_value(one).map(|host| vec![host]),
    }
    .map_err(|error| format!("{error} (what the host writes in `web-host.json`)"))?;
    for host in &hosts {
        let names_itself = host.origin.as_deref().is_some_and(|origin| {
            url::Url::parse(origin).is_ok_and(|origin| tilo_net::secure_origin(&origin))
        });
        if !names_itself {
            return Err(
                "a host names no https origin (what the host writes in `web-host.json`)".into(),
            );
        }
    }
    Ok(hosts.into_iter().map(ServerInfo::from).collect())
}

/// The hosts an API server may forward to, in the order it was configured.
pub struct Hosts {
    relays: Vec<Arc<Server>>,
}

impl Hosts {
    /// The hosts each `info` names.
    #[must_use]
    pub fn new(infos: &[ServerInfo]) -> Self {
        Self {
            relays: infos
                .iter()
                .cloned()
                .map(|info| Arc::new(Server::new(info)))
                .collect(),
        }
    }

    /// The hosts' names, as hosts are listed to people.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.relays
            .iter()
            .map(|each| each.name().to_owned())
            .collect()
    }

    /// A session with the host that serves `key`'s workspace best right now:
    /// every host that answers and lists a workspace for the key is a
    /// candidate (every one that answers, while none lists any), and the one
    /// with the shortest round trip wins ([`placement::pick`]).
    ///
    /// # Errors
    /// No host answered: nothing that can read the workspace is online.
    pub async fn connect(&self, key: &GrantKey) -> Result<Connection, ServerError> {
        let mut answers = Vec::with_capacity(self.relays.len());
        let mut connected = Vec::with_capacity(self.relays.len());
        let mut keeping = Vec::new();
        for server in &self.relays {
            let began = Instant::now();
            let attempt = tokio::time::timeout(ANSWERING, server.connect(key)).await;
            let latency_ms = u32::try_from(began.elapsed().as_millis()).unwrap_or(u32::MAX);
            match attempt {
                Ok(Ok(connection)) => {
                    let device = device_of(server.name());
                    let lists =
                        tokio::time::timeout(ANSWERING, server.workspaces(&connection)).await;
                    if matches!(lists, Ok(Ok(listed)) if !listed.is_empty()) {
                        keeping.push(device);
                    }
                    answers.push((
                        device,
                        Answer::taking(
                            Rank {
                                power: Power::Unknown,
                                load_permille: 0,
                                latency_ms,
                            },
                            Vec::new(),
                        ),
                    ));
                    connected.push((device, connection));
                }
                Ok(Err(error)) => {
                    tracing::debug!(host = server.name(), %error, "a host did not answer a key");
                    answers.push((
                        device_of(server.name()),
                        Answer::declining(Decline::Offline),
                    ));
                }
                Err(_) => {
                    tracing::debug!(host = server.name(), "a host took too long to answer a key");
                    answers.push((
                        device_of(server.name()),
                        Answer::declining(Decline::Offline),
                    ));
                }
            }
        }
        let Some(worker) = choose(&among(answers, &keeping)) else {
            return Err(ServerError::NoHolder);
        };
        let (_, connection) = connected
            .into_iter()
            .find(|(device, _)| *device == worker)
            .ok_or(ServerError::NoHolder)?;
        Ok(connection)
    }
}

/// The host that serves a call among the ones that answered, as the
/// workspace's devices rank each other: the measured round trip first, then
/// the host's key. `None` when none answered.
fn choose(answers: &[([u8; 32], Answer)]) -> Option<[u8; 32]> {
    let work = Work {
        inputs: Vec::new(),
        input_bytes: 0,
        // Moving no input beats any run here, so every host that answered is
        // a candidate and rank alone chooses.
        local_wall_ms: Some(u64::MAX),
    };
    let choice = placement::pick(answers.iter().cloned(), &work);
    tracing::debug!(%choice, "an API server chose the host serving a key");
    choice.worker
}

/// The answers the choice is made among, `keeping` being the devices that list
/// a workspace for the key: while any does, one that answered and lists none
/// keeps nothing of the key's person and is ranked as away, however soon it
/// answered. While none does, every one that answered stays a candidate.
fn among(mut answers: Vec<([u8; 32], Answer)>, keeping: &[[u8; 32]]) -> Vec<([u8; 32], Answer)> {
    if !keeping.is_empty() {
        for (device, answer) in &mut answers {
            if !keeping.contains(device) {
                *answer = Answer::away();
            }
        }
    }
    answers
}

/// The placement rank key of the host named `name`: its origin's bytes, so
/// equal hosts always resolve the same way.
fn device_of(name: &str) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(name.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taking(latency_ms: u32) -> Answer {
        Answer::taking(
            Rank {
                power: Power::Unknown,
                load_permille: 0,
                latency_ms,
            },
            Vec::new(),
        )
    }

    /// A server that answers sooner and lists nothing for the key is passed
    /// over for the one that lists its workspace. While none lists any, the
    /// nearest serves, so a person's first workspace can be made.
    #[test]
    fn a_nearer_server_that_lists_nothing_for_the_key_is_passed_over() {
        let near = device_of("near");
        let far = device_of("far");
        let answered = || vec![(far, taking(40)), (near, taking(3))];
        assert_eq!(
            choose(&among(answered(), &[far])),
            Some(far),
            "the one that lists the key's workspace serves, though it is the longer way"
        );
        assert_eq!(
            choose(&among(answered(), &[near, far])),
            Some(near),
            "both list it: the shorter round trip wins"
        );
        assert_eq!(
            choose(&among(answered(), &[])),
            Some(near),
            "none lists any: every one that answered is a candidate"
        );
    }
}

//! The hosts an API server may forward a call to, and which one serves it.
//!
//! An API server keeps no workspace: which host holds the workspace, and which
//! of its holders can read it, is the workspace's. Every host that answers is
//! a candidate, and the API server picks among the ones that answer the grant
//! as the workspace's own devices pick among themselves
//! ([`tilo_peer_state::placement`]): the measured round trip, then the
//! host's own key, so equal candidates always resolve the same way. A host
//! that does not answer is ranked as away.
//!
//! When no host answers at all, nothing that can read the workspace is
//! online, and the caller gets the typed refusal
//! ([`crate::relay::RelayError::NoHolder`]) rather than a call tried again
//! forever.

use crate::grant_key::GrantKey;
use crate::relay::{Connection, Relay, RelayError};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tilo_peer_state::placement::{self, Answer, Decline, Power, Rank, Work};

/// How long one host may take to answer with a session.
const ANSWERING: Duration = Duration::from_secs(12);

/// The hosts an API server may forward to, in the order it was configured.
pub struct Hosts {
    relays: Vec<Arc<Relay>>,
}

impl Hosts {
    /// The hosts at `origins`, reached over `client`.
    #[must_use]
    pub fn new(client: &tilo_net::Client, origins: &[String]) -> Self {
        Self {
            relays: origins
                .iter()
                .filter(|origin| !origin.trim().is_empty())
                .map(|origin| Arc::new(Relay::new(client, origin)))
                .collect(),
        }
    }

    /// The hosts' names, as hosts are listed to people.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.relays
            .iter()
            .map(|relay| relay.name().to_owned())
            .collect()
    }

    /// A session with the host that serves `key`'s workspace best right now:
    /// every host that answers is a candidate, and the one with the shortest
    /// round trip wins ([`placement::pick`]).
    ///
    /// # Errors
    /// No host answered: nothing that can read the workspace is online.
    pub async fn connect(&self, key: &GrantKey) -> Result<Connection, RelayError> {
        let mut answers = Vec::with_capacity(self.relays.len());
        let mut connected = Vec::with_capacity(self.relays.len());
        for relay in &self.relays {
            let began = Instant::now();
            let attempt = tokio::time::timeout(ANSWERING, relay.connect(key)).await;
            let latency_ms = u32::try_from(began.elapsed().as_millis()).unwrap_or(u32::MAX);
            match attempt {
                Ok(Ok(connection)) => {
                    answers.push((
                        device_of(relay.name()),
                        Answer::taking(
                            Rank {
                                power: Power::Unknown,
                                load_permille: 0,
                                latency_ms,
                            },
                            Vec::new(),
                        ),
                    ));
                    connected.push((device_of(relay.name()), connection));
                }
                Ok(Err(error)) => {
                    tracing::debug!(host = relay.name(), %error, "a host did not answer a key");
                    answers.push((device_of(relay.name()), Answer::declining(Decline::Offline)));
                }
                Err(_) => {
                    tracing::debug!(host = relay.name(), "a host took too long to answer a key");
                    answers.push((device_of(relay.name()), Answer::declining(Decline::Offline)));
                }
            }
        }
        let Some(worker) = choose(&answers) else {
            return Err(RelayError::NoHolder);
        };
        let (_, connection) = connected
            .into_iter()
            .find(|(device, _)| *device == worker)
            .ok_or(RelayError::NoHolder)?;
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
        offload: None,
    };
    let choice = placement::pick(answers.iter().cloned(), &work);
    tracing::debug!(%choice, "an API server chose the host serving a key");
    choice.worker
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

    fn host(name: &str) -> [u8; 32] {
        device_of(name)
    }

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

    /// The nearest host serves; a host that went away costs nothing, and the
    /// one still there takes the call instead.
    #[test]
    fn the_nearest_host_that_answers_serves_the_call() {
        let near = host("a");
        let far = host("b");
        assert_eq!(
            choose(&[(far, taking(40)), (near, taking(3))]),
            Some(near),
            "the shorter round trip wins"
        );
        assert_eq!(
            choose(&[
                (near, Answer::declining(Decline::Offline)),
                (far, taking(40))
            ]),
            Some(far),
            "a host that went away passes the call to the one still there"
        );
        assert_eq!(
            choose(&[
                (near, Answer::declining(Decline::Offline)),
                (far, Answer::declining(Decline::Offline))
            ]),
            None,
            "nothing that can read the workspace answered"
        );
    }
}

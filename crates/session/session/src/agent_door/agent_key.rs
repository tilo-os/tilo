//! The agent key a person starts their session with (`tilo-session --agent-key FILE`): a session
//! started for an agent, headless or in a hosted workspace's machine, has no one at the door to
//! admit it, so the person decides ahead of time, by starting the session with a key, and their
//! root certifies it.
//!
//! The key is a secret in FILE, readable by this account only; the session makes one there when
//! FILE is empty or absent, and keeps the one there otherwise, so an agent configured with it
//! reaches the same session after a restart. The grant's own key is derived from that secret, so
//! whoever holds the file holds the grant, and the person's root certifies it as an agent's grant
//! ([`tilo_user_identity::DeviceCapabilities::agent_grant`]) once the session knows who the person
//! is. The certificate is kept with the person's devices (their fleet), where it is listed, and
//! ends as a device does: a revocation their root signs, from their device list or the agent's
//! card. A revoked key is never certified again; a new key is a new FILE.
//!
//! A program presenting the key at the door ([`tilo_runtime::session_door::KEY`]) is admitted as
//! the grant's agent without a question: every connection an instance of its own, attributed to the
//! grant, working in a run its admission starts, like a client the person admitted on the sheet.
//! A key the session does not hold, an expired or revoked grant, or one the person's root could not
//! certify here is refused, saying which.

use std::io;
use std::path::Path;

use ed25519_dalek::SigningKey;
use sha2::{Digest as _, Sha256};
use tilo_desktop_surface::DesktopHandle;
use tilo_user_identity::AgentScope;

/// What a key the session makes begins with, so it is recognised wherever it leaks.
const PREFIX: &str = "tilo_ak_";

/// What separates the grant key derived from an agent key from any other use of its bytes.
const GRANT_KEY_DOMAIN: &[u8] = b"tilo-agent-key-grant-v1\0";

/// How long a certified grant lasts: an agent's grant expires
/// ([`tilo_user_identity::MAX_AGENT_GRANT_LIFETIME_NS`] at most), and the session certifies it
/// again as it starts once this much of it is left.
const LIFETIME_NS: u64 = 30 * DAY_NS;
const RENEW_WITHIN_NS: u64 = 7 * DAY_NS;
const DAY_NS: u64 = 24 * 60 * 60 * 1_000_000_000;

/// Where the grant stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Standing {
    /// Not certified yet: the session has not learnt who its person is.
    Uncertified,
    /// Certified by the person's root, of the identity `identity`, until `expires_at_ns`.
    Certified {
        identity: String,
        expires_at_ns: u64,
    },
    /// The person revoked it.
    Revoked,
    /// The person's root could not certify it here, for this reason.
    Failed(String),
}

/// The session's agent key, and the grant certified for it.
pub(crate) struct AgentKey {
    /// The grant's public key, derived from the secret in the file.
    grant: [u8; 32],
    standing: Standing,
}

/// Why a client presenting a key is refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyRefusal {
    /// The session was started with no key, or with another one.
    NotThisSessions,
    Revoked,
    Expired,
    /// The grant is not certified, and why.
    Uncertified(String),
}

impl KeyRefusal {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotThisSessions => {
                "this session was not started with that agent key (tilo-session --agent-key)".into()
            }
            Self::Revoked => {
                "the person revoked this agent key; start the session with a new one".into()
            }
            Self::Expired => {
                "this agent key's grant expired; restart the session to have it certified again"
                    .into()
            }
            Self::Uncertified(why) => format!("this agent key's grant is not certified: {why}"),
        }
    }
}

impl super::Loop {
    /// Admit the client `facts` describes, which presented the agent key `presented` , as the key's
    /// agent, without asking the person: they decided by starting the session with it. A key this
    /// session does not hold, or whose grant does not admit it now, is refused, saying why.
    pub(super) fn admit_by_key(
        &mut self,
        handle: &DesktopHandle,
        request: super::Request,
        person: &str,
    ) {
        let super::Request {
            facts,
            workspace: asked,
            key,
            seat,
            decide,
            ..
        } = request;
        let presented = key.unwrap_or_default();
        let admitted = match self.agent_key.as_mut() {
            Some(key) => key.admits(&presented).map(|()| key.agent()),
            None => Err(KeyRefusal::NotThisSessions),
        };
        let agent = match admitted {
            Ok(agent) => agent,
            Err(refusal) => {
                tracing::info!(client = %facts.client, pid = facts.pid, ?refusal, "agent door: an agent key is refused");
                let _ = decide.send(super::Consent::Refuse(refusal.message()));
                return;
            }
        };
        let (workspace, name) =
            match super::where_it_works(handle, person, &facts, false, asked.as_deref()) {
                Ok(found) => found,
                Err(reason) => {
                    let _ = decide.send(super::Consent::Refuse(reason));
                    return;
                }
            };
        let (agent, shown) = self.agent_of(&agent, &facts);
        tracing::info!(client = %facts.client, pid = facts.pid, %agent, %workspace, "agent door: the person's agent key admits this client");
        self.seats
            .admitted(seat, &agent, &shown, (workspace.as_str(), name.as_str()));
        let _ = decide.send(super::Consent::Admit {
            workspace,
            agent,
            person: person.to_owned(),
            name: shown,
        });
    }
}

impl AgentKey {
    /// [`Self::open`], or `None` , saying why, for a key that cannot be kept: it admits no one,
    /// and the session still serves the door and asks.
    pub(crate) fn kept(file: &Path) -> Option<Self> {
        Self::open(file)
            .inspect_err(|error| {
                tracing::warn!(%error, file = %file.display(), "agent door: the agent key is not kept");
            })
            .ok()
    }

    /// The key in `file`, made there when the file is empty or absent.
    ///
    /// # Errors
    /// The file cannot be read or written, or holds something that is no key.
    pub(crate) fn open(file: &Path) -> io::Result<Self> {
        let held = match std::fs::read_to_string(file) {
            Ok(text) => Some(text.trim().to_owned()).filter(|key| !key.is_empty()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let key = if let Some(key) = held {
            key
        } else {
            let mut secret = [0_u8; 32];
            getrandom::fill(&mut secret).map_err(|error| io::Error::other(error.to_string()))?;
            let key = format!("{PREFIX}{}", hex::encode(secret));
            super::write_private(file, format!("{key}\n").as_bytes())?;
            key
        };
        if key.len() > 128 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} holds no agent key", file.display()),
            ));
        }
        Ok(Self {
            grant: grant_key(&key),
            standing: Standing::Uncertified,
        })
    }

    /// The agent the grant admits clients as, before their place and instance: named for its key,
    /// so what it does is attributed to this grant and no other.
    pub(crate) fn agent(&self) -> String {
        let short = hex::encode(self.grant);
        format!("agent-key-{}", short.get(..8).unwrap_or(&short))
    }

    /// Have the person `person` 's root certify the grant, unless it is certified for long enough
    /// already, revoked, or certifying failed: once per session start, when the session first
    /// knows who its person is.
    pub(crate) fn certify(&mut self, person: &str) {
        if self.standing != Standing::Uncertified {
            return;
        }
        self.standing = match certify(self.grant, person) {
            Ok(standing) => standing,
            Err(why) => {
                tracing::warn!(%why, "agent door: the agent key's grant is not certified");
                Standing::Failed(why)
            }
        };
        if let Standing::Certified { expires_at_ns, .. } = &self.standing {
            tracing::info!(
                agent = %self.agent(),
                expires_at_ns,
                "agent door: the agent key's grant is certified"
            );
        }
    }

    /// Whether `presented` is this session's key and its grant admits a client now.
    pub(crate) fn admits(&mut self, presented: &str) -> Result<(), KeyRefusal> {
        if grant_key(presented) != self.grant {
            return Err(KeyRefusal::NotThisSessions);
        }
        match &self.standing {
            Standing::Uncertified => Err(KeyRefusal::Uncertified(
                "the session has not certified it yet; try again shortly".into(),
            )),
            Standing::Failed(why) => Err(KeyRefusal::Uncertified(why.clone())),
            Standing::Revoked => Err(KeyRefusal::Revoked),
            Standing::Certified {
                identity,
                expires_at_ns,
            } => {
                // The person may have revoked it since, from their device list.
                if revoked(identity, &self.grant) {
                    self.standing = Standing::Revoked;
                    return Err(KeyRefusal::Revoked);
                }
                if now_ns() >= *expires_at_ns {
                    return Err(KeyRefusal::Expired);
                }
                Ok(())
            }
        }
    }

    /// Take the grant back: what the agent's card's Revoke does. Its root signs the revocation,
    /// which is kept with the person's devices, and no client of the key is admitted again.
    pub(crate) fn revoke(&mut self) {
        if let Standing::Certified { identity, .. } = &self.standing {
            let revoked = crate::identity_admin::fleet_store().and_then(|fleets| {
                fleets
                    .revoke(
                        identity,
                        self.grant,
                        tilo_user_identity::RevocationReason::Retired,
                        now_ns(),
                    )
                    .map_err(|error| error.to_string())
            });
            if let Err(why) = revoked {
                tracing::warn!(%why, "agent door: the agent key's revocation was not signed");
            }
        }
        self.standing = Standing::Revoked;
    }
}

/// The grant key the agent key `key` stands for.
fn grant_key(key: &str) -> [u8; 32] {
    let seed: [u8; 32] = Sha256::new()
        .chain_update(GRANT_KEY_DOMAIN)
        .chain_update(key.trim().as_bytes())
        .finalize()
        .into();
    SigningKey::from_bytes(&seed).verifying_key().to_bytes()
}

fn now_ns() -> u64 {
    tilo_clock::unix_now_ns()
}

/// Whether `identity` 's fleet holds a revocation of `grant`.
fn revoked(identity: &str, grant: &[u8; 32]) -> bool {
    crate::identity_admin::fleet_store()
        .ok()
        .and_then(|fleets| fleets.load(identity).ok())
        .is_some_and(|fleet| fleet.is_revoked(grant))
}

/// Certify `grant` as an agent's grant of the person `person` (their root key in hex), or find it
/// certified for long enough already, or revoked.
fn certify(grant: [u8; 32], person: &str) -> Result<Standing, String> {
    let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env()
        .and_then(|store| store.load_reconciled())
        .map_err(|error| format!("the person's identities could not be read: {error}"))?;
    let profile = registry
        .identities
        .values()
        // The session names its person by their root key in hex, which is their identity's id.
        .find(|profile| profile.id == person || profile.principal_id() == person)
        .cloned()
        .ok_or("this device does not hold the person's identity")?;
    let fleets = crate::identity_admin::fleet_store()?;
    let now = now_ns();
    if let Ok(fleet) = fleets.load(&profile.id) {
        if fleet.is_revoked(&grant) {
            return Ok(Standing::Revoked);
        }
        let lasting = fleet
            .certificates
            .iter()
            .filter(|cert| cert.device_public_key == grant)
            .filter_map(|cert| cert.expires_at_ns)
            .max()
            .filter(|expires| expires.saturating_sub(now) > RENEW_WITHIN_NS);
        if let Some(expires_at_ns) = lasting {
            return Ok(Standing::Certified {
                identity: profile.id,
                expires_at_ns,
            });
        }
    }
    let expires_at_ns = now.saturating_add(LIFETIME_NS);
    fleets
        .certify_agent_grant(
            &profile.id,
            grant,
            AgentScope::AllWorkspaces,
            "Agent key",
            now,
            expires_at_ns,
        )
        .map_err(|error| format!("the person's root did not certify it here: {error}"))?;
    Ok(Standing::Certified {
        identity: profile.id,
        expires_at_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_made_once_and_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.key");
        let made = AgentKey::open(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with(PREFIX), "{text}");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let again = AgentKey::open(&file).unwrap();
        assert_eq!(made.grant, again.grant, "the same file is the same grant");
        assert_eq!(made.agent(), again.agent());
    }

    #[test]
    fn only_the_sessions_own_key_is_admitted_and_only_once_certified() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent.key");
        let mut key = AgentKey::open(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            key.admits("tilo_ak_someone-elses"),
            Err(KeyRefusal::NotThisSessions)
        );
        assert!(matches!(
            key.admits(text.trim()),
            Err(KeyRefusal::Uncertified(_))
        ));
        key.standing = Standing::Failed("no root here".into());
        assert_eq!(
            key.admits(&text),
            Err(KeyRefusal::Uncertified("no root here".into()))
        );
        key.revoke();
        assert_eq!(key.admits(&text), Err(KeyRefusal::Revoked));
    }
}

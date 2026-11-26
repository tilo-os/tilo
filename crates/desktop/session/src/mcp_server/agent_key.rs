//! The agent key a person starts their session with (`tilo-session --agent-key FILE`): a session
//! started for an agent, headless or in a hosted workspace's machine, has no one at the API to
//! admit it, so the person decides ahead of time, by starting the session with a key, and their
//! root certifies it.
//!
//! The key is a secret in FILE, readable by this account only; the session makes one there when
//! FILE is empty or absent, and keeps the one there otherwise, so an agent configured with it
//! reaches the same session after a restart. The grant's own key is derived from that secret, so
//! whoever holds the file holds the grant, and the person's root certifies it as an agent's grant
//! ([`tilo_user_identity::DeviceCapabilities::agent_grant`]) once the session knows who the person
//! is. The certificate is kept among the person's keys in their own workspace, the one way every
//! key their root certifies is ([`tilo_workspace_service::workspaces::person_workspace::devices`]),
//! where it is listed as an agent's, and ends as a device does: their root revokes it and the
//! record marks it, from their device list or the agent's card. A revoked key is never certified
//! again; a new key is a new FILE.
//!
//! A program presenting the key at the API ([`tilo_mcp::session_api::KEY`]) is admitted as
//! the grant's agent without a question: every connection an instance of its own, attributed to the
//! grant, working in a run its admission starts, like a client the person admitted on the sheet.
//! A key the session does not hold, an expired or revoked grant, or one the person's root could not
//! certify here is refused, saying which.
//!
//! On a device that keeps no root of the person's, certifying waits for them at one that does:
//! the call waits there for their answer, and the session follows it until it settles, admitting
//! no client of the key meanwhile.

use std::io;
use std::path::Path;
use std::sync::mpsc;

use ed25519_dalek::SigningKey;
use sha2::{Digest as _, Sha256};
use tilo_session_core::desktop::state::DesktopHandle;

/// What a key the session makes begins with, so it is recognised wherever it leaks.
const PREFIX: &str = "tilo_ak_";

/// What separates the grant key derived from an agent key from any other use of its bytes.
const GRANT_KEY_DOMAIN: &[u8] = b"tilo-agent-key-grant-v1\0";

/// Where the grant stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GrantState {
    /// Not certified yet: the session has not learnt who its person is.
    Uncertified,
    /// Certified by the person's root until `expires_at_ns`.
    Certified { expires_at_ns: u64 },
    /// The person revoked it.
    Revoked,
    /// The person's root could not certify it here, for this reason.
    Failed(String),
}

/// The session's agent key, and the grant certified for it.
pub(crate) struct AgentKey {
    /// The grant's public key, derived from the secret in the file.
    grant: [u8; 32],
    state: GrantState,
    /// The person whose root certified it, once it did: whose revocations end it.
    person: Option<[u8; 32]>,
    /// The certifying under way, followed until it settles.
    certifying: Option<mpsc::Receiver<Certified>>,
}

/// How certifying the grant settled: where it stands, and the person whose root certified it.
type Certified = Result<(GrantState, Option<[u8; 32]>), String>;

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
        members: &super::Members<'_>,
    ) {
        let super::Request {
            facts,
            workspace: asked,
            key,
            session_name,
            session,
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
                tracing::info!(client = %facts.client, pid = facts.pid, ?refusal, "MCP server: an agent key is refused");
                let _ = decide.send(super::Consent::Refuse(refusal.message()));
                return;
            }
        };
        let (workspace, name) =
            match super::where_it_works(handle, person, &facts, asked.as_deref()) {
                Ok(found) => found,
                Err(reason) => {
                    let _ = decide.send(super::Consent::Refuse(reason));
                    return;
                }
            };
        tracing::info!(client = %facts.client, pid = facts.pid, %agent, %workspace, "MCP server: the person's agent key admits this client");
        let consent = super::as_member(
            members,
            &facts,
            (&workspace, person),
            session_name.as_deref(),
        );
        self.sessions.admit(&session, &consent, &agent, &name);
        let _ = decide.send(consent);
    }
}

impl AgentKey {
    /// [`Self::open`], or `None` , saying why, for a key that cannot be kept: it admits no one,
    /// and the session still serves the API and asks.
    pub(crate) fn kept(file: &Path) -> Option<Self> {
        Self::open(file)
            .inspect_err(|error| {
                tracing::warn!(%error, file = %file.display(), "MCP server: the agent key is not kept");
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
            tilo_files::native::atomic::write_private(file, format!("{key}\n").as_bytes())?;
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
            state: GrantState::Uncertified,
            person: None,
            certifying: None,
        })
    }

    /// The agent the grant admits clients as: named for its key,
    /// so what it does is attributed to this grant and no other.
    pub(crate) fn agent(&self) -> String {
        let short = hex::encode(self.grant);
        format!("agent-key-{}", short.get(..8).unwrap_or(&short))
    }

    /// Have the device certify the grant under the person's root, unless it is certified for long
    /// enough already, revoked, or certifying failed: once per session start, when the session
    /// first knows who its person is. It is followed apart from the session, which goes on
    /// meanwhile: on a device without the root it waits for the person's answer at one that has
    /// it.
    pub(crate) fn certify(&mut self) {
        if self.state != GrantState::Uncertified || self.certifying.is_some() {
            return;
        }
        let (settled, certifying) = mpsc::channel();
        let grant = self.grant;
        let started = std::thread::Builder::new()
            .name("agent-key-certify".into())
            .spawn(move || {
                let _ = settled.send(certify(grant));
            });
        match started {
            Ok(_) => self.certifying = Some(certifying),
            Err(error) => self.settle(Err(error.to_string())),
        }
    }

    /// Take how certifying settled, once it has.
    fn certified(&mut self) {
        let Some(certifying) = &self.certifying else {
            return;
        };
        let settled = match certifying.try_recv() {
            Ok(settled) => settled,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("certifying it ended without an answer".to_owned())
            }
        };
        self.certifying = None;
        self.settle(settled);
    }

    fn settle(&mut self, settled: Certified) {
        match settled {
            Ok((state, person)) => {
                self.state = state;
                self.person = person;
            }
            Err(why) => {
                tracing::warn!(%why, "MCP server: the agent key's grant is not certified");
                self.state = GrantState::Failed(why);
            }
        }
        if let GrantState::Certified { expires_at_ns } = &self.state {
            tracing::info!(
                agent = %self.agent(),
                expires_at_ns,
                "MCP server: the agent key's grant is certified"
            );
        }
    }

    /// Whether `presented` is this session's key and its grant admits a client now.
    pub(crate) fn admits(&mut self, presented: &str) -> Result<(), KeyRefusal> {
        if grant_key(presented) != self.grant {
            return Err(KeyRefusal::NotThisSessions);
        }
        self.certified();
        match &self.state {
            GrantState::Uncertified if self.certifying.is_some() => Err(KeyRefusal::Uncertified(
                "it is being certified; where this device does not keep your identity, it waits \
                 for your answer on one that does"
                    .into(),
            )),
            GrantState::Uncertified => Err(KeyRefusal::Uncertified(
                "the session has not certified it yet; try again shortly".into(),
            )),
            GrantState::Failed(why) => Err(KeyRefusal::Uncertified(why.clone())),
            GrantState::Revoked => Err(KeyRefusal::Revoked),
            GrantState::Certified { expires_at_ns } => {
                // The person may have revoked it since, from their device list: their
                // revocations, as this device reads them, say.
                if let Some(person) = &self.person
                    && let Ok(keys) = tilo_app_host::agents::agent_keys::this_device()
                    && matches!(keys.revocation(person, &self.grant), Ok(Some(_)))
                {
                    self.state = GrantState::Revoked;
                    return Err(KeyRefusal::Revoked);
                }
                if now_ns() >= *expires_at_ns {
                    return Err(KeyRefusal::Expired);
                }
                Ok(())
            }
        }
    }

    /// Take the grant back: what the agent's card's Revoke does. No client of the key is admitted
    /// here again, and the grant's key, in hex, is answered where it was certified: the person's
    /// keys revoke it ([`tilo_actions::remote::device_tickets::REVOKE`]), as they revoke any.
    pub(crate) fn revoke(&mut self) -> Option<String> {
        let certified = matches!(self.state, GrantState::Certified { .. });
        self.state = GrantState::Revoked;
        certified.then(|| hex::encode(self.grant))
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

/// Have this computer's daemon certify `grant` as an agent's grant of its person, with the root
/// it keeps (`devices#certifyKey`), or find it certified for long enough already, or revoked:
/// on a device that keeps none, once the person answered at one that does. Where it stands, and
/// the person whose root certified it.
fn certify(grant: [u8; 32]) -> Certified {
    let answer = tilo_api::daemon::this_computer()?.act_followed(
        tilo_device_identity::login::CERTIFY_KEY,
        serde_json::json!({ "key": hex::encode(grant) }),
    )?;
    if answer.get("revoked").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok((GrantState::Revoked, None));
    }
    // A certificate the daemon answers that does not read is its fault, not one to pass over.
    let cert = answer
        .get("cert")
        .and_then(serde_json::Value::as_str)
        .map(|cert| {
            let bytes = hex::decode(cert).map_err(|err| {
                format!("this computer's daemon answered a certificate that is not hex: {err}")
            })?;
            tilo_user_identity::canonical::from_canonical_bytes::<tilo_user_identity::DeviceCert>(
                &bytes,
            )
            .map_err(|err| {
                format!("this computer's daemon answered a certificate that does not read: {err}")
            })
        })
        .transpose()?;
    // Every device of the person's lists it among their keys, as an agent's.
    if let Some(cert) = &cert
        && let Ok(keys) = tilo_app_host::agents::agent_keys::this_device()
    {
        keys.keep_key(cert);
    }
    Ok((
        GrantState::Certified {
            expires_at_ns: answer
                .get("expires_at_ns")
                .and_then(serde_json::Value::as_u64)
                .ok_or("this computer's daemon named no time the key's certificate ends")?,
        },
        cert.map(|cert| cert.user_public_key),
    ))
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
        key.state = GrantState::Failed("no root here".into());
        assert_eq!(
            key.admits(&text),
            Err(KeyRefusal::Uncertified("no root here".into()))
        );
        assert_eq!(
            key.revoke(),
            None,
            "a grant never certified is no key to revoke"
        );
        assert_eq!(key.admits(&text), Err(KeyRefusal::Revoked));
    }
}

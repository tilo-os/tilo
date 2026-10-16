//! Delegated agents as governed members of a room.
//!
//! An agent is a device of its person: its own key, a certificate the
//! person's root signed, and a root-signed [`Delegation`] naming the
//! device an agent, how it is shown, the rooms and apps it may act in and
//! until when. The room's governor admits it as a governed identity that
//! carries that delegation ([`PeerGovernanceMut::admit_agent`](crate::PeerGovernanceMut::admit_agent)), so every member
//! knows the device, the person it acts for, and its window: its changes
//! are signed by its own key and attributed to it, never to the device that
//! launched it.
//!
//! The rule every member applies, in one place ([`GovernedDelegateRow::acting`]):
//! an agent identity acts only while its delegation verifies against the
//! certificate's own root, names that device and this room, is inside its
//! validity window, and the governor has not withdrawn it. It acts with its
//! person's grants bounded by its delegation
//! ([`tilo_action_policy::roles::agent_delegation`]): an agent writes where
//! its person may write and governs nothing. Outside its window the
//! identity stays on the roster, so the history it wrote stays attributed,
//! but it is no reader of the room's channels and no op of it is admitted.

use super::facades::{PeerGovernance, PeerGovernanceMut};
use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::agent_policy;
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::delegation::{Delegate, Delegation};

use super::{GovernedIdentity, Role, SigningKey, SystemTime, err, now_ns, room};

/// What a governed identity carries when its device is a delegate of its
/// person: an agent, or an installed bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedDelegateRow {
    /// The person's root-signed delegation naming this device.
    pub delegation: Delegation,
    /// When the governor withdrew it; it acts on nothing from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_at_ns: Option<u64>,
    /// For a bot, its installation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<crate::installation::Installation>,
}

/// Why an agent identity does not act at a given time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DelegateInactive {
    /// The governor withdrew it.
    Withdrawn,
    /// The delegation is not the certificate root's, or is outside its
    /// validity window.
    Delegation,
    /// The delegation names another device.
    OtherDevice,
    /// The delegation does not name this room.
    OtherRoom,
    /// A bot row without its installation, an installation that is not the
    /// delegation's, or an agent row carrying one.
    Mismatch,
    /// The installation names another installer than the delegation's root.
    InstallerMismatch,
}

impl std::fmt::Display for DelegateInactive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Withdrawn => "the delegate was withdrawn",
            Self::Delegation => "the delegate's delegation is not current",
            Self::OtherDevice => "the delegate's delegation names another device",
            Self::OtherRoom => "the delegate is not delegated this room",
            Self::Mismatch => "the delegation does not match its installation",
            Self::InstallerMismatch => "the bot's installer did not sign its delegation",
        })
    }
}

impl GovernedDelegateRow {
    /// Whether the agent whose certificate is `cert` acts in `room` at
    /// `now_ns`: the one rule (see the module documentation).
    pub(crate) fn acting(
        &self,
        cert: &DeviceCert,
        room: &str,
        now_ns: u64,
    ) -> Result<(), DelegateInactive> {
        if self.withdrawn_at_ns.is_some_and(|at| at <= now_ns) {
            return Err(DelegateInactive::Withdrawn);
        }
        let delegation = &self.delegation;
        if delegation.device_public_key != cert.device_public_key {
            return Err(DelegateInactive::OtherDevice);
        }
        delegation
            .verify(&cert.user_public_key, now_ns)
            .map_err(|_| DelegateInactive::Delegation)?;
        let workspace = tilo_workspace_join::room_workspace_id(room)
            .map_err(|_| DelegateInactive::OtherRoom)?;
        if !delegation.permits_workspace(workspace) {
            return Err(DelegateInactive::OtherRoom);
        }
        match (&delegation.delegate, &self.installation) {
            (Delegate::Agent { .. }, None) => Ok(()),
            (Delegate::Bot { installation }, Some(row)) if row.id.as_str() == installation => {
                if row.installed_by != hex::encode(delegation.user_public_key) {
                    return Err(DelegateInactive::InstallerMismatch);
                }
                Ok(())
            }
            _ => Err(DelegateInactive::Mismatch),
        }
    }
}

impl GovernedIdentity {
    /// Whether this identity may act in `room` at `now_ns` as far as its
    /// delegation goes. Every identity that is no delegate passes.
    pub(crate) fn delegate_acting(&self, room: &str, now_ns: u64) -> Result<(), DelegateInactive> {
        self.delegate
            .as_ref()
            .map_or(Ok(()), |row| row.acting(&self.device_cert, room, now_ns))
    }
}

/// The role an agent acts with when its person holds `role`: its person's,
/// within what its delegation grants (the agent cap), so it never governs.
pub(crate) fn agent_role(role: Role) -> Role {
    agent_policy::role(role, agent_policy::ROLE_CAP)
}

/// One delegated agent on a room's roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernedAgent {
    /// The agent's own device key, which signs everything it writes.
    pub device_public_key: [u8; 32],
    /// The person it acts for: the root that signed its certificate and
    /// its delegation.
    pub user_public_key: [u8; 32],
    /// The delegation's stable agent id.
    pub agent_id: String,
    /// Its display name.
    pub label: String,
    /// Its cursor colour, sRGB.
    pub color: [u8; 3],
    /// When its delegation ends, ns since the epoch.
    pub expires_at_ns: u64,
    /// When the governor withdrew it, if it did.
    pub withdrawn_at_ns: Option<u64>,
    /// Whether it acts in the room at the time the roster was read.
    pub acting: bool,
}

/// Capabilities an agent's certificate never carries.
pub(crate) const fn holds_no_power(cert: &DeviceCert) -> bool {
    let capabilities = &cert.capabilities;
    !(capabilities.can_own_workspaces
        || capabilities.can_admit_devices
        || capabilities.can_act_as_host
        || capabilities.can_read_all_workspaces
        || capabilities.can_administer_devices)
}

impl PeerGovernance<'_> {
    /// The certificate and delegation of agent `device` as this room's
    /// governance records them, and when it was withdrawn: what a governor
    /// decides an agent's place in the room from again.
    pub fn governed_agent(
        self,
        device: &[u8; 32],
    ) -> Option<(DeviceCert, Delegation, Option<u64>)> {
        let doc = self.peer.core.room_doc.as_ref()?;
        let bytes = doc.policy().identities.get(&hex::encode(device))?.clone();
        let identity: GovernedIdentity = serde_json::from_slice(&bytes).ok()?;
        let agent = identity.delegate?;
        agent.delegation.delegate.agent_id()?;
        Some((
            identity.device_cert,
            agent.delegation,
            agent.withdrawn_at_ns,
        ))
    }
    /// Every delegated agent on this room's roster, acting or not at `now`.
    ///
    /// # Errors
    ///
    /// Returns an error if room governance is missing or malformed.
    pub fn governed_agents(self, now: SystemTime) -> Result<Vec<GovernedAgent>, String> {
        let doc = self
            .peer
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut agents = Vec::new();
        for bytes in doc.policy().identities.values() {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            let acting = identity
                .delegate_acting(&self.peer.core.room, now_ns(now))
                .is_ok();
            let Some(agent) = identity.delegate else {
                continue;
            };
            let Some(agent_id) = agent.delegation.delegate.agent_id().map(str::to_owned) else {
                continue;
            };
            agents.push(GovernedAgent {
                device_public_key: identity.device_cert.device_public_key,
                user_public_key: identity.device_cert.user_public_key,
                agent_id,
                label: agent.delegation.label,
                color: agent.delegation.color,
                expires_at_ns: agent.delegation.expires_at_ns,
                withdrawn_at_ns: agent.withdrawn_at_ns,
                acting,
            });
        }
        agents.sort_by_key(|agent| agent.device_public_key);
        Ok(agents)
    }
}
impl PeerGovernanceMut<'_> {
    /// Admit the certified agent device `cert` to this room as a delegated
    /// agent of its person, bound to `delegation`. Only the pinned room
    /// authority may, and only an agent of the authority's own person: one
    /// whose certificate holds no capability and whose delegation verifies
    /// against that root, names this device and this room, and is current.
    /// A newer delegation of the same agent replaces the one recorded; a
    /// withdrawn agent is never admitted again, and a person's ordinary
    /// device never becomes an agent (nor an agent an ordinary device).
    ///
    /// Returns whether governance changed.
    ///
    /// # Errors
    ///
    /// Returns why the agent may not be admitted, or why governance could
    /// not be updated.
    pub fn admit_agent(
        self,
        cert: DeviceCert,
        delegation: Delegation,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        if delegation.delegate.agent_id().is_none() {
            return Err("the delegation is not an agent's".into());
        }
        self.peer.require_governance_writer(key, now)?;
        if !agent_policy::OTHER_PEOPLES_AGENTS_JOIN
            && cert.user_public_key != self.peer.core.authority.user_public_key
        {
            return Err("only the room owner's agents are admitted".into());
        }
        if !holds_no_power(&cert) {
            return Err("an agent's certificate holds no capability".into());
        }
        tilo_user_identity::verify_device_cert_at(&cert, now_ns(now)).map_err(err)?;
        if self
            .peer
            .core
            .revocations
            .is_revoked(&cert.device_public_key)
        {
            return Err("device revoked".into());
        }
        if let Some(old) = self.peer.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != &cert
        {
            return Err("certificate conflict".into());
        }
        let row = GovernedDelegateRow {
            delegation,
            withdrawn_at_ns: None,
            installation: None,
        };
        row.acting(&cert, &self.peer.core.room, now_ns(now))
            .map_err(|reason| reason.to_string())?;
        let doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let id = hex::encode(cert.device_public_key);
        let prior = room::governance_of(Some(&doc))
            .identities
            .get(&id)
            .map(|bytes| serde_json::from_slice::<GovernedIdentity>(bytes))
            .transpose()
            .map_err(err)?;
        let identity = match prior {
            None => GovernedIdentity {
                device_cert: cert,
                revocations: vec![],
                admission: None,
                admissions: vec![],
                workspace_delegations: vec![],
                delegate: Some(row),
            },
            Some(prior) if prior.device_cert != cert => {
                return Err("certificate conflict".into());
            }
            Some(GovernedIdentity { delegate: None, .. }) => {
                return Err("the device is a member, not an agent".into());
            }
            Some(GovernedIdentity {
                delegate: Some(ref known),
                ..
            }) if known.installation.is_some() => {
                return Err("the device is a bot, not an agent".into());
            }
            Some(GovernedIdentity {
                delegate:
                    Some(GovernedDelegateRow {
                        withdrawn_at_ns: Some(_),
                        ..
                    }),
                ..
            }) if agent_policy::WITHDRAWAL_IS_FINAL => {
                return Err("the agent was withdrawn".into());
            }
            Some(GovernedIdentity {
                delegate: Some(ref known),
                ..
            }) if known.delegation.issued_at_ns >= row.delegation.issued_at_ns => {
                return Ok(false);
            }
            Some(mut prior) => {
                prior.delegate = Some(row);
                prior
            }
        };
        self.peer.put_identity(doc, &identity, key, now)
    }
    /// Withdraw the agent `device` from this room as the pinned authority
    /// `key`: from `now` on it reads and writes nothing here, and its
    /// earlier changes stay attributed to it. Returns whether governance
    /// changed: `false` for a device that is not on the roster, or was
    /// already withdrawn.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governance writer, the device is
    /// a member rather than an agent, or governance cannot be updated.
    pub fn withdraw_agent(
        self,
        device: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_governance_writer(key, now)?;
        let doc = self
            .peer
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let Some(bytes) = room::governance_of(Some(&doc))
            .identities
            .get(&hex::encode(device))
            .cloned()
        else {
            return Ok(false);
        };
        let mut identity: GovernedIdentity = serde_json::from_slice(&bytes).map_err(err)?;
        let Some(agent) = identity.delegate.as_mut() else {
            return Err("the device is a member, not an agent".into());
        };
        if agent.installation.is_some() {
            return Err("the device is a bot, not an agent".into());
        }
        if agent.withdrawn_at_ns.is_some() {
            return Ok(false);
        }
        agent.withdrawn_at_ns = Some(now_ns(now));
        self.peer.put_identity(doc, &identity, key, now)
    }
}

#[cfg(test)]
#[path = "delegates_tests.rs"]
mod tests;

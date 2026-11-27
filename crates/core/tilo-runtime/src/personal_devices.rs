//! The person's devices as their own workspace says them, for a key's
//! sessions with this device's host, which admit another of their devices.
//!
//! What a device is to its person (its settings, its person's account and identities, the
//! devices it lets in, the agent sessions it serves) is reached by the person's own devices
//! alone ([`own_devices_only`]). A workspace the device holds is the workspace's: another
//! member's device asks its actions as that member, and the workspace's grants for them decide
//! each call, never this device's person's.

use std::time::SystemTime;

use tilo_actions::remote::{DEVICE_SETTINGS, RemoteActionMessage};
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::workspaces::Workspaces;

/// Whose a device that this device's host admitted is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Whose {
    /// One of the person's own devices.
    Own,
    /// A device of someone else, which the workspace it asks in decides for as its member.
    Member,
}

/// Why a device is not taken as one of the person's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The certificate it showed names another device than the one that
    /// connected.
    NotTheDialer,
    /// Someone else's device asked for what is this device's own.
    OtherPerson,
    /// Its certificate does not hold now.
    NotCertified(String),
    /// The person removed it from their devices.
    Revoked,
    /// A key made for an agent asked for a workspace its scope does not
    /// reach, or named none.
    OutOfScope,
    /// A device a host placed for records of a workspace: it reaches those records alone and is
    /// none of its person's devices.
    Placed,
}

impl Refusal {
    /// The reason the dialing side is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotTheDialer => {
                "the certificate shown names another device than the one that connected".to_owned()
            }
            Self::OtherPerson => {
                "that is this device's own (its settings, its person's account, the devices it \
                 lets in); only its person's own devices reach it"
                    .to_owned()
            }
            Self::NotCertified(why) => format!("that device's certificate does not hold: {why}"),
            Self::Revoked => {
                "that device was removed from its person's devices; it no longer reaches them"
                    .to_owned()
            }
            Self::OutOfScope => {
                "this key reaches only the workspaces it was made for: name one of them".to_owned()
            }
            Self::Placed => {
                "that device was placed for records of a workspace; it reaches those records alone"
                    .to_owned()
            }
        }
    }
}

/// The person's devices: every device their identity root certified, less the
/// ones their root's revocation in the person's devices record refuses.
#[derive(Clone)]
pub struct PersonalDevices {
    workspaces: Workspaces,
    /// The person's identity root.
    root: [u8; 32],
}

impl PersonalDevices {
    /// The devices of the person `this`, this device's own certificate, names, with the
    /// revocations the record of their devices in `workspaces`, this device's own service, holds.
    #[must_use]
    pub fn new(workspaces: Workspaces, this: &DeviceCert) -> Self {
        Self {
            workspaces,
            root: this.user_public_key,
        }
    }

    /// Whose `certificate`, shown on a connection the transport proved is from `dialer`, is
    /// now: one of the person's devices, or a device of someone else, which only the workspace
    /// it asks in admits.
    pub fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<Whose, Refusal> {
        if certificate.device_public_key != dialer {
            return Err(Refusal::NotTheDialer);
        }
        if certificate.capabilities.records.is_some() {
            return Err(Refusal::Placed);
        }
        tilo_user_identity::verify_device_cert_at(certificate, now_ns(SystemTime::now()))
            .map_err(|error| Refusal::NotCertified(error.to_string()))?;
        // Another person's revocations are their root's, which the workspace's roster carries:
        // the workspace decides for their device.
        if certificate.user_public_key != self.root {
            return Ok(Whose::Member);
        }
        // A grant a revoked device minted ends with it.
        if self.workspaces.blocked_keys().revokes(certificate) {
            return Err(Refusal::Revoked);
        }
        Ok(Whose::Own)
    }

    /// The revocations the record of the person's devices holds, as a verifier takes them: each
    /// entry the root signed, checked into a list as it is read.
    pub(crate) fn revocations(&self) -> RevocationList {
        let mut list = RevocationList::new();
        for entry in self.workspaces.blocked_keys().revocations() {
            let _ = list.insert(entry.clone());
        }
        list
    }
}

/// Whether `message` asks for what is this device's own, which its person's own devices alone
/// reach: the device's actions (its settings, `devices#issue` and the rest of what lets a device
/// in, its person's account and identities, its updates), the agent sessions it serves, and its
/// records (`devices/<id>/…`, its settings in any workspace).
#[must_use]
pub fn own_devices_only(message: &RemoteActionMessage) -> bool {
    let own_provider = |provider: &str| {
        provider == tilo_device_identity::login::PROVIDER
            || provider == tilo_action_services::mcp_server::PROVIDER
    };
    match message {
        RemoteActionMessage::Invoke(request) => own_provider(&request.target.provider),
        RemoteActionMessage::CatalogueRequest { query, .. } => {
            query.provider.as_deref().is_some_and(own_provider)
        }
        RemoteActionMessage::ResourceRead { read, .. } => {
            read.app == DEVICE_SETTINGS || read.workspace.starts_with("devices/")
        }
        _ => false,
    }
}

fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}

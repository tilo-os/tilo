//! The registries a workspace fetches apps from: their names, their addresses,
//! the owner key each is pinned to, whether it is read, and which channel of
//! its releases it offers.
//!
//! The list travels in the map of the workspace tree every member reads: a
//! registry by name. A change to it is a member's change,
//! admitted by the workspace's install grant. What a registry offers is no part
//! of the tree: a device fetches the signed catalogue itself and checks it
//! against its own rollback checkpoint.

use automerge::{ROOT, ReadDoc, transaction::Transactable};
use collab_types::signer::DeviceSigner;
use tilo_action_policy::roles::governance;
use tilo_user_identity::DeviceCert;

use super::facades::PeerGovernance;
use super::package_trust::valid_pubkey;
use super::{StatePeer, SystemTime, err, governed_map, person_of};
use crate::workspace_membership::WorkspaceMembershipCommand;

/// The map of the registries, by name.
pub(super) const REGISTRIES: &str = "package_registries_v1";

/// The channels a registry offers, as its row names one.
pub const CHANNELS: [&str; 2] = ["stable", "development"];

/// A registry the workspace fetches apps from, as its list keeps it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageRegistry {
    /// The short name every offer from it is labelled with.
    pub name: String,
    /// Its address: credential-free HTTPS, or loopback HTTP for development.
    pub base_url: String,
    /// The owner key its signed catalogue is pinned to, hex. Never empty:
    /// discovery that creates trust is no discovery.
    pub owner_pubkey_hex: String,
    /// Whether the workspace reads it.
    pub enabled: bool,
    /// `stable` or `development` (`[`CHANNELS`]`).
    pub channel: String,
}

impl StatePeer {
    /// Apply a registry command; `None` for every other command.
    pub(super) fn apply_registry_command(
        &mut self,
        caller: &DeviceCert,
        command: &WorkspaceMembershipCommand,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Option<Result<bool, String>> {
        match command {
            WorkspaceMembershipCommand::AddRegistry(registry) => {
                Some(self.apply_add_registry(caller, registry, key, now))
            }
            WorkspaceMembershipCommand::RemoveRegistry { name } => {
                Some(self.apply_remove_registry(caller, name, key, now))
            }
            WorkspaceMembershipCommand::SetRegistryEnabled { name, enabled } => {
                Some(self.apply_set_registry(caller, name, Some(*enabled), None, key, now))
            }
            WorkspaceMembershipCommand::SetRegistryChannel { name, channel } => {
                Some(self.apply_set_registry(caller, name, None, Some(channel), key, now))
            }
            _ => None,
        }
    }

    fn apply_add_registry(
        &mut self,
        caller: &DeviceCert,
        registry: &PackageRegistry,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        if self
            .governance()
            .package_registries()
            .iter()
            .any(|kept| kept == registry)
        {
            return Ok(false);
        }
        let bytes = serde_json::to_vec(registry).map_err(err)?;
        self.put_registry_row(&registry.name, Some(bytes), key, now)
    }

    fn apply_remove_registry(
        &mut self,
        caller: &DeviceCert,
        name: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        if !self
            .governance()
            .package_registries()
            .iter()
            .any(|kept| kept.name == name)
        {
            return Ok(false);
        }
        self.put_registry_row(name, None, key, now)
    }

    /// Enable, disable or move a registry to another channel: one change, on
    /// the row the workspace already keeps.
    fn apply_set_registry(
        &mut self,
        caller: &DeviceCert,
        name: &str,
        enabled: Option<bool>,
        channel: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_install_grant(caller, now)?;
        let Some(mut row) = self
            .governance()
            .package_registries()
            .into_iter()
            .find(|kept| kept.name == name)
        else {
            return Ok(false);
        };
        if let Some(enabled) = enabled {
            row.enabled = enabled;
        }
        if let Some(channel) = channel {
            row.channel = channel.to_owned();
        }
        let bytes = serde_json::to_vec(&row).map_err(err)?;
        self.put_registry_row(name, Some(bytes), key, now)
    }

    /// The person behind `caller`, when the workspace lets them choose its
    /// registries.
    fn check_install_grant(&self, caller: &DeviceCert, now: SystemTime) -> Result<String, String> {
        let member = self.membership(caller.device_public_key, now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                governance::INSTALL_APP,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(person_of(&member)?.to_owned())
    }

    /// Write or remove the registry `name` in one commit.
    fn put_registry_row(
        &mut self,
        name: &str,
        bytes: Option<Vec<u8>>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .core
            .workspace_doc
            .clone()
            .ok_or("the workspace tree is missing")?;
        let before = doc.get_heads();
        doc.set_actor(self.workspace_actor_of(key));
        let rows = governed_map(&doc, REGISTRIES)?;
        match bytes {
            Some(bytes) => doc
                .put(&rows, name, automerge::ScalarValue::Bytes(bytes))
                .map_err(err)?,
            None => doc.delete(&rows, name).map_err(err)?,
        }
        self.write_governance(doc, &before, key, now)
    }
}

impl PeerGovernance<'_> {
    /// Every registry the workspace fetches apps from, by name.
    #[must_use]
    pub fn package_registries(self) -> Vec<PackageRegistry> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Vec::new();
        };
        let Ok(Some((_, rows))) = doc.get(ROOT, REGISTRIES) else {
            return Vec::new();
        };
        let mut out: Vec<PackageRegistry> = doc
            .keys(&rows)
            .filter_map(|name| read_registry(&**doc, &rows, &name).filter(|row| row.name == name))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// The registry kept under `name`, if it reads as a valid row of that name.
fn read_registry(
    doc: &impl ReadDoc,
    rows: &automerge::ObjId,
    name: &str,
) -> Option<PackageRegistry> {
    match doc.get(rows, name).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => tilo_failure::ignored!(
                "a registry row that does not decode names no registry",
                serde_json::from_slice::<PackageRegistry>(bytes)
                    .ok()
                    .filter(valid_registry)
            ),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `registry` is a row a workspace may keep: a name, an address, a
/// pinned key and a channel it knows.
#[must_use]
pub(crate) fn valid_registry(registry: &PackageRegistry) -> bool {
    let name = registry.name.trim();
    !name.is_empty()
        && name.len() <= 128
        && !name.chars().any(char::is_control)
        && registry.name == name
        && valid_base_url(&registry.base_url)
        && valid_pubkey(&registry.owner_pubkey_hex)
        && CHANNELS.contains(&registry.channel.as_str())
}

/// Whether `base` is a credential-free HTTPS address, or loopback HTTP for
/// development, with no query or fragment.
#[must_use]
pub(crate) fn valid_base_url(base: &str) -> bool {
    if base.trim() != base || base.len() > 2048 || base.chars().any(char::is_control) {
        return false;
    }
    if base.contains('@') || base.contains('?') || base.contains('#') {
        return false;
    }
    let (rest, loopback_allowed) = if let Some(rest) = base.strip_prefix("https://") {
        (rest, false)
    } else if let Some(rest) = base.strip_prefix("http://") {
        (rest, true)
    } else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains(char::is_whitespace) {
        return false;
    }
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return false;
    }
    !loopback_allowed || is_loopback(host)
}

/// Whether `host` is a loopback address, written as one.
fn is_loopback(host: &str) -> bool {
    host == "::1"
        || host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback())
}

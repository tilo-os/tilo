//! An installed bot as a governed member of its room, kept current.
//!
//! The room's governance records each bot: its device, its installer's
//! delegation to that device, its grants and its program, so every member
//! can read what runs and enforce what it may do. A delegation is issued
//! for at most 30 days. In a hosted room whose owner root this host holds
//! nobody else is there to renew it, so the host does: it signs the bot's
//! device certificate and delegation with the root itself and commits the
//! renewal as a governed change, visible in the room's history, once less
//! than a third of the delegation is left.

use ed25519_dalek::SigningKey;
use tilo_peer_state::workspace_membership::{
    BotDevice, BotInstall, BotRenewal, WorkspaceMembershipCommand,
};
use tilo_peer_state::{AppearancePolicy, Installation, InstallationId};
use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

use super::installed::InstalledBot;
use crate::rooms::Rooms;

/// How long a bot's delegation lasts.
pub const DELEGATION_LIFETIME_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// Whether a delegation running to `expires_at_ns` is due for renewal at
/// `now_ns`: less than a third of its lifetime is left.
#[must_use]
pub const fn renewal_due(expires_at_ns: u64, now_ns: u64) -> bool {
    expires_at_ns.saturating_sub(now_ns) < DELEGATION_LIFETIME_NS / 3
}

/// Signs bot devices and delegations with a room root this host holds.
/// Only for hosted rooms whose root the operator generated and mounted
/// into this host.
pub struct HostRootCertifier {
    root: SigningKey,
    lifetime_ns: u64,
}

impl HostRootCertifier {
    /// The certifier signing with `root`.
    #[must_use]
    pub const fn new(root: SigningKey) -> Self {
        Self {
            root,
            lifetime_ns: DELEGATION_LIFETIME_NS,
        }
    }

    /// Issuing delegations that last `lifetime_ns`, to see one run out.
    #[cfg(test)]
    pub(crate) const fn lasting(mut self, lifetime_ns: u64) -> Self {
        self.lifetime_ns = lifetime_ns;
        self
    }

    /// The bot's device key: derived from the root and the installation,
    /// so the host keeps no second secret per bot and a restart finds the
    /// same device.
    #[must_use]
    pub fn device_key(&self, installation: &str) -> SigningKey {
        let mut material = self.root.to_bytes().to_vec();
        material.extend_from_slice(installation.as_bytes());
        SigningKey::from_bytes(&blake3::derive_key(
            "tilo hosted bot device key v1",
            &material,
        ))
    }

    /// The bot's device and a fresh delegation to it in `room` from
    /// `now_ns`. The certificate is the same on every call, as a renewal
    /// on the same device requires.
    ///
    /// # Errors
    /// Signing failed or the bot's name does not fit a delegation label.
    pub fn device(&self, room: &str, bot: &InstalledBot, now_ns: u64) -> Result<BotDevice, String> {
        let device_public_key = self
            .device_key(&bot.installation)
            .verifying_key()
            .to_bytes();
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key,
            device_label: format!("Bot: {}", bot.name),
            capabilities: DeviceCapabilities {
                can_own_workspaces: false,
                can_admit_devices: false,
                can_act_as_host: false,
                can_read_all_workspaces: false,
                foreign: false,
                can_administer_devices: false,
            },
            issued_at_ns: u64::try_from(bot.installed_at_ms)
                .unwrap_or(0)
                .saturating_mul(1_000_000)
                .max(1),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &self.root).map_err(|error| error.to_string())?;
        let hash = blake3::hash(bot.installation.as_bytes());
        let color = [hash.as_bytes()[0], hash.as_bytes()[1], hash.as_bytes()[2]];
        let delegation = Delegation::sign(
            &self.root,
            device_public_key,
            Delegate::Bot {
                installation: bot.installation.clone(),
            },
            bot.name.clone(),
            color,
            DelegateScope {
                rooms: vec![room.to_owned()],
                apps: Vec::new(),
            },
            now_ns,
            now_ns.saturating_add(self.lifetime_ns),
        )
        .map_err(|error| error.to_string())?;
        Ok(BotDevice { cert, delegation })
    }
}

/// The governed row for `bot`, installed by `installer` (a root, hex).
fn installation_row(bot: &InstalledBot, installer: &str) -> Result<Installation, String> {
    let shortcut = bot.shortcut()?;
    // A program that posts as other names (a bridge relaying people from
    // elsewhere) needs the room to allow it; anything else keeps its own.
    let personas = tilo_automation::requirements::requirements(&shortcut).appearance;
    Ok(Installation {
        id: InstallationId::parse(&bot.installation)
            .map_err(|_| format!("{} is not an installation id", bot.installation))?,
        program: tilo_automation::program::program_hash(bot.program.as_bytes()),
        name: bot.name.clone(),
        icon: None,
        installed_by: installer.to_owned(),
        installed_at_ms: bot.installed_at_ms,
        appearance: if personas {
            AppearancePolicy::Personas
        } else {
            AppearancePolicy::Own
        },
        source: None,
        armed_at: Vec::new(),
        connector_cursors_from_ms: bot.installed_at_ms,
        inputs: bot.inputs.clone().into_iter().collect(),
        paused: false,
    })
}

/// What [`keep_member`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The room now has the bot, with a fresh delegation.
    Installed,
    /// The room's row now carries the program this host runs.
    Updated,
    /// The delegation was running out and is renewed.
    Renewed,
    /// Nothing was due.
    Current,
}

/// Make `bot` a current governed member of `room`, governed by `host` (a
/// device of `certifier`'s root): install it when the room lacks it,
/// update its row when its program changed, renew its delegation when due.
/// Answers what it did and when the delegation now ends.
///
/// # Errors
/// The room is not governed here, or governance refused the change.
pub async fn keep_member(
    rooms: &Rooms,
    room: &str,
    host: &DeviceCert,
    certifier: &HostRootCertifier,
    bot: &InstalledBot,
) -> Result<(Kept, u64), String> {
    let installer = hex::encode(host.user_public_key);
    let row = installation_row(bot, &installer)?;
    let now = tilo_clock::unix_now_ns();
    let govern = |command| async move {
        rooms
            .govern_bot(room, host, command)
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    };
    let live = rooms
        .installed_bot(room, &bot.installation)
        .await
        .map_err(|error| error.to_string())?
        .filter(|live| live.withdrawn_at_ns.is_none());
    let Some(live) = live else {
        let device = certifier.device(room, bot, now)?;
        let expires = device.delegation.expires_at_ns;
        govern(WorkspaceMembershipCommand::InstallBot(Box::new(
            BotInstall {
                installation: row,
                program: bot.program.clone(),
                device: Some(device),
            },
        )))
        .await?;
        return Ok((Kept::Installed, expires));
    };
    let mut kept = Kept::Current;
    if live.installation.program != row.program {
        govern(WorkspaceMembershipCommand::InstallBot(Box::new(
            BotInstall {
                installation: row.clone(),
                program: bot.program.clone(),
                device: None,
            },
        )))
        .await?;
        kept = Kept::Updated;
    }
    if !renewal_due(live.delegation_expires_at_ns, now) {
        return Ok((kept, live.delegation_expires_at_ns));
    }
    let device = certifier.device(room, bot, now)?;
    let expires = device.delegation.expires_at_ns;
    govern(WorkspaceMembershipCommand::RenewBot(Box::new(BotRenewal {
        installation: row.id,
        device,
    })))
    .await?;
    Ok((Kept::Renewed, expires))
}

/// Keep `bot` a current member of `room` for as long as the host runs:
/// [`keep_member`] now, then again whenever the delegation is due, and
/// after a refusal or an unreachable room a little later.
pub async fn keep_renewed(
    rooms: Rooms,
    room: String,
    host: DeviceCert,
    certifier: HostRootCertifier,
    bot: InstalledBot,
) {
    const RETRY: std::time::Duration = std::time::Duration::from_secs(5 * 60);
    loop {
        let wait = match keep_member(&rooms, &room, &host, &certifier, &bot).await {
            Ok((kept, expires)) => {
                if kept != Kept::Current {
                    tracing::info!(installation = %bot.installation, ?kept, expires_at_ns = expires, "bot membership kept");
                }
                let due = expires.saturating_sub(DELEGATION_LIFETIME_NS / 3);
                std::time::Duration::from_nanos(due.saturating_sub(tilo_clock::unix_now_ns()))
                    .max(RETRY)
            }
            Err(error) => {
                tracing::warn!(installation = %bot.installation, %error, "bot membership not kept");
                RETRY
            }
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renewal_is_due_once_less_than_a_third_remains() {
        let expires = 10 * DELEGATION_LIFETIME_NS;
        let two_thirds = expires - DELEGATION_LIFETIME_NS / 3;
        assert!(!renewal_due(expires, two_thirds - 1));
        assert!(renewal_due(expires, two_thirds + 1));
        assert!(renewal_due(expires, expires + 1), "an expired one too");
    }
}

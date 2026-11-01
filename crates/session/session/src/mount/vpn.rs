//! The VPN kill switch: a `NetworkManager` VPN profile the person set a
//! kill switch on blocks traffic while it is set to connect and is not
//! connected, so what they send never leaves outside the tunnel.
//!
//! Which profiles block is the device VPN crate's decision
//! ([`device_vpn::profile_block`]), followed from the person's settings and
//! `NetworkManager` and carried out by the root helper that owns the
//! firewall. The session runs that follower for as long as it lasts, and
//! publishes what blocks where Settings reads it next to each profile's
//! switch, so the row says "blocking traffic until it is" while the block
//! is up ([`settings_store::device_vpn::DeviceVpnStatus`]).

use std::path::PathBuf;

use device_vpn::profile_block::{self, Sources, Vpns};
use settings_store::device_vpn::{DeviceVpnStatus, ProfileBlockStatus};
use settings_store::live::LiveSettings;

/// The person's VPN profile kill switches, as this session follows them.
pub struct KillSwitch {
    /// The helper's socket.
    helper: PathBuf,
    /// Where `NetworkManager`'s VPNs come from.
    vpns: Vpns,
}

impl KillSwitch {
    /// The machine's own: the installed helper and its `NetworkManager`.
    #[must_use]
    pub fn machine() -> Self {
        Self::with(Sources::system())
    }

    /// Against `sources`: a session that is not on the machine the helper
    /// runs on follows what it is given.
    #[must_use]
    pub fn with(sources: Sources) -> Self {
        Self {
            helper: sources.helper,
            vpns: sources.vpns,
        }
    }

    /// Follow the person's choices from now on, for the rest of the
    /// session: ask the helper to hold or lift the block as the settings
    /// and `NetworkManager` change, and publish what blocks.
    pub fn start(self) {
        let live = LiveSettings::start(|| {});
        let published = std::sync::Mutex::new(None);
        // Published once before anything blocks, so Settings can tell that
        // this session runs even while nothing is blocked.
        publish(&published, ProfileBlockStatus::default());
        profile_block::start_with(
            &live,
            move |block| publish(&published, block),
            Sources {
                helper: self.helper,
                vpns: self.vpns,
            },
        );
    }
}

/// Write what blocks where Settings reads it, when it changed.
fn publish(published: &std::sync::Mutex<Option<ProfileBlockStatus>>, block: ProfileBlockStatus) {
    let Ok(mut published) = published.lock() else {
        return;
    };
    if published.as_ref() == Some(&block) {
        return;
    }
    let (pid, ticks) = match settings_store::hardware::publisher_identity() {
        Ok(identity) => identity,
        Err(error) => {
            tracing::warn!(%error, "could not publish the VPN kill switch's block");
            return;
        }
    };
    // This session routes through no other device and serves no exit, so
    // the rest of the status stays empty: what it publishes is the block.
    let status = DeviceVpnStatus {
        publisher_pid: pid,
        publisher_start_ticks: ticks,
        profile_block: block.clone(),
        ..DeviceVpnStatus::default()
    };
    match status.save() {
        Ok(()) => *published = Some(block),
        Err(error) => tracing::warn!(%error, "could not publish the VPN kill switch's block"),
    }
}

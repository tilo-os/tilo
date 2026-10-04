//! What the VPN client does on this device: import a provider profile into
//! `NetworkManager`, set a shared profile up here, bring a tunnel up or down,
//! delete the local profile, route all traffic over Tor.
//!
//! None of these is a document change. Each is one typed [`VpnEffect`],
//! answered by [`crate::app::VpnApp`] as a job off the UI thread; what an
//! effect changes in the shared fleet (a profile imported here joins it, a
//! deleted one leaves it) it changes afterwards with an ordinary
//! [`crate::actions::VpnAction`]. A saved profile offers its own effects as a
//! live row merged into its published child, with this device's availability:
//! a profile nobody set up here cannot be connected here.

use ui::framework::LiveRow;
use ui::tilo_actions::Availability;
use ui::tilo_resource::{self, Effect};

use crate::model::{ConnId, ConnStatus, Connection};

/// The VPN client's effects.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "vpn.effects")]
pub enum VpnEffect {
    /// Import WireGuard profile.
    ///
    /// Pick a `.conf` file; `NetworkManager` keeps its keys and only the
    /// profile's name joins the shared fleet.
    #[effect(job, cancellable, permission = "admin")]
    ImportWireguard,
    /// Import OpenVPN profile.
    ///
    /// Pick a `.ovpn` file; `NetworkManager` keeps its credentials and only
    /// the profile's name joins the shared fleet.
    #[effect(job, cancellable, permission = "admin")]
    ImportOpenvpn,
    /// Set up on this device.
    ///
    /// Import the exact provider file of a shared profile into this device's
    /// `NetworkManager`.
    #[effect(job, cancellable, permission = "admin")]
    ProvisionLocal {
        #[param("Id of the saved profile")]
        connection: String,
    },
    /// Connect.
    #[effect(job, permission = "admin")]
    Connect {
        #[param("Id of the saved profile")]
        connection: String,
    },
    /// Disconnect.
    #[effect(job, permission = "admin")]
    Disconnect {
        #[param("Id of the saved profile")]
        connection: String,
    },
    /// Delete connection.
    ///
    /// Delete the profile from this device's `NetworkManager`, then from the
    /// shared fleet.
    #[effect(job, destructive, permission = "admin")]
    DeleteConnection {
        #[param("Id of the saved profile")]
        connection: String,
    },
    /// Route all traffic over Tor.
    ///
    /// Start or stop this device's Tor service.
    #[effect(job, permission = "admin")]
    ToggleTorGlobal,
}

/// The id of a saved profile's published child, which its live row joins.
pub fn connection_resource_id(id: ConnId) -> String {
    format!("vpn.connection:{id}")
}

/// What this device can do with one saved profile right now.
pub struct ProfileHere<'a> {
    pub connection: &'a Connection,
    /// The exact `NetworkManager` profile is provisioned here.
    pub provisioned: bool,
    /// A connect or disconnect of it is running.
    pub changing: bool,
}

fn unavailable(code: &str, reason: &str) -> Availability {
    Availability::Unavailable {
        code: code.into(),
        reason: reason.into(),
    }
}

/// The live row of one saved profile: its effects, bound to it, with this
/// device's availability.
pub fn profile_row(profile: &ProfileHere<'_>) -> LiveRow {
    let connection = profile.connection;
    let id = connection.id.to_string();
    let tunnel = if profile.changing || connection.status == ConnStatus::Connecting {
        Availability::Unavailable {
            code: "transition_in_progress".into(),
            reason: "The tunnel is already changing state".into(),
        }
    } else if profile.provisioned {
        Availability::Available
    } else {
        unavailable(
            "local_credentials_required",
            "Set up this shared profile on this device before connecting",
        )
    };
    let connect = if connection.is_connected() {
        unavailable(
            "already_connected",
            "This tunnel is already up on this device",
        )
    } else {
        tunnel.clone()
    };
    let disconnect = if connection.is_connected() {
        tunnel
    } else {
        unavailable("not_connected", "This tunnel is not up on this device")
    };
    let delete = if profile.provisioned {
        Availability::Available
    } else {
        unavailable(
            "local_credentials_required",
            "No exact local NetworkManager profile is provisioned on this device",
        )
    };
    let effects = [
        (
            VpnEffect::ProvisionLocal {
                connection: id.clone(),
            },
            Availability::Available,
        ),
        (
            VpnEffect::Connect {
                connection: id.clone(),
            },
            connect,
        ),
        (
            VpnEffect::Disconnect {
                connection: id.clone(),
            },
            disconnect,
        ),
        (VpnEffect::DeleteConnection { connection: id }, delete),
    ];
    let mut row = LiveRow::new(
        connection_resource_id(connection.id),
        "vpn.connection",
        connection.name.clone(),
    );
    for (effect, availability) in effects {
        let mut spec = effect.bound();
        spec.contract.availability = availability;
        row = row.operation(spec);
    }
    row
}

/// The saved profile an effect names.
///
/// # Errors
/// Returns why `raw` names no profile id.
pub fn connection_id(raw: &str) -> Result<ConnId, String> {
    raw.parse()
        .map_err(|_| "Invalid VPN connection id".to_string())
}

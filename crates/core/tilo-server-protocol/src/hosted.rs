//! What a host hands the machine keeper of a workspace's VM when it places the workspace there
//! (`docs/hosted-browser.md`).
//!
//! A hosted workspace's VM runs the device daemon: one device of the person the workspace
//! belongs to, which runs Browser and the workspace's Linux apps as its own apps. It joins with a
//! join request, never a key: the daemon makes the VM's one device key itself and logs in with the
//! ticket [`Material`] carries, which this host serves for that one placement, and the host has
//! the person's root certify the key for this workspace alone. [`Material`] is what the machine
//! keeper starts the VM's daemon with. Nothing in it is the host's secret, and the ticket's own
//! secret is spent by the first daemon that brings it.

use collab_types::WorkspaceId;
use std::net::ToSocketAddrs as _;

use iroh_base::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};

/// What a host hands the machine keeper of a workspace's VM when it places the workspace there:
/// the workspace, the record its Browser opens on, and the ticket the VM's daemon logs in with.
/// The host is reached where the ticket's issuer is.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    /// The workspace, by its UUID: the one workspace the VM's device reaches.
    pub workspace: String,
    /// The workspace, as the host carries it.
    pub workspace_id: WorkspaceId,
    /// The Browser record the window opens on.
    pub record: String,
    /// The ticket the VM's daemon logs in with (devices#logIn), served by this host for this
    /// placement: the daemon makes the VM's one device key, and the host certifies that key
    /// with its own for the VM's records alone.
    pub ticket: tilo_device_identity::login::Ticket,
    /// The person the workspace belongs to.
    pub user_public_key: [u8; 32],
    /// Who pays for the workspace's machines, as the host knows it.
    pub account: String,
}

/// The endpoint `text` names, written `KEY@HOST:PORT`: the device key in hex,
/// then where the endpoint is reached.
///
/// A machine outside a host's network reaches it at a public address, and
/// only there: the value an operator sets names that address, never one of
/// the host's own.
///
/// # Errors
///
/// The text is not `KEY@HOST:PORT` with a key that is one and an address that
/// resolves.
pub fn endpoint(text: &str) -> Result<EndpointAddr, String> {
    let (key, address) = text
        .split_once('@')
        .ok_or_else(|| format!("{text:?} is not KEY@HOST:PORT"))?;
    let key: [u8; 32] = hex::decode(key)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| format!("{key:?} is not a device key"))?;
    let id = EndpointId::from_bytes(&key).map_err(|error| error.to_string())?;
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{address}: {error}"))?
        .map(iroh_base::TransportAddr::Ip);
    Ok(EndpointAddr::from_parts(id, sockets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_endpoint_is_the_device_key_and_where_it_is_reached() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes();
        let written = format!("{}@127.0.0.1:7704", hex::encode(key));
        let read = endpoint(&written).expect("an endpoint");
        assert_eq!(read.id, EndpointId::from_bytes(&key).unwrap());
        assert!(
            !read.addrs.is_empty(),
            "the address it is reached at comes with it"
        );
        // and anything else is refused in words that say what it should be
        for wrong in ["127.0.0.1:7704", "@127.0.0.1:7704", "nothex@127.0.0.1:1"] {
            assert!(endpoint(wrong).is_err(), "{wrong:?}");
        }
    }
}

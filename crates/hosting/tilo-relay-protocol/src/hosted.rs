//! What a host hands the browser session in a workspace's VM when it gives it the
//! workspace (`docs/hosted-browser.md`).
//!
//! A hosted workspace's Browser runs in the workspace's own VM,
//! beside its Linux VM, and the session in that VM is a device of the
//! workspace: this host certifies it one, for as long as the session may
//! run, and lists it in that one workspace. [`Material`] is what the session
//! is started with — the workspace it joins, the record whose window it
//! opens, its own device certificate and key, and where the host is reached.
//! It travels as one line of the session's input; nothing here is the host's
//! secret.

use collab_types::WorkspaceId;
use std::net::ToSocketAddrs as _;

use iroh_base::{EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// What a host hands a workspace's Browser when it gives it a workspace: the
/// device it certified for it, and where to be that device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    /// The workspace, by its UUID: the one that lists the device.
    pub workspace: String,
    /// The workspace, as the host carries it.
    pub workspace_id: WorkspaceId,
    /// The Browser record the window opens on.
    pub record: String,
    /// The device the workspace lists while the workspace's VM runs, and no
    /// other: certified by the host as a member device of the person the
    /// workspace belongs to.
    pub device: DeviceCert,
    /// That device's signing key, the only copy: the session holds it.
    pub secret: [u8; 32],
    /// Where the session joins the workspace: the host's endpoint, at the
    /// address a machine outside its network reaches it by.
    pub relay: EndpointAddr,
    /// The person the workspace belongs to.
    pub user_public_key: [u8; 32],
    /// Who pays for the workspace's machines, as the host knows it.
    pub account: String,
}

impl Material {
    /// This device's signing key.
    ///
    /// # Errors
    ///
    /// The secret is not a key.
    pub fn key(&self) -> Result<ed25519_dalek::SigningKey, String> {
        let bytes: [u8; 32] = self
            .secret
            .as_slice()
            .try_into()
            .map_err(|_| "an environment's secret is a 32-byte key".to_owned())?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&bytes))
    }
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

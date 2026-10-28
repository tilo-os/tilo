//! Where this device's secret store keeps a connection's password or key
//! passphrase: the purpose each kind of connection's secret is filed under.

use crate::model::{AuthMethod, ConnectionKind};

/// The secret-store purposes of a connection's credentials, keyed by its id.
pub(super) const SSH_PASSWORD: &str = "ssh-password";

pub(super) const KEY_PASSPHRASE: &str = "ssh-key-passphrase";

pub(super) const VNC_PASSWORD: &str = "vnc-password";

/// Which credential a connection of `kind` authenticating with `auth` uses.
pub(super) fn secret_purpose(kind: ConnectionKind, auth: AuthMethod) -> &'static str {
    match (kind, auth) {
        (ConnectionKind::Vnc, _) => VNC_PASSWORD,
        (_, AuthMethod::PublicKey) => KEY_PASSPHRASE,
        _ => SSH_PASSWORD,
    }
}

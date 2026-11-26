//! What the Remote app asks a device for, and what a session says back: the asks a saved
//! connection opens a session with, read into a session's configuration, and the words a stream
//! says of itself as JSON the app's views decode.
//!
//! A connection's password or key passphrase is never in an ask. The app names it by the key it
//! was kept under (`secret_key`, `password_key`), and whoever opens the session reads it from the
//! store the device keeps the app's secrets in: the workspace's vault, for the Remote app's own
//! process. The value lives in that process for the session it signs in, and nowhere else.

use serde::Deserialize;
use serde_json::json;

use crate::ssh::{SshConfig, SshStatus, default_key_paths, default_user, resolve_auth};
use crate::trust::{HostKeyIdentity, HostKeyIssue};
use crate::vnc::{VncConfig, VncStatus};

/// Reads a secret kept under a key: `None` when nothing is kept there, an error in words a person
/// reads when the store refused.
pub type Kept<'a> = &'a dyn Fn(&str) -> Result<Option<String>, String>;

/// An SSH shell the app asks for.
#[derive(Clone, Debug, Deserialize)]
pub struct SshAsk {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub user: Option<String>,
    /// `password` or `key`: how the saved connection signs in.
    pub method: String,
    /// The key the password or the key's passphrase was kept under, when one was.
    #[serde(default)]
    pub secret_key: Option<String>,
    /// The identity file a key sign-in uses; the usual `~/.ssh` ones when none is named.
    #[serde(default)]
    pub key_path: Option<String>,
    /// The key this device trusted for the machine, when it trusted one.
    #[serde(default)]
    pub expected: Option<HostKeyIdentity>,
    pub columns: u16,
    pub rows: u16,
}

/// A VNC desktop the app asks for.
#[derive(Clone, Debug, Deserialize)]
pub struct VncAsk {
    pub host: String,
    pub port: u16,
    /// The key the server's password was kept under, when one was.
    #[serde(default)]
    pub password_key: Option<String>,
}

/// A password or passphrase the app hands over once, to be kept under `key`.
#[derive(Clone, Debug, Deserialize)]
pub struct KeepAsk {
    pub key: String,
    pub secret: String,
}

/// The secret kept under `key`, when the ask names one.
fn secret_of(key: Option<&str>, kept: Kept<'_>) -> Result<Option<String>, String> {
    match key {
        Some(key) if !key.is_empty() => kept(key).map_err(|why| {
            format!("The saved password could not be read from the workspace's vault: {why}")
        }),
        _ => Ok(None),
    }
}

/// The session `ask` opens, signed in with what `kept` holds for it.
///
/// # Errors
///
/// The vault refused the saved secret, or a key sign-in has no identity file to use.
pub fn ssh_config(ask: SshAsk, kept: Kept<'_>) -> Result<SshConfig, String> {
    let secret = secret_of(ask.secret_key.as_deref(), kept)?;
    let keys = ask
        .key_path
        .as_ref()
        .map_or_else(default_key_paths, |path| {
            vec![std::path::PathBuf::from(path)]
        });
    let auth =
        resolve_auth(ask.method == "key", secret, &keys).map_err(|error| error.to_string())?;
    Ok(SshConfig {
        host: ask.host,
        port: ask.port,
        user: ask
            .user
            .filter(|user| !user.is_empty())
            .unwrap_or_else(default_user),
        auth,
        cols: ask.columns.max(2),
        rows: ask.rows.max(2),
        expected: ask.expected,
    })
}

/// The desktop `ask` opens, signed in with what `kept` holds for it.
///
/// # Errors
///
/// The vault refused the saved password.
pub fn vnc_config(ask: VncAsk, kept: Kept<'_>) -> Result<VncConfig, String> {
    Ok(VncConfig {
        password: secret_of(ask.password_key.as_deref(), kept)?,
        host: ask.host,
        port: ask.port,
    })
}

/// What an SSH stream says of itself.
#[must_use]
pub fn ssh_word(status: &SshStatus) -> String {
    match status {
        SshStatus::Connecting => json!({ "phase": "connecting" }).to_string(),
        SshStatus::Connected => json!({ "phase": "connected" }).to_string(),
        SshStatus::Closed => json!({ "phase": "closed" }).to_string(),
        SshStatus::Failed {
            why,
            issue: Some(HostKeyIssue::FirstContact { observed }),
        } => json!({
            "phase": "need-trust",
            "algorithm": observed.algorithm,
            "fingerprint": observed.fingerprint,
            "why": why,
        })
        .to_string(),
        SshStatus::Failed {
            why,
            issue: Some(HostKeyIssue::Changed { expected, observed }),
        } => json!({
            "phase": "key-changed",
            "expected": expected,
            "observed": observed,
            "why": why,
        })
        .to_string(),
        SshStatus::Failed { why, issue: None } => {
            json!({ "phase": "failed", "why": why }).to_string()
        }
    }
}

/// What a VNC stream says of itself.
#[must_use]
pub fn vnc_word(status: &VncStatus) -> String {
    match status {
        VncStatus::Connecting => json!({ "phase": "connecting" }).to_string(),
        VncStatus::Connected { width, height } => json!({
            "phase": "connected",
            "width": width,
            "height": height,
        })
        .to_string(),
        VncStatus::Closed => json!({ "phase": "closed" }).to_string(),
        VncStatus::Failed(why) => json!({ "phase": "failed", "why": why }).to_string(),
    }
}

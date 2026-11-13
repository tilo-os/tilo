//! Tilo's API served in process, as a key its caller holds, against a Tilo
//! host the caller reaches directly: what the SDKs' native part and the
//! `tilo` command use when there is no API server between them and the host.
//!
//! It is the API server itself ([`Server`]), with one key in its memory: the
//! caller's own, a device of the person's (a computer `tilo login` made one)
//! or a grant of theirs, with the certificate the person's root (or a key of
//! theirs) gave it. Every route is answered as on an API server, by the same
//! code, and the key dials the host as one of the person's devices does; the
//! host decides everything, as it decides for an API server. Nothing here
//! knows what kind of host it reaches.
//!
//! A key is carried as a key file ([`KeyFile`]): its secret and its
//! certificate, hex, and the host it reaches.

use crate::grant_key::GrantKey;
use crate::store::{GrantKind, Store};
use crate::{Config, Reply, Server, now_ms};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tilo_user_identity::DeviceCert;

/// A key as a file holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyFile {
    /// The Tilo host it reaches: an origin (`https://eu1.tilo.sh`).
    pub host: String,
    /// What the host's people see it as, and History names its runs.
    pub name: String,
    /// The key's 32-byte secret, hex.
    pub key: String,
    /// Its certificate, canonical bytes as hex.
    pub cert: String,
}

impl KeyFile {
    /// `key` with `cert`, reaching `host`, named `name`.
    #[must_use]
    pub fn new(host: &str, name: &str, key: &ed25519_dalek::SigningKey, cert: &DeviceCert) -> Self {
        Self {
            host: host.trim_end_matches('/').to_owned(),
            name: name.to_owned(),
            key: hex::encode(key.to_bytes()),
            cert: hex::encode(
                tilo_user_identity::canonical::canonical_bytes(cert).unwrap_or_default(),
            ),
        }
    }

    /// The key file at `path`.
    ///
    /// # Errors
    /// It cannot be read, or is not a key file.
    pub fn read(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Tilo's API in process, as one key.
pub struct Direct {
    server: Arc<Server>,
    authorization: String,
}

impl Direct {
    /// The API as the key `file` holds, reaching its host.
    ///
    /// # Errors
    /// The key or its certificate is not one, or does not hold now.
    pub fn new(file: &KeyFile) -> Result<Self, String> {
        let secret = hex::decode(&file.key).map_err(|_| "the key is not hex".to_owned())?;
        let cert = hex::decode(&file.cert).map_err(|_| "the certificate is not hex".to_owned())?;
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| "a key is 32 bytes".to_owned())?;
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert)
            .map_err(|error| format!("the certificate: {error}"))?;
        let key = GrantKey::held(
            ed25519_dalek::SigningKey::from_bytes(&secret),
            cert,
            now_ms().saturating_mul(1_000_000),
        )
        .map_err(|refused| refused.to_string())?;
        let host = file.host.trim_end_matches('/');
        let server = Server::new(
            &Config {
                issuer: host.to_owned(),
                web_entry: host.to_owned(),
                relay: host.to_owned(),
            },
            Store::in_memory(),
        )
        .map_err(|error| error.to_string())?;
        let (_, token) = server
            .store
            .record_key(&key, &file.name, GrantKind::Key, None, now_ms())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            server,
            authorization: format!("Bearer {token}"),
        })
    }

    /// Answer `method` on `target` (a path with its query) with `headers` and
    /// `body`, as the API server answers it.
    pub async fn request(
        &self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Reply {
        let Ok(method) = hyper::Method::from_bytes(method.as_bytes()) else {
            return crate::json_reply(
                hyper::StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": {"code": "invalid_request", "message": "no such method"}}),
            );
        };
        let mut map = hyper::HeaderMap::new();
        for (name, value) in headers {
            if let (Ok(name), Ok(value)) = (
                hyper::header::HeaderName::from_bytes(name.as_bytes()),
                hyper::header::HeaderValue::from_str(value),
            ) {
                map.append(name, value);
            }
        }
        if let Ok(value) = hyper::header::HeaderValue::from_str(&self.authorization) {
            map.insert(hyper::header::AUTHORIZATION, value);
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        self.server.dispatch(&method, path, query, &map, body).await
    }
}

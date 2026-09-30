//! The registry's HTTP API, from a publisher's side.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use registry_types::application::PublisherApplication;
use registry_types::{EntryAction, EntryChangeRequest, RegistryEntry, SignedRegistrySnapshot};

/// The registry Tilo OS and Tilo Web install from.
pub const DEFAULT_REGISTRY: &str = "https://apps.tilo.so";

/// The environment variable that names another registry.
pub const REGISTRY_ENV: &str = "TILO_REGISTRY";

/// A registry, reached over HTTPS (or plain HTTP on this machine, for a
/// registry under test).
pub struct RegistryClient {
    base: String,
    http: reqwest::blocking::Client,
}

impl RegistryClient {
    /// The registry at `base`, else the one in [`REGISTRY_ENV`], else
    /// [`DEFAULT_REGISTRY`].
    ///
    /// # Errors
    ///
    /// When the URL is not HTTPS or loopback HTTP.
    pub fn resolve(base: Option<&str>) -> Result<Self> {
        let from_env = std::env::var(REGISTRY_ENV).ok();
        Self::new(
            base.or_else(|| from_env.as_deref().filter(|value| !value.is_empty()))
                .unwrap_or(DEFAULT_REGISTRY),
        )
    }

    /// The registry at `base`.
    ///
    /// # Errors
    ///
    /// When the URL is not HTTPS or loopback HTTP.
    pub fn new(base: &str) -> Result<Self> {
        let url = reqwest::Url::parse(base).with_context(|| format!("registry URL {base}"))?;
        let loopback = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            bail!("a registry is reached over https (plain http only on this machine)");
        }
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(300))
            .build()
            .context("build HTTP client")?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            http,
        })
    }

    /// The registry's URL.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The published entry for `package_id`, if the catalogue has one.
    ///
    /// # Errors
    ///
    /// When the registry cannot be reached or answers with something else.
    pub fn entry(&self, package_id: &str) -> Result<Option<RegistryEntry>> {
        let url = format!("{}/entries/{package_id}", self.base);
        let response = self.http.get(&url).send().with_context(|| url.clone())?;
        match response.status() {
            // An empty registry has no snapshot yet.
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::SERVICE_UNAVAILABLE => Ok(None),
            status if status.is_success() => {
                let snapshot: SignedRegistrySnapshot =
                    response.json().with_context(|| format!("decode {url}"))?;
                Ok(snapshot
                    .entries
                    .into_iter()
                    .find(|entry| entry.package_id == package_id))
            }
            _ => Err(failure(&url, response)),
        }
    }

    /// The entry `key` has waiting for review for `package_id`, if any.
    /// Only its publisher may read it.
    ///
    /// # Errors
    ///
    /// When the registry cannot be reached or refuses the read.
    pub fn pending(&self, key: &SigningKey, package_id: &str) -> Result<Option<RegistryEntry>> {
        let url = format!("{}/entries/{package_id}/pending", self.base);
        let signed_at = tilo_clock::unix_now();
        let response = self
            .http
            .get(&url)
            .header(
                registry_types::upload::UPLOAD_PUBLISHER_HEADER,
                crate::key::public_hex(key),
            )
            .header(
                registry_types::pending::PENDING_TIME_HEADER,
                signed_at.to_string(),
            )
            .header(
                registry_types::upload::UPLOAD_SIGNATURE_HEADER,
                registry_types::pending::sign_pending_read(key, package_id, signed_at),
            )
            .send()
            .with_context(|| url.clone())?;
        match response.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            status if status.is_success() => {
                let change: EntryChangeRequest =
                    response.json().with_context(|| format!("decode {url}"))?;
                Ok(Some(change.entry))
            }
            _ => Err(failure(&url, response)),
        }
    }

    /// Upload a package `key` signed, for review. Answers the URL the
    /// package will be published at.
    ///
    /// # Errors
    ///
    /// When the registry refuses the upload.
    pub fn upload(&self, key: &SigningKey, tapp_bytes: &[u8]) -> Result<String> {
        let digest = blake3::hash(tapp_bytes).to_hex().to_string();
        let url = format!("{}/uploads/{digest}.tapp", self.base);
        let response = self
            .http
            .put(&url)
            .header(
                registry_types::upload::UPLOAD_PUBLISHER_HEADER,
                crate::key::public_hex(key),
            )
            .header(
                registry_types::upload::UPLOAD_SIGNATURE_HEADER,
                registry_types::upload::sign_upload(key, &digest),
            )
            .body(tapp_bytes.to_vec())
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        let answer: serde_json::Value = response.json().with_context(|| format!("decode {url}"))?;
        answer
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{url} answered without the package URL"))
    }

    /// Ask the registry to add, update or delete `entry`.
    ///
    /// # Errors
    ///
    /// When the registry refuses the request.
    pub fn submit(
        &self,
        action: EntryAction,
        entry: RegistryEntry,
        key: &SigningKey,
    ) -> Result<()> {
        let url = format!("{}/entries", self.base);
        let response = self
            .http
            .post(&url)
            .json(&registry_types::sign_change_request(action, entry, key))
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        Ok(())
    }

    /// Send a signed publisher application. Answers its status.
    ///
    /// # Errors
    ///
    /// When the registry refuses it; the message names any missing proof.
    pub fn apply(&self, application: &PublisherApplication) -> Result<serde_json::Value> {
        let url = format!("{}/publishers/applications", self.base);
        let response = self
            .http
            .post(&url)
            .json(application)
            .send()
            .with_context(|| url.clone())?;
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        response.json().with_context(|| format!("decode {url}"))
    }

    /// The status of the application from `pubkey_hex`, if there is one.
    ///
    /// # Errors
    ///
    /// When the registry cannot be reached.
    pub fn application(&self, pubkey_hex: &str) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/publishers/applications/{pubkey_hex}", self.base);
        let response = self.http.get(&url).send().with_context(|| url.clone())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(failure(&url, response));
        }
        response
            .json()
            .map(Some)
            .with_context(|| format!("decode {url}"))
    }
}

fn failure(url: &str, response: reqwest::blocking::Response) -> anyhow::Error {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        anyhow!("{url}: the registry answered {status}")
    } else {
        anyhow!("{url}: the registry answered {status}: {body}")
    }
}

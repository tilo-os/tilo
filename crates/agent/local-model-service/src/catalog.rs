//! Shared verification for model consumers and the release compiler.
use crate::SignedRuntime;
use agent_provider_config::local::LocalModelRelease;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

pub const DEVELOPER_SIGNATURE_DOMAIN: &str = "tilo.local-model.developer-offline.v1";

pub const CATALOG_SIGNATURE_DOMAIN: &str = "tilo.local-model.catalog.v1";
pub const IMAGE_CATALOG: &str = "/usr/share/tilo/system-extensions/local-model-catalog.json";
pub const IMAGE_KEYS: &str = "/usr/share/tilo/system-extensions/local-model-keys.json";

/// Signed, release-channel catalog. The signature covers canonical JSON of
/// `payload`; model and runtime artifacts within it remain individually pinned.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedCatalog {
    pub schema: u16,
    pub signature_domain: String,
    pub key_id: String,
    pub signature: Vec<u8>,
    pub payload: CatalogPayload,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogPayload {
    pub schema: u16,
    pub models: Vec<LocalModelRelease>,
    pub runtimes: Vec<SignedRuntime>,
    pub recommendation: String,
}
#[derive(Default, Deserialize)]
struct KeyFile {
    keys: BTreeMap<String, String>,
}

/// A catalogue verified against explicitly supplied trust input. The payload
/// is immutable through this witness; speech loading requires this type.
pub struct VerifiedCatalog(CatalogPayload);
impl VerifiedCatalog {
    /// Verify envelope, canonical payload, schema and all release shapes.
    /// # Errors
    /// Rejects unreadable, oversized, untrusted, malformed or unsigned input.
    pub fn load(path: &Path, keys: &Path) -> Result<Self, String> {
        load_catalog(path, keys, CATALOG_SIGNATURE_DOMAIN).map(Self)
    }
    /// Verify an explicitly selected offline developer catalogue. Normal image
    /// consumers reject this separate signature domain even with the same key.
    /// # Errors
    /// Rejects unsigned, mismatched-domain or malformed receipt metadata.
    pub fn load_developer_offline(path: &Path, keys: &Path) -> Result<Self, String> {
        load_catalog(path, keys, DEVELOPER_SIGNATURE_DOMAIN).map(Self)
    }

    pub fn payload(&self) -> &CatalogPayload {
        &self.0
    }
    pub fn into_payload(self) -> CatalogPayload {
        self.0
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("model catalogue input is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// Canonical signing bytes shared by the publisher and consumer.
pub fn catalog_signing_bytes(payload: &CatalogPayload) -> Result<Vec<u8>, String> {
    signing_bytes_for_domain(payload, CATALOG_SIGNATURE_DOMAIN)
}

/// Signing bytes for the explicit developer channel.
pub fn developer_signing_bytes(payload: &CatalogPayload) -> Result<Vec<u8>, String> {
    signing_bytes_for_domain(payload, DEVELOPER_SIGNATURE_DOMAIN)
}

fn signing_bytes_for_domain(payload: &CatalogPayload, domain: &str) -> Result<Vec<u8>, String> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(serde_json::to_vec(payload).map_err(|error| error.to_string())?);
    Ok(bytes)
}

fn load_catalog(path: &Path, keys: &Path, domain: &str) -> Result<CatalogPayload, String> {
    let signed: SignedCatalog = read_json(path)?;
    if signed.schema != 1 {
        return Err("unsupported signed catalog envelope schema".into());
    }
    if signed.payload.schema != 1 {
        return Err("unsupported model catalog schema".into());
    }
    let keyfile: KeyFile = read_json(keys)?;
    let key_hex = keyfile
        .keys
        .get(&signed.key_id)
        .ok_or_else(|| "catalog signing key is not trusted".to_string())?;
    let bytes = hex::decode(key_hex).map_err(|_| "invalid catalog public key")?;
    let key = VerifyingKey::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid catalog public key")?,
    )
    .map_err(|_| "invalid catalog public key")?;
    let signature =
        Signature::from_slice(&signed.signature).map_err(|_| "invalid catalog signature")?;
    if signed.signature_domain != domain {
        return Err("unexpected catalog signature domain".into());
    }
    let payload = signing_bytes_for_domain(&signed.payload, domain)?;
    key.verify(&payload, &signature)
        .map_err(|_| "catalog signature verification failed")?;
    for runtime in &signed.payload.runtimes {
        runtime.validate().map_err(|error| error.to_string())?;
        validate_pin(&runtime.release.artifact, domain)?;
    }
    for model in &signed.payload.models {
        validate_pin(&model.artifact, domain)?;
        model
            .validate_shape()
            .map_err(|error| format!("{}: {error}", model.id))?;
    }
    Ok(signed.payload)
}

fn validate_pin(
    pin: &agent_provider_config::local::PinnedArtifact,
    domain: &str,
) -> Result<(), String> {
    if domain == DEVELOPER_SIGNATURE_DOMAIN && !pin.url.starts_with("https://") {
        crate::developer::validate_offline_pin(pin).map_err(|error| error.to_string())
    } else {
        pin.validate().map_err(|error| error.to_string())
    }
}

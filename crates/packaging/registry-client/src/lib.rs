//! A publisher's side of the Tilo registry: keys, signed entries, and the
//! registry's HTTP API for uploads, entries and publisher applications.
//!
//! `tilo-app publish`, `tilo-app apply` and the App Publisher app use this;
//! so can any CI that would rather not shell out.

pub mod bot;
pub mod client;
pub mod entry;
pub mod key;
pub mod publish;
pub mod runtime_history;

pub use bot::{BotRelease, build_bot_entry};
pub use client::{DEFAULT_REGISTRY, REGISTRY_ENV, RegistryClient};
pub use entry::{
    EntryMetadata, Platform, ThinPackage, build_entry, build_entry_with_objects, check_package,
    check_package_with,
};
pub use publish::{Published, publish};

use anyhow::{Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use registry_types::application::{
    NamespaceClaim, PublisherApplication, dns_txt_name, forge_proof_url, well_known_url,
};
use registry_types::namespace::Forge;
use serde_json::Value;

/// Sign an application for `claims` and send it. Answers its status.
///
/// # Errors
///
/// When the registry refuses it; the message names any missing proof.
pub fn apply(
    client: &RegistryClient,
    key: &SigningKey,
    claims: Vec<NamespaceClaim>,
    contact: String,
    note: String,
) -> Result<serde_json::Value> {
    let mut application = PublisherApplication {
        pubkey_hex: String::new(),
        claims,
        contact,
        note,
        submitted_at_unix: tilo_clock::unix_now(),
        signature_hex: String::new(),
    };
    application.sign(key);
    client.apply(&application)
}

/// The claim `FORGE:ACCOUNT` names, such as `github:ada`.
pub fn forge_claim(text: &str) -> Result<NamespaceClaim> {
    let (forge, account) = text
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("{text:?}: expected FORGE:ACCOUNT, such as github:ada"))?;
    let parsed = Forge::parse(forge).ok_or_else(|| anyhow!("{text:?}: unknown forge"))?;
    if parsed.namespace(account).is_none() {
        bail!("{text:?}: {account:?} is not an account name");
    }
    Ok(NamespaceClaim::Forge {
        forge: forge.to_owned(),
        account: account.to_owned(),
    })
}

/// Every place the registry looks for the proof of `claim`.
#[must_use]
pub fn proof_places(claim: &NamespaceClaim) -> Vec<String> {
    match claim {
        NamespaceClaim::Domain { domain } => vec![
            format!("DNS TXT {}", dns_txt_name(domain)),
            format!("or {}", well_known_url(domain)),
        ],
        NamespaceClaim::Forge { forge, account } => Forge::parse(forge)
            .map(|forge| vec![forge_proof_url(&forge, account)])
            .unwrap_or_default(),
    }
}

/// One line for an application status: its state, namespaces and any reason.
#[must_use]
pub fn status_summary(status: &Value) -> String {
    let namespaces = status
        .get("namespaces")
        .and_then(Value::as_array)
        .map(|namespaces| {
            namespaces
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let state = status
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let reason = status
        .get("reason")
        .and_then(Value::as_str)
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    format!("{state} ({namespaces}){reason}")
}

//! Nextcloud metadata definitions shared by review and selected capture.
use super::{Candidate, Inventory, Origin, Provider};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub struct NativeAccount {
    pub account_id: String,
    pub server: String,
    pub username: String,
    pub label: String,
    pub supported: bool,
}
pub fn source_id(account: &NativeAccount) -> String {
    let mut hash = Sha256::new();
    for field in [&account.account_id, &account.server, &account.username] {
        hash.update(field.len().to_le_bytes());
        hash.update(field.as_bytes());
    }
    format!("nextcloud-source-{}", hex::encode(hash.finalize()))
}
pub fn accounts(text: &str) -> Vec<NativeAccount> {
    let parsed = super::sections(
        text,
        &[
            "url",
            "user",
            "http_user",
            "webflow_user",
            "displayName",
            "authType",
            "http_clientCertPkcs12",
        ],
    );
    let Some(config) = parsed.get("Accounts") else {
        return Vec::new();
    };
    let mut groups: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in config {
        if let Some((id, leaf)) = key.split_once('\\')
            && !leaf.contains('\\')
        {
            groups.entry(id).or_default().insert(leaf, value);
        }
    }
    groups
        .into_iter()
        .filter_map(|(id, values)| {
            let raw_server = *values.get("url")?;
            super::safe_server(raw_server)?;
            // QtKeychain uses the stored URL spelling, adding only a trailing slash.
            let server = raw_server.to_owned();
            let username = values
                .get("http_user")
                .or_else(|| values.get("user"))
                .or_else(|| values.get("webflow_user"))
                .copied()
                .unwrap_or_default()
                .to_owned();
            let supported = values.get("http_user").is_some_and(|user| !user.is_empty())
                && values.get("authType").is_none_or(|kind| *kind == "http")
                && values
                    .get("http_clientCertPkcs12")
                    .is_none_or(|cert| cert.is_empty())
                && tilo_secrets::owner_migration::NextcloudCredential::new(&server, &username, id)
                    .is_ok();
            let label = values
                .get("displayName")
                .copied()
                .unwrap_or(&username)
                .chars()
                .take(256)
                .collect();
            Some(NativeAccount {
                account_id: id.into(),
                server,
                username,
                label,
                supported,
            })
        })
        .collect()
}
pub fn discover(text: &str, path: &Path, inventory: &mut Inventory) {
    for account in accounts(text) {
        inventory.candidates.push(Candidate {
            id: source_id(&account),
            provider: Provider::Nextcloud,
            origin: Origin::Nextcloud,
            label: account.label,
            identity: Some(account.username),
            server: Some(account.server),
            profile: path.to_owned(),
            needs_sign_in: true,
            can_import_credentials: account.supported,
        });
    }
}

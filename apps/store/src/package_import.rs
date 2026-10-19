//! Local package review against the same independently verified Store offers.
//! An archive can describe itself; it cannot authorize its publisher or identity.

use std::sync::Arc;

use app_host::install::InstallExpectation;

use crate::{StoreItem, StoreSource, StoreTrust};

pub const MAX_IMPORT_BYTES: usize = 64 * 1024 * 1024;

/// Immutable bytes and independent catalogue identity retained until approval.
#[derive(Clone, Debug)]
pub struct ImportedPackage {
    pub bytes: Arc<[u8]>,
    pub item: StoreItem,
    pub expected: InstallExpectation,
    pub registry: String,
    pub local: Option<app_host::local_package::LocalReview>,
}

/// Review a package without installing or trusting anything it says about itself.
///
/// # Errors
/// Returns a bounded human-readable refusal for malformed, unsigned, unlisted,
/// mismatched or non-application packages.
pub fn review(bytes: Vec<u8>, offers: &[StoreItem]) -> Result<ImportedPackage, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMPORT_BYTES {
        return Err(ui::i18n!("store-package-size-refused"));
    }
    let archive = app_host::local_package::review_archive(&bytes).map_err(|error| match error {
        app_host::local_package::ArchiveReviewError::Invalid => ui::i18n!("store-package-invalid"),
        app_host::local_package::ArchiveReviewError::NotApplication => {
            ui::i18n!("store-package-app-only")
        }
    })?;
    for item in offers {
        let StoreSource::Tilo { registry_name } = &item.source else {
            continue;
        };
        if item.trust != StoreTrust::PublisherSigned || item.id != archive.app_id() {
            continue;
        }
        let Some(key) = item
            .publisher_pubkey_hex
            .as_deref()
            .and_then(|key| hex::decode(key).ok())
        else {
            continue;
        };
        let Ok(publisher) = <[u8; 32]>::try_from(key) else {
            continue;
        };
        // The offer's size describes the installer's canonical download,
        // not this file's full/thin representation. Identity is bound by the
        // signed root; preserve the independently authenticated transport size.
        for version in &item.versions {
            let Some(package_root) = version
                .root_hash_hex
                .as_deref()
                .and_then(|root| format!("tapp:{root}").parse().ok())
            else {
                continue;
            };
            let expected = InstallExpectation {
                package_root,
                publisher,
                app_id: item.id.clone(),
                version: version.version.clone(),
                size_bytes: version.size_bytes,
            };
            if !archive.accepts_offer(&expected) {
                continue;
            }
            return Ok(ImportedPackage {
                expected,
                item: {
                    let mut reviewed = item.clone();
                    reviewed.versions = vec![version.clone()];
                    reviewed
                },
                registry: registry_name.clone(),
                local: None,
                bytes: bytes.into(),
            });
        }
    }
    Err(ui::i18n!("store-package-offer-required"))
}

/// Review an unlisted archive through the launcher-issued host capability.
///
/// # Errors
/// Returns an existing-trust refusal; never supplies a publisher trust key.
pub fn review_local(
    bytes: Vec<u8>,
    host: &app_host::client::BlockingClient,
) -> Result<ImportedPackage, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMPORT_BYTES {
        return Err(ui::i18n!("store-package-size-refused"));
    }
    let checked = host.review_local(&bytes)?;
    let package = &checked.manifest.package;
    let expected = app_host::local_package::local_expectation(&checked, bytes.len() as u64)
        .map_err(|_| ui::i18n!("store-package-invalid"))?;
    let item = StoreItem {
        bot: None,
        awaiting_developer: false,
        id: package.id.clone(),
        name: package.name.clone().unwrap_or_else(|| package.id.clone()),
        description: ui::i18n!("store-package-local-description"),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: Vec::new(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: ui::i18n!("store-package-local-source"),
        },
        trust: StoreTrust::TrustedAttestor,
        versions: vec![crate::StoreItemVersion {
            version: package.version.clone(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: Some(checked.root.clone()),
            size_bytes: bytes.len() as u64,
            runtime: None,
        }],
        publisher_pubkey_hex: Some(checked.publisher.clone()),
        installed: checked.installed_version.as_ref().map_or(
            crate::InstalledState::Not,
            |version| crate::InstalledState::Installed {
                version: version.clone(),
                available: package.version.clone(),
                pinned: checked.pinned,
                user_managed: checked.user_managed,
            },
        ),
    };
    Ok(ImportedPackage {
        expected,
        item,
        registry: String::new(),
        local: Some(checked),
        bytes: bytes.into(),
    })
}

/// The ordinary file/content handoff, resolved only on the package worker.
#[derive(Clone, Debug)]
pub enum Input {
    Path(std::path::PathBuf),
    Content(ui::framework::content::ContentStore, String),
    Bytes(Vec<u8>),
}

impl Input {
    /// Read bounded bytes without granting access beyond the app's file policy.
    ///
    /// # Errors
    /// Returns why the handed file is unavailable or too large.
    pub fn read(self) -> Result<Vec<u8>, String> {
        use std::io::Read as _;
        let mut reader: Box<dyn std::io::Read> = match self {
            Self::Path(path) => Box::new(
                std::fs::File::open(path).map_err(|_| ui::i18n!("store-package-unavailable"))?,
            ),
            Self::Content(content, hash) => Box::new(
                content
                    .open(&hash)
                    .ok_or_else(|| ui::i18n!("store-package-unavailable"))?,
            ),
            Self::Bytes(bytes) => return Ok(bytes),
        };
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take((MAX_IMPORT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ui::i18n!("store-package-unavailable"))?;
        Ok(bytes)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tilo_app_package::DiscardBlobs;

    fn published_bytes(encoded: &str) -> Vec<u8> {
        use base64::Engine as _;
        use std::io::Read as _;
        let packed = base64::engine::general_purpose::STANDARD
            .decode(encoded.lines().collect::<String>())
            .unwrap();
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(packed.as_slice())
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    pub(crate) fn published_fixture() -> (Vec<u8>, StoreItem) {
        let snapshot_bytes = published_bytes(include_str!(
            "../fixtures/published/snapshot379.json.gz.b64"
        ));
        let snapshot = registry_types::CatalogueSnapshot::from_json(&snapshot_bytes).unwrap();
        // Historical fixture time, not a runtime freshness override. The owner
        // anchor is independently recorded in the booted production OS history.
        let entries = app_cli::client::classify_snapshot(
            &snapshot,
            "55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06",
            1_791_214_328,
        )
        .unwrap();
        let verified = entries
            .iter()
            .find(|entry| entry.entry.package_id == "so.tilo.store")
            .unwrap();
        let bytes = published_bytes(include_str!(
            "../fixtures/published/store70738-x86_64.tapp.gz.b64"
        ));
        let offered = verified
            .entry
            .versions
            .iter()
            .find(|version| {
                version.version.starts_with("0.1.70738+")
                    && version.target().unwrap().architecture == "x86_64"
            })
            .and_then(registry_types::RegistryVersion::tapp)
            .unwrap();
        assert_eq!(bytes.len() as u64, offered.tapp_size_bytes);
        assert_eq!(
            ui::framework::content::hash_bytes(&bytes),
            offered.tapp_blake3_hex
        );
        let mut item = crate::sources::tilo_to_store_item(verified, "Tilo historical fixture", &[]);
        item.versions.retain(|version| {
            version.root_hash_hex.as_deref() == Some(offered.tapp_root_hash_hex.as_str())
        });
        assert_eq!(
            item.versions.len(),
            1,
            "the real native target remains offered"
        );
        (bytes, item)
    }

    #[test]
    fn package_import_published_full_and_thin_preserve_actual_offer_size() {
        let (bytes, item) = published_fixture();
        let full = review(bytes.clone(), std::slice::from_ref(&item)).unwrap();
        let decoded = tilo_app_package::tapp::read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
        assert!(
            decoded.is_complete(),
            "the actual public archive carries all payloads"
        );
        let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None).unwrap();
        assert!(thin.len() < bytes.len());
        let thin_review = review(thin, std::slice::from_ref(&item)).unwrap();
        assert_eq!(thin_review.expected, full.expected);
        assert_eq!(full.expected.size_bytes, bytes.len() as u64);
        assert_eq!(full.expected.package_root, decoded.meta.id);
    }

    pub(crate) fn signed_fixture() -> (Vec<u8>, StoreItem) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/app"), b"#!/bin/true\nimport-fixture\n").unwrap();
        let key = tilo_app_package::SigningIdentity::from_seed(&[73; 32]);
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"[package]
id = "so.tilo.import-fixture"
version = "1.0.0"
kind = "app"
name = "Import fixture"
[package.isa]
x86_64 = "baseline"
aarch64 = "armv8-a"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(key.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(dir.path(), &manifest, &key).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let thin = tilo_app_package::tapp::to_bytes(&packed.meta, None).unwrap();
        let item = StoreItem {
            bot: None,
            awaiting_developer: false,
            id: manifest.package.id,
            name: "Import fixture".into(),
            description: "A signed test app".into(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: Vec::new(),
            icon_url: None,
            screenshot_urls: Vec::new(),
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Independent registry".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![crate::StoreItemVersion {
                version: "1.0.0".into(),
                released_at_unix: 0,
                download_url: Some("https://packages.test/import.tapp".into()),
                root_hash_hex: Some(packed.id().to_hex()),
                size_bytes: thin.len() as u64,
                runtime: None,
            }],
            publisher_pubkey_hex: Some(hex::encode(key.public_key_bytes())),
            installed: crate::InstalledState::Not,
        };
        (bytes, item)
    }

    #[test]
    fn import_matches_all_independent_offer_fields_and_preserves_bytes() {
        let (bytes, item) = signed_fixture();
        let reviewed = review(bytes.clone(), std::slice::from_ref(&item)).unwrap();
        assert_eq!(&*reviewed.bytes, &bytes);
        assert_eq!(reviewed.expected.app_id, item.id);
        assert_eq!(reviewed.expected.version, "1.0.0");
        let decoded = tilo_app_package::tapp::read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
        let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None).unwrap();
        assert_eq!(reviewed.expected.size_bytes, thin.len() as u64);
        let thin_review = review(thin, std::slice::from_ref(&item)).unwrap();
        assert_eq!(thin_review.expected, reviewed.expected);
        assert_eq!(reviewed.registry, "Independent registry");
        for changed in 0..6 {
            let mut wrong = item.clone();
            match changed {
                0 => wrong.id = "so.tilo.other".into(),
                1 => wrong.versions[0].version = "2.0.0".into(),
                2 => wrong.versions[0].root_hash_hex = Some("00".repeat(32)),
                3 => wrong.versions[0].size_bytes = 0,
                4 => wrong.publisher_pubkey_hex = Some("11".repeat(32)),
                _ => wrong.trust = StoreTrust::ConfiguredRemote,
            }
            assert!(review(bytes.clone(), &[wrong]).is_err(), "field {changed}");
        }
        let mut oversized_offer = item;
        oversized_offer.versions[0].size_bytes = tilo_app_package::MAX_APP_TAPP_BYTES + 1;
        assert!(review(bytes.clone(), &[oversized_offer]).is_err());
        assert!(
            review(bytes, &[]).is_err(),
            "archive key does not authorize itself"
        );
    }

    #[test]
    fn import_rejects_actual_corrupt_payload_and_unavailable_handoffs() {
        let (mut bytes, item) = signed_fixture();
        let pos = bytes
            .windows(b"#!/bin/true\nimport-fixture\n".len())
            .position(|s| s == b"#!/bin/true\nimport-fixture\n")
            .unwrap();
        bytes[pos] ^= 1;
        assert!(
            review(bytes, &[item]).is_err(),
            "real archive payload corruption"
        );
        assert!(review(vec![], &[]).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(Input::Path(dir.path().join("missing.tapp")).read().is_err());
        assert!(
            Input::Content(
                ui::framework::content::ContentStore::memory(),
                "ab".repeat(32)
            )
            .read()
            .is_err()
        );
    }
}

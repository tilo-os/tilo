//! Local package review against the same independently verified Store offers, or against the
//! workspace's trust for an archive no catalogue lists. An archive can describe itself; it cannot
//! authorize its publisher or identity. Installing a reviewed archive hands its file to the
//! workspace and names that version on its tree, which every device then installs, checking it
//! again.

use crate::resource::CATALOG;
use std::sync::Arc;

use tilo_app_package::review::{ArchiveReviewError, AttestedReview, InstallExpectation};
use tilo_app_package::trust::{
    AttestorScope, RevocationList, RevokedKey, TrustedAttestor, TrustedAttestors,
};

use crate::{StoreItem, StoreSource, StoreTrust};

pub const MAX_IMPORT_BYTES: usize = tilo_app_package::review::MAX_REVIEWED_BYTES;

/// Immutable bytes and independent catalogue identity retained until approval.
#[derive(Clone, Debug)]
pub struct ImportedPackage {
    pub bytes: Arc<[u8]>,
    pub item: StoreItem,
    pub expected: InstallExpectation,
    pub registry: String,
    /// The review against the workspace's trust, for an archive no catalogue lists.
    pub local: Option<AttestedReview>,
}

impl ImportedPackage {
    /// The version the workspace's install names for these bytes, handed over as `file` (their
    /// BLAKE3, hex): its app, version, surface, package root and publisher.
    ///
    /// # Errors
    /// The package's surface cannot be read.
    pub fn release(
        &self,
        file: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let surface = app_registry::publisher::entry::surface_hex(&self.bytes)
            .map_err(|_| CATALOG.t("store-package-invalid"))?;
        let mut release = serde_json::Map::new();
        release.insert("app".into(), self.expected.app_id.clone().into());
        release.insert("version".into(), self.expected.version.clone().into());
        release.insert("surface".into(), surface.into());
        release.insert("package".into(), self.expected.package_root.to_hex().into());
        release.insert(
            "publisher".into(),
            hex::encode(self.expected.publisher).into(),
        );
        release.insert("file".into(), file.to_owned().into());
        Ok(release)
    }
}

pub fn review(bytes: Vec<u8>, offers: &[StoreItem]) -> Result<ImportedPackage, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMPORT_BYTES {
        return Err(CATALOG.t("store-package-size-refused"));
    }
    let archive =
        tilo_app_package::review::review_archive(&bytes).map_err(|error| match error {
            ArchiveReviewError::Invalid => CATALOG.t("store-package-invalid"),
            ArchiveReviewError::NotApplication => CATALOG.t("store-package-app-only"),
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
    Err(CATALOG.t("store-package-offer-required"))
}

/// The workspace's trust as a package's verification takes it.
///
/// # Errors
/// An attestor's scope is not one a package can name.
fn trust_lists(
    trust: &app_cli::config::TrustList,
) -> Result<(TrustedAttestors, RevocationList), String> {
    let attestor = trust
        .attestors
        .iter()
        .map(|attestor| {
            Ok(TrustedAttestor {
                id: attestor.id.clone(),
                pubkey_hex: attestor.pubkey_hex.clone(),
                scope: AttestorScope::parse(&attestor.scope).map_err(|e| e.to_string())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let revoked = trust
        .revoked
        .iter()
        .map(|key| RevokedKey {
            pubkey_hex: key.pubkey_hex.clone(),
            reason: key.reason.clone(),
        })
        .collect();
    Ok((TrustedAttestors { attestor }, RevocationList { revoked }))
}

/// Review an archive no catalogue lists against the workspace's trust (`trust`): an attestor it
/// already trusts must have signed it. `installed` says what the workspace and this device
/// already have of the app. `unlisted` is what to say when the workspace trusts no attestor at
/// all, so nothing but a catalogue offer could vouch for the archive.
///
/// # Errors
/// The archive is not attested by a trusted attestor, is revoked, or is malformed; never supplies
/// a publisher trust key.
pub fn review_local(
    bytes: Vec<u8>,
    trust: &app_cli::config::TrustList,
    installed: &crate::sources::Installed,
    unlisted: String,
) -> Result<ImportedPackage, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMPORT_BYTES {
        return Err(CATALOG.t("store-package-size-refused"));
    }
    let (attestors, revoked) = trust_lists(trust)?;
    if attestors.attestor.is_empty() {
        return Err(unlisted);
    }
    let checked = tilo_app_package::review::review_attested(&bytes, &attestors, &revoked)?;
    let package = &checked.manifest.package;
    let expected = checked
        .expectation(bytes.len() as u64)
        .map_err(|_| CATALOG.t("store-package-invalid"))?;
    let item = StoreItem {
        bot: None,
        awaiting_developer: false,
        listing: crate::model::Listing::default(),
        id: package.id.clone(),
        name: package.name.clone().unwrap_or_else(|| package.id.clone()),
        description: CATALOG.t("store-package-local-description"),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: Vec::new(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Tilo {
            registry_name: CATALOG.t("store-package-local-source"),
        },
        trust: StoreTrust::TrustedAttestor,
        versions: vec![crate::StoreItemVersion {
            version: package.version.clone(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: Some(checked.root.clone()),
            surface_hex: None,
            size_bytes: bytes.len() as u64,
            runtime: None,
        }],
        publisher_pubkey_hex: Some(checked.publisher.clone()),
        installed: installed.state(&package.id, &package.version),
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
    pub fn read(self) -> Result<Vec<u8>, String> {
        use std::io::Read as _;
        let mut reader: Box<dyn std::io::Read> = match self {
            Self::Path(path) => Box::new(
                std::fs::File::open(path).map_err(|_| CATALOG.t("store-package-unavailable"))?,
            ),
            Self::Content(content, hash) => Box::new(
                content
                    .open(&hash)
                    .ok_or_else(|| CATALOG.t("store-package-unavailable"))?,
            ),
            Self::Bytes(bytes) => return Ok(bytes),
        };
        let mut bytes = Vec::new();
        reader
            .by_ref()
            .take((MAX_IMPORT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| CATALOG.t("store-package-unavailable"))?;
        Ok(bytes)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tilo_app_package::DiscardBlobs;

    fn native_target() -> (&'static str, &'static str, &'static str) {
        #[cfg(target_arch = "aarch64")]
        {
            ("aarch64", "armv8-a", "aarch64")
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            ("x86_64", "baseline", "x86_64")
        }
    }

    /// A complete archive offered the way the registry offers one: an
    /// owner-signed catalogue lists it with a publisher-signed entry whose
    /// size and hash are the full archive's, and the Store's offer comes from
    /// verifying that catalogue, not from the test.
    pub(crate) fn catalogue_fixture() -> (Vec<u8>, StoreItem) {
        use ed25519_dalek::SigningKey;
        let (arch, isa, target_isa) = native_target();
        let publisher = SigningKey::from_bytes(&[74; 32]);
        let identity = tilo_app_package::SigningIdentity::from_signing_key(publisher.clone());
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(
            dir.path().join("bin/app"),
            b"#!/bin/true\ncatalogue-fixture\n",
        )
        .unwrap();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"[package]
id = "so.tilo.catalogue-fixture"
version = "1.2.0"
kind = "app"
name = "Catalogue fixture"
[package.isa]
{arch} = "{isa}"
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
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let mut entry = ui::registry::RegistryEntry {
            package_id: manifest.package.id,
            kind: ui::registry::EntryKind::App,
            name: "Catalogue fixture".into(),
            description: "A signed test app".into(),
            tagline: "Signed and listed".into(),
            images: ui::registry::LocalizedImages::default(),
            supported_languages: vec!["en".into()],
            categories: Vec::new(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            versions: vec![ui::registry::RegistryVersion {
                version: "1.2.0".into(),
                released_at_unix: 1,
                artifact: ui::registry::VersionArtifact::Tapp(ui::registry::TappArtifact {
                    tapp_url: "https://packages.test/catalogue-fixture.tapp".into(),
                    tapp_root_hash_hex: packed.id().to_hex(),
                    tapp_size_bytes: bytes.len() as u64,
                    tapp_blake3_hex: ui::framework::content::hash_bytes(&bytes),
                    tapp_surface_hex: String::new(),
                    target: ui::registry::RegistryTarget {
                        platform: ui::registry::TILO_OS_PLATFORM.into(),
                        architecture: arch.into(),
                        isa: target_isa.into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 2,
            signature_hex: String::new(),
        };
        ui::registry::sign_entry(&mut entry, &publisher);
        let owner = SigningKey::from_bytes(&[75; 32]);
        let now = 1_800_000_000;
        let mut snapshot = ui::registry::SignedRegistrySnapshot {
            protocol: ui::registry::PACKAGE_FORMAT_VERSION,
            sequence: 1,
            issued_at_unix: now,
            expires_at_unix: now + 3600,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![ui::registry::PublisherAuthorization {
                pubkey_hex: entry.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![entry],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        ui::registry::sign_snapshot(&mut snapshot, &owner).unwrap();
        let entries = app_cli::client::classify_snapshot(
            &ui::registry::CatalogueSnapshot::Full(snapshot),
            &hex::encode(owner.verifying_key().to_bytes()),
            now,
        )
        .unwrap();
        let item = crate::sources::tilo_to_store_item(&entries[0], "Signed catalogue", &[]);
        assert_eq!(item.versions.len(), 1, "the native build is offered");
        assert_eq!(item.versions[0].size_bytes, bytes.len() as u64);
        (bytes, item)
    }

    tilo_test_env::scenario! {
        fn a_package_import_is_reviewed_against_its_offer;

        phase package_import_offered_full_and_thin_preserve_actual_offer_size;
        {
            let (bytes, item) = catalogue_fixture();
            let full = review(bytes.clone(), std::slice::from_ref(&item)).unwrap();
            let decoded = tilo_app_package::tapp::read(bytes.as_slice(), &mut DiscardBlobs).unwrap();
            assert!(
                decoded.is_complete(),
                "the offered archive carries all payloads"
            );
            let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None).unwrap();
            assert!(thin.len() < bytes.len());
            let thin_review = review(thin, std::slice::from_ref(&item)).unwrap();
            assert_eq!(thin_review.expected, full.expected);
            assert_eq!(full.expected.size_bytes, bytes.len() as u64);
            assert_eq!(full.expected.package_root, decoded.meta.id);
        }

        phase import_matches_all_independent_offer_fields_and_preserves_bytes;
        {
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

        phase import_rejects_actual_corrupt_payload_and_unavailable_handoffs;
        {
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
            listing: crate::model::Listing::default(),
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
                surface_hex: None,
                size_bytes: thin.len() as u64,
                runtime: None,
            }],
            publisher_pubkey_hex: Some(hex::encode(key.public_key_bytes())),
            installed: crate::InstalledState::Not,
        };
        (bytes, item)
    }
}

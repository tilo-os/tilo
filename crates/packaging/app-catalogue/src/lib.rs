//! `app-catalogue` — what software this machine can open but has not
//! installed yet, and how to get it.
//!
//! The desktop's applications list is "what is in the `tilo-install`
//! registry" — already installed. This crate is the other half: what
//! the machine's configured registries ([`config::load_registries_no_seed`],
//! the user's `registries.toml` — nothing bundled or seeded) describe,
//! minus what's already installed. No registries configured means an
//! empty catalogue, correctly.
//!
//! [`main`](../src/main.rs) builds `tilo-catalogue`, and the compositor
//! shells out to it rather than linking this crate — listing means
//! HTTP, installing means HTTP plus signature verification plus
//! archive extraction, none of which belongs in the most privileged
//! process (same precedent as the desktop execing `tilo-launcher`
//! instead of building sandboxes itself).
//!
//! This crate never decides whether bytes are safe to extract:
//! [`install::install`] downloads to a staging file and hands the
//! path to `tilo-install`, which checks the publisher signature
//! fail-closed before extracting anything; `--allow-self-signed` is never passed.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod client;
pub mod config;
pub mod install;

use serde::{Deserialize, Serialize};

pub use client::{TrustLevel, VerifiedEntry};
pub use config::{RegistriesFile, RegistryConfig};

/// One application a registry offers that is not installed here.
///
/// The wire shape `tilo-catalogue list --json` prints and the
/// compositor parses — field names are a contract between the two.
/// Everything on it is metadata a signed registry entry carried.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueApp {
    /// Reverse-DNS package id — the same identity `tilo-install` records once this is installed.
    pub package_id: String,
    /// The publisher's name for it.
    pub display_name: String,
    /// The publisher's one-line description.
    #[serde(default)]
    pub description: String,
    /// First declared category, when any; matches an installed app's single `category`.
    #[serde(default)]
    pub category: Option<String>,
    /// The newest version the registry advertises.
    pub version: String,
    /// Which configured registry offered it.
    pub registry: String,
    /// Where the `.qpkg` is fetched from.
    pub download_url: String,
    /// Signed-root/package-id hash, covered by the registry signature.
    #[serde(default)]
    pub qpkg_root_hash_hex: String,
    /// Expected download size, covered by the registry signature.
    #[serde(default)]
    pub qpkg_size_bytes: u64,
    /// BLAKE3 of the exact downloaded `.qpkg` bytes.
    #[serde(default)]
    pub qpkg_blake3_hex: String,
    /// Signed execution target metadata.
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub isa: String,
    #[serde(default)]
    pub runtime: Option<String>,
    /// Publisher key the downloaded qpkg must be signed by.
    #[serde(default)]
    pub publisher_pubkey_hex: String,
    /// `"publisher-signed"` means publisher and pinned-owner snapshot proofs
    /// both verified. Untrusted entries never become a `CatalogueApp`.
    pub trust: String,
}

/// The JSON envelope `tilo-catalogue list --json` prints.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    /// Registries configured but unreadable, with the reason — reported
    /// rather than swallowed so a caller can tell "nothing available"
    /// from "we couldn't find out".
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
}

/// Turn a verified registry entry into a catalogue row, or `None` if
/// it can't be offered: its metadata signature didn't verify, or it
/// advertises no version to download.
pub fn entry_to_app(v: &VerifiedEntry, registry_name: &str) -> Option<CatalogueApp> {
    if !v.trust.is_offerable() {
        return None;
    }
    entry_to_app_for_channel(v, registry_name, pkg::version::ReleaseChannel::Stable)
}

/// Select versions within the user's explicit release channel.
pub fn entry_to_app_for_channel(
    v: &VerifiedEntry,
    registry_name: &str,
    channel: pkg::version::ReleaseChannel,
) -> Option<CatalogueApp> {
    if !v.trust.is_offerable() {
        return None;
    }
    // Artifact version orders releases. A later publication timestamp must
    // not make an older semantic/CalVer artifact the offered update.
    let newest = v
        .entry
        .versions
        .iter()
        .filter(|version| {
            !version.qpkg_url.trim().is_empty()
                && target_supported(version)
                && channel.accepts(&version.version)
        })
        .max_by(|a, b| pkg::version::compare(&a.version, &b.version))?;
    let target = newest.target.as_ref()?;
    let display_name = if v.entry.name.trim().is_empty() {
        launcher::registry::display_name_for_package_id(&v.entry.package_id)
    } else {
        v.entry.name.clone()
    };
    Some(CatalogueApp {
        package_id: v.entry.package_id.clone(),
        display_name,
        description: v.entry.description.clone(),
        category: v
            .entry
            .categories
            .iter()
            .find(|c| !c.trim().is_empty())
            .cloned(),
        version: newest.version.clone(),
        registry: registry_name.to_string(),
        download_url: newest.qpkg_url.clone(),
        qpkg_root_hash_hex: newest.qpkg_root_hash_hex.clone(),
        qpkg_size_bytes: newest.qpkg_size_bytes,
        qpkg_blake3_hex: newest.qpkg_blake3_hex.clone()?,
        architecture: target.architecture.clone(),
        isa: target.isa.clone(),
        runtime: target.runtime.clone(),
        publisher_pubkey_hex: v.entry.publisher_pubkey_hex.clone(),
        trust: match v.trust {
            TrustLevel::PublisherSigned => "publisher-signed".into(),
            // Unreachable — filtered above. Kept total rather than panicking on a shape change.
            TrustLevel::Untrusted => "untrusted".into(),
        },
    })
}

fn target_supported(version: &registry_types::RegistryVersion) -> bool {
    let Some(target) = &version.target else {
        return false;
    };
    if target.architecture != std::env::consts::ARCH {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        match target.isa.as_str() {
            "x86_64" => true,
            "x86_64-v2" => is_x86_feature_detected!("sse4.2") && is_x86_feature_detected!("popcnt"),
            "x86_64-v3" => {
                is_x86_feature_detected!("avx2")
                    && is_x86_feature_detected!("bmi2")
                    && is_x86_feature_detected!("fma")
            }
            "x86_64-v4" => {
                is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw")
            }
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        #[cfg(target_arch = "aarch64")]
        {
            match target.isa.as_str() {
                "aarch64" => true,
                "aarch64-v8.2" => std::arch::is_aarch64_feature_detected!("dotprod"),
                "aarch64-v8.4" => std::arch::is_aarch64_feature_detected!("i8mm"),
                "aarch64-v9" => std::arch::is_aarch64_feature_detected!("sve2"),
                _ => false,
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            false
        }
    }
}

/// The package ids `tilo-install` has already installed here, read
/// through the launcher's registry reader (same file, same
/// `TILO_APPS_DATA_DIR` override) so the two can't disagree about what's present.
pub fn installed_ids() -> Vec<String> {
    launcher::registry::list_installed()
        .map(|v| v.into_iter().map(|e| e.id).collect())
        .unwrap_or_default()
}

/// Subtract what is installed, and collapse duplicate ids. Registries
/// are consulted in file order, so the first that offers an id wins —
/// the user's ordering is the tie-break.
pub fn dedupe_and_subtract(apps: Vec<CatalogueApp>, installed: &[String]) -> Vec<CatalogueApp> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for app in apps {
        if installed.iter().any(|i| i == &app.package_id) {
            continue;
        }
        if seen.iter().any(|s| s == &app.package_id) {
            continue;
        }
        seen.push(app.package_id.clone());
        out.push(app);
    }
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    out
}

/// Everything the configured registries offer that is not installed.
/// Blocking, but enabled registries are fetched concurrently so one slow
/// mirror does not serially delay every healthy source. Results are still
/// folded in configuration order, preserving the user's registry priority.
pub fn available() -> CatalogueListing {
    let registries = config::load_registries_no_seed();
    let enabled: Vec<RegistryConfig> = registries.enabled().cloned().collect();
    #[allow(
        clippy::needless_collect,
        reason = "all registry workers must be spawned before the first join; joining the mapped iterator directly would serialize the HTTP requests"
    )]
    let fetched = std::thread::scope(|scope| {
        let handles: Vec<_> = enabled
            .iter()
            .map(|registry| scope.spawn(move || client::fetch_entries(registry)))
            .collect();
        handles
            .into_iter()
            .map(std::thread::ScopedJoinHandle::join)
            .collect::<Vec<_>>()
    });
    let mut apps = Vec::new();
    let mut unreachable = Vec::new();
    for (reg, result) in enabled.iter().zip(fetched) {
        match result {
            Err(_) => unreachable.push(UnreachableRegistry {
                name: reg.name.clone(),
                detail: "registry worker stopped unexpectedly".to_string(),
            }),
            Ok(result) => match result {
                Ok(entries) => {
                    for v in &entries {
                        if let Some(app) = entry_to_app_for_channel(v, &reg.name, reg.channel) {
                            apps.push(app);
                        }
                    }
                }
                Err(e) => unreachable.push(UnreachableRegistry {
                    name: reg.name.clone(),
                    detail: e.to_string(),
                }),
            },
        }
    }
    CatalogueListing {
        apps: dedupe_and_subtract(apps, &installed_ids()),
        unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::{RegistryEntry, RegistryVersion, sign_entry};

    fn native_baseline_isa() -> &'static str {
        #[cfg(target_arch = "x86_64")]
        {
            "x86_64"
        }
        #[cfg(target_arch = "aarch64")]
        {
            "aarch64"
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            "unsupported"
        }
    }

    fn entry(id: &str, name: &str, categories: Vec<&str>) -> RegistryEntry {
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let mut e = RegistryEntry {
            package_id: id.into(),
            name: name.into(),
            description: "does a thing".into(),
            categories: categories.into_iter().map(str::to_string).collect(),
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "2.0.0".into(),
                qpkg_url: format!("https://apps.example/{id}.qpkg"),
                qpkg_root_hash_hex: "ab".repeat(32),
                qpkg_size_bytes: 10,
                released_at_unix: 5,
                qpkg_blake3_hex: Some("ef".repeat(32)),
                target: Some(registry_types::RegistryTarget {
                    architecture: std::env::consts::ARCH.into(),
                    isa: native_baseline_isa().into(),
                    runtime: None,
                }),
            }],
            updated_at_unix: 7,
            signature_hex: String::new(),
        };
        sign_entry(&mut e, &key);
        e
    }

    fn verified(e: RegistryEntry) -> VerifiedEntry {
        let owner = ed25519_dalek::SigningKey::from_bytes(&[10; 32]);
        let mut snapshot = registry_types::SignedRegistrySnapshot {
            protocol: registry_types::SNAPSHOT_PROTOCOL,
            sequence: 1,
            issued_at_unix: 10,
            expires_at_unix: 20,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![registry_types::PublisherAuthorization {
                pubkey_hex: e.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![e.clone()],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        registry_types::sign_snapshot(&mut snapshot, &owner).unwrap();
        client::classify_snapshot(
            &snapshot,
            &hex::encode(owner.verifying_key().to_bytes()),
            15,
        )
        .map_or(
            VerifiedEntry {
                entry: e,
                trust: TrustLevel::Untrusted,
            },
            |mut entries| entries.remove(0),
        )
    }

    #[test]
    fn a_signed_entry_becomes_a_row_carrying_the_publishers_own_metadata() {
        let v = verified(entry("dev.tilo.sheets", "Sheets", vec!["Office"]));
        let app = entry_to_app(&v, "main").expect("an offerable entry");
        assert_eq!(app.package_id, "dev.tilo.sheets");
        assert_eq!(app.display_name, "Sheets");
        assert_eq!(app.category.as_deref(), Some("Office"));
        assert_eq!(app.version, "2.0.0");
        assert_eq!(app.registry, "main");
        assert_eq!(
            app.download_url,
            "https://apps.example/dev.tilo.sheets.qpkg"
        );
        assert_eq!(app.trust, "publisher-signed");
    }

    /// An entry whose signature does not cover its own metadata must not
    /// become something a user can activate — the name and download URL would be unattributed.
    #[test]
    fn a_tampered_entry_never_becomes_a_row() {
        let mut e = entry("dev.tilo.sheets", "Sheets", vec![]);
        e.name = "Totally Legit Banking".into();
        let v = verified(e);
        assert_eq!(v.trust, TrustLevel::Untrusted);
        assert!(entry_to_app(&v, "main").is_none());
    }

    #[test]
    fn an_entry_with_no_downloadable_version_is_not_offered() {
        let mut e = entry("dev.tilo.ghost", "Ghost", vec![]);
        e.versions.clear();
        sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
        assert!(entry_to_app(&verified(e), "main").is_none());

        let mut blank = entry("dev.tilo.blank", "Blank", vec![]);
        blank.versions[0].qpkg_url = "  ".into();
        sign_entry(
            &mut blank,
            &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]),
        );
        assert!(entry_to_app(&verified(blank), "main").is_none());
    }

    #[test]
    fn catalogue_selects_latest_release_independent_of_registry_array_order() {
        let mut e = entry("dev.tilo.timeline", "Timeline", vec![]);
        e.versions = vec![
            RegistryVersion {
                version: "1.0.0".into(),
                qpkg_url: "https://apps.example/timeline-1.qpkg".into(),
                qpkg_root_hash_hex: "11".repeat(32),
                qpkg_size_bytes: 10,
                released_at_unix: 100,
                qpkg_blake3_hex: Some("ef".repeat(32)),
                target: Some(registry_types::RegistryTarget {
                    architecture: std::env::consts::ARCH.into(),
                    isa: native_baseline_isa().into(),
                    runtime: None,
                }),
            },
            RegistryVersion {
                version: "2027.01".into(),
                qpkg_url: "https://apps.example/timeline-new.qpkg".into(),
                qpkg_root_hash_hex: "22".repeat(32),
                qpkg_size_bytes: 20,
                released_at_unix: 200,
                qpkg_blake3_hex: Some("fe".repeat(32)),
                target: Some(registry_types::RegistryTarget {
                    architecture: std::env::consts::ARCH.into(),
                    isa: native_baseline_isa().into(),
                    runtime: None,
                }),
            },
        ];
        sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));

        let app = entry_to_app(&verified(e), "main").expect("offerable latest release");
        assert_eq!(app.version, "2027.01");
        assert_eq!(app.download_url, "https://apps.example/timeline-new.qpkg");
        assert_eq!(app.qpkg_size_bytes, 20);
    }

    #[test]
    fn catalogue_channels_separate_stable_and_continuous_updates() {
        use pkg::version::ReleaseChannel;
        let mut e = entry("dev.tilo.channel-test", "Channel", vec![]);
        let template = e.versions[0].clone();
        e.versions = ["0.1.0", "0.1.1-dev.9+gffff", "0.1.1-dev.10+gaaaa"]
            .into_iter()
            .map(|version| RegistryVersion {
                version: version.into(),
                ..template.clone()
            })
            .collect();
        sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
        let current = verified(e.clone());
        assert_eq!(entry_to_app(&current, "main").unwrap().version, "0.1.0");
        assert_eq!(
            entry_to_app_for_channel(&current, "main", ReleaseChannel::Development)
                .unwrap()
                .version,
            "0.1.1-dev.10+gaaaa"
        );
        e.versions.push(RegistryVersion {
            version: "0.1.1".into(),
            ..template
        });
        sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
        let promoted = verified(e);
        for channel in [ReleaseChannel::Stable, ReleaseChannel::Development] {
            assert_eq!(
                entry_to_app_for_channel(&promoted, "main", channel)
                    .unwrap()
                    .version,
                "0.1.1"
            );
        }
    }

    #[test]
    fn a_nameless_entry_falls_back_to_the_same_derivation_installed_apps_use() {
        let mut e = entry("dev.tilo.hex-editor", "", vec![]);
        sign_entry(&mut e, &ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]));
        let app = entry_to_app(&verified(e), "main").expect("offerable");
        assert_eq!(app.display_name, "Hex Editor");
    }

    /// The wire contract with the compositor, pinned by exact field name.
    /// A rename here without a matching change there would silently
    /// produce an empty catalogue rather than an error — both tests must be edited together.
    #[test]
    fn the_json_envelope_field_names_are_the_contract() {
        let listing = CatalogueListing {
            apps: vec![CatalogueApp {
                package_id: "dev.tilo.sheets".into(),
                display_name: "Sheets".into(),
                description: "a spreadsheet".into(),
                category: Some("Office".into()),
                version: "2.0.0".into(),
                registry: "main".into(),
                download_url: "https://x/y.qpkg".into(),
                qpkg_root_hash_hex: "ab".repeat(32),
                qpkg_size_bytes: 10,
                qpkg_blake3_hex: "ef".repeat(32),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
                publisher_pubkey_hex: "cd".repeat(32),
                trust: "verified".into(),
            }],
            unreachable: vec![UnreachableRegistry {
                name: "mirror".into(),
                detail: "connection refused".into(),
            }],
        };
        let json = serde_json::to_string(&listing).expect("encodes");
        assert_eq!(
            json,
            r#"{"apps":[{"package_id":"dev.tilo.sheets","display_name":"Sheets","description":"a spreadsheet","category":"Office","version":"2.0.0","registry":"main","download_url":"https://x/y.qpkg","qpkg_root_hash_hex":"abababababababababababababababababababababababababababababababab","qpkg_size_bytes":10,"qpkg_blake3_hex":"efefefefefefefefefefefefefefefefefefefefefefefefefefefefefefefef","architecture":"x86_64","isa":"x86_64-v3","runtime":null,"publisher_pubkey_hex":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd","trust":"verified"}],"unreachable":[{"name":"mirror","detail":"connection refused"}]}"#
        );
    }

    #[test]
    fn installed_packages_are_subtracted() {
        let a = entry_to_app(&verified(entry("dev.tilo.a", "A", vec![])), "main").unwrap();
        let b = entry_to_app(&verified(entry("dev.tilo.b", "B", vec![])), "main").unwrap();
        let out = dedupe_and_subtract(vec![a, b], &["dev.tilo.a".to_string()]);
        assert_eq!(out.len(), 1, "the installed one is gone: {out:?}");
        assert_eq!(out[0].package_id, "dev.tilo.b");
    }

    #[test]
    fn the_first_registry_offering_an_id_wins() {
        let first = entry_to_app(&verified(entry("dev.tilo.a", "A", vec![])), "primary").unwrap();
        let second = entry_to_app(&verified(entry("dev.tilo.a", "A", vec![])), "mirror").unwrap();
        let out = dedupe_and_subtract(vec![first, second], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].registry, "primary",
            "the user's registry ordering breaks the tie"
        );
    }
}

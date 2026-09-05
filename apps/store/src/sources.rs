//! Merger: qOS registry entries + Flathub records + local install
//! snapshot → one canonical [`StoreItem`] list.
//!
//! The merge rule the task calls out: **a qOS entry with the same
//! `name` as a Flathub app wins.** Duplicates on the Flathub side
//! are dropped. Case-insensitive matching, whitespace-trimmed.

use registry_types::RegistryVersion;

use crate::flathub_client::{AppStreamRecord, PopularEntry};
use crate::installed::InstalledEntry;
use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};
use crate::registry_client::VerifiedEntry;

/// Convert a qOS registry entry into the unified store item shape.
///
/// `registry_name` is the human label the store card shows (from
/// the user's `registries.toml`, not the wire — the registry
/// doesn't publish its own name to a fetched entry).
///
/// `installed_index` is the pre-indexed install snapshot from
/// [`crate::installed`] — passed by-ref so the merge can cheaply
/// look up install state per package id.
#[allow(
    clippy::unreachable,
    reason = "VerifiedEntry is constructed only after the registry verifier rejects Untrusted; reaching this arm would violate that security boundary"
)]
pub fn qos_to_store_item(
    verified: &VerifiedEntry,
    registry_name: &str,
    installed_index: &[InstalledEntry],
) -> StoreItem {
    let entry = &verified.entry;
    let versions: Vec<StoreItemVersion> = entry.versions.iter().map(qos_version_to_store).collect();
    let latest = versions
        .first()
        .map(|v| v.version.clone())
        .unwrap_or_default();
    let installed = installed_index
        .iter()
        .find(|e| e.id == entry.package_id)
        .map_or(InstalledState::Not, |e| InstalledState::Installed {
            version: e.version.clone(),
            available: latest.clone(),
            pinned: e.pinned,
        });
    StoreItem {
        id: entry.package_id.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        categories: entry.categories.clone(),
        icon_url: entry.icon_url.clone(),
        screenshot_urls: Vec::new(),
        homepage_url: entry.homepage.clone(),
        source_url: entry.source_url.clone(),
        install_ref_path: None,
        source: StoreSource::Qos {
            registry_name: registry_name.to_string(),
        },
        trust: match verified.trust {
            crate::registry_client::TrustLevel::PublisherSigned => StoreTrust::PublisherSigned,
            crate::registry_client::TrustLevel::Untrusted => {
                unreachable!("untrusted registry entry reached Store conversion")
            }
        },
        versions,
        publisher_pubkey_hex: Some(entry.publisher_pubkey_hex.clone()),
        installed,
    }
}

fn qos_version_to_store(v: &RegistryVersion) -> StoreItemVersion {
    StoreItemVersion {
        version: v.version.clone(),
        released_at_unix: v.released_at_unix,
        download_url: Some(v.qpkg_url.clone()),
        root_hash_hex: Some(v.qpkg_root_hash_hex.clone()),
        size_bytes: v.qpkg_size_bytes,
    }
}

/// Convert a Flathub popular entry (id-only) into a placeholder
/// store item — the store shows the id + a "Loading…" description
/// until a follow-up `fetch_appstream` populates the record.
///
/// `installed_flatpaks` is the `(app_id, version)` snapshot from
/// [`crate::flathub_client::installed_flatpaks`].
pub fn flathub_placeholder_to_item(
    p: &PopularEntry,
    installed_flatpaks: &[(String, String)],
) -> StoreItem {
    let installed = installed_flatpaks
        .iter()
        .find(|(id, _)| id == &p.app_id)
        .map_or(InstalledState::Not, |(_, v)| InstalledState::Installed {
            version: v.clone(),
            available: String::new(),
            pinned: false,
        });
    StoreItem {
        id: p.app_id.clone(),
        name: p.name.clone().unwrap_or_else(|| p.app_id.clone()),
        description: String::new(),
        categories: Vec::new(),
        icon_url: None,
        screenshot_urls: Vec::new(),
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: String::new(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
        }],
        publisher_pubkey_hex: None,
        installed,
    }
}

/// Turn a full appstream record into a store item (the enriched
/// form used once we've fetched details).
pub fn flathub_record_to_item(
    r: &AppStreamRecord,
    installed_flatpaks: &[(String, String)],
) -> StoreItem {
    let categories: Vec<String> = r
        .categories
        .iter()
        .filter(|c| !c.name.trim().is_empty())
        .map(|c| c.name.clone())
        .collect();
    let latest = r.current_release_version.clone().unwrap_or_default();
    let installed = installed_flatpaks
        .iter()
        .find(|(id, _)| id == &r.app_id)
        .map_or(InstalledState::Not, |(_, v)| InstalledState::Installed {
            version: v.clone(),
            available: latest.clone(),
            pinned: false,
        });
    let homepage_url = r
        .homepage
        .clone()
        .or_else(|| r.urls.as_ref().and_then(|u| u.homepage.clone()));
    StoreItem {
        id: r.app_id.clone(),
        name: if r.name.is_empty() {
            r.app_id.clone()
        } else {
            r.name.clone()
        },
        description: if r.description.is_empty() {
            r.summary.clone()
        } else {
            plain_text(&r.description)
        },
        categories,
        icon_url: r.icon.clone(),
        screenshot_urls: r
            .screenshots
            .iter()
            .filter_map(|shot| {
                shot.img_desktop_url
                    .clone()
                    .or_else(|| shot.img_mobile_url.clone())
            })
            .take(6)
            .collect(),
        homepage_url,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: latest,
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
        }],
        publisher_pubkey_hex: None,
        installed,
    }
}

fn plain_text(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output.trim().to_string()
}

/// Merge a qOS pile and a Flathub pile into one deduped list.
/// qOS wins on any name collision (case-insensitive).
pub fn merge(qos_items: Vec<StoreItem>, flathub_items: Vec<StoreItem>) -> Vec<StoreItem> {
    let mut out = qos_items;
    let qos_names: std::collections::HashSet<String> = out
        .iter()
        .map(|i| i.name.trim().to_ascii_lowercase())
        .collect();
    for fh in flathub_items {
        if !qos_names.contains(&fh.name.trim().to_ascii_lowercase()) {
            out.push(fh);
        }
    }
    out
}

/// Distinct categories across a set of items, alpha-sorted, useful
/// for driving the derived category chip row.
pub fn distinct_categories(items: &[StoreItem]) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for item in items {
        for c in &item.categories {
            let t = c.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry_client::TrustLevel;
    use ed25519_dalek::SigningKey;
    use registry_types::{RegistryEntry, sign_entry};

    fn qos_entry(id: &str, name: &str, version: &str, publisher: [u8; 32]) -> RegistryEntry {
        let mut e = RegistryEntry {
            package_id: id.into(),
            name: name.into(),
            description: format!("Description of {name}"),
            categories: vec!["Productivity".into()],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher),
            versions: vec![RegistryVersion {
                version: version.into(),
                qpkg_url: format!("https://apps.qos.dev/{id}/{version}.qpkg"),
                qpkg_root_hash_hex: "cd".repeat(32),
                qpkg_size_bytes: 100,
                released_at_unix: 200,
            }],
            updated_at_unix: 300,
            signature_hex: String::new(),
        };
        let key = SigningKey::from_bytes(&publisher);
        // Update the pubkey hex to match the SigningKey's public half.
        e.publisher_pubkey_hex = hex::encode(key.verifying_key().to_bytes());
        sign_entry(&mut e, &key);
        e
    }

    fn verified(entry: RegistryEntry) -> VerifiedEntry {
        VerifiedEntry {
            entry,
            trust: TrustLevel::PublisherSigned,
        }
    }

    #[test]
    fn qos_entry_deserializes_installed_state() {
        let entry = qos_entry("dev.qos.notes", "Notes", "0.1.0", [1u8; 32]);
        let installed = vec![InstalledEntry {
            id: "dev.qos.notes".into(),
            version: "0.1.0".into(),
            publisher_key_hex: entry.publisher_pubkey_hex.clone(),
            installed_at: 0,
            pinned: true,
        }];
        let item = qos_to_store_item(&verified(entry), "qOS main", &installed);
        assert!(matches!(item.installed, InstalledState::Installed { .. }));
        // The pin flag from qos-install's registry must survive the
        // merge so the store can show "marked installed".
        assert!(item.installed.is_pinned());
        assert_eq!(item.latest_version(), "0.1.0");
        assert!(matches!(item.source, StoreSource::Qos { .. }));
    }

    #[test]
    fn qos_entry_with_newer_registry_version_flags_update() {
        let entry = qos_entry("dev.qos.notes", "Notes", "0.2.0", [1u8; 32]);
        let installed = vec![InstalledEntry {
            id: "dev.qos.notes".into(),
            version: "0.1.0".into(),
            publisher_key_hex: entry.publisher_pubkey_hex.clone(),
            installed_at: 0,
            pinned: false,
        }];
        let item = qos_to_store_item(&verified(entry), "qOS main", &installed);
        assert!(item.installed.update_available());
    }

    #[test]
    fn merge_dedups_by_name_qos_wins() {
        let qos_entry = qos_entry("dev.qos.notes", "Notes", "0.1.0", [1u8; 32]);
        let qos = qos_to_store_item(&verified(qos_entry), "qOS main", &[]);
        // Flathub carries a same-name item; it must be dropped.
        let fh = PopularEntry {
            app_id: "com.other.Notes".into(),
            name: Some("Notes".into()),
        };
        let fh_item = flathub_placeholder_to_item(&fh, &[]);
        let merged = merge(vec![qos], vec![fh_item]);
        assert_eq!(merged.len(), 1);
        assert!(matches!(merged[0].source, StoreSource::Qos { .. }));
    }

    #[test]
    fn merge_preserves_distinct_flathub_items() {
        let q = qos_to_store_item(
            &verified(qos_entry("dev.qos.notes", "Notes", "0.1.0", [1u8; 32])),
            "qOS main",
            &[],
        );
        let f = flathub_placeholder_to_item(
            &PopularEntry {
                app_id: "org.videolan.VLC".into(),
                name: Some("VLC".into()),
            },
            &[],
        );
        let merged = merge(vec![q], vec![f]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn distinct_categories_sorted_and_deduped() {
        let a = qos_to_store_item(&verified(qos_entry("a", "A", "1", [2u8; 32])), "qOS", &[]);
        let mut b = qos_to_store_item(&verified(qos_entry("b", "B", "1", [3u8; 32])), "qOS", &[]);
        b.categories = vec!["Games".into(), "Productivity".into()];
        let cats = distinct_categories(&[a, b]);
        assert_eq!(cats, vec!["Games".to_string(), "Productivity".to_string()]);
    }

    #[test]
    fn appstream_detail_prefers_plain_long_description() {
        let record = AppStreamRecord {
            app_id: "org.example.App".into(),
            name: "Example".into(),
            summary: "Short".into(),
            description: "<p>Long description</p>".into(),
            icon: None,
            current_release_version: Some("2".into()),
            categories: Vec::new(),
            homepage: None,
            urls: None,
            screenshots: Vec::new(),
        };
        assert_eq!(
            flathub_record_to_item(&record, &[]).description,
            "Long description"
        );
    }
}

//! A publisher's side of bot entries: bind a program file and list what it
//! needs, the way the registry will check it.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result, bail};
use ed25519_dalek::SigningKey;
use registry_types::retention::RetentionPolicy;
use registry_types::{
    BotArtifact, BotRequirements, EntryKind, PackageRequirement, RegistryEntry, RegistryVersion,
    VersionArtifact,
};

use crate::EntryMetadata;

pub struct BotRelease<'a> {
    /// The program's canonical bytes.
    pub program: &'a [u8],
    pub package_id: &'a str,
    pub version: &'a str,
    /// The registry origin that will serve the program at `/blobs/<blake3>`.
    pub registry_origin: &'a str,
    /// Minimum versions of the connectors the program's triggers name.
    pub connectors: &'a [PackageRequirement],
}

/// The version `release` adds, after the same checks the registry runs,
/// with the program's name for a first entry's presentation.
fn bot_version(
    release: &BotRelease<'_>,
    released_at_unix: u64,
) -> Result<(String, RegistryVersion)> {
    let program = tilo_automation::program::parse_program(release.program)
        .context("the program file is not a canonical Shortcut")?;
    if let Err(errors) = tilo_automation::program::check_structure(&program) {
        let reasons: Vec<String> = errors.iter().map(ToString::to_string).collect();
        bail!("the program does not check: {}", reasons.join("; "));
    }
    let derived = tilo_automation::requirements::requirements(&program);
    let mut connectors = release.connectors.to_vec();
    connectors.sort_by(|a, b| a.package_id.cmp(&b.package_id));
    let listed: BTreeSet<String> = connectors.iter().map(|c| c.package_id.clone()).collect();
    if listed != derived.connectors {
        bail!(
            "--connector must name exactly the connectors the program's triggers use: {:?}",
            derived.connectors
        );
    }
    let hash = tilo_automation::program::program_hash(release.program);
    let version = RegistryVersion {
        version: release.version.to_owned(),
        released_at_unix,
        artifact: VersionArtifact::Bot(BotArtifact {
            program_url: registry_types::bot_program_url(release.registry_origin, &hash),
            program_blake3_hex: hash,
            program_size_bytes: release.program.len() as u64,
            requires: BotRequirements {
                connectors,
                apps: derived.apps.into_iter().collect(),
                appearance: derived.appearance,
                triggers: derived.triggers.into_iter().collect(),
            },
        }),
    };
    Ok((program.name, version))
}

/// Build and sign the entry for `release`, appending to `previous`.
///
/// # Errors
/// When the program is not canonical or fails its structural checks, the
/// connector list differs from the connectors the program uses, `previous`
/// belongs to another package, publisher or kind, or the version already
/// exists with other bytes.
pub fn build_bot_entry(
    release: &BotRelease<'_>,
    signer: &SigningKey,
    previous: Option<&RegistryEntry>,
    metadata: Option<&EntryMetadata>,
    released_at_unix: u64,
) -> Result<RegistryEntry> {
    let (name, version) = bot_version(release, released_at_unix)?;
    let publisher = hex::encode(signer.verifying_key().to_bytes());
    let mut versions = Vec::new();
    if let Some(previous) = previous {
        if previous.package_id != release.package_id
            || !previous
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&publisher)
            || previous.kind != EntryKind::Bot
        {
            bail!(
                "the previous entry is not this publisher's bot {}",
                release.package_id
            );
        }
        if let Some(existing) = previous
            .versions
            .iter()
            .find(|v| v.version == version.version)
        {
            if existing.artifact != version.artifact {
                bail!(
                    "version {} already exists with another program",
                    version.version
                );
            }
            if metadata.is_none() {
                return Ok(previous.clone());
            }
        } else {
            versions.push(version);
        }
        versions.extend(previous.versions.iter().cloned());
    } else {
        versions.push(version);
    }
    let versions = crate::entry::retain_versions(versions, &RetentionPolicy::default());
    let fresh = crate::entry::FreshEntry {
        package_id: release.package_id.to_owned(),
        kind: EntryKind::Bot,
        description: format!("{name} is a Tilo bot."),
        tagline: name.clone(),
        name,
        categories: vec!["Bots".into()],
    };
    crate::entry::signed_entry(
        fresh,
        versions,
        previous,
        metadata,
        released_at_unix,
        signer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program() -> Vec<u8> {
        let shortcut: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
            "id": "echo", "name": "Echo",
            "steps": [{"id": "post", "step": "invoke",
                "action": {"app": "so.tilo.chat", "kind": "chat.workspace", "action": "post"},
                "resource": {"first_of_kind": "chat.workspace"},
                "args": {"text": {"literal": "echo"}}}],
            "triggers": [{"trigger": "incoming_message", "app_id": "so.tilo.chat"}]
        }))
        .unwrap();
        tilo_automation::program::canonical_bytes(&shortcut).unwrap()
    }

    fn release(program: &[u8]) -> BotRelease<'_> {
        BotRelease {
            program,
            package_id: "so.tilo.bots.echo",
            version: "1.0.0",
            registry_origin: "https://apps.tilo.so",
            connectors: &[],
        }
    }

    #[test]
    fn a_bot_entry_binds_the_program_and_lists_what_it_uses() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let bytes = program();
        let entry = build_bot_entry(&release(&bytes), &key, None, None, 100).unwrap();
        assert_eq!(entry.kind, EntryKind::Bot);
        assert!(registry_types::verify_entry_signature(&entry));
        let bot = entry.versions[0].bot().unwrap();
        assert_eq!(
            bot.program_blake3_hex,
            blake3::hash(&bytes).to_hex().to_string()
        );
        assert_eq!(
            bot.program_url,
            format!("https://apps.tilo.so/blobs/{}", bot.program_blake3_hex)
        );
        assert_eq!(bot.requires.apps, ["so.tilo.chat"]);
        assert_eq!(bot.requires.triggers, ["incoming_message"]);
        assert!(!bot.requires.appearance);
    }

    #[test]
    fn a_release_refuses_bad_programs_and_mismatched_history() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let value: serde_json::Value = serde_json::from_slice(&program()).unwrap();
        let pretty = serde_json::to_vec_pretty(&value).unwrap();
        assert!(build_bot_entry(&release(&pretty), &key, None, None, 100).is_err());
        let connectors = [PackageRequirement {
            package_id: "so.tilo.discord".into(),
            min_version: "0.1.0".into(),
        }];
        let bytes = program();
        let with_unused = BotRelease {
            connectors: &connectors,
            ..release(&bytes)
        };
        let error = build_bot_entry(&with_unused, &key, None, None, 100)
            .unwrap_err()
            .to_string();
        assert!(error.contains("connector"), "{error}");
        let first = build_bot_entry(&release(&bytes), &key, None, None, 100).unwrap();
        let again = build_bot_entry(&release(&bytes), &key, Some(&first), None, 200).unwrap();
        assert_eq!(again, first, "re-running a release reproduces the entry");
        let mut renamed = value;
        renamed["name"] = "Echo 2".into();
        let renamed: tilo_automation::Shortcut = serde_json::from_value(renamed).unwrap();
        let other = tilo_automation::program::canonical_bytes(&renamed).unwrap();
        assert!(
            build_bot_entry(&release(&other), &key, Some(&first), None, 300).is_err(),
            "same version, other bytes"
        );
        let next = BotRelease {
            version: "1.1.0",
            ..release(&other)
        };
        let updated = build_bot_entry(&next, &key, Some(&first), None, 300).unwrap();
        assert_eq!(updated.versions.len(), 2);
        assert_eq!(updated.versions[0].version, "1.1.0", "newest first");
    }
}

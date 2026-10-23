//! Checks for bot versions. A bot version binds a program file the registry
//! itself holds (uploaded, then published as a blob), so review reads the
//! bytes from storage, never from the network.

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;

use registry_types::{BotArtifact, BotRequirements, RegistryEntry, RegistryVersion};
use tilo_automation::program::{check_structure, parse_program};
use tilo_automation::requirements::{ProgramRequirements, requirements};

use crate::{Storage, StorageError};

/// What review verified about one bot version's program.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InspectedProgram {
    pub version: String,
    pub package_id: String,
    pub size_bytes: u64,
    pub name: String,
    pub steps: usize,
    pub triggers: Vec<String>,
    pub apps: Vec<String>,
    pub connectors: Vec<String>,
    pub appearance: bool,
}

#[derive(Debug, Error)]
pub enum BotReviewError {
    #[error("bot program for {version} is not uploaded; PUT /uploads/<blake3>.program first")]
    NotUploaded { version: String },
    #[error("bot program for {version} is {actual} bytes, the entry says {expected}")]
    SizeMismatch {
        version: String,
        expected: u64,
        actual: u64,
    },
    #[error("bot program for {version} does not hash to the entry's program_blake3_hex")]
    HashMismatch { version: String },
    #[error("bot program for {version} is invalid: {reason}")]
    Program { version: String, reason: String },
    #[error("bot version {version} lists requirements its program does not have: {reason}")]
    Requirements { version: String, reason: String },
    #[error("reading the bot program for {version}: {reason}")]
    Storage { version: String, reason: String },
}

/// Whether `listed` says exactly what `derived` needs. Every difference is
/// named, so a publisher can fix the listing in one go.
///
/// # Errors
/// A sentence listing each difference.
pub fn requirements_match(
    listed: &BotRequirements,
    derived: &ProgramRequirements,
) -> Result<(), String> {
    let mut problems = Vec::new();
    let mut compare = |what: &str, listed: BTreeSet<String>, needed: &BTreeSet<String>| {
        for missing in needed.difference(&listed) {
            problems.push(format!("{what} {missing} is used but not listed"));
        }
        for extra in listed.difference(needed) {
            problems.push(format!("{what} {extra} is listed but not used"));
        }
    };
    compare("app", listed.apps.iter().cloned().collect(), &derived.apps);
    compare(
        "connector",
        listed
            .connectors
            .iter()
            .map(|c| c.package_id.clone())
            .collect(),
        &derived.connectors,
    );
    compare(
        "trigger",
        listed.triggers.iter().cloned().collect(),
        &derived.triggers,
    );
    if listed.appearance != derived.appearance {
        problems.push(format!(
            "appearance is listed as {} but the program {}",
            listed.appearance,
            if derived.appearance {
                "posts under other names"
            } else {
                "never does"
            }
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Check `bytes` against the version that binds them.
///
/// # Errors
/// [`BotReviewError`] for a size, hash, program or requirements mismatch.
pub fn inspect_program(
    package_id: &str,
    version: &str,
    bot: &BotArtifact,
    bytes: &[u8],
) -> Result<InspectedProgram, BotReviewError> {
    let v = || version.to_owned();
    let actual = bytes.len() as u64;
    if actual != bot.program_size_bytes {
        return Err(BotReviewError::SizeMismatch {
            version: v(),
            expected: bot.program_size_bytes,
            actual,
        });
    }
    if blake3::hash(bytes).to_hex().as_str() != bot.program_blake3_hex {
        return Err(BotReviewError::HashMismatch { version: v() });
    }
    let program = parse_program(bytes).map_err(|e| BotReviewError::Program {
        version: v(),
        reason: e.to_string(),
    })?;
    check_structure(&program).map_err(|errors| BotReviewError::Program {
        version: v(),
        reason: errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "),
    })?;
    let derived = requirements(&program);
    requirements_match(&bot.requires, &derived).map_err(|reason| BotReviewError::Requirements {
        version: v(),
        reason,
    })?;
    Ok(InspectedProgram {
        version: v(),
        package_id: package_id.to_owned(),
        size_bytes: bot.program_size_bytes,
        name: program.name.clone(),
        steps: program.steps.len(),
        triggers: derived.triggers.into_iter().collect(),
        apps: derived.apps.into_iter().collect(),
        connectors: derived.connectors.into_iter().collect(),
        appearance: derived.appearance,
    })
}

/// The program bytes of `version`: the publisher's upload, or the blob an
/// earlier approval already published.
fn program_bytes(
    storage: &Storage,
    version: &str,
    bot: &BotArtifact,
) -> Result<Vec<u8>, BotReviewError> {
    let storage_error = |e: StorageError| BotReviewError::Storage {
        version: version.to_owned(),
        reason: e.to_string(),
    };
    if let Some(bytes) = storage
        .load_program_upload(&bot.program_blake3_hex)
        .map_err(storage_error)?
    {
        return Ok(bytes);
    }
    storage
        .load_payload_blob(&bot.program_blake3_hex)
        .map_err(storage_error)?
        .ok_or_else(|| BotReviewError::NotUploaded {
            version: version.to_owned(),
        })
}

pub fn review_bot_versions(
    storage: &Storage,
    entry: &RegistryEntry,
) -> Result<Vec<(InspectedProgram, Vec<u8>)>, BotReviewError> {
    entry
        .versions
        .iter()
        .filter_map(|version| version.bot().map(|bot| (version, bot)))
        .map(|(version, bot)| {
            let bytes = program_bytes(storage, &version.version, bot)?;
            let inspected = inspect_program(&entry.package_id, &version.version, bot, &bytes)?;
            Ok((inspected, bytes))
        })
        .collect()
}

/// Inspect one version of a bot entry: the check `submit` runs on each
/// version a request adds.
pub(crate) fn check_submitted_version(
    storage: &Storage,
    entry: &RegistryEntry,
    version: &RegistryVersion,
) -> Result<(), BotReviewError> {
    let Some(bot) = version.bot() else {
        return Ok(());
    };
    let bytes = program_bytes(storage, &version.version, bot)?;
    inspect_program(&entry.package_id, &version.version, bot, &bytes).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::PackageRequirement;

    fn derived() -> ProgramRequirements {
        ProgramRequirements {
            apps: BTreeSet::from(["so.tilo.chat".to_owned()]),
            connectors: BTreeSet::from(["so.tilo.discord".to_owned()]),
            appearance: true,
            triggers: BTreeSet::from(["connector".to_owned()]),
        }
    }

    fn listed() -> BotRequirements {
        BotRequirements {
            connectors: vec![PackageRequirement {
                package_id: "so.tilo.discord".into(),
                min_version: "0.1.0".into(),
            }],
            apps: vec!["so.tilo.chat".into()],
            appearance: true,
            triggers: vec!["connector".into()],
        }
    }

    #[test]
    fn listed_requirements_must_equal_what_the_program_needs() {
        assert_eq!(requirements_match(&listed(), &derived()), Ok(()));
        let mut missing_app = listed();
        missing_app.apps.clear();
        assert!(
            requirements_match(&missing_app, &derived())
                .unwrap_err()
                .contains("so.tilo.chat")
        );
        let mut extra_connector = listed();
        extra_connector.connectors.push(PackageRequirement {
            package_id: "so.tilo.mail".into(),
            min_version: "1.0.0".into(),
        });
        assert!(
            requirements_match(&extra_connector, &derived())
                .unwrap_err()
                .contains("so.tilo.mail")
        );
        let mut no_appearance = listed();
        no_appearance.appearance = false;
        assert!(
            requirements_match(&no_appearance, &derived())
                .unwrap_err()
                .contains("appearance")
        );
        let mut other_trigger = listed();
        other_trigger.triggers = vec!["schedule".into()];
        assert!(requirements_match(&other_trigger, &derived()).is_err());
    }
}

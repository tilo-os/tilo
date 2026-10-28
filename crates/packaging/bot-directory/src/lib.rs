//! Installing a bot from a registry: verify the listed program against its
//! signed entry, derive the grant it needs, check its connectors, and build
//! the `install-bot` arguments a room's governance action takes. The Store
//! and the room service share this, so both show and install the same thing.

use std::collections::BTreeSet;

use app_cli::client::{RegistryError, VerifiedEntry};
use app_cli::config::RegistryConfig;
use registry_types::{BotArtifact, DirectoryRef, EntryKind, RegistryEntry, RegistryImageKind};
use tilo_action_policy::{Grant, Labels, Rule, Subject};
use tilo_automation::{ActionCatalog, Shortcut};

/// A bot version the directory lists, with the exact program bytes it binds.
#[derive(Clone, Debug)]
pub struct ListedBot {
    pub source: DirectoryRef,
    pub entry: RegistryEntry,
    pub artifact: BotArtifact,
    /// The canonical program, as the room stores it.
    pub program_text: String,
    pub program: Shortcut,
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("{entry} {version} is not listed in the directory")]
    NotListed { entry: String, version: String },
    #[error("{entry} is not a bot")]
    NotABot { entry: String },
    #[error("registry: {0}")]
    Registry(String),
    #[error("program: {0}")]
    Program(String),
    #[error("program does not check: {0}")]
    Structure(String),
    #[error("the entry's requirements differ from its program: {0}")]
    RequirementsMismatch(String),
    #[error("not ready to install: {0}")]
    NotReady(String),
}

impl From<RegistryError> for DirectoryError {
    fn from(error: RegistryError) -> Self {
        match error {
            RegistryError::ProgramMismatch(reason) => Self::Program(reason),
            other => Self::Registry(other.to_string()),
        }
    }
}

fn not_listed(entry: &str, version: &str) -> DirectoryError {
    DirectoryError::NotListed {
        entry: entry.to_owned(),
        version: version.to_owned(),
    }
}

/// Check `program_bytes` against `version` of the bot `entry` in `entries`.
/// The registry checked this at review. A client checks again, because the
/// bytes it installs are the ones that count.
pub fn verify_listed_bot(
    entries: &[VerifiedEntry],
    registry_owner: &str,
    entry: &str,
    version: &str,
    program_bytes: &[u8],
) -> Result<ListedBot, DirectoryError> {
    if entries
        .iter()
        .any(|e| e.entry.package_id == entry && e.entry.kind != EntryKind::Bot)
    {
        return Err(DirectoryError::NotABot {
            entry: entry.to_owned(),
        });
    }
    let (listed, artifact) = app_cli::bots::listed_version(entries, entry, version)
        .ok_or_else(|| not_listed(entry, version))?;
    if program_bytes.len() as u64 != artifact.program_size_bytes
        || tilo_automation::program::program_hash(program_bytes) != artifact.program_blake3_hex
    {
        return Err(DirectoryError::Program(
            "bytes differ from the signed program".into(),
        ));
    }
    let program = tilo_automation::program::parse_program(program_bytes)
        .map_err(|error| DirectoryError::Program(error.to_string()))?;
    tilo_automation::program::check_structure(&program).map_err(|errors| {
        DirectoryError::Structure(
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    let derived = tilo_automation::requirements::requirements(&program);
    let listed_apps: BTreeSet<String> = artifact.requires.apps.iter().cloned().collect();
    let listed_connectors: BTreeSet<String> = artifact
        .requires
        .connectors
        .iter()
        .map(|c| c.package_id.clone())
        .collect();
    let listed_triggers: BTreeSet<String> = artifact.requires.triggers.iter().cloned().collect();
    if listed_apps != derived.apps
        || listed_connectors != derived.connectors
        || listed_triggers != derived.triggers
        || artifact.requires.appearance != derived.appearance
    {
        return Err(DirectoryError::RequirementsMismatch(format!(
            "program uses apps {:?}, connectors {:?}, triggers {:?}, appearance {}",
            derived.apps, derived.connectors, derived.triggers, derived.appearance
        )));
    }
    let program_text = String::from_utf8(program_bytes.to_vec())
        .map_err(|_| DirectoryError::Program("not UTF-8".into()))?;
    Ok(ListedBot {
        source: DirectoryRef {
            registry_owner: registry_owner.to_owned(),
            entry: entry.to_owned(),
            version: version.to_owned(),
        },
        entry: listed.clone(),
        artifact: artifact.clone(),
        program_text,
        program,
    })
}

/// Fetch the signed listing of `entry` (every retained version) and the
/// program `version` binds, then verify both.
pub fn fetch_listed_bot(
    reg: &RegistryConfig,
    entry: &str,
    version: &str,
) -> Result<ListedBot, DirectoryError> {
    let listed = match app_cli::client::fetch_all_versions(reg, entry) {
        Ok(listed) => listed,
        Err(RegistryError::NotListed(_)) => return Err(not_listed(entry, version)),
        Err(other) => return Err(other.into()),
    };
    // `newest` asks for the most recently released version listed now: what
    // an app that sets up a bot it ships beside (the Discord connector's
    // bridge) asks for, without knowing the registry's numbering.
    let resolved = if version == NEWEST {
        newest_version(&listed.entry).ok_or_else(|| not_listed(entry, version))?
    } else {
        version.to_owned()
    };
    let version = resolved.as_str();
    let entries = [listed];
    let (_, artifact) = app_cli::bots::listed_version(&entries, entry, version)
        .ok_or_else(|| not_listed(entry, version))?;
    let bytes = app_cli::bots::fetch_bot_program(artifact)?;
    verify_listed_bot(&entries, &reg.owner_pubkey_hex, entry, version, &bytes)
}

/// The version a request names to get the most recently released one.
pub const NEWEST: &str = "newest";

/// The most recently released bot version `entry` lists.
fn newest_version(entry: &RegistryEntry) -> Option<String> {
    entry
        .versions
        .iter()
        .filter(|listed| listed.bot().is_some())
        .max_by_key(|listed| listed.released_at_unix)
        .map(|listed| listed.version.clone())
}

/// Whether the host that will run the bot has a connector it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectorState {
    Ready,
    NotInstalled,
    /// Installed but not set up, such as a missing bot token. The connector's
    /// `connector.status` read action says why.
    NotConfigured {
        problem: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorStatus {
    pub package_id: String,
    pub min_version: String,
    pub state: ConnectorState,
}

/// The caller's view of the connectors on the host that will run the bot.
/// The Store answers it from the action catalogue (`connector.status`); the
/// room service answers it from its own configuration.
pub trait HostConnectors {
    fn status(&self, package_id: &str, min_version: &str) -> ConnectorState;
}

/// What installing a listed bot into a room would do, for review before the
/// grant is given.
#[derive(Clone, Debug, PartialEq)]
pub struct BotInstallPlan {
    pub source: DirectoryRef,
    pub name: String,
    pub icon_url: Option<String>,
    pub program_text: String,
    pub program_hash: String,
    /// The grant the room gives the bot: exactly what its steps and triggers use.
    pub rules: Vec<Rule>,
    /// `rules` in plain language.
    pub grant_sentence: String,
    pub triggers: Vec<String>,
    pub connectors: Vec<ConnectorStatus>,
    /// Whether the program posts under other names and avatars.
    pub appearance: bool,
    /// Apps the program uses that are not installed where it would run.
    pub missing_apps: Vec<String>,
}

impl BotInstallPlan {
    /// Every app installed and every connector ready.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.missing_apps.is_empty()
            && self
                .connectors
                .iter()
                .all(|c| c.state == ConnectorState::Ready)
    }
}

/// Plan installing `bot`. `catalog` is the action catalogue of the room's
/// host, which classifies each step's action kind for the grant.
#[must_use]
pub fn plan_install(
    bot: &ListedBot,
    catalog: &ActionCatalog,
    connectors: &dyn HostConnectors,
    installed_apps: &BTreeSet<String>,
    labels: &dyn Labels,
) -> BotInstallPlan {
    let rules = tilo_automation::grant::derived_rules(&bot.program, catalog);
    // The subject only names the preview grants; the sentence never prints it.
    let preview_subject = Subject::Shortcut(bot.source.entry.clone());
    let grants: Vec<Grant> = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            Grant::new(
                format!("preview-{index}"),
                preview_subject.clone(),
                rule.node.clone(),
                rule.kinds,
                rule.answer,
                preview_subject.clone(),
            )
        })
        .collect();
    let derived = tilo_automation::requirements::requirements(&bot.program);
    let images = bot.entry.images.resolve("en");
    BotInstallPlan {
        source: bot.source.clone(),
        name: bot
            .entry
            .name
            .resolve("en")
            .unwrap_or(&bot.entry.package_id)
            .to_owned(),
        icon_url: images
            .iter()
            .find(|image| image.kind == RegistryImageKind::Icon)
            .map(|image| image.url.clone()),
        program_text: bot.program_text.clone(),
        program_hash: bot.artifact.program_blake3_hex.clone(),
        grant_sentence: tilo_action_policy::describe(&grants, labels),
        rules,
        triggers: derived.triggers.into_iter().collect(),
        connectors: bot
            .artifact
            .requires
            .connectors
            .iter()
            .map(|c| ConnectorStatus {
                package_id: c.package_id.clone(),
                min_version: c.min_version.clone(),
                state: connectors.status(&c.package_id, &c.min_version),
            })
            .collect(),
        appearance: derived.appearance,
        missing_apps: derived
            .apps
            .into_iter()
            .filter(|app| !installed_apps.contains(app))
            .collect(),
    }
}

/// What the person installing chooses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstallInputs {
    /// The installation to update; `None` installs a new one.
    pub installation: Option<String>,
    /// Let the bot post under per-message names and avatars (always
    /// labelled BOT). Only honoured when the program asks for it.
    pub appearance_personas: bool,
    /// Values for the program's declared inputs.
    pub inputs: serde_json::Map<String, serde_json::Value>,
}

/// The arguments of the room's `install-bot` action for `plan`.
///
/// # Errors
/// [`DirectoryError::NotReady`] when an app is missing or a connector is not
/// ready.
pub fn install_arguments(
    plan: &BotInstallPlan,
    inputs: &InstallInputs,
) -> Result<serde_json::Map<String, serde_json::Value>, DirectoryError> {
    if !plan.ready() {
        let unready: Vec<&str> = plan
            .connectors
            .iter()
            .filter(|c| c.state != ConnectorState::Ready)
            .map(|c| c.package_id.as_str())
            .collect();
        return Err(DirectoryError::NotReady(format!(
            "missing apps {:?}, connectors not ready {unready:?}",
            plan.missing_apps
        )));
    }
    let mut args = serde_json::Map::new();
    if let Some(installation) = &inputs.installation {
        args.insert("installation".into(), installation.clone().into());
    }
    args.insert("program".into(), plan.program_text.clone().into());
    args.insert("name".into(), plan.name.clone().into());
    if let Some(icon) = &plan.icon_url {
        args.insert("icon".into(), icon.clone().into());
    }
    let personas = plan.appearance && inputs.appearance_personas;
    args.insert(
        "appearance".into(),
        if personas { "personas" } else { "own" }.into(),
    );
    args.insert(
        "source".into(),
        serde_json::json!({
            "registry_owner": plan.source.registry_owner,
            "entry": plan.source.entry,
            "version": plan.source.version,
        }),
    );
    args.insert(
        "inputs".into(),
        serde_json::Value::Object(inputs.inputs.clone()),
    );
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_cli::client::TrustLevel;

    const OWNER: &str = "bb";

    fn program_bytes() -> Vec<u8> {
        let shortcut: Shortcut = serde_json::from_value(serde_json::json!({
            "id": "echo", "name": "Echo",
            "steps": [{"id": "post", "step": "invoke",
                "action": {"app": "so.tilo.chat", "kind": "chat.workspace", "action": "post"},
                "resource": {"first_of_kind": "chat.workspace"},
                "args": {"text": {"literal": "echo"}}}],
            "triggers": [{"trigger": "record_changed", "kind": "chat.workspace"}]
        }))
        .unwrap();
        tilo_automation::program::canonical_bytes(&shortcut).unwrap()
    }

    fn listing(program: &[u8], versions: &[&str], apps: &[&str]) -> Vec<VerifiedEntry> {
        let hash = tilo_automation::program::program_hash(program);
        let versions = versions
            .iter()
            .map(|version| registry_types::RegistryVersion {
                version: (*version).into(),
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Bot(BotArtifact {
                    program_url: registry_types::bot_program_url("https://apps.tilo.so", &hash),
                    program_blake3_hex: hash.clone(),
                    program_size_bytes: program.len() as u64,
                    requires: registry_types::BotRequirements {
                        connectors: Vec::new(),
                        apps: apps.iter().map(|app| (*app).to_owned()).collect(),
                        appearance: false,
                        triggers: vec!["record_changed".into()],
                    },
                }),
            })
            .collect();
        vec![VerifiedEntry {
            entry: RegistryEntry {
                package_id: "so.tilo.bots.echo".into(),
                kind: EntryKind::Bot,
                name: "Echo".into(),
                description: "Posts echo".into(),
                tagline: "Echo".into(),
                images: registry_types::LocalizedImages::default(),
                supported_languages: vec!["en".into()],
                categories: vec!["Bots".into()],
                source_url: None,
                homepage: None,
                publisher_pubkey_hex: "aa".repeat(32),
                versions,
                updated_at_unix: 1,
                signature_hex: String::new(),
            },
            trust: TrustLevel::PublisherSigned,
            version_index: None,
        }]
    }

    struct NoConnectors;
    impl HostConnectors for NoConnectors {
        fn status(&self, _: &str, _: &str) -> ConnectorState {
            ConnectorState::NotInstalled
        }
    }

    #[test]
    fn a_listed_bot_is_verified_against_its_entry() {
        let program = program_bytes();
        let entries = listing(&program, &["1.0.0"], &["so.tilo.chat"]);
        let bot =
            verify_listed_bot(&entries, OWNER, "so.tilo.bots.echo", "1.0.0", &program).unwrap();
        assert_eq!(bot.source.entry, "so.tilo.bots.echo");
        assert_eq!(bot.source.version, "1.0.0");
        assert_eq!(bot.source.registry_owner, OWNER);
        assert_eq!(bot.program_text.as_bytes(), program.as_slice());
        let dishonest = listing(&program, &["1.0.0"], &["so.tilo.notes"]);
        assert!(matches!(
            verify_listed_bot(&dishonest, OWNER, "so.tilo.bots.echo", "1.0.0", &program),
            Err(DirectoryError::RequirementsMismatch(_))
        ));
        assert!(matches!(
            verify_listed_bot(&entries, OWNER, "so.tilo.bots.echo", "1.0.0", b"{}"),
            Err(DirectoryError::Program(_))
        ));
    }

    #[test]
    fn newest_is_the_most_recently_released_version() {
        let program = program_bytes();
        let mut entries = listing(&program, &["1.0.0", "2.0.0", "1.5.0"], &["so.tilo.chat"]);
        let versions = &mut entries[0].entry.versions;
        versions[0].released_at_unix = 10;
        versions[1].released_at_unix = 20;
        versions[2].released_at_unix = 30;
        assert_eq!(newest_version(&entries[0].entry).as_deref(), Some("1.5.0"));
        entries[0].entry.versions.clear();
        assert_eq!(newest_version(&entries[0].entry), None);
    }

    #[test]
    fn a_version_no_longer_listed_is_refused() {
        let program = program_bytes();
        let entries = listing(&program, &["1.1.0"], &["so.tilo.chat"]);
        assert!(matches!(
            verify_listed_bot(&entries, OWNER, "so.tilo.bots.echo", "1.0.0", &program),
            Err(DirectoryError::NotListed { .. })
        ));
        assert!(matches!(
            verify_listed_bot(&[], OWNER, "so.tilo.bots.echo", "1.1.0", &program),
            Err(DirectoryError::NotListed { .. })
        ));
    }

    #[test]
    fn the_plan_shows_the_grant_and_what_is_missing() {
        let program = program_bytes();
        let entries = listing(&program, &["1.0.0"], &["so.tilo.chat"]);
        let bot =
            verify_listed_bot(&entries, OWNER, "so.tilo.bots.echo", "1.0.0", &program).unwrap();
        let catalog = ActionCatalog::new(Vec::new());
        let without_chat = plan_install(
            &bot,
            &catalog,
            &NoConnectors,
            &BTreeSet::new(),
            &tilo_action_policy::Ids,
        );
        assert_eq!(without_chat.missing_apps, ["so.tilo.chat"]);
        assert!(!without_chat.ready());
        assert!(matches!(
            install_arguments(&without_chat, &InstallInputs::default()),
            Err(DirectoryError::NotReady(_))
        ));
        let installed = BTreeSet::from(["so.tilo.chat".to_owned()]);
        let plan = plan_install(
            &bot,
            &catalog,
            &NoConnectors,
            &installed,
            &tilo_action_policy::Ids,
        );
        assert!(plan.ready());
        assert!(!plan.rules.is_empty(), "the trigger reads what it watches");
        assert!(!plan.grant_sentence.is_empty());
        assert_eq!(
            plan.program_hash,
            tilo_automation::program::program_hash(&program)
        );
        let args = install_arguments(&plan, &InstallInputs::default()).unwrap();
        assert_eq!(
            args["program"],
            serde_json::Value::String(plan.program_text)
        );
        assert_eq!(args["appearance"], "own");
        assert_eq!(args["source"]["entry"], "so.tilo.bots.echo");
        assert!(!args.contains_key("installation"));
    }
}

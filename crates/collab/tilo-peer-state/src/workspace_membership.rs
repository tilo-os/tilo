//! Typed native workspace membership commands, not grant or identity blobs.
use serde::{Deserialize, Serialize};
use tilo_action_policy::roles::AgentPreset;
use tilo_action_policy::{Answer, Grant, GrantId, KindSet, TreePath};

use crate::installation::{Installation, InstallationId};
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::delegation::Delegation;

/// A bot's device: its capability-free certificate and its installer's
/// delegation naming it the installation in this room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotDevice {
    /// The bot device's certificate, signed by its installer's root.
    pub cert: DeviceCert,
    /// The installer's delegation naming the device.
    pub delegation: Delegation,
}

/// Install a bot, or update an installed bot's program.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotInstall {
    /// The row to install, or the updated row.
    pub installation: Installation,
    /// The program's canonical bytes, as text. The room keeps them so every
    /// member can read exactly what runs.
    pub program: String,
    /// Required for a new installation, absent for an update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<BotDevice>,
}

/// Take over an installed bot: its new device and the adopter's delegation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAdoption {
    /// The installed bot.
    pub installation: InstallationId,
    /// The adopter's new device for it and their delegation naming it.
    pub device: BotDevice,
}

/// A fresh delegation for an installed bot, on its device or a new one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRenewal {
    /// The installed bot.
    pub installation: InstallationId,
    /// Its device, the same or a new one, and the fresh delegation.
    pub device: BotDevice,
}

/// A bot from the registry's directory that the room asks its host to run:
/// the host installs it as itself and keeps it a governed member for as
/// long as the request stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotRequest {
    /// The directory entry (`so.tilo.bots.standup`).
    pub entry: String,
    /// The listed version to install.
    pub version: String,
    /// The bot's inputs the request gives its host (a bridge's
    /// `guild=<server id>` and `server=<name>`): settings every member may
    /// read, never a secret. A secret reaches the host on its own, through
    /// the room's `set-connector-secret` action.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub inputs: std::collections::BTreeMap<String, String>,
    /// A developer's own program for the entry, canonical text, run instead
    /// of the listed one: the room keeps it with the request, so every
    /// member reads exactly what runs, and `version` names the build. The
    /// person asking must hold every grant the program asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
}

/// Most bytes a requested entry or version takes.
const MAX_BOT_REQUEST_ID: usize = 128;
/// Most inputs a request gives, and the most bytes of one input's name and
/// value.
const MAX_BOT_INPUTS: usize = 8;
const MAX_BOT_INPUT_NAME: usize = 32;
const MAX_BOT_INPUT_VALUE: usize = 256;

impl BotRequest {
    /// Validate the entry and version the request names.
    ///
    /// # Errors
    /// Refuses an empty, unbounded or non-canonical entry or version.
    pub fn validate(&self) -> Result<(), String> {
        validate_bot_entry(&self.entry)?;
        if self.version.is_empty()
            || self.version.len() > MAX_BOT_REQUEST_ID
            || !self
                .version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err("invalid bot version".into());
        }
        if self.inputs.len() > MAX_BOT_INPUTS {
            return Err("too many bot inputs".into());
        }
        for (name, value) in &self.inputs {
            if name.is_empty()
                || name.len() > MAX_BOT_INPUT_NAME
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err("invalid bot input name".into());
            }
            if value.len() > MAX_BOT_INPUT_VALUE || value.chars().any(char::is_control) {
                return Err(format!("invalid value for the bot input `{name}`"));
            }
        }
        if let Some(program) = &self.program {
            if program.len() > tilo_automation::program::MAX_PROGRAM_BYTES {
                return Err("the bot program is too large".into());
            }
            tilo_automation::program::parse_program(program.as_bytes())
                .map_err(|error| format!("the bot program does not parse: {error}"))?;
        }
        Ok(())
    }
}

/// The version of an app a room runs. Every member's desktop runs this
/// version, and a change made with another version waits until its author
/// runs this one: mixed versions never write each other's data unseen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRelease {
    /// The app (`so.tilo.notes`).
    pub app: String,
    /// The version its package names (`1.4.0`, `1.5.0-dev.3`).
    pub version: String,
    /// The BLAKE3 hash of the package's `surface.toml`, hex: the surface
    /// every change made with this version names.
    pub surface: String,
    /// The package's `tapp:` hash (the BLAKE3 of its signed root), hex:
    /// what each member fetches and verifies before running the version.
    pub package: String,
    /// The publisher key that signed the package, hex.
    pub publisher: String,
    /// A developer's own build, signed with their key and never reviewed by
    /// a registry; every window of the app says so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dev: bool,
}

/// Most bytes an app release's version takes.
const MAX_APP_VERSION: usize = 64;

impl AppRelease {
    /// Validate what the release names.
    ///
    /// # Errors
    /// An app id, version or hash that is not canonical.
    pub fn validate(&self) -> Result<(), String> {
        validate_app_id(&self.app)?;
        if self.version.is_empty()
            || self.version.len() > MAX_APP_VERSION
            || !self
                .version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err("invalid app version".into());
        }
        for (what, hash) in [
            ("surface", &self.surface),
            ("package", &self.package),
            ("publisher", &self.publisher),
        ] {
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(format!("the app's {what} is not 64 lowercase hex digits"));
            }
        }
        Ok(())
    }

    /// The surface the changes made with this release name.
    #[must_use]
    pub fn surface_hash(&self) -> Option<[u8; 32]> {
        hex::decode(&self.surface).ok()?.try_into().ok()
    }
}

/// A canonical application identifier: lowercase, dotted.
fn validate_app_id(app: &str) -> Result<(), String> {
    if app.is_empty()
        || app.len() > 128
        || app.split('.').any(str::is_empty)
        || !app.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err("invalid canonical application identifier".into());
    }
    Ok(())
}

/// A directory entry id, as the registry names bots: lowercase, dotted.
fn validate_bot_entry(entry: &str) -> Result<(), String> {
    if entry.is_empty()
        || entry.len() > MAX_BOT_REQUEST_ID
        || entry.split('.').any(str::is_empty)
        || !entry.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        return Err("invalid bot directory entry".into());
    }
    Ok(())
}

/// Why a retired task grant command is refused.
pub const RETIRED_TASK_GRANTS: &str =
    "a task's grants are retired; an issue's grants are ordinary grants";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceMembershipCommand {
    AddAgent {
        agent: String,
        preset: AgentPreset,
    },
    RemoveAgent {
        agent: String,
    },
    /// Delegate normal app authorship after an authenticated person opens it.
    GrantAppAuthorship {
        app: String,
    },
    /// Retired with room tasks: an issue's grants are ordinary grants. It
    /// keeps the commands after it at their encoded positions, still reads
    /// where rooms hold it, and is refused.
    #[serde(rename = "GrantForTask")]
    RetiredGrantForTask {
        task: String,
        node: TreePath,
        kinds: KindSet,
        answer: Answer,
    },
    /// Retired with room tasks, as [`Self::RetiredGrantForTask`].
    #[serde(rename = "RevokeForTask")]
    RetiredRevokeForTask {
        task: String,
        grant: GrantId,
    },
    /// Record `grant` in the room, as the caller gives it: they must hold the
    /// room's grant action and may give no more than they hold.
    Grant {
        grant: Grant,
    },
    /// Take back the grant `grant`, as the caller: they must hold the room's
    /// revoke action.
    Revoke {
        grant: GrantId,
    },
    /// Install a bot or update its program ([`BotInstall`]).
    InstallBot(Box<BotInstall>),
    /// Renew an installed bot's delegation, or move it to a new device key.
    RenewBot(Box<BotRenewal>),
    /// Withdraw a bot and revoke its grants; its row stays.
    UninstallBot {
        installation: InstallationId,
    },
    /// Stop a bot from starting runs.
    PauseBot {
        installation: InstallationId,
    },
    /// Let a paused bot start runs again.
    ResumeBot {
        installation: InstallationId,
    },
    /// Change whether a bot may post under other names.
    SetBotAppearance {
        installation: InstallationId,
        appearance: crate::installation::AppearancePolicy,
    },
    /// Become a bot's installer ([`BotAdoption`]).
    AdoptBot(Box<BotAdoption>),
    /// Ask the room's host to run a bot from the directory ([`BotRequest`]).
    RequestBot(BotRequest),
    /// Take back the request for directory entry `entry`: its host
    /// uninstalls the bot it installed for it.
    WithdrawBotRequest {
        entry: String,
    },
    /// Run this release of its app in the room, for every member
    /// ([`AppRelease`]).
    InstallApp(AppRelease),
    /// Stop choosing a version of `app`: each member runs the one they
    /// have.
    UninstallApp {
        app: String,
    },
}

impl WorkspaceMembershipCommand {
    #[must_use]
    pub fn agent(&self) -> Option<&str> {
        match self {
            Self::AddAgent { agent, .. } | Self::RemoveAgent { agent } => Some(agent),
            Self::GrantAppAuthorship { .. }
            | Self::RetiredGrantForTask { .. }
            | Self::RetiredRevokeForTask { .. }
            | Self::Grant { .. }
            | Self::Revoke { .. }
            | Self::InstallBot(_)
            | Self::RenewBot(_)
            | Self::UninstallBot { .. }
            | Self::PauseBot { .. }
            | Self::ResumeBot { .. }
            | Self::SetBotAppearance { .. }
            | Self::AdoptBot(_)
            | Self::RequestBot(_)
            | Self::WithdrawBotRequest { .. }
            | Self::InstallApp(_)
            | Self::UninstallApp { .. } => None,
        }
    }

    /// Validate the canonical identifier before entering the owner turn.
    ///
    /// # Errors
    /// Refuses empty, unbounded, unnormalised or control-containing identifiers.
    pub fn validate(&self) -> Result<(), String> {
        if matches!(
            self,
            Self::RetiredGrantForTask { .. } | Self::RetiredRevokeForTask { .. }
        ) {
            return Err(RETIRED_TASK_GRANTS.into());
        }
        if let Self::InstallBot(install) = self {
            install.installation.validate()?;
            if install.program.len() > tilo_automation::program::MAX_PROGRAM_BYTES {
                return Err("the bot program is too large".into());
            }
            return Ok(());
        }
        if let Self::RequestBot(request) = self {
            return request.validate();
        }
        if let Self::WithdrawBotRequest { entry } = self {
            return validate_bot_entry(entry);
        }
        if let Self::InstallApp(release) = self {
            return release.validate();
        }
        if let Self::UninstallApp { app } = self {
            return validate_app_id(app);
        }
        // Installation ids are checked when they are read; a bot command
        // carries nothing else free-form.
        if matches!(
            self,
            Self::RenewBot(_)
                | Self::UninstallBot { .. }
                | Self::PauseBot { .. }
                | Self::ResumeBot { .. }
                | Self::SetBotAppearance { .. }
                | Self::AdoptBot(_)
        ) {
            return Ok(());
        }
        if let Self::Grant { grant } = self
            && grant.id.as_str().trim().is_empty()
        {
            return Err("a grant needs an id".into());
        }
        if matches!(self, Self::Grant { .. } | Self::Revoke { .. }) {
            return Ok(());
        }
        if let Self::GrantAppAuthorship { app } = self {
            return validate_app_id(app);
        }
        let agent = self.agent().ok_or("missing canonical agent identifier")?;
        if agent.is_empty()
            || agent.len() > 128
            || agent.trim() != agent
            || agent.to_lowercase() != agent
            || agent.chars().any(char::is_control)
        {
            return Err("invalid canonical workspace agent identifier".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_action_policy::{Answer, Subject, TreePath, roles};

    #[test]
    fn presets_are_closed_admission_markers_without_admin_authority() {
        use tilo_action_policy::{ActionKind, Authority, Context, Party, RoomPolicy};
        let room = RoomPolicy::new("room", "owner");
        let content = TreePath::room("room").app("notes").resource("page");
        let comment = content.clone().field(roles::COMMENTS_FIELD).action("add");
        let invite = roles::governance_action("room", roles::governance::INVITE);
        for (id, read, undoable, comments, outside) in [
            (
                "observer",
                Answer::Allow,
                Answer::Deny,
                Answer::Deny,
                Answer::Deny,
            ),
            (
                "contributor",
                Answer::Allow,
                Answer::Deny,
                Answer::Allow,
                Answer::Deny,
            ),
            (
                "operator",
                Answer::Allow,
                Answer::Allow,
                Answer::Allow,
                Answer::Ask,
            ),
            (
                "custom",
                Answer::Allow,
                Answer::Deny,
                Answer::Deny,
                Answer::Deny,
            ),
        ] {
            let preset = AgentPreset::parse(id).unwrap();
            assert_eq!(preset.id(), id);
            let grants = preset.grants("room", "scribe", &Subject::person("owner"));
            let authority = Authority::person(room.party("owner"))
                .bounded_by(Party::new(vec![Subject::agent("scribe")], &grants));
            let at = |node: &TreePath, kind| authority.decide(node, kind, &Context::at(0)).answer;
            assert_eq!(at(&content, ActionKind::Read), read, "{id}");
            assert_eq!(at(&content, ActionKind::Undoable), undoable, "{id}");
            assert_eq!(at(&comment, ActionKind::Undoable), comments, "{id}");
            assert_eq!(at(&content, ActionKind::Outside), outside, "{id}");
            assert_eq!(
                at(&invite, roles::GOVERNANCE_KIND),
                Answer::Deny,
                "{id} never governs"
            );
        }
        for unknown in ["admin", "owner", "Observer", "operator ", ""] {
            assert!(AgentPreset::parse(unknown).is_none());
        }
    }

    #[test]
    fn a_bot_request_names_a_canonical_entry_and_version() {
        let request = |entry: &str, version: &str| {
            WorkspaceMembershipCommand::RequestBot(BotRequest {
                entry: entry.into(),
                version: version.into(),
                inputs: std::collections::BTreeMap::new(),
                program: None,
            })
            .validate()
        };
        let with_inputs = |inputs: &[(&str, &str)]| {
            BotRequest {
                entry: "so.tilo.bots.discord-bridge".into(),
                version: "0.1.0".into(),
                inputs: inputs
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                program: None,
            }
            .validate()
        };
        with_inputs(&[("guild", "123456789012345678"), ("server", "Garden club")]).unwrap();
        assert!(
            with_inputs(&[("Guild", "1")]).is_err(),
            "names are lowercase"
        );
        assert!(
            with_inputs(&[("guild", "1\n2")]).is_err(),
            "no control characters"
        );
        assert!(with_inputs(&[("server", &"x".repeat(257))]).is_err());
        let old: BotRequest =
            serde_json::from_str(r#"{"entry":"so.tilo.bots.standup","version":"1.0.0"}"#).unwrap();
        assert!(old.inputs.is_empty());
        request("so.tilo.bots.standup", "1.2.0").unwrap();
        request("so.tilo.bots.standup", "1.2.0-rc.1+build.7").unwrap();
        for (entry, version) in [
            ("", "1.0.0"),
            ("So.Tilo.Bots", "1.0.0"),
            ("so..tilo", "1.0.0"),
            ("so.tilo bot", "1.0.0"),
            (&"x".repeat(129), "1.0.0"),
            ("so.tilo.bots.standup", ""),
            ("so.tilo.bots.standup", "1.0 beta"),
        ] {
            assert!(request(entry, version).is_err(), "{entry:?} {version:?}");
        }
        WorkspaceMembershipCommand::WithdrawBotRequest {
            entry: "so.tilo.bots.standup".into(),
        }
        .validate()
        .unwrap();
        assert!(
            WorkspaceMembershipCommand::WithdrawBotRequest {
                entry: "../standup".into(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn commands_refuse_ambiguous_or_unbounded_agent_identifiers() {
        for agent in ["", "Scribe", " scribe", "scribe\n", &"x".repeat(129)] {
            assert!(
                WorkspaceMembershipCommand::RemoveAgent {
                    agent: agent.into()
                }
                .validate()
                .is_err()
            );
        }
        WorkspaceMembershipCommand::AddAgent {
            agent: "scribe".into(),
            preset: AgentPreset::Observer,
        }
        .validate()
        .unwrap();
    }

    /// Postcard names a variant by its position; rooms keep these commands,
    /// and the digests of their administration requests, in those bytes.
    #[test]
    fn a_membership_command_keeps_its_bytes_around_the_retired_task_grants() {
        let cases: Vec<(WorkspaceMembershipCommand, Vec<u8>)> = vec![
            (
                WorkspaceMembershipCommand::RemoveAgent { agent: "a".into() },
                vec![0x01, 0x01, b'a'],
            ),
            (
                WorkspaceMembershipCommand::GrantAppAuthorship { app: "x".into() },
                vec![0x02, 0x01, b'x'],
            ),
            (
                WorkspaceMembershipCommand::RetiredGrantForTask {
                    task: "t".into(),
                    node: TreePath::room("r"),
                    kinds: KindSet::READ,
                    answer: Answer::Allow,
                },
                [
                    &[0x03, 0x01, b't', 0x06][..],
                    b"room:r",
                    &[0x01, 0x00, 0x02],
                ]
                .concat(),
            ),
            (
                WorkspaceMembershipCommand::RetiredRevokeForTask {
                    task: "t".into(),
                    grant: GrantId::new("g"),
                },
                vec![0x04, 0x01, b't', 0x01, b'g'],
            ),
            (
                WorkspaceMembershipCommand::Revoke {
                    grant: GrantId::new("g"),
                },
                vec![0x06, 0x01, b'g'],
            ),
            (
                WorkspaceMembershipCommand::WithdrawBotRequest { entry: "e".into() },
                vec![0x0f, 0x01, b'e'],
            ),
        ];
        for (command, golden) in cases {
            assert_eq!(
                postcard::to_stdvec(&command).unwrap(),
                golden,
                "{command:?}"
            );
            assert_eq!(
                postcard::from_bytes::<WorkspaceMembershipCommand>(&golden).unwrap(),
                command
            );
        }
    }

    #[test]
    fn a_retired_task_grant_command_reads_and_is_refused() {
        let held = [0x04, 0x01, b't', 0x01, b'g'];
        let command = postcard::from_bytes::<WorkspaceMembershipCommand>(&held).unwrap();
        assert_eq!(command.validate(), Err(RETIRED_TASK_GRANTS.into()));
    }
}

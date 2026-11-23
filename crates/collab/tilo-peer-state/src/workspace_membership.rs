//! Typed native workspace membership commands, not grant or identity blobs.
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Grant, GrantId, TreePath};

use crate::installation::{Installation, InstallationId};
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::delegation::Delegation;

/// A bot's device: its capability-free certificate and its installer's
/// delegation naming it the installation in this workspace.
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
    /// The program's canonical bytes, as text. The workspace keeps them so every
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

/// A bot from the registry's directory that the workspace asks its host to run:
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
    /// the workspace's `set-connector-secret` action.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub inputs: std::collections::BTreeMap<String, String>,
    /// A developer's own program for the entry, canonical text, run instead
    /// of the listed one: the workspace keeps it with the request, so every
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

/// The version of an app a workspace runs. Every member's desktop runs this
/// version, and a change made with another version waits until its author
/// runs this one: mixed versions never write each other's data unseen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRelease {
    /// The app (`so.tilo.notes`).
    pub app: String,
    /// The version its package names (`1.4.0`, `1.5.0-dev.3`).
    pub version: String,
    /// The BLAKE3 hash of the version's surface in the form it is kept by,
    /// hex: the surface every change made with this version names.
    pub surface: String,
    /// The package's `tapp:` hash (the BLAKE3 of its signed root), hex:
    /// what each member fetches and verifies before running the version.
    pub package: String,
    /// The publisher key that signed the package, hex.
    pub publisher: String,
    /// The BLAKE3 hash of the `.tapp` file, hex, when the workspace's host keeps
    /// it: members fetch it from the workspace by this hash and install it,
    /// bound to `package` and `publisher`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// A developer's own build, signed with their key and never reviewed by
    /// a registry; every window of the app says so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dev: bool,
}

/// Most bytes an app release's version takes.
const MAX_APP_VERSION: usize = 64;

impl AppRelease {
    /// Validate what the release names.
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
        ]
        .into_iter()
        .chain(self.file.iter().map(|file| ("file", file)))
        {
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

    /// The package file a device that runs `installed` (the package root it
    /// has of this app, hex, if any) fetches from the workspace to run this
    /// release: `None` when it already runs it or the workspace's host keeps no
    /// file of it.
    #[must_use]
    pub fn file_to_fetch(&self, installed: Option<&str>) -> Option<&str> {
        if installed == Some(self.package.as_str()) {
            return None;
        }
        self.file.as_deref()
    }

    /// Check `bytes` fetched from the workspace are this release's package file.
    /// The install that follows binds them to `package` and `publisher`.
    pub fn check_file(&self, bytes: &[u8]) -> Result<(), String> {
        let file = self
            .file
            .as_deref()
            .ok_or("the workspace's host keeps no package file of this version")?;
        if blake3::hash(bytes).to_hex().as_str() == file {
            Ok(())
        } else {
            Err("the fetched package does not hash to the workspace's version".into())
        }
    }
}

/// A canonical application identifier: lowercase ASCII, at most 128 bytes,
/// with non-empty `.`-separated segments.
///
/// Differs from [`tilo_app_id::AppId::parse`] by requiring lowercase ASCII,
/// capping length at 128 bytes, and allowing a single segment without a dot
/// because `validate()` filters stored rows in `state/app_versions.rs`, where
/// a dot was never required and requiring one could drop stored rows.
fn validate_app_id(app: &str) -> Result<(), String> {
    const GRAMMAR: tilo_app_id::Grammar = tilo_app_id::Grammar::CANONICAL
        .max_bytes(128)
        .lowercase_only()
        .allow_single_segment();
    if !GRAMMAR.matches(app) {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorkspaceMembershipCommand {
    /// Record `grant` in the workspace, as the caller gives it: they must hold the
    /// workspace's grant action and may give no more than they hold.
    Grant { grant: Grant },
    /// Take back the grant `grant`, as the caller: they must hold the workspace's
    /// revoke action.
    Revoke { grant: GrantId },
    /// Install a bot or update its program ([`BotInstall`]).
    InstallBot(Box<BotInstall>),
    /// Renew an installed bot's delegation, or move it to a new device key.
    RenewBot(Box<BotRenewal>),
    /// Withdraw a bot and revoke its grants; its row stays.
    UninstallBot { installation: InstallationId },
    /// Stop a bot from starting runs.
    PauseBot { installation: InstallationId },
    /// Let a paused bot start runs again.
    ResumeBot { installation: InstallationId },
    /// Change whether a bot may post under other names.
    SetBotAppearance {
        installation: InstallationId,
        appearance: crate::installation::AppearancePolicy,
    },
    /// Become a bot's installer ([`BotAdoption`]).
    AdoptBot(Box<BotAdoption>),
    /// Ask the workspace's host to run a bot from the directory ([`BotRequest`]).
    RequestBot(BotRequest),
    /// Take back the request for directory entry `entry`: its host
    /// uninstalls the bot it installed for it.
    WithdrawBotRequest { entry: String },
    /// Run this release of its app in the workspace, for every member
    /// ([`AppRelease`]).
    InstallApp(AppRelease),
    /// Stop choosing a version of `app`: each member runs the one they
    /// have.
    UninstallApp { app: String },
    /// A change to the workspace's members, as the caller makes it: a role, a
    /// removal, a ban or a restriction, or a grant given or taken back. The
    /// workspace's admission decides it against the caller's own grants where
    /// they made it, on every device that receives it.
    Member { change: tilo_action_policy::Change },
    /// A change to the workspace's own settings, as the caller makes it
    /// ([`SettingsChange`]): decided against the caller's own grants where
    /// they made it, on every device that receives it.
    Settings(SettingsChange),
    /// Share `node`, one record or one item of it, with the workspace's members
    /// as `access` gives, under the share id `share`: grants the caller
    /// gives of what they hold ([`crate::workspace::share`]).
    Share {
        share: String,
        node: TreePath,
        access: tilo_action_policy::share::ShareAccess,
    },
    /// End the share `share`: the grants of it the caller gave, or all of
    /// them when they may revoke in the workspace.
    Unshare { share: String },
    /// Claim the lease `id` of the workspace's own state for the caller's
    /// device, for `ttl_ms` from now, ranked by `rank`, or let it go
    /// (`ttl_ms` 0) ([`crate::state::leases`]). A member device that does
    /// work for the workspace — keeps its VM going — claims it as every key holder
    /// does, its claim travelling as any member's change.
    Lease {
        id: String,
        ttl_ms: u64,
        rank: neighbour_wire::placement::Rank,
    },
    /// Keep that the caller's device answered the schedule at `trigger` of the
    /// automation `automation` through `answered_ms`, the schedule marked
    /// `mark` ([`crate::state::schedules`]): the progress whichever device
    /// fires the schedule next carries on from, written from a member device
    /// as any member's change.
    AnswerSchedule {
        automation: String,
        trigger: u32,
        mark: String,
        answered_ms: i64,
    },
    /// Keep that the caller's device did the work of the lease `id`, and
    /// what it answered ([`crate::state::done`]): no device takes the work
    /// again.
    KeepDone {
        id: String,
        answer: serde_json::Value,
    },
    /// Hold the person whose user root is `person` in the workspace at `place`,
    /// whether or not a device of theirs has joined yet: a role, one of its
    /// administrators, or its owner, whom the caller then hands it to. A member
    /// already is moved to the place, which takes an administrator's grants
    /// back when it is a role. `name`, when it carries one, is how the
    /// workspace names them until they name themselves. Decided against the
    /// caller's own grants where they made it, on every device that receives
    /// it: an administrator is unmade and the workspace handed on by its owner
    /// alone.
    Place {
        person: [u8; 32],
        place: MemberPlace,
        name: Option<String>,
    },
    /// Change what the call in voice channel `channel` allows: end or restart it, deny a person
    /// or let them back, or set what everyone in it may do. Decided against the caller's own
    /// grants (`call`) where they made it, on every device that receives it.
    Call {
        channel: String,
        change: collab_types::calls::CallChange,
    },
}

/// Where [`WorkspaceMembershipCommand::Place`] puts a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemberPlace {
    /// A member at this role, governing nothing.
    Role(crate::join::Role),
    /// An editor holding every governance action.
    Administrator,
    /// The workspace's owner: the owner the caller is hands the workspace to them.
    Owner,
}

impl MemberPlace {
    /// The role an invitation for this place holds, and whether its grant makes the person an
    /// administrator: an administrator joins as an Editor and is made one by the invitation's
    /// own grant, never by a role above the inviter's; an owner is an administrator as well.
    #[must_use]
    pub fn invited_as(self) -> (crate::join::Role, bool) {
        match self {
            Self::Role(role) => (role, false),
            Self::Administrator | Self::Owner => (crate::join::Role::Editor, true),
        }
    }

    /// What [`Self::parse`] reads this place from.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Role(role) => role.key(),
            Self::Administrator => "administrator",
            Self::Owner => "owner",
        }
    }

    /// The place `text` names: a role, `administrator` or `owner`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "administrator" => Some(Self::Administrator),
            "owner" => Some(Self::Owner),
            role => crate::join::Role::parse(role)
                .filter(|role| role.assignable())
                .map(Self::Role),
        }
    }
}

/// The longest lease id a member claims.
const MAX_LEASE_ID: usize = 256;

/// A change to the workspace's own settings: its name, its saved grant sets and
/// their assignment, its apps' sandbox capabilities and its saved shortcuts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SettingsChange {
    /// Name the workspace `name`.
    Rename { name: String },
    /// Save a named grant set, replacing the one with the same name.
    SavePreset { preset: tilo_action_policy::Preset },
    /// Give `subject` the grants the saved set `preset` expands to, until
    /// `lifetime` ends.
    AssignPreset {
        preset: String,
        subject: tilo_action_policy::Grantee,
        lifetime: tilo_action_policy::Lifetime,
    },
    /// Record a sandbox capability for the workspace's apps.
    GrantSandbox {
        capability: collab_types::caps::CapOverride,
    },
    /// Take back the sandbox capability with this id.
    RevokeSandbox { capability: String },
    /// Save a shortcut of the workspace, or remove the one with this id, as its
    /// saver's own device signed it ([`tilo_automation::ShortcutChange`]).
    Shortcuts(tilo_automation::ShortcutChange),
    /// Settle the workspace's entry: whether a directory lists it, whether it
    /// takes people by invitation only, and how many requests it keeps
    /// ([`tilo_action_policy::EntryPolicy`]).
    Entry(tilo_action_policy::EntryPolicy),
}

/// The name a workspace is founded with, trimmed: none, which every reader
/// shows in their own language, or a name [`valid_workspace_name`] takes.
///
/// # Errors
///
/// A name that is too long or holds control characters.
pub fn valid_founding_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(name);
    }
    valid_workspace_name(name)
}

/// The workspace's name, trimmed, when it is 1 to 80 characters and no control
/// characters.
pub fn valid_workspace_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if !(1..=80).contains(&name.chars().count()) || name.chars().any(char::is_control) {
        return Err("workspace name must contain 1–80 characters".into());
    }
    Ok(name)
}

impl WorkspaceMembershipCommand {
    /// Validate the canonical identifier before entering the owner turn.
    pub fn validate(&self) -> Result<(), String> {
        if let Self::Lease { id, .. } | Self::KeepDone { id, .. } = self {
            if id.is_empty() || id.len() > MAX_LEASE_ID || id.chars().any(char::is_control) {
                return Err("invalid lease id".into());
            }
            return Ok(());
        }
        if let Self::AnswerSchedule {
            automation, mark, ..
        } = self
        {
            if [automation, mark].iter().any(|part| {
                part.is_empty() || part.len() > MAX_LEASE_ID || part.chars().any(char::is_control)
            }) {
                return Err("invalid schedule answer".into());
            }
            return Ok(());
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
        if let Self::Call { channel, .. } = self {
            return collab_types::calls::validate_channel(channel);
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
        if let Self::Member { change } = self {
            return validate_member_change(change);
        }
        if let Self::Place { name, .. } = self {
            return name.as_deref().map_or(Ok(()), |name| {
                tilo_user_identity::display_name(name)
                    .map(drop)
                    .map_err(|error| error.reason().to_owned())
            });
        }
        if let Self::Settings(change) = self {
            return match change {
                SettingsChange::Rename { name } => valid_workspace_name(name).map(drop),
                SettingsChange::SavePreset { preset } => {
                    preset.validate().map_err(|error| error.to_string())
                }
                SettingsChange::Entry(entry) => entry
                    .clone()
                    .checked()
                    .map(drop)
                    .map_err(|error| error.to_string()),
                // Each is checked where it is written: a preset and a sandbox
                // capability against the writer's own grants, and a shortcut
                // against what the broker offers its saver, held to the proof
                // of who saved it that its workspace's admission asks for.
                SettingsChange::AssignPreset { .. }
                | SettingsChange::GrantSandbox { .. }
                | SettingsChange::RevokeSandbox { .. }
                | SettingsChange::Shortcuts(_) => Ok(()),
            };
        }
        if matches!(self, Self::Share { .. } | Self::Unshare { .. }) {
            // The share's id and node are checked as its grants are made.
            return Ok(());
        }
        Ok(())
    }
}

/// Whether `agent` is an id a workspace names an agent by: lowercase, trimmed,
/// bounded, with no control characters.
pub fn valid_agent_id(agent: &str) -> Result<(), String> {
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

/// A member change names its person by their user root key, and a grant
/// it gives by an id.
fn validate_member_change(change: &tilo_action_policy::Change) -> Result<(), String> {
    use tilo_action_policy::Change;
    let person = match change {
        Change::AddMember { person, .. }
        | Change::RemoveMember { person }
        | Change::Ban { person }
        | Change::Unban { person }
        | Change::Restrict { person, .. }
        | Change::Unrestrict { person }
        | Change::SetRole { person, .. }
        | Change::SetMemberCustomRole { person, .. }
        | Change::TransferOwnership { person } => person,
        Change::RetiredSetMemberRoles { .. } => {
            return Err("roles are assigned one at a time".into());
        }
        Change::Grant(grant) if grant.id.as_str().trim().is_empty() => {
            return Err("a grant needs an id".into());
        }
        Change::Grant(_)
        | Change::Revoke(_)
        | Change::DefineRole { .. }
        | Change::RemoveRole { .. } => return Ok(()),
    };
    if person.len() != 64 || hex::decode(person).is_err() {
        return Err("a member is named by their user root key".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn bot_and_agent_identifiers_are_canonical;

        phase a_bot_request_names_a_canonical_entry_and_version;
        {
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

        phase agent_identifiers_are_canonical;
        {
            for agent in ["", "Scribe", " scribe", "scribe\n", &"x".repeat(129)] {
                assert!(valid_agent_id(agent).is_err(), "{agent:?}");
            }
            valid_agent_id("scribe").unwrap();
        }
    }

    /// Postcard names a variant by its position; workspaces keep these
    /// commands, and the digests of their administration requests, in those
    /// bytes.
    #[test]
    fn a_membership_command_keeps_its_bytes() {
        {
            let cases: Vec<(WorkspaceMembershipCommand, Vec<u8>)> = vec![
                (
                    WorkspaceMembershipCommand::Revoke {
                        grant: GrantId::new("g"),
                    },
                    vec![0x01, 0x01, b'g'],
                ),
                (
                    WorkspaceMembershipCommand::WithdrawBotRequest { entry: "e".into() },
                    vec![0x0a, 0x01, b'e'],
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
    }

    #[test]
    fn app_release_and_uninstall_ids_are_lowercase_bounded_and_non_empty_segments() {
        let longest_dotted = format!("a.{}", "b".repeat(126));
        let longest_single = "a".repeat(128);
        let too_long = format!("a.{}", "b".repeat(127));
        let too_long_single = "a".repeat(129);
        let hex64 = |ch: &str| ch.repeat(64);
        for (app, expected) in [
            ("so.tilo.notes", true),
            ("so.tilo.my_app-2", true),
            ("test.app", true),
            ("notes", true),
            ("app", true),
            ("app-1c8152bfbdd80870708389a79cf20728", true),
            ("-notes", true),
            ("-so.tilo.notes", true),
            (longest_dotted.as_str(), true),
            (longest_single.as_str(), true),
            (too_long.as_str(), false),
            (too_long_single.as_str(), false),
            ("", false),
            ("So.Tilo.Notes", false),
            ("so.tilo.Notes", false),
            (".so.tilo", false),
            ("so.tilo.", false),
            ("so..tilo", false),
            ("..", false),
            (".", false),
            ("so.tilo notes", false),
            ("../notes", false),
        ] {
            let install = WorkspaceMembershipCommand::InstallApp(AppRelease {
                app: app.to_owned(),
                version: "1.0.0".into(),
                surface: hex64("a"),
                package: hex64("b"),
                publisher: hex64("c"),
                file: None,
                dev: false,
            });
            assert_eq!(install.validate().is_ok(), expected, "install {app:?}");
            let uninstall = WorkspaceMembershipCommand::UninstallApp {
                app: app.to_owned(),
            };
            assert_eq!(uninstall.validate().is_ok(), expected, "uninstall {app:?}");
        }
    }
}

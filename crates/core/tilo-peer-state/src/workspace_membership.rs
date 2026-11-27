//! Typed native workspace membership commands, not grant or identity blobs.
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Grant, GrantId, TreePath};

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
    /// hex: the surface every change made with this version names. Empty for a
    /// version that publishes no surface, and for a Flatpak.
    pub surface: String,
    /// The package's `tapp:` hash (the BLAKE3 of its signed root), hex:
    /// what each member fetches and verifies before running the version.
    /// Empty for a Flatpak, which names its payload by reference instead.
    pub package: String,
    /// The publisher key that signed the package, hex. Empty for a Flatpak.
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
    /// The workspace keeps this version: an update is refused until a member unpins it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// What each device installs to run the app.
    #[serde(default, skip_serializing_if = "AppPayload::is_tapp")]
    pub payload: AppPayload,
}

/// What a device installs to run an app version the workspace runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AppPayload {
    /// The Tilo package the release names (`package`, `publisher`, `file`).
    #[default]
    Tapp,
    /// A Flatpak (`app/org.example.App/x86_64/stable`), installed into the system's Flatpak
    /// installation of each device that can run it.
    Flatpak { reference: String },
}

impl AppPayload {
    /// Whether the payload is a Tilo package, the one a release names unless it says otherwise.
    #[must_use]
    pub const fn is_tapp(&self) -> bool {
        matches!(self, Self::Tapp)
    }

    /// Whether the payload is a Flatpak.
    #[must_use]
    pub const fn is_flatpak(&self) -> bool {
        matches!(self, Self::Flatpak { .. })
    }

    /// The Flatpak reference (`app/org.example.App/x86_64/stable`) this payload
    /// names, if it is one.
    #[must_use]
    pub fn flatpak_reference(&self) -> Option<&str> {
        match self {
            Self::Tapp => None,
            Self::Flatpak { reference } => Some(reference),
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Tapp => Ok(()),
            Self::Flatpak { reference }
                if !reference.is_empty()
                    && reference.len() <= MAX_FLATPAK_REFERENCE
                    && reference.split('/').count() == 4
                    && reference.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'/')
                    }) =>
            {
                Ok(())
            }
            Self::Flatpak { .. } => Err(
                "a Flatpak is named kind/id/arch/branch, as `app/org.example.App/x86_64/stable`"
                    .into(),
            ),
        }
    }
}

/// Most bytes an app release's version takes.
const MAX_APP_VERSION: usize = 64;
/// Most bytes a Flatpak reference takes.
const MAX_FLATPAK_REFERENCE: usize = 255;

/// Check `hash` is 64 lowercase hex digits.
fn check_hash(what: &str, hash: &str) -> Result<(), String> {
    if hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("the app's {what} is not 64 lowercase hex digits"))
    }
}

/// The app id a Flatpak reference (`app/org.example.App/x86_64/stable`) is
/// recorded under in a workspace's tree: its application id, lowercased, so
/// every device names the same record. `None` for anything but an application
/// reference.
#[must_use]
pub fn flatpak_app_id(reference: &str) -> Option<String> {
    let mut parts = reference.split('/');
    let (kind, id, arch, branch) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || kind != "app"
        || id.is_empty()
        || arch.is_empty()
        || branch.is_empty()
    {
        return None;
    }
    Some(id.to_ascii_lowercase())
}

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
        match &self.payload {
            AppPayload::Tapp => {
                // A Tilo package names the package each member verifies and its publisher; a
                // version that publishes no surface (a language pack) names none.
                check_hash("package", &self.package)?;
                check_hash("publisher", &self.publisher)?;
                if !self.surface.is_empty() {
                    check_hash("surface", &self.surface)?;
                }
                if let Some(file) = &self.file {
                    check_hash("file", file)?;
                }
            }
            AppPayload::Flatpak { .. } => {
                // A Flatpak carries none of a Tilo package's fields: its reference is its payload.
                for (what, present) in [
                    ("surface", !self.surface.is_empty()),
                    ("package", !self.package.is_empty()),
                    ("publisher", !self.publisher.is_empty()),
                    ("file", self.file.is_some()),
                ] {
                    if present {
                        return Err(format!(
                            "a Flatpak release carries no {what}: its payload is its reference"
                        ));
                    }
                }
            }
        }
        self.payload.validate()
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
/// Differs from [`tilo_member_types::AppId::parse`] by requiring lowercase ASCII,
/// capping length at 128 bytes, and allowing a single segment without a dot
/// because `validate()` filters stored rows in `state/app_versions.rs`, where
/// a dot was never required and requiring one could drop stored rows.
fn validate_app_id(app: &str) -> Result<(), String> {
    const GRAMMAR: tilo_member_types::Grammar = tilo_member_types::Grammar::CANONICAL
        .max_bytes(128)
        .lowercase_only()
        .allow_single_segment();
    if !GRAMMAR.matches(app) {
        return Err("invalid canonical application identifier".into());
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
    /// Add, replace or remove an attestor whose signature the workspace
    /// accepts ([`TrustAttestor`](crate::TrustAttestor)).
    AddAttestor(crate::TrustAttestor),
    /// Remove the attestor `id` from the workspace's trust list.
    RemoveAttestor { id: String },
    /// Refuse every package publisher key `pubkey` signed, from now on.
    RevokePublisher { pubkey: String, reason: String },
    /// Add or replace a registry the workspace fetches apps from
    /// ([`PackageRegistry`](crate::PackageRegistry)).
    AddRegistry(crate::PackageRegistry),
    /// Stop reading the registry `name`.
    RemoveRegistry { name: String },
    /// Read the registry `name`, or stop reading it.
    SetRegistryEnabled { name: String, enabled: bool },
    /// Offer `channel`'s releases from the registry `name`.
    SetRegistryChannel { name: String, channel: String },
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
    /// Keep what the caller's device made of `trigger` of the automation
    /// `automation` at its last look, the trigger marked `mark`
    /// ([`crate::state::schedules`]): what a feed's or a page's looks took in,
    /// which moments of a calendar were answered. Whichever device looks next
    /// carries on from it, written from a member device as any member's
    /// change.
    KeepTriggerProgress {
        automation: String,
        trigger: u32,
        mark: String,
        kept_ms: i64,
        progress: serde_json::Value,
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
    /// Settle the workspace's content policy: the largest file a host keeps,
    /// and what one member's files may take, now and in any hour
    /// ([`tilo_action_policy::ContentPolicy`]).
    Content(tilo_action_policy::ContentPolicy),
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
            return valid_lease_id(id);
        }
        if let Self::AnswerSchedule {
            automation, mark, ..
        }
        | Self::KeepTriggerProgress {
            automation, mark, ..
        } = self
        {
            return valid_schedule_answer(automation, mark);
        }
        if let Self::AddAttestor(attestor) = self {
            return if crate::state::package_trust::valid_attestor(attestor) {
                Ok(())
            } else {
                Err("an attestor needs an id, a 64-hex key and a scope of `all`, `mime:` or `prefix:`".into())
            };
        }
        if let Self::RemoveAttestor { id } = self {
            return valid_attestor_id(id);
        }
        if let Self::RevokePublisher { pubkey, .. } = self {
            return if crate::state::package_trust::valid_pubkey(pubkey) {
                Ok(())
            } else {
                Err("a revoked key is an Ed25519 key in hex".into())
            };
        }
        if let Self::AddRegistry(registry) = self {
            return if crate::state::package_registries::valid_registry(registry) {
                Ok(())
            } else {
                Err("a registry needs a name, a credential-free address, a pinned 64-hex owner key and a known channel".into())
            };
        }
        if let Self::RemoveRegistry { name } | Self::SetRegistryEnabled { name, .. } = self {
            return valid_registry_name(name);
        }
        if let Self::SetRegistryChannel { name, channel } = self {
            valid_registry_name(name)?;
            if !crate::state::package_registries::CHANNELS.contains(&channel.as_str()) {
                return Err("a registry offers `stable` or `development` releases".into());
            }
            return Ok(());
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
                | SettingsChange::Content(_)
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

/// `id`, when it is one a lease or a schedule answer is named by: 1 to `MAX_LEASE_ID` bytes, no
/// control characters.
fn valid_lease_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_LEASE_ID || id.chars().any(char::is_control) {
        return Err("invalid lease id".into());
    }
    Ok(())
}

/// `automation` and `mark`, when both are ids a lease is named by.
fn valid_schedule_answer(automation: &str, mark: &str) -> Result<(), String> {
    if [automation, mark].iter().any(|part| {
        part.is_empty() || part.len() > MAX_LEASE_ID || part.chars().any(char::is_control)
    }) {
        return Err("invalid schedule answer".into());
    }
    Ok(())
}

/// `name`, when it names a package registry.
fn valid_registry_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err("invalid registry name".into());
    }
    Ok(())
}

/// `id`, when it names an attestor.
fn valid_attestor_id(id: &str) -> Result<(), String> {
    if id.trim().is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err("invalid attestor id".into());
    }
    Ok(())
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
        fn agent_identifiers_are_canonical;

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
                    WorkspaceMembershipCommand::UninstallApp { app: "e".into() },
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
                pinned: false,
                payload: AppPayload::Tapp,
            });
            assert_eq!(install.validate().is_ok(), expected, "install {app:?}");
            let uninstall = WorkspaceMembershipCommand::UninstallApp {
                app: app.to_owned(),
            };
            assert_eq!(uninstall.validate().is_ok(), expected, "uninstall {app:?}");
        }
    }

    #[test]
    fn a_language_pack_release_names_no_surface() {
        let pack = WorkspaceMembershipCommand::InstallApp(AppRelease {
            app: "so.tilo.language-pack.de".into(),
            version: "1.0.0".into(),
            surface: String::new(),
            package: "b".repeat(64),
            publisher: "c".repeat(64),
            file: Some("d".repeat(64)),
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        });
        assert_eq!(pack.validate(), Ok(()));
        let no_package = WorkspaceMembershipCommand::InstallApp(AppRelease {
            app: "so.tilo.language-pack.de".into(),
            version: "1.0.0".into(),
            surface: String::new(),
            package: String::new(),
            publisher: "c".repeat(64),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        });
        assert!(
            no_package.validate().is_err(),
            "a Tilo package names its package"
        );
    }

    #[test]
    fn a_flatpak_release_carries_only_its_reference() {
        let reference = "app/org.gnome.Calculator/x86_64/stable";
        assert_eq!(
            flatpak_app_id(reference).as_deref(),
            Some("org.gnome.calculator")
        );
        assert_eq!(
            flatpak_app_id("runtime/org.freedesktop.Platform/x86_64/24.08"),
            None
        );
        let flatpak = WorkspaceMembershipCommand::InstallApp(AppRelease {
            app: flatpak_app_id(reference).expect("an application reference"),
            version: "stable".into(),
            surface: String::new(),
            package: String::new(),
            publisher: String::new(),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Flatpak {
                reference: reference.into(),
            },
        });
        assert_eq!(flatpak.validate(), Ok(()));
        let with_package = WorkspaceMembershipCommand::InstallApp(AppRelease {
            app: "org.gnome.calculator".into(),
            version: "stable".into(),
            surface: String::new(),
            package: "b".repeat(64),
            publisher: String::new(),
            file: None,
            dev: false,
            pinned: false,
            payload: AppPayload::Flatpak {
                reference: reference.into(),
            },
        });
        assert!(
            with_package.validate().is_err(),
            "a Flatpak carries no package"
        );
    }
}

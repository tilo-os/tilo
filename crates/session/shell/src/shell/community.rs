//! Where this device finds the Tilo community and the Tilo team.
//!
//! Feedback goes to one of two rooms (`docs/architecture/feedback.md`): the
//! public **Tilo Community** room, joined by redeeming its standing invitation,
//! and the reporter's private direct room with the **Tilo team principal**.
//! Both are facts about the OS release, not about the person, so they ship in
//! the image as `/usr/share/tilo/community.toml`:
//!
//! ```toml
//! [community]
//! # The community room's standing invitation. Its host addresses say where
//! # the room is served; redeeming it admits this device as a Commenter.
//! invite = "…"
//!
//! [team]
//! # The Tilo team's user root key (hex). Private feedback goes to the direct
//! # room between the reporter and this person.
//! user_public_key = "…"
//! label = "Tilo team"
//! ```
//!
//! `TILO_COMMUNITY_CONFIG` names a different file, which is how a local relay
//! and nested shells point at a development community. Nothing else differs
//! between the development and production paths. A missing file or section is
//! "not configured", reported as such, never guessed around.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::shell::state::WorkspaceAuthority as _;

/// Where the OS image ships the community configuration.
pub const DEFAULT_COMMUNITY_CONFIG: &str = "/usr/share/tilo/community.toml";
/// Names a community configuration to use instead of the shipped one.
pub const COMMUNITY_CONFIG_ENV: &str = "TILO_COMMUNITY_CONFIG";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunityFile {
    #[serde(default)]
    community: Option<CommunitySection>,
    #[serde(default)]
    team: Option<TeamSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommunitySection {
    #[serde(default)]
    invite: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamSection {
    #[serde(default)]
    user_public_key: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// The Tilo team principal: the person private feedback is addressed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamPrincipal {
    /// The team's user root key.
    pub user_public_key: [u8; 32],
    /// What to call the team on screen.
    pub label: String,
}

impl TeamPrincipal {
    /// The team as a room member: the same person id every membership and
    /// direct room uses (`Principal::person_from_key`).
    #[must_use]
    pub fn person_id(&self) -> String {
        collab_types::caps::Principal::person_from_key(collab_types::Ed25519PublicKey::from_bytes(
            self.user_public_key,
        ))
        .id()
        .to_string()
    }
}

/// This device's community configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommunityConfig {
    /// Where it was read from, for messages.
    pub source: PathBuf,
    /// The community room's standing invitation, when configured.
    pub invite: Option<String>,
    /// The team principal, when configured.
    pub team: Option<TeamPrincipal>,
}

/// Why the community configuration could not be read.
#[derive(Debug, thiserror::Error)]
pub enum CommunityConfigError {
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("{path}: team.user_public_key must be 32 bytes of hex")]
    TeamKey { path: PathBuf },
}

/// The configuration file this device uses: `TILO_COMMUNITY_CONFIG` when set
/// and non-empty, otherwise the shipped one — `/usr/share/tilo`, or, for a
/// package installed under another prefix (Nix), the first
/// `tilo/community.toml` in `XDG_DATA_DIRS`.
#[must_use]
pub fn config_path() -> PathBuf {
    resolve_config_path(
        std::env::var_os(COMMUNITY_CONFIG_ENV),
        std::env::var_os("XDG_DATA_DIRS").as_deref(),
    )
}

fn resolve_config_path(
    configured: Option<std::ffi::OsString>,
    data_dirs: Option<&std::ffi::OsStr>,
) -> PathBuf {
    if let Some(path) = configured.filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    let shipped = PathBuf::from(DEFAULT_COMMUNITY_CONFIG);
    if shipped.exists() {
        return shipped;
    }
    data_dirs
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("tilo").join("community.toml"))
        .find(|candidate| candidate.is_file())
        .unwrap_or(shipped)
}

/// Read [`config_path`]. A file that does not exist is an unconfigured
/// community, not an error.
///
/// # Errors
///
/// A file that exists but cannot be read or parsed, or names an invalid team
/// key.
pub fn load() -> Result<CommunityConfig, CommunityConfigError> {
    load_from(&config_path())
}

/// [`load`] from an explicit path.
///
/// # Errors
///
/// As [`load`].
pub fn load_from(path: &Path) -> Result<CommunityConfig, CommunityConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CommunityConfig {
                source: path.to_path_buf(),
                ..CommunityConfig::default()
            });
        }
        Err(source) => {
            return Err(CommunityConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let file: CommunityFile =
        toml::from_str(&text).map_err(|source| CommunityConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
    let invite = file
        .community
        .and_then(|community| community.invite)
        .map(|invite| invite.trim().to_string())
        .filter(|invite| !invite.is_empty());
    let team = match file.team {
        Some(TeamSection {
            user_public_key: Some(key),
            label,
        }) if !key.trim().is_empty() => {
            let user_public_key = hex::decode(key.trim())
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .ok_or_else(|| CommunityConfigError::TeamKey {
                    path: path.to_path_buf(),
                })?;
            Some(TeamPrincipal {
                user_public_key,
                label: label
                    .map(|label| label.trim().to_string())
                    .filter(|label| !label.is_empty())
                    .unwrap_or_else(|| "Tilo team".to_string()),
            })
        }
        _ => None,
    };
    Ok(CommunityConfig {
        source: path.to_path_buf(),
        invite,
        team,
    })
}

/// The direct room between the person `me` (a person id) and the team: the
/// same derived id on both ends, so neither has to invite the other.
#[must_use]
pub fn team_direct_room(me: &str, team: &TeamPrincipal) -> Option<crate::shell::state::RoomId> {
    let room = tilo_reference::DirectRoom::between(me, &team.person_id())?;
    crate::shell::state::RoomId::parse(&room.room_id())
}

/// Everyone is in the community by default. How long the shell waits before
/// asking again when a join has not gone through yet (offline, or the host
/// not reached), doubling up to [`AUTO_JOIN_MAX_BACKOFF`].
pub const AUTO_JOIN_FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
/// The longest wait between two join attempts.
pub const AUTO_JOIN_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(300);

/// What the session knows about the community when it decides what to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    /// The session has a certified identity to join with.
    pub identity: bool,
    /// The id (hex) of the configured standing invitation, if any.
    pub invite: Option<String>,
    /// The person's remembered choice.
    pub membership: settings_store::community::Membership,
    /// The invitation the community's host refused, as remembered.
    pub refused_invite: Option<String>,
    /// This session holds the community room.
    pub held: bool,
    /// This session holds a role in it: the host admitted it.
    pub admitted: bool,
}

/// The next thing the shell does about the community.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing, for now.
    Wait,
    /// Redeem the standing invitation: the session is not in the room.
    Join,
    /// Ask the room's hosts again: the session holds the room, but the host
    /// has not admitted it yet.
    Redeem,
    /// Open the private room with the Tilo team, once.
    OpenTeamRoom,
    /// Leave the room: the person left, or its host refused this device.
    Leave,
    /// Remember that the host refused the invitation, then leave.
    RecordRefusal,
}

/// Keeps a session in the Tilo Community without asking: it joins once the
/// session has an identity and the network is up, retries with back-off
/// while the host cannot be reached, never re-joins a person who left, and
/// takes a refusal by the host as final for that invitation. Pure: the
/// caller observes, performs the step and reports back.
#[derive(Clone, Debug)]
pub struct AutoJoin {
    invite: Option<String>,
    due: Option<std::time::Instant>,
    backoff: std::time::Duration,
    team_room_opened: bool,
    refused: Option<String>,
}

impl Default for AutoJoin {
    fn default() -> Self {
        Self {
            invite: None,
            due: None,
            backoff: AUTO_JOIN_FIRST_BACKOFF,
            team_room_opened: false,
            refused: None,
        }
    }
}

impl AutoJoin {
    /// Decide the next step. A different invitation (a new release) starts
    /// over.
    pub fn next(&mut self, now: std::time::Instant, observed: &Observed) -> Step {
        use settings_store::community::Membership;
        if self.invite != observed.invite {
            *self = Self {
                invite: observed.invite.clone(),
                ..Self::default()
            };
        }
        let Some(invite) = observed.invite.as_deref() else {
            return Step::Wait;
        };
        if !observed.identity {
            return Step::Wait;
        }
        if observed.membership == Membership::Left {
            return if observed.held {
                Step::Leave
            } else {
                Step::Wait
            };
        }
        if self.refused.as_deref() == Some(invite)
            && observed.refused_invite.as_deref() != Some(invite)
        {
            return Step::RecordRefusal;
        }
        if observed.refused_invite.as_deref() == Some(invite) {
            return if observed.held && !observed.admitted {
                Step::Leave
            } else {
                Step::Wait
            };
        }
        if observed.admitted {
            self.due = None;
            self.backoff = AUTO_JOIN_FIRST_BACKOFF;
            return if self.team_room_opened {
                Step::Wait
            } else {
                Step::OpenTeamRoom
            };
        }
        if self.due.is_some_and(|due| now < due) {
            return Step::Wait;
        }
        if observed.held {
            Step::Redeem
        } else {
            Step::Join
        }
    }

    /// A join or redemption was attempted at `now`: the next one waits,
    /// longer each time until the host admits the session.
    pub fn attempted(&mut self, now: std::time::Instant) {
        self.due = now.checked_add(self.backoff);
        self.backoff = self.backoff.saturating_mul(2).min(AUTO_JOIN_MAX_BACKOFF);
    }

    /// The team room was opened, or cannot be (no team configured, or this
    /// session is the team); either way it is not tried again.
    pub fn team_room_opened(&mut self) {
        self.team_room_opened = true;
    }

    /// The community's host refused the invitation `invite` (hex).
    pub fn refused(&mut self, invite: &str) {
        self.refused = Some(invite.to_string());
    }

    /// The person chose to join again, or to leave: whatever was pending
    /// starts over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// How a direct room is set up on this end.
#[derive(Clone, Copy)]
pub struct DirectRoomSetup<'a> {
    pub room: crate::shell::state::RoomId,
    /// The other member of the pair: the team, from a reporter's end.
    pub other: &'a str,
    pub label: &'a str,
    /// This end creates and governs the room (the reporter's end).
    pub governing: bool,
    /// Admit the team's triage agent, operated by the team, when the room is
    /// created here. Once only: a room that already exists is left as its
    /// owner made it, so an agent the reporter removed stays removed.
    pub triage: bool,
}

/// Whether the team has its triage agent join reporters' direct rooms, as
/// the community room `community` says: its governance turns it on, and the
/// agent is admitted there.
pub fn team_triages_direct_rooms(
    tree: &crate::shell::state::WorkspaceTree,
    community: crate::shell::state::RoomId,
) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == community)
        .is_some_and(|workspace| {
            workspace.triage_in_direct_rooms
                && workspace
                    .role_of_agent(crate::shell::workspace_settings::TRIAGE_AGENT)
                    .is_some_and(|role| role != collab_types::role::Role::Denied)
        })
}

/// Hold the direct room in `tree`, as [`DirectRoomSetup`] says: a new room
/// is created, and on the governing end owned here, with the other member an
/// Editor and, when asked, the team's triage agent admitted with the team as
/// its operator.
///
/// # Errors
///
/// When this end governs the room and may not administer it.
pub fn prepare_direct_room(
    tree: &mut crate::shell::state::WorkspaceTree,
    setup: &DirectRoomSetup<'_>,
) -> Result<(), String> {
    use crate::shell::state::{CapOverride, Subject, Verb};
    let room = setup.room;
    let held = tree
        .desktop
        .workspaces
        .iter()
        .any(|workspace| workspace.room == room);
    let me = crate::shell::state::local_user_id_for_room(&room);
    if !held {
        let local = tree.append_empty_workspace(setup.label);
        tree.set_room(local, room);
        if setup.governing {
            // Joining a derived room surrenders governance by default; this
            // end creates it, so it governs it.
            tree.room_authority.remove(&room);
            if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|ws| ws.id == local) {
                workspace.set_owner(me.clone());
                if setup.triage {
                    let room_id = room.to_string();
                    let agent = crate::shell::workspace_settings::TRIAGE_AGENT;
                    for cap in
                        crate::shell::workspace_settings::triage_agent_grants(&room_id, agent)
                            .into_iter()
                            .chain(std::iter::once(
                                crate::shell::workspace_settings::triage_operator_grant(
                                    &room_id,
                                    agent,
                                    setup.other,
                                ),
                            ))
                    {
                        workspace
                            .grant_as(&me, cap)
                            .ok_or_else(|| format!("{me} may not administer room {room}"))?;
                    }
                }
            }
        }
        // Otherwise the other end created and governs it: `set_room` left the
        // authority unpinned, and only the other member of the derived pair
        // may supply its first governance.
    }
    // Idempotent: the creating end makes sure the other member holds Editor,
    // whether the room is new or was opened before.
    if setup.governing && tree.governs_room(room) {
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
            .ok_or_else(|| format!("no workspace holds room {room}"))?;
        if workspace.role_of(&setup.other.to_string()).is_none() {
            workspace
                .grant_as(
                    &me,
                    CapOverride::membership(
                        Subject::person(setup.other.to_string()),
                        room.to_string(),
                        Verb::Write,
                    ),
                )
                .ok_or_else(|| format!("{me} may not administer room {room}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// With the team's choice on, a reporter's new direct room admits the
    /// triage agent with the team as its operator; the team may launch it
    /// there; the reporter removes it, and opening the room again does not
    /// bring it back.
    #[test]
    fn a_reporters_direct_room_admits_the_team_triage_agent_once() {
        use crate::shell::state::{RoomId, WorkspaceTree};
        use crate::shell::workspace_settings::{TRIAGE_AGENT, remove_agent};
        const TEAM: &str = "team-person";
        let room = RoomId::parse("7a1a9e00-0000-4c0d-9a11-0000000000d1").expect("room id");
        let setup = super::DirectRoomSetup {
            room,
            other: TEAM,
            label: "Tilo team",
            governing: true,
            triage: true,
        };
        let mut tree = WorkspaceTree::default();
        super::prepare_direct_room(&mut tree, &setup).expect("the room is created");
        let me = crate::shell::state::local_user_id_for_room(&room);
        let now = tilo_clock::unix_now();
        {
            let ws = tree
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.room == room)
                .expect("the room is held");
            assert_eq!(
                ws.role_of_agent(TRIAGE_AGENT),
                Some(collab_types::role::Role::Commenter)
            );
            assert!(ws.may_operate_agent_at(&TEAM.to_string(), TRIAGE_AGENT, now));
            assert!(
                !ws.may_operate_agent_at(&TEAM.to_string(), "recorder", now),
                "the operator grant names exactly the triage agent"
            );
            assert!(
                !ws.may_administer(&TEAM.to_string()),
                "the team operates its agent without governing the room"
            );
        }

        let ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
            .expect("the room is held");
        let removed = remove_agent(ws, &me, TRIAGE_AGENT);
        assert_eq!(
            removed.revoked.len(),
            3,
            "membership, Chat and the operator grant"
        );
        assert!(!ws.may_operate_agent_at(&TEAM.to_string(), TRIAGE_AGENT, now));

        super::prepare_direct_room(&mut tree, &setup).expect("opened again");
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room == room)
            .expect("the room is held");
        assert_eq!(ws.role_of_agent(TRIAGE_AGENT), None, "a removal sticks");
        assert!(
            ws.role_of(&TEAM.to_string()).is_some(),
            "the team is still a member"
        );
    }

    use super::*;

    #[test]
    fn a_missing_file_is_an_unconfigured_community() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_from(&dir.path().join("community.toml")).unwrap();
        assert_eq!(config.invite, None);
        assert_eq!(config.team, None);
    }

    #[test]
    fn the_shipped_shape_parses_and_names_the_team_as_a_person() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("community.toml");
        let key = [0x5E; 32];
        std::fs::write(
            &path,
            format!(
                "[community]\ninvite = \" token \"\n\n[team]\nuser_public_key = \"{}\"\n",
                hex::encode(key)
            ),
        )
        .unwrap();
        let config = load_from(&path).unwrap();
        assert_eq!(config.invite.as_deref(), Some("token"));
        let team = config.team.unwrap();
        assert_eq!(team.label, "Tilo team");
        assert_eq!(team.person_id(), hex::encode(key));

        let me = hex::encode([0x11; 32]);
        let room = team_direct_room(&me, &team).unwrap();
        assert_eq!(
            Some(room),
            team_direct_room(&me, &team),
            "one room, derived the same way on both ends"
        );
        assert_eq!(team_direct_room(&team.person_id(), &team), None);
    }

    #[test]
    fn the_override_wins_and_another_prefix_is_found_through_xdg_data_dirs() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_config_path(Some("/dev/community.toml".into()), None),
            PathBuf::from("/dev/community.toml")
        );
        if Path::new(DEFAULT_COMMUNITY_CONFIG).exists() {
            return; // this machine ships one, which wins over any prefix
        }
        let shipped = dir.path().join("share/tilo/community.toml");
        std::fs::create_dir_all(shipped.parent().unwrap()).unwrap();
        std::fs::write(&shipped, "").unwrap();
        let dirs = std::env::join_paths([
            PathBuf::from("relative/share"),
            dir.path().join("empty"),
            dir.path().join("share"),
        ])
        .unwrap();
        assert_eq!(
            resolve_config_path(Some("".into()), Some(dirs.as_os_str())),
            shipped
        );
        assert_eq!(
            resolve_config_path(None, None),
            PathBuf::from(DEFAULT_COMMUNITY_CONFIG)
        );
    }

    #[test]
    fn an_invalid_team_key_or_an_unknown_field_is_an_error_not_a_guess() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("community.toml");
        std::fs::write(&path, "[team]\nuser_public_key = \"abcd\"\n").unwrap();
        assert!(matches!(
            load_from(&path),
            Err(CommunityConfigError::TeamKey { .. })
        ));
        std::fs::write(&path, "[community]\ninvitation = \"x\"\n").unwrap();
        assert!(matches!(
            load_from(&path),
            Err(CommunityConfigError::Parse { .. })
        ));
    }

    fn observed(held: bool, admitted: bool) -> Observed {
        Observed {
            identity: true,
            invite: Some("aa".into()),
            held,
            admitted,
            ..Observed::default()
        }
    }

    #[test]
    fn a_fresh_session_joins_retries_with_back_off_then_opens_the_team_room_once() {
        let start = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        assert_eq!(auto.next(start, &observed(false, false)), Step::Join);
        auto.attempted(start);
        assert_eq!(auto.next(start, &observed(true, false)), Step::Wait);
        let first = start + AUTO_JOIN_FIRST_BACKOFF;
        assert_eq!(auto.next(first, &observed(true, false)), Step::Redeem);
        auto.attempted(first);
        assert_eq!(
            auto.next(first + AUTO_JOIN_FIRST_BACKOFF, &observed(true, false)),
            Step::Wait,
            "the wait doubles while the host is out of reach"
        );
        let mut later = first;
        for _ in 0..20 {
            later += AUTO_JOIN_MAX_BACKOFF;
            assert_eq!(auto.next(later, &observed(true, false)), Step::Redeem);
            auto.attempted(later);
        }
        assert_eq!(
            auto.next(later + AUTO_JOIN_MAX_BACKOFF / 2, &observed(true, false)),
            Step::Wait,
            "and stops growing at the cap"
        );
        assert_eq!(auto.next(later, &observed(true, true)), Step::OpenTeamRoom);
        auto.team_room_opened();
        assert_eq!(auto.next(later, &observed(true, true)), Step::Wait);
    }

    #[test]
    fn nothing_happens_without_an_identity_or_an_invitation() {
        let now = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        let mut no_identity = observed(false, false);
        no_identity.identity = false;
        assert_eq!(auto.next(now, &no_identity), Step::Wait);
        let mut no_invite = observed(false, false);
        no_invite.invite = None;
        assert_eq!(auto.next(now, &no_invite), Step::Wait);
    }

    #[test]
    fn a_person_who_left_is_never_joined_again() {
        let mut auto = AutoJoin::default();
        let mut left = observed(false, false);
        left.membership = settings_store::community::Membership::Left;
        let mut now = std::time::Instant::now();
        for _ in 0..100 {
            assert_eq!(auto.next(now, &left), Step::Wait);
            now += AUTO_JOIN_MAX_BACKOFF;
        }
        left.held = true;
        left.admitted = true;
        assert_eq!(
            auto.next(now, &left),
            Step::Leave,
            "a device that learns the choice leaves the room"
        );
    }

    #[test]
    fn a_refusal_is_final_until_the_invitation_changes() {
        let mut now = std::time::Instant::now();
        let mut auto = AutoJoin::default();
        assert_eq!(auto.next(now, &observed(false, false)), Step::Join);
        auto.attempted(now);
        auto.refused("aa");
        assert_eq!(auto.next(now, &observed(true, false)), Step::RecordRefusal);
        let mut refused = observed(true, false);
        refused.refused_invite = Some("aa".into());
        assert_eq!(auto.next(now, &refused), Step::Leave);
        refused.held = false;
        for _ in 0..100 {
            now += AUTO_JOIN_MAX_BACKOFF;
            assert_eq!(auto.next(now, &refused), Step::Wait, "never asked again");
        }
        refused.invite = Some("bb".into());
        assert_eq!(
            auto.next(now, &refused),
            Step::Join,
            "a different invitation is asked once more"
        );
    }
}

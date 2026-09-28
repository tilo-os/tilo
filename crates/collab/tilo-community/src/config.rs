//! Where a host finds the Tilo community and the Tilo team.
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
use tilo_desktop_model::RoomId;

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
pub fn team_direct_room(me: &str, team: &TeamPrincipal) -> Option<RoomId> {
    let room = tilo_reference::DirectRoom::between(me, &team.person_id())?;
    RoomId::parse(&room.room_id())
}

#[cfg(test)]
mod tests {

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
}

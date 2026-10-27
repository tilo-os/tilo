//! Bounded, auditable knowledge about recognizable folders and game saves.
//!
//! Everything here is public metadata: a fixed vocabulary of known folder
//! names, the Steam app ids whose save locations are documented, and the
//! documented locations themselves. Nothing in this module is derived from a
//! person's directory names, and nothing here authorizes access to a path.
//! A game that is not listed is reported as having an unknown save location
//! rather than as having no saves.

use serde::Serialize;

/// Recognizable top-level folders. A path whose last component matches one of
/// these names is labeled with the known name; every other path is `Other` and
/// its own name is never published.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownFolder {
    Documents,
    Desktop,
    Downloads,
    Pictures,
    Music,
    Videos,
    SavedGames,
    AppData,
    OutlookFiles,
    Steam,
    OneDrive,
    Nextcloud,
    Dropbox,
    GoogleDrive,
    OwnCloud,
}

impl KnownFolder {
    /// Case-insensitive, locale-independent recognition of a single folder name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "documents" => Self::Documents,
            "desktop" => Self::Desktop,
            "downloads" => Self::Downloads,
            "pictures" | "my pictures" => Self::Pictures,
            "music" | "my music" => Self::Music,
            "videos" | "my video" | "my videos" => Self::Videos,
            "saved games" => Self::SavedGames,
            "appdata" => Self::AppData,
            "outlook files" => Self::OutlookFiles,
            "steam" | "steamlibrary" => Self::Steam,
            "onedrive" => Self::OneDrive,
            "nextcloud" => Self::Nextcloud,
            "dropbox" => Self::Dropbox,
            "google drive" | "googledrive" => Self::GoogleDrive,
            "owncloud" => Self::OwnCloud,
            _ => return None,
        })
    }

    pub fn is_cloud(self) -> bool {
        matches!(
            self,
            Self::OneDrive | Self::Nextcloud | Self::Dropbox | Self::GoogleDrive | Self::OwnCloud
        )
    }
}

/// A cloud provider whose folder is on this computer. Whether a destination
/// connection exists is a separate fact: it is never inferred from the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudProvider {
    OneDrive,
    Nextcloud,
    Dropbox,
    GoogleDrive,
    OwnCloud,
}

impl CloudProvider {
    pub fn for_folder(folder: KnownFolder) -> Option<Self> {
        Some(match folder {
            KnownFolder::OneDrive => Self::OneDrive,
            KnownFolder::Nextcloud => Self::Nextcloud,
            KnownFolder::Dropbox => Self::Dropbox,
            KnownFolder::GoogleDrive => Self::GoogleDrive,
            KnownFolder::OwnCloud => Self::OwnCloud,
            _ => return None,
        })
    }
}

/// Where a documented save location starts. Windows locations are relative to
/// the account profile; Proton locations use the same relative path inside the
/// prefix's `drive_c/users/steamuser`; Linux locations are relative to the home.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SaveRoot {
    Documents,
    SavedGames,
    AppDataRoaming,
    AppDataLocal,
    AppDataLocalLow,
    SteamUserdata,
    HomeLocalShare,
    HomeConfig,
    HomeData,
}

/// One documented save location for one game.
#[derive(Clone, Copy, Debug)]
pub struct SaveLocation {
    pub root: SaveRoot,
    /// Relative to the save root, with `/` separators.
    pub relative: &'static str,
}

const fn windows(root: SaveRoot, relative: &'static str) -> SaveLocation {
    SaveLocation { root, relative }
}

/// A documented game. `saves` covers locations the project has verified for
/// that game; an empty list means the game is known but its save location is
/// not, which is reported as unknown.
#[derive(Clone, Copy, Debug)]
pub struct SteamApp {
    pub app_id: u32,
    pub name: &'static str,
    pub windows: &'static [SaveLocation],
    pub linux: &'static [SaveLocation],
}

const STARDEW_WINDOWS: &[SaveLocation] =
    &[windows(SaveRoot::AppDataRoaming, "StardewValley/Saves")];
const STARDEW_LINUX: &[SaveLocation] = &[windows(SaveRoot::HomeConfig, "StardewValley/Saves")];
const TERRARIA_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::Documents, "My Games/Terraria")];
const TERRARIA_LINUX: &[SaveLocation] = &[windows(SaveRoot::HomeLocalShare, "Terraria")];
const VALHEIM_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::AppDataLocalLow, "IronGate/Valheim")];
const VALHEIM_LINUX: &[SaveLocation] = &[windows(SaveRoot::HomeConfig, "unity3d/IronGate/Valheim")];
const ELDEN_RING_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::AppDataRoaming, "EldenRing")];
const SKYRIM_SE_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::Documents,
    "My Games/Skyrim Special Edition/Saves",
)];
const FALLOUT4_WINDOWS: &[SaveLocation] =
    &[windows(SaveRoot::Documents, "My Games/Fallout4/Saves")];
const WITCHER3_WINDOWS: &[SaveLocation] =
    &[windows(SaveRoot::Documents, "The Witcher 3/gamesaves")];
const WITCHER3_LINUX: &[SaveLocation] = &[windows(
    SaveRoot::HomeLocalShare,
    "CD Projekt Red/The Witcher 3/gamesaves",
)];
const CYBERPUNK_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::SavedGames,
    "CD Projekt Red/Cyberpunk 2077",
)];
const CYBERPUNK_LINUX: &[SaveLocation] = &[windows(
    SaveRoot::HomeLocalShare,
    "CD Projekt Red/Cyberpunk 2077",
)];
const BG3_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::AppDataLocal,
    "Larian Studios/Baldur's Gate 3",
)];
const BG3_LINUX: &[SaveLocation] = &[windows(
    SaveRoot::HomeLocalShare,
    "Larian Studios/Baldur's Gate 3",
)];
const GTA5_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::Documents,
    "Rockstar Games/GTA V/Profiles",
)];
const RDR2_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::Documents,
    "Rockstar Games/Red Dead Redemption 2/Profiles",
)];
const HOLLOW_KNIGHT_WINDOWS: &[SaveLocation] = &[windows(
    SaveRoot::AppDataLocalLow,
    "Team Cherry/Hollow Knight",
)];
const HOLLOW_KNIGHT_LINUX: &[SaveLocation] = &[windows(
    SaveRoot::HomeConfig,
    "unity3d/Team Cherry/Hollow Knight",
)];
const HADES_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::Documents, "Saved Games/Hades")];
const DST_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::Documents, "Klei/DoNotStarveTogether")];
const DONT_STARVE_WINDOWS: &[SaveLocation] = &[windows(SaveRoot::Documents, "Klei/DoNotStarve")];
const USERDATA_REMOTE: &[SaveLocation] = &[windows(SaveRoot::SteamUserdata, "remote")];
const USERDATA_LOCAL: &[SaveLocation] = &[windows(SaveRoot::SteamUserdata, "local")];

/// The catalog. Kept deliberately small: a documented, reviewable set of games
/// rather than a heuristic over every installed title. Unknown titles keep an
/// explicit unknown save-location fact.
const APPS: &[SteamApp] = &[
    SteamApp {
        app_id: 413_150,
        name: "Stardew Valley",
        windows: STARDEW_WINDOWS,
        linux: STARDEW_LINUX,
    },
    SteamApp {
        app_id: 105_600,
        name: "Terraria",
        windows: TERRARIA_WINDOWS,
        linux: TERRARIA_LINUX,
    },
    SteamApp {
        app_id: 892_970,
        name: "Valheim",
        windows: VALHEIM_WINDOWS,
        linux: VALHEIM_LINUX,
    },
    SteamApp {
        app_id: 1_245_620,
        name: "Elden Ring",
        windows: ELDEN_RING_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 489_830,
        name: "The Elder Scrolls V: Skyrim Special Edition",
        windows: SKYRIM_SE_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 377_160,
        name: "Fallout 4",
        windows: FALLOUT4_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 292_030,
        name: "The Witcher 3: Wild Hunt",
        windows: WITCHER3_WINDOWS,
        linux: WITCHER3_LINUX,
    },
    SteamApp {
        app_id: 1_091_500,
        name: "Cyberpunk 2077",
        windows: CYBERPUNK_WINDOWS,
        linux: CYBERPUNK_LINUX,
    },
    SteamApp {
        app_id: 1_086_940,
        name: "Baldur's Gate 3",
        windows: BG3_WINDOWS,
        linux: BG3_LINUX,
    },
    SteamApp {
        app_id: 271_590,
        name: "Grand Theft Auto V",
        windows: GTA5_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 1_174_180,
        name: "Red Dead Redemption 2",
        windows: RDR2_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 367_520,
        name: "Hollow Knight",
        windows: HOLLOW_KNIGHT_WINDOWS,
        linux: HOLLOW_KNIGHT_LINUX,
    },
    SteamApp {
        app_id: 1_145_360,
        name: "Hades",
        windows: HADES_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 322_330,
        name: "Don't Starve Together",
        windows: DST_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 219_740,
        name: "Don't Starve",
        windows: DONT_STARVE_WINDOWS,
        linux: &[],
    },
    SteamApp {
        app_id: 570,
        name: "Dota 2",
        windows: USERDATA_REMOTE,
        linux: USERDATA_REMOTE,
    },
    SteamApp {
        app_id: 730,
        name: "Counter-Strike 2",
        windows: USERDATA_LOCAL,
        linux: USERDATA_LOCAL,
    },
    SteamApp {
        app_id: 440,
        name: "Team Fortress 2",
        windows: USERDATA_REMOTE,
        linux: USERDATA_REMOTE,
    },
    SteamApp {
        app_id: 620,
        name: "Portal 2",
        windows: USERDATA_REMOTE,
        linux: USERDATA_REMOTE,
    },
];

/// The documented catalog. Callers may pass their own for tests, but the
/// production set is this bounded, reviewable table.
#[derive(Clone, Copy, Debug, Default)]
pub struct SteamCatalog;

impl SteamCatalog {
    pub const fn builtin() -> Self {
        Self
    }

    pub fn app(&self, app_id: u32) -> Option<&'static SteamApp> {
        APPS.iter().find(|app| app.app_id == app_id)
    }

    pub fn apps(&self) -> &'static [SteamApp] {
        APPS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_folder_names_are_a_closed_vocabulary() {
        assert_eq!(
            KnownFolder::from_name("Documents"),
            Some(KnownFolder::Documents)
        );
        assert_eq!(
            KnownFolder::from_name("ONEDRIVE"),
            Some(KnownFolder::OneDrive)
        );
        assert_eq!(KnownFolder::from_name("alice-private"), None);
        assert!(KnownFolder::OneDrive.is_cloud());
        assert!(!KnownFolder::Documents.is_cloud());
    }

    #[test]
    fn catalog_is_bounded_and_has_no_duplicate_app_ids() {
        assert!(APPS.len() <= 256);
        for (index, app) in APPS.iter().enumerate() {
            assert!(app.app_id > 0 && !app.name.is_empty());
            assert!(!APPS[..index].iter().any(|other| other.app_id == app.app_id));
            for location in app.windows.iter().chain(app.linux.iter()) {
                assert!(!location.relative.is_empty());
                assert!(!location.relative.starts_with('/'));
                assert!(!location.relative.contains(".."));
            }
        }
    }
}

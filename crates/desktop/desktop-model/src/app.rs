//! What kind of app a tile shows, and the id grants are keyed by.

use serde::{Deserialize, Serialize};
use tilo_app_id::apps::{self, app_id_for_builtin_label};

pub use tilo_member_types::AppId;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Other(String),
}

impl AppKind {
    pub fn as_str(&self) -> &str {
        match self {
            AppKind::Terminal => "Terminal",
            AppKind::FileExplorer => "Files",
            AppKind::Browser => "Browser",
            AppKind::Settings => "Settings",
            AppKind::Other(s) => s,
        }
    }

    /// The key into the per-workspace `Workspace::grants` map — distinct
    /// from [`Self::as_str`]'s display label. Foreign clients carry their
    /// real `xdg_toplevel::set_app_id` (`org.gimp.GIMP`, …) straight in.
    /// Built-ins are installed apps too, so they answer to the app id
    /// (`so.tilo.terminal`); a built-in label no app answers to keeps its
    /// `UNPACKAGED_BUILTIN_PREFIX` id.
    pub fn app_id(&self) -> AppId {
        match self {
            AppKind::Terminal => AppId::new(apps::TERMINAL),
            AppKind::FileExplorer => AppId::new(apps::FILE_EXPLORER),
            AppKind::Browser => AppId::new(apps::BROWSER),
            AppKind::Settings => AppId::new(apps::SETTINGS),
            AppKind::Other(s) if s.contains('.') => AppId::new(s.as_str()),
            AppKind::Other(s) => AppId::new(
                app_id_for_builtin_label(s)
                    .map_or_else(|| format!("{UNPACKAGED_BUILTIN_PREFIX}.{s}"), str::to_owned),
            ),
        }
    }
}

/// Reverse-DNS namespace of first-party apps, which the built-in kinds are.
pub const APP_ID_PREFIX: &str = apps::FIRST_PARTY_PREFIX;

/// Namespace of a built-in label that no package answers to (the shell's own
/// `Editor`, fixtures) — the id such a tile has always been granted under.
pub const UNPACKAGED_BUILTIN_PREFIX: &str = "org.tilo";

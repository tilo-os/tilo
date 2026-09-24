//! What kind of app a tile shows, and the id grants are keyed by.

use serde::{Deserialize, Serialize};

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

    /// The key into the per-workspace [`Workspace::grants`] map — distinct
    /// from [`Self::as_str`]'s display label. Foreign clients carry their
    /// real `xdg_toplevel::set_app_id` (`org.gimp.GIMP`, …) straight in;
    /// built-ins get namespaced under the compositor's reverse-DNS prefix
    /// so every grant key lives in one namespace.
    pub fn app_id(&self) -> AppId {
        match self {
            AppKind::Terminal => format!("{APP_ID_PREFIX}.Terminal"),
            AppKind::FileExplorer => format!("{APP_ID_PREFIX}.Files"),
            AppKind::Browser => format!("{APP_ID_PREFIX}.Browser"),
            AppKind::Settings => format!("{APP_ID_PREFIX}.Settings"),
            AppKind::Other(s) if s.contains('.') => s.clone(),
            AppKind::Other(s) => format!("{APP_ID_PREFIX}.{s}"),
        }
    }
}

/// Reverse-DNS prefix the compositor stamps on built-in apps' ids,
/// matching its own portal backend name
/// (`org.freedesktop.impl.portal.desktop.tilo`).
pub const APP_ID_PREFIX: &str = "org.tilo";

/// Identifier for an installed app — opaque to the state layer; the
/// portal layer fills it in with the caller's wayland app-id.
pub type AppId = String;

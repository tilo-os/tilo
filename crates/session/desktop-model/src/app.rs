//! What kind of app a tile shows, and the id grants are keyed by.

use crate::first_party::package_id_for_builtin_label;
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

    /// The key into the per-workspace `Workspace::grants` map — distinct
    /// from [`Self::as_str`]'s display label. Foreign clients carry their
    /// real `xdg_toplevel::set_app_id` (`org.gimp.GIMP`, …) straight in.
    /// Built-ins are the same apps as their packages, so they answer to the
    /// package id (`so.tilo.terminal`); a built-in label no package answers
    /// to keeps its `UNPACKAGED_BUILTIN_PREFIX` id.
    pub fn app_id(&self) -> AppId {
        match self {
            AppKind::Terminal => format!("{APP_ID_PREFIX}.terminal"),
            AppKind::FileExplorer => format!("{APP_ID_PREFIX}.file-explorer"),
            AppKind::Browser => format!("{APP_ID_PREFIX}.browser"),
            AppKind::Settings => format!("{APP_ID_PREFIX}.settings"),
            AppKind::Other(s) if s.contains('.') => s.clone(),
            AppKind::Other(s) => package_id_for_builtin_label(s)
                .map_or_else(|| format!("{UNPACKAGED_BUILTIN_PREFIX}.{s}"), str::to_owned),
        }
    }
}

/// Reverse-DNS namespace of first-party packages, which the built-in kinds
/// are.
pub const APP_ID_PREFIX: &str = crate::first_party::FIRST_PARTY_PREFIX;

/// Namespace of a built-in label that no package answers to (the shell's own
/// `Editor`, fixtures) — the id such a tile has always been granted under.
pub const UNPACKAGED_BUILTIN_PREFIX: &str = "org.tilo";

/// Identifier for an installed app — opaque to the state layer; the
/// portal layer fills it in with the caller's wayland app-id.
pub type AppId = String;

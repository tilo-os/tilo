//! One app of the catalogue, whatever its source.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which Flatpak installation a Flathub entry was read from, and so where
/// installing it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Installation {
    /// The person's own (`~/.local/share/flatpak`).
    User,
    /// The device's (`/var/lib/flatpak`).
    System,
}

/// Where an app comes from, which decides how it is got.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Source {
    /// A Tilo package a configured registry offers. It is not installed:
    /// opening it downloads it when it is absent.
    Tilo {
        /// The registry's name as `registries.toml` gives it.
        registry: String,
    },
    /// A Flatpak application from a Flathub remote, installed on request.
    Flathub {
        /// The remote's name in that installation (`flathub`).
        remote: String,
        installation: Installation,
    },
}

impl Source {
    /// Whether getting the app takes an explicit install (Flathub), rather
    /// than simply opening it (Tilo).
    #[must_use]
    pub fn needs_install(&self) -> bool {
        matches!(self, Self::Flathub { .. })
    }
}

/// A kind of document the app creates ("Workbook" for Sheets). Choosing it
/// opens the app on a new one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceType {
    /// The resource kind the app publishes (`sheets.workbook`).
    pub kind: String,
    /// What a person reads ("Workbook").
    pub label: String,
}

/// An action the app offers while it is not running (its manifest's
/// `[[offers]]`). Choosing it gets the app if needed, opens it and runs the
/// action through the normal action path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueAction {
    /// The resource the app publishes the action on (`calendar.calendar`).
    pub resource: String,
    /// The action's id on that resource (`show-tasks`).
    pub action: String,
    /// What a person reads ("Show tasks").
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One app a person can get.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueEntry {
    /// The Tilo package id (`so.tilo.sheets`) or the Flatpak application id
    /// (`com.spotify.Client`): the identity installed apps are matched on.
    pub id: String,
    pub source: Source,
    /// Its name in the desktop's language.
    pub name: String,
    /// Its name in other languages, and other names it is known by.
    #[serde(default)]
    pub other_names: Vec<String>,
    /// One line saying what it is; empty when the source gives none.
    #[serde(default)]
    pub tagline: String,
    /// Longer description; empty when the source gives none.
    #[serde(default)]
    pub description: String,
    /// Other words people know it by.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// First declared category.
    #[serde(default)]
    pub category: Option<String>,
    /// Its icon as a local file (Flathub's cached `AppStream` icon).
    #[serde(default)]
    pub icon_file: Option<PathBuf>,
    /// Its icon as published artwork (a registry's HTTPS image).
    #[serde(default)]
    pub icon_url: Option<String>,
    /// Documents it creates.
    #[serde(default)]
    pub resource_types: Vec<ResourceType>,
    /// Actions it offers while closed.
    #[serde(default)]
    pub actions: Vec<CatalogueAction>,
    /// Its source has no build of it for this device's processor (Flathub
    /// lists Steam for `x86_64` only): it is still found, says it is not
    /// available for this device, and is never offered for installing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub not_built_for_this_device: bool,
}

impl CatalogueEntry {
    /// The identity matched case-insensitively, as installed apps and block
    /// lists name it.
    #[must_use]
    pub fn key(&self) -> String {
        self.id.to_ascii_lowercase()
    }
}

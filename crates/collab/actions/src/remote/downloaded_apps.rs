//! The programs the person downloaded and added to their apps: the
//! `AppImage`s, tarball programs and loose binaries that ship no desktop
//! entry, and the entry Tilo keeps for each of them.
//!
//! The session keeps one registry of them ([`RESOURCE`]): the apps it has,
//! each as [`app`] of its entry, and the programs running now that have no
//! entry yet — the offers — which the person is asked once about. Its
//! reading is a [`DownloadedApps`]: the apps and the offers, each with the
//! file that starts it, whether that file is still there, and the artwork
//! its entry names ([`DownloadedApp`], [`DownloadedOffer`]).
//!
//! What changes them, over the same names: [`ADD`] and [`DECLINE`] answer an
//! offer (the person's answer to "add this to your apps?"), [`LOCATE`]
//! points an entry at the file the person found after it went missing,
//! [`REMOVE`] takes an entry off the apps and leaves its file, and
//! [`UNINSTALL`] moves every copy of the program to the trash and then takes
//! the entry off.

use serde::{Deserialize, Serialize};

use super::RemoteActionTarget;
use crate::door::ActionAddress;

/// The provider the session answers the downloaded programs under.
pub const PROVIDER: &str = "tilo.downloaded-apps";
/// The resource the registry is published as, and the session's reading of
/// every app and offer ([`DownloadedApps`]).
pub const RESOURCE: &str = "downloaded-apps";
/// Add the program at `file` to the person's apps, as the offer for it
/// names it; `move_to_applications` puts a self-contained `AppImage` in the
/// person's Applications folder first.
pub const ADD: &str = "add";
/// Do not add the program at `file`, and do not ask about that file again.
pub const DECLINE: &str = "decline";
/// Point the entry at `path`, the file the person found where it is now.
pub const LOCATE: &str = "locate";
/// Take the entry off the person's apps. The program's file stays.
pub const REMOVE: &str = "remove";
/// Move every copy of the program to the trash, then take the entry off.
pub const UNINSTALL: &str = "uninstall";

/// The parameters of [`ADD`]: the program's file, and whether to move it
/// into the person's Applications folder first.
pub const FILE_PARAM: &str = "file";
pub const MOVE_PARAM: &str = "move_to_applications";
/// The parameter of [`LOCATE`]: where the program's file is now.
pub const PATH_PARAM: &str = "path";

/// What an app's resource id starts with, before its desktop id.
pub const APP_PREFIX: &str = "downloaded-app:";

/// The resource the app with desktop id `desktop_id` reads as: its facts,
/// and the actions that change its entry.
#[must_use]
pub fn app(desktop_id: &str) -> String {
    format!("{APP_PREFIX}{desktop_id}")
}

#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}

/// What kind of download an app is: one self-contained file with its name
/// and icon inside, or a binary alone or in the folder a tarball unpacked
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadedKind {
    /// One self-contained file, its artwork inside it.
    AppImage,
    /// A binary, alone or beside the files it needs.
    Program,
}

/// One of the person's apps that came from a download, as [`RESOURCE`]
/// lists it: the entry Tilo keeps for it and the file that starts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadedApp {
    /// The entry's desktop id, `tilo-downloaded-…`.
    pub desktop_id: String,
    /// What the person calls it.
    pub name: String,
    pub kind: DownloadedKind,
    /// The file the entry starts.
    pub file: String,
    /// Every copy of the app still known, the one the entry starts first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copies: Vec<String>,
    /// Whether that file is still there. When it is not, the entry offers
    /// [`LOCATE`] instead of starting the app.
    pub present: bool,
    /// The artwork the entry names: an absolute file, or an icon name in
    /// the person's themes. The system's generic application icon when the
    /// app brought none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    /// One line about the app, from its own entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// What starts it, as the desktop asks for an app: `host:` and the
    /// entry's desktop id.
    pub launch: String,
}

/// A program running now that has no entry: what adding it would add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadedOffer {
    /// The file that starts it, which [`ADD`] and [`DECLINE`] take.
    pub file: String,
    /// What it calls itself.
    pub name: String,
    pub kind: DownloadedKind,
    /// The artwork its entry would name, when the program brings any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork: Option<String>,
    /// Whether the person already said not to add it. The offer stays, so
    /// asking again stays their choice; nothing asks them by itself.
    #[serde(default, skip_serializing_if = "is_false")]
    pub declined: bool,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// The registry's reading: the apps the person has, and the programs
/// running now that have no entry yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadedApps {
    #[serde(default)]
    pub apps: Vec<DownloadedApp>,
    #[serde(default)]
    pub offers: Vec<DownloadedOffer>,
}

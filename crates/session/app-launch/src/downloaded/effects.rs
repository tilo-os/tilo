//! What a person does with the programs they downloaded: answer the offer
//! to add one to their apps, and point, take off or uninstall an app that is
//! already there.

use tilo_resource::{AbsolutePath, Effect};

/// The registry's own actions: the person's answer about a program that runs
/// with no entry yet.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.downloaded-apps")]
pub enum DownloadedAppsAction {
    /// Add this program to your apps.
    #[effect(
        own_device,
        icon = "plus",
        keywords = "add app, appimage, downloaded program, add to apps, hinzufügen"
    )]
    Add {
        #[param("The program's file")]
        file: AbsolutePath,
        #[param("Move the AppImage into your Applications folder")]
        move_to_applications: Option<bool>,
    },
    /// Do not add this program, and do not ask about this file again.
    #[effect(
        own_device,
        idempotent,
        icon = "x",
        keywords = "do not add, decline, ignore, downloaded program, ablehnen"
    )]
    Decline {
        #[param("The program's file")]
        file: AbsolutePath,
    },
}

/// One downloaded app's actions, on its own resource.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.downloaded-app")]
pub enum DownloadedAppAction {
    /// Point this app at its file, wherever it is now.
    #[effect(
        own_device,
        icon = "search",
        keywords = "locate, find file, missing file, moved, wiederfinden"
    )]
    Locate {
        #[param("Where the program's file is now")]
        path: AbsolutePath,
    },
    /// Take this app off your apps, and leave its file where it is.
    #[effect(
        own_device,
        idempotent,
        icon = "x",
        keywords = "remove from apps, take off, forget, entfernen"
    )]
    Remove,
    /// Move every copy of this program to the trash, and take it off your
    /// apps.
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "uninstall, delete, trash, remove app, deinstallieren"
    )]
    Uninstall,
}

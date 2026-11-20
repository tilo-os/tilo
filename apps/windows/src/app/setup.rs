//! What the manager says of a setup: the programs one installer put in a private capsule,
//! which share the files stored there.

use crate::model::{Catalogue, Setup, WindowsApp};
use crate::strings;

/// The title, body and button of deleting `app`'s setup, when the setup holds more than one
/// program: every program the body names goes with the capsule they share.
pub(super) fn delete_confirmation(
    catalogue: &Catalogue,
    app: &WindowsApp,
) -> Option<(String, String, String)> {
    let setup = catalogue.setup_of(app).filter(Setup::holds_several)?;
    let title = if setup.name.trim().is_empty() {
        strings::t("windows-confirm-delete-setup-unnamed-title")
    } else {
        strings::format(
            "windows-confirm-delete-setup-title",
            "setup",
            setup.name.trim(),
        )
    };
    Some((
        title,
        strings::format(
            "windows-confirm-delete-setup-body",
            "programs",
            &strings::names(&setup.programs),
        ),
        strings::t("windows-confirm-delete-setup-action"),
    ))
}

/// The page's sentence naming the programs a setup shares, when it holds more than one.
pub(super) fn programs_sentence(catalogue: &Catalogue, app: &WindowsApp) -> Option<String> {
    let setup = catalogue.setup_of(app).filter(Setup::holds_several)?;
    Some(strings::format(
        "windows-setup-programs",
        "programs",
        &strings::names(&setup.programs),
    ))
}

/// The button that offers deleting `app`'s data: the setup's own when it holds more than one
/// program, else the app's. `None` while the service offers no delete.
pub(super) fn delete_button(catalogue: &Catalogue, app: &WindowsApp) -> Option<&'static str> {
    let setup = catalogue.setup_of(app)?;
    Some(if setup.holds_several() {
        "windows-delete-setup"
    } else {
        "windows-delete-data"
    })
}

/// The notice key for having deleted `capsule`: the setup's own when more than one program
/// shared it, else the one app's.
pub(super) fn deleted_notice_key(catalogue: &Catalogue, capsule: &str) -> &'static str {
    let programs = catalogue
        .apps
        .iter()
        .filter(|app| app.capsule == capsule)
        .count();
    if programs > 1 {
        "windows-setup-data-deleted"
    } else {
        "windows-data-deleted"
    }
}

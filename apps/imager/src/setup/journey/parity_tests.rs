//! Each route through the guided setup, walked as a person or an agent walks
//! it: the actions the window offers at every step, what it refuses and why,
//! and the words the page shows.
//!
//! These are the routes a fold of the desktop setup's own window into journey
//! pages must keep. They assert the published action surface and the visible
//! text, never a field of the state machine, so the same walk proves the fold
//! lost nothing.

use super::harness::private_state;
use super::*;
use crate::hosting::testing::TestHost;
use crate::resource::ImagerEffect;
use crate::setup::desktop_catalog::{DesktopRelease, PackageFormat};
use crate::setup::desktop_flow::{DesktopFlow, DesktopFlowState, DesktopSelection};
use tilo_actions::{ApiError, Availability};

/// The ids of a window's effects the palette or an agent is offered now.
fn offered(app: &mut TestHost, operation: &str) -> Vec<String> {
    let mut ids: Vec<String> = app
        .published()
        .actions
        .into_iter()
        .filter(|spec| spec.operation_type.as_deref() == Some(operation))
        .filter(|spec| matches!(spec.contract.availability, Availability::Available))
        .map(|spec| spec.id.clone())
        .collect();
    ids.sort();
    ids
}

/// Every one of a window's effects, with the code it is refused by now.
fn surface(app: &mut TestHost, operation: &str) -> Vec<(String, Option<String>)> {
    let mut ids: Vec<(String, Option<String>)> = app
        .published()
        .actions
        .into_iter()
        .filter(|spec| spec.operation_type.as_deref() == Some(operation))
        .map(|spec| {
            let code = match &spec.contract.availability {
                Availability::Available => None,
                Availability::Unavailable { code, .. } => Some(code.clone()),
            };
            (spec.id.clone(), code)
        })
        .collect();
    ids.sort();
    ids
}

/// The journey effects refused right now, and the code each is refused by.
fn refused_journey(app: &mut TestHost) -> Vec<(String, String)> {
    let mut refused: Vec<(String, String)> = surface(app, InstallerPreviewEffect::OPERATION)
        .into_iter()
        .filter_map(|(id, code)| code.map(|code| (id, code)))
        .collect();
    refused.sort();
    refused
}

/// The visible words of the window, outermost first, with a repeated word
/// kept once.
fn labels(app: &mut TestHost) -> Vec<String> {
    fn walk(root: &dyn Widget, out: &mut Vec<String>) {
        if let Some(node) = root.accessibility()
            && let Some(name) = node.name
        {
            out.push(name);
        }
        for child in root.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(app.harness().root_widget_mut(), &mut out);
    out.dedup();
    out
}

#[track_caller]
fn assert_shows(app: &mut TestHost, expected: &[String]) {
    let shown = labels(app);
    for word in expected {
        assert!(
            shown.contains(word),
            "the page must show {word:?}; it shows {shown:?}"
        );
    }
}

#[track_caller]
fn assert_offered(app: &mut TestHost, expected: &[&str]) {
    let shown = offered(app, InstallerPreviewEffect::OPERATION);
    let expected: Vec<String> = expected.iter().map(|id| (*id).to_owned()).collect();
    assert_eq!(shown, expected, "the journey's offered actions");
}

#[track_caller]
fn assert_refused_with(app: &mut TestHost, id: &str, code: &str) {
    let refused = refused_journey(app);
    assert!(
        refused.contains(&(id.to_owned(), code.to_owned())),
        "{id} must be refused with {code}: {refused:?}"
    );
}

/// What the window refused `effect` with: its stable code, and the words it
/// gave the person. A refusal the window itself gave is coded `failed`.
#[track_caller]
fn refusal<E: tilo_resource::Effect>(app: &mut TestHost, effect: &E) -> (String, String) {
    match app.effect(effect) {
        Ok(_) => panic!("`{}` was accepted, not refused", effect.id()),
        Err(ApiError::Unavailable { code, reason }) => (code, reason),
        Err(ApiError::Failed(reason)) => ("failed".into(), reason),
        Err(other) => panic!("`{}` was refused unexpectedly: {other:?}", effect.id()),
    }
}

#[track_caller]
fn assert_refused<E: tilo_resource::Effect>(
    app: &mut TestHost,
    effect: &E,
    code: &str,
    reason: &str,
) {
    let (refused, words) = refusal(app, effect);
    assert_eq!((refused.as_str(), words.as_str()), (code, reason));
}

/// The signed Tilo Desktop package the fixture catalog offers.
fn desktop_release() -> DesktopRelease {
    DesktopRelease {
        id: "fedora-44-x86_64-fixture".into(),
        version: "0.1.9-1.fc44".into(),
        distro_id: "fedora".into(),
        distro_version: "44".into(),
        architecture: "x86_64".into(),
        package_format: PackageFormat::Rpm,
        url: "https://code.tilo.so/tilo/releases/fixture.rpm".into(),
        byte_size: 7,
        sha256: "a".repeat(64),
        verified_runtime: None,
    }
}

/// The desktop setup's page once this computer is known to take it, so the
/// walk is the same on every host that runs the test.
fn ready_desktop_flow() -> DesktopFlow {
    DesktopFlow::from_fixture_state(DesktopFlowState::Ready(DesktopSelection {
        host: "Fedora Linux 44 (x86_64)".into(),
        release: desktop_release(),
        already_installed: false,
    }))
}

// Tilo Desktop packages onto this Linux.
#[test]
fn the_desktop_route_offers_the_desktop_choice_and_hands_the_review_to_its_setup() {
    let (_state, mut environment) = private_state();
    environment.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "0");
    let mut app = TestHost::launched(&["--setup"]);

    // The first page: two install paths for this computer, and the desktop.
    assert_offered(
        &mut app,
        &[
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
            "restart-preview",
        ],
    );
    for (id, code) in [
        ("back-in-preview", "at_start"),
        ("check-storage", "wrong_page"),
        ("set-preview-password", "wrong_page"),
        ("prepare-direct-installation", "installation_unavailable"),
    ] {
        assert_refused_with(&mut app, id, code);
    }
    assert_shows(
        &mut app,
        &[
            t("installer-preview-destination-heading"),
            t("installer-preview-destination-intro"),
            t("installer-preview-desktop-title"),
            t("installer-preview-desktop-body"),
        ],
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::ContinuePreview,
        "failed",
        &t("installer-preview-choose-first"),
    );

    // Choosing the desktop opens the desktop setup's own page for the review,
    // with nothing on this computer's disk decided.
    let status = app
        .effect(&InstallerPreviewEffect::ChooseAndContinuePreview {
            choice: "desktop".into(),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["page"], "review");
    assert_eq!(status["destination"], "desktop");
    assert_eq!(status["handoff"], "deferred");

    // The desktop setup's page is now the one the person answers: it
    // offers what it accepts, the install, its recheck and the way back.
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.native.as_mut().unwrap().desktop_flow = ready_desktop_flow();
        preview.dirty = true;
    })
    .unwrap();
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "install-desktop",
            "read-preview",
            "recheck-desktop",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("imager-section-desktop"),
            t_args(
                "imager-desktop-ready",
                &[
                    ("version", desktop_release().version.into()),
                    ("host", "Fedora Linux 44 (x86_64)".into()),
                ],
            ),
            t("installer-desktop-keep-existing"),
            t("imager-setup-desktop-install"),
            t("installer-preview-back"),
        ],
    );

    // What the desktop page cannot do yet says so in words, without a press.
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::ContinuePreview,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing",
    );
    let status = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    let desktop = &status["setup"]["desktop"];
    assert_eq!(desktop["phase"], "ready");
    assert_eq!(desktop["release_id"], desktop_release().id);
    assert_eq!(desktop["actions"]["install"], true);

    // The review's own effects are not this page's: they are not offered,
    // and a caller who asks anyway is told so.
    for effect in [
        InstallerPreviewEffect::PrepareDesktopInstallation,
        InstallerPreviewEffect::PrepareUsb,
        InstallerPreviewEffect::RestartPreview,
    ] {
        assert_refused(
            &mut app,
            &effect,
            "wrong_page",
            &t("installer-preview-wrong-page"),
        );
    }

    // Back returns to the choice, and the desktop page is gone with it.
    let back = app
        .effect(&InstallerPreviewEffect::BackInPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(back["page"], "destination");
    assert_eq!(back["destination"], "desktop");
    assert!(back["setup"].is_null());
    assert_offered(
        &mut app,
        &[
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
            "restart-preview",
        ],
    );
    assert_shows(&mut app, &[t("installer-preview-destination-heading")]);
    assert_refused_with(&mut app, "back-in-preview", "at_start");
}

// Full Tilo next to the current system, through the internal staging setup.
#[test]
fn the_internal_staging_route_walks_the_review_and_keeps_the_setup_when_no_release_is_offered() {
    let (_state, mut environment) = private_state();
    environment.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "1");
    let mut app = TestHost::launched(&["--setup"]);
    // This computer's disk as the read-only inspection reports it: a
    // partitioned disk the planner can place Tilo beside.
    app.with_widget::<InstallerPreview, _>(|preview| preview.storage.stage())
        .unwrap();
    assert_shows(&mut app, &[t("installer-preview-computer-body")]);

    let status = app
        .effect(&InstallerPreviewEffect::ChooseAndContinuePreview {
            choice: "computer".into(),
        })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["page"], "migrate");
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "open-migrations",
            "read-preview",
            "restart-preview",
            "set-migrate-ticket",
            "sign-in-for-migration",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-migrate-heading"),
            t("installer-preview-migrate-intro"),
            t("installer-preview-ticket-label"),
            t("installer-migrate-sign-in"),
            t("installer-migrate-open"),
            t("installer-preview-continue"),
        ],
    );

    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
            "restart-preview",
            "set-preview-device-name",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-devices-heading"),
            t("installer-preview-devices-intro"),
            t("installer-preview-device-name-label"),
        ],
    );

    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "check-storage",
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
            "restart-preview",
            "set-storage-size",
            "toggle-storage-details",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-storage-heading"),
            t("installer-preview-storage-intro"),
            t("installer-preview-storage-size-label"),
            t("installer-preview-storage-show-details"),
        ],
    );
    // The space is chosen within the inspected disk's bounds, and the page
    // refuses a size the disk cannot give.
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::SetStorageSize { gib: 20 },
        "failed",
        &t("installer-preview-storage-range-error"),
    );
    let stored = app
        .effect(&InstallerPreviewEffect::SetStorageSize { gib: 100 })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(stored["storage"]["size_gib"], 100);

    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "edit-preview",
            "prepare-direct-installation",
            "prepare-usb",
            "read-preview",
            "restart-preview",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-review-heading"),
            t("installer-preview-review-intro"),
            t("installer-review-setup"),
            t("installer-preview-deferred-handoff"),
            t("installer-preview-prepare-direct"),
        ],
    );
    // Storage is not a page of the review: inspecting it again is refused.
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::CheckStorage,
        "wrong_page",
        &t("installer-preview-wrong-page"),
    );

    // The password section of the review opens and closes as a page of its
    // own, without deciding anything.
    app.effect(&InstallerPreviewEffect::EditPreview {
        section: "password".into(),
    })
    .unwrap();
    let password = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(password["page"], "password");
    assert_eq!(password["editing"], "password");
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    assert_eq!(
        app.effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap()["page"],
        "review"
    );

    // A test build offers no released Tilo to stage, so the preparation is
    // refused in words and the reviewed setup is kept for another try.
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::PrepareDirectInstallation,
        "failed",
        &t("installer-preview-direct-review-unavailable"),
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-review-heading"),
            t("installer-preview-direct-review-unavailable"),
        ],
    );
    let kept = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(kept["page"], "review");
    assert_eq!(kept["storage"]["size_gib"], 100);

    // Back returns to the space page, which the review came from.
    app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
    assert_eq!(
        app.effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap()["page"],
        "storage"
    );
}

// Writing to a drive: this computer's installer, when direct staging is left
// out of the build.
#[test]
fn the_drive_route_makes_a_usb_installer_and_hands_the_page_to_the_writer() {
    let (_state, mut environment) = private_state();
    environment.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "0");
    let mut app = TestHost::launched(&["--setup"]);
    assert_shows(&mut app, &[t("installer-preview-computer-usb-body")]);

    app.effect(&InstallerPreviewEffect::ChooseAndContinuePreview {
        choice: "computer".into(),
    })
    .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();

    // There is no space page to decide: the review says the installer goes
    // to a drive, and offers to write it.
    let review = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(review["page"], "review");
    assert_eq!(review["destination"], "computer");
    assert_shows(
        &mut app,
        &[
            t("installer-preview-review-heading"),
            t("installer-review-usb-fallback"),
            t("installer-preview-prepare-usb"),
        ],
    );
    assert!(
        !offered(&mut app, InstallerPreviewEffect::OPERATION)
            .iter()
            .any(|id| id == "prepare-direct-installation"),
        "this build does not offer the direct installation"
    );
    assert_refused_with(
        &mut app,
        "prepare-direct-installation",
        "installation_unavailable",
    );

    let written = app
        .effect(&InstallerPreviewEffect::PrepareUsb)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(written["page"], "review");

    // The media writer takes the page over, with the drive choice its own.
    assert!(offered(&mut app, InstallerPreviewEffect::OPERATION).is_empty());
    let writer = offered(&mut app, ImagerEffect::OPERATION);
    for id in ["choose-image", "rescan"] {
        assert!(
            writer.iter().any(|offered| offered == id),
            "the writer offers {id}: {writer:?}"
        );
    }
    assert_shows(
        &mut app,
        &[
            "Source".to_owned(),
            "Choose an image".to_owned(),
            "Tilo releases".to_owned(),
            "No Tilo releases are stored on this computer.".to_owned(),
            "Continue".to_owned(),
        ],
    );
}

// The Windows host path: the native Full Tilo setup, without the guided pages.
#[test]
fn the_windows_route_offers_native_full_tilo_and_no_guided_pages() {
    let (_state, mut environment) = private_state();
    environment.set(crate::setup::internal_install_flow::RUNTIME_ALLOW_VAR, "0");
    let mut app = TestHost::launched(&["--setup"]);
    // A Windows host, which this build of the test is not.
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.full_os_host = true;
        preview.dirty = true;
    })
    .unwrap();

    // Nothing is chosen yet: there is no way back, and a USB installer is
    // written only for another computer.
    assert_offered(
        &mut app,
        &[
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
        ],
    );
    assert_refused_with(&mut app, "back-in-preview", "at_start");
    assert_shows(
        &mut app,
        &[
            t("installer-preview-destination-heading"),
            t("installer-preview-computer-title"),
            t("installer-preview-another-title"),
            t("imager-setup-internal-not-enabled"),
            t("installer-preview-continue"),
        ],
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::ContinuePreview,
        "failed",
        &t("installer-preview-choose-first"),
    );

    // A card chooses its path and continues in one press, and Continue hands
    // the review to the native Full Tilo setup.
    app.effect(&InstallerPreviewEffect::ChooseAndContinuePreview {
        choice: "computer".into(),
    })
    .unwrap();
    assert_eq!(
        offered(&mut app, InstallerPreviewEffect::OPERATION),
        ["read-preview"]
    );
    assert_shows(
        &mut app,
        &[
            t("imager-setup-internal-download-title"),
            t("imager-setup-error-download-not-offered"),
            t("imager-setup-internal-blockers-title"),
            t("imager-setup-internal-install"),
            t("installer-preview-back"),
        ],
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::ContinuePreview,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing",
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::InstallInternal {
            reviewed: String::new(),
        },
        "internal_unavailable",
        &t("imager-setup-internal-error-command-unavailable"),
    );
    let status = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    let status = &status["setup"];
    assert_eq!(status["experience"], "full_os");
    assert_eq!(status["next"], false);
    assert_eq!(status["internal"]["on_review"], true);

    // Back waits for the check the native page runs, then returns to the
    // choice with nothing installed.
    tilo_test_env::wait_for_within(
        "the native setup's check to finish",
        std::time::Duration::from_secs(30),
        || {
            app.settle();
            offered(&mut app, InstallerPreviewEffect::OPERATION)
                .iter()
                .any(|id| id == "back-in-preview")
                .then_some(())
        },
    );
    let back = app
        .effect(&InstallerPreviewEffect::BackInPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(back["page"], "destination");
    assert_eq!(back["destination"], "computer");
    assert!(back["setup"].is_null());
    assert_shows(
        &mut app,
        &[
            t("installer-preview-destination-heading"),
            t("installer-preview-computer-title"),
        ],
    );
}

// The preview and its screenshot paths: every page walked without a disk.
#[test]
fn the_preview_route_walks_every_page_and_writes_nothing() {
    let mut app = TestHost::launched(&["--demo"]);
    assert_offered(
        &mut app,
        &[
            "choose-and-continue-preview",
            "choose-preview",
            "continue-preview",
            "read-preview",
            "restart-preview",
        ],
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::CheckStorage,
        "demo",
        &t("installer-demo-unavailable"),
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-destination-heading"),
            t("installer-demo-notice-title"),
        ],
    );

    app.effect(&InstallerPreviewEffect::ChooseAndContinuePreview {
        choice: "computer".into(),
    })
    .unwrap();
    assert_shows(&mut app, &[t("installer-preview-migrate-heading")]);
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_shows(&mut app, &[t("installer-preview-devices-heading")]);
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    // The staged disk is the preview's example, and its space can be chosen
    // without inspecting this machine.
    assert_shows(&mut app, &[t("installer-preview-storage-heading")]);
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::CheckStorage,
        "demo",
        &t("installer-demo-unavailable"),
    );
    let stored = app
        .effect(&InstallerPreviewEffect::SetStorageSize { gib: 100 })
        .unwrap()
        .value
        .unwrap();
    assert_eq!(stored["storage"]["size_gib"], 100);

    // Back retraces the pages the person came through, one at a time, to the
    // first; walking forward again reaches the same page.
    for heading in [
        "installer-preview-devices-heading",
        "installer-preview-migrate-heading",
        "installer-preview-destination-heading",
    ] {
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_shows(&mut app, &[t(heading)]);
    }
    for effect in [
        InstallerPreviewEffect::ChooseAndContinuePreview {
            choice: "computer".into(),
        },
        InstallerPreviewEffect::ContinuePreview,
        InstallerPreviewEffect::SetPreviewDeviceName {
            value: "Ada's computer".into(),
        },
        InstallerPreviewEffect::ContinuePreview,
    ] {
        app.effect(&effect).unwrap();
    }
    assert_shows(&mut app, &[t("installer-preview-storage-heading")]);

    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_shows(
        &mut app,
        &[
            t("installer-preview-review-heading"),
            t("installer-preview-finish"),
        ],
    );
    app.effect(&InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_offered(
        &mut app,
        &[
            "back-in-preview",
            "choose-and-continue-preview",
            "choose-preview",
            "read-preview",
            "restart-preview",
        ],
    );
    assert_shows(
        &mut app,
        &[
            t("installer-preview-ready-heading"),
            t("installer-preview-ready-intro"),
            t("installer-preview-restart"),
        ],
    );
    assert_refused(
        &mut app,
        &InstallerPreviewEffect::ContinuePreview,
        "finished",
        &t("installer-preview-finished"),
    );

    app.effect(&InstallerPreviewEffect::RestartPreview).unwrap();
    assert_shows(
        &mut app,
        &[
            t("installer-preview-destination-heading"),
            t("installer-demo-notice-title"),
        ],
    );
    assert_refused_with(&mut app, "back-in-preview", "at_start");

    // The plan scenario shows the confirmation the plan page carries: the
    // single-use code, its lifetime, and what carries it.
    let mut shot = TestHost::capture_at(Some("installer-plan-ready"), (1180, 820));
    assert_shows(
        &mut shot,
        &[t("installer-plan-heading"), t("installer-plan-carry-usb")],
    );
    let carried = labels(&mut shot);
    assert!(
        carried
            .iter()
            .any(|label| label.starts_with("Carry this single-use code to the computer:")),
        "{carried:?}"
    );
}

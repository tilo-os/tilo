//! A failure in the window bar carries Try again, and Try again asks for
//! the very work that failed, pressed by pointer or through its semantic
//! action, in a wide window or a compact one.

use super::stand_in;
use crate::resource::{PrintCentreEffect, PrintCentreShell};
use crate::surface::{Page, PrintCentre};
use ui::prelude::TestApp;

type Window = TestApp<PrintCentreShell>;

/// Every way to press a notice's action: wide and compact, by pointer and
/// through the semantic action.
const WAYS: [(f64, bool); 4] = [
    (1280.0, false),
    (1280.0, true),
    (420.0, false),
    (420.0, true),
];

/// A live Print Centre on the studio printer, its notices in the window
/// bar, its devices stand-ins.
fn opened() -> (Window, stand_in::Shared) {
    let devices = stand_in::install();
    let mut app = crate::surface::tests::live_with_studio();
    app.harness().present_in_window_bar();
    app.settle();
    (app, devices)
}

fn centre(app: &mut Window) -> &mut PrintCentre {
    app.surface_mut::<PrintCentre>()
}

fn until(app: &mut Window, what: &str, done: impl Fn(&PrintCentre) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let centre = centre(app);
        if done(centre) {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "gave up waiting for {what}: the bar says {:?} (retry {:?}), on {:?}",
            centre
                .notice
                .get()
                .map(|notice| (&notice.text, notice.success)),
            centre.notice.get().map(|notice| notice.retry.is_some()),
            centre.page,
        );
    }
}

fn set_failing(devices: &stand_in::Shared, failing: bool) {
    devices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .failing = failing;
}

fn asked(devices: &stand_in::Shared) -> Vec<String> {
    devices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .asked
        .clone()
}

/// The labels of the notice's buttons in a bar `width` wide.
fn buttons(app: &mut Window, width: f64) -> Vec<String> {
    app.harness()
        .window_bar_notice(width)
        .expect("a notice is up")
        .buttons
        .into_iter()
        .map(|button| button.name)
        .collect()
}

/// Wait for the failure, check it carries Try again and nothing else, let
/// the stand-in `devices` (if any) work again, and press Try again the
/// `semantic` way in a bar `width` wide.
fn try_again(app: &mut Window, devices: Option<&stand_in::Shared>, width: f64, semantic: bool) {
    until(app, "the failure to be said in the bar", |centre| {
        centre
            .notice
            .get()
            .is_some_and(|notice| !notice.success && notice.retry.is_some())
    });
    assert_eq!(buttons(app, width), ["Try again"], "at {width} px");
    if let Some(devices) = devices {
        set_failing(devices, false);
    }
    let harness = app.harness();
    let pressed = if semantic {
        harness.invoke_window_notice_action(width, 0)
    } else {
        harness.click_window_notice_action(width, 0)
    };
    pressed.expect("Try again stands in the bar");
    app.settle();
}

/// Wait until the work asked again, the same as the first time, went
/// through, and check the success only informs.
fn asked_again(app: &mut Window, devices: &stand_in::Shared, before: usize, width: f64) {
    until(app, "Try again to ask for the work again", |centre| {
        centre.notice.get().is_some_and(|notice| notice.success)
    });
    let asked = asked(devices);
    let [first, again] = &asked[before..] else {
        panic!("asked once, then once again: {asked:?}");
    };
    assert_eq!(first, again, "Try again asks for the very work that failed");
    assert!(buttons(app, width).is_empty(), "a success only informs");
}

/// Adding, preferring, pausing and resuming a printer, cancelling and
/// retrying a document: Try again runs the same effect on the same printer
/// or document.
#[test]
fn a_failed_printer_or_document_operations_try_again_runs_it_again() {
    let (mut app, devices) = opened();
    centre(&mut app).discovered = Some(Ok(super::super::demo_discovered_printers()));
    let printer = || "Studio_printer".to_owned();
    let effects = [
        PrintCentreEffect::PausePrinter { printer: printer() },
        PrintCentreEffect::ResumePrinter { printer: printer() },
        PrintCentreEffect::SetDefaultPrinter { printer: printer() },
        PrintCentreEffect::CancelDocument {
            job: "Studio_printer-42".into(),
        },
        PrintCentreEffect::RetryDocument {
            job: "Studio_printer-41".into(),
        },
        PrintCentreEffect::AddPrinter {
            printer: "ipps://office.local/ipp/print".into(),
        },
    ];
    let ways = WAYS.iter().cycle();
    for (effect, &(width, semantic)) in effects.iter().chain(&effects[..2]).zip(ways) {
        let before = asked(&devices).len();
        set_failing(&devices, true);
        let _ = app.effect(effect);
        try_again(&mut app, Some(&devices), width, semantic);
        asked_again(&mut app, &devices, before, width);
    }
}

/// Printing that failed: Try again prints the same document on the same
/// printer with the same settings.
#[test]
fn a_failed_prints_try_again_prints_it_again() {
    let (mut app, devices) = opened();
    for (width, semantic) in WAYS {
        centre(&mut app).stage_scenario(Some("print"));
        centre(&mut app).print_copies.set(3.0);
        app.settle();
        let before = asked(&devices).len();
        set_failing(&devices, true);
        let _ = app.effect(&PrintCentreEffect::SendToPrinter);
        try_again(&mut app, Some(&devices), width, semantic);
        asked_again(&mut app, &devices, before, width);
        let asked = asked(&devices);
        assert!(
            asked[before].contains("Studio_printer")
                && asked[before].contains("Travel itinerary.pdf")
                && asked[before].contains("copies: 3"),
            "{asked:?}"
        );
    }
}

/// A page that did not scan: Try again scans with the same scanner.
#[test]
fn a_failed_scans_try_again_scans_again() {
    let (mut app, devices) = opened();
    centre(&mut app).stage_scenario(Some("scan"));
    app.settle();
    for (width, semantic) in WAYS {
        let pages = centre(&mut app).scan.pages.len();
        let before = asked(&devices).len();
        set_failing(&devices, true);
        let _ = app.effect(&PrintCentreEffect::ScanPage);
        try_again(&mut app, Some(&devices), width, semantic);
        // A scanned page shows itself; nothing is said.
        until(&mut app, "Try again to scan the page", |centre| {
            centre.scan.pages.len() == pages.saturating_add(1)
        });
        assert_eq!(
            asked(&devices)[before..],
            ["scan scanner0", "scan scanner0"]
        );
        let _ = app.effect(&PrintCentreEffect::ScanAnother);
        app.settle();
    }
}

/// Text that was not recognized: Try again recognizes the same page.
#[test]
fn a_failed_recognitions_try_again_recognizes_the_page_again() {
    let (mut app, devices) = opened();
    centre(&mut app).stage_scenario(Some("scan-result"));
    app.settle();
    for (width, semantic) in WAYS {
        let before = asked(&devices).len();
        set_failing(&devices, true);
        let _ = app.effect(&PrintCentreEffect::RecognizeText);
        try_again(&mut app, Some(&devices), width, semantic);
        until(
            &mut app,
            "the page's text to be recognized again",
            |centre| {
                centre
                    .scan
                    .result
                    .as_ref()
                    .is_some_and(|page| page.recognized_text.as_deref() == Some("Stand-in text"))
            },
        );
        assert_eq!(asked(&devices)[before..], ["recognize", "recognize"]);
        centre(&mut app).scan.pages[0].recognized_text = None;
    }
}

/// A scan that could not be saved: Try again asks where to save the same
/// page again, and saves it there.
#[test]
fn a_failed_saves_try_again_saves_the_scan() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, _devices) = opened();
    {
        let centre = centre(&mut app);
        centre.open_scan();
        centre.scan_finished(Ok(crate::surface::ScannedPage {
            png: std::sync::Arc::from(b"scanned page".to_vec()),
            ..crate::surface::demo_scanned_page()
        }));
    }
    app.settle();
    let folder = tempfile::tempdir().expect("a folder to save in");
    for (way, (width, semantic)) in WAYS.into_iter().enumerate() {
        let choosers = recorder.pick_requests().len();
        let target = folder.path().join(format!("page-{way}.png"));
        recorder.respond_to_pick(ui::portal::PortalOutcome::Failed("no chooser".into()));
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(target.clone()));
        let _ = app.effect(&PrintCentreEffect::SaveScannedPage);
        try_again(&mut app, None, width, semantic);
        until(&mut app, "the scan to be saved where chosen", |centre| {
            centre.notice.get().is_some_and(|notice| notice.success)
        });
        assert!(target.exists(), "saved at {}", target.display());
        assert_eq!(
            recorder.pick_requests().len(),
            choosers.saturating_add(2),
            "Try again asked the chooser again"
        );
        assert!(
            buttons(&mut app, width).is_empty(),
            "a success only informs"
        );
    }
}

/// A file chooser that did not open to pick a document: Try again asks
/// for one again.
#[test]
fn a_failed_document_chooser_try_again_asks_again() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, _devices) = opened();
    let folder = tempfile::tempdir().expect("a folder for the document");
    let document = folder.path().join("Itinerary.txt");
    std::fs::write(&document, "Day one").expect("the document is written");
    for (width, semantic) in WAYS {
        let choosers = recorder.pick_requests().len();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Failed("no chooser".into()));
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(document.clone()));
        let _ = app.effect(&PrintCentreEffect::PrintDocument);
        try_again(&mut app, None, width, semantic);
        until(
            &mut app,
            "the chosen document to be set up to print",
            |centre| centre.page == Page::Print,
        );
        assert_eq!(
            recorder.pick_requests().len(),
            choosers.saturating_add(2),
            "Try again asked the chooser again"
        );
        let _ = app.effect(&PrintCentreEffect::ClosePrintSetup);
        app.settle();
    }
}

/// A success only informs, and the pages draw no result card of their own.
#[test]
fn a_success_only_informs_and_no_page_draws_it() {
    let (mut app, devices) = opened();
    let _ = app.effect(&PrintCentreEffect::PausePrinter {
        printer: "Studio_printer".into(),
    });
    until(&mut app, "the printer to pause", |centre| {
        centre.notice.get().is_some_and(|notice| notice.success)
    });
    assert_eq!(asked(&devices).len(), 1);
    for width in [1280.0, 420.0] {
        let notice = app
            .harness()
            .window_bar_notice(width)
            .expect("the result is said");
        assert_eq!(
            notice.text,
            "Printer paused. Documents will wait until you resume it."
        );
        assert!(notice.buttons.is_empty(), "a success only informs");
    }
    let frame = app.harness().inspection_json().expect("the frame");
    assert!(
        !frame.contains("Printer paused"),
        "no page draws the result: {frame}"
    );
}

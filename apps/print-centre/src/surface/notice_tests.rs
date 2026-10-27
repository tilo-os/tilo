//! A failure waits for a person, carrying Try again, and Try again asks for
//! the very work that failed; a success is recorded and informs nobody.
//!
//! The rows are [`ui::attention`]'s: the same stack every app shows, so what
//! is checked here is that print centre's outcomes reach it with the verb
//! that asks again, and that the same work is asked for when it is pressed.

use super::stand_in;
use crate::resource::{PrintCentreEffect, PrintCentreShell};
use crate::surface::{Page, PrintCentre};
use ui::prelude::TestApp;

type Window = TestApp<PrintCentreShell>;

thread_local! {
    /// The rows that were already waiting when this test opened its window:
    /// another test in this process may have raised one with the same verb,
    /// and it is not this test's to answer.
    static FOREIGN: std::cell::RefCell<Vec<ui::attention::ItemId>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A live Print Centre on the studio printer, with stand-in devices.
fn opened() -> (Window, stand_in::Shared) {
    let devices = stand_in::install();
    ui::attention::clear();
    ui::attention::clear_ledger();
    let app = crate::surface::tests::live_with_studio();
    FOREIGN.with(|foreign| {
        *foreign.borrow_mut() = ui::attention::items()
            .iter()
            .map(|item| item.id())
            .collect();
    });
    (app, devices)
}

fn centre(app: &mut Window) -> &mut PrintCentre {
    app.surface_mut::<PrintCentre>()
}

/// The row this test's work raised, waiting with Try again.
fn waiting() -> Option<ui::attention::Item> {
    let foreign = FOREIGN.with(|foreign| foreign.borrow().clone());
    ui::attention::items()
        .into_iter()
        .find(|item| item.action().verb == super::TRY_AGAIN_ACTION && !foreign.contains(&item.id()))
}

fn until_waiting(app: &mut Window, what: &str) -> ui::attention::Item {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        if let Some(item) = waiting() {
            return item;
        }
        assert!(
            ui::time::now() < deadline,
            "gave up waiting for {what}: on {:?}, ledger {:?}",
            centre(app).page,
            ui::attention::entries()
                .last()
                .map(|entry| entry.what().to_owned()),
        );
    }
}

/// Wait until the row `item` left the stack and the work it asked for was
/// asked again: `before` is how much of it had been asked before, so one
/// further ask is the original failure and the one after it the answer.
fn until_answered(
    app: &mut Window,
    item: &ui::attention::Item,
    devices: &stand_in::Shared,
    before: usize,
    what: &str,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let gone = !ui::attention::items()
            .iter()
            .any(|waiting| waiting.id() == item.id());
        if gone && asked(devices).len() > before.saturating_add(1) {
            return;
        }
        assert!(ui::time::now() < deadline, "gave up waiting for {what}");
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

/// Press the waiting row's one action, as the stack does.
fn try_again(app: &mut Window, item: &ui::attention::Item) {
    centre(app)
        .needs
        .pressed(ui::attention::Press::Act(item.id()));
    app.settle();
}

/// Wait for the failure to wait, check it carries Try again and says why,
/// let the stand-in `devices` (if any) work again, and press Try again.
fn fail_then_try_again(
    app: &mut Window,
    devices: Option<&stand_in::Shared>,
) -> ui::attention::Item {
    let item = until_waiting(app, "the failure to wait for a person");
    assert_eq!(item.action().label, "Try again");
    assert!(
        !item.detail_line().is_empty() || !item.title().is_empty(),
        "a failure says what happened"
    );
    if let Some(devices) = devices {
        set_failing(devices, false);
    }
    try_again(app, &item);
    item
}

/// Adding, preferring, pausing and resuming a printer, cancelling and
/// retrying a document: Try again runs the same effect on the same printer
/// or document.
#[test]
fn a_failed_printer_or_document_operations_try_again_runs_it_again() {
    let _serial = crate::surface::serial();
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
    for effect in &effects {
        // The search for nearby printers is periodic: an answered failure
        // must still find what it asked about.
        centre(&mut app).discovered = Some(Ok(super::super::demo_discovered_printers()));
        let before = asked(&devices).len();
        set_failing(&devices, true);
        let _ = app.effect(effect);
        let item = fail_then_try_again(&mut app, Some(&devices));
        until_answered(
            &mut app,
            &item,
            &devices,
            before,
            "the work asked again to go through",
        );
        // Another test in this process may be asking the same stand-in for
        // its own work: what matters is that the very work that failed is
        // asked again.
        let asked = asked(&devices);
        let Some(first) = asked.get(before) else {
            panic!("the work was not asked at all: {asked:?}");
        };
        assert!(
            asked[before + 1..].contains(first),
            "Try again asks for the very work that failed: {asked:?}"
        );
    }
}

/// Printing that failed: Try again prints the same document on the same
/// printer with the same settings.
#[test]
fn a_failed_prints_try_again_prints_it_again() {
    let _serial = crate::surface::serial();
    let (mut app, devices) = opened();
    centre(&mut app).stage_scenario(Some("print"));
    centre(&mut app).print_copies.set(3.0);
    app.settle();
    let before = asked(&devices).len();
    set_failing(&devices, true);
    let _ = app.effect(&PrintCentreEffect::SendToPrinter);
    let item = fail_then_try_again(&mut app, Some(&devices));
    until_answered(
        &mut app,
        &item,
        &devices,
        before,
        "the document to be sent again",
    );
    let asked = asked(&devices);
    assert!(
        asked[before].contains("Studio_printer")
            && asked[before].contains("Travel itinerary.pdf")
            && asked[before].contains("copies: 3"),
        "{asked:?}"
    );
    assert!(
        asked[before + 1..].contains(&asked[before]),
        "the same document on the same printer: {asked:?}"
    );
}

/// A page that did not scan: Try again scans with the same scanner.
#[test]
fn a_failed_scans_try_again_scans_again() {
    let _serial = crate::surface::serial();
    let (mut app, devices) = opened();
    centre(&mut app).stage_scenario(Some("scan"));
    app.settle();
    let pages = centre(&mut app).scan.pages.len();
    let before = asked(&devices).len();
    set_failing(&devices, true);
    let _ = app.effect(&PrintCentreEffect::ScanPage);
    fail_then_try_again(&mut app, Some(&devices));
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        if centre(&mut app).scan.pages.len() == pages.saturating_add(1) {
            break;
        }
        assert!(ui::time::now() < deadline, "the page to be scanned again");
    }
    assert_eq!(
        asked(&devices)[before..],
        ["scan scanner0", "scan scanner0"]
    );
}

/// Text that was not recognized: Try again recognizes the same page.
#[test]
fn a_failed_recognitions_try_again_recognizes_the_page_again() {
    let _serial = crate::surface::serial();
    let (mut app, devices) = opened();
    centre(&mut app).stage_scenario(Some("scan-result"));
    app.settle();
    let before = asked(&devices).len();
    set_failing(&devices, true);
    let _ = app.effect(&PrintCentreEffect::RecognizeText);
    fail_then_try_again(&mut app, Some(&devices));
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        let done = centre(&mut app)
            .scan
            .result
            .as_ref()
            .is_some_and(|page| page.recognized_text.as_deref() == Some("Stand-in text"));
        if done {
            break;
        }
        assert!(
            ui::time::now() < deadline,
            "the text to be recognized again"
        );
    }
    assert_eq!(asked(&devices)[before..], ["recognize", "recognize"]);
}

/// A success needs nobody: it is recorded in the ledger and no row waits.
#[test]
fn a_success_is_recorded_and_nobody_is_called() {
    let _serial = crate::surface::serial();
    let (mut app, devices) = opened();
    let _ = app.effect(&PrintCentreEffect::PausePrinter {
        printer: "Studio_printer".into(),
    });
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.settle();
        if ui::attention::entries()
            .iter()
            .any(|entry| entry.what() == "Printer paused. Documents will wait until you resume it.")
        {
            break;
        }
        assert!(ui::time::now() < deadline, "the pause to be recorded");
    }
    assert_eq!(asked(&devices).len(), 1);
    assert!(
        !ui::attention::items()
            .iter()
            .any(|item| item.title().contains("Printer paused")),
        "a success asks nothing of anyone"
    );
    assert_eq!(centre(&mut app).page, Page::Dashboard);
}

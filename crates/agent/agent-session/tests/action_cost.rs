//! What an action on a tracker nobody watches costs: its own work, never a
//! frame of the app's view.
//!
//! The times are printed for reading (`cargo test -p tilo-agent-session
//! --test it action_cost -- --nocapture`), never asserted: on a shared
//! machine they vary with the load. The counts are asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::tilo_resource::profiling::observe;

/// The issues a tracker holds before its actions are timed.
const ISSUES: usize = 200;

/// The actions timed on it.
const ACTIONS: usize = 30;

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ))
}

/// File one issue through the tracker's own action.
fn file(page: &mut Replica, address: &ActionAddress, number: usize) {
    page.invoke(
        address,
        serde_json::json!({"title": format!("Issue {number}")})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the issue is filed");
}

/// Make one project on the tracker itself, which changes none of its issues:
/// what an agent does to a tracker between the issues it works.
fn project(page: &mut Replica, address: &ActionAddress, number: usize) {
    page.invoke(
        address,
        serde_json::json!({"name": format!("Project {number}")})
            .as_object()
            .unwrap(),
        false,
    )
    .expect("the project is made");
}

fn median(mut took: Vec<Duration>) -> Duration {
    took.sort();
    took.get(took.len() / 2).copied().unwrap_or_default()
}

/// A tracker nobody watches answers its actions without laying its view out:
/// the work of the action lands before it answers, and a tracker holding many
/// issues costs what the one action does, not a frame of all of them.
#[test]
fn an_action_on_a_tracker_nobody_watches_lays_nothing_out() {
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let address = ActionAddress::new(issues(), "create");
    for number in 0..ISSUES {
        file(&mut page, &address, number);
    }
    assert!(
        page.survey_rows().len() >= ISSUES,
        "the tracker holds every issue filed through it"
    );

    // The first actions of a view pay what opening it left owed; the
    // steady state an agent moving a tracker in sees is what is timed.
    let tracker = page
        .resources()
        .into_iter()
        .find(|resource| resource.kind == "tasks.tracker")
        .map(|resource| ActionAddress::new(resource.id.as_str(), "create-area"))
        .expect("the tracker is the only one it holds");
    let rows_before = page.survey_rows().len();
    let mut took = Vec::with_capacity(ACTIONS);
    let ((), acting) = observe(|| {
        for number in 0..5 {
            project(&mut page, &tracker, number);
        }
        for number in 0..ACTIONS {
            let started = Instant::now();
            project(&mut page, &tracker, 5 + number);
            took.push(started.elapsed());
        }
    });
    assert_eq!(
        layouts(&acting),
        0,
        "an action on a tracker nobody watches builds no frame: {acting:?}"
    );
    println!(
        "a tracker of {ISSUES} issues: {ACTIONS} actions, median {:?}, total {:?}",
        median(took.clone()),
        took.iter().sum::<Duration>()
    );
    let rows_after = page.survey_rows().len();
    println!("rows before {rows_before}, after {rows_after}");
    assert!(
        rows_after > rows_before,
        "every action left what it made ({rows_before} then {rows_after})"
    );
}

/// The address of the operation `action` the resource offers its member.
fn offered(page: &Replica, action: &str) -> ActionAddress {
    page.survey()
        .into_iter()
        .find(|descriptor| descriptor.address.action == action)
        .unwrap_or_else(|| panic!("the resource offers {action}"))
        .address
}

/// The frames `observed` built.
fn layouts(observed: &[ui::tilo_resource::profiling::Sample]) -> usize {
    observed
        .iter()
        .filter(|phase| phase.phase == "ui.layout")
        .map(|phase| phase.calls)
        .sum()
}

/// What `invoke` of `address` with `arguments` took and answered; it must
/// build no frame of the resource's view.
fn timed(
    page: &mut Replica,
    address: &ActionAddress,
    arguments: &serde_json::Value,
) -> (Duration, serde_json::Value) {
    let started = Instant::now();
    let (output, acting) = observe(|| {
        page.invoke(address, arguments.as_object().unwrap(), false)
            .unwrap_or_else(|error| panic!("{} answers: {error}", address.action))
    });
    let took = started.elapsed();
    assert_eq!(
        layouts(&acting),
        0,
        "{} on a resource nobody watches builds no frame: {acting:?}",
        address.action
    );
    (took, output.value.unwrap_or_default())
}

/// The nanoseconds `phase` took inside one `invoke` of `address` with
/// `arguments`: the work the app does for the action, apart from what the
/// framework pays every action on a document.
fn phased(
    page: &mut Replica,
    address: &ActionAddress,
    arguments: &serde_json::Value,
    phase: &str,
) -> u128 {
    let (_, acting) = observe(|| {
        page.invoke(address, arguments.as_object().unwrap(), false)
            .unwrap_or_else(|error| panic!("{} answers: {error}", address.action))
    });
    acting
        .iter()
        .filter(|sample| sample.phase == phase)
        .map(|sample| sample.elapsed_ns)
        .sum()
}

/// An effect or a job takes its input from the resource and its arguments,
/// never from what the view shows: a PDF pinned and read at once (no one
/// looked at the resource between the two) reads the pinned PDF, and reading a
/// page, looking at it and reading the form lay nothing out, at 2 pages or 80.
#[test]
fn a_pdf_pinned_and_read_at_once_lays_nothing_out() {
    for pages in [2, 80] {
        let said: Vec<String> = (1..=pages)
            .map(|page| format!("Page {page} says lamps"))
            .collect();
        let said: Vec<&str> = said.iter().map(String::as_str).collect();
        // Sealed, as the host keeps what is put in a workspace.
        let content = tilo_content::ContentStore::memory()
            .sealing()
            .for_one_workspace();
        let reference = content.put(&crate::common::pdf(&said)).unwrap();
        let mut page = Replica::open_lending(
            &crate::common::catalogue(),
            "so.tilo.pdf-viewer",
            crate::common::WORKSPACE,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
            false,
            Some(content),
        )
        .unwrap();
        let pin = offered(&page, "pin-source");
        let read = offered(&page, "read-text");
        let look = offered(&page, "look-page");
        let form = offered(&page, "read-form-fields");
        timed(
            &mut page,
            &pin,
            &serde_json::json!({
                "source": tilo_actions::content_resource(&reference),
                "name": "inventory.pdf",
            }),
        );
        let (read_took, text) = timed(
            &mut page,
            &read,
            &serde_json::json!({"from_page": 2, "to_page": 2}),
        );
        assert!(
            text["pages"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Page 2 says lamps")),
            "the PDF the resource pins reads: {text}"
        );
        let (look_took, _) = timed(
            &mut page,
            &look,
            &serde_json::json!({"page": 1, "size": 128}),
        );
        let (_, fields) = timed(&mut page, &form, &serde_json::json!({}));
        assert_eq!(fields["file"], "inventory.pdf", "{fields}");
        println!("a PDF of {pages} pages: read-text {read_took:?}, look-page {look_took:?}");
    }
}

/// The sheet a new sheets document keeps its first sheet under.
const FIRST_SHEET: &str = "00000000-0000-4000-8000-000000000001";

/// The rows of `count` lamps, each with what it costs and how many there are.
fn lamp_rows(count: usize) -> Vec<serde_json::Value> {
    (0..count)
        .map(|row| serde_json::json!([format!("Lamp {row}"), row, row * 2]))
        .collect()
}

/// What a sheet's actions cost is the work of the cells they name, never the
/// sheet's size: writing 2,000 rows is thousands of cells a second, and reading
/// a few cells costs what those cells do at 2,000 rows as at 10.
///
/// The framework pays every action on a document a little of its size — the
/// change a replica applies, the snapshot an effect is lent — so the read is
/// asserted on the work the app does for it (`sheets.read_cells`); the times a
/// whole action takes at both sizes are printed beside it.
#[test]
fn a_sheets_actions_cost_the_same_at_ten_rows_and_two_thousand() {
    let mut measured = Vec::new();
    for rows in [10_usize, 2_000] {
        let mut page = Replica::open_lending(
            &crate::common::catalogue(),
            "so.tilo.sheets",
            crate::common::WORKSPACE,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
            false,
            Some(tilo_content::ContentStore::memory_in_workspace()),
        )
        .unwrap();
        let set = offered(&page, "set-cells");
        let read = offered(&page, "read-cells");
        let (set_took, _) = timed(
            &mut page,
            &set,
            &serde_json::json!({"sheet": FIRST_SHEET, "start": "A1", "rows": lamp_rows(rows)}),
        );
        // The first read answers on the sheet's own state; the rest are what an
        // agent working a sheet pays per read.
        let (first_took, first) = timed(
            &mut page,
            &read,
            &serde_json::json!({"sheet": FIRST_SHEET, "range": "A1:C3"}),
        );
        assert!(
            first.to_string().contains("Lamp 2"),
            "the cells set read: {first}"
        );
        let mut reads = vec![first_took];
        for _ in 0..9 {
            let (took, _) = timed(
                &mut page,
                &read,
                &serde_json::json!({"sheet": FIRST_SHEET, "range": "A1:C3"}),
            );
            reads.push(took);
        }
        let reading = phased(
            &mut page,
            &read,
            &serde_json::json!({"sheet": FIRST_SHEET, "range": "A1:C3"}),
            "sheets.read_cells",
        );
        let last = format!("A{}:C{rows}", rows.saturating_sub(2));
        let (last_took, tail) = timed(
            &mut page,
            &read,
            &serde_json::json!({"sheet": FIRST_SHEET, "range": last}),
        );
        assert!(
            tail.to_string().contains(&format!("Lamp {}", rows - 1)),
            "the last rows set read: {tail}"
        );
        println!(
            "a sheet of {rows} rows: set-cells of {} cells {set_took:?}, first read {first_took:?}, \
             median read {:?}, the read itself {reading} ns, last rows {last_took:?}",
            rows * 3,
            median(reads.clone())
        );
        measured.push((rows, set_took, reading));
    }
    let (small_rows, small_set, small_reading) = measured[0];
    let (big_rows, big_set, big_reading) = measured[1];
    let per_cell = |took: Duration, rows: usize| took.as_secs_f64() / (rows * 3) as f64;
    assert!(
        per_cell(big_set, big_rows) < per_cell(small_set, small_rows) * 3.0,
        "writing a cell costs what it does at {small_rows} rows at {big_rows} too: \
         {big_set:?} for {} cells against {small_set:?}",
        big_rows * 3
    );
    assert!(
        big_reading < small_reading * 3 + 1_000_000,
        "a read of a few cells costs what it does on a small sheet: {big_reading} ns at \
         {big_rows} rows against {small_reading} ns at {small_rows}"
    );
    assert!(
        big_set.as_secs_f64() <= 6.0,
        "writing {} cells takes under six seconds: {big_set:?}",
        big_rows * 3
    );
}

/// A sheet's reads and its export as content lay nothing out, whether it
/// holds 10 rows or 2,000: they read the workbook the document holds.
#[test]
fn a_sheet_read_and_exported_lays_nothing_out() {
    for rows in [10_usize, 2_000] {
        let mut page = Replica::open_lending(
            &crate::common::catalogue(),
            "so.tilo.sheets",
            crate::common::WORKSPACE,
            &MemberId::person("ada"),
            ReplicaActor::Owner,
            Seed::New,
            false,
            Some(tilo_content::ContentStore::memory_in_workspace()),
        )
        .unwrap();
        let set = offered(&page, "set-cells");
        let read = offered(&page, "read-cells");
        let export = offered(&page, "export-csv-content");
        let sheet = "00000000-0000-4000-8000-000000000001";
        let cells: Vec<serde_json::Value> = (0..rows)
            .map(|row| serde_json::json!([format!("Lamp {row}"), row, row * 2]))
            .collect();
        timed(
            &mut page,
            &set,
            &serde_json::json!({"sheet": sheet, "start": "A1", "rows": cells}),
        );
        let (read_took, cells) = timed(
            &mut page,
            &read,
            &serde_json::json!({"sheet": sheet, "range": "A1:C3"}),
        );
        assert!(
            cells.to_string().contains("Lamp 2"),
            "the cells set read: {cells}"
        );
        let (export_took, _) = timed(&mut page, &export, &serde_json::json!({"sheet": sheet}));
        println!(
            "a sheet of {rows} rows: read-cells {read_took:?}, export-csv-content {export_took:?}"
        );
    }
}

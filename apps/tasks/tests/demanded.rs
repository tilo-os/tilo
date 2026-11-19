//! Opening an issue of a long tracker reads that issue's description and thread and no other
//! issue's: the same reads at 40 issues as at 400. The window's list read none of them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tasks::App;
use ui::prelude::*;

/// How many issue descriptions and threads (`#[model(demanded)]` fields) `scopes` read.
fn demanded_reads(scopes: &[ui::tilo_resource::profiling::Sample]) -> usize {
    scopes
        .iter()
        .filter(|scope| scope.phase == "resource.read_demanded")
        .map(|scope| scope.calls)
        .sum()
}

/// Open the issue whose row reads `#number`, as a press on the row does: what it read of
/// descriptions and threads.
fn open_issue(app: &mut TestApp<App>, number: u64) -> usize {
    let reference = format!("#{number} ");
    let row = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label()
                .is_some_and(|label| label.starts_with(&reference))
        })
        .map(|(id, _)| id)
        .expect("the list shows the issue's row");
    let ((), scopes) = ui::tilo_resource::profiling::observe(|| {
        app.harness()
            .perform_a11y_action(row, &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
    });
    assert!(
        app.harness().targets().contains_key("tasks-issue-title"),
        "the row opens the issue's page"
    );
    demanded_reads(&scopes)
}

#[test]
fn opening_an_issue_reads_its_description_and_thread_alone_at_any_size() {
    ui::i18n::I18nManager::init("en-US");
    let mut reads = Vec::new();
    for issues in [40, 400] {
        let mut app = TestApp::<App>::drive(&format!("issues-{issues}")).expect("open");
        reads.push(open_issue(&mut app, 3));
    }
    // One issue's description and thread, whatever the tracker holds.
    assert_eq!(
        reads,
        [2, 2],
        "reads of descriptions and threads at 40 and 400 issues"
    );
}

//! Opening an issue of a long tracker reads that issue's description and thread and no other
//! issue's: the same reads at 40 issues as at 400. The window's list read none of them, and the
//! page carries no action of the issues it does not show.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]


use tasks::App;
use ui::prelude::*;

/// How many times `scopes` entered `phase`.
fn calls(scopes: &[ui::tilo_resource::profiling::Sample], phase: &str) -> usize {
    scopes
        .iter()
        .filter(|scope| scope.phase == phase)
        .map(|scope| scope.calls)
        .sum()
}

/// Open the issue whose row reads `#number`, as a press on the row does: how many descriptions
/// and threads (`#[model(demanded)]` fields) it read, and how many actions it carried out on the
/// tracker's resources.
fn open_issue(app: &mut TestApp<App>, number: u64) -> (usize, usize) {
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
    (
        calls(&scopes, "resource.read_demanded"),
        calls(&scopes, "app.route_spec"),
    )
}

#[test]
fn opening_an_issue_reads_its_description_and_thread_alone_at_any_size() {
    ui::i18n::I18nManager::init("en-US");
    let mut reads = Vec::new();
    let mut routed = Vec::new();
    for issues in [40, 400] {
        let mut app = TestApp::<App>::drive(&format!("issues-{issues}")).expect("open");
        let (read, actions) = open_issue(&mut app, 3);
        reads.push(read);
        routed.push(actions);
    }
    // One issue's description and thread, whatever the tracker holds.
    assert_eq!(
        reads,
        [2, 2],
        "reads of descriptions and threads at 40 and 400 issues"
    );
    // The actions of the page it opens, and none of the issues it does not show.
    assert_eq!(
        routed[0], routed[1],
        "actions carried out on the tracker's resources at 40 and 400 issues"
    );
}

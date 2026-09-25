use std::time::Duration;

use generated_app::{App, Scan};
use ui::a11y::Action;
use ui::headless::SemanticExpectation;
use ui::prelude::TestApp;

#[test]
fn app_runs_background_work_on_the_resource_input() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    assert!(
        app.harness().resources().iter().any(|resource| {
            resource.id.as_str() == "generated.background-task:root"
                && resource.actions.iter().any(|action| action.id == "run")
        }),
        "the task is an action the palette, agents and scripts can start"
    );
    app.harness()
        .perform_named("run-task", &Action::Activate)
        .expect("the run button remains addressable");
    app.harness()
        .wait_for_named("task-result", Duration::from_secs(2))
        .expect("the background task completes");
    app.harness()
        .expect(&SemanticExpectation::named("task-result").name_contains("Found"))
        .expect("the result replaces the loading state");
}

#[test]
fn the_scanned_folder_is_durable() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    app.invoke(Scan::SET_FOLDER.with("..".into()))
        .expect("choose a folder");
    let app = app.reopen().expect("the app reopens");
    assert_eq!(app.state().folder, "..");
}

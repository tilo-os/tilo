#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use activity::App;
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::prelude::*;
use ui::tilo_actions::remote::workspace_activity as wire;
use ui::tilo_actions::remote::{
    InvocationState, InvokeArgument, InvokeRequest, RemoteActionMessage,
};
use ui::tilo_actions::{InvocationId, InvocationUpdate};

fn english() {
    ui::i18n::I18nManager::init("en-US");
}

fn shows(app: &mut TestApp<App>, target: &str) -> bool {
    app.harness().targets().contains_key(target)
}

/// Whether the window says `text`, as a name or description a screen
/// reader reads.
fn says(app: &mut TestApp<App>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
    })
}

#[test]
fn every_scenario_shows_its_place_built_from_components() {
    english();
    let cases: [(&str, &[&str]); 5] = [
        (
            "whats-new",
            &["activity-brief-0", "activity-brief-1", "activity-resume"],
        ),
        ("caught-up", &["activity-caught-up", "activity-resume"]),
        (
            "history",
            &[
                "activity-filter-period",
                "activity-filter-person",
                "activity-filter-lens",
                "activity-group-select-0",
                "activity-detail-none",
            ],
        ),
        (
            "detail",
            &[
                "activity-offer-undo",
                "activity-offer-restore",
                "activity-change-0",
                "activity-name",
            ],
        ),
        (
            "replay",
            &["activity-replay-position", "activity-replay-change"],
        ),
    ];
    for (scenario, targets) in cases {
        let mut app = TestApp::<App>::capture(Some(scenario)).expect("capture");
        for target in targets {
            assert!(shows(&mut app, target), "{scenario} shows {target}");
        }
    }
}

/// The reads and writes the window asked the session for since last asked.
fn asked(app: &mut TestApp<App>) -> Vec<InvokeRequest> {
    app.frame();
    app.harness()
        .take_collaboration_requests()
        .into_iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } => Some(request),
            _ => None,
        })
        .collect()
}

/// The read of `action` among `requests`; a missing one fails where the
/// test names the action.
#[track_caller]
fn read<'a>(requests: &'a [InvokeRequest], action: &str) -> &'a InvokeRequest {
    requests
        .iter()
        .find(|request| request.target == wire::target(action))
        .expect("the window reads the action named")
}

fn answer(
    app: &mut TestApp<App>,
    invocation: InvocationId,
    sequence: u64,
    value: serde_json::Value,
) {
    app.harness()
        .handle_collaboration_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Item {
                        invocation,
                        sequence,
                        value,
                    },
                )),
            },
        });
    app.frame();
}

fn brief(title: &str, entries: &[&str]) -> wire::Brief {
    wire::Brief {
        title: title.to_owned(),
        entries: entries.iter().map(|entry| (*entry).to_owned()).collect(),
        changes: entries.len(),
        ..wire::Brief::default()
    }
}

#[test]
fn whats_new_follows_the_session_and_marks_what_it_showed_as_viewed() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let summary = read(&requests, wire::SUMMARY).invocation;
    assert!(
        !shows(&mut app, "activity-brief-0"),
        "nothing is shown before the session answers"
    );

    let first = wire::Summary {
        briefs: vec![brief("Ada rewrote the intro", &["aa", "ab"])],
        ..wire::Summary::default()
    };
    answer(&mut app, summary, 1, serde_json::to_value(&first).unwrap());
    assert!(shows(&mut app, "activity-brief-0"));

    // The history moved: the session sends the summary again, unasked.
    let second = wire::Summary {
        briefs: vec![
            brief("Grace moved the venue", &["ac"]),
            brief("Ada rewrote the intro", &["aa", "ab"]),
        ],
        ..wire::Summary::default()
    };
    answer(&mut app, summary, 2, serde_json::to_value(&second).unwrap());
    assert!(
        shows(&mut app, "activity-brief-1"),
        "the new work session is listed live"
    );

    app.harness().click_named("activity-mark-viewed").unwrap();
    let requests = asked(&mut app);
    let viewed = read(&requests, wire::MARK_VIEWED);
    let InvokeArgument::Json(argument) = &viewed.argument else {
        panic!("marking viewed names the changes");
    };
    let viewed: wire::Viewed =
        serde_json::from_value(serde_json::Value::Object(argument.clone())).unwrap();
    assert_eq!(viewed.entries, ["ac", "aa", "ab"], "every change it showed");
    assert!(!viewed.later);
}

#[test]
fn a_refused_read_says_so_instead_of_showing_history() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let invocation = read(&requests, wire::SUMMARY).invocation;
    app.harness()
        .handle_collaboration_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Update {
                invocation,
                state: Some(InvocationState::new(
                    invocation,
                    InvocationUpdate::Failed {
                        invocation,
                        error: ui::tilo_actions::ActionFailure {
                            code: "not_permitted".into(),
                            message: "Only members read this workspace's history".into(),
                            details: None,
                            retryable: false,
                        },
                    },
                )),
            },
        });
    app.frame();
    assert!(
        says(&mut app, "Only members read this workspace's history"),
        "the refusal says why"
    );
    assert!(!shows(&mut app, "activity-brief-0"));
}

#[test]
fn a_focus_asked_for_outside_opens_history_on_it_once() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let focus = read(&requests, wire::FOCUS).invocation;
    let asked_for = wire::Focus {
        serial: 7,
        view: wire::View::History,
        author: Some("Grace".into()),
        lens: None,
        entry: Some("ac".into()),
    };
    answer(
        &mut app,
        focus,
        1,
        serde_json::to_value(Some(&asked_for)).unwrap(),
    );
    assert_eq!(app.state().author, "Grace", "History shows Grace's changes");
    let requests = asked(&mut app);
    read(&requests, wire::FEED);
    let detail = read(&requests, wire::DETAIL);
    let InvokeArgument::Json(argument) = &detail.argument else {
        panic!("the detail names its range");
    };
    let query: wire::DetailQuery =
        serde_json::from_value(serde_json::Value::Object(argument.clone())).unwrap();
    assert_eq!(
        query.range,
        wire::Range::one("ac"),
        "the change is selected"
    );
    let shown = read(&requests, wire::FOCUSED);
    let InvokeArgument::Json(argument) = &shown.argument else {
        panic!("the shown focus is named");
    };
    assert_eq!(argument.get("serial"), Some(&serde_json::json!(7)));
}

#[test]
fn a_view_asked_for_outside_opens_that_place() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let requests = asked(&mut app);
    let focus = read(&requests, wire::FOCUS).invocation;
    let asked_for = wire::Focus {
        serial: 3,
        view: wire::View::Replay,
        ..wire::Focus::default()
    };
    answer(
        &mut app,
        focus,
        1,
        serde_json::to_value(Some(&asked_for)).unwrap(),
    );
    let requests = asked(&mut app);
    read(&requests, wire::REPLAY);
    read(&requests, wire::FOCUSED);
}

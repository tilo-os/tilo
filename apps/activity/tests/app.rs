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

/// Whether the window says `text`, as a name, value or description a screen reader reads.
fn says(app: &mut TestApp<App>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        [node.label(), node.value(), node.description()]
            .into_iter()
            .flatten()
            .any(|said| said.contains(text))
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
        ("caught-up", &["activity-see-history", "activity-resume"]),
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
        // Facts are parts of a row, never joined with a glyph: where the person left off
        // names its app and its time apart.
        assert!(!says(&mut app, "·"), "{scenario} joins facts with a dot");
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

/// The read of `action` among `requests`; a missing one fails where the test names the
/// action.
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
                correlation: None,
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
                correlation: None,
            },
        });
    app.frame();
    assert!(
        says(&mut app, "Only members read this workspace's history"),
        "the refusal says why"
    );
    assert!(!shows(&mut app, "activity-brief-0"));
}

/// A change one device will not take says so on the row's own line: where it is not, and
/// why.
#[test]
fn a_change_a_device_refused_says_where_it_is_not() {
    english();
    let mut app = TestApp::<App>::capture(Some("history")).expect("capture");
    assert!(
        says(&mut app, "Not on Ada's laptop: no permission"),
        "the row of the refused change says where it is not"
    );
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
    let requests = asked(&mut app);
    assert_eq!(
        feed_query(read(&requests, wire::FEED))
            .expect("the feed names its query")
            .author
            .as_deref(),
        Some("Grace"),
        "History shows Grace's changes"
    );
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

/// Whether the window offers "Mark as viewed" as an action to take: its button sits in the
/// header row of the column it acts on, the window's bar hosting the window or not.
fn mark_viewed_offered(scenario: &str) -> bool {
    fn find(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("activity-mark-viewed") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let mut app = TestApp::<App>::capture(Some(scenario)).expect("capture");
    app.harness().present_in_window_bar();
    let _ = app.harness().poll_frame();
    let _ = app.harness().poll_frame();
    assert!(
        ui::window_toolbar::declared().is_none(),
        "the bar holds only what is the window's"
    );
    let inspection = app.harness().inspection();
    let mark = find(&inspection.root).expect("the header row holds Mark as viewed");
    assert!(
        mark.rect[2] > 0.0,
        "Mark as viewed stands where it is reached"
    );
    !mark
        .semantics
        .as_ref()
        .is_some_and(|access| access.state.disabled)
}

/// Mark as viewed acts while there is something new, and is disabled once What's new is
/// caught up.
#[test]
fn mark_as_viewed_is_offered_only_while_there_is_something_new() {
    english();
    assert!(mark_viewed_offered("whats-new"));
    assert!(!mark_viewed_offered("caught-up"));
}

/// The name of the row the history lists for the `n`th change of the scenario's workspace.
fn change(n: u8) -> String {
    format!("activity-entry-{}", wire::entry_hex(&[n; 16]))
}

/// The query a feed read asks for, when it names one.
fn feed_query(request: &InvokeRequest) -> Option<wire::FeedQuery> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return None;
    };
    serde_json::from_value(serde_json::Value::Object(argument.clone())).ok()
}

/// A change by `name`, the `n`th of a test's feed.
fn by(name: &str, n: u8, summary: &str) -> wire::Entry {
    wire::Entry {
        id: wire::entry_hex(&[n; 16]),
        author: wire::Author {
            name: name.to_owned(),
            identity: name.to_lowercase(),
            ..wire::Author::default()
        },
        at_ms: 1_700_000_000_000_i64.saturating_add(i64::from(n)),
        summary: summary.to_owned(),
        ..wire::Entry::default()
    }
}

/// The feed of `entries`, as the session answers one.
fn feed_of(entries: Vec<wire::Entry>) -> wire::Feed {
    wire::Feed {
        total: entries.len(),
        entries,
        people: vec!["Ada".to_owned(), "Grace".to_owned()],
        ..wire::Feed::default()
    }
}

/// Everything the window asked the session for since last asked, reads and writes alike.
fn exchanged(app: &mut TestApp<App>) -> Vec<CollabRequest> {
    app.frame();
    app.harness().take_collaboration_requests()
}

/// The invoke of `action` among `requests`; a missing one fails where the test names it.
#[track_caller]
fn invoke_of<'a>(requests: &'a [CollabRequest], action: &str) -> &'a InvokeRequest {
    requests
        .iter()
        .find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } if request.target == wire::target(action) => Some(request),
            _ => None,
        })
        .expect("the window asks for the action named")
}

/// How many document changes `requests` carry: what narrowing the view must never make.
fn document_changes(requests: &[CollabRequest]) -> usize {
    requests
        .iter()
        .filter(|request| matches!(request, CollabRequest::Document { .. }))
        .count()
}

/// The window's record of the workspace: its committed heads and the writes still to commit.
fn document_state(app: &TestApp<App>) -> (usize, usize) {
    use ui::automerge::transaction::Transactable as _;
    app.read_document(|doc| (doc.clone().get_heads().len(), doc.pending_ops()))
}

/// The window bar's search is History's: what is typed narrows the feed through the filter
/// the app already keeps, and the bar counts what the filter left of the page it read.
#[test]
fn the_window_bars_search_narrows_history_and_the_bar_counts_what_it_kept() {
    english();
    let mut app = TestApp::<App>::capture(Some("history")).expect("capture");
    app.harness().present_in_window_bar();
    app.settle();
    assert!(shows(&mut app, &change(6)), "every change before a search");

    app.harness()
        .search_in_window(ui::window_toolbar::SearchOpening::Find, "press");
    app.settle();
    assert_eq!(
        app.harness().window_search_count(),
        Some(ui::widget::FilterCount::new(3, 6)),
        "the three changes to the press release, of the six it read"
    );
    for matched in [1, 2, 3] {
        assert!(
            shows(&mut app, &change(matched)),
            "change {matched} says press"
        );
    }
    for other in [4, 5, 6] {
        assert!(
            !shows(&mut app, &change(other)),
            "change {other} does not say press"
        );
    }

    app.harness()
        .perform_named("window-search-close", &ui::a11y::Action::Activate)
        .expect("the search closes");
    app.settle();
    assert_eq!(app.harness().window_search_count(), None);
    assert!(
        shows(&mut app, &change(6)),
        "closing the search shows every change again"
    );
}

/// Open History as the shell asks for it — `author`'s changes, or everyone's — and answer the
/// feed that filter reads with `entries`.
fn open_history(
    app: &mut TestApp<App>,
    serial: u64,
    author: Option<&str>,
    entries: Vec<wire::Entry>,
) {
    let requests = exchanged(app);
    let focus = invoke_of(&requests, wire::FOCUS).invocation;
    answer(
        app,
        focus,
        1,
        serde_json::to_value(Some(&wire::Focus {
            serial,
            view: wire::View::History,
            author: author.map(str::to_owned),
            ..wire::Focus::default()
        }))
        .unwrap(),
    );
    let requests = exchanged(app);
    assert_eq!(
        document_changes(&requests),
        0,
        "showing History writes nothing"
    );
    let feed = invoke_of(&requests, wire::FEED);
    assert_eq!(
        feed_query(feed)
            .expect("the feed names its query")
            .author
            .as_deref(),
        author,
        "the feed asks for the changes History shows"
    );
    answer(
        app,
        feed.invocation,
        1,
        serde_json::to_value(feed_of(entries)).unwrap(),
    );
}

/// The feed `app` asks for after a change of the filter, once it settles; its author is
/// checked, and its choices are answered with `entries`.
fn refiltered(app: &mut TestApp<App>, author: Option<&str>, entries: Vec<wire::Entry>) {
    app.settle();
    let requests = exchanged(app);
    assert_eq!(
        document_changes(&requests),
        0,
        "narrowing a view writes nothing"
    );
    let feed = invoke_of(&requests, wire::FEED);
    assert_eq!(
        feed_query(feed)
            .expect("the feed names its query")
            .author
            .as_deref(),
        author,
        "the feed asks for the changes History shows"
    );
    answer(
        app,
        feed.invocation,
        1,
        serde_json::to_value(feed_of(entries)).unwrap(),
    );
}

/// The shell narrowing History to a person is the window's own: the feed is read for them,
/// their changes are listed, and the workspace records no change under their name.
#[test]
fn history_narrowed_to_the_person_the_shell_asks_for_writes_no_change() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let before = document_state(&app);
    open_history(
        &mut app,
        9,
        Some("Grace"),
        vec![by("Grace", 2, "Moved the venue to the river hall")],
    );
    assert!(shows(&mut app, &change(2)), "Grace's change is listed");
    assert_eq!(
        document_state(&app),
        before,
        "narrowing a view is not a change to the workspace"
    );
}

/// A person picks someone in the filter row, and clears it: the feed follows the picker, the
/// list shows whom it asks for, and none of it is a change to the workspace.
#[test]
fn choosing_a_person_and_clearing_filters_narrows_history_without_a_change() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let before = document_state(&app);
    open_history(
        &mut app,
        9,
        None,
        vec![
            by("Ada", 1, "Rewrote the press release intro"),
            by("Grace", 2, "Moved the venue to the river hall"),
        ],
    );
    for listed in [1, 2] {
        assert!(shows(&mut app, &change(listed)), "change {listed} is listed");
    }

    app.harness()
        .perform_named(
            "activity-filter-person",
            &ui::a11y::Action::SetValue("Ada".to_owned()),
        )
        .expect("the person picker takes the choice");
    refiltered(
        &mut app,
        Some("Ada"),
        vec![by("Ada", 1, "Rewrote the press release intro")],
    );
    assert!(shows(&mut app, &change(1)), "Ada's change is listed");
    assert!(!shows(&mut app, &change(2)), "Grace's change is not");

    app.harness()
        .click_named("activity-clear-filters")
        .expect("Clear filters is offered while the filter narrows");
    refiltered(
        &mut app,
        None,
        vec![
            by("Ada", 1, "Rewrote the press release intro"),
            by("Grace", 2, "Moved the venue to the river hall"),
        ],
    );
    for listed in [1, 2] {
        assert!(
            shows(&mut app, &change(listed)),
            "change {listed} is listed again"
        );
    }
    assert_eq!(
        document_state(&app),
        before,
        "narrowing a view is not a change to the workspace"
    );
}

/// The window bar's sidebar button flips the sidebar on every click, in a window wide
/// enough to dock it and in a narrow one.
#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(|| {
        ui::framework::testing::TestApp::<App>::capture(Some("whats-new"))
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn removing_a_morning_source_asks_the_session_through_the_published_effect() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    asked(&mut app);
    app.effect(&activity::ActivityEffect::RemoveSource {
        key: "weather".to_owned(),
    })
    .expect("the window answers its own effect");
    let requests = asked(&mut app);
    let InvokeArgument::Json(argument) = &read(&requests, wire::SOURCE).argument else {
        panic!("the request names the source");
    };
    let request: wire::SourceRequest =
        serde_json::from_value(serde_json::Value::Object(argument.clone())).unwrap();
    assert_eq!(request.key, "weather");
    assert_eq!(request.command, wire::SourceCommand::Remove);
}

/// German is one of the languages the app ships, and it reads in German: the name of the
/// app and its copy come from `locales/de-DE.ftl`, never from the English source catalogue
/// beside it.
#[test]
fn a_german_window_reads_the_activity_catalogue() {
    let catalogue = &activity::locales::CATALOG;
    assert!(
        catalogue.languages().contains(&"de-DE"),
        "the app ships German among {:?}",
        catalogue.languages()
    );
    catalogue.set_locale("de-DE");
    assert_eq!(catalogue.t("activity-title"), "Aktivität");
    assert_eq!(catalogue.t("activity-place-history"), "Verlauf");
    catalogue.set_locale("en-US");
    assert_eq!(catalogue.t("activity-title"), "Activity");
}

/// A long History scrolls under a person's wheel: the list moves up under it, toward its oldest
/// change.
#[test]
fn a_person_scrolls_history_toward_its_oldest_change() {
    english();
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    let entries = (1..=40)
        .rev()
        .map(|n| by("Ada", n, &format!("Change {n}")))
        .collect();
    open_history(&mut app, 3, None, entries);
    app.settle();
    let at = |app: &mut TestApp<App>, n: u8| app.harness().targets().get(&change(n)).copied();
    let newest = at(&mut app, 40).expect("the newest change is listed");
    let oldest = at(&mut app, 1).expect("the oldest change is laid out");
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(newest.center().x, newest.center().y));
    for _ in 0..60 {
        app.harness().scroll(0.0, -3.0);
        app.frame();
    }
    app.settle();
    let scrolled = at(&mut app, 1).expect("the oldest change is laid out");
    assert!(
        scrolled.y0 < oldest.y0 - 300.0,
        "the list moved up under the wheel: the oldest change was at {}, is at {}",
        oldest.y0,
        scrolled.y0
    );
}

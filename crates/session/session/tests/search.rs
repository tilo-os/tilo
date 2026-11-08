//! The desktop's search in the headless native session, driven only through what the session
//! publishes: search opens and takes its query by the desk's `search` , shows its rows as
//! resources, and a chosen row runs by its `choose-search-row` through the action host.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    RemoteActionTarget, application_action_target, application_open_target,
};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_app_launch::SessionEnvironment;
use tilo_compositor::{InputEvent, KeyRepeat};
use tilo_desktop::desk_verbs::{
    CHROME_RESOURCE, DESK_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND, launchpad_card_id,
};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_search::providers::settings::SETTINGS_APP;
use tilo_session::{AppActions, AppLists, Lists, Options, SearchOptions, Session};

/// The host search door every window and the shell publish: what a window's search control, its
/// toolbar's magnifier and Ctrl+K run (`tilo_ui::search::SearchRequest`).
const HOST_SEARCH: &str = "tilo.host:search";

type Arguments = serde_json::Map<String, serde_json::Value>;

/// An action host that keeps what it was asked to run.
#[derive(Default)]
struct Host {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
    /// The uris it was asked to open, by the workspace it was asked for.
    opened: RefCell<Vec<(String, String)>>,
    /// The machine's own apps it was asked to start.
    started: RefCell<Vec<String>>,
    /// What the workspace's apps publish, as their resources reach it.
    publishing: RefCell<Vec<PublishedActions>>,
}

impl AppActions for Host {
    fn published(&self, workspace: &str) -> Vec<PublishedActions> {
        self.publishing
            .borrow()
            .iter()
            .filter(|published| workspace.is_empty() || published.workspace == workspace)
            .cloned()
            .collect()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), DoorError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Ok(())
    }

    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError> {
        self.opened
            .borrow_mut()
            .push((workspace.to_owned(), uri.to_owned()));
        Ok(())
    }

    fn start_host_app(&self, desktop_id: &str) -> Result<(), DoorError> {
        self.started.borrow_mut().push(desktop_id.to_owned());
        Ok(())
    }
}

fn session(host: Rc<Host>) -> Session {
    session_over(host, None, None)
}

/// A session whose search also lists the records `records` keeps current and what apps published
/// into `content` .
fn session_over(
    host: Rc<Host>,
    records: Option<tilo_session::Records>,
    content: Option<Arc<AppContentIndex>>,
) -> Session {
    crate::common::headless(Options {
        search: SearchOptions {
            lists: Lists::fixed(AppLists {
                applications: vec![AppEntry {
                    id: "so.tilo.sheets".into(),
                    title: "Sheets".into(),
                    category: None,
                    description: None,
                }],
                ..AppLists::default()
            }),
            content,
            records,
            ..SearchOptions::default()
        },
        apps: Some(host),
        collab: false,
        ..Options::default()
    })
}

/// Run frames until `done` holds.
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    common::wait_for(session, "what the test asked for", |session| {
        done(session).then_some(())
    });
}

fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The open search's rows, as the desk publishes them.
fn rows(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| search.children)
        .unwrap_or_default()
}

/// The first row whose label starts with `title` .
fn row(session: &Session, title: &str) -> Option<ResourceSpec> {
    rows(session)
        .into_iter()
        .find(|row| row.kind == SEARCH_ROW_KIND && row.label.starts_with(title))
}

fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// Tap each key in turn, a frame apart, as a person's letters arrive.
fn keys(session: &mut Session, keycodes: &[u32]) {
    for keycode in keycodes {
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::Key {
                    keycode: *keycode,
                    pressed,
                })
                .expect("input");
            session.step(Duration::from_millis(12)).expect("step");
        }
    }
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// Press the search chord: Super and Space together.
fn chord(session: &mut Session) {
    for (keycode, pressed) in [(125, true), (57, true), (57, false), (125, false)] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("input");
        session.step(Duration::from_millis(1)).expect("step");
    }
    for _ in 0..10 {
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// Type one key of the keyboard, as the compositor takes it from the seat.
fn type_key(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("input");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The query the open search carries, as the desk publishes it.
fn query(session: &Session) -> Option<String> {
    find(&session.resources(), SEARCH_RESOURCE).map(|search| search.label)
}

/// A session with key repeat pushed past the test's own steps: a held key repeats, and a step of a
/// headless session can take longer than a repeat waits, so these keys are tapped instead.
fn seated() -> Session {
    let mut session = session(Rc::new(Host::default()));
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Open the desk's search: by its own action, or by the chord the person presses, Super+Space
/// through the seat.
fn open(session: &mut Session, by_chord: bool) {
    if by_chord {
        chord(session);
    } else {
        invoke(session, DESK_RESOURCE, "search", serde_json::json!({}));
    }
    until(session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
}

/// Keys typed while the desk's search is open go into its query field: the desk takes the keyboard
/// while its own search shows, rather than leaving the letters to the window behind it. Where the
/// search was opened by its action or by the chord, and whether the letters are tapped at once or
/// after the desk has settled, they all reach the same field.
#[test]
fn typing_reaches_the_searches_query_however_it_was_opened() {
    let _settled = ui::motion::testing::settled();
    for (by_chord, waited) in [(false, false), (true, false), (true, true)] {
        let mut session = seated();
        open(&mut session, by_chord);
        if waited {
            // The field keeps the keyboard while the desk redraws under it,
            // so a person who takes their time before typing loses nothing.
            await_search(&mut session, None);
        }
        for keycode in [46, 30, 38] {
            type_key(&mut session, keycode);
        }
        until(&mut session, |session| {
            query(session).as_deref() == Some("cal")
        });
    }
}

/// Letters a fast typist sends close together reach the query all the same: three keys land in one
/// batch, with no frame between press and release.
#[test]
fn typing_letters_in_one_batch_reaches_the_query() {
    let _settled = ui::motion::testing::settled();
    let mut session = seated();
    open(&mut session, true);
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    for keycode in [46, 30, 38] {
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::Key { keycode, pressed })
                .expect("input");
        }
    }
    session.step(Duration::from_millis(12)).expect("step");
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    assert_eq!(
        query(&session).as_deref(),
        Some("cal"),
        "every letter reaches the query"
    );
}

/// Clearing the query by hand does not end the typing: what is typed after the last letter is
/// deleted reaches the query as the first letters did.
#[test]
fn typing_after_deleting_the_whole_query_reaches_it_again() {
    let _settled = ui::motion::testing::settled();
    let mut session = seated();
    open(&mut session, true);
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    // Letters and deletions alike arrive a frame apart, as a person's do.
    let untyped = query(&session);
    keys(&mut session, &[46, 30, 38]);
    assert_eq!(
        query(&session).as_deref(),
        Some("cal"),
        "the first letters reach it"
    );
    keys(&mut session, &[14, 14, 14]);
    assert_eq!(query(&session), untyped, "the query is empty again");
    keys(&mut session, &[48, 24, 24]);
    assert_eq!(
        query(&session).as_deref(),
        Some("boo"),
        "typing after deleting still lands"
    );
}

/// The chord toggles: a press over the open search closes it, and the next one brings it back, as
/// the shell's own search key does.
#[test]
fn the_search_chord_closes_the_open_search_and_opens_it_again() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    chord(&mut session);
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
    chord(&mut session);
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });
    chord(&mut session);
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
}

/// Nothing typed offers what a person may want to do from an empty desk: making a workspace, and
/// joining one where this session can. Leaving this workspace, and ending the computer's session,
/// are looked for by name — and found.
#[test]
fn the_empty_search_offers_making_what_is_useful_and_never_a_way_out() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.step(Duration::from_millis(1)).expect("step");
    invoke(&mut session, DESK_RESOURCE, "search", serde_json::json!({}));
    await_search(&mut session, None);
    let titles: Vec<String> = rows(&session)
        .into_iter()
        .filter(|row| row.kind == SEARCH_ROW_KIND)
        .map(|row| row.label)
        .collect();
    assert!(
        titles.iter().any(|title| title == "New workspace"),
        "the empty search offers making a workspace: {titles:?}"
    );
    for kept_back in session
        .handle()
        .host_actions()
        .into_iter()
        .filter(|action| action.destructive)
        .map(|action| action.title)
    {
        assert!(
            !titles.contains(&kept_back),
            "the empty search offers no way out ({kept_back}): {titles:?}"
        );
    }
    // Typed, every one of them is found.
    invoke(
        &mut session,
        SEARCH_RESOURCE,
        "search",
        serde_json::json!({"query": "leave"}),
    );
    await_search(&mut session, Some("leave"));
    assert!(
        titled(&session, "Leave workspace").is_some(),
        "typing finds the way out: {:?}",
        rows(&session)
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
    );
}

/// The desk publishes its own actions on a resource named after the person and the desktop. That
/// name says whose desk it is, never what an action does, so searching for the person finds none of
/// the desk's actions.
#[test]
fn searching_for_the_person_lists_no_action_of_the_desks_own() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.step(Duration::from_millis(1)).expect("step");
    search_for(&mut session, "person");
    let titles: Vec<String> = rows(&session)
        .into_iter()
        .filter(|row| row.kind == SEARCH_ROW_KIND)
        .map(|row| row.label)
        .collect();
    for own in [
        "Clear the notification centre",
        "Leave workspace",
        "Start a call",
        "Invite people",
        "Catch up",
    ] {
        assert!(
            !titles.iter().any(|title| title.starts_with(own)),
            "the person's name is no action's name ({own} is listed): {titles:?}"
        );
    }
}

/// The empty desk's welcome verbs are declared actions, so search offers
/// the ones that bring people in or catch them up before anything is typed,
/// and typing finds all six by the name on their cards. The cards that open
/// an app are looked for by the app's own name: search already lists that
/// app, and two rows to it say less than one.
#[test]
fn the_empty_search_offers_the_desks_welcome_verbs_and_typing_finds_them_all() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.step(Duration::from_millis(1)).expect("step");
    invoke(&mut session, DESK_RESOURCE, "search", serde_json::json!({}));
    await_search(&mut session, None);
    let titles: Vec<String> = rows(&session)
        .into_iter()
        .filter(|row| row.kind == SEARCH_ROW_KIND)
        .map(|row| row.label)
        .collect();
    for offered in ["Start a call", "Invite people", "Catch up"] {
        assert!(
            titles.iter().any(|title| title == offered),
            "the empty search offers {offered}: {titles:?}"
        );
    }
    for kept_back in ["Discuss", "Notes", "Tasks"] {
        assert!(
            !titles.contains(&kept_back.to_owned()),
            "the empty search opens no app by a card's name ({kept_back}): {titles:?}"
        );
    }
    // Typed, every one of them is found.
    for (typed, found) in [
        ("discuss", "Discuss"),
        ("notes", "Notes"),
        ("tasks", "Tasks"),
        ("start a call", "Start a call"),
        ("invite people", "Invite people"),
        ("catch up", "Catch up"),
    ] {
        search_for(&mut session, typed);
        assert!(
            titled(&session, found).is_some(),
            "typing {typed:?} finds {found}: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        );
    }
}

/// A welcome card, the verb an agent reads in the catalogue and the row
/// search lists are one declaration: what the card does, the published verb
/// does. Starting a call opens Chat on the workspace and asks it to call;
/// inviting opens the workspace's settings, where people are added.
#[test]
fn a_welcome_card_does_what_its_published_action_does() {
    use tilo_sheets::Seat as _;
    use ui::window_targets::TargetAction;

    let _settled = ui::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut session = session(Rc::clone(&host));
    session.step(Duration::from_millis(1)).expect("step");

    // The verb published for agents: Meet's own card, by the resource action the catalogue lists.
    invoke(
        &mut session,
        &launchpad_card_id("meet"),
        "launchpad",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    let published = host.invoked.borrow().clone();
    let [(target, _)] = published.as_slice() else {
        panic!("one invocation, not {published:?}");
    };
    assert!(
        target.provider.ends_with(":so.tilo.chat"),
        "the verb reaches Chat: {target:?}"
    );
    assert_eq!(
        (
            target.address.resource.as_str(),
            target.address.action.as_str()
        ),
        ("chat.workspace", "start-call"),
        "Chat starts a call in the workspace: {target:?}"
    );

    // The card on the desk, pressed, asks Chat for the same thing. The empty desk and its cards are
    // the desktop surface's own: the session's door names them as the controls an assistive
    // technology presses.
    let card = session
        .controls()
        .into_iter()
        .find_map(|(name, rect)| (name == "Start a call").then_some(rect))
        .expect("the desk shows the card its verb is named on");
    assert!(
        card.2 > 0.0 && card.3 > 0.0,
        "the card stands somewhere the person can reach: {card:?}"
    );
    host.invoked.borrow_mut().clear();
    session
        .press_action("Start a call", TargetAction::Activate)
        .expect("the card is pressed");
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    assert_eq!(
        host.invoked.borrow().clone(),
        published,
        "the card runs the verb it declares"
    );

    // Inviting brings someone into this workspace: the invite sheet is open,
    // where the link is made.
    invoke(
        &mut session,
        &launchpad_card_id("invite"),
        "launchpad",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        session
            .invite_sheet()
            .is_some_and(|seat| seat.presentation().is_open())
    });
}

#[test]
fn search_opens_by_its_action_and_runs_an_app_and_a_setting_by_address() {
    let _settled = ui::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut session = session(Rc::clone(&host));
    session.step(Duration::from_millis(1)).unwrap();
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });

    invoke(&mut session, DESK_RESOURCE, "search", serde_json::json!({}));
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
    invoke(
        &mut session,
        SEARCH_RESOURCE,
        "search",
        serde_json::json!({"query": "sheets"}),
    );
    until(&mut session, |session| row(session, "Sheets").is_some());
    let app = row(&session, "Sheets").unwrap();
    invoke(
        &mut session,
        app.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    assert_eq!(
        host.invoked.borrow()[0].0,
        application_open_target(&workspace, "so.tilo.sheets"),
        "the app opens by the workspace's address for it"
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    invoke(
        &mut session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({"query": "keyboard layout"}),
    );
    until(&mut session, |session| {
        row(session, "Keyboard layout").is_some()
    });
    let setting = row(&session, "Keyboard layout").unwrap();
    invoke(
        &mut session,
        setting.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| host.invoked.borrow().len() == 2);
    let (target, arguments) = host.invoked.borrow()[1].clone();
    assert_eq!(
        target,
        application_action_target(
            &workspace,
            SETTINGS_APP,
            "settings.setting:keyboard.layout",
            "open"
        ),
        "the setting opens in Settings by its address"
    );
    assert_eq!(arguments["anchor"], "settings.setting:keyboard.layout");
}

#[test]
fn the_sessions_own_actions_are_found_and_run_on_its_published_surface() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.step(Duration::from_millis(1)).unwrap();
    let before = session.accessible_labels();
    invoke(
        &mut session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({"query": "overview"}),
    );
    until(&mut session, |session| {
        row(session, "Open or close the overview").is_some()
    });
    let toggle = row(&session, "Open or close the overview").unwrap();
    invoke(
        &mut session,
        toggle.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });
    session.step(Duration::from_millis(1)).unwrap();
    assert_ne!(
        session.accessible_labels(),
        before,
        "the overview shows over the desk"
    );
}

/// A session with the settings service, over a home of its own: its search leaves out what the
/// person switched off, as the desk reads that when it starts.
fn session_with_settings() -> Session {
    crate::common::headless(Options {
        services: vec![Box::new(tilo_settings_service::SettingsService::new())],
        search: SearchOptions {
            preferences: tilo_desktop_search::SearchPreferences::from_values(
                &settings_store::load_section(tilo_desktop_search::preferences::SECTION),
            ),
            ..SearchOptions::default()
        },
        collab: false,
        ..Options::default()
    })
}

/// Search for `query` and choose the row titled `title` : a switch stays in view, anything else
/// closes search.
fn choose(session: &mut Session, query: &str, title: &str) {
    search_for(session, query);
    let Some(row) = row(session, title) else {
        let labels: Vec<String> = rows(session).into_iter().map(|row| row.label).collect();
        panic!("no row {title:?} for {query:?} among {labels:?}");
    };
    invoke(
        session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
}

/// The field `field` of the settings section `kind` as stored.
fn stored(session: &Session, kind: &str, field: &str) -> serde_json::Value {
    session
        .read(&format!("settings:{kind}"))
        .map(|section| section[field].clone())
        .unwrap_or_default()
}

#[test]
fn a_switch_and_a_choice_are_changed_from_search_and_the_choice_is_kept() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_settings();
    session.step(Duration::from_millis(1)).unwrap();

    choose(&mut session, "high contrast", "High contrast");
    // The switch flips where it is kept.
    until(&mut session, |session| {
        stored(session, "settings.appearance", "high_contrast") == serde_json::json!(true)
    });
    assert!(
        find(&session.resources(), SEARCH_RESOURCE).is_some(),
        "a switch stays in view"
    );

    choose(&mut session, "dark mode", "Dark mode");
    until(&mut session, |session| {
        stored(session, "settings.appearance", "mode") == serde_json::json!("dark")
    });
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    // What was chosen outlasts the session.
    until(&mut session, |session| {
        stored(session, "settings.search_history", "chosen")
            .as_str()
            .is_some_and(|chosen| !chosen.is_empty())
    });
}

/// What the person switched off, as their settings keep it.
fn switched_off() -> Vec<String> {
    settings_store::load_section(tilo_desktop_search::preferences::SECTION)
        .get("disabled_providers")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The open search, as the desk publishes it.
fn open_search(session: &Session) -> Option<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
}

/// Whether the desk publishes the open search as over: no source has still to answer for the query
/// and its rows are cut to the card. The search says it is still searching in its detail until
/// then.
fn search_settled(search: &ResourceSpec) -> bool {
    search
        .presentation
        .as_ref()
        .and_then(|presentation| presentation.detail.as_deref())
        .is_none()
}

/// Step until the open search is over: it names `query` when one is given,
/// and is no longer filling in.
fn await_search(session: &mut Session, query: Option<&str>) {
    let over = common::wait_maybe(session, |session| {
        open_search(session)
            .is_some_and(|search| {
                search_settled(&search) && query.is_none_or(|query| search.label == query)
            })
            .then_some(())
    });
    if over.is_some() {
        return;
    }
    let state = open_search(session).map(|search| {
        (
            search.label,
            search
                .presentation
                .and_then(|presentation| presentation.detail),
            search
                .children
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>(),
        )
    });
    panic!(
        "the search for {query:?} is still filling in after {:?}: \
         open search (typed, searching, rows) = {state:?}",
        tilo_test_env::hang_bound()
    );
}

/// Search for `query` and wait for the search to be over: the published search names what is typed,
/// and its rows stand once every source has answered and they are cut to the card.
fn search_for(session: &mut Session, query: &str) {
    invoke(
        session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({ "query": query }),
    );
    await_search(session, Some(query));
}

/// The row whose label is exactly `title` .
fn titled(session: &Session, title: &str) -> Option<ResourceSpec> {
    rows(session)
        .into_iter()
        .find(|row| row.kind == SEARCH_ROW_KIND && row.label == title)
}

#[test]
fn a_source_switched_off_from_search_leaves_search_and_stays_off() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_settings();
    session.step(Duration::from_millis(1)).unwrap();

    // Settings' pages are one of the sources search asks: its Keyboard page is a row of its own.
    search_for(&mut session, "keyboard layout");
    assert!(titled(&session, "Keyboard").is_some());

    // What finds a source is the row that switches it off, and it stays in view so it can be
    // switched back on.
    choose(
        &mut session,
        "disable settings search",
        "Disable Settings search",
    );
    // The choice is the person's, kept with their settings.
    until(&mut session, |_| {
        switched_off()
            .iter()
            .any(|source| source == "settings-pages")
    });
    assert!(
        find(&session.resources(), SEARCH_RESOURCE).is_some(),
        "a switch stays in view"
    );
    search_for(&mut session, "keyboard layout");
    assert!(
        titled(&session, "Keyboard").is_none(),
        "what the source found is gone: {:?}",
        rows(&session)
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
    );
    drop(session);

    // A session started anew reads the choice back, as the desk does.
    let mut session = session_with_settings();
    session.step(Duration::from_millis(1)).unwrap();
    search_for(&mut session, "keyboard layout");
    assert!(
        titled(&session, "Keyboard").is_none(),
        "it stays off across a restart"
    );

    // And what was switched off is switched back on where it was: the source's own row says what it
    // is set to.
    choose(
        &mut session,
        "enable settings search",
        "Enable Settings search",
    );
    until(&mut session, |_| switched_off().is_empty());
    search_for(&mut session, "keyboard layout");
    assert!(
        titled(&session, "Keyboard").is_some(),
        "the source is back: {:?}",
        rows(&session)
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
    );
}

/// The app whose record index the record of this test is named in: any app of the workspace keeps
/// one, and search reads them all the same way.
const NOTES: &str = "so.tilo.notes";

/// The name the document a row brings here is kept under: a file-backed record, as a workspace that
/// is not this one names it.
const QUARTERLY: &str = "Quarterly plan.odt";

/// The workspaces the desk shows, by id and by the name it gives each.
fn workspaces(session: &Session) -> Vec<(String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| (workspace.id().to_string(), workspace.name.clone()))
            .collect()
    })
}

/// The id of the workspace in front.
fn workspace_in_front(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace")
}

/// Save `rows` (`(id, kind)`) as `package` 's index of `workspace` holds them — what it held before
/// is what the workspace no longer names — and publish it in one rename: the index of a workspace
/// that appears while the session runs appears as one directory, which is what the worker following
/// the device's indexes is told about.
fn write_index(
    home: &tilo_test_env::TestHome,
    package: &str,
    workspace: &str,
    rows: &[(&str, &str)],
) {
    let directory = app_state::workspace_index_directory(home.data(), package, workspace)
        .expect("an index directory");
    let staged = directory.with_file_name(format!("staging-{workspace}"));
    for old in [&staged, &directory] {
        if old.exists() {
            std::fs::remove_dir_all(old).expect("the index it replaces");
        }
    }
    std::fs::create_dir_all(&staged).expect("the index's directory");
    let mut index = automerge::AutoCommit::new();
    for (id, kind) in rows {
        let id = ui::record::RecordId::parse(id).expect("a record id");
        assert!(
            ui::record::index::reference(&mut index, &id, &ui::record::RecordKind::new(*kind)),
            "the index names {id}"
        );
    }
    std::fs::write(staged.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");
    std::fs::rename(&staged, &directory).expect("the index published");
}

/// The records `package` 's index of `workspace` names, read the way the workspace's apps read
/// them: the record set's own discovery over the index document.
fn known_in(package: &str, workspace: &str) -> Vec<String> {
    let Ok(mut store) = app_state::DocStore::open_workspace(package, workspace) else {
        return Vec::new();
    };
    let Some(index) = store.load().ok().flatten() else {
        return Vec::new();
    };
    let mut records = ui::record::RecordSet::in_memory().with_home(workspace);
    records.discover_workspace(&index);
    records
        .known_records()
        .iter()
        .map(|record| format!("{} {}", record.id.as_str(), record.kind.as_str()))
        .collect()
}

/// Wait for `done` as long as a loaded machine takes: the session writes the reference on a worker
/// of its own.
fn within(mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < tilo_test_env::hang_bound() {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

/// Whether `done` never holds while something that would make it hold is given `window` to land.
fn never(window: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < window {
        if done() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    !done()
}

/// The row titled `title` for `query` , reopening search until the session has read the device's
/// indexes again: a record another workspace holds is listed once the worker following them has
/// read that workspace's index.
fn find_row(session: &mut Session, query: &str, title: &str) -> ResourceSpec {
    let started = std::time::Instant::now();
    loop {
        invoke(
            session,
            DESK_RESOURCE,
            "search",
            serde_json::json!({ "query": query }),
        );
        for _ in 0..10 {
            session.step(Duration::from_millis(1)).expect("step");
        }
        if let Some(row) = row(session, title) {
            return row;
        }
        if started.elapsed() > tilo_test_env::hang_bound() {
            let labels: Vec<String> = rows(session).into_iter().map(|row| row.label).collect();
            panic!("no row {title:?} for {query:?} among {labels:?}");
        }
    }
}

#[test]
fn a_record_of_another_workspace_is_found_by_name_and_brought_here() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    // An app of this device already keeps an index, in a workspace the desk does not show: the
    // directories the session follows as more appear.
    write_index(
        &home,
        NOTES,
        "elsewhere",
        &[("file:/work/Receipts.odt", "document")],
    );
    let records = tilo_session::Records::of(home.data().to_path_buf());
    let host = Rc::new(Host::default());
    let mut session = session_over(Rc::clone(&host), Some(records), None);
    session.step(Duration::from_millis(1)).unwrap();

    // A second workspace, so a record can be held somewhere else than here.
    invoke(
        &mut session,
        CHROME_RESOURCE,
        "new-workspace",
        serde_json::json!({}),
    );
    until(&mut session, |session| workspaces(session).len() == 2);
    let here = workspace_in_front(&session);
    let (other_room, other_name) = workspaces(&session)
        .into_iter()
        .find(|(workspace, _)| *workspace != here)
        .expect("a workspace that is not the one in front");
    let document = home.home().join(QUARTERLY);
    std::fs::write(&document, "what the plan says").unwrap();
    let record = ui::record::RecordId::for_file(&document);
    write_index(&home, NOTES, &other_room, &[(record.as_str(), "document")]);

    // The row says which workspace holds the record.
    let row = find_row(&mut session, "quarterly", QUARTERLY);
    assert_eq!(
        row.presentation
            .and_then(|presentation| presentation.detail),
        Some(format!("in {other_name}, ~/{QUARTERLY}")),
        "the row says the record is held in a workspace that is not this one"
    );

    // Choosing it names the record in this workspace: one reference, no copy.
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });
    assert!(
        within(|| known_in(NOTES, &here).contains(&format!(
            "{} {}",
            record.as_str(),
            ui::record::RecordKind::new("document").as_str()
        ))),
        "this workspace holds the record: {:?}",
        known_in(NOTES, &here)
    );
    // A record whose identity is a path is also opened, so bringing it here and opening it are one
    // gesture; the file opens in the workspace the reference went into, by the address an app takes
    // a document by.
    assert_eq!(
        host.opened.borrow().clone(),
        vec![(
            here,
            tilo_desktop_search::providers::files::file_uri(&document)
        )],
        "the file the record is opens in this workspace"
    );
}

/// The record a row names after the workspace holding it stopped naming it.
const STALE: &str = "file:/work/Stale plan.odt";

/// The record the index names instead: search offers it only once the session has read the index
/// again, which is also when [`STALE`] stopped being held.
const FRESH: &str = "file:/work/Fresh plan.odt";

#[test]
fn a_row_for_a_record_no_workspace_holds_any_more_brings_nothing_here() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    write_index(&home, NOTES, "elsewhere", &[(STALE, "document")]);
    let records = tilo_session::Records::of(home.data().to_path_buf());
    let mut session = session_over(Rc::new(Host::default()), Some(records), None);
    session.step(Duration::from_millis(1)).unwrap();
    let here = workspace_in_front(&session);

    // The row is collected now and chosen below.
    let stale = find_row(&mut session, "stale", "Stale plan.odt");

    // The workspace holding it stops naming it, in one write that names another record: search
    // offering the record added is what says the session has read the index again.
    write_index(&home, NOTES, "elsewhere", &[(FRESH, "document")]);
    find_row(&mut session, "fresh", "Fresh plan.odt");
    invoke(
        &mut session,
        DESK_RESOURCE,
        "search",
        serde_json::json!({ "query": "stale" }),
    );
    for _ in 0..10 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert!(
        row(&session, "Stale plan.odt").is_none(),
        "a record no workspace names any more is not offered"
    );

    // The row collected before it stopped being held brings nothing here. A chosen row is carried
    // out for its effect rather than reported by an error, so what is checked is the effect: no
    // reference is written, for as long as writing one takes. The refusal itself, for a row still
    // shown by an open search, is the session's own to make and is proven in
    // `search::tests::a_row_for_a_record_no_workspace_names_any_more_is_refused` .
    let _ = session.invoke(
        stale.id.as_str(),
        "choose-search-row",
        &serde_json::Map::new(),
    );
    assert!(
        never(Duration::from_secs(1), || known_in(NOTES, &here)
            .iter()
            .any(|record| record.starts_with(STALE))),
        "no reference is written: {:?}",
        known_in(NOTES, &here)
    );
}

/// Publish one entry an app of `workspace` holds, as its record `tab:0001` filled with what it has:
/// the one item, named `title` .
fn published(index: &AppContentIndex, workspace: &str, title: &str) {
    use ui::search::index_publication::{IndexDelta, IndexEntry};

    let delta = IndexDelta {
        reset: true,
        upsert: vec![IndexEntry {
            key: "n1".into(),
            resource: "n1".into(),
            action: None,
            title: title.into(),
            subtitle: None,
            group: None,
            icon: None,
            boost: 0,
            fields: Vec::new(),
            secret: false,
            references: Vec::new(),
            ..IndexEntry::default()
        }],
        ..IndexDelta::default()
    };
    index
        .publish(
            workspace,
            NOTES,
            "tab:0001",
            &delta.encode().expect("a delta"),
        )
        .expect("the index takes the publication");
}

/// Wait until the index has kept a slot on disk.
fn kept(index_dir: &std::path::Path) -> bool {
    std::fs::read_dir(index_dir).is_ok_and(|files| {
        files.flatten().any(|file| {
            file.path()
                .extension()
                .is_some_and(|extension| extension == "json")
                && file.file_name() != "secrets.json"
        })
    })
}

#[test]
fn an_apps_published_entry_is_found_with_the_app_closed_and_after_a_restart() {
    let _settled = ui::motion::testing::settled();
    let dir = tempfile::tempdir().expect("a directory to keep the index in");
    let content = AppContentIndex::persistent(dir.path().to_path_buf());
    let host = Rc::new(Host::default());
    let mut session = session_over(Rc::clone(&host), None, Some(Arc::clone(&content)));
    session.step(Duration::from_millis(1)).unwrap();
    let workspace = workspace_in_front(&session);

    // What an app published before it closed: no app runs here, and the only actions host is the
    // fixture that runs nothing.
    published(&content, &workspace, "Groceries for the weekend");
    let row = find_row(&mut session, "groceries", "Groceries for the weekend");
    assert_eq!(
        row.presentation
            .and_then(|presentation| presentation.detail),
        Some("Notes".to_owned()),
        "the row says which app holds it"
    );

    // Choosing it asks the app to open at the entry, starting it first when it is closed.
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    let (target, arguments) = host.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(&workspace, NOTES, "n1", "open"),
        "the entry opens in the app that published it"
    );
    assert_eq!(arguments["anchor"], "n1");
    drop(session);

    // A restart reads back what the index kept on disk: the entry is found again, with nothing
    // publishing it.
    assert!(
        within(|| kept(dir.path())),
        "the index keeps the entry on disk"
    );
    let reopened = AppContentIndex::persistent(dir.path().to_path_buf());
    let mut session = session_over(Rc::new(Host::default()), None, Some(reopened));
    session.step(Duration::from_millis(1)).unwrap();
    find_row(&mut session, "groceries", "Groceries for the weekend");
}

/// A program the person added to their apps: its file at `path` , and the entry Tilo keeps for it,
/// written the way the registry writes one.
fn add_downloaded(home: &tilo_test_env::TestHome, key: &str, name: &str, path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::create_dir_all(path.parent().expect("a folder")).expect("the folder");
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("the program");
    let mut permissions = std::fs::metadata(path).expect("the program").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("a runnable program");
    let store = desktop_entry::downloaded::Store::in_data_home(home.data());
    store
        .add(
            &desktop_entry::downloaded::Identity {
                key: key.to_owned(),
                name: name.to_owned(),
                icon: None,
                comment: Some("What it says of itself".into()),
                categories: Vec::new(),
                keywords: Vec::new(),
                mime_types: Vec::new(),
                startup_wm_class: None,
                field_code: None,
            },
            &desktop_entry::downloaded::Program {
                path: path.to_path_buf(),
                kind: desktop_entry::downloaded::ProgramKind::AppImage,
            },
        )
        .expect("the entry");
}

#[test]
fn a_downloaded_program_is_found_and_starts_and_one_whose_file_is_gone_says_so() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let dropped = home.home().join("Applications/Dropped.AppImage");
    add_downloaded(&home, "Dropped", "Dropped Tool", &dropped);
    let gone = home.home().join("Downloads/Gone.AppImage");
    add_downloaded(&home, "Gone", "Gone Tool", &gone);
    // The file went away after the person added it.
    std::fs::remove_file(&gone).expect("the file went away");

    let host = Rc::new(Host::default());
    let mut session = session(Rc::clone(&host));
    session.step(Duration::from_millis(1)).unwrap();

    // The app is found by its name and starts the way every host app starts: the entry Tilo keeps
    // for it.
    let row = find_row(&mut session, "dropped", "Dropped Tool");
    assert_eq!(
        row.presentation.and_then(|shown| shown.detail),
        Some("What it says of itself".to_owned())
    );
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.started.borrow().is_empty());
    assert_eq!(
        host.started.borrow().as_slice(),
        ["tilo-downloaded-Dropped"]
    );

    // The one whose file went away is found all the same, and its row says what happened instead of
    // starting nothing.
    let row = find_row(&mut session, "gone", "Gone Tool");
    assert_eq!(
        row.presentation.and_then(|shown| shown.detail),
        Some("Can't find this app".to_owned())
    );
}

/// A host app of the machine, as its desktop entry names it, and one of the person's Tilo apps
/// beside it: what a device has, both ways.
fn host_app_lists() -> AppLists {
    AppLists {
        applications: vec![AppEntry {
            id: NOTES.into(),
            title: "Notes".into(),
            category: None,
            description: None,
        }],
        host_apps: vec![tilo_desktop_search::providers::host_apps::HostAppEntry {
            desktop_id: "Alacritty.desktop".into(),
            name: "Alacritty".into(),
            generic_name: Some("Terminal".into()),
            ..tilo_desktop_search::providers::host_apps::HostAppEntry::default()
        }],
        ..AppLists::default()
    }
}

/// Type `text` as the keyboard attached to the seat types it: each character by the key this seat's
/// layout puts it on, as `tilo-control type` does.
fn type_at_the_seat(session: &mut Session, text: &str) {
    for ch in text.chars() {
        assert!(
            session.compositor().type_char(ch).expect("input"),
            "the seat's layout types {ch:?}"
        );
        for _ in 0..2 {
            session.step(Duration::from_millis(12)).expect("step");
        }
    }
    for _ in 0..20 {
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// The palette `tilo.host:search` opens takes what the person types: the session's search, drawn
/// over everything its own tree draws, so the keys go to its field and not to the desk or a window
/// under it. What is typed is the query the desk's search runs, which the desk publishes for
/// whoever reads it (the shell, an agent) as its own open search.
#[test]
fn the_palette_the_host_search_opens_takes_the_keys() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Host::default()));
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    // The host search door, as `so.tilo.session` publishes it: what a window's search control, its
    // toolbar's magnifier and Ctrl+K run.
    invoke(&mut session, HOST_SEARCH, "open", serde_json::json!({}));
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
    type_at_the_seat(&mut session, "nothinghere");
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE)
            .is_some_and(|search| search.label == "nothinghere")
    });
    // The palette answers the query it holds: the row offering the web for it is there, which a
    // palette that never took the keys would not have.
    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("nothinghere")),
        "the opened palette's field holds what was typed: {:?}",
        session.accessible_labels()
    );
}

/// A session whose search lists the apps this device has — the machine's `Alacritty` and the
/// person's `Notes` — with `host` taking the launches.
fn host_session(host: Rc<Host>) -> Session {
    let mut session = crate::common::headless(Options {
        search: SearchOptions {
            lists: Lists::fixed(host_app_lists()),
            ..SearchOptions::default()
        },
        apps: Some(host as Rc<dyn AppActions>),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// The titles of the rows the search shows now.
fn labels(session: &Session) -> Vec<String> {
    rows(session).into_iter().map(|row| row.label).collect()
}

/// The ways a person reaches search, each of which searches the session's one list of apps.
#[derive(Clone, Copy, Debug)]
enum Opening {
    /// The platform search door (`tilo.host:search`): what a window's search control, its toolbar's
    /// magnifier and Ctrl+K run, and what an agent asks.
    HostDoor,
    /// The desk's own `search` verb, in the workspace scope its chip names.
    Desk,
    /// The applications alone, as the launchpad's "Open an app" lists them.
    Applications,
}

impl Opening {
    /// Open search this way, already asking `query` .
    fn open(self, session: &mut Session, query: &str) {
        match self {
            Self::HostDoor => {
                invoke(
                    session,
                    HOST_SEARCH,
                    "open",
                    serde_json::json!({ "query": query }),
                );
            }
            Self::Desk => {
                invoke(
                    session,
                    DESK_RESOURCE,
                    "search",
                    serde_json::json!({ "query": query }),
                );
            }
            Self::Applications => {
                session
                    .handle()
                    .open_search(ui::search::Scope::WindowGroup(None), Some(query.to_owned()));
            }
        }
        await_search(session, Some(query));
    }
}

/// Every way into search finds the person's apps by name, from one list of apps: the machine's
/// `Alacritty` and the person's `Notes` alike.
#[test]
fn the_searches_find_the_persons_apps_by_name_however_it_opens() {
    let _settled = ui::motion::testing::settled();
    for (query, title) in [("alacritty", "Alacritty"), ("notes", "Notes")] {
        let mut session = host_session(Rc::new(Host::default()));
        Opening::HostDoor.open(&mut session, query);
        assert!(
            titled(&session, title).is_some(),
            "{title} is found by name from the platform search door: {:?}",
            labels(&session)
        );
    }
    for opening in [Opening::Desk, Opening::Applications] {
        let mut session = host_session(Rc::new(Host::default()));
        opening.open(&mut session, "alacritty");
        assert!(
            titled(&session, "Alacritty").is_some(),
            "{opening:?} lists an app the person can open: {:?}",
            labels(&session)
        );
    }
}

/// Choosing the row any search found starts the app: the launch reaches the launcher, as choosing a
/// row of the desk's own search does.
#[test]
fn choosing_the_row_any_search_found_starts_the_app() {
    let _settled = ui::motion::testing::settled();
    for opening in [Opening::HostDoor, Opening::Desk, Opening::Applications] {
        let host = Rc::new(Host::default());
        let mut session = host_session(Rc::clone(&host));
        opening.open(&mut session, "alacritty");
        let app = titled(&session, "Alacritty")
            .unwrap_or_else(|| panic!("the app's row from {opening:?}: {:?}", labels(&session)));
        invoke(
            &mut session,
            app.id.as_str(),
            "choose-search-row",
            serde_json::json!({}),
        );
        until(&mut session, |_| !host.started.borrow().is_empty());
        assert_eq!(
            host.started.borrow().as_slice(),
            ["Alacritty.desktop"],
            "the app found by {opening:?} starts"
        );
    }
}

/// Tasks as it publishes its capture to the workspace: its root, with the action that takes the one
/// line a person types (`title` is that parameter).
fn tasks_capture() -> Vec<ResourceSpec> {
    let capture = tilo_actions::ActionSpec::prompted_param(
        "new_task",
        "New task",
        tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, "What to do")
            .taking_entry(),
        "Call the landlord fri 10am",
        tilo_actions::ExecutionLocus::ClientLocal,
        |text| tilo_actions::EncodedAction {
            type_id: "so.tilo.lang.tasks.new_task".into(),
            payload: text.as_bytes().to_vec(),
        },
    );
    vec![ResourceSpec::new("tasks", "tl.Tasks.tasks", "Tasks").with_action(capture)]
}

/// One line typed into the search writes a task down with no Tasks window open: the workspace's
/// capture answers the sentence as its own line, and choosing it runs the app's action with the
/// words as typed.
#[test]
fn a_line_typed_into_search_runs_the_workspace_s_capture_with_no_app_open() {
    let _settled = ui::motion::testing::settled();
    let host = Rc::new(Host::default());
    let mut session = session(Rc::clone(&host));
    session.step(Duration::from_millis(1)).expect("step");
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });
    // The workspace's action host publishes Tasks' capture; no window of the app is open to write
    // it in.
    host.publishing.borrow_mut().push(PublishedActions::of(
        "so.tilo.lang.tasks",
        &workspace,
        &tasks_capture(),
    ));

    let line = "Call the landlord fri 10am";
    search_for(&mut session, line);
    let capture = titled(&session, "New task").unwrap_or_else(|| {
        panic!(
            "the workspace's capture answers the line typed: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        )
    });
    invoke(
        &mut session,
        capture.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !host.invoked.borrow().is_empty());
    let (target, arguments) = host.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(&workspace, "so.tilo.lang.tasks", "tasks", "new_task"),
        "the capture runs through the workspace's address for the app's action"
    );
    assert_eq!(
        arguments["title"], line,
        "what was typed is the line itself, word for word"
    );
}

/// Choosing an app's row in search starts the app through the session's own action host: the
/// app is handed to the launcher, and no refusal stands on the row. A row is published as the
/// workspace's `open` of an app (`workspace:<workspace>`), which the host reads as one of its
/// two shapes — the other being an app's own action (`application:<workspace>:<app>`).
///
/// The failure this pins was silent: every app's row answered "Apps cannot be started right
/// now." and search stayed open over the query, because the workspace provider was read with
/// the application provider's parser and never matched.
#[test]
fn a_search_row_for_an_app_starts_it_through_the_sessions_own_host() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let store = home.data().join("tilo-apps");
    common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!("printf 'started\\n' >> '{}'\nhold\n", handed.display()),
    )
    .expect("a launcher");
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    let apps = Rc::new(tilo_session::mount::apps::Apps::with_launcher(
        environment,
        None,
        Some(launcher.path().to_owned()),
    ));
    let mut session = crate::common::headless(Options {
        search: SearchOptions {
            lists: Lists::fixed(AppLists {
                applications: vec![AppEntry {
                    id: "so.tilo.lang.notes".into(),
                    title: "Notebook".into(),
                    category: None,
                    description: None,
                }],
                ..AppLists::default()
            }),
            ..SearchOptions::default()
        },
        apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
        launch: Some(Rc::clone(&apps)),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");

    search_for(&mut session, "notebook");
    let row = titled(&session, "Notebook")
        .unwrap_or_else(|| panic!("the app's row: {:?}", labels(&session)));
    assert!(
        !row.presentation
            .as_ref()
            .and_then(|presentation| presentation.detail.as_deref())
            .is_some_and(|detail| detail.contains("cannot be started")),
        "the row does not say the app cannot start: {}",
        row.label
    );
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    let started = common::wait_maybe_past(&mut session, Duration::from_secs(2), |_| {
        std::fs::read_to_string(&handed)
            .is_ok_and(|said| said.contains("started"))
            .then_some(())
    })
    .is_some();
    assert!(
        started,
        "choosing the app's row starts it: the launcher was handed nothing; the row says {:?}",
        titled(&session, "Notebook").map(|row| {
            row.presentation
                .map(|presentation| presentation.detail)
                .unwrap_or_default()
        })
    );
    assert!(
        titled(&session, "Notebook").is_none_or(|row| {
            !row.presentation
                .as_ref()
                .and_then(|presentation| presentation.detail.as_deref())
                .is_some_and(|detail| detail.contains("cannot be started"))
        }),
        "no refusal stands on the row after choosing it: {:?}",
        labels(&session)
    );
}

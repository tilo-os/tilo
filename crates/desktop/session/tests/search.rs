//! The desktop's search in the headless native session, driven only through what the session
//! publishes: search opens and takes its query by the shell's `search` , shows its rows as
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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    RemoteActionTarget, application_action_target, application_open_target,
};
use tilo_actions::{ApiError, ResourceSpec};
use tilo_app_launch::SessionEnvironment;
use tilo_compositor::{InputEvent, KeyRepeat};
use tilo_session_core::desktop::verbs::{LAUNCHPAD_RESOURCE, SEARCH_RESOURCE, SEARCH_ROW_KIND, SHELL_RESOURCE, launchpad_card_id};
use tilo_desktop_search::content_index::AppContentIndex;
use tilo_desktop_search::providers::AppEntry;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_search::providers::settings::SETTINGS_APP;
use tilo_session::{AppActions, AppLists, Lists, Options, SearchOptions, Session};

/// The platform search action every window and the shell publish: what a window's search control, its
/// toolbar's magnifier and Ctrl+K run (`tilo_ui::search::SearchRequest`).
const HOST_SEARCH: &str = "tilo.host:search";

type Arguments = serde_json::Map<String, serde_json::Value>;

/// An action host that keeps what it was asked to run.
#[derive(Default)]
struct Asked {
    invoked: RefCell<Vec<(RemoteActionTarget, Arguments)>>,
    /// The uris it was asked to open, by the workspace it was asked for.
    opened: RefCell<Vec<(String, String)>>,
    /// The machine's own apps it was asked to start.
    started: RefCell<Vec<String>>,
    /// What the workspace's apps publish, as their resources reach it.
    publishing: RefCell<Vec<PublishedActions>>,
}

impl AppActions for Asked {
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

    fn invoke(&self, target: RemoteActionTarget, arguments: Arguments) -> Result<(), ApiError> {
        self.invoked.borrow_mut().push((target, arguments));
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), ApiError> {
        Ok(())
    }

    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), ApiError> {
        self.opened
            .borrow_mut()
            .push((workspace.to_owned(), uri.to_owned()));
        Ok(())
    }

    fn start_host_app(&self, desktop_id: &str) -> Result<(), ApiError> {
        self.started.borrow_mut().push(desktop_id.to_owned());
        Ok(())
    }
}

fn session(asked: Rc<Asked>) -> Session {
    session_over(asked, None, None)
}

/// A session whose search also lists the records `records` keeps current and what apps published
/// into `content` .
fn session_over(
    asked: Rc<Asked>,
    records: Option<tilo_session::Records>,
    content: Option<Arc<AppContentIndex>>,
) -> Session {
    crate::common::headless(Options {
        lists: Lists::fixed(AppLists {
            applications: vec![AppEntry {
                id: "so.tilo.sheets".into(),
                title: "Sheets".into(),
                category: None,
            }],
            ..AppLists::default()
        }),
        search: SearchOptions {
            indexes: content.map(|content| {
                std::sync::Arc::new(tilo_session::platform::indexes::SessionIndexes::new(
                    content, None,
                )) as std::sync::Arc<dyn tilo_session::platform::indexes::Indexes>
            }),
            records,
            ..SearchOptions::default()
        },
        apps: Some(asked),
        collab: false,
        ..Options::default()
    })
}

/// A session on a device that has `apps` installed, by the id and the name this device lists them
/// under: what the shell offers a card for, and what search lists as an app.
fn session_installing(asked: Rc<Asked>, apps: &[(&str, &str)]) -> Session {
    crate::common::headless(Options {
        lists: Lists::fixed(AppLists {
            applications: apps
                .iter()
                .map(|(id, title)| AppEntry {
                    id: (*id).into(),
                    title: (*title).into(),
                    category: None,
                })
                .collect(),
            ..AppLists::default()
        }),
        apps: Some(asked),
        collab: false,
        ..Options::default()
    })
}

/// Settings' pages are the session's own search source, so every desktop a session runs finds
/// them — the web page's, which offers the pages its own Settings has. "dark mode" is the Theme
/// setting on the Appearance page, and Enter shows it through the action Settings publishes for
/// a setting, told which one it is.
#[test]
fn settings_search_finds_the_appearance_theme_and_enter_opens_it() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session(Rc::clone(&asked));
    session.step(Duration::from_millis(1)).unwrap();
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("workspace")
    });
    // The tour's own query: a person typing "settings" finds where the settings are.
    search_for(&mut session, "settings");
    assert!(
        rows(&session).iter().any(|row| row.kind == SEARCH_ROW_KIND),
        "settings finds its pages: {:?}",
        rows(&session)
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
    );
    search_for(&mut session, "dark mode");
    let theme = row(&session, "Theme").unwrap_or_else(|| {
        panic!(
            "dark mode is the Appearance page's theme: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        )
    });
    invoke(
        &mut session,
        theme.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let (target, arguments) = asked.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(&workspace, SETTINGS_APP, "settings.root", "show-setting"),
        "a setting is shown by the action Settings publishes for one"
    );
    assert_eq!(arguments["setting"], "appearance.style");
}

/// A page's row runs the action the page's own row of Settings publishes, at the page's own
/// address: it asks nothing of an `open` on Settings' root resource, which Settings never
/// offers, so pressing it opens Settings where a person sees the page.
#[test]
fn a_page_found_in_search_runs_that_pages_own_action() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session(Rc::clone(&asked));
    session.step(Duration::from_millis(1)).unwrap();
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("workspace")
    });
    search_for(&mut session, "accounts");
    let page = row(&session, "Accounts").unwrap_or_else(|| {
        panic!(
            "accounts is a page of Settings: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        )
    });
    invoke(
        &mut session,
        page.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let (target, arguments) = asked.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(
            &workspace,
            SETTINGS_APP,
            "settings.section:external_accounts",
            "open-pane"
        ),
        "the page's own row shows it"
    );
    assert!(
        arguments.is_empty(),
        "the row binds its pane: {arguments:?}"
    );
}

/// Run frames until `done` holds.
fn until(session: &mut Session, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, "what the test asked for", |session| {
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

/// The open search's rows, as the shell publishes them.
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

/// Type one key of the keyboard, as the compositor takes it from the state.
fn type_key(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .expect("input");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The query the open search carries, as the shell publishes it.
fn query(session: &Session) -> Option<String> {
    find(&session.resources(), SEARCH_RESOURCE).map(|search| search.label)
}

/// A session with key repeat pushed past the test's own steps: a held key repeats, and a step of a
/// headless session can take longer than a repeat waits, so these keys are tapped instead.
fn with_slow_keys() -> Session {
    let mut session = session(Rc::new(Asked::default()));
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Open the shell's search: by its own action, or by the chord the person presses, Super+Space
/// through the state.
fn open(session: &mut Session, by_chord: bool) {
    if by_chord {
        chord(session);
    } else {
        invoke(session, SHELL_RESOURCE, "search", serde_json::json!({}));
    }
    until(session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_some()
    });
}

/// Keys typed while the shell's search is open go into its query field: the shell takes the keyboard
/// while its own search shows, rather than leaving the letters to the window behind it. Where the
/// search was opened by its action or by the chord, and whether the letters are tapped at once or
/// after the shell has settled, they all reach the same field.
#[test]
fn typing_reaches_the_searches_query_however_it_was_opened() {
    let _settled = ui::motion::testing::settled();
    for (by_chord, waited) in [(false, false), (true, false), (true, true)] {
        let mut session = with_slow_keys();
        open(&mut session, by_chord);
        if waited {
            // The field keeps the keyboard while the shell redraws under it,
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
    let mut session = with_slow_keys();
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
    let mut session = with_slow_keys();
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
    let mut session = session(Rc::new(Asked::default()));
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

/// Nothing typed offers what a person may want to do from an empty shell: making a workspace, and
/// joining one where this session can. Leaving this workspace, and ending the computer's session,
/// are looked for by name — and found.
#[test]
fn the_empty_search_offers_making_what_is_useful_and_never_a_way_out() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Asked::default()));
    session.step(Duration::from_millis(1)).expect("step");
    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({}),
    );
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

/// The shell publishes its own actions on a resource named after the person and the desktop. That
/// name says whose shell it is, never what an action does, so searching for the person finds none of
/// the shell's actions.
#[test]
fn searching_for_the_person_lists_no_action_of_the_shells_own() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Asked::default()));
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

/// The empty shell's welcome verbs are declared actions, so search offers
/// the ones that bring people in or catch them up before anything is typed,
/// and typing finds all six by the name on their cards. A card that opens an
/// app stands only where that app is installed here, and is no row of its
/// own: search lists the app under its own name, and two rows to it say less
/// than one.
#[test]
fn the_empty_search_offers_the_shells_welcome_verbs_and_typing_finds_them_all() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_installing(
        Rc::new(Asked::default()),
        &[
            ("so.tilo.sheets", "Sheets"),
            ("so.tilo.chat", "Chat"),
            ("so.tilo.notes", "Notes"),
            ("so.tilo.tasks", "Tasks"),
        ],
    );
    session.step(Duration::from_millis(1)).expect("step");
    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({}),
    );
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
    assert!(
        !titles.contains(&"Discuss".to_owned()),
        "the empty search opens no app by a card's name: {titles:?}"
    );
    // The cards that open an app add no row of their own: each app stands once, by its own name.
    for app in ["Chat", "Notes", "Tasks"] {
        assert_eq!(
            titles.iter().filter(|title| *title == app).count(),
            1,
            "the empty search lists {app} once, not again by its card's name: {titles:?}"
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

    // Starting a call is Chat's: on a shell without Chat it is no row, neither offered before
    // anything is typed nor found by its name.
    let mut without =
        session_installing(Rc::new(Asked::default()), &[("so.tilo.sheets", "Sheets")]);
    without.step(Duration::from_millis(1)).expect("step");
    invoke(
        &mut without,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({}),
    );
    await_search(&mut without, None);
    let titles: Vec<String> = rows(&without)
        .into_iter()
        .filter(|row| row.kind == SEARCH_ROW_KIND)
        .map(|row| row.label)
        .collect();
    assert!(
        !titles.iter().any(|title| title == "Start a call"),
        "a shell without Chat offers no call: {titles:?}"
    );
    search_for(&mut without, "start a call");
    assert!(
        titled(&without, "Start a call").is_none(),
        "and typing its name finds none: {:?}",
        rows(&without)
            .into_iter()
            .map(|row| row.label)
            .collect::<Vec<_>>()
    );
}

/// A welcome card, the verb an agent reads in the catalogue and the row
/// search lists are one declaration: what the card does, the published verb
/// does. Starting a call opens Chat on the workspace and asks it to call;
/// connecting an agent opens Console where agents are let in; inviting opens
/// the invite sheet, where the link is made.
#[test]
fn a_welcome_card_does_what_its_published_action_does() {
    use tilo_sheets::SheetState as _;
    use ui::window_targets::TargetAction;

    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session_installing(
        Rc::clone(&asked),
        &[("so.tilo.chat", "Chat"), ("so.tilo.console", "Console")],
    );
    session.step(Duration::from_millis(1)).expect("step");

    // The verb published for agents: Meet's own card, by the resource action the catalogue lists.
    invoke(
        &mut session,
        &launchpad_card_id("meet"),
        "launchpad",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let published = asked.invoked.borrow().clone();
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

    // Someone alone in their workspace has nobody to call, so the shell shows no call card, while
    // the verb stays published. The card it shows in its place, pressed, asks Console for what its
    // own published verb asks. The empty shell and its cards are the desktop surface's own: the
    // session's API names them as the controls an assistive technology presses.
    assert!(
        !session
            .controls()
            .into_iter()
            .any(|control| control.name == "Start a call"),
        "someone alone is not offered a call"
    );
    asked.invoked.borrow_mut().clear();
    invoke(
        &mut session,
        &launchpad_card_id("agent"),
        "launchpad",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let published = asked.invoked.borrow().clone();
    let [(target, _)] = published.as_slice() else {
        panic!("one invocation, not {published:?}");
    };
    assert_eq!(
        (
            target.address.resource.as_str(),
            target.address.action.as_str()
        ),
        ("console.effects", "view-connect"),
        "Console opens where agents are let in: {target:?}"
    );
    let card = session
        .controls()
        .into_iter()
        .find_map(|control| (control.name == "Connect an agent").then_some(control.rect))
        .expect("the shell shows the card its verb is named on");
    assert!(
        card.2 > 0.0 && card.3 > 0.0,
        "the card stands somewhere the person can reach: {card:?}"
    );
    asked.invoked.borrow_mut().clear();
    session
        .press_action("Connect an agent", TargetAction::Activate)
        .expect("the card is pressed");
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    assert_eq!(
        asked.invoked.borrow().clone(),
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
            .is_some_and(|state| state.presentation().is_open())
    });
}

/// The apps whose windows the shell holds, by the app each window is of.
fn shell_apps(session: &Session) -> Vec<String> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
            .map(String::from)
            .collect()
    })
}

/// A card that opens an app opens the app whose name it carries: pressing Tasks puts Tasks on the
/// shell, the app that holds the workspace's tasks, not another app's screen that happens to list
/// to-dos. Each card gets a shell of its own, because the launchpad stands only while no window does.
#[test]
fn an_app_card_opens_the_app_it_names() {
    for (verb, app, name) in [
        ("chat", "so.tilo.chat", "Chat"),
        ("notes", "so.tilo.notes", "Notes"),
        ("tasks", "so.tilo.tasks", "Tasks"),
    ] {
        let _settled = ui::motion::testing::settled();
        let mut session = session_installing(Rc::new(Asked::default()), &[(app, name)]);
        session.step(Duration::from_millis(1)).expect("step");
        invoke(
            &mut session,
            &launchpad_card_id(verb),
            "launchpad",
            serde_json::json!({}),
        );
        until(&mut session, |session| {
            shell_apps(session).iter().any(|shown| shown == app)
        });
        assert_eq!(
            shell_apps(&session),
            [app.to_owned()],
            "the {verb} card opens {app} and nothing else"
        );
    }
}

/// The shell offers the apps this device has and no others: with only the tilolang Tasks package
/// installed, there is no Tasks card to open the native app that is not here — the shell carries
/// Tasks because it is installed, search lists it by its name, and choosing it puts it on the shell.
/// The verb of the app that is not installed opens nothing, neither a window nor a card.
#[test]
fn the_shell_offers_the_tasks_that_are_installed_and_no_card_for_the_app_that_is_not() {
    const TASKS: &str = "so.tilo.tasks";
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session_installing(Rc::clone(&asked), &[(TASKS, "Tasks")]);
    session.step(Duration::from_millis(1)).expect("step");

    // The cards the shell draws, the rows search lists and the verbs agents read are published for
    // the apps this device has: the native Tasks is not here, so its card is not offered, and the
    // shell's own two verbs are.
    let published: Vec<String> = session
        .handle()
        .host_actions()
        .into_iter()
        .map(|action| action.id)
        .collect();
    for card in [
        "launchpad-chat",
        "launchpad-notes",
        "launchpad-tasks",
        "launchpad-meet",
    ] {
        assert!(
            !published.iter().any(|id| id == card),
            "no card for an app this device has not got, let alone {card}: {published:?}"
        );
    }
    for card in ["launchpad-invite", "launchpad-activity"] {
        assert!(
            published.iter().any(|id| id == card),
            "the shell's own verb {card} stands: {published:?}"
        );
    }
    // The shell draws the cards it publishes, and no other: the launchpad's own rows are the ones
    // the person and a script can press.
    let cards: Vec<String> = session
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == LAUNCHPAD_RESOURCE)
        .expect("the empty shell shows its launchpad")
        .children
        .into_iter()
        .map(|row| row.id.as_str().to_owned())
        .collect();
    assert!(
        !cards.contains(&launchpad_card_id("tasks")),
        "the shell offers no Tasks card while this device has no native Tasks app: {cards:?}"
    );

    // The verb itself is gone with the card: a script or an agent invoking it by the resource the
    // shell publishes is refused, and no window of an app that is not here stands on the shell.
    assert!(
        session
            .invoke(
                &launchpad_card_id("tasks"),
                "launchpad",
                &serde_json::Map::new(),
            )
            .is_err(),
        "the shell offers no verb for an app this device has not got"
    );
    session.step(Duration::from_millis(1)).expect("step");
    assert_eq!(
        shell_apps(&session),
        Vec::<String>::new(),
        "no window of an app this device has not got"
    );

    // Tasks is here, under the name of the package that provides it: search lists it, and choosing
    // it puts Tasks on the shell.
    search_for(&mut session, "tasks");
    let tasks = row(&session, "Tasks").unwrap_or_else(|| {
        panic!(
            "the shell lists the Tasks it has: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        tasks.label, "Tasks",
        "the app names itself as the person reads it"
    );
    // Choosing it opens that app: the workspace's own address for the package that has it.
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });
    invoke(
        &mut session,
        tasks.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    assert_eq!(
        asked.invoked.borrow()[0].0,
        application_open_target(&workspace, TASKS),
        "the Tasks this device has opens, and nothing of an app it has not"
    );
}

#[test]
fn search_opens_by_its_action_and_runs_an_app_and_a_setting_by_address() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = session(Rc::clone(&asked));
    session.step(Duration::from_millis(1)).unwrap();
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });

    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({}),
    );
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
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    assert_eq!(
        asked.invoked.borrow()[0].0,
        application_open_target(&workspace, "so.tilo.sheets"),
        "the app opens by the workspace's address for it"
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    invoke(
        &mut session,
        SHELL_RESOURCE,
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
    until(&mut session, |_| asked.invoked.borrow().len() == 2);
    let (target, arguments) = asked.invoked.borrow()[1].clone();
    assert_eq!(
        target,
        application_action_target(&workspace, SETTINGS_APP, "settings.root", "show-setting"),
        "the setting is shown by the action Settings publishes for one"
    );
    assert_eq!(arguments["setting"], "keyboard.layout");
}

#[test]
fn the_sessions_own_actions_are_found_and_run_on_its_published_surface() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Asked::default()));
    session.step(Duration::from_millis(1)).unwrap();
    let before = session.accessible_labels();
    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({"query": "overview"}),
    );
    // The chrome names the action "Overview"; what it does is the row's line under that name.
    until(&mut session, |session| row(session, "Overview").is_some());
    let toggle = row(&session, "Overview").unwrap();
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
        "the overview shows over the shell"
    );
}

/// A session with the settings service, over a home of its own: its search leaves out what the
/// person switched off, as the shell reads that when it starts.
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

/// The open search, as the shell publishes it.
fn open_search(session: &Session) -> Option<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
}

/// Whether the shell publishes the open search as over: the rows it shows are the search's answer
/// for the query, cut to the card. A source that answers after the frame that typed still owes
/// rows then ([`tilo_session_core::desktop::state::DesktopHandle::search_expecting`]), so this says the search
/// has stopped filling in, not that every source has answered.
fn search_settled(search: &ResourceSpec) -> bool {
    search
        .presentation
        .as_ref()
        .and_then(|presentation| presentation.detail.as_deref())
        .is_none()
}

/// Step until the open search is over: it names `query` when one is given, every source that owes
/// a row has answered, and it is no longer filling in.
///
/// The shell shows each source's rows when it answers, so a search that has rows is not yet a
/// search that has *every* row: the first frame holds the sources that answer on the frame, and
/// the rest arrive from the workers after it. What waits here waits for the rows still expected,
/// not only for the rows already shown.
fn await_search(session: &mut Session, query: Option<&str>) {
    let over = crate::common::wait_maybe(session, |session| {
        if session.handle().search_expecting() {
            return None;
        }
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
        SHELL_RESOURCE,
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

    // A session started anew reads the choice back, as the shell does.
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

const NOTES: &str = "so.tilo.notes";

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

/// The row titled `title` for `query` , reopening search until the session has read the device's
/// indexes again: a record another workspace holds is listed once the worker following them has
/// read that workspace's index.
fn find_row(session: &mut Session, query: &str, title: &str) -> ResourceSpec {
    let started = std::time::Instant::now();
    loop {
        invoke(
            session,
            SHELL_RESOURCE,
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
    let asked = Rc::new(Asked::default());
    let mut session = session_over(Rc::clone(&asked), None, Some(Arc::clone(&content)));
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
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let (target, arguments) = asked.invoked.borrow()[0].clone();
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
    let mut session = session_over(Rc::new(Asked::default()), None, Some(reopened));
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

    let asked = Rc::new(Asked::default());
    let mut session = session(Rc::clone(&asked));
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
    until(&mut session, |_| !asked.started.borrow().is_empty());
    assert_eq!(
        asked.started.borrow().as_slice(),
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

/// Type `text` as the keyboard attached to the state types it: each character by the key this state's
/// layout puts it on, as `tilo-control type` does.
fn type_at_the_seat(session: &mut Session, text: &str) {
    for ch in text.chars() {
        assert!(
            session.compositor().type_char(ch).expect("input"),
            "the state's layout types {ch:?}"
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
/// over everything its own tree draws, so the keys go to its field and not to the shell or a window
/// under it. What is typed is the query the shell's search runs, which the shell publishes for
/// whoever reads it (the shell, an agent) as its own open search.
#[test]
fn the_palette_the_host_search_opens_takes_the_keys() {
    let _settled = ui::motion::testing::settled();
    let mut session = session(Rc::new(Asked::default()));
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    // The platform search action, as `so.tilo.session` publishes it: what a window's search control, its
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
fn host_session(asked: Rc<Asked>) -> Session {
    let mut session = crate::common::headless(Options {
        lists: Lists::fixed(host_app_lists()),
        apps: Some(asked as Rc<dyn AppActions>),
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
    /// The platform search action (`tilo.host:search`): what a window's search control, its toolbar's
    /// magnifier and Ctrl+K run, and what an agent asks.
    Asked,
    /// The shell's own `search` verb, in the workspace scope its chip names.
    Shell,
    /// The applications alone, as the launchpad's "Open an app" lists them.
    Applications,
}

impl Opening {
    /// Open search this way, already asking `query` .
    fn open(self, session: &mut Session, query: &str) {
        match self {
            Self::Asked => {
                invoke(
                    session,
                    HOST_SEARCH,
                    "open",
                    serde_json::json!({ "query": query }),
                );
            }
            Self::Shell => {
                invoke(
                    session,
                    SHELL_RESOURCE,
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
        let mut session = host_session(Rc::new(Asked::default()));
        Opening::Asked.open(&mut session, query);
        assert!(
            titled(&session, title).is_some(),
            "{title} is found by name from the platform search action: {:?}",
            labels(&session)
        );
    }
    for opening in [Opening::Shell, Opening::Applications] {
        let mut session = host_session(Rc::new(Asked::default()));
        opening.open(&mut session, "alacritty");
        assert!(
            titled(&session, "Alacritty").is_some(),
            "{opening:?} lists an app the person can open: {:?}",
            labels(&session)
        );
    }
}

/// Choosing the row any search found starts the app: the launch reaches the launcher, as choosing a
/// row of the shell's own search does.
#[test]
fn choosing_the_row_any_search_found_starts_the_app() {
    let _settled = ui::motion::testing::settled();
    for opening in [Opening::Asked, Opening::Shell, Opening::Applications] {
        let asked = Rc::new(Asked::default());
        let mut session = host_session(Rc::clone(&asked));
        opening.open(&mut session, "alacritty");
        let app = titled(&session, "Alacritty")
            .unwrap_or_else(|| panic!("the app's row from {opening:?}: {:?}", labels(&session)));
        invoke(
            &mut session,
            app.id.as_str(),
            "choose-search-row",
            serde_json::json!({}),
        );
        until(&mut session, |_| !asked.started.borrow().is_empty());
        assert_eq!(
            asked.started.borrow().as_slice(),
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
            type_id: "so.tilo.tasks.new_task".into(),
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
    let asked = Rc::new(Asked::default());
    let mut session = session(Rc::clone(&asked));
    session.step(Duration::from_millis(1)).expect("step");
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });
    // The workspace's action host publishes Tasks' capture; no window of the app is open to write
    // it in.
    asked.publishing.borrow_mut().push(PublishedActions::of(
        "so.tilo.tasks",
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
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let (target, arguments) = asked.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(&workspace, "so.tilo.tasks", "tasks", "new_task"),
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
    crate::common::install_apps_in(&store, &home.state().join("tilolang"));
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let handed = home.home().join("handed");
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!("printf 'started\\n' >> '{}'\nhold\n", handed.display()),
    )
    .expect("a launcher");
    let mut environment = SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    let apps = Rc::new(tilo_session::apps::Apps::with_launcher(
        environment,
        None,
        Some(launcher.path().to_owned()),
    ));
    let mut session = crate::common::headless(Options {
        lists: Lists::fixed(AppLists {
            applications: vec![AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notebook".into(),
                category: None,
            }],
            ..AppLists::default()
        }),
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
    let started = crate::common::wait_maybe_past(&mut session, Duration::from_secs(2), |_| {
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

/// A session with the apps a development session seeds, which keep Chat's records: the
/// conversations and the agents a workspace keeps. Its shell's search reads the agents each
/// workspace keeps, which the pass hands it before a search opens.
fn session_with_agents() -> Session {
    let mut session = crate::common::headless(Options {
        person: crate::common::PERSON.to_owned(),
        collab: false,
        services: crate::common::seeded_apps(),
        ..Options::default()
    });
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    session
}

/// A request typed into search is the person's send of those words to the workspace's agent — one
/// where they keep none yet, named as the workspace names its agent — through the session's one
/// send (`session:send-to#send`), and search closes behind the row.
///
/// The agent the first request made is the one every later request goes to: the workspace's record
/// names it as the person's, so asking again continues that conversation rather than making a
/// second agent. The second request is the one after the first in the same session, as it is for
/// the person.
#[test]
fn the_first_request_typed_into_search_makes_the_workspaces_agent_and_the_second_goes_to_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_agents();

    // The first request: the person keeps no agent yet.
    let request = "book a table for two";
    let row = find_row(
        &mut session,
        request,
        "Send to Assistant: book a table for two",
    );
    assert!(
        row.presentation.is_none(),
        "the row says who takes the request, and nothing else"
    );
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    let first = crate::common::sent(&mut session, 0);
    assert_eq!(first["text"], request, "{first}");
    assert_eq!(
        first["to"],
        serde_json::json!([{"kind": "new-agent", "name": "Assistant"}]),
        "the person kept no agent yet: {first}"
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    // The agent the send made is the workspace's to keep once the pass has read its record, as a
    // person's next search is opened long after.
    for _ in 0..10 {
        session.step(Duration::from_millis(16)).expect("step");
    }

    // The second request: the agent the first one made is kept, and asked.
    let row = find_row(&mut session, "and move it to Friday", "Send to Assistant");
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    let second = crate::common::sent(&mut session, 1);
    assert_eq!(second["text"], "and move it to Friday", "{second}");
    let asked = second["to"].as_array().expect("whom the second went to");
    assert_eq!(asked.len(), 1, "one agent is asked again: {second}");
    let to = &second["to"][0];
    assert_eq!(to["kind"], "agent", "the agent kept now: {second}");
    assert_eq!(to["name"], "Assistant", "{second}");
    assert!(
        to["id"].as_str().is_some_and(|id| !id.is_empty()),
        "named by its id: {second}"
    );
    until(&mut session, |session| {
        find(&session.resources(), SEARCH_RESOURCE).is_none()
    });

    // Two requests are two sends, each carried out once, and the second leaves the first as it was
    // sent: the agent the first made is named in the second, not written back over the first.
    let reading = session
        .read("session:send-to")
        .expect("the sends' own reading");
    let sends = reading["sent"].as_array().expect("the sends carried out");
    assert_eq!(sends.len(), 2, "two requests are two sends: {reading}");
    assert_eq!(sends[0], first, "the first send reads as it was sent");
    assert_eq!(sends[1], second, "the second is the last carried out");
}

/// Step the session until `done`, naming the wait that ran out: a test of one gesture says which
/// step of it the shell did not answer.
fn stepped(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    let started = std::time::Instant::now();
    loop {
        if done(session) {
            return;
        }
        assert!(started.elapsed() < tilo_test_env::hang_bound(), "{what}");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// The search open, as the shell publishes it.
fn search_showing(session: &Session) -> bool {
    find(&session.resources(), SEARCH_RESOURCE).is_some()
}

/// An app's own row leads a record its window shows, for the app's name: a person who types
/// "sheets" gets Sheets, and the workbook the sheet window holds comes after it.
#[test]
fn an_apps_own_row_leads_a_record_of_the_app_it_names() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    // The records as a host reads them from the store they are kept in, told what moved in it.
    let changes = tilo_session::platform::records::Changes::default();
    let records = tilo_session::Records::over(
        std::sync::Arc::new(app_state::FsStore::new(home.data())),
        changes.clone(),
        true,
    );
    let mut session = session_over(Rc::new(Asked::default()), Some(records), None);
    session.step(Duration::from_millis(1)).unwrap();
    // The workbook the Sheets window shows, as its app's index of this workspace names it.
    let here = workspace_in_front(&session);
    let workbook = ui::resource::DocumentId::parse("uuid:6d1c1f36-0000-4000-8000-0000000000ab")
        .expect("a record id");
    let directory = app_state::workspace_index_directory(home.data(), "so.tilo.sheets", &here)
        .expect("an index directory");
    std::fs::create_dir_all(&directory).expect("the index's directory");
    let mut index = automerge::AutoCommit::new();
    assert!(ui::resource::index::reference(
        &mut index,
        &workbook,
        &ui::resource::ResourceType::new("sheets.workbook")
    ));
    assert!(ui::resource::index::set_name(
        &mut index,
        &workbook,
        Some("Sheet1")
    ));
    std::fs::write(directory.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");
    changes.note(&here, "so.tilo.sheets", "", false);

    find_row(&mut session, "sheets", "Sheet1");
    search_for(&mut session, "sheets");
    let labels: Vec<String> = rows(&session).into_iter().map(|row| row.label).collect();
    assert_eq!(
        labels.first().map(String::as_str),
        Some("Sheets"),
        "the app's row leads for its own name: {labels:?}"
    );
}

/// The search chord opens the search with a window in front: the press is the desktop's wherever it
/// is pressed, never the window's under it, and it is the window's keyboard the chord crosses.
#[test]
fn the_search_chord_opens_the_search_with_a_window_in_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_installing(Rc::new(Asked::default()), &[("so.tilo.notes", "Notes")]);
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session.step(Duration::from_millis(1)).expect("step");
    invoke(
        &mut session,
        &launchpad_card_id("notes"),
        "launchpad",
        serde_json::json!({}),
    );
    stepped(&mut session, "the window opens on the shell", |session| {
        shell_apps(session).iter().any(|app| app == "so.tilo.notes")
    });

    chord(&mut session);
    stepped(
        &mut session,
        "the chord opens the search with a window in front",
        search_showing,
    );
}

/// Calendar installed and never opened here, offering what its manifest says it does while closed.
fn closed_calendar() -> Lists {
    let offer = tilo_app_package::manifest::OfferedAction {
        resource: "calendar.calendar".into(),
        resource_kind: None,
        action: "show-tasks".into(),
        label: "Show tasks".into(),
        description: None,
        kind: tilo_app_package::manifest::OfferKind::Read,
        operation: tilo_app_package::manifest::OfferOperation::View,
        confirm: false,
        asks: None,
        keywords: vec!["to-do list".into()],
        icon: None,
    };
    Lists::fixed(AppLists {
        launchable: vec![tilo_broker_core::services::LaunchableApplication {
            package_id: "so.tilo.calendar".into(),
            display_name: "Calendar".into(),
            unconfined: false,
            offers: vec![offer],
            capabilities: Vec::new(),
            surface: None,
        }],
        ..AppLists::default()
    })
}

/// An app installed here that never ran is found by what it offers: "to-do list" finds Calendar's
/// Show tasks with Calendar closed and nothing it published, and choosing it asks the shell to
/// open Calendar and carry it out there. Once the app publishes its own
/// actions, the row is the one it publishes, never both.
#[test]
fn search_finds_what_an_installed_app_offers_before_it_ever_ran() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = crate::common::headless(Options {
        lists: closed_calendar(),
        apps: Some(Rc::clone(&asked) as Rc<dyn AppActions>),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    let workspace = session.handle().read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
            .expect("a workspace")
    });

    search_for(&mut session, "to-do list");
    let row = titled(&session, "Show tasks").unwrap_or_else(|| {
        panic!(
            "the closed app's offer answers its words: {:?}",
            rows(&session)
                .into_iter()
                .map(|row| row.label)
                .collect::<Vec<_>>()
        )
    });
    invoke(
        &mut session,
        row.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    let (target, _) = asked.invoked.borrow()[0].clone();
    assert_eq!(
        target,
        application_action_target(
            &workspace,
            "so.tilo.calendar",
            "calendar.calendar",
            "show-tasks"
        ),
        "the row invokes Calendar's own action by its address"
    );

    // Calendar opened and published its actions: those are what search offers of it now.
    asked.publishing.borrow_mut().push(PublishedActions::of(
        "so.tilo.calendar",
        &workspace,
        &[
            ResourceSpec::new("calendar.calendar", "calendar.calendar", "Calendar").with_action(
                tilo_actions::ActionSpec::immediate_encoded(
                    "show-tasks",
                    "Show tasks",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    tilo_actions::EncodedAction {
                        type_id: "calendar.show-tasks".into(),
                        payload: Vec::new(),
                    },
                )
                .with_keywords(["to-do list"]),
            ),
        ],
    ));
    search_for(&mut session, "show tasks");
    let found = rows(&session)
        .into_iter()
        .filter(|row| row.label == "Show tasks")
        .count();
    assert_eq!(found, 1, "what the app published stands in for its offer");
}

/// Connecting an agent is one thing to find: typing it lists one row, however many ways the shell
/// offers it (the empty shell's card, the person's own card, Console's place).
#[test]
fn connecting_an_agent_is_one_row() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_installing(
        Rc::new(Asked::default()),
        &[
            ("so.tilo.console", "Console"),
            ("so.tilo.notes", "Notes"),
            ("so.tilo.chat", "Chat"),
        ],
    );
    session.step(Duration::from_millis(1)).expect("step");
    search_for(&mut session, "connect");
    let found: Vec<String> = rows(&session)
        .into_iter()
        .filter(|row| row.kind == SEARCH_ROW_KIND)
        .map(|row| row.label)
        .collect();
    assert_eq!(
        found
            .iter()
            .filter(|label| label.to_lowercase().contains("connect an agent"))
            .count(),
        1,
        "one row connects an agent: {found:?}"
    );
}

/// The row search put first acts on the first Enter once the search is over: "show tasks" finds
/// Calendar's Show tasks, which nobody moved to, and Enter invokes it.
#[test]
fn enter_on_the_row_search_put_first_acts_at_once() {
    let _settled = ui::motion::testing::settled();
    let asked = Rc::new(Asked::default());
    let mut session = crate::common::headless(Options {
        lists: closed_calendar(),
        apps: Some(Rc::clone(&asked) as Rc<dyn AppActions>),
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("step");
    search_for(&mut session, "show tasks");
    assert!(titled(&session, "Show tasks").is_some());
    crate::common::key(&mut session, crate::common::KEY_ENTER);
    until(&mut session, |_| !asked.invoked.borrow().is_empty());
    assert_eq!(asked.invoked.borrow()[0].0.address.action, "show-tasks");
}

/// With the shell's search card over the windows, the compositor knows the person types into its
/// field: the input method serves it, a caret is published where it stands, and a touch on the
/// glass asks for the on-screen keyboard as it does over any field.
#[test]
fn the_searches_field_takes_typing_and_asks_for_the_glass_keyboard() {
    let _settled = ui::motion::testing::settled();
    let mut session = with_slow_keys();
    open(&mut session, true);
    await_search(&mut session, None);
    let entry = session.compositor().text_entry();
    assert!(entry.wants_typing, "the search's own field takes typing");
    assert!(
        session.compositor().text_caret().is_some(),
        "the caret the search draws is where the person types"
    );
    // A key first, then a touch on the caret the field draws: the keyboard belongs over the
    // field now, and the touch stays in the card rather than dismissing it.
    type_key(&mut session, 46);
    let caret = session
        .compositor()
        .text_caret()
        .expect("the search field's caret");
    session
        .compositor()
        .input(InputEvent::TouchDown {
            slot: 0,
            x: caret.x + caret.width / 2.0,
            y: caret.y + caret.height / 2.0,
        })
        .expect("touch");
    session.step(Duration::from_millis(16)).expect("step");
    assert!(
        find(&session.resources(), SEARCH_RESOURCE).is_some(),
        "the touch on the field keeps the search up"
    );
    let entry = session.compositor().text_entry();
    assert!(entry.wants_typing, "the search's field still takes typing");
    assert!(entry.on_the_glass, "the last typing was on the glass");
    assert!(entry.touch_available, "the glass can be typed on again");
}

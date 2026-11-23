//! The workspace's activity as a desktop hosting its windows in process serves it to the Activity
//! app: through the session the windows ask, as the app asks a native session's broker.

use std::sync::Arc;

use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_actions::{InvocationUpdate, InvocationUpdate::Item};
use tilo_desktop_model::WorkspaceId;
use ui::automerge::AutoCommit;
use ui::{Arrival, RemoteActions};

use super::*;
use tilo_session_core::desktop::navigation::ChromeEffect;
use ui::widget::TileContent;

const WORKSPACE: &str = "workspace-a";

/// Write the shell as it stands into `doc` and commit it at `time` , as a page records each pass:
/// one change of the shell, by its person.
fn stamp(doc: &mut AutoCommit, tree: &Layout, time: i64) {
    tilo_desktop_model::desktop_doc::seed(doc, tree);
    doc.commit_with(
        automerge::transaction::CommitOptions::default()
            .with_message(tilo_desktop_model::shared_resource::change_message(Some(
                "Ada",
            )))
            .with_time(time),
    );
}

struct Activity;

impl TileSource for Activity {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
        (app == wire::APP_ID).then(|| Tab {
            id,
            title: "Activity".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.into(),
                label: "Activity".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }
}

/// The layout's own document, as a browser page keeps it.
struct RetainedHistory {
    document: Rc<std::cell::RefCell<AutoCommit>>,
}

impl HistorySource for RetainedHistory {
    fn workspace_history(
        &mut self,
        tree: &Layout,
        workspace: WorkspaceId,
    ) -> Option<tilo_session_core::desktop::activity::ActivitySnapshot> {
        let mut desktop = tree.clone();
        desktop.active = workspace;
        Some(
            tilo_session_core::desktop::activity::ActivitySnapshot::of_desktop(
                desktop,
                self.document.borrow().clone(),
            ),
        )
    }

    fn record_histories(
        &mut self,
        _: &Layout,
        _: WorkspaceId,
    ) -> Vec<tilo_session_core::desktop::activity::DetachedRecordHistory> {
        Vec::new()
    }

    fn host(&self) -> Rc<dyn tilo_session_core::desktop::activity::ActivityHost> {
        Rc::new(tilo_session_core::desktop::activity::UnavailableActivityHost)
    }

    fn revision(&self) -> u64 {
        use automerge::transaction::Transactable as _;
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.document.borrow().base_heads().hash(&mut hasher);
        hasher.finish()
    }
}

/// A host that fetches the model's code when it is first needed, as the browser desktop does.
struct DeferredHistory {
    retained: RetainedHistory,
    code: Rc<std::cell::Cell<ActivityPaneCode>>,
}

impl HistorySource for DeferredHistory {
    fn workspace_history(
        &mut self,
        tree: &Layout,
        workspace: WorkspaceId,
    ) -> Option<tilo_session_core::desktop::activity::ActivitySnapshot> {
        self.retained.workspace_history(tree, workspace)
    }

    fn record_histories(
        &mut self,
        tree: &Layout,
        workspace: WorkspaceId,
    ) -> Vec<tilo_session_core::desktop::activity::DetachedRecordHistory> {
        self.retained.record_histories(tree, workspace)
    }

    fn host(&self) -> Rc<dyn tilo_session_core::desktop::activity::ActivityHost> {
        self.retained.host()
    }

    fn activity_pane(&self) -> ActivityPaneCode {
        self.code.get()
    }
}

/// A desktop of one window whose history holds its setup and a rename,
/// serving its activity to the windows through `session`.
struct Served {
    surface: DesktopSurface,
    handle: DesktopHandle,
    session: Arc<crate::session::DesktopSession>,
    document: Rc<std::cell::RefCell<AutoCommit>>,
    tree: Layout,
}

impl Served {
    fn new(code: Option<Rc<std::cell::Cell<ActivityPaneCode>>>) -> Self {
        let mut minter = IdMinter::new(7);
        let tab = Tab {
            id: minter.mint(),
            title: "Notes".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Office", vec![tab]);
        let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Activity));
        let mut tree = handle.read(Clone::clone);
        let mut retained = AutoCommit::new();
        stamp(&mut retained, &tree, 1_000);
        tree.workspaces[0].strips[0].name = "Renamed".into();
        stamp(&mut retained, &tree, 2_000);
        let document = Rc::new(std::cell::RefCell::new(retained));
        let retained = RetainedHistory {
            document: Rc::clone(&document),
        };
        match code {
            Some(code) => surface.set_history_source(Box::new(DeferredHistory { retained, code })),
            None => surface.set_history_source(Box::new(retained)),
        }
        let (session, openings) = crate::session::serve(WORKSPACE, Arc::default());
        surface.serve_activity(openings.activity());
        Self {
            surface,
            handle,
            session,
            document,
            tree,
        }
    }

    /// The windows' action API of the session, as `caller` .
    fn api(&self, caller: Caller) -> RemoteActions {
        RemoteActions::with_provider(self.session.clone(), caller.in_workspace(WORKSPACE))
    }

    /// One pass of the desktop: what the windows asked is answered.
    fn frame(&mut self) {
        self.surface
            .update(&AutoCommit::new(), &ui::automerge::ROOT);
    }

    /// Someone renames the strip: the history moves.
    fn rename(&mut self, name: &str, at_ms: i64) {
        self.tree.workspaces[0].strips[0].name = name.into();
        stamp(&mut self.document.borrow_mut(), &self.tree, at_ms);
    }

    fn activity_windows(&self) -> usize {
        self.handle
            .snapshot("g", vec![])
            .entries
            .iter()
            .filter(|entry| entry.app.as_deref() == Some(wire::APP_ID))
            .count()
    }

    fn workspace(&self) -> String {
        self.tree.active.to_string()
    }
}

fn person() -> Caller {
    Caller::local_person("ada")
}

/// The latest result `followed` was sent, and its sequence.
fn latest<T: serde::de::DeserializeOwned>(followed: &ui::Followed) -> Option<(u64, T)> {
    followed.call().with(|arrival| match arrival {
        Arrival::Ready(Item {
            sequence, value, ..
        }) => Some((*sequence, serde_json::from_value(value.clone()).unwrap())),
        _ => None,
    })
}

/// Why `call` was refused, once it was.
fn refusal(call: &ui::InvocationCall) -> Option<String> {
    call.with(|arrival| match arrival {
        Arrival::Failed(ActionCallError::Refused { reason }) => Some(reason.clone()),
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(error.message.clone()),
        _ => None,
    })
}

fn succeeded(call: &ui::InvocationCall) -> bool {
    call.with(|arrival| matches!(arrival, Arrival::Ready(InvocationUpdate::Succeeded { .. })))
}

/// The app follows the history: it is read once the desktop next serves,
/// and again only when the history moved, each time as the next result.
#[test]
fn the_app_reads_the_retained_history_and_follows_new_changes() {
    let mut shell = Served::new(None);
    let api = shell.api(person());
    let feed = api.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    assert!(
        latest::<wire::Feed>(&feed).is_none(),
        "read on the next pass"
    );
    shell.frame();
    let (first, read) = latest::<wire::Feed>(&feed).expect("the history is read");
    // The setup is no change to show; the rename is.
    assert_eq!(read.entries.len(), 1);
    assert!(!read.mutations, "a page's history is read only");

    shell.frame();
    shell.frame();
    assert_eq!(
        latest::<wire::Feed>(&feed).map(|(sequence, _)| sequence),
        Some(first),
        "nothing moved, nothing is sent"
    );

    shell.rename("Renamed again", 3_000);
    shell.frame();
    let (next, read) = latest::<wire::Feed>(&feed).unwrap();
    assert!(next > first);
    assert_eq!(read.entries.len(), 2);

    // Closing the app stops following: nothing is open on the desktop.
    drop(feed);
    shell.frame();
    assert!(shell.surface.activity.is_none());
}

/// A tool that asks for the history and waits for it hears it as soon as the desktop has read it,
/// not when its wait runs out: a followed read never ends, so its first whole answer is what a
/// waiting caller is answered with.
#[test]
fn a_waiting_read_is_answered_once_the_history_is_read() {
    use tilo_actions::remote::{InvokeRequest, RemoteActionMessage, serve};
    let mut shell = Served::new(None);
    let caller = person().in_workspace(WORKSPACE);
    let invocation = tilo_actions::InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target: wire::target(wire::FEED),
        argument: wire::argument(&wire::FeedQuery::default()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    serve(
        &shell.session,
        &caller,
        RemoteActionMessage::Invoke(request),
    )
    .expect("an invoke is answered")
    .wait_timeout(Duration::from_secs(5))
    .ok()
    .flatten()
    .expect("the read is admitted");
    let observe = RemoteActionMessage::Observe {
        correlation: 1,
        invocation,
        seen: None,
        wait_ms: Some(10_000),
    };
    let waiting = serve(&shell.session, &caller, observe).expect("an observe is answered");
    let started = std::time::Instant::now();
    shell.frame();
    let answer = waiting
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("answered with the history, not at the end of its ten-second wait");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let RemoteActionMessage::Update {
        state: Some(state), ..
    } = answer
    else {
        panic!("an observe is answered with the read's state: {answer:?}");
    };
    let Item { value, .. } = state.update else {
        panic!("the read's answer is its item: {:?}", state.update);
    };
    let feed: wire::Feed = serde_json::from_value(value).unwrap();
    assert_eq!(feed.entries.len(), 1, "the rename, as the app reads it");
}

/// Asked for from outside the app (the bar's History, a person's changes,
/// a command), Activity opens as a window, or comes forward, and the app
/// is offered the view; closing it closes its window.
#[test]
fn asking_for_activity_opens_the_app_at_the_view_asked_for() {
    let mut shell = Served::new(None);
    let workspace = shell.workspace();
    let open = |view, author: Option<&str>| ChromeEffect::Activity {
        workspace: workspace.clone(),
        view,
        author: author.map(str::to_owned),
    };
    let replay = open(navigation::ActivityView::Replay, None);
    shell.handle.dispatch(replay).unwrap();
    shell.frame();
    assert_eq!(shell.activity_windows(), 1);

    let api = shell.api(person());
    let focus = api.follow(wire::target(wire::FOCUS), wire::argument(&()));
    shell.frame();
    let (_, offered) = latest::<Option<wire::Focus>>(&focus).unwrap();
    let offered = offered.expect("the view is offered");
    assert_eq!(offered.view, wire::View::Replay);

    // The app showed it: it is not offered again.
    let shown = api.invoke(
        wire::target(wire::FOCUSED),
        wire::argument(&wire::Focused {
            serial: offered.serial,
        }),
        None,
    );
    shell.frame();
    assert!(succeeded(&shown));
    assert_eq!(latest::<Option<wire::Focus>>(&focus).unwrap().1, None);

    // One person's changes: the window there already comes forward.
    let theirs = open(navigation::ActivityView::CatchUp, Some("Ben"));
    shell.handle.dispatch(theirs).unwrap();
    shell.frame();
    assert_eq!(shell.activity_windows(), 1);
    let (_, offered) = latest::<Option<wire::Focus>>(&focus).unwrap();
    let offered = offered.unwrap();
    assert_eq!(offered.author.as_deref(), Some("Ben"));
    assert_eq!(offered.view, wire::View::History);

    let close = ChromeEffect::CloseActivity { workspace };
    shell.handle.dispatch(close).unwrap();
    shell.frame();
    assert_eq!(shell.activity_windows(), 0);
}

/// What this device has seen is the person's own, and so is every workspace's history but the one
/// an agent works in; the history itself a page only shows.
#[test]
fn what_is_seen_is_the_persons_own_and_the_history_is_read_only() {
    let mut shell = Served::new(None);
    let agent = shell.api(Caller::local_agent("helper"));
    let viewed = wire::argument(&wire::Viewed {
        entries: Vec::new(),
        later: false,
    });
    let marked = agent.invoke(wire::target(wire::MARK_VIEWED), viewed.clone(), None);
    let everywhere = agent.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery {
            scope: wire::Scope::AllWorkspaces,
            ..wire::FeedQuery::default()
        }),
    );
    let here = agent.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    shell.frame();
    let personal = tilo_session_core::desktop::i18n::t("activity-served-personal-only");
    assert_eq!(refusal(&marked), Some(personal.clone()));
    assert_eq!(refusal(everywhere.call()), Some(personal));
    assert!(
        latest::<wire::Feed>(&here).is_some(),
        "an agent reads the workspace it works in"
    );

    let person = shell.api(person());
    let marked = person.invoke(wire::target(wire::MARK_VIEWED), viewed, None);
    let range = wire::Range::one(&wire::entry_hex(&[0; 16]));
    let undone = person.invoke(wire::target(wire::UNDO), wire::argument(&range), None);
    shell.frame();
    assert!(succeeded(&marked));
    assert_eq!(
        refusal(&undone),
        Some(tilo_session_core::desktop::i18n::t(
            "activity-served-read-only"
        ))
    );

    // Someone outside the workspace reads nothing of it.
    let outside = RemoteActions::with_provider(shell.session.clone(), Caller::local_person("eve"));
    let feed = outside.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    shell.frame();
    assert!(
        feed.call()
            .with(|arrival| matches!(arrival, Arrival::Failed(ActionCallError::UnknownAction)))
    );
}

/// A host still loading the model's code serves once it has it; frames keep coming until then,
/// whether or not the host wakes the desktop.
#[test]
fn reads_asked_while_the_code_loads_are_served_once_it_has_loaded() {
    let code = Rc::new(std::cell::Cell::new(ActivityPaneCode::Loading));
    let mut shell = Served::new(Some(Rc::clone(&code)));
    let api = shell.api(person());
    let feed = api.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    shell.frame();
    assert!(latest::<wire::Feed>(&feed).is_none());
    assert!(shell.surface.activity_loading && shell.surface.needs_animation_frame());

    code.set(ActivityPaneCode::Ready(open_activity));
    shell.frame();
    assert!(!shell.surface.activity_loading);
    assert_eq!(latest::<wire::Feed>(&feed).unwrap().1.entries.len(), 1);
}

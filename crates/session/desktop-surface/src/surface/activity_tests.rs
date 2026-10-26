//! The workspace's activity as a desktop hosting its windows in process
//! serves it to the Activity app: through the session the windows ask, as
//! the app asks a native session's broker.

use std::sync::Arc;

use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{ActionHostError, Caller};
use tilo_actions::{InvocationUpdate, InvocationUpdate::Item};
use ui::{Arrival, RemoteActions};

use super::*;

const ROOM: &str = "room-a";

struct Activity;

impl TileSource for Activity {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        (app == wire::APP_ID).then(|| Tab {
            id,
            title: "Activity".into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.into(),
                label: "Activity".into(),
                note: String::new(),
            },
            app_title: None,
        })
    }
}

struct RetainedHistory {
    log: Rc<std::cell::RefCell<AutoCommit>>,
}

impl HistorySource for RetainedHistory {
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Option<crate::activity::ActivitySnapshot> {
        let room = tree.workspaces.iter().find(|w| w.id == workspace)?.room();
        let history = crate::history_log::room_history(self.log.borrow().clone(), room);
        let mut desktop = tree.clone();
        desktop.active = workspace;
        crate::activity::ActivitySnapshot::new(desktop, history.doc).ok()
    }

    fn record_histories(
        &mut self,
        _: &DesktopTree,
        _: WorkspaceId,
    ) -> Vec<crate::activity::DetachedRecordHistory> {
        Vec::new()
    }

    fn host(&self) -> Rc<dyn crate::activity::ActivityHost> {
        Rc::new(crate::activity::UnavailableActivityHost)
    }

    fn revision(&self) -> u64 {
        use automerge::transaction::Transactable as _;
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.log.borrow().base_heads().hash(&mut hasher);
        hasher.finish()
    }
}

/// A host that fetches the model's code when it is first needed, as the
/// browser desktop does.
struct DeferredHistory {
    retained: RetainedHistory,
    code: Rc<std::cell::Cell<ActivityPaneCode>>,
}

impl HistorySource for DeferredHistory {
    fn workspace_history(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Option<crate::activity::ActivitySnapshot> {
        self.retained.workspace_history(tree, workspace)
    }

    fn record_histories(
        &mut self,
        tree: &DesktopTree,
        workspace: WorkspaceId,
    ) -> Vec<crate::activity::DetachedRecordHistory> {
        self.retained.record_histories(tree, workspace)
    }

    fn host(&self) -> Rc<dyn crate::activity::ActivityHost> {
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
    log: Rc<std::cell::RefCell<AutoCommit>>,
    tree: DesktopTree,
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
        };
        let tree = seed_tree(&mut minter, "Desk", vec![tab]);
        let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Activity));
        let mut tree = handle.read(Clone::clone);
        let mut retained = AutoCommit::new();
        assert!(crate::history_log::record(
            &mut retained,
            &serde_json::to_value(&tree).unwrap(),
            7,
            "Ada",
            1_000,
        ));
        tree.workspaces[0].name = "Renamed".into();
        assert!(crate::history_log::record(
            &mut retained,
            &serde_json::to_value(&tree).unwrap(),
            7,
            "Ada",
            2_000,
        ));
        let log = Rc::new(std::cell::RefCell::new(retained));
        let retained = RetainedHistory {
            log: Rc::clone(&log),
        };
        match code {
            Some(code) => surface.set_history_source(Box::new(DeferredHistory { retained, code })),
            None => surface.set_history_source(Box::new(retained)),
        }
        let (session, openings) = crate::session::serve(ROOM, Arc::default());
        surface.serve_activity(openings.activity());
        Self {
            surface,
            handle,
            session,
            log,
            tree,
        }
    }

    /// The windows' door to the session, as `caller`.
    fn door(&self, caller: Caller) -> RemoteActions {
        RemoteActions::with_host(self.session.clone(), caller.in_room(ROOM))
    }

    /// One pass of the desktop: what the windows asked is answered.
    fn frame(&mut self) {
        self.surface
            .update(&AutoCommit::new(), &ui::automerge::ROOT);
    }

    /// Someone renames the workspace: the history moves.
    fn rename(&mut self, name: &str, at_ms: i64) {
        self.tree.workspaces[0].name = name.into();
        assert!(crate::history_log::record(
            &mut self.log.borrow_mut(),
            &serde_json::to_value(&self.tree).unwrap(),
            7,
            "Ada",
            at_ms,
        ));
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
        self.tree.active.raw().to_string()
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
        Arrival::Failed(ActionHostError::Refused { reason }) => Some(reason.clone()),
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
    let mut desk = Served::new(None);
    let door = desk.door(person());
    let feed = door.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    assert!(
        latest::<wire::Feed>(&feed).is_none(),
        "read on the next pass"
    );
    desk.frame();
    let (first, read) = latest::<wire::Feed>(&feed).expect("the history is read");
    // The setup is no change to show; the rename is.
    assert_eq!(read.entries.len(), 1);
    assert!(!read.mutations, "a page's history is read only");

    desk.frame();
    desk.frame();
    assert_eq!(
        latest::<wire::Feed>(&feed).map(|(sequence, _)| sequence),
        Some(first),
        "nothing moved, nothing is sent"
    );

    desk.rename("Renamed again", 3_000);
    desk.frame();
    let (next, read) = latest::<wire::Feed>(&feed).unwrap();
    assert!(next > first);
    assert_eq!(read.entries.len(), 2);

    // Closing the app stops following: nothing is open on the desktop.
    drop(feed);
    desk.frame();
    assert!(desk.surface.activity.is_none());
}

/// Asked for from outside the app (the bar's History, a person's changes,
/// a command), Activity opens as a window, or comes forward, and the app
/// is offered the view; closing it closes its window.
#[test]
fn asking_for_activity_opens_the_app_at_the_view_asked_for() {
    let mut desk = Served::new(None);
    let workspace = desk.workspace();
    let open = |view, author: Option<&str>| Command::WorkspaceActivity {
        generation: "g".into(),
        workspace: workspace.clone(),
        view,
        author: author.map(str::to_owned),
    };
    let replay = open(navigation::ActivityView::Replay, None);
    desk.handle.dispatch(&replay, "g").unwrap();
    desk.frame();
    assert_eq!(desk.activity_windows(), 1);

    let door = desk.door(person());
    let focus = door.follow(wire::target(wire::FOCUS), wire::argument(&()));
    desk.frame();
    let (_, offered) = latest::<Option<wire::Focus>>(&focus).unwrap();
    let offered = offered.expect("the view is offered");
    assert_eq!(offered.view, wire::View::Replay);

    // The app showed it: it is not offered again.
    let shown = door.invoke(
        wire::target(wire::FOCUSED),
        wire::argument(&wire::Focused {
            serial: offered.serial,
        }),
        None,
    );
    desk.frame();
    assert!(succeeded(&shown));
    assert_eq!(latest::<Option<wire::Focus>>(&focus).unwrap().1, None);

    // One person's changes: the window there already comes forward.
    let theirs = open(navigation::ActivityView::CatchUp, Some("Ben"));
    desk.handle.dispatch(&theirs, "g").unwrap();
    desk.frame();
    assert_eq!(desk.activity_windows(), 1);
    let (_, offered) = latest::<Option<wire::Focus>>(&focus).unwrap();
    let offered = offered.unwrap();
    assert_eq!(offered.author.as_deref(), Some("Ben"));
    assert_eq!(offered.view, wire::View::History);

    let close = Command::CloseWorkspaceActivity {
        generation: "g".into(),
        workspace,
    };
    desk.handle.dispatch(&close, "g").unwrap();
    desk.frame();
    assert_eq!(desk.activity_windows(), 0);
}

/// What this device has seen is the person's own, and so is every
/// workspace's history but the one an agent works in; the history itself
/// a page only shows.
#[test]
fn what_is_seen_is_the_persons_own_and_the_history_is_read_only() {
    let mut desk = Served::new(None);
    let agent = desk.door(Caller::local_agent("helper"));
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
    desk.frame();
    let personal = crate::i18n::t("activity-served-personal-only");
    assert_eq!(refusal(&marked), Some(personal.clone()));
    assert_eq!(refusal(everywhere.call()), Some(personal));
    assert!(
        latest::<wire::Feed>(&here).is_some(),
        "an agent reads the workspace it works in"
    );

    let person = desk.door(person());
    let marked = person.invoke(wire::target(wire::MARK_VIEWED), viewed, None);
    let range = wire::Range::one(&wire::entry_hex(&[0; 16]));
    let undone = person.invoke(wire::target(wire::UNDO), wire::argument(&range), None);
    desk.frame();
    assert!(succeeded(&marked));
    assert_eq!(
        refusal(&undone),
        Some(crate::i18n::t("activity-served-read-only"))
    );

    // Someone outside the workspace reads nothing of it.
    let outside = RemoteActions::with_host(desk.session.clone(), Caller::local_person("eve"));
    let feed = outside.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    desk.frame();
    assert!(
        feed.call()
            .with(|arrival| matches!(arrival, Arrival::Failed(ActionHostError::UnknownAction)))
    );
}

/// A host still loading the model's code serves once it has it; frames
/// keep coming until then, whether or not the host wakes the desktop.
#[test]
fn reads_asked_while_the_code_loads_are_served_once_it_has_loaded() {
    let code = Rc::new(std::cell::Cell::new(ActivityPaneCode::Loading));
    let mut desk = Served::new(Some(Rc::clone(&code)));
    let door = desk.door(person());
    let feed = door.follow(
        wire::target(wire::FEED),
        wire::argument(&wire::FeedQuery::default()),
    );
    desk.frame();
    assert!(latest::<wire::Feed>(&feed).is_none());
    assert!(desk.surface.activity_loading && desk.surface.needs_animation_frame());

    code.set(ActivityPaneCode::Ready(open_activity));
    desk.frame();
    assert!(!desk.surface.activity_loading);
    assert_eq!(latest::<wire::Feed>(&feed).unwrap().1.entries.len(), 1);
}

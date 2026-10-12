//! Carrying a native app's window to another workspace, end to end through
//! the shell's transfer flow: the sheet, the record crossing with its
//! history, the staged resume projection, the new instance binding, and only
//! then the old one closing. The app processes are modelled by what they
//! hold — the record's writer in their room's profile — and by the surfaces
//! the compositor would adopt and destroy.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{ROOT, ReadDoc as _, Value};
use shell::shell::chrome_state::{Chrome, ChromeState};
use shell::shell::document_import::{Awaiting, HostedRooms, ProposalStatus};
use shell::shell::personal_continuity::AppInstanceId;
use shell::shell::personal_continuity_registry::global_registry;
use shell::shell::portal_dialog::{PortalDialogController, PortalDialogQueue};
use shell::shell::record_hydration::{RecordAddress, RecordSource, set_source};
use shell::shell::state::{
    FreshId as _, RoomId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTree,
};
use shell::shell::window_actions::{MoveOutcome, transfer_tab_to_workspace};
use shell::shell::window_transfer::{
    HandoffLink, TransferLauncher, TransferMode, report_handoff, tick,
};
use tilo_peer_state::document_import::{
    DocumentImportProposalV1, DocumentImportTerms, ImportState,
};
use ui::application::NativeProfile;
use ui::application_view::SavedInstanceView;
use ui::collaboration::handoff::{HandoffReport, HandoffRequest};
use ui::record::transfer::{self, Retirement, TransferError};
use ui::record::{
    RecordCheckout, RecordId, RecordKind, RecordOpenError, RecordOwners, RecordStore,
};

const APP: &str = "org.example.notes";
const TAB: TabId = 4;
const SURFACE: u64 = 4_404;
const NEW_SURFACE: u64 = 5_505;
const INSTANCE: u64 = 7_001;

/// What the destination launch received, the way the real launcher reads it.
#[derive(Clone, Debug)]
struct Launched {
    room: RoomId,
    tab: TabId,
    record_instance: u64,
    resume: Option<(u16, Vec<u8>)>,
}

struct FakeLauncher {
    launched: Rc<RefCell<Vec<Launched>>>,
    refuse: bool,
}

impl TransferLauncher for FakeLauncher {
    fn launch(&mut self, app_id: &str, workspace: &Workspace, tab: TabId) -> Result<(), String> {
        if self.refuse {
            return Err("the app could not be started".into());
        }
        let (resume, record_instance) =
            shell::shell::launcher::take_staged_resume_for_package(app_id, workspace.room)
                .expect("the launch consumes the continuation staged for its tab");
        self.launched.borrow_mut().push(Launched {
            room: workspace.room,
            tab,
            record_instance,
            resume,
        });
        Ok(())
    }
}

fn profile_root(data: &Path, room: RoomId) -> PathBuf {
    let room = room.to_string();
    launcher::appdir::host_app_dir(data, &launcher::appdir::room_slug(Some(&room)), APP)
        .join(".local/share")
}

fn read_record(
    store: &Rc<dyn transfer::DurableStore>,
    app: &str,
    room: &str,
    record: &RecordCheckout,
) -> Result<automerge::AutoCommit, TransferError> {
    transfer::read(store, app, room, &transfer::Origin::Record(record.clone()))
}

fn body(doc: &automerge::AutoCommit) -> Option<String> {
    match doc.get(ROOT, "body") {
        Ok(Some((Value::Scalar(value), _))) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

struct Fixture {
    chrome: Chrome,
    queue: PortalDialogQueue,
    launched: Rc<RefCell<Vec<Launched>>>,
    home: WorkspaceId,
    home_room: RoomId,
    elsewhere: WorkspaceId,
    elsewhere_room: RoomId,
    data: tempfile::TempDir,
    record: RecordCheckout,
    resume: Vec<u8>,
    _env: tilo_test_env::TestHome,
}

impl Fixture {
    /// "Home" shows a Notes window whose instance saved its notebook in
    /// Home's profile; "Elsewhere" is empty. `old` is the running Notes
    /// process, holding the notebook's writer. Without a record, the window
    /// is an app that keeps its state only in its process.
    #[allow(
        clippy::too_many_lines,
        reason = "the whole desktop, profile and running instance the scenarios share, in one place"
    )]
    fn new(with_record: bool, refuse_launch: bool) -> (Self, Option<RecordStore>) {
        let env = tilo_test_env::TestHome::new().expect("isolated home");
        let data = tempfile::tempdir().unwrap();
        let mut tree = WorkspaceTree::bootstrap_default();
        let home = tree.desktop.workspaces[0].id;
        let home_room = tree.desktop.workspaces[0].room;
        let home_strip = tree.desktop.workspaces[0].strips[0].id;
        tree.desktop.workspaces[0].strips[0].window_groups = vec![WindowGroup {
            id: 40,
            active_tab: TAB,
            tabs: vec![Tab {
                id: TAB,
                title: "Notes".into(),
                tile: TileContent::Foreign {
                    app_id: APP.into(),
                    surface_id: SURFACE,
                },
            }],
            width: WindowWidth::Normal,
            maximized: false,
        }];
        tree.desktop.set_active_group(home_strip, 40);
        let elsewhere = WorkspaceId::new();
        let mut other = Workspace::new(elsewhere, "Elsewhere");
        let elsewhere_room = other.room;
        let other_strip = StripId::new();
        other.strips.push(Strip::new(other_strip, "Main"));
        tree.desktop.workspaces.push(other);
        tree.desktop.active_strip.insert(elsewhere, other_strip);
        tree.seed_doc_from_cache();
        let chrome = ChromeState::new(tree);

        let record = RecordCheckout::new(
            RecordId::derived("default", &RecordId::for_tab(0)),
            RecordKind::new("notes.notebook"),
        );
        let view = ui::view::View {
            scroll: vec![120.0],
            ..ui::view::View::default()
        };
        let resume = ui::app::compose_structural_continuation_metadata(
            ui::view::View {
                scroll: vec![480.0],
                ..ui::view::View::default()
            },
            None,
        )
        .expect("a structural checkpoint")
        .bytes;
        let old = with_record.then(|| {
            let root = profile_root(data.path(), home_room);
            std::fs::create_dir_all(&root).unwrap();
            let profile = NativeProfile::open(&root, Some(&home_room.to_string())).unwrap();
            let records = RecordOwners::new(root)
                .open(APP, profile.workspace_id())
                .unwrap();
            records.create_record(&record).unwrap();
            records
                .with(&record.id, |doc| {
                    doc.put(ROOT, "body", "Groceries: milk").unwrap();
                })
                .unwrap();
            records.try_flush().unwrap();
            profile
                .adopt_view(
                    &SavedInstanceView {
                        version: 2,
                        app_id: APP.into(),
                        device_id: profile.device_id().into(),
                        workspace_id: profile.workspace_id().into(),
                        instance_id: INSTANCE,
                        record_id: record.id.as_str().into(),
                        record_kind: record.kind.as_str().into(),
                        view,
                        application: None,
                    },
                    INSTANCE,
                    &record,
                )
                .unwrap();
            let shared = global_registry();
            let mut registry = shared.lock().unwrap();
            registry.bind_tab(AppInstanceId(TAB), AppInstanceId(INSTANCE));
            registry
                .report(
                    home_room,
                    APP,
                    AppInstanceId(INSTANCE),
                    ui::app::STRUCTURAL_CONTINUATION_SCHEMA,
                    resume.clone(),
                )
                .expect("the old instance reported where it was");
            records
        });

        let queue = PortalDialogQueue::default();
        let launched = Rc::new(RefCell::new(Vec::new()));
        {
            let mut state = chrome.borrow_mut();
            state.window_transfers.set_launcher(Box::new(FakeLauncher {
                launched: Rc::clone(&launched),
                refuse: refuse_launch,
            }));
            state.window_transfers.set_dialog_queue(queue.clone());
            state.window_transfers.set_attended(|| true);
            state
                .window_transfers
                .set_data_home(data.path().to_path_buf());
        }
        (
            Self {
                chrome,
                queue,
                launched,
                home,
                home_room,
                elsewhere,
                elsewhere_room,
                data,
                record,
                resume,
                _env: env,
            },
            old,
        )
    }

    fn ask(&self, mode: TransferMode) {
        assert_eq!(
            transfer_tab_to_workspace(&mut self.chrome.borrow_mut(), TAB, self.elsewhere, mode),
            Ok(MoveOutcome::AwaitingConsent)
        );
    }

    /// Answer the sheet with the choice whose label contains `choice`.
    fn answer(&self, choice: &str) -> shell::shell::portal_dialog::DialogView {
        let mut sheet = PortalDialogController::new(self.queue.clone());
        assert!(sheet.pump(), "the transfer sheet is up");
        let view = sheet.view().expect("the transfer sheet");
        let index = view
            .choices()
            .iter()
            .position(|label| label.contains(choice))
            .unwrap_or_else(|| panic!("no {choice:?} among {:?}", view.choices()));
        while sheet.selected() != index {
            assert!(sheet.select_next(false));
        }
        assert!(sheet.confirm());
        tick(&mut self.chrome.borrow_mut());
        view
    }

    fn tabs_in(&self, workspace: WorkspaceId) -> Vec<(TabId, TileContent)> {
        self.chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .strips
            .iter()
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .map(|tab| (tab.id, tab.tile.clone()))
            .collect()
    }

    fn close_requests(&self) -> Vec<u64> {
        self.chrome.borrow_mut().take_surface_close_requests()
    }

    /// The compositor adopting the destination instance's first window.
    fn bind(&self, surface: u64) -> TabId {
        let chrome = self.chrome.borrow();
        let mut tree = chrome.tree_mut();
        shell::shell::tab_binding::adopt_toplevel_in_room(
            &mut tree,
            surface,
            Some(APP.into()),
            self.elsewhere_room,
        )
        .expect("the destination room is hosted here")
    }

    /// The compositor removing a window whose client went away.
    fn destroy(&self, surface: u64) {
        let chrome = self.chrome.borrow();
        let mut tree = chrome.tree_mut();
        shell::shell::tab_binding::close_tab_for_destroyed_surface(&mut tree, surface);
    }

    fn profile(&self, room: RoomId) -> NativeProfile {
        NativeProfile::open(
            &profile_root(self.data.path(), room),
            Some(&room.to_string()),
        )
        .unwrap()
    }

    /// The record the destination instance's saved presentation names.
    fn arrived(&self, instance: u64) -> (NativeProfile, SavedInstanceView, RecordCheckout) {
        let profile = self.profile(self.elsewhere_room);
        let view = profile
            .saved_view(instance)
            .unwrap()
            .expect("the moved instance has its presentation in the destination");
        let record = RecordCheckout::new(
            RecordId::parse(&view.record_id).unwrap(),
            RecordKind::new(view.record_kind.clone()),
        );
        (profile, view, record)
    }

    fn receipts(&self, workspace: WorkspaceId) -> Vec<String> {
        self.chrome
            .borrow()
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .effect_receipts
            .iter()
            .filter_map(|receipt| ui::history_source::sentence::step_summary(&receipt.context))
            .collect()
    }

    fn notifications(&self) -> Vec<String> {
        self.chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| format!("{} — {}", n.title, n.body))
            .collect()
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one move, followed step by step from the sheet to the old window closing"
)]
fn a_moved_window_keeps_its_document_history_and_place_and_the_old_one_closes_last() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    f.ask(TransferMode::Move);
    assert!(
        f.tabs_in(f.elsewhere).is_empty(),
        "nothing moves before the answer"
    );
    assert!(f.close_requests().is_empty());

    let sheet = f.answer("Move");
    assert!(sheet.title().contains("Notes") && sheet.title().contains("Elsewhere"));
    assert!(
        sheet.detail().contains("history") && sheet.detail().contains("Only you"),
        "the sheet says what crosses and who has access: {}",
        sheet.detail()
    );

    // The destination instance is starting on the carried record, with the
    // old instance's resume projection; the old window is untouched.
    let launched = f.launched.borrow().clone();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].room, f.elsewhere_room);
    assert_eq!(launched[0].record_instance, INSTANCE);
    assert_eq!(
        launched[0].resume,
        Some((ui::app::STRUCTURAL_CONTINUATION_SCHEMA, f.resume.clone())),
        "the resume projection travels with the window"
    );
    let new_tab = launched[0].tab;
    assert!(matches!(
        f.tabs_in(f.home).as_slice(),
        [(
            TAB,
            TileContent::Foreign {
                surface_id: SURFACE,
                ..
            }
        )]
    ));
    assert!(
        f.close_requests().is_empty(),
        "the old window stays until the new one has bound"
    );
    let (destination, view, carried) = f.arrived(INSTANCE);
    assert_ne!(
        carried.id, f.record.id,
        "the destination gets an id of its own"
    );
    assert_eq!(
        view.view.scroll,
        [120.0],
        "the saved presentation came along"
    );
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .expect("the record is in the destination room");
    assert_eq!(body(&arrived).as_deref(), Some("Groceries: milk"));
    assert_eq!(
        transfer::provenance(&arrived),
        std::slice::from_ref(&f.record.id)
    );

    // The person keeps typing in the old window after the record was read.
    old.with(&f.record.id, |doc| {
        doc.put(ROOT, "body", "Groceries: milk, eggs").unwrap();
    })
    .unwrap();
    old.try_flush().unwrap();
    tick(&mut f.chrome.borrow_mut());
    assert!(
        f.close_requests().is_empty(),
        "still waiting for the new window"
    );

    assert_eq!(f.bind(NEW_SURFACE), new_tab);
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(
        f.close_requests(),
        [SURFACE],
        "only now is the old window asked to close"
    );
    f.destroy(SURFACE);
    tick(&mut f.chrome.borrow_mut());
    assert!(
        f.chrome.borrow().window_transfers.is_busy(),
        "the source record is not retired while its writer still runs"
    );

    // The old process exits; its last edit comes over and the source retires.
    drop(old);
    tick(&mut f.chrome.borrow_mut());
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(body(&arrived).as_deref(), Some("Groceries: milk, eggs"));
    let source = f.profile(f.home_room);
    assert!(matches!(
        read_record(source.store(), APP, source.workspace_id(), &f.record),
        Err(TransferError::Open(RecordOpenError::Retired))
    ));
    assert!(f.tabs_in(f.home).is_empty());
    assert!(matches!(
        f.tabs_in(f.elsewhere).as_slice(),
        [(tab, TileContent::Foreign { surface_id: NEW_SURFACE, .. })] if *tab == new_tab
    ));
    assert!(
        f.notifications()
            .iter()
            .any(|n| n.contains("last edits") && n.contains("Home")),
        "late edits are never merged silently: {:?}",
        f.notifications()
    );
    assert!(
        f.receipts(f.home)
            .iter()
            .any(|r| r.contains("to Elsewhere"))
    );
    assert!(
        f.receipts(f.elsewhere)
            .iter()
            .any(|r| r.contains("from Home"))
    );
}

#[test]
fn late_edits_restart_a_destination_instance_that_already_has_the_record() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    f.ask(TransferMode::Move);
    f.answer("Move");
    let (destination, _, carried) = f.arrived(INSTANCE);
    old.with(&f.record.id, |doc| {
        doc.put(ROOT, "body", "Groceries: milk, bread").unwrap();
    })
    .unwrap();
    old.try_flush().unwrap();

    let new_tab = f.bind(NEW_SURFACE);
    // The new instance opens its record, taking the destination writer.
    let new_instance = RecordOwners::new(profile_root(f.data.path(), f.elsewhere_room))
        .open(APP, destination.workspace_id())
        .unwrap();
    new_instance.try_open_record(&carried).unwrap();
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.close_requests(), [SURFACE]);
    f.destroy(SURFACE);
    drop(old);
    tick(&mut f.chrome.borrow_mut());

    // It is restarted around the merge rather than raced.
    assert_eq!(f.close_requests(), [NEW_SURFACE]);
    assert!(matches!(
        f.tabs_in(f.elsewhere).as_slice(),
        [(tab, TileContent::LocalApp { .. })] if *tab == new_tab
    ));
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.launched.borrow().len(), 1, "not before it let go");
    drop(new_instance);
    tick(&mut f.chrome.borrow_mut());
    let launched = f.launched.borrow().clone();
    assert_eq!(launched.len(), 2, "reopened on the merged record");
    assert_eq!(
        (launched[1].tab, launched[1].record_instance),
        (new_tab, INSTANCE)
    );
    assert_eq!(f.bind(NEW_SURFACE + 1), new_tab);
    tick(&mut f.chrome.borrow_mut());
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    let merged = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(body(&merged).as_deref(), Some("Groceries: milk, bread"));
    assert!(f.notifications().iter().any(|n| n.contains("last edits")));
}

#[test]
fn a_move_whose_new_window_never_opens_rolls_back_and_keeps_the_old_one() {
    let (f, old) = Fixture::new(true, true);
    f.ask(TransferMode::Move);
    f.answer("Move");

    assert!(
        f.tabs_in(f.elsewhere).is_empty(),
        "the new tab is taken away"
    );
    assert!(matches!(
        f.tabs_in(f.home).as_slice(),
        [(
            TAB,
            TileContent::Foreign {
                surface_id: SURFACE,
                ..
            }
        )]
    ));
    assert!(
        f.close_requests().is_empty(),
        "the old window was never asked to close"
    );
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    assert!(
        f.notifications()
            .iter()
            .any(|n| n.contains("Couldn't move") && n.contains("still in Home")),
        "{:?}",
        f.notifications()
    );
    let (destination, _, carried) = f.arrived(INSTANCE);
    assert!(
        matches!(
            read_record(
                destination.store(),
                APP,
                destination.workspace_id(),
                &carried
            ),
            Err(TransferError::Open(RecordOpenError::Retired))
        ),
        "the unopened import can never be taken for live state"
    );
    drop(old);
    let source = f.profile(f.home_room);
    let kept = read_record(source.store(), APP, source.workspace_id(), &f.record).unwrap();
    assert_eq!(body(&kept).as_deref(), Some("Groceries: milk"));
}

#[test]
fn an_old_window_that_crashes_mid_move_still_moves_its_saved_work_and_says_so() {
    let (f, old) = Fixture::new(true, false);
    f.ask(TransferMode::Move);
    f.answer("Move");
    // The old process dies on its own before the new window appears.
    f.destroy(SURFACE);
    drop(old);
    tick(&mut f.chrome.borrow_mut());
    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());

    assert!(
        f.close_requests().is_empty(),
        "there is nothing left to close"
    );
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    let (destination, _, carried) = f.arrived(INSTANCE);
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(body(&arrived).as_deref(), Some("Groceries: milk"));
    assert!(
        f.notifications()
            .iter()
            .any(|n| n.contains("last few seconds may be missing")),
        "{:?}",
        f.notifications()
    );
}

#[test]
fn a_copy_leaves_the_original_window_and_document_where_they_are() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    f.ask(TransferMode::Copy);
    let sheet = f.answer("Copy");
    assert!(
        sheet.choices()[0].contains("Copy"),
        "the answer the action asked for is offered first"
    );
    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());

    assert!(
        f.close_requests().is_empty(),
        "a copy never closes the original"
    );
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    assert!(matches!(
        f.tabs_in(f.home).as_slice(),
        [(
            TAB,
            TileContent::Foreign {
                surface_id: SURFACE,
                ..
            }
        )]
    ));
    drop(old);
    let source = f.profile(f.home_room);
    let original = read_record(source.store(), APP, source.workspace_id(), &f.record)
        .expect("the original stays live in its room");
    let beside = f.launched.borrow()[0].record_instance;
    assert_ne!(beside, INSTANCE, "a copy is a window of its own");
    let (destination, _, carried) = f.arrived(beside);
    let copy = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(body(&original), body(&copy));
    assert!(f.receipts(f.home).is_empty());
    assert!(f.receipts(f.elsewhere).iter().any(|r| r.contains("Copied")));
}

#[test]
fn a_window_whose_state_cannot_travel_is_offered_a_new_window_and_never_closed() {
    let (f, _) = Fixture::new(false, false);
    f.ask(TransferMode::Move);
    let sheet = f.answer("new window");
    assert_eq!(sheet.choices().len(), 1, "Move and Copy are not offered");
    assert!(
        sheet.detail().contains("running program"),
        "{}",
        sheet.detail()
    );

    assert!(matches!(
        f.tabs_in(f.home).as_slice(),
        [(
            TAB,
            TileContent::Foreign {
                surface_id: SURFACE,
                ..
            }
        )]
    ));
    assert!(matches!(
        f.tabs_in(f.elsewhere).as_slice(),
        [(_, TileContent::LocalApp { app_id, note, .. })] if app_id == APP && note.is_empty()
    ));
    assert!(f.close_requests().is_empty());
    assert!(f.launched.borrow().is_empty(), "nothing was staged for it");

    // Cancelling the sheet changes nothing at all.
    f.ask(TransferMode::Move);
    let mut sheet = PortalDialogController::new(f.queue.clone());
    assert!(sheet.pump());
    assert!(sheet.cancel());
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.tabs_in(f.elsewhere).len(), 1);
    assert!(f.close_requests().is_empty());
    assert!(!f.chrome.borrow().window_transfers.is_pending());
}

#[test]
fn a_second_window_on_the_same_document_blocks_move_but_not_copy() {
    let (f, old) = Fixture::new(true, false);
    {
        let chrome = f.chrome.borrow();
        let mut tree = chrome.tree_mut();
        tree.desktop.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(Tab {
                id: 9,
                title: "Notes".into(),
                tile: TileContent::Foreign {
                    app_id: APP.into(),
                    surface_id: 9_909,
                },
            });
    }
    // The second window runs another instance showing the same notebook.
    let root = profile_root(f.data.path(), f.home_room);
    let profile = NativeProfile::open(&root, Some(&f.home_room.to_string())).unwrap();
    let mine = profile.saved_view(INSTANCE).unwrap().unwrap();
    profile.adopt_view(&mine, INSTANCE + 1, &f.record).unwrap();
    global_registry()
        .lock()
        .unwrap()
        .bind_tab(AppInstanceId(9), AppInstanceId(INSTANCE + 1));

    f.ask(TransferMode::Move);
    let sheet = f.answer("Copy");
    assert_eq!(sheet.choices().len(), 1, "{:?}", sheet.choices());
    assert!(
        sheet.detail().contains("another window"),
        "{}",
        sheet.detail()
    );
    drop(old);
}

#[test]
fn an_app_without_records_carries_the_document_it_keeps_in_the_room() {
    let (f, _) = Fixture::new(false, false);
    // A plain persisted app: one document for itself in Home, which its
    // running instance writes.
    let root = profile_root(f.data.path(), f.home_room);
    std::fs::create_dir_all(&root).unwrap();
    let home = NativeProfile::open(&root, Some(&f.home_room.to_string())).unwrap();
    let (mut state, mut old) = ui::persist::open_in(Rc::clone(home.store()), APP).unwrap();
    state
        .doc
        .put(ROOT, "body", "Scratch: call the plumber")
        .unwrap();
    old.request(&mut state.doc);
    old.flush();
    global_registry()
        .lock()
        .unwrap()
        .bind_tab(AppInstanceId(TAB), AppInstanceId(INSTANCE));

    f.ask(TransferMode::Move);
    let sheet = f.answer("Move");
    assert_eq!(
        sheet.choices().len(),
        2,
        "its document can move or be copied"
    );
    let (destination, _, carried) = f.arrived(INSTANCE);
    assert_eq!(carried.kind.as_str(), transfer::APP_DOCUMENT_KIND);
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(body(&arrived).as_deref(), Some("Scratch: call the plumber"));

    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.close_requests(), [SURFACE]);
    f.destroy(SURFACE);
    tick(&mut f.chrome.borrow_mut());
    assert!(
        f.chrome.borrow().window_transfers.is_busy(),
        "the app document is not retired while its writer still runs"
    );
    drop((state, old));
    tick(&mut f.chrome.borrow_mut());
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    let origin = transfer::Origin::AppDocument(RecordKind::new(transfer::APP_DOCUMENT_KIND));
    assert!(matches!(
        transfer::read(home.store(), APP, home.workspace_id(), &origin),
        Err(TransferError::Open(RecordOpenError::Retired))
    ));
}

type Asked = Rc<RefCell<Vec<(RoomId, String, u64, HandoffRequest)>>>;

/// The instances' primary routes, as the broker reaches them.
struct Routes(Asked);

impl HandoffLink for Routes {
    fn request(&self, room: RoomId, app_id: &str, instance: u64, request: HandoffRequest) {
        self.0
            .borrow_mut()
            .push((room, app_id.to_owned(), instance, request));
    }
}

fn answer(f: &Fixture, report: &HandoffReport) {
    report_handoff(f.home_room, APP.into(), INSTANCE, &report.encode()).unwrap();
    tick(&mut f.chrome.borrow_mut());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one move, followed step by step through both handoff steps"
)]
fn a_move_checkpoints_first_and_the_old_instance_retires_its_own_document() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    let asked: Asked = Rc::default();
    f.chrome
        .borrow_mut()
        .window_transfers
        .set_handoff_link(Box::new(Routes(Rc::clone(&asked))));
    f.ask(TransferMode::Move);
    f.answer("Move");

    // Nothing is read before the running instance has made its work durable.
    let token = match asked.borrow().as_slice() {
        [(room, app, INSTANCE, HandoffRequest::Checkpoint { token })]
            if *room == f.home_room && app == APP =>
        {
            *token
        }
        other => panic!("expected one checkpoint request, got {other:?}"),
    };
    assert!(f.launched.borrow().is_empty());
    assert!(f.tabs_in(f.elsewhere).is_empty());

    // The instance flushes the edit it was holding in memory, and answers.
    old.with(&f.record.id, |doc| {
        doc.put(ROOT, "body", "Groceries: milk, eggs").unwrap();
    })
    .unwrap();
    old.try_flush().unwrap();
    answer(&f, &HandoffReport::done(token, &[]));
    assert_eq!(f.launched.borrow().len(), 1);
    let (destination, _, carried) = f.arrived(INSTANCE);
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(
        body(&arrived).as_deref(),
        Some("Groceries: milk, eggs"),
        "the checkpoint is what was read"
    );

    // The new window binds; the old instance is asked to retire what it shows.
    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());
    let (token, successor) = match asked.borrow().last() {
        Some((_, _, INSTANCE, HandoffRequest::Retire { token, successor })) => {
            (*token, successor.clone())
        }
        other => panic!("expected a retire request, got {other:?}"),
    };
    assert_eq!(successor.room, f.elsewhere_room.to_string());
    assert_eq!(successor.record, carried.id.as_str());
    assert!(
        f.close_requests().is_empty(),
        "the old window stays until its document says where it went"
    );

    // It stamps the retirement on its own document, which is what travels to
    // everyone holding the record, and answers with the heads before it.
    let before = old
        .with(&f.record.id, |doc| {
            let before = doc.get_heads();
            transfer::stamp_retired(doc, &Retirement::Moved(Some(successor.clone()))).unwrap();
            before
        })
        .unwrap();
    old.try_flush().unwrap();
    answer(&f, &HandoffReport::done(token, &before));
    assert_eq!(f.close_requests(), [SURFACE]);
    f.destroy(SURFACE);
    drop(old);
    tick(&mut f.chrome.borrow_mut());
    assert!(!f.chrome.borrow().window_transfers.is_pending());

    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert!(
        !transfer::is_retired(&arrived),
        "the source's retirement never comes along into its successor"
    );
    assert_eq!(body(&arrived).as_deref(), Some("Groceries: milk, eggs"));
    let source = f.profile(f.home_room);
    let retired = transfer::read_until(
        source.store(),
        APP,
        source.workspace_id(),
        &transfer::Origin::Record(f.record.clone()),
        &before,
    );
    assert!(retired.is_ok(), "the history before the stamp is intact");
    assert!(matches!(
        read_record(source.store(), APP, source.workspace_id(), &f.record),
        Err(TransferError::Open(RecordOpenError::Retired))
    ));
    assert!(
        !f.notifications().iter().any(|n| n.contains("last edits")),
        "nothing arrived late: {:?}",
        f.notifications()
    );
}

#[test]
fn a_collaborator_is_told_who_moved_their_document_and_where_if_they_can_see_it() {
    let (f, _) = Fixture::new(false, false);
    global_registry()
        .lock()
        .unwrap()
        .bind_tab(AppInstanceId(TAB), AppInstanceId(INSTANCE));
    let moved = |room: String| HandoffReport::Retired {
        successor: transfer::Successor {
            room,
            record: "uuid:elsewhere".into(),
            by: "person:ada".into(),
        },
    };
    answer(&f, &moved(f.elsewhere_room.to_string()));
    assert!(
        f.notifications()
            .iter()
            .any(|n| n.contains("“Notes” was moved") && n.contains("to Elsewhere")),
        "{:?}",
        f.notifications()
    );
    answer(&f, &moved(uuid::Uuid::new_v4().to_string()));
    assert!(
        f.notifications()
            .iter()
            .any(|n| n.contains("a workspace you're not in")),
        "a workspace the person is not in is not named: {:?}",
        f.notifications()
    );
    assert!(!f.chrome.borrow().window_transfers.is_pending());
}

/// The collaborator's other device, which keeps the moved document in its
/// new room.
struct Holder(Vec<u8>, std::sync::Mutex<Vec<RecordAddress>>);

impl RecordSource for Holder {
    fn fetch(
        &self,
        address: &RecordAddress,
    ) -> tilo_actions::remote::Pending<Result<Vec<u8>, String>> {
        self.1.lock().unwrap().push(address.clone());
        tilo_actions::remote::Pending::ready(Ok(self.0.clone()))
    }
}

#[test]
fn a_collaborators_window_follows_a_moved_document_into_the_workspace_it_went_to() {
    let (f, _) = Fixture::new(false, false);
    global_registry()
        .lock()
        .unwrap()
        .bind_tab(AppInstanceId(TAB), AppInstanceId(INSTANCE));
    // The successor as the mover's device keeps it in Elsewhere.
    let there = tempfile::tempdir().unwrap();
    let root = profile_root(there.path(), f.elsewhere_room);
    std::fs::create_dir_all(&root).unwrap();
    let profile = NativeProfile::open(&root, Some(&f.elsewhere_room.to_string())).unwrap();
    let successor = RecordCheckout::new(RecordId::minted(), RecordKind::new("notes.notebook"));
    let records = RecordOwners::new(root)
        .open(APP, profile.workspace_id())
        .unwrap();
    records.create_record(&successor).unwrap();
    records
        .with(&successor.id, |doc| {
            doc.put(ROOT, "body", "Groceries: milk, eggs").unwrap();
        })
        .unwrap();
    records.try_flush().unwrap();
    drop(records);
    let bytes = transfer::read_by_id(profile.store(), APP, profile.workspace_id(), &successor.id)
        .unwrap()
        .save();
    let holder = std::sync::Arc::new(Holder(bytes, std::sync::Mutex::default()));
    set_source(std::sync::Arc::clone(&holder) as std::sync::Arc<dyn RecordSource>);

    answer(
        &f,
        &HandoffReport::Retired {
            successor: transfer::Successor {
                room: f.elsewhere_room.to_string(),
                record: successor.id.as_str().into(),
                by: "person:ada".into(),
            },
        },
    );
    tick(&mut f.chrome.borrow_mut());

    assert_eq!(
        holder.1.lock().unwrap().as_slice(),
        [RecordAddress {
            room: f.elsewhere_room,
            app_id: APP.into(),
            record: successor.id.clone(),
        }],
        "the successor is asked for by the room it now lives in"
    );
    assert!(
        f.tabs_in(f.elsewhere).iter().any(|(_, tile)| matches!(
            tile,
            TileContent::LocalApp { app_id, .. } if app_id == APP
        )),
        "the window reopens where the document went: {:?}",
        f.tabs_in(f.elsewhere)
    );
    let here = f.profile(f.elsewhere_room);
    let kept = read_record(here.store(), APP, here.workspace_id(), &successor).unwrap();
    assert_eq!(body(&kept).as_deref(), Some("Groceries: milk, eggs"));
    assert!(!f.chrome.borrow().window_transfers.is_pending());
}

/// A workspace governed by its owner, as this desktop reaches it: what it
/// was asked, and what the test says its owner answered.
#[derive(Default)]
struct Governed {
    room: Option<RoomId>,
    proposed: Vec<(RoomId, RecordId, Vec<u8>)>,
    status: Option<ProposalStatus>,
    awaiting: Vec<Awaiting>,
    decided: Vec<([u8; 32], bool)>,
    /// The committed document the owner holds, for reads out of the room.
    committed: Option<Vec<u8>>,
}

struct GovernedRooms(Rc<RefCell<Governed>>);

const PROPOSAL: [u8; 32] = [9; 32];

impl HostedRooms for GovernedRooms {
    fn hosts(&self, room: RoomId) -> bool {
        self.0.borrow().room == Some(room)
    }
    fn read(&self, room: RoomId, _: &str, _: &RecordId) -> Result<Vec<u8>, String> {
        let governed = self.0.borrow();
        governed
            .committed
            .clone()
            .filter(|_| governed.room == Some(room))
            .ok_or_else(|| "the room does not hold that document".into())
    }
    fn propose(
        &self,
        room: RoomId,
        _: &str,
        record: &RecordId,
        _: &str,
        document: &[u8],
    ) -> tilo_actions::remote::Pending<Result<[u8; 32], String>> {
        self.0
            .borrow_mut()
            .proposed
            .push((room, record.clone(), document.to_vec()));
        tilo_actions::remote::Pending::ready(Ok(PROPOSAL))
    }
    fn proposal(&self, _: RoomId, id: &[u8; 32]) -> Option<ProposalStatus> {
        (*id == PROPOSAL).then(|| self.0.borrow().status).flatten()
    }
    fn awaiting_me(&self, room: RoomId) -> Option<Vec<Awaiting>> {
        let governed = self.0.borrow();
        (governed.room == Some(room)).then(|| governed.awaiting.clone())
    }
    fn decide(
        &self,
        _: RoomId,
        proposal: &DocumentImportProposalV1,
        accepted: bool,
    ) -> tilo_actions::remote::Pending<Result<(), String>> {
        self.0
            .borrow_mut()
            .decided
            .push((proposal.proposal_id, accepted));
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

fn governed(f: &Fixture) -> Rc<RefCell<Governed>> {
    let governed = Rc::new(RefCell::new(Governed {
        room: Some(f.elsewhere_room),
        ..Governed::default()
    }));
    f.chrome
        .borrow_mut()
        .window_transfers
        .set_hosted_rooms(Box::new(GovernedRooms(Rc::clone(&governed))));
    governed
}

const fn status(state: ImportState, record_present: bool) -> ProposalStatus {
    ProposalStatus {
        state,
        record_present,
    }
}

fn card_note(f: &Fixture) -> Option<String> {
    f.tabs_in(f.elsewhere)
        .into_iter()
        .find_map(|(_, tile)| match tile {
            TileContent::LocalApp { note, .. } => Some(note),
            _ => None,
        })
}

#[test]
fn a_window_moved_into_a_governed_workspace_opens_there_only_once_its_owner_accepts() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    let governed = governed(&f);
    f.ask(TransferMode::Move);
    let sheet = f.answer("Move");
    assert!(
        sheet.detail().contains("has to accept"),
        "the sheet says the owner decides: {}",
        sheet.detail()
    );

    // The document is proposed, whole and scoped to the governed room, and
    // nothing opens there yet.
    let (room, record, bytes) = governed.borrow().proposed[0].clone();
    assert_eq!(room, f.elsewhere_room);
    let (arriving, _) = transfer::validate_import(&bytes, &room.to_string(), &record, bytes.len())
        .expect("the governor would take exactly this record");
    assert_eq!(body(&arriving).as_deref(), Some("Groceries: milk"));
    assert!(f.launched.borrow().is_empty());
    assert!(
        card_note(&f).is_some_and(|note| note.contains("Waiting for the owner")),
        "the destination says what it waits for: {:?}",
        card_note(&f)
    );
    for waiting in [
        status(ImportState::Pending, false),
        status(ImportState::Accepted, false),
    ] {
        governed.borrow_mut().status = Some(waiting);
        tick(&mut f.chrome.borrow_mut());
        assert!(
            f.launched.borrow().is_empty(),
            "nothing opens before the accepted record is here"
        );
    }
    assert!(f.close_requests().is_empty());

    // Accepted and arrived: the instance reopens on the accepted record there.
    governed.borrow_mut().status = Some(status(ImportState::Accepted, true));
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.launched.borrow().len(), 1);
    let (_, view, _) = f.arrived(INSTANCE);
    assert_eq!(view.record_id, record.as_str());

    // Only now does the old window close and the original retire.
    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(f.close_requests(), [SURFACE]);
    f.destroy(SURFACE);
    drop(old);
    tick(&mut f.chrome.borrow_mut());
    assert!(!f.chrome.borrow().window_transfers.is_pending());
    let home = f.profile(f.home_room);
    assert!(matches!(
        read_record(home.store(), APP, home.workspace_id(), &f.record),
        Err(TransferError::Open(RecordOpenError::Retired))
    ));
}

#[test]
fn a_declined_or_unanswered_proposal_creates_nothing_and_keeps_the_window() {
    for (answer, told) in [
        (status(ImportState::Declined, false), "declined"),
        (status(ImportState::Expired, false), "didn't answer in time"),
    ] {
        let (f, old) = Fixture::new(true, false);
        let governed = governed(&f);
        f.ask(TransferMode::Move);
        f.answer("Move");
        assert_eq!(governed.borrow().proposed.len(), 1);
        governed.borrow_mut().status = Some(answer);
        tick(&mut f.chrome.borrow_mut());
        assert!(f.launched.borrow().is_empty(), "nothing was started");
        assert!(f.tabs_in(f.elsewhere).is_empty(), "the waiting card goes");
        assert!(f.close_requests().is_empty(), "the window stays");
        assert!(
            f.tabs_in(f.home)
                .iter()
                .any(|(tab, tile)| *tab == TAB && matches!(tile, TileContent::Foreign { .. }))
        );
        assert!(
            f.notifications().iter().any(|n| n.contains(told)),
            "the person is told: {:?}",
            f.notifications()
        );
        let home = f.profile(f.home_room);
        assert!(
            read_record(home.store(), APP, home.workspace_id(), &f.record).is_ok(),
            "the original stays live"
        );
        assert!(!f.chrome.borrow().window_transfers.is_pending());
        drop(old);
    }
}

#[test]
fn an_owner_answers_each_proposal_for_their_workspace_on_a_sheet() {
    let (f, _) = Fixture::new(false, false);
    let governed = governed(&f);
    let proposal = |id: u8, title: &str| {
        DocumentImportProposalV1::new(
            &DocumentImportTerms {
                room: f.elsewhere_room.to_string(),
                proposal_id: [id; 32],
                app: APP.into(),
                record: RecordId::minted().as_str().into(),
                title: title.into(),
                document: b"document".to_vec(),
                issued_unix: 1,
                expires_unix: 3_000,
            },
            &ed25519_dalek::SigningKey::from_bytes(&[id; 32]),
        )
        .unwrap()
    };
    governed.borrow_mut().awaiting = vec![
        Awaiting {
            proposal: proposal(1, "Groceries"),
            proposer: None,
        },
        Awaiting {
            proposal: proposal(2, "Taxes"),
            proposer: None,
        },
    ];
    tick(&mut f.chrome.borrow_mut());
    let mut sheet = PortalDialogController::new(f.queue.clone());
    for (choice, title) in [(0, "Groceries"), (1, "Taxes")] {
        assert!(sheet.pump(), "the owner is asked about “{title}”");
        let view = sheet.view().unwrap();
        assert!(view.title().contains(title) && view.title().contains("Elsewhere"));
        assert_eq!(view.choices()[0], "Accept it into Elsewhere");
        assert_eq!(view.choices()[1], "Decline");
        while sheet.selected() != choice {
            assert!(sheet.select_next(false));
        }
        assert!(sheet.confirm());
    }
    tick(&mut f.chrome.borrow_mut());
    assert_eq!(
        governed.borrow().decided,
        [([1; 32], true), ([2; 32], false)],
        "each answer is sent for its own proposal"
    );
    assert!(!f.chrome.borrow().window_transfers.is_busy());
}

#[test]
fn a_window_leaving_a_governed_workspace_carries_the_document_its_owner_committed() {
    let (f, old) = Fixture::new(true, false);
    let old = old.unwrap();
    // What the owner committed is newer than this device's app cache.
    let mut committed = old.read(&f.record.id, Clone::clone).unwrap();
    committed
        .put(ROOT, "body", "Groceries: milk, bread")
        .unwrap();
    let governed = Rc::new(RefCell::new(Governed {
        room: Some(f.home_room),
        committed: Some(committed.save()),
        ..Governed::default()
    }));
    f.chrome
        .borrow_mut()
        .window_transfers
        .set_hosted_rooms(Box::new(GovernedRooms(Rc::clone(&governed))));
    f.ask(TransferMode::Copy);
    f.answer("Copy");
    f.bind(NEW_SURFACE);
    tick(&mut f.chrome.borrow_mut());
    let beside = f.launched.borrow()[0].record_instance;
    let (destination, _, carried) = f.arrived(beside);
    let arrived = read_record(
        destination.store(),
        APP,
        destination.workspace_id(),
        &carried,
    )
    .unwrap();
    assert_eq!(
        body(&arrived).as_deref(),
        Some("Groceries: milk, bread"),
        "the copy comes from the owner's committed document"
    );
    assert!(f.close_requests().is_empty(), "a copy leaves the window");
    drop(old);
}

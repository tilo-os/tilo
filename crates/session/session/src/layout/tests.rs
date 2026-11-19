tilo_automerge_isolate::test_worker!();

use super::*;

fn view(strip: u64) -> PersonView {
    PersonView {
        front_strip: Some(tilo_desktop_model::StripId(strip)),
        ..PersonView::default()
    }
}

tilo_test_env::scenario! {
    fn a_view_is_written_only_once_it_rests_and_between_writes;

    phase a_view_that_did_not_move_is_not_written;
    // A view that did not move is not written again: the look that finds it writes nothing.
    {
        let written = view(11);
        let mut held = Held {
            read: true,
            written: Some(Written::View(written.clone())),
            ..Held::default()
        };
        assert_eq!(rested(&mut held, &written, false), None);
    }

    phase a_view_is_written_once_it_rests;
    // A view is written once it rests, not on the look that found it move: a drag or a burst of
    // switches changes it on every look and is one write when it is over.
    {
        let mut held = Held {
            read: true,
            ..Held::default()
        };
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "the look that found it"
        );
        assert_eq!(
            rested(&mut held, &view(11), false),
            Some(view(11)),
            "the look after it, unchanged"
        );
        assert_eq!(
            rested(&mut held, &view(12), false),
            None,
            "a look that moved it again writes nothing yet"
        );
        assert_eq!(rested(&mut held, &view(12), false), Some(view(12)));
    }

    phase a_view_is_written_only_between_writes_and_after_a_read;
    // A write of the view under way holds the look back, and a record this device has not read is
    // not written over.
    {
        let mut held = Held::default();
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "the record was not read"
        );
        held.read = true;
        held.writing = Some(Going::View(Box::new(Publishing {
            writing: crate::engine_write::Writing::write(
                std::sync::Arc::new(|| {}) as tilo_service::Wake,
                async {
                    Err(tilo_peer_state::owner::WriteRefusal::Refused(
                        "the test writes nothing".to_owned(),
                    ))
                },
            ),
            candidate: automerge::AutoCommit::new(),
            replaced: None,
            revision: None,
        })));
        assert_eq!(
            rested(&mut held, &view(11), false),
            None,
            "a write is under way"
        );
        held.writing = None;
        let _ = rested(&mut held, &view(11), false);
        assert_eq!(rested(&mut held, &view(11), false), Some(view(11)));
    }

    phase a_view_that_could_not_be_read_is_written_once_it_can;
    // A look that cannot read the view here writes nothing of it; the look that can writes the
    // person's view, the one a session ends with as well.
    {
        let mut held = Held {
            read: true,
            unreadable: true,
            ..Held::default()
        };
        assert_eq!(
            rested(&mut held, &view(13), true),
            None,
            "the view cannot be read here yet"
        );
        held.unreadable = false;
        assert_eq!(
            rested(&mut held, &view(13), true),
            Some(view(13)),
            "read here, the view is written"
        );
    }
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse("00000000-0000-4000-8000-000000000042").expect("an id")
}

/// A strip of one window, `title`, with ids minted by `ids`; the window's id beside it.
fn strip_of(
    ids: &mut tilo_desktop_model::IdMinter,
    title: &str,
) -> (tilo_desktop_model::Strip, u64) {
    let (strip, group, tab) = (ids.mint(), ids.mint(), ids.mint());
    let mut held = tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(strip), title);
    held.window_groups.push(tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: title.to_owned(),
            tile: tilo_desktop_model::TileContent::Stub {
                kind: tilo_desktop_model::AppKind::Other(title.to_owned()),
                label: title.to_owned(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    (held, tab)
}

/// Write `strips` as the layout of the workspace into `doc`, as another device does.
fn lay_out(doc: &mut automerge::AutoCommit, strips: Vec<tilo_desktop_model::Strip>) {
    laid_out_as(doc, strips, shared_resource::change_message(None));
}

/// [`lay_out`], committed with `message`.
fn laid_out_as(
    doc: &mut automerge::AutoCommit,
    strips: Vec<tilo_desktop_model::Strip>,
    message: String,
) {
    let mut workspace = Workspace::new(workspace_id(), "Layout");
    workspace.strips = strips;
    let desktop = tilo_desktop_model::ownership::SharedDesktop {
        tree: Layout {
            active: workspace_id(),
            workspaces: vec![workspace],
            ..Layout::default()
        },
        ..tilo_desktop_model::ownership::SharedDesktop::default()
    };
    shared_resource::write(doc, &desktop).expect("the layout is written");
    doc.commit_with(automerge::transaction::CommitOptions::default().with_message(message));
}

/// The windows of each strip of the layout `doc` holds, by the strip's name.
fn strips_in(doc: &automerge::AutoCommit) -> Vec<(String, BTreeSet<u64>)> {
    let layout = shared_resource::read(doc, None).expect("a layout");
    let workspace = layout.tree.workspaces.first().expect("the workspace");
    let strips = workspace.strips.iter();
    strips
        .map(|strip| {
            let tabs = strip.window_groups.iter().flat_map(|group| &group.tabs);
            (strip.name.clone(), tabs.map(|tab| tab.id).collect())
        })
        .collect()
}

/// Ada makes a strip with her window in it and Bob opens his in that strip: when she takes her
/// step back, her window goes and the strip stays with his.
#[test]
fn an_undo_never_takes_another_persons_window_from_the_strip() {
    let as_person = |person: &str, device: u8| {
        automerge::ActorId::from(ui::history_source::authors::device_actor(
            person.as_bytes(),
            &automerge::ActorId::from(vec![device; 32]),
        ))
    };
    let mut adas = tilo_desktop_model::IdMinter::new(7);
    let main = tilo_desktop_model::Strip::new(tilo_desktop_model::StripId(adas.mint()), "Main");
    let mut doc = automerge::AutoCommit::new().with_actor(as_person("ada", 1));
    shared_resource::found(&mut doc).expect("the record's maps stand");
    laid_out_as(
        &mut doc,
        vec![main.clone()],
        shared_resource::housekeeping_message("ada"),
    );
    let (made, hers) = strip_of(&mut adas, "Made");
    laid_out_as(
        &mut doc,
        vec![main.clone(), made.clone()],
        shared_resource::step_message("ada"),
    );

    let mut bobs = doc.fork().with_actor(as_person("bob", 2));
    let (his_strip, his) = strip_of(&mut tilo_desktop_model::IdMinter::new(9), "His");
    let mut shared = made;
    shared.window_groups.extend(his_strip.window_groups);
    laid_out_as(
        &mut bobs,
        vec![main, shared],
        shared_resource::step_message("bob"),
    );
    tilo_desktop_model::merge::merge_into(&mut doc, &bobs).expect("Bob's change merges");
    assert_eq!(
        strips_in(&doc),
        vec![
            ("Main".to_owned(), BTreeSet::new()),
            ("Made".to_owned(), BTreeSet::from([hers, his])),
        ]
    );

    let mut log = history(&mut doc);
    assert!(
        log.undo_as(&mut doc, Some(&user("ada")), None, None)
            .is_some(),
        "Ada has a step to take back"
    );
    doc.commit();
    assert_eq!(
        strips_in(&doc),
        vec![
            ("Main".to_owned(), BTreeSet::new()),
            ("Made".to_owned(), BTreeSet::from([his])),
        ],
        "her window goes, his stays in the strip"
    );
}

/// A window this device opened before it read the workspace's layout stays on its layout when the
/// layout is laid out, and when another member's change arrives before this device wrote it.
#[test]
fn a_window_opened_before_the_layout_is_read_stays() {
    let (here, there) = (7, 9);
    let mut theirs = tilo_desktop_model::IdMinter::new(there);
    let (chat, sent) = strip_of(&mut theirs, "Chat");
    let mut doc = automerge::AutoCommit::new();
    lay_out(&mut doc, vec![chat.clone()]);

    let mut ours = tilo_desktop_model::IdMinter::new(here);
    let (notes, opened) = strip_of(&mut ours, "Notes");
    let mut layout = Workspace::new(workspace_id(), "Layout");
    layout.strips = vec![notes];
    let desktops = Layouts::default();
    let fold = |layout: &mut Workspace, doc: &automerge::AutoCommit| {
        let record = shared_resource::read(doc, None).expect("the layout");
        desktops.fold_into(workspace_id(), doc, &record.tree, layout, here, "");
    };
    fold(&mut layout, &doc);
    assert_eq!(
        windows_of(&layout),
        BTreeSet::from([sent, opened]),
        "the layout's window, and the one opened here before it was read"
    );

    let (files, later) = strip_of(&mut theirs, "Files");
    lay_out(&mut doc, vec![chat, files]);
    fold(&mut layout, &doc);
    assert_eq!(
        windows_of(&layout),
        BTreeSet::from([sent, later, opened]),
        "another member's later change takes nothing of this layout's away"
    );
}

/// A write of the layout under way holds the session open.
#[test]
fn a_layout_write_under_way_holds_the_session_open() {
    let desktops = Layouts::default();
    let key = (workspace_id(), Route::layout());
    desktops.held(&key, |held| {
        held.read = true;
        held.writing = Some(Going::View(Box::new(Publishing {
            writing: crate::engine_write::Writing::write(
                std::sync::Arc::new(|| {}) as tilo_service::Wake,
                async { std::future::pending::<crate::engine_write::Written>().await },
            ),
            candidate: automerge::AutoCommit::new(),
            replaced: None,
            revision: None,
        })));
    });
    assert!(
        !desktops.settle_publishing(),
        "a write that has not answered leaves the session unsettled"
    );
    assert!(
        desktops.held(&key, |held| held.writing.is_some()),
        "the write is still under way"
    );
}

/// A record a look skipped while its write was under way, or whose write finished since, is
/// read again: what reached it meanwhile moved the engine's revision before the skip, so only
/// the record's own read state says it is behind.
#[test]
fn a_record_skipped_for_its_write_is_read_once_the_write_is_done() {
    let workspace = workspace_id();
    let tree = Layout {
        workspaces: vec![Workspace::new(workspace, "Layout")],
        active: workspace,
        ..Layout::default()
    };
    let (wake, heard) = woken();
    let desktops = Layouts {
        wake,
        ..Layouts::default()
    };
    let key = (workspace, Route::layout());
    let at = |revision| EngineVersion { revision, keys: 0 };
    let read_at = |revision| {
        desktops.held(&key, |held| {
            held.read = true;
            held.read_at = Some(at(revision));
        });
    };
    assert!(
        !desktops.settled(at(7), &tree),
        "the first look reads the records"
    );
    read_at(7);
    assert!(desktops.settled(at(7), &tree), "nothing moved since");
    // Another device's write moves the record, and this look starts a write of its own, which
    // it skips reading the record for.
    assert!(!desktops.settled(at(8), &tree), "the record moved");
    let (answer, answered) = tokio::sync::oneshot::channel::<()>();
    let wake = desktops.wake.handle();
    desktops.held(&key, |held| {
        held.writing = Some(Going::View(Box::new(Publishing {
            writing: crate::engine_write::Writing::write(wake, async {
                let _ = answered.await;
                Err(tilo_peer_state::owner::WriteRefusal::Refused(
                    "refused".to_owned(),
                ))
            }),
            candidate: automerge::AutoCommit::new(),
            replaced: None,
            revision: None,
        })));
    });
    assert!(!desktops.settle_publishing(), "the write is under way");
    let _ = answer.send(());
    heard
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the write wakes the loop once it is done");
    assert!(
        !desktops.settled(at(8), &tree),
        "the look after the write reads the record again"
    );
    read_at(8);
    assert!(
        desktops.settled(at(8), &tree),
        "and the one after that has nothing to read"
    );
    // Keys that arrive open what the record held sealed, though the engine's copy did not move.
    assert!(
        !desktops.settled(
            EngineVersion {
                revision: 8,
                keys: 1
            },
            &tree
        ),
        "a record read before its keys came is read again"
    );
}

/// A key whose signatures wait for the test to let them go: a write that signs through it is
/// under way until then, whatever the machine's speed.
#[derive(Default)]
struct Hold {
    held: std::sync::atomic::AtomicBool,
    waiting: std::sync::Mutex<Vec<std::task::Waker>>,
}

impl Hold {
    fn held(&self) -> bool {
        self.held.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn hold(&self) {
        self.held.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn open(&self) {
        self.held.store(false, std::sync::atomic::Ordering::SeqCst);
        let waiting = std::mem::take(&mut *self.waiting.lock().expect("the hold"));
        for waker in waiting {
            waker.wake();
        }
    }
}

/// A device key signing behind a [`Hold`].
struct HeldSigner {
    key: ed25519_dalek::SigningKey,
    hold: Arc<Hold>,
}

impl collab_types::signer::DeviceSigner for HeldSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn endpoint_secret(&self) -> Option<[u8; 32]> {
        Some(self.key.to_bytes())
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.key, &message).to_bytes();
        let hold = Arc::clone(&self.hold);
        Box::pin(std::future::poll_fn(move |context| {
            if hold.held() {
                hold.waiting
                    .lock()
                    .expect("the hold")
                    .push(context.waker().clone());
                if hold.held() {
                    return std::task::Poll::Pending;
                }
            }
            std::task::Poll::Ready(Ok(signature))
        }))
    }
}

/// A device's workspace service as these tests stand it in: every write waits on `hold` and
/// answers as the workspace took it. A layout's write reaches its workspace through the service.
struct HeldWrites {
    hold: Arc<Hold>,
    person: String,
}

impl tilo_workspace_service::workspace_documents::DeviceWrites for HeldWrites {
    fn write(
        &self,
        workspace: &str,
        app: &str,
        document: &str,
        _changes: &[u8],
        _founding: bool,
        _sealed: Option<&key_group::ContentKeys>,
    ) -> tilo_workspace_service::workspace_documents::DocumentWrite {
        let hold = Arc::clone(&self.hold);
        let person = self.person.clone();
        let (workspace, app, document) =
            (workspace.to_owned(), app.to_owned(), document.to_owned());
        Box::pin(async move {
            std::future::poll_fn(move |context| {
                if hold.held() {
                    hold.waiting
                        .lock()
                        .expect("the hold")
                        .push(context.waker().clone());
                    if hold.held() {
                        return std::task::Poll::Pending;
                    }
                }
                std::task::Poll::Ready(())
            })
            .await;
            Ok(write_of(&workspace, &app, &document, &person))
        })
    }
}

/// The write a workspace took, as this device's apps hear it: what the stand-in service answers.
fn write_of(
    workspace: &str,
    app: &str,
    document: &str,
    person: &str,
) -> tilo_peer_state::owner::AppWrite {
    tilo_peer_state::owner::AppWrite {
        peer: 1,
        app_id: app.to_owned(),
        cluster_id: workspace.to_owned(),
        document_id: document.to_owned(),
        author: tilo_action_policy::MemberAccess::person(
            tilo_action_policy::WorkspacePolicy::new(workspace, person),
            person.to_owned(),
        ),
        replayed: false,
        envelope: collab_types::SignedOp::new_unsigned(
            Vec::new(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpKind::Change,
            std::time::SystemTime::now(),
            0,
        ),
        own: None,
    }
}

/// The wake that says a record's write is done, as a channel.
fn woken() -> (tilo_service::LoopWake, std::sync::mpsc::Receiver<()>) {
    struct Rings(std::sync::Mutex<std::sync::mpsc::Sender<()>>);
    impl std::task::Wake for Rings {
        fn wake(self: std::sync::Arc<Self>) {
            let _ = self.0.lock().map(|rings| rings.send(()));
        }
    }
    let (rings, heard) = std::sync::mpsc::channel();
    let wake = tilo_service::LoopWake::default();
    wake.bind(std::task::Waker::from(std::sync::Arc::new(Rings(
        std::sync::Mutex::new(rings),
    ))));
    (wake, heard)
}

/// A layout save runs off the loop: folding the record into the layout, committing this device's
/// change and the workspace's answer are the task's; the look only hands the save over.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one save from the look that hands it over to the record the task wrote"
)]
fn a_layout_save_is_made_and_written_off_the_loop() {
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{Strip, StripId, WindowGroup, WindowWidth};

    use crate::engine::Engine;

    let state = tempfile::tempdir().expect("a state directory");
    let root = SigningKey::from_bytes(&[0x30; 32]);
    let key = SigningKey::from_bytes(&[0x31; 32]);
    let signing = key.clone();
    let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
    let hold = Arc::new(Hold::default());
    let theirs = Arc::clone(&hold);
    let engine = Arc::new(
        Engine::open(
            tilo_identity::identity_store::DeviceIdentity::kept(
                cert.clone(),
                HeldSigner { key, hold: theirs },
            ),
            Some(state.path()),
            std::iter::empty(),
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this device's engine opens"),
    );
    let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
    let person = hex::encode(cert.user_public_key);
    let workspace = tilo_peer_state::owner::testing::founded(engine.owner(), &cert, &signing)
        .expect("the layout's workspace is founded");
    engine.bind(workspace);
    assert!(
        engine.holds(workspace),
        "the layout's workspace is held here"
    );

    // The layout: one workspace with one window of this device's.
    let mut ids = tilo_desktop_model::IdMinter::new(salt);
    let (strip, group, tab) = (ids.mint(), ids.mint(), ids.mint());
    let mut workspace_view = Workspace::new(workspace, "Layout");
    let mut strip_view = Strip::new(StripId(strip), "Main");
    strip_view.window_groups.push(WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: "Notes".to_owned(),
            tile: tilo_desktop_model::TileContent::Stub {
                kind: tilo_desktop_model::AppKind::Other("Notes".to_owned()),
                label: "Notes".to_owned(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: WindowWidth::Wide,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace_view.strips = vec![strip_view];
    let tree = Layout {
        workspaces: vec![workspace_view],
        active: workspace,
        salt,
        ..Layout::default()
    };

    let (wake, heard) = woken();
    let desktops = Layouts::new(
        Arc::new(crate::continuations::Continuations::default()),
        wake,
        person.clone(),
        None,
    );
    let writes = Arc::new(HeldWrites {
        hold: Arc::clone(&hold),
        person: person.clone(),
    }) as Arc<dyn tilo_workspace_service::workspace_documents::DeviceWrites>;
    let reads: Arc<dyn tilo_workspace_service::workspace_documents::DeviceDocuments> = Arc::new(
        crate::records::testing::EngineDocuments(Arc::clone(&engine)),
    );
    desktops.attach(&engine, &reads, &writes);
    // The workspace's keys, as the key service hands them to this device.
    let keys = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("owner").expect("a member key"),
    )
    .expect("the workspace's group")
    .content_keys()
    .expect("the workspace's keys");
    engine.hand_keys(workspace, keys);
    let layout = (workspace, Route::layout());
    // The write's signature waits for the test: it is under way, so nothing of it can have been
    // taken in here.
    hold.hold();
    desktops.capture(&engine, &tree);
    assert!(
        desktops.held(&layout, |held| matches!(
            held.writing,
            Some(Going::Layout(_))
        )),
        "the look hands the save to the task that makes it"
    );
    assert!(
        desktops.held(&layout, |held| held.written.is_none()),
        "the loop decides nothing of what the save came to"
    );
    assert!(
        desktops.held(&layout, |held| held.kept.is_none() && held.undo.is_none()),
        "the loop folds no record and begins no history"
    );
    assert!(
        desktops.reached(&layout).0.is_none(),
        "the loop writes nothing into the workspace"
    );
    assert!(
        !desktops.settle_publishing(),
        "the save is the task's, and has not answered yet"
    );

    // The task answers, and the change it wrote is one step of the person's in the layout's
    // history, on any of their devices.
    hold.open();
    heard
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the write wakes the loop once it is done");
    assert!(desktops.settle_publishing(), "the write is done");
    assert!(
        desktops.held(&layout, |held| held.written.is_some()),
        "the look takes what the write came to"
    );
    let record = desktops
        .layout(workspace)
        .expect("the record this device holds");
    assert_eq!(
        shared_resource::read(&record, None).map(|layout| windows_of(
            layout
                .tree
                .workspaces
                .first()
                .expect("the record's workspace")
        )),
        Some(std::collections::BTreeSet::from([tab])),
        "the record holds the window this layout shows"
    );
    assert!(
        desktops.last_step(workspace, &person).is_some(),
        "the change the task wrote is one step of the person's"
    );

    // A change can reach the record while this device's next write is under way: the look that handed the write noted the engine's revision, and its follow skipped the
    // record. The write's answer need not move the revision again (one that finds nothing to
    // write does not), and the look after it still reads the record.
    let mut tree = tree;
    desktops.follow(&engine, "", &mut tree);
    tree.external_revision += 1;
    let strip = tree.workspaces.iter_mut().flat_map(|held| &mut held.strips);
    strip
        .take(1)
        .for_each(|strip| strip.name = "Moved".to_owned());
    let version = EngineVersion::of(&engine);
    let _ = desktops.settled(version, &tree);
    hold.hold();
    desktops.capture(&engine, &tree);
    desktops.follow(&engine, "", &mut tree.clone());
    hold.open();
    heard
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the write wakes the loop once it is done");
    assert!(
        !desktops.settled(version, &tree),
        "the look after a write reads the record its follow skipped"
    );
}

/// What the person changed of a workspace's layout and of their view of it that a look could not
/// write is kept on the device, and the session after writes both as their steps.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one session that cannot write, the one after it, and what each of them holds"
)]
fn what_a_session_ends_before_it_can_write_is_kept_for_the_next_one() {
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    use crate::engine::Engine;

    let state = tempfile::tempdir().expect("a state directory");
    let root = SigningKey::from_bytes(&[0x40; 32]);
    let key = SigningKey::from_bytes(&[0x41; 32]);
    let signing = key.clone();
    let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
    let engine = Arc::new(
        Engine::open(
            tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key),
            Some(state.path()),
            std::iter::empty(),
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this device's engine opens"),
    );
    let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
    let person = hex::encode(cert.user_public_key);
    let workspace = tilo_peer_state::owner::testing::founded(engine.owner(), &cert, &signing)
        .expect("the workspace is founded");
    engine.bind(workspace);

    // One workspace, one strip, one group of two windows: the person brings the second to the
    // front.
    let mut ids = tilo_desktop_model::IdMinter::new(salt);
    let (strip, group, first, second) = (ids.mint(), ids.mint(), ids.mint(), ids.mint());
    let window = |id: u64, title: &str| Tab {
        id,
        title: title.to_owned(),
        tile: TileContent::Stub {
            kind: tilo_desktop_model::AppKind::Other(title.to_owned()),
            label: title.to_owned(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut workspace_layout = Workspace::new(workspace, "Layout");
    workspace_layout.strips = vec![Strip {
        id: StripId(strip),
        name: "Main".to_owned(),
        window_groups: vec![WindowGroup {
            id: group,
            tabs: vec![window(first, "Notes"), window(second, "Mail")],
            active_tab: first,
            width: WindowWidth::Wide,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut layout = Layout {
        workspaces: vec![workspace_layout],
        active: workspace,
        salt,
        ..Layout::default()
    };
    layout.active_strip.insert(workspace, StripId(strip));
    layout.set_active_group(StripId(strip), group);
    // The person brought the second window to the front.
    layout.workspaces[0].strips[0].window_groups[0].active_tab = second;

    let notes = Arc::new(crate::continuations::Continuations::default());
    let (view_key, layout_key) = (
        (workspace, Route::view(&person)),
        (workspace, Route::layout()),
    );
    let (wake, _heard) = woken();
    let desktops = Layouts::new(Arc::clone(&notes), wake, person.clone(), Some(state.path()));
    let writes: Arc<dyn tilo_workspace_service::workspace_documents::DeviceWrites> =
        Arc::new(crate::records::testing::EngineWrites(Arc::clone(&engine)));
    let reads: Arc<dyn tilo_workspace_service::workspace_documents::DeviceDocuments> = Arc::new(
        crate::records::testing::EngineDocuments(Arc::clone(&engine)),
    );
    desktops.attach(&engine, &reads, &writes);
    let mut shown = layout.clone();
    desktops.follow(&engine, "", &mut shown);
    // What a look that cannot reach the workspace leaves behind: the layout the person made of
    // it, and the view they switched, each as a step of theirs.
    desktops.held(&layout_key, |held| {
        held.pending_steps.push(
            LayoutStep {
                workspace: shown.workspaces[0].clone(),
                step: shared_resource::step_message(&person),
            },
            None,
        );
    });
    desktops.held(&view_key, |held| {
        held.pending_view
            .push(PersonView::of(&shown, workspace), None);
    });
    desktops.keep_pending();
    drop(desktops);
    tilo_durable_writer::global().flush();

    // The session after reads what the one before could not write, and writes it once the keys
    // are here.
    let (wake, _heard) = woken();
    let next = Layouts::new(Arc::clone(&notes), wake, person, Some(state.path()));
    let writes: Arc<dyn tilo_workspace_service::workspace_documents::DeviceWrites> =
        Arc::new(crate::records::testing::EngineWrites(Arc::clone(&engine)));
    let reads: Arc<dyn tilo_workspace_service::workspace_documents::DeviceDocuments> = Arc::new(
        crate::records::testing::EngineDocuments(Arc::clone(&engine)),
    );
    next.attach(&engine, &reads, &writes);
    assert!(
        !next.held(&view_key, |held| held.pending_view.is_empty()),
        "the view is back"
    );
    assert!(
        !next.held(&layout_key, |held| held.pending_steps.is_empty()),
        "and so is the layout"
    );
    let keys = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("owner").expect("a member key"),
    )
    .expect("the workspace's group")
    .content_keys()
    .expect("the workspace's keys");
    engine.hand_keys(workspace, keys);
    let mut shown = layout.clone();
    next.follow(&engine, "", &mut shown);
    next.capture(&engine, &shown);
    next.finish_writes(&engine, &shown, std::time::Duration::from_secs(30));
    let held = next
        .held(&view_key, |held| held.kept.clone())
        .expect("the view this device wrote");
    assert_eq!(
        PersonView::read(&held).front_tab.get(&group),
        Some(&second),
        "the window the person brought to the front is in the workspace"
    );
    assert!(
        next.held(&view_key, |held| held.pending_view.is_empty())
            && next.held(&layout_key, |held| held.pending_steps.is_empty()),
        "nothing of it waits any more"
    );
}

/// The name of each strip of a workspace, and its groups with the windows in them.
type Strips = Vec<(String, Vec<(u64, Vec<u64>)>)>;

/// The strips of `workspace`, each with its groups and their windows: what a session shows.
fn strips_of(workspace: &Workspace) -> Strips {
    workspace
        .strips
        .iter()
        .map(|strip| {
            (
                strip.name.clone(),
                strip
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                    .collect(),
            )
        })
        .collect()
}

/// A session founds its workspace from the shell it shows and ends before that write can reach it,
/// with a step of the person's kept beside it; the session after shows the whole shell: both
/// strips, every group, and the window of the person's own the workspace's layout never holds.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the shell a session founds, the step it keeps, and what the session after shows"
)]
fn a_whole_shell_a_session_founds_is_shown_by_the_next_one() {
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    use crate::engine::Engine;

    let state = tempfile::tempdir().expect("a state directory");
    let root = SigningKey::from_bytes(&[0x50; 32]);
    let key = SigningKey::from_bytes(&[0x51; 32]);
    let signing = key.clone();
    let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
    let engine = Arc::new(
        Engine::open(
            tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key),
            Some(state.path()),
            std::iter::empty(),
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this device's engine opens"),
    );
    let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
    let person = hex::encode(cert.user_public_key);
    let workspace = tilo_peer_state::owner::testing::founded(engine.owner(), &cert, &signing)
        .expect("the workspace is founded");
    engine.bind(workspace);
    let keys = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("owner").expect("a member key"),
    )
    .expect("the workspace's group")
    .content_keys()
    .expect("the workspace's keys");
    engine.hand_keys(workspace, keys);

    // The shell the person shows: a strip of two groups, and a strip of one window of their own.
    let mut ids = tilo_desktop_model::IdMinter::new(salt);
    let window = |id: u64, title: &str, app: &str| Tab {
        id,
        title: title.to_owned(),
        tile: TileContent::LocalApp {
            app_id: app.to_owned(),
            label: title.to_owned(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut strip_of = |title: &str, groups: Vec<Vec<(&str, &str)>>| {
        let strip = ids.mint();
        Strip {
            id: StripId(strip),
            name: title.to_owned(),
            window_groups: groups
                .into_iter()
                .map(|tabs| {
                    let group = ids.mint();
                    let tabs: Vec<Tab> = tabs
                        .into_iter()
                        .map(|(label, app)| window(ids.mint(), label, app))
                        .collect();
                    WindowGroup {
                        id: group,
                        active_tab: tabs[0].id,
                        tabs,
                        width: WindowWidth::Normal,
                        maximized: false,
                        focus_history: Vec::new(),
                    }
                })
                .collect(),
        }
    };
    let writing = strip_of(
        "Writing",
        vec![
            vec![("Notes", "so.tilo.notes"), ("Build", "so.tilo.build")],
            vec![("Files", "so.tilo.files")],
        ],
    );
    let reference = strip_of("Reference", vec![vec![("Mail", "so.tilo.mail")]]);
    let mut workspace_layout = Workspace::new(workspace, "Home");
    workspace_layout.strips = vec![writing, reference];
    let mut layout = Layout {
        workspaces: vec![workspace_layout],
        active: workspace,
        salt,
        ..Layout::default()
    };
    let in_front = layout.workspaces[0].strips[0].id;
    layout.active_strip.insert(workspace, in_front);
    layout.set_active_group(in_front, layout.workspaces[0].strips[0].window_groups[0].id);

    let notes = Arc::new(crate::continuations::Continuations::default());
    let layout_key = (workspace, Route::layout());
    let (wake, _heard) = woken();
    let before = Layouts::new(Arc::clone(&notes), wake, person.clone(), Some(state.path()));
    attach_through_engine(&before, &engine);
    let mut shown = layout.clone();
    before.follow(&engine, "", &mut shown);
    // The step the session keeps while its workspace cannot hold the layout yet: the whole shell.
    before.held(&layout_key, |held| {
        held.pending_steps.push(
            LayoutStep {
                workspace: shown.workspaces[0].clone(),
                step: shared_resource::step_message(&person),
            },
            None,
        );
    });
    before.capture(&engine, &shown);
    before.finish_writes(&engine, &shown, std::time::Duration::from_secs(30));
    drop(before);
    tilo_durable_writer::global().flush();

    // The session after opens on the workspace and folds its layout and view in: the shell as it
    // was.
    let (wake, _heard) = woken();
    let next = Layouts::new(Arc::clone(&notes), wake, person, Some(state.path()));
    attach_through_engine(&next, &engine);
    let mut reopened = Layout {
        workspaces: vec![Workspace::new(workspace, "Home")],
        active: workspace,
        salt,
        ..Layout::default()
    };
    next.follow(&engine, "", &mut reopened);
    let strips = strips_of(&reopened.workspaces[0]);
    let wanted = strips_of(&layout.workspaces[0]);
    assert_eq!(strips, wanted, "every strip, group and window is back");

    // The layout written and folded again: the window of the person's own is theirs, and the
    // workspace's layout it is not in never takes it away.
    next.capture(&engine, &reopened);
    next.finish_writes(&engine, &reopened, std::time::Duration::from_secs(30));
    next.follow(&engine, "", &mut reopened);
    let strips = strips_of(&reopened.workspaces[0]);
    assert_eq!(
        strips, wanted,
        "and stays after the layout is written again"
    );
}

/// A view whose write does not answer before the session ends is kept for the session after, the
/// person's own windows with it: a desktop that ends cannot wait for its last write.
#[test]
fn a_view_the_session_cannot_write_before_it_ends_is_kept() {
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    use crate::engine::Engine;

    let state = tempfile::tempdir().expect("a state directory");
    let root = SigningKey::from_bytes(&[0x60; 32]);
    let key = SigningKey::from_bytes(&[0x61; 32]);
    let signing = key.clone();
    let hold = Arc::new(Hold::default());
    let theirs = Arc::clone(&hold);
    let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
    let engine = Arc::new(
        Engine::open(
            tilo_identity::identity_store::DeviceIdentity::kept(
                cert.clone(),
                HeldSigner { key, hold: theirs },
            ),
            Some(state.path()),
            std::iter::empty(),
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this device's engine opens"),
    );
    let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
    let person = hex::encode(cert.user_public_key);
    let workspace = tilo_peer_state::owner::testing::founded(engine.owner(), &cert, &signing)
        .expect("the workspace is founded");
    engine.bind(workspace);
    let keys = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("owner").expect("a member key"),
    )
    .expect("the workspace's group")
    .content_keys()
    .expect("the workspace's keys");
    engine.hand_keys(workspace, keys);

    // One strip of one window of the person's own: what the workspace's layout never holds.
    let mut ids = tilo_desktop_model::IdMinter::new(salt);
    let (strip, group, tab) = (ids.mint(), ids.mint(), ids.mint());
    let window = Tab {
        id: tab,
        title: "Settings".to_owned(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.settings".to_owned(),
            label: "Settings".to_owned(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut workspace_layout = Workspace::new(workspace, "Home");
    workspace_layout.strips = vec![Strip {
        id: StripId(strip),
        name: "Main".to_owned(),
        window_groups: vec![WindowGroup {
            id: group,
            tabs: vec![window],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut layout = Layout {
        workspaces: vec![workspace_layout],
        active: workspace,
        salt,
        ..Layout::default()
    };
    layout.active_strip.insert(workspace, StripId(strip));

    let (wake, _heard) = woken();
    let desktops = Layouts::new(
        Arc::new(crate::continuations::Continuations::default()),
        wake,
        person.clone(),
        Some(state.path()),
    );
    attach_through_engine(&desktops, &engine);
    let mut shown = layout.clone();
    desktops.follow(&engine, "", &mut shown);
    let view_key = (workspace, Route::view(&person));
    let own = || {
        desktops.held(&view_key, |held| {
            held.pending_view
                .last()
                .map(|step| step.changed.own.keys().copied().collect::<Vec<_>>())
        })
    };
    assert_eq!(own(), None, "nothing of the view waits yet");

    // The write of the view signs behind a hold the test never opens: it does not answer while
    // the session ends, and the view it held is kept for the session after.
    hold.hold();
    desktops.capture(&engine, &shown);
    desktops.finish_writes(&engine, &shown, std::time::Duration::from_millis(1));
    assert_eq!(
        own(),
        Some(vec![tab]),
        "the view, the person's own window with it, waits for the session after"
    );
    hold.open();
}

/// Attach `layouts` to read and write the workspaces' documents through `engine`'s own copy, as the
/// device's workspace service holds them in a session.
fn attach_through_engine(layouts: &Layouts, engine: &Arc<crate::engine::Engine>) {
    let writes: Arc<dyn tilo_workspace_service::workspace_documents::DeviceWrites> =
        Arc::new(crate::records::testing::EngineWrites(Arc::clone(engine)));
    let reads: Arc<dyn tilo_workspace_service::workspace_documents::DeviceDocuments> =
        Arc::new(crate::records::testing::EngineDocuments(Arc::clone(engine)));
    layouts.attach(engine, &reads, &writes);
}

/// A layout whose write does not answer before the session ends is kept for the session after,
/// which shows it and writes it: a desktop that ends cannot wait for its last write.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the session that cannot write the layout, and the one after it that does"
)]
fn a_layout_the_session_cannot_write_before_it_ends_is_kept() {
    use ed25519_dalek::SigningKey;
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};

    use crate::engine::Engine;

    let state = tempfile::tempdir().expect("a state directory");
    let root = SigningKey::from_bytes(&[0x70; 32]);
    let key = SigningKey::from_bytes(&[0x71; 32]);
    let signing = key.clone();
    let hold = Arc::new(Hold::default());
    let theirs = Arc::clone(&hold);
    let cert = tilo_peer_state::owner::testing::certificate(&root, &key, "Laptop");
    let engine = Arc::new(
        Engine::open(
            tilo_identity::identity_store::DeviceIdentity::kept(
                cert.clone(),
                HeldSigner { key, hold: theirs },
            ),
            Some(state.path()),
            std::iter::empty(),
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this device's engine opens"),
    );
    let salt = tilo_desktop_model::ids::device_salt_of_key(&cert.device_public_key);
    let person = hex::encode(cert.user_public_key);
    let workspace = tilo_peer_state::owner::testing::founded(engine.owner(), &cert, &signing)
        .expect("the workspace is founded");
    engine.bind(workspace);
    let keys = key_group::KeyGroup::create(
        workspace,
        key_group::MemberKey::generate("owner").expect("a member key"),
    )
    .expect("the workspace's group")
    .content_keys()
    .expect("the workspace's keys");
    engine.hand_keys(workspace, keys);

    // One strip of one window: the person names the strip.
    let mut ids = tilo_desktop_model::IdMinter::new(salt);
    let (strip, group, tab) = (ids.mint(), ids.mint(), ids.mint());
    let window = Tab {
        id: tab,
        title: "Notes".to_owned(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let mut workspace_layout = Workspace::new(workspace, "Home");
    workspace_layout.strips = vec![Strip {
        id: StripId(strip),
        name: "Main".to_owned(),
        window_groups: vec![WindowGroup {
            id: group,
            tabs: vec![window],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let mut layout = Layout {
        workspaces: vec![workspace_layout],
        active: workspace,
        salt,
        ..Layout::default()
    };
    layout.active_strip.insert(workspace, StripId(strip));

    let (wake, heard) = woken();
    let desktops = Layouts::new(
        Arc::new(crate::continuations::Continuations::default()),
        wake,
        person.clone(),
        Some(state.path()),
    );
    attach_through_engine(&desktops, &engine);
    let mut shown = layout.clone();
    desktops.follow(&engine, "", &mut shown);
    let layout_key = (workspace, Route::layout());
    assert!(
        desktops.held(&layout_key, |held| held.pending_steps.is_empty()),
        "nothing of the layout waits yet"
    );

    // The write of the renamed strip signs behind a hold the test never opens: it does not answer
    // while the session ends, and the step it held is kept for the session after.
    hold.hold();
    shown.workspaces[0].strips[0].name = "Moved".to_owned();
    desktops.capture(&engine, &shown);
    desktops.wait_writes(std::time::Duration::from_millis(1));
    assert_eq!(
        desktops.held(&layout_key, |held| {
            held.pending_steps
                .last()
                .map(|step| strips_of(&step.changed.workspace))
        }),
        Some(strips_of(&shown.workspaces[0])),
        "the layout, the renamed strip with it, waits for the session after"
    );
    drop(desktops);
    tilo_durable_writer::global().flush();
    // The write the session gave up on finishes now the hold is open: the next session starts from
    // the copy it left, not from a write still under way.
    hold.open();
    heard
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the write wakes the loop once it is done");

    // The session after reads what the one before could not write, shows it, and writes it.
    let (wake, _heard) = woken();
    let next = Layouts::new(
        Arc::new(crate::continuations::Continuations::default()),
        wake,
        person,
        Some(state.path()),
    );
    attach_through_engine(&next, &engine);
    assert!(
        !next.held(&layout_key, |held| held.pending_steps.is_empty()),
        "the layout is back"
    );
    let mut reopened = Layout {
        workspaces: vec![Workspace::new(workspace, "Home")],
        active: workspace,
        salt,
        ..Layout::default()
    };
    next.follow(&engine, "", &mut reopened);
    assert_eq!(
        strips_of(&reopened.workspaces[0]),
        strips_of(&shown.workspaces[0]),
        "the session after shows the renamed strip"
    );
    next.capture(&engine, &reopened);
    next.finish_writes(&engine, &reopened, std::time::Duration::from_secs(30));
    assert!(
        next.held(&layout_key, |held| held.pending_steps.is_empty()),
        "and writes it"
    );
    let written = next
        .held(&layout_key, |held| held.kept.clone())
        .expect("the layout this device wrote");
    let kept = shared_resource::read(&written, None).expect("the layout reads");
    let shown_then = kept
        .tree
        .workspaces
        .first()
        .expect("the layout's workspace");
    assert_eq!(
        strips_of(shown_then),
        strips_of(&shown.workspaces[0]),
        "the workspace's layout holds the renamed strip"
    );
}

use super::*;
use automerge::transaction::Transactable;
fn history_document(tree: &DesktopTree) -> AutoCommit {
    let mut doc = AutoCommit::new();
    if let Some(workspace) = tree.active_workspace() {
        doc.put(automerge::ROOT, "room", workspace.room.to_string())
            .unwrap();
        doc.put(automerge::ROOT, "name", &workspace.name).unwrap();
        tilo_desktop_model::projection::project_owned_contents(
            &mut doc,
            &workspace.strips,
            5,
            None,
            tilo_desktop_model::projection::EmptyStrips::Skip,
        )
        .unwrap();
        doc.commit();
    }
    doc
}
fn fixture_with_topology(
    configure: impl FnOnce(&mut DesktopTree),
) -> (DesktopRoot, DesktopHandle, PortableStorage) {
    fixture_with_extra_calculators(0, configure)
}
fn fixture_with_extra_calculators(
    extra_calculators: usize,
    configure: impl FnOnce(&mut DesktopTree),
) -> (DesktopRoot, DesktopHandle, PortableStorage) {
    let cache = PortableStorage::new("desktop-host-test", "device").unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let mut tree = DesktopTree::default();
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    let room = workspace.room.to_string();
    let mut tabs = vec![];
    let mut routes = BTreeMap::new();
    // Keep the real initialized stores alive until their checked components take custody.
    let mut retained = vec![];
    let fixture_apps = [
        "so.tilo.word-processor",
        "so.tilo.chat",
        "so.tilo.notes",
        "so.tilo.calculator",
    ]
    .map(|app| desktop::definition(app).expect("fixture app is installed"));
    for (i, definition) in fixture_apps.into_iter().enumerate() {
        let app_id = definition.app_id;
        let id = (5 << 48) | ((i as u64) + 10);
        let record = if i == 0 {
            RecordCheckout::new(
                ui::record::RecordId::for_tab(1),
                definition.record_kind.clone(),
            )
        } else {
            tilo_desktop::records::component_record(app_id, id, definition.record_kind.clone())
        };
        let store = owners.open(app_id, &room).unwrap();
        store.create_record(&record).unwrap();
        store.with(&record.id, |doc| match i {
            0 => word_processor::initialize_empty_document(doc).unwrap(),
            1 => chat::doc::ChatDoc::seed_starter(doc, "author", "Author", "AU"),
            2 => notes::notebook::initialize_empty(doc).unwrap(),
            _ => calculator::doc::initialize_empty(doc).unwrap(),
        });
        if i == 0 {
            routes.insert(id, record.id.as_str().into());
        }
        tabs.push(Tab {
            id,
            title: definition.title.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(app_id.into()),
                label: definition.title.into(),
            },
        });
        retained.push(store);
    }
    let calculator_definition = desktop::definition("so.tilo.calculator")
        .expect("Calculator is installed in the production desktop catalogue");
    let calculator_app = calculator_definition.app_id;
    for extra in 0..extra_calculators {
        let id = (5 << 48) | (14 + extra as u64);
        let record = tilo_desktop::records::component_record(
            calculator_app,
            id,
            calculator_definition.record_kind.clone(),
        );
        let store = owners.open(calculator_app, &room).unwrap();
        store.create_record(&record).unwrap();
        store.with(&record.id, |doc| {
            calculator::doc::initialize_empty(doc).unwrap();
        });
        tabs.push(Tab {
            id,
            title: calculator_definition.title.into(),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(calculator_app.into()),
                label: calculator_definition.title.into(),
            },
        });
        retained.push(store);
    }
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 3,
            active_tab: tabs[0].id,
            tabs,
            width: WindowWidth::Normal,
            maximized: false,
        }],
    }];
    tree.active = workspace.id;
    tree.workspaces.push(workspace);
    configure(&mut tree);
    let permissions = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
        vec![],
    );
    let admission = Admission {
        admitted_room: history_document(&tree),
        source_heads: vec![[7; 32]],
        tree,
        routes,
        required: vec![],
        missing: vec![],
        unsupported: vec![],
        pending: 0,
    };
    let (root, handle) = DesktopRoot::new(
        DesktopSources {
            admission,
            storage: cache.clone(),
            permissions,
            content: None,
            local: None,
        },
        &room,
        "device",
        "author",
        1,
        Arc::new(|| {}),
    )
    .unwrap();
    assert!(
        handle.errors.borrow().is_empty(),
        "{:?}",
        handle.errors.borrow()
    );
    (root, handle, cache)
}

#[test]
fn workspace_activity_actions_are_local_and_select_empty_workspaces() {
    use tilo_desktop::activity::{ActivityView, view_actions::*};
    let (mut root, handle, _) = fixture();
    let other = WorkspaceId(u64::MAX - 1);
    handle
        .tree
        .borrow_mut()
        .workspaces
        .push(Workspace::new(other, "Work"));
    let actions = ui::resources::caller_view_resources(&root, &AutoCommit::new());
    assert!(actions.iter().any(|r| r.id.0 == RESOURCE_ID));
    let mut doc = AutoCommit::new();
    for view in [
        ActivityView::CatchUp,
        ActivityView::Explore,
        ActivityView::Replay,
    ] {
        let request = WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view,
        };
        let previous_view = root.activity.as_ref().map(|activity| activity.pane.view());
        root.start_operation(
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                OPERATION_TYPE,
                &serde_json::to_vec(&request).unwrap(),
            ),
            &mut doc,
            &automerge::ROOT,
        )
        .unwrap()
        .and_then(ui::widget::ResourceOperationStart::completed)
        .unwrap();
        assert_eq!(
            root.activity.as_ref().map(|activity| activity.pane.view()),
            previous_view,
            "operations must defer structural changes until the frame captures focus"
        );
        root.update(&mut AutoCommit::new(), &automerge::ROOT);
        assert_eq!(root.activity.as_ref().unwrap().pane.view(), view);
        assert!(!root.activity.as_ref().unwrap().pane.replay_is_playing());
        assert!(handle.session_requests.borrow().is_empty());
    }
    let remembered = LocalView::from_tree(&handle.tree.borrow());
    root.apply_view_request(WorkspaceViewRequest::Select { workspace: other })
        .unwrap();
    assert_eq!(handle.tree.borrow().active, other);
    assert!(root.activity.is_none());
    assert!(
        root.apply_view_request(WorkspaceViewRequest::Select {
            workspace: WorkspaceId(999)
        })
        .is_err()
    );
    assert_eq!(handle.tree.borrow().active, other);
    root.apply_view_request(WorkspaceViewRequest::Select {
        workspace: WorkspaceId(1),
    })
    .unwrap();
    let returned = LocalView::from_tree(&handle.tree.borrow());
    assert_eq!(returned.tabs, remembered.tabs);
    assert_eq!(returned.strips, remembered.strips);
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn workspace_activity_capture_never_settles_pending_edits_and_filters_stale_owners() {
    use crate::desktop_activity::capture_records;
    let (_root, handle, storage) = fixture();
    let owners = handle.records.borrow();
    let writer = owners
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap();
    let required = vec![desktop::Requirement {
        app_id: writer.app.clone(),
        record_id: writer.checkout.id.as_str().into(),
    }];
    writer.store.with(&writer.checkout.id, |doc| {
        doc.commit();
    });
    assert!(
        capture_records(&required, &owners, "author")[0]
            .document
            .is_some()
    );
    writer.store.with(&writer.checkout.id, |doc| {
        doc.put(automerge::ROOT, "pending-history-test", "unfinished")
            .unwrap();
    });
    let pending = writer
        .store
        .read(&writer.checkout.id, |doc| {
            (doc.pending_ops(), doc.base_heads().to_vec())
        })
        .unwrap();
    assert!(pending.0 > 0);
    let generation = writer.store.action_generation(&writer.checkout.id).unwrap();
    let writes = storage.pending_writes().len();
    assert!(
        capture_records(&required, &owners, "author")[0]
            .document
            .is_none()
    );
    assert_eq!(
        writer
            .store
            .read(&writer.checkout.id, |doc| (
                doc.pending_ops(),
                doc.base_heads().to_vec()
            ))
            .unwrap(),
        pending
    );
    assert_eq!(
        writer.store.action_generation(&writer.checkout.id).unwrap(),
        generation
    );
    assert_eq!(storage.pending_writes().len(), writes);
    assert!(
        capture_records(&[], &owners, "author").is_empty(),
        "obsolete owners cannot enter a newer admission"
    );
    let missing = vec![desktop::Requirement {
        app_id: writer.app.clone(),
        record_id: ui::record::RecordId::for_tab(98765).as_str().into(),
    }];
    assert!(
        capture_records(&missing, &owners, "author")[0]
            .document
            .is_none()
    );
}

#[test]
fn workspace_activity_modal_retains_desktop_and_closes_on_access_loss() {
    use activity::{ActivityView, WorkspaceViewRequest};
    let (mut root, handle, _) = fixture();
    let writer = root.surface.row(StripId(2)).unwrap().groups[0]
        .active_host()
        .unwrap() as *const _;
    root.apply_view_request(WorkspaceViewRequest::OpenActivity {
        workspace: WorkspaceId(1),
        view: ActivityView::Replay,
    })
    .unwrap();
    assert!(
        root.children()
            .iter()
            .all(|child| child.name() == "WorkspaceActivity")
    );
    assert!(root.activity.as_ref().unwrap().traps_focus());
    let mut boxed = Box::new(root);
    let root_ptr = std::ptr::from_mut(boxed.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        boxed,
        (1000, 700),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("Workspace activity")
    );
    // SAFETY: frame owns the stable boxed root; no harness operation
    // overlaps the temporary borrow used to find the real pane control.
    let play = unsafe {
        (&*root_ptr)
            .activity
            .as_ref()
            .unwrap()
            .pane
            .accessibility_snapshot()
            .children
            .into_iter()
            .find(|node| node.name.as_deref() == Some("Play replay"))
            .unwrap()
            .bounds
            .unwrap()
            .to_rect()
            .center()
    };
    frame.click_at(play.x, play.y);
    assert!(unsafe {
        (&*root_ptr)
            .activity
            .as_ref()
            .unwrap()
            .pane
            .replay_is_playing()
    });
    let before = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| {
                    (doc.base_heads().to_vec(), doc.pending_ops())
                })
                .unwrap()
        })
        .collect::<Vec<_>>();
    frame.type_text("must not reach the editor");
    assert_eq!(
        before,
        handle
            .records
            .borrow()
            .values()
            .map(|r| r
                .store
                .read(&r.checkout.id, |doc| (
                    doc.base_heads().to_vec(),
                    doc.pending_ops()
                ))
                .unwrap())
            .collect::<Vec<_>>()
    );
    frame.key("Escape");
    frame.relayout();
    assert!(frame.inspection_json().unwrap().contains("WordProcessor"));
    assert!(handle.session_requests.borrow().is_empty());
    // Permission updates retire the modal before another event can use it.
    let (mut root, handle, _) = fixture();
    assert!(unsafe { (&*root_ptr).activity.is_none() });
    assert_eq!(writer, unsafe {
        (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
            .active_host()
            .unwrap() as *const _
    });
    root.apply_view_request(WorkspaceViewRequest::OpenActivity {
        workspace: WorkspaceId(1),
        view: ActivityView::Explore,
    })
    .unwrap();
    *handle.permissions.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(None, "revoked"),
        vec![],
    );
    root.reconcile();
    assert!(root.activity.is_none());
    assert!(
        root.apply_view_request(WorkspaceViewRequest::OpenActivity {
            workspace: WorkspaceId(1),
            view: ActivityView::Explore
        })
        .is_err()
    );
}

#[test]
fn workspace_activity_admission_replaces_sources_and_reports_unsupported_coverage() {
    use activity::{ActivityView, WorkspaceViewRequest};
    let (mut root, handle, _) = fixture();
    let requirements = handle
        .records
        .borrow()
        .values()
        .map(|r| desktop::Requirement {
            app_id: r.app.clone(),
            record_id: if r.checkout.id.is_package_primary() {
                String::new()
            } else {
                r.checkout.id.as_str().into()
            },
        })
        .collect::<Vec<_>>();
    root.required_records = requirements.clone();
    root.apply_view_request(WorkspaceViewRequest::OpenActivity {
        workspace: WorkspaceId(1),
        view: ActivityView::Explore,
    })
    .unwrap();
    assert_eq!(
        root.activity
            .as_ref()
            .unwrap()
            .pane
            .feed_coverage()
            .referenced_records,
        4
    );
    let writer = requirements
        .into_iter()
        .find(|r| r.app_id == "so.tilo.word-processor")
        .unwrap();
    let mut source = root.admitted_room.clone();
    source
        .put(automerge::ROOT, "name", "Updated admission")
        .unwrap();
    source.commit();
    let heads = source.get_heads();
    let topology = handle.tree.borrow().clone();
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: source,
            source_heads: heads.iter().map(|head| head.0).collect(),
            tree: topology,
            routes: root.resolver.host.routes.clone(),
            required: vec![writer],
            missing: vec![],
            unsupported: vec!["unsupported.actual.app".into()],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    root.reconcile();
    assert_eq!(
        handle.records.borrow().len(),
        4,
        "old live owners deliberately remain mounted"
    );
    let activity = root.activity.as_mut().unwrap();
    assert_eq!(activity.snapshot.doc.get_heads(), heads);
    assert_eq!(activity.pane.feed_coverage().referenced_records, 1);
    assert_eq!(
        activity.pane.feed_coverage().excluded_apps,
        ["unsupported.actual.app"]
    );
    assert!(!activity.pane.feed_coverage().is_complete());
    assert_eq!(
        root.source_heads,
        heads.iter().map(|head| head.0).collect::<Vec<_>>()
    );
}

#[test]
fn workspace_activity_revoked_admission_and_resource_opening_use_current_authority() {
    use crate::desktop_activity::ActivityCloseReason;
    use activity::{ActivityView, WorkspaceViewRequest};
    let (mut root, handle, _) = fixture();
    let request = WorkspaceViewRequest::OpenActivity {
        workspace: WorkspaceId(1),
        view: ActivityView::Explore,
    };
    root.apply_view_request(request.clone()).unwrap();
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    let target = ui::HistoryResourceTarget {
        app_id: Some(writer.app.clone()),
        app_label: None,
        id: writer.checkout.id.as_str().into(),
        kind: writer.checkout.kind.as_str().into(),
        label: "untrusted label".into(),
    };
    root.activity.as_mut().unwrap().open_target = Some(ui::HistoryResourceTarget {
        app_id: Some("so.tilo.notes".into()),
        ..target.clone()
    });
    root.refresh_activity(false);
    assert!(
        root.activity.is_some(),
        "a record ID in the wrong app must not resolve"
    );
    root.activity.as_mut().unwrap().open_target = Some(target);
    root.refresh_activity(false);
    assert!(root.activity.is_none());
    assert_eq!(
        root.activity_close_reason,
        Some(ActivityCloseReason::ResourceOpened)
    );
    root.apply_view_request(request.clone()).unwrap();
    handle.invalidate_history();
    root.reconcile();
    assert!(root.activity.is_none());
    assert_eq!(
        root.activity_close_reason,
        Some(ActivityCloseReason::AccessChanged)
    );
    assert!(root.apply_view_request(request).is_err());
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn workspace_activity_real_palette_uses_local_handler_and_restores_editor_focus() {
    let (root, handle, _) = fixture();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1000, 700),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    frame.click_at(200.0, 180.0);
    let focused = frame.focused_access_node().unwrap();
    frame.set_modifiers(&["ctrl".into()]);
    frame.key("k");
    frame.set_modifiers(&[]);
    frame.type_text("Replay workspace activity");
    frame.key("Enter");
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("Workspace activity")
    );
    assert!(unsafe {
        (&*root_ptr)
            .activity
            .as_ref()
            .is_some_and(|activity| activity.pane.view() == activity::ActivityView::Replay)
    });
    assert!(handle.session_requests.borrow().is_empty());
    assert!(
        frame.take_caller_operation().is_none(),
        "workspace views must not enter the certified broker"
    );
    frame.key("Escape");
    frame.relayout();
    assert_eq!(frame.focused_access_node().unwrap().name, focused.name);
    frame
        .open_caller_search("Explore workspace activity")
        .unwrap();
    frame.key("Enter");
    let chat = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.chat")
        .unwrap()
        .clone();
    // A pane outcome is processed during update, after outgoing focus capture.
    unsafe {
        (&mut *root_ptr).activity.as_mut().unwrap().open_target = Some(ui::HistoryResourceTarget {
            app_id: Some(chat.app),
            app_label: None,
            id: chat.checkout.id.as_str().into(),
            kind: chat.checkout.kind.as_str().into(),
            label: "Chat".into(),
        });
    }
    frame.relayout();
    assert_eq!(
        unsafe { (&*root_ptr).activity_close_reason },
        Some(crate::desktop_activity::ActivityCloseReason::ResourceOpened)
    );
    assert_eq!(
        handle.navigation.borrow()["active"]["tab"],
        ((5_u64 << 48) | 11).to_string()
    );
    assert_ne!(
        frame.focused_access_node().and_then(|node| node.name),
        focused.name
    );
}

#[test]
fn workspace_activity_and_system_search_preserve_modal_and_editor_focus_ownership() {
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 700),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.click_at(200.0, 180.0);
    let owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("retained editor owner");
    let route = handle.navigation.borrow()["active"].clone();
    *handle.navigation_command.borrow_mut() =
        Some(crate::desktop_navigation::Command::WorkspaceActivity {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            view: activity::ActivityView::Replay,
        });
    frame.relayout();
    frame.poll_frame().unwrap();
    frame.relayout();
    assert_eq!(
        handle.navigation.borrow()["workspaceActivity"]["view"],
        "replay"
    );
    assert!(
        handle
            .open_global_search(&mut frame, &handle.generation, "")
            .is_err(),
        "history modal must keep input ownership"
    );
    frame.key("Escape");
    frame.relayout();
    assert!(handle.navigation.borrow()["workspaceActivity"].is_null());
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(owner.as_str())
    );
    handle
        .open_global_search(&mut frame, &handle.generation, "Notes")
        .unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some("system-search")
    );
    frame.key("Escape");
    frame.relayout();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(owner.as_str())
    );
    assert_eq!(handle.navigation.borrow()["active"], route);
    assert!(handle.session_requests.borrow().is_empty());
    assert!(frame.take_caller_operation().is_none());
}

#[test]
fn workspace_activity_queued_views_revalidate_admission_and_destination() {
    use activity::{ActivityView, WorkspaceViewRequest};
    let (mut root, handle, _) = fixture();
    let open = WorkspaceViewRequest::OpenActivity {
        workspace: WorkspaceId(1),
        view: ActivityView::Explore,
    };
    root.queue_view_request(open.clone()).unwrap();
    assert!(root.activity.is_none());
    // An admission captured before revocation cannot later re-enable history.
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: root.admitted_room.clone(),
            tree: handle.tree.borrow().clone(),
            routes: root.resolver.host.routes.clone(),
            source_heads: root.source_heads.clone(),
            required: root.required_records.clone(),
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    handle.invalidate_history();
    root.update(&mut AutoCommit::new(), &automerge::ROOT);
    assert!(root.activity.is_none());
    assert!(
        handle
            .errors
            .borrow()
            .iter()
            .any(|error| error.contains("admission unavailable"))
    );
    let (mut root, handle, _) = fixture();
    root.queue_view_request(open).unwrap();
    handle
        .tree
        .borrow_mut()
        .workspaces
        .push(Workspace::new(WorkspaceId(2), "Other"));
    handle.tree.borrow_mut().active = WorkspaceId(2);
    root.update(&mut AutoCommit::new(), &automerge::ROOT);
    assert!(root.activity.is_none());
    assert_eq!(handle.tree.borrow().active, WorkspaceId(2));
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn workspace_navigation_idle_accepted_request_wakes_queued_transition() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        tree.workspaces[0].strips.clear();
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 600),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    assert_external_frame_idle(&mut frame, "empty workspace before navigation");
    *handle.navigation_command.borrow_mut() =
        Some(crate::desktop_navigation::Command::WorkspaceActivity {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            view: activity::ActivityView::Replay,
        });
    // Match desktop_navigate: one synchronous acceptance/layout pass, then
    // only the ordinary host frame clock. No input or second relayout.
    frame.relayout();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert!(
        handle.navigation.borrow()["workspaceActivity"].is_null(),
        "acceptance must leave replacement queued"
    );
    assert!(
        frame.poll_frame().unwrap(),
        "accepted queued transition must request a frame while the desktop is idle"
    );
    assert_eq!(
        handle.navigation.borrow()["workspaceActivity"]["view"],
        "replay"
    );
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("Workspace activity")
    );
    assert!(handle.session_requests.borrow().is_empty());
    assert_external_frame_idle(&mut frame, "paused replay after queued transition");
}

#[test]
fn workspace_navigation_refresh_preserves_search_close_and_history_capabilities() {
    let (mut root, handle, _) = fixture();
    for topology in [false, true, false] {
        root.refresh_navigation(topology);
        for capability in [
            "globalSearch",
            "close",
            "workspaceSelect",
            "workspaceHistory",
            "workspaceReplay",
        ] {
            assert_eq!(
                handle.navigation.borrow()["capabilities"][capability],
                true,
                "missing {capability} after topology={topology}"
            );
        }
    }
    handle.invalidate_history();
    root.reconcile();
    for capability in ["globalSearch", "close", "workspaceSelect"] {
        assert_eq!(
            handle.navigation.borrow()["capabilities"][capability],
            true,
            "history revocation removed {capability}"
        );
    }
    for capability in ["workspaceHistory", "workspaceReplay"] {
        assert_eq!(
            handle.navigation.borrow()["capabilities"][capability],
            false
        );
    }
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn workspace_navigation_admitted_state_and_queued_commands() {
    let (mut root, handle, _) = fixture_with_topology(|tree| {
        tree.workspaces
            .push(Workspace::new(WorkspaceId(9_007_199_254_740_993), "Empty"));
    });
    assert_eq!(
        handle.navigation.borrow()["capabilities"]["workspaceHistory"],
        true
    );
    let command = |kind: &str, workspace: &str, view: Option<&str>| {
        let mut wire =
            serde_json::json!({"kind":kind,"generation":handle.generation,"workspace":workspace});
        if let Some(view) = view {
            wire["view"] = view.into();
        }
        serde_json::from_value(wire).unwrap()
    };
    root.navigate(
        command("workspace-activity", "1", Some("replay")),
        &AutoCommit::new(),
        &automerge::ROOT,
    )
    .unwrap();
    assert!(
        root.activity.is_none(),
        "bridge acceptance must only queue the pane"
    );
    root.update(&mut AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        handle.navigation.borrow()["workspaceActivity"],
        serde_json::json!({"workspace":"1","view":"replay","playing":false,"coverage":"complete"})
    );
    root.activity
        .as_mut()
        .unwrap()
        .pane
        .set_view(activity::ActivityView::Tasks);
    root.refresh_navigation(false);
    assert_eq!(
        handle.navigation.borrow()["workspaceActivity"]["view"],
        "tasks"
    );
    for workspace in ["01", "-1", "18446744073709551616", "2", "9007199254740993"] {
        assert!(
            root.navigate(
                command("workspace-activity", workspace, Some("explore")),
                &AutoCommit::new(),
                &automerge::ROOT
            )
            .is_err()
        );
    }
    let mut stale =
        serde_json::json!({"kind":"close-workspace-activity","generation":"old","workspace":"1"});
    assert!(
        root.navigate(
            serde_json::from_value(stale.clone()).unwrap(),
            &AutoCommit::new(),
            &automerge::ROOT
        )
        .is_err()
    );
    stale["kind"] = "workspace".into();
    assert!(
        root.navigate(
            serde_json::from_value(stale).unwrap(),
            &AutoCommit::new(),
            &automerge::ROOT
        )
        .is_err()
    );
    root.navigate(
        command("close-workspace-activity", "1", None),
        &AutoCommit::new(),
        &automerge::ROOT,
    )
    .unwrap();
    assert!(root.activity.is_some());
    root.update(&mut AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        handle.navigation.borrow()["workspaceActivityCloseReason"],
        "dismissed"
    );
    root.navigate(
        command("workspace", "9007199254740993", None),
        &AutoCommit::new(),
        &automerge::ROOT,
    )
    .unwrap();
    assert_eq!(handle.tree.borrow().active, WorkspaceId(1));
    root.update(&mut AutoCommit::new(), &automerge::ROOT);
    assert_eq!(
        handle.navigation.borrow()["activeWorkspace"],
        "9007199254740993"
    );
    assert!(handle.navigation.borrow()["active"].is_null());
    handle.invalidate_history();
    root.reconcile();
    assert_eq!(
        handle.navigation.borrow()["capabilities"]["workspaceHistory"],
        false
    );
    assert_eq!(
        handle.navigation.borrow()["capabilities"]["workspaceReplay"],
        false
    );
    assert!(
        root.navigate(
            command("workspace-activity", "9007199254740993", Some("replay")),
            &AutoCommit::new(),
            &automerge::ROOT
        )
        .is_err()
    );
    assert!(handle.session_requests.borrow().is_empty());
}

fn fixture() -> (DesktopRoot, DesktopHandle, PortableStorage) {
    fixture_with_topology(|_| {})
}

#[test]
fn local_view_recovery_selects_the_right_group_after_the_active_group_closes() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 3 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
        tree.active_group.insert(strip.id, 4);
    });
    handle
        .tree
        .borrow_mut()
        .active_strip
        .insert(WorkspaceId(1), StripId(2));
    let before = LocalView::from_tree(&handle.tree.borrow());
    let mut admitted = handle.tree.borrow().clone();
    let location = {
        let strip = &admitted.workspaces[0].strips[0];
        tilo_desktop_model::commands::TabLocation {
            strip: strip.id.0,
            group: strip.window_groups[1].id,
            tab: strip.window_groups[1].tabs[0].id,
        }
    };
    tilo_desktop_model::commands::apply_command(
        &mut admitted.workspaces[0],
        &tilo_desktop_model::commands::DesktopCommand::CloseTab { location },
        &mut || Ok(0),
        &mut |_, _| panic!("close cannot create a record"),
    )
    .unwrap();
    admitted.active_group.clear();

    before.apply(&mut admitted);

    assert_eq!(
        admitted.active_group.get(&StripId(2)),
        Some(&5),
        "closing the middle visible window selects its immediate right neighbour"
    );
    drop(root);
}

#[test]
fn local_view_recovery_follows_visible_group_order_for_close_and_keeps_inactive_selection() {
    for (active_index, close_index, expected_group) in [
        (0, 0, Some(4)),
        (1, 1, Some(5)),
        (3, 3, Some(5)),
        (2, 0, Some(5)),
        (1, 3, Some(4)),
    ] {
        let (_root, handle, _) = fixture_with_topology(|tree| {
            let strip = &mut tree.workspaces[0].strips[0];
            let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
            strip.window_groups = tabs
                .into_iter()
                .enumerate()
                .map(|(index, tab)| WindowGroup {
                    id: 3 + index as u64,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                })
                .collect();
            tree.active_group.insert(strip.id, 3 + active_index as u64);
        });
        handle
            .tree
            .borrow_mut()
            .active_strip
            .insert(WorkspaceId(1), StripId(2));
        let before = LocalView::from_tree(&handle.tree.borrow());
        let mut admitted = handle.tree.borrow().clone();
        let location = {
            let strip = &admitted.workspaces[0].strips[0];
            tilo_desktop_model::commands::TabLocation {
                strip: strip.id.0,
                group: strip.window_groups[close_index].id,
                tab: strip.window_groups[close_index].tabs[0].id,
            }
        };
        tilo_desktop_model::commands::apply_command(
            &mut admitted.workspaces[0],
            &tilo_desktop_model::commands::DesktopCommand::CloseTab { location },
            &mut || Ok(0),
            &mut |_, _| panic!("close cannot create a record"),
        )
        .unwrap();
        admitted.active_group.clear();

        before.apply(&mut admitted);

        assert_eq!(
            admitted.active_group.get(&StripId(2)),
            expected_group.as_ref(),
            "active group {active_index}, close {close_index}"
        );
        assert_eq!(
            active_route(&admitted).map(|route| route.group),
            expected_group.map(|group| group.to_string()),
            "the recovered route must select the same visible neighbour"
        );
    }
}

#[test]
fn local_view_recovery_leaves_an_empty_workspace_without_an_active_group() {
    let (_root, handle, _) = fixture_with_topology(|tree| {
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        group.tabs.truncate(1);
        group.active_tab = group.tabs[0].id;
    });
    handle
        .tree
        .borrow_mut()
        .active_strip
        .insert(WorkspaceId(1), StripId(2));
    let before = LocalView::from_tree(&handle.tree.borrow());
    let mut admitted = handle.tree.borrow().clone();
    let location = {
        let strip = &admitted.workspaces[0].strips[0];
        tilo_desktop_model::commands::TabLocation {
            strip: strip.id.0,
            group: strip.window_groups[0].id,
            tab: strip.window_groups[0].tabs[0].id,
        }
    };
    tilo_desktop_model::commands::apply_command(
        &mut admitted.workspaces[0],
        &tilo_desktop_model::commands::DesktopCommand::CloseTab { location },
        &mut || Ok(0),
        &mut |_, _| panic!("close cannot create a record"),
    )
    .unwrap();
    admitted.active_group.clear();

    before.apply(&mut admitted);

    assert!(admitted.active_group.is_empty());
    assert!(active_route(&admitted).is_none());
}

#[test]
fn local_view_recovery_keeps_a_valid_admitted_group_selection() {
    let (_root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 3 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
        tree.active_group.insert(strip.id, 4);
    });
    handle
        .tree
        .borrow_mut()
        .active_strip
        .insert(WorkspaceId(1), StripId(2));
    let before = LocalView::from_tree(&handle.tree.borrow());
    let mut admitted = handle.tree.borrow().clone();
    let location = {
        let strip = &admitted.workspaces[0].strips[0];
        tilo_desktop_model::commands::TabLocation {
            strip: strip.id.0,
            group: strip.window_groups[1].id,
            tab: strip.window_groups[1].tabs[0].id,
        }
    };
    tilo_desktop_model::commands::apply_command(
        &mut admitted.workspaces[0],
        &tilo_desktop_model::commands::DesktopCommand::CloseTab { location },
        &mut || Ok(0),
        &mut |_, _| panic!("close cannot create a record"),
    )
    .unwrap();
    admitted.active_group.insert(StripId(2), 3);

    before.apply(&mut admitted);

    assert_eq!(admitted.active_group.get(&StripId(2)), Some(&3));
}

fn assert_external_frame_idle(frame: &mut ui::headless::FrameHarness, context: &str) {
    let output = frame.frame_output();
    assert!(
        !output.needs_redraw,
        "{context}: settled presentation stayed dirty"
    );
    assert!(
        !output.needs_animation_frame,
        "{context}: settled presentation kept animating"
    );
    assert!(
        output
            .next_wake_deadline
            .is_none_or(|deadline| deadline > ui::clock::Instant::now()),
        "{context}: settled presentation retained a due wake deadline"
    );
    assert!(
        !frame.poll_frame().unwrap(),
        "{context}: idle poll produced another scene"
    );
}

fn settle_external_navigation(root: *mut DesktopRoot, frame: &mut ui::headless::FrameHarness) {
    // Initial group framing is a legitimate finite animation. Advance its
    // real clock seam, then run the ordinary frame pipeline once more.
    let settled_at = ui::clock::Instant::now() + ui::clock::Duration::from_secs(1);
    // SAFETY: `frame` retains this boxed root and no harness method runs
    // while the temporary exclusive references are alive.
    unsafe {
        let strips = (&*root).surface.strip_ids();
        for strip in strips {
            (&mut *root)
                .surface
                .row_mut(strip)
                .unwrap()
                .tick_snap_at(settled_at);
        }
    }
    frame.relayout();
}

fn settle_notes_status_bubble(frame: &mut ui::headless::FrameHarness) {
    assert!(
        frame.frame_output().needs_animation_frame,
        "selecting Notes must preserve its finite visible status animation"
    );
    std::thread::sleep(ui::status_bubble::DEFAULT_LINGER + ui::clock::Duration::from_millis(10));
    frame.relayout();
}

#[test]
fn external_ordinary_inactive_notes_renders_when_selected_without_redraw_loop() {
    let (root, handle, _) = fixture_with_topology(|topology| {
        let groups = &mut topology.workspaces[0].strips[0].window_groups;
        let chat = groups[0].tabs.remove(1);
        let calculator = groups[0].tabs.pop().unwrap();
        assert_eq!(calculator.id, (5 << 48) | 13);
        groups.push(WindowGroup {
            id: 4,
            active_tab: chat.id,
            tabs: vec![chat],
            width: WindowWidth::Normal,
            maximized: false,
        });
        topology.active_strip.insert(topology.active, StripId(2));
        topology.active_group.insert(StripId(2), 3);
    });
    let routes = root.resolver.host.routes.clone();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    settle_external_navigation(root_ptr, &mut frame);

    let initial = frame.inspection_json().unwrap();
    assert!(
        initial.contains("WordProcessor"),
        "Writer must be presented"
    );
    assert!(initial.contains("ChatApp"), "Chat must be presented");
    assert!(
        !initial.contains("NotesApp"),
        "inactive Notes must stay retained off-screen"
    );
    assert!(
        frame
            .resources()
            .iter()
            .any(|resource| resource.kind == "notes.notebook"),
        "inactive Notes must retain its resource owner"
    );
    assert_external_frame_idle(&mut frame, "Writer/Chat with inactive Notes");

    *handle.navigation_command.borrow_mut() = Some(crate::desktop_navigation::Command::Select {
        generation: handle.generation.clone(),
        route: crate::desktop_navigation::Route {
            workspace: "1".into(),
            strip: "2".into(),
            group: "3".into(),
            tab: ((5_u64 << 48) | 12).to_string(),
        },
    });
    frame.relayout();
    settle_external_navigation(root_ptr, &mut frame);
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert!(
        frame.inspection_json().unwrap().contains("NotesApp"),
        "selecting retained Notes must present its real checked widget"
    );
    settle_notes_status_bubble(&mut frame);
    assert_external_frame_idle(&mut frame, "selected Notes");

    let refreshed = handle.tree.borrow().clone();
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&refreshed),
            source_heads: vec![[8; 32]],
            tree: refreshed,
            routes,
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    assert!(
        frame.frame_output().needs_redraw,
        "an admitted remote snapshot must still wake the real root"
    );
    assert!(
        frame.poll_frame().unwrap(),
        "remote admission must produce a frame"
    );
    assert_external_frame_idle(&mut frame, "reconciled remote snapshot");
}

#[test]
fn external_hidden_notes_strip_renders_when_selected_without_redraw_loop() {
    let (root, handle, _) = fixture_with_topology(|topology| {
        let notes = topology.workspaces[0].strips[0].window_groups[0]
            .tabs
            .remove(2);
        topology.workspaces[0].strips.push(Strip {
            id: StripId(9),
            name: "Notes strip".into(),
            window_groups: vec![WindowGroup {
                id: 90,
                active_tab: notes.id,
                tabs: vec![notes],
                width: WindowWidth::Normal,
                maximized: false,
            }],
        });
        topology.active_strip.insert(topology.active, StripId(2));
    });
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    settle_external_navigation(root_ptr, &mut frame);

    assert!(
        !frame.inspection_json().unwrap().contains("NotesApp"),
        "the retained hidden strip must not be in the presentation tree"
    );
    assert_external_frame_idle(&mut frame, "hidden Notes strip");

    let hidden_title = "Hidden update";
    let hidden_notes_instance = (5_u64 << 48) | 12;
    frame
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: hidden_notes_instance,
                address: tilo_actions::ActionAddress::new("notes.notebook", "new-page"),
            },
            serde_json::json!({"value": hidden_title})
                .as_object()
                .unwrap(),
        )
        .unwrap();
    frame.relayout();
    assert!(
        !frame.inspection_json().unwrap().contains("NotesApp"),
        "updating the retained owner must not present its hidden strip"
    );
    let notes_record = handle
        .records
        .borrow()
        .values()
        .find(|record| {
            record.app == "so.tilo.notes"
                && record.checkout.id == ui::record::RecordId::for_tab(hidden_notes_instance)
        })
        .unwrap()
        .clone();
    assert!(
        notes_record
            .store
            .read(&notes_record.checkout.id, |doc| {
                notes::notebook::project(doc, None)
                    .unwrap()
                    .pages()
                    .iter()
                    .any(|page| page.title == hidden_title)
            })
            .unwrap(),
        "the real retained Notes record must accept the update while hidden"
    );

    *handle.navigation_command.borrow_mut() = Some(crate::desktop_navigation::Command::Strip {
        generation: handle.generation.clone(),
        workspace: "1".into(),
        strip: "9".into(),
    });
    frame.relayout();
    settle_external_navigation(root_ptr, &mut frame);
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert!(
        frame.inspection_json().unwrap().contains("NotesApp"),
        "selecting the retained strip must present its real checked widget"
    );
    frame
        .expect(&ui::headless::SemanticExpectation::named("note_title").value(hidden_title))
        .unwrap();
    settle_notes_status_bubble(&mut frame);
    assert_external_frame_idle(&mut frame, "selected Notes strip");
}

#[test]
fn external_primary_scroll_is_real_private_and_rejects_stale_region() {
    use crate::desktop_navigation::{Command, ScrollAction};
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 420),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    let before = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let first = handle.navigation.borrow().clone();
    assert!(first["scroll"]["max"].as_f64().unwrap() > 0.0);
    *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
        generation: handle.generation.clone(),
        region: first["scroll"]["region"].as_str().unwrap().into(),
        action: ScrollAction::End,
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    let end = handle.navigation.borrow().clone();
    assert_eq!(end["scroll"]["offset"], end["scroll"]["max"]);
    assert!(end["scroll"]["offset"].as_f64().unwrap() > 0.0);
    let after_scroll = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        before, after_scroll,
        "scrolling must not change any document heads"
    );
    let chat = end["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["label"] == "Chat")
        .unwrap()["route"]
        .clone();
    *handle.navigation_command.borrow_mut() = Some(Command::Select {
        generation: handle.generation.clone(),
        route: serde_json::from_value(chat).unwrap(),
    });
    frame.relayout();
    // First native Chat presentation may commit its existing read watermark.
    let before_stale = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
        generation: handle.generation.clone(),
        region: first["scroll"]["region"].as_str().unwrap().into(),
        action: ScrollAction::Start,
    });
    frame.relayout();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_err()
    );
    let after = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(before_stale, after);
    assert!(handle.session_requests.borrow().is_empty());
}
#[test]
fn external_width_and_maximize_are_local_and_reject_stale_routes() {
    use crate::desktop_navigation::{Command, Route};
    let (root, handle, _) = fixture();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1000, 600),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    let route: Route =
        serde_json::from_value(handle.navigation.borrow()["active"].clone()).unwrap();
    let heads = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    // SAFETY: `frame` owns this root for the test lifetime and no harness
    // call overlaps these temporary shared references.
    let host = unsafe {
        std::ptr::from_ref(
            (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                .active_host()
                .unwrap(),
        )
    };
    for permille in [333, 500, 667, 1000] {
        *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
            generation: handle.generation.clone(),
            route: route.clone(),
            maximized: true,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert_eq!(handle.navigation.borrow()["maximized"], true);
        assert_eq!(handle.navigation.borrow()["desktopPan"]["max"], 0.0);
        assert_eq!(handle.navigation.borrow()["desktopPan"]["offset"], 0.0);
        *handle.navigation_command.borrow_mut() = Some(Command::Width {
            generation: handle.generation.clone(),
            route: route.clone(),
            permille,
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert_eq!(handle.navigation.borrow()["widthPermille"], permille);
        assert_eq!(handle.navigation.borrow()["maximized"], false);
    }
    *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
        generation: handle.generation.clone(),
        route: route.clone(),
        maximized: true,
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    for command in [
        Command::Width {
            generation: handle.generation.clone(),
            route: route.clone(),
            permille: 99,
        },
        Command::Width {
            generation: handle.generation.clone(),
            route: Route {
                tab: "0".into(),
                ..route.clone()
            },
            permille: 333,
        },
        Command::Width {
            generation: "disposed-mount".into(),
            route: route.clone(),
            permille: 333,
        },
    ] {
        *handle.navigation_command.borrow_mut() = Some(command);
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
        assert_eq!(handle.navigation.borrow()["widthPermille"], 1000);
        assert_eq!(handle.navigation.borrow()["maximized"], true);
    }
    *handle.navigation_command.borrow_mut() = Some(Command::Maximize {
        generation: handle.generation.clone(),
        route: route.clone(),
        maximized: false,
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    let mut stale = route.clone();
    stale.tab = "0".into();
    for command in [
        Command::Maximize {
            generation: handle.generation.clone(),
            route: stale,
            maximized: true,
        },
        Command::Maximize {
            generation: "disposed-mount".into(),
            route: route.clone(),
            maximized: true,
        },
    ] {
        *handle.navigation_command.borrow_mut() = Some(command);
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
    }
    assert_eq!(
        handle.navigation.borrow()["active"],
        serde_json::to_value(&route).unwrap()
    );
    assert!(frame.inspection_json().unwrap().contains("WordProcessor"));
    // SAFETY: same stable frame-owned root as above.
    assert_eq!(host, unsafe {
        std::ptr::from_ref(
            (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                .active_host()
                .unwrap(),
        )
    });
    assert_eq!(handle.navigation.borrow()["widthPermille"], 1000);
    assert_eq!(handle.navigation.borrow()["maximized"], false);
    assert!(handle.session_requests.borrow().is_empty());
    assert_eq!(
        heads,
        handle
            .records
            .borrow()
            .values()
            .map(|r| r
                .store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap())
            .collect::<Vec<_>>()
    );
}
#[test]
fn external_horizontal_pan_moves_real_groups_and_rejects_stale_targets() {
    use crate::desktop_navigation::{Command, Route};
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 3 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Fraction(667),
                maximized: false,
            })
            .collect();
        tree.active_group.insert(strip.id, 3);
    });
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1000, 420),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    settle_external_navigation(root_ptr, &mut frame);
    let first = handle.navigation.borrow().clone();
    let pan = &first["desktopPan"];
    let max = pan["max"].as_f64().unwrap();
    assert!(max > 0.0, "four real groups must overflow the viewport");
    let before = unsafe {
        (&*root_ptr)
            .surface
            .row(StripId(2))
            .unwrap()
            .groups
            .iter()
            .map(|g| (g.id, g.get_rect()))
            .collect::<Vec<_>>()
    };
    let content = pan["content"].clone();
    let vertical = first["scroll"].clone();
    let heads = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let pan_to = |offset| Command::Pan {
        generation: handle.generation.clone(),
        workspace: "1".into(),
        strip: "2".into(),
        offset,
    };
    *handle.navigation_command.borrow_mut() = Some(pan_to(max));
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    let at_end = handle.navigation.borrow().clone();
    assert_eq!(at_end["desktopPan"]["offset"], at_end["desktopPan"]["max"]);
    assert_eq!(at_end["desktopPan"]["content"], content);
    assert_eq!(at_end["scroll"], vertical);
    let after = unsafe {
        (&*root_ptr)
            .surface
            .row(StripId(2))
            .unwrap()
            .groups
            .iter()
            .map(|g| (g.id, g.get_rect()))
            .collect::<Vec<_>>()
    };
    for ((before_id, before_rect), (after_id, after_rect)) in before.iter().zip(&after) {
        assert_eq!(before_id, after_id);
        assert_eq!(before_rect.width(), after_rect.width());
        assert_eq!(before_rect.height(), after_rect.height());
        assert!(after_rect.x0 < before_rect.x0);
    }
    for command in [
        Command::Pan {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            strip: "9".into(),
            offset: 0.0,
        },
        Command::Pan {
            generation: "disposed-mount".into(),
            workspace: "1".into(),
            strip: "2".into(),
            offset: 0.0,
        },
        pan_to(f64::NAN),
    ] {
        *handle.navigation_command.borrow_mut() = Some(command);
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
        assert_eq!(handle.navigation.borrow()["desktopPan"]["offset"], max);
    }
    frame.resize((1200, 420)).unwrap();
    let resized = handle.navigation.borrow()["desktopPan"].clone();
    let resized_offset = resized["offset"].as_f64().unwrap();
    assert!(
        resized_offset > 0.0 && resized_offset <= resized["max"].as_f64().unwrap(),
        "resize keeps manual pan bounded without reframing the selected app"
    );
    let selected_route = first["active"].clone();
    *handle.navigation_command.borrow_mut() = Some(Command::Select {
        generation: handle.generation.clone(),
        route: serde_json::from_value(selected_route).unwrap(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    settle_external_navigation(root_ptr, &mut frame);
    assert!(
        handle.navigation.borrow()["desktopPan"]["offset"]
            .as_f64()
            .unwrap()
            < max,
        "reselecting the same app must reveal it after manual pan"
    );
    let chat = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["label"] == "Chat")
        .unwrap()["route"]
        .clone();
    *handle.navigation_command.borrow_mut() = Some(Command::Select {
        generation: handle.generation.clone(),
        route: serde_json::from_value::<Route>(chat.clone()).unwrap(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert_eq!(handle.navigation.borrow()["active"], chat);
    settle_external_navigation(root_ptr, &mut frame);
    let row = unsafe { (&*root_ptr).surface.row(StripId(2)).unwrap() };
    let selected = row.groups.iter().find(|g| g.id == 4).unwrap().get_rect();
    assert!(
        selected.x0 >= row.get_rect().x0 - 1.0 && selected.x1 <= row.get_rect().x1 + 1.0,
        "selection must reframe its exact group"
    );
    assert_eq!(
        heads,
        handle
            .records
            .borrow()
            .values()
            .map(|r| r
                .store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap())
            .collect::<Vec<_>>()
    );
    assert!(handle.session_requests.borrow().is_empty());
    // All four app leaves are visible here; whole-frame idle is covered by
    // the separately queued PreviewCanvas regression work.
}
#[test]
fn external_close_uses_the_real_root_request_and_rejects_stale_routes() {
    use crate::desktop_navigation::{Command, Route};
    use tilo_desktop_model::commands::DesktopCommand;
    let (root, handle, _) = fixture();
    let before = serde_json::to_string(&*handle.tree.borrow()).unwrap();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 600),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    assert_eq!(handle.navigation.borrow()["capabilities"]["close"], true);
    let route: Route =
        serde_json::from_value(handle.navigation.borrow()["active"].clone()).unwrap();
    let close = || Command::Close {
        generation: handle.generation.clone(),
        route: route.clone(),
    };

    *handle.navigation_command.borrow_mut() = Some(close());
    frame.relayout();
    let invocation = handle
        .navigation_result
        .borrow_mut()
        .take()
        .unwrap()
        .unwrap()
        .expect("close invocation");
    let requests = handle.session_requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0.to_string(), invocation);
    assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
    assert_eq!(
        requests[0].1.source.room,
        handle
            .tree
            .borrow()
            .active_workspace()
            .unwrap()
            .room
            .to_string()
    );
    assert!(
        matches!(&requests[0].1.command,DesktopCommand::CloseTab{location} if location.strip==2 && location.group==3 && location.tab.to_string()==route.tab)
    );
    drop(requests);
    assert_eq!(
        serde_json::to_string(&*handle.tree.borrow()).unwrap(),
        before,
        "close request must not remove local topology"
    );

    *handle.navigation_command.borrow_mut() = Some(close());
    frame.relayout();
    assert_eq!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .unwrap()
            .as_deref(),
        Some(invocation.as_str()),
        "repeat close reuses the pending invocation"
    );
    assert_eq!(handle.session_requests.borrow().len(), 1);

    let mut stale_route = route.clone();
    stale_route.tab = "0".into();
    for command in [
        Command::Close {
            generation: handle.generation.clone(),
            route: stale_route,
        },
        Command::Close {
            generation: "stale-generation".into(),
            route: route.clone(),
        },
    ] {
        *handle.navigation_command.borrow_mut() = Some(command);
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_err()
        );
    }
    assert_eq!(
        handle.session_requests.borrow().len(),
        1,
        "invalid close must not enqueue a request"
    );
    assert_eq!(
        serde_json::to_string(&*handle.tree.borrow()).unwrap(),
        before
    );
}

#[test]
fn external_close_of_an_inactive_route_queues_its_exact_tab_without_selecting_it() {
    use crate::desktop_navigation::{Command, Route};
    use tilo_desktop_model::commands::DesktopCommand;
    let (mut root, handle, _) = fixture_with_topology(|tree| {
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        let tab = group.tabs.remove(1);
        tree.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 4,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            });
        tree.active_group.insert(StripId(2), 3);
    });
    let before = root.active_route().expect("active route");
    let target = Route {
        workspace: "1".into(),
        strip: "2".into(),
        group: "4".into(),
        tab: handle.tree.borrow().workspaces[0].strips[0].window_groups[1].tabs[0]
            .id
            .to_string(),
    };

    let invocation = root
        .navigate(
            Command::Close {
                generation: handle.generation.clone(),
                route: target.clone(),
            },
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .expect("inactive close is accepted")
        .1
        .expect("close invocation");

    assert_eq!(root.active_route(), Some(before));
    let requests = handle.session_requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0.to_string(), invocation);
    assert!(matches!(
        &requests[0].1.command,
        DesktopCommand::CloseTab { location }
            if location.strip == 2 && location.group == 4 && location.tab.to_string() == target.tab
    ));
    drop(requests);

    assert_eq!(
        root.navigate(
            Command::Close {
                generation: handle.generation.clone(),
                route: target.clone(),
            },
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .expect("duplicate inactive close is accepted")
        .1
        .as_deref(),
        Some(invocation.as_str())
    );
    assert_eq!(handle.session_requests.borrow().len(), 1);

    let mut foreign = target;
    foreign.workspace = "2".into();
    assert!(
        root.navigate(
            Command::Close {
                generation: handle.generation.clone(),
                route: foreign,
            },
            &AutoCommit::new(),
            &automerge::ROOT,
        )
        .is_err()
    );
    assert_eq!(handle.session_requests.borrow().len(), 1);
}
#[test]
fn external_close_uses_its_target_when_the_active_strip_is_empty() {
    use crate::desktop_navigation::{Command, Route};
    use tilo_desktop_model::commands::DesktopCommand;
    let (mut root, handle, _) = fixture_with_topology(|tree| {
        let group = &mut tree.workspaces[0].strips[0].window_groups[0];
        let tab = group.tabs.remove(1);
        tree.workspaces[0].strips[0]
            .window_groups
            .push(WindowGroup {
                id: 4,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            });
        tree.workspaces[0].strips.push(Strip {
            id: StripId(5),
            name: "Empty".into(),
            window_groups: vec![],
        });
        tree.active_strip.insert(WorkspaceId(1), StripId(5));
    });
    let target = Route {
        workspace: "1".into(),
        strip: "2".into(),
        group: "4".into(),
        tab: handle.tree.borrow().workspaces[0].strips[0].window_groups[1].tabs[0]
            .id
            .to_string(),
    };
    assert_eq!(root.active_route(), None);

    root.navigate(
        Command::Close {
            generation: handle.generation.clone(),
            route: target.clone(),
        },
        &AutoCommit::new(),
        &automerge::ROOT,
    )
    .expect("target close is available without an active group");

    assert_eq!(root.active_route(), None);
    assert!(matches!(
        &handle.session_requests.borrow()[0].1.command,
        DesktopCommand::CloseTab { location }
            if location.strip == 2 && location.group == 4 && location.tab.to_string() == target.tab
    ));
}
#[test]
fn workspace_navigation_document_history_interlock_preserves_exact_owner() {
    use crate::desktop_navigation::Command;
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 600),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    let notes = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["label"] == "Notes")
        .unwrap()["route"]
        .clone();
    *handle.navigation_command.borrow_mut() = Some(Command::Select {
        generation: handle.generation.clone(),
        route: serde_json::from_value(notes).unwrap(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    let nav = handle.navigation.borrow().clone();
    *handle.navigation_command.borrow_mut() = Some(serde_json::from_value(serde_json::json!({"kind":"workspace-activity","generation":handle.generation,"workspace":"1","view":"replay"})).unwrap());
    frame.relayout();
    frame.poll_frame().unwrap();
    frame.relayout();
    assert_eq!(
        handle.navigation.borrow()["workspaceActivity"]["view"],
        "replay"
    );
    let mut route: crate::desktop_navigation::Route =
        serde_json::from_value(nav["active"].clone()).unwrap();
    route.workspace = "9007199254740993".into();
    *handle.navigation_command.borrow_mut() = Some(Command::History {
        generation: handle.generation.clone(),
        route,
        target: nav["history"]["target"].as_str().unwrap().into(),
    });
    frame.relayout();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_err()
    );
    assert!(!handle.navigation.borrow()["workspaceActivity"].is_null());
    let before = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    *handle.navigation_command.borrow_mut() = Some(Command::History {
        generation: handle.generation.clone(),
        route: serde_json::from_value(nav["active"].clone()).unwrap(),
        target: nav["history"]["target"].as_str().unwrap().into(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert!(handle.navigation.borrow()["workspaceActivity"].is_null());
    assert_eq!(
        handle.navigation.borrow()["workspaceActivityCloseReason"],
        "resource-opened"
    );
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("ActivityTimeline")
    );
    assert!(
        !frame
            .inspection_json()
            .unwrap()
            .contains("Workspace activity")
    );
    let after = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(before, after);
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn external_history_opens_retained_framework_without_changing_heads() {
    use crate::desktop_navigation::Command;
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 600),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    // Writer owns its existing app-local history panel; Notes registers the
    // shared framework history which this external entry point exposes.
    assert!(handle.navigation.borrow()["history"].is_null());
    let notes = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["label"] == "Notes")
        .unwrap()["route"]
        .clone();
    *handle.navigation_command.borrow_mut() = Some(Command::Select {
        generation: handle.generation.clone(),
        route: serde_json::from_value(notes).unwrap(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    let nav = handle.navigation.borrow().clone();
    assert!(
        !nav["scroll"].is_null(),
        "Notes live editor is the primary region before opening History"
    );
    let before = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    *handle.navigation_command.borrow_mut() = Some(Command::History {
        generation: handle.generation.clone(),
        route: serde_json::from_value(nav["active"].clone()).unwrap(),
        target: nav["history"]["target"].as_str().unwrap().into(),
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("ActivityTimeline")
    );
    assert!(
        handle.navigation.borrow()["scroll"].is_null(),
        "History owns input until returning to the live editor"
    );
    let calculator = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["label"] == "Calculator")
        .unwrap()["route"]
        .clone();
    assert_eq!(
        handle
            .navigate(
                &mut frame,
                Command::Select {
                    generation: handle.generation.clone(),
                    route: serde_json::from_value(calculator).unwrap(),
                },
            )
            .unwrap_err(),
        "Finish the current dialog or command first"
    );
    let after = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            r.store
                .read(&r.checkout.id, |doc| doc.base_heads().to_vec())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(before, after);
}
#[test]
fn external_strips_fill_viewport_and_preserve_inactive_owners_and_scroll() {
    use crate::desktop_navigation::{Command, ScrollAction};
    let (mut root, handle, _) = fixture();
    let mut topology = handle.tree.borrow().clone();
    let notes = topology.workspaces[0].strips[0].window_groups[0]
        .tabs
        .remove(2);
    topology.workspaces[0].strips.push(Strip {
        id: StripId(9),
        name: "Notes strip".into(),
        window_groups: vec![WindowGroup {
            id: 90,
            active_tab: notes.id,
            tabs: vec![notes],
            width: WindowWidth::Normal,
            maximized: false,
        }],
    });
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&topology),
            source_heads: vec![[7; 32]],
            tree: topology,
            routes: root.resolver.host.routes.clone(),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    root.reconcile();
    let mut root = Box::new(root);
    let ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1000, 420),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    let region = handle.navigation.borrow()["scroll"]["region"]
        .as_str()
        .unwrap()
        .to_string();
    *handle.navigation_command.borrow_mut() = Some(Command::Scroll {
        generation: handle.generation.clone(),
        region: region.clone(),
        action: ScrollAction::End,
    });
    frame.relayout();
    frame.poll_frame().unwrap();
    let offset = handle.navigation.borrow()["scroll"]["offset"]
        .as_f64()
        .unwrap();
    let records = handle.records.borrow().len();
    for (strip, other) in [(9, 2), (2, 9)] {
        *handle.navigation_command.borrow_mut() = Some(Command::Strip {
            generation: handle.generation.clone(),
            workspace: "1".into(),
            strip: strip.to_string(),
        });
        frame.relayout();
        frame.poll_frame().unwrap();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        // Frame owns this box; no frame mutation while the references are live.
        let surface = unsafe { &(&*ptr).surface };
        assert!(surface.row(StripId(strip)).unwrap().get_rect().height() > 390.0);
        assert_eq!(
            surface.row(StripId(other)).unwrap().get_rect().height(),
            0.0
        );
        assert_eq!(surface.strip_ids().len(), 2);
        assert_eq!(handle.records.borrow().len(), records);
    }
    assert_eq!(handle.navigation.borrow()["scroll"]["region"], region);
    assert_eq!(handle.navigation.borrow()["scroll"]["offset"], offset);
    assert!(handle.session_requests.borrow().is_empty());
}

#[test]
fn real_caller_palette_enqueues_bound_command_without_mutating_canonical_tree() {
    let (root, handle, _) = fixture();
    let before = serde_json::to_string(&*handle.tree.borrow()).unwrap();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(root),
        (1024, 768),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    frame.set_modifiers(&["ctrl".into()]);
    frame.key("k");
    frame.set_modifiers(&[]);
    frame.type_text("Open Notes");
    frame.key("Enter");
    let requests = handle.session_requests.borrow();
    assert_eq!(
        requests.len(),
        1,
        "actual palette must dispatch a desktop request"
    );
    assert!(!requests[0].0.is_nil());
    assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
    assert!(
        matches!(&requests[0].1.command, tilo_desktop_model::commands::DesktopCommand::OpenApp { app } if app == "so.tilo.notes")
    );
    assert!(
        frame.take_caller_operation().is_none(),
        "local View queue owns the opaque command"
    );
    assert_eq!(
        serde_json::to_string(&*handle.tree.borrow()).unwrap(),
        before
    );
}

#[test]
fn empty_workspace_palette_enqueues_durable_notes_command() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        tree.workspaces[0].strips.clear();
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(root),
        (1024, 768),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    frame.set_modifiers(&["ctrl".into()]);
    frame.key("k");
    frame.set_modifiers(&[]);
    frame.type_text("Open Notes");
    frame.key("Enter");
    let requests = handle.session_requests.borrow();
    assert_eq!(requests.len(), 1);
    // The owner places the parity open: an empty workspace gets a strip.
    assert!(matches!(
        &requests[0].1.command,
        tilo_desktop_model::commands::DesktopCommand::OpenApp { app } if app == "so.tilo.notes"
    ));
}
fn browser_semantic_route(frame: &ui::headless::FrameHarness, label: &str) -> (String, String) {
    fn find(node: &ui::headless::InspectionNode, label: &str) -> Option<(String, String)> {
        fn target(node: &ui::a11y::AccessNode, label: &str) -> Option<String> {
            if node.name.as_deref() == Some(label) && node.action_target.is_some() {
                return node.action_target.clone();
            }
            node.children.iter().find_map(|node| target(node, label))
        }
        if let Some(target) = node.semantics.as_ref().and_then(|node| target(node, label)) {
            return Some((node.path.clone(), target));
        }
        node.children.iter().find_map(|node| find(node, label))
    }
    find(&frame.inspection().root, label).expect("exported browser semantic route")
}
fn browser_semantic_route_for_instance(
    frame: &ui::headless::FrameHarness,
    label: &str,
    instance: u64,
) -> (String, String) {
    fn targets(node: &ui::a11y::AccessNode, label: &str, out: &mut Vec<String>) {
        if node.name.as_deref() == Some(label) && node.action_target.is_some() {
            out.push(node.action_target.clone().unwrap());
        }
        for child in &node.children {
            targets(child, label, out);
        }
    }
    type RouteMatch = (String, String, Option<String>, Option<u64>, Vec<String>);
    fn matching_routes(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        label: &str,
        out: &mut Vec<RouteMatch>,
    ) {
        if let Some(semantics) = &node.semantics {
            let mut action_targets = Vec::new();
            targets(semantics, label, &mut action_targets);
            if !action_targets.is_empty() {
                out.push((
                    node.path.clone(),
                    node.widget.clone(),
                    node.test_id.clone(),
                    frame.semantic_owner_instance(&node.path),
                    action_targets,
                ));
            }
        }
        for child in &node.children {
            matching_routes(frame, child, label, out);
        }
    }
    fn find(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        label: &str,
        instance: u64,
    ) -> Option<(String, String)> {
        if frame.semantic_owner_instance(&node.path) == Some(instance) {
            let mut action_targets = Vec::new();
            if let Some(semantics) = &node.semantics {
                targets(semantics, label, &mut action_targets);
            }
            if let Some(target) = action_targets.into_iter().next() {
                return Some((node.path.clone(), target));
            }
        }
        node.children
            .iter()
            .find_map(|node| find(frame, node, label, instance))
    }
    let inspection = frame.inspection();
    let found = find(frame, &inspection.root, label, instance);
    found.unwrap_or_else(|| {
        let mut candidates = Vec::new();
        matching_routes(frame, &inspection.root, label, &mut candidates);
        panic!(
            "selected app instance {instance} exports no semantic route for {label:?}; matching retained routes (path, widget, test_id, registered owner, action targets): {candidates:#?}"
        )
    })
}
fn browser_semantic_action_for_instance(
    frame: &ui::headless::FrameHarness,
    action_target: &str,
    instance: u64,
) -> (String, ui::a11y::AccessNode) {
    fn action(node: &ui::a11y::AccessNode, action_target: &str) -> Option<ui::a11y::AccessNode> {
        if node.action_target.as_deref() == Some(action_target) {
            return Some(node.clone());
        }
        node.children
            .iter()
            .find_map(|child| action(child, action_target))
    }
    fn find(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        action_target: &str,
        instance: u64,
    ) -> Option<(String, ui::a11y::AccessNode)> {
        if frame.semantic_owner_instance(&node.path) == Some(instance)
            && let Some(action) = node
                .semantics
                .as_ref()
                .and_then(|node| action(node, action_target))
        {
            return Some((node.path.clone(), action));
        }
        node.children
            .iter()
            .find_map(|child| find(frame, child, action_target, instance))
    }
    find(frame, &frame.inspection().root, action_target, instance).unwrap_or_else(|| {
        panic!(
            "selected app instance {instance} exports no semantic action target {action_target:?}"
        )
    })
}
fn browser_semantic_node_for_instance(
    frame: &ui::headless::FrameHarness,
    label: &str,
    instance: u64,
) -> ui::a11y::AccessNode {
    fn access(node: &ui::a11y::AccessNode, label: &str) -> Option<ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(label) {
            return Some(node.clone());
        }
        node.children.iter().find_map(|node| access(node, label))
    }
    fn find(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        label: &str,
        instance: u64,
    ) -> Option<ui::a11y::AccessNode> {
        if frame.semantic_owner_instance(&node.path) == Some(instance)
            && let Some(access) = node.semantics.as_ref().and_then(|node| access(node, label))
        {
            return Some(access);
        }
        node.children
            .iter()
            .find_map(|node| find(frame, node, label, instance))
    }
    find(frame, &frame.inspection().root, label, instance)
        .expect("selected app instance exports its semantic node")
}
fn browser_owned_semantic_node(
    frame: &ui::headless::FrameHarness,
    owner: &str,
    label: &str,
) -> ui::a11y::AccessNode {
    fn access(node: &ui::a11y::AccessNode, label: &str) -> Option<ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(label) {
            return Some(node.clone());
        }
        node.children.iter().find_map(|node| access(node, label))
    }
    fn find<'a>(
        node: &'a ui::headless::InspectionNode,
        owner: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        (node.path == owner)
            .then_some(node)
            .or_else(|| node.children.iter().find_map(|node| find(node, owner)))
    }
    let inspection = frame.inspection();
    find(&inspection.root, owner)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|node| access(node, label))
        .expect("exported browser semantic node")
}
#[test]
fn focused_calculator_expression_clear_and_type_preserve_the_semantic_editor() {
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1280, 720),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let calculator = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["label"] == "Calculator")
        .unwrap()["route"]
        .clone();
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: serde_json::from_value(calculator).unwrap(),
            },
        )
        .unwrap();
    let (owner, expression_target) = browser_semantic_route(&frame, "Expression");
    handle
        .perform_semantic_input(
            &mut frame,
            &owner,
            Some(&expression_target),
            &ui::a11y::Action::Focus,
        )
        .unwrap();
    let keyboard_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("Calculator owns keyboard input");
    for value in ["2", ""] {
        handle
            .perform_semantic_input(
                &mut frame,
                &owner,
                Some(&expression_target),
                &ui::a11y::Action::SetValue(value.into()),
            )
            .unwrap();
    }
    let expression = browser_owned_semantic_node(&frame, &owner, "Expression");
    assert_eq!(expression.value.as_deref(), Some(""));
    assert!(!expression.state.focused);
    assert!(
        browser_owned_semantic_node(&frame, &owner, "Calculator")
            .state
            .focused
    );
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(keyboard_owner.as_str())
    );

    handle
        .perform_semantic_input(
            &mut frame,
            &owner,
            Some(&expression_target),
            &ui::a11y::Action::SetValue("2^2".into()),
        )
        .unwrap();
    let expression = browser_owned_semantic_node(&frame, &owner, "Expression");
    assert_eq!(expression.value.as_deref(), Some("2^2"));
    assert!(!expression.state.focused);
    assert!(
        browser_owned_semantic_node(&frame, &owner, "Calculator")
            .state
            .focused
    );
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(keyboard_owner.as_str())
    );
    assert_eq!(
        browser_owned_semantic_node(&frame, &owner, "Result 4")
            .name
            .as_deref(),
        Some("Result 4")
    );

    frame.key("Enter");
    let calculator = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.calculator")
        .unwrap()
        .clone();
    let history = calculator
        .store
        .read(&calculator.checkout.id, |doc| {
            calculator::doc::load(doc).unwrap().history
        })
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].expr, "2^2");
    assert_eq!(history[0].result, "4");

    let (_, evaluate_target) = browser_semantic_route(&frame, "Evaluate expression");
    handle
        .perform_semantic_input(
            &mut frame,
            &owner,
            Some(&evaluate_target),
            &ui::a11y::Action::Focus,
        )
        .unwrap();
    assert!(
        !browser_owned_semantic_node(&frame, &owner, "Expression")
            .state
            .focused,
        "focusing another Calculator control must release semantic Expression focus"
    );
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(keyboard_owner.as_str())
    );
}
#[test]
fn selected_calculator_enter_commits_only_its_checked_record_after_local_history_opens() {
    let (root, handle, _) = fixture_with_extra_calculators(2, |tree| {
        let groups = &mut tree.workspaces[0].strips[0].window_groups;
        let tabs = std::mem::take(&mut groups[0].tabs);
        let (calculators, remaining): (Vec<_>, Vec<_>) =
            tabs.into_iter().partition(|tab| tab.title == "Calculator");
        assert_eq!(
            calculators.len(),
            3,
            "fixture must expose three Calculator records"
        );
        groups[0].tabs = remaining;
        groups[0].active_tab = groups[0].tabs[0].id;
        for (offset, calculator) in calculators.into_iter().enumerate() {
            groups.push(WindowGroup {
                id: 4 + offset as u64,
                active_tab: calculator.id,
                tabs: vec![calculator],
                width: WindowWidth::Normal,
                maximized: false,
            });
        }
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1280, 720),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let navigation = handle.navigation.borrow();
    let calculator_routes = navigation["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["label"] == "Calculator")
        .collect::<Vec<_>>();
    assert_eq!(
        calculator_routes.len(),
        3,
        "each Calculator group must remain addressable"
    );
    let target_route = calculator_routes
        .into_iter()
        .find(|entry| entry["route"]["group"] == "5")
        .expect("middle Calculator group must have an exact desktop route")["route"]
        .clone();
    drop(navigation);
    let target_tab = target_route["tab"]
        .as_str()
        .expect("route has a canonical tab identity")
        .parse::<u64>()
        .unwrap();
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: serde_json::from_value(target_route.clone()).unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        active_route(&handle.tree.borrow()),
        Some(serde_json::from_value(target_route).unwrap()),
        "the intended Calculator route must be active before input"
    );
    let default_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("selected Calculator must own native keyboard input");
    assert_eq!(
        frame.semantic_owner_instance(&default_owner),
        Some(target_tab),
        "route selection must activate the exact Calculator instance before semantic input"
    );
    let (initial_history_owner, initial_history) =
        browser_semantic_action_for_instance(&frame, "calculator.history.toggle", target_tab);
    assert_eq!(
        frame.semantic_owner_instance(&initial_history_owner),
        Some(target_tab),
        "the selected Calculator must export the exact local History owner"
    );
    match initial_history.name.as_deref() {
        Some("Hide history") => {
            handle
                .perform_semantic_input(
                    &mut frame,
                    &initial_history_owner,
                    Some("calculator.history.toggle"),
                    &ui::a11y::Action::Activate,
                )
                .unwrap();
            let (_, closed_history) = browser_semantic_action_for_instance(
                &frame,
                "calculator.history.toggle",
                target_tab,
            );
            assert_eq!(
                closed_history.name.as_deref(),
                Some("Show history"),
                "closing the selected Calculator drawer must expose its Show history control"
            );
        }
        Some("Show history") => {}
        name => {
            panic!("selected Calculator history toggle has unexpected semantic name: {name:?}")
        }
    }
    let (history_owner, history) =
        browser_semantic_action_for_instance(&frame, "calculator.history.toggle", target_tab);
    assert_eq!(history.name.as_deref(), Some("Show history"));
    handle
        .perform_semantic_input(
            &mut frame,
            &history_owner,
            Some("calculator.history.toggle"),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    let (_, open_history) =
        browser_semantic_action_for_instance(&frame, "calculator.history.toggle", target_tab);
    assert_eq!(
        open_history.name.as_deref(),
        Some("Hide history"),
        "the selected Calculator's local drawer must be open without switching routes"
    );
    let (owner, expression_target) =
        browser_semantic_route_for_instance(&frame, "Expression", target_tab);
    assert_eq!(
        frame.semantic_owner_instance(&owner),
        Some(target_tab),
        "the selected Calculator must export the exact Expression semantic owner"
    );
    handle
        .perform_semantic_input(
            &mut frame,
            &owner,
            Some(&expression_target),
            &ui::a11y::Action::Focus,
        )
        .unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(owner.as_str()),
        "the exact Expression owner must receive the subsequent native Enter"
    );
    handle
        .perform_semantic_input(
            &mut frame,
            &owner,
            Some(&expression_target),
            &ui::a11y::Action::SetValue("2^2".into()),
        )
        .unwrap();
    assert_eq!(
        browser_semantic_node_for_instance(&frame, "Result 4", target_tab)
            .name
            .as_deref(),
        Some("Result 4"),
        "the selected Calculator received the semantic expression input"
    );
    let target_record = ui::record::RecordId::for_tab(target_tab);
    let before = handle
        .records
        .borrow()
        .values()
        .filter(|record| record.app == "so.tilo.calculator")
        .map(|record| {
            (
                record.checkout.id.clone(),
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.base_heads().to_vec())
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(before.len(), 3);
    assert!(
        before.iter().any(|(id, _)| id == &target_record),
        "the exact routed Calculator record must be captured before Enter"
    );
    frame.key("Enter");
    assert_eq!(
        browser_semantic_node_for_instance(&frame, "Expression", target_tab)
            .value
            .as_deref(),
        Some("4"),
        "Enter must replace the selected Calculator expression with its committed result"
    );
    for record in handle
        .records
        .borrow()
        .values()
        .filter(|record| record.app == "so.tilo.calculator")
    {
        let (heads, history) = record
            .store
            .read(&record.checkout.id, |doc| {
                (
                    doc.base_heads().to_vec(),
                    calculator::doc::load(doc).unwrap().history,
                )
            })
            .unwrap();
        let previous_heads = before
            .iter()
            .find(|(id, _)| id == &record.checkout.id)
            .map(|(_, heads)| heads)
            .expect("each checked Calculator record was captured before Enter");
        if record.checkout.id == target_record {
            assert_ne!(
                heads, *previous_heads,
                "Enter must durably commit the selected record"
            );
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].expr, "2^2");
            assert_eq!(history[0].result, "4");
        } else {
            assert_eq!(
                heads, *previous_heads,
                "Enter must not commit another Calculator record"
            );
            assert!(
                history.is_empty(),
                "Enter must not add history to another Calculator"
            );
        }
    }
    assert!(
        handle.errors.borrow().is_empty(),
        "the mounted owner/permission path must not reject the committed Calculator action: {:?}",
        handle.errors.borrow()
    );
}
#[test]
fn desktop_cold_keyboard_focus_reveals_chat_without_prior_writer_focus() {
    let (mut root, handle, _storage) = fixture();
    let mut topology = handle.tree.borrow().clone();
    let groups = &mut topology.workspaces[0].strips[0].window_groups;
    let chat = groups[0].tabs.remove(1);
    groups[0].tabs.pop();
    groups.push(WindowGroup {
        id: 4,
        active_tab: chat.id,
        tabs: vec![chat],
        width: WindowWidth::Normal,
        maximized: false,
    });
    topology.active_strip.insert(topology.active, StripId(2));
    topology.active_group.insert(StripId(2), 3);
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&topology),
            source_heads: vec![[7; 32]],
            tree: topology,
            routes: root.resolver.host.routes.clone(),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    root.reconcile();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        root,
        (740, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 3);
    let (owner, target) = browser_semantic_route(&frame, "Message composer");
    frame
        .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus)
        .unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(owner.as_str())
    );
    assert_eq!(
        handle.tree.borrow().active_group[&StripId(2)],
        4,
        "authoritative cold Chat focus must outrank retained Writer focus flags"
    );
    unsafe {
        (&mut *root_ptr)
            .surface
            .row_mut(StripId(2))
            .unwrap()
            .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
    }
    frame.relayout();
    let row = unsafe { (&*root_ptr).surface.row(StripId(2)).unwrap() };
    assert!(row.groups[1].focused());
    assert!(row.groups[1].get_rect().x1 <= row.get_rect().x1 + 0.5);
}

#[test]
fn desktop_cold_chat_pointer_and_explicit_focus_own_first_key_and_ime() {
    fn access(node: &ui::headless::InspectionNode, label: &str) -> Option<ui::a11y::AccessNode> {
        fn semantic(node: &ui::a11y::AccessNode, label: &str) -> Option<ui::a11y::AccessNode> {
            if node.name.as_deref() == Some(label) {
                return Some(node.clone());
            }
            node.children
                .iter()
                .find_map(|child| semantic(child, label))
        }
        if node.test_id.as_deref() == Some(label) {
            return node.semantics.clone();
        }
        node.semantics
            .as_ref()
            .and_then(|node| semantic(node, label))
            .or_else(|| node.children.iter().find_map(|node| access(node, label)))
    }
    for (pointer, beforeinput, pointer_move) in [
        (true, false, false),
        (false, false, false),
        (true, true, true),
        (true, false, true),
    ] {
        let (mut root, handle, _storage) = fixture();
        let mut topology = handle.tree.borrow().clone();
        let groups = &mut topology.workspaces[0].strips[0].window_groups;
        let chat = groups[0].tabs.remove(1);
        groups[0].tabs.pop();
        groups.push(WindowGroup {
            id: 4,
            active_tab: chat.id,
            tabs: vec![chat],
            width: WindowWidth::Normal,
            maximized: false,
        });
        topology.active_strip.insert(topology.active, StripId(2));
        topology.active_group.insert(StripId(2), 3);
        *handle.inbox.borrow_mut() = Some((
            Admission {
                admitted_room: history_document(&topology),
                source_heads: vec![[7; 32]],
                tree: topology,
                routes: root.resolver.host.routes.clone(),
                required: vec![],
                missing: vec![],
                unsupported: vec![],
                pending: 0,
            },
            handle.permissions.borrow().clone(),
        ));
        root.reconcile();
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::state::State::new(),
            root,
            (740, 720),
            ui::Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        assert!(
            access(&frame.inspection().root, "word_processor")
                .unwrap()
                .state
                .focused,
            "cold Writer retains a local focus flag"
        );
        assert!(
            access(&frame.inspection().root, "Message composer")
                .unwrap()
                .state
                .focused,
            "cold Chat already has a local composer caret"
        );
        let before: Vec<_> = handle
            .records
            .borrow()
            .values()
            .filter(|record| record.app != "so.tilo.chat")
            .map(|record| {
                (
                    record.clone(),
                    record
                        .store
                        .read(&record.checkout.id, |doc| doc.clone().save())
                        .unwrap(),
                )
            })
            .collect();
        let (owner, target) = browser_semantic_route(&frame, "Message composer");
        if pointer {
            let rect = access(&frame.inspection().root, "Message composer")
                .unwrap()
                .bounds
                .unwrap()
                .to_rect();
            // The real partially visible composer, without any previous
            // Writer gesture or semantic activation warming its flags.
            if pointer_move {
                // Real pointer streams can move while pressed without
                // selecting any text, especially in an empty composer.
                use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
                let moved = WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: winit::dpi::PhysicalPosition::new(
                        (rect.x0 + 740.0) / 2.0,
                        rect.center().y,
                    ),
                };
                frame.send_host_event(&moved);
                frame.send_host_event(&WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                });
                frame.send_host_event(&moved);
                frame.send_host_event(&WindowEvent::MouseInput {
                    device_id: DeviceId::dummy(),
                    state: ElementState::Released,
                    button: MouseButton::Left,
                });
            } else {
                frame.click_at((rect.x0 + 740.0) / 2.0, rect.center().y);
            }
        } else {
            frame
                .perform_semantic_named(&owner, Some(&target), &ui::a11y::Action::Focus)
                .unwrap();
        }
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str()),
            "cold pointer={pointer}"
        );
        assert!(
            !access(&frame.inspection().root, "word_processor")
                .unwrap()
                .state
                .focused,
            "the previous visible group must lose its focused control"
        );
        assert!(
            frame.frame_output().ime.is_some(),
            "cold Chat must expose its actual IME owner"
        );
        // Exercise both ordinary keys and the hidden editor's beforeinput
        // route, which delivers committed text through IME.
        if beforeinput {
            use winit::event::{Ime, Modifiers, WindowEvent};
            frame.send_host_event(&WindowEvent::Focused(true));
            frame.send_host_event(&WindowEvent::Ime(Ime::Enabled));
            // Printable hidden-editor keys publish modifier snapshots,
            // but the text itself is delivered only by beforeinput/IME.
            frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::default()));
            frame.send_host_event(&WindowEvent::Ime(Ime::Commit("N".into())));
            frame.send_host_event(&WindowEvent::ModifiersChanged(Modifiers::default()));
        } else {
            frame.key("N");
        }
        assert_eq!(
            access(&frame.inspection().root, "Message composer")
                .unwrap()
                .value
                .as_deref(),
            Some("N")
        );
        let selection = access(&frame.inspection().root, "Message composer")
            .unwrap()
            .text_selection
            .unwrap();
        assert_eq!(
            (selection.anchor, selection.focus),
            (1, 1),
            "first insertion must leave a collapsed caret"
        );
        if pointer_move {
            if beforeinput {
                frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
                    "x".into(),
                )));
            } else {
                frame.key("x");
            }
            assert_eq!(
                access(&frame.inspection().root, "Message composer")
                    .unwrap()
                    .value
                    .as_deref(),
                Some("Nx")
            );
            frame.key("Backspace");
        }
        frame.type_text("avigation two-peer check");
        for event in [
            winit::event::WindowEvent::Focused(true),
            winit::event::WindowEvent::Ime(winit::event::Ime::Enabled),
            winit::event::WindowEvent::Ime(winit::event::Ime::Commit(" IME".into())),
            winit::event::WindowEvent::Ime(winit::event::Ime::Disabled),
            winit::event::WindowEvent::Focused(false),
            winit::event::WindowEvent::Focused(true),
            winit::event::WindowEvent::Ime(winit::event::Ime::Enabled),
            winit::event::WindowEvent::Ime(winit::event::Ime::Commit(" resumed".into())),
        ] {
            frame.send_host_event(&event);
        }
        assert_eq!(
            access(&frame.inspection().root, "Message composer")
                .unwrap()
                .value
                .as_deref(),
            Some("Navigation two-peer check IME resumed")
        );
        assert_eq!(
            frame.inspection().keyboard_focus_owner.as_deref(),
            Some(owner.as_str())
        );
        assert!(frame.frame_output().ime.is_some());
        assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 4);
        for (record, bytes) in before {
            assert_eq!(
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
                bytes,
                "cold pointer={pointer} typing must not mutate {}",
                record.app
            );
        }
        // SAFETY: the boxed root stays in the harness and this access ends before its next call.
        unsafe {
            (&mut *root_ptr)
                .surface
                .row_mut(StripId(2))
                .unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        frame.relayout();
    }
}

#[test]
fn desktop_navigation_reveals_real_chat_and_preserves_local_records_and_return_focus() {
    let (mut root, handle, storage) = fixture();
    let mut topology = handle.tree.borrow().clone();
    let groups = &mut topology.workspaces[0].strips[0].window_groups;
    let chat = groups[0].tabs.remove(1);
    groups[0].tabs.pop();
    groups.push(WindowGroup {
        id: 4,
        active_tab: chat.id,
        tabs: vec![chat],
        width: WindowWidth::Normal,
        maximized: false,
    });
    topology.active_strip.insert(topology.active, StripId(2));
    topology.active_group.insert(StripId(2), 4);
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&topology),
            source_heads: vec![[7; 32]],
            tree: topology,
            routes: root.resolver.host.routes.clone(),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    root.reconcile();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        root,
        (740, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    assert_eq!(
        handle.tree.borrow().active_group[&StripId(2)],
        4,
        "mounting retained apps is not a new local focus gesture"
    );
    fn rect(node: &ui::headless::InspectionNode, label: &str) -> Option<Rect> {
        fn semantic(node: &ui::a11y::AccessNode, label: &str) -> Option<Rect> {
            if node.name.as_deref() == Some(label) {
                return node.bounds.map(|bounds| bounds.to_rect());
            }
            node.children
                .iter()
                .find_map(|child| semantic(child, label))
        }
        node.semantics
            .as_ref()
            .and_then(|node| semantic(node, label))
            .or_else(|| node.children.iter().find_map(|child| rect(child, label)))
    }
    // SAFETY: frame retains the boxed root; each exclusive access ends
    // before invoking a harness method or using another root reference.
    let settle = |frame: &mut ui::headless::FrameHarness| {
        unsafe {
            (&mut *root_ptr)
                .surface
                .row_mut(StripId(2))
                .unwrap()
                .tick_snap_at(ui::clock::Instant::now() + ui::clock::Duration::from_secs(1));
        }
        frame.relayout();
    };
    frame.click_named("word_processor").unwrap();
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "Retained draft".into(),
    )));
    settle(&mut frame);
    let before: Vec<_> = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            (
                r.clone(),
                r.store
                    .read(&r.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
            )
        })
        .collect();
    let writer_owner = frame.inspection().keyboard_focus_owner.unwrap();
    let (composer, _) = browser_semantic_route(&frame, "Message composer");
    let composer_rect = rect(&frame.inspection().root, "Message composer").unwrap();
    assert!(
        composer_rect.x0 < 740.0 && composer_rect.x1 > 740.0,
        "real narrow-panel precondition: {composer_rect:?}"
    );
    // Physical pointer at the visible part of the existing composer.
    frame.click_at((composer_rect.x0 + 740.0) / 2.0, composer_rect.center().y);
    frame.poll_frame().unwrap();
    settle(&mut frame);
    assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 4);
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(composer.as_str())
    );
    let send_rect = rect(&frame.inspection().root, "Send message").unwrap();
    assert!(
        send_rect.x1 <= 740.0 && send_rect.x0 >= 0.0,
        "Send must be usable: {send_rect:?}"
    );
    unsafe {
        let row = (&*root_ptr).surface.row(StripId(2)).unwrap();
        assert!(row.groups[1].focused());
        assert!(!row.groups[0].focused());
        let saved: LocalView =
            serde_json::from_slice(&storage.get(&(&*root_ptr).view_key).unwrap().bytes).unwrap();
        assert_eq!(saved.groups[&StripId(2)], 4);
    }
    for (record, bytes) in &before {
        assert_eq!(
            record
                .store
                .read(&record.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            *bytes,
            "pointer navigation alone must not alter shared records"
        );
    }
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "Chat pointer draft".into(),
    )));
    let chat = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.chat")
        .unwrap()
        .clone();
    assert!(
        chat.store
            .read(&chat.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("Chat pointer draft")
    );
    for (record, bytes) in &before {
        if record.app != "so.tilo.chat" {
            assert_eq!(
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
                *bytes,
                "Chat typing must not reach the previously focused Writer"
            );
        }
    }
    let before: Vec<_> = handle
        .records
        .borrow()
        .values()
        .map(|r| {
            (
                r.clone(),
                r.store
                    .read(&r.checkout.id, |doc| doc.clone().save())
                    .unwrap(),
            )
        })
        .collect();
    // Keyboard/semantic focus returns to the same retained editor and caret.
    frame
        .perform_semantic_named(&writer_owner, None, &ui::a11y::Action::Focus)
        .unwrap();
    frame.poll_frame().unwrap();
    settle(&mut frame);
    assert_eq!(handle.tree.borrow().active_group[&StripId(2)], 3);
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(writer_owner.as_str())
    );
    assert!(frame.inspection_json().unwrap().contains("Retained draft"));
    for (record, bytes) in before {
        assert_eq!(
            record
                .store
                .read(&record.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            bytes,
            "navigation must not alter {} shared bytes",
            record.app
        );
    }
    assert!(handle.session_requests.borrow().is_empty());
    assert!(handle.errors.borrow().is_empty());
    // A local group shortcut may leave Writer's retained caret suspended.
    // A subsequent remote strip insertion must not turn it into new input.
    handle.tree.borrow_mut().active_group.insert(StripId(2), 4);
    frame.relayout();
    settle(&mut frame);
    let mut refreshed = handle.tree.borrow().clone();
    refreshed.workspaces[0].strips.push(Strip {
        id: StripId(99),
        name: "Remote strip".into(),
        window_groups: vec![],
    });
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&refreshed),
            source_heads: vec![[7; 32]],
            tree: refreshed,
            routes: unsafe { (&*root_ptr).resolver.host.routes.clone() },
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    frame.relayout();
    frame.relayout();
    settle(&mut frame);
    assert_eq!(
        handle.tree.borrow().active_group[&StripId(2)],
        4,
        "retained Writer caret is not a new focus gesture after remote rebuild"
    );
}

/// Alt+Tab itself belongs to the operating system in a browser, so
/// the chord that reaches the page walks the focused app's windows.
#[test]
fn alt_grave_switches_between_an_apps_windows_through_the_shared_switcher() {
    let calculators = std::cell::RefCell::new(Vec::new());
    let (root, handle, _) = fixture_with_extra_calculators(1, |tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 3 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
        calculators.borrow_mut().extend(
            strip
                .window_groups
                .iter()
                .filter(|group| {
                    matches!(&group.tabs[0].tile, TileContent::AppPlaceholder {
                        kind: AppKind::Other(app), ..
                    } if app == "so.tilo.calculator")
                })
                .map(|group| group.id),
        );
        let first = calculators.borrow()[0];
        tree.active_group.insert(strip.id, first);
    });
    let calculators = calculators.into_inner();
    assert_eq!(calculators.len(), 2, "{calculators:?}");
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    settle_external_navigation(root_ptr, &mut frame);
    let strip = handle.tree.borrow().workspaces[0].strips[0].id;
    assert_eq!(handle.tree.borrow().active_group[&strip], calculators[0]);

    frame.set_modifiers(&["alt".into()]);
    frame.key("`");
    // SAFETY: `frame` retains this boxed root and no harness method runs
    // while the temporary reference is alive.
    assert!(unsafe { (&*root_ptr).window_switcher.is_active() });
    frame.set_modifiers(&[]);
    frame.relayout();

    assert!(!unsafe { (&*root_ptr).window_switcher.is_active() });
    assert_eq!(handle.tree.borrow().active_group[&strip], calculators[1]);
}

#[test]
fn desktop_group_focus_selects_its_strip_and_rejects_stale_identity() {
    let (root, handle, _) = fixture();
    let mut tree = handle.tree.borrow().clone();
    let mut second = tree.workspaces[0].strips[0].clone();
    second.id = StripId(9);
    second.window_groups[0].id = 90;
    tree.workspaces[0].strips.push(second);
    tilo_desktop::intents::apply_local_view(
        &mut tree,
        LocalViewIntent::Group {
            strip: StripId(9),
            group: 90,
        },
    );
    assert_eq!(tree.active_strip[&tree.active], StripId(9));
    assert_eq!(tree.active_group[&StripId(9)], 90);
    let before = serde_json::to_vec(&tree).unwrap();
    tilo_desktop::intents::apply_local_view(
        &mut tree,
        LocalViewIntent::Group {
            strip: StripId(2),
            group: 90,
        },
    );
    assert_eq!(serde_json::to_vec(&tree).unwrap(), before);
    assert!(root.handle.session_requests.borrow().is_empty());
}

#[test]
fn browser_external_tab_activation_updates_local_selection_and_preserves_real_input() {
    let (root, handle, _) = fixture();
    let mut topology = handle.tree.borrow().clone();
    let groups = &mut topology.workspaces[0].strips[0].window_groups;
    let chat = groups[0].tabs.remove(1);
    groups[0].tabs.pop();
    groups.push(WindowGroup {
        id: 4,
        active_tab: chat.id,
        tabs: vec![chat],
        width: WindowWidth::Normal,
        maximized: false,
    });
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&topology),
            source_heads: vec![[7; 32]],
            tree: topology,
            routes: root.resolver.host.routes.clone(),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        root,
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.poll_frame().unwrap();
    for tab in [(5 << 48) | 12, (5 << 48) | 10] {
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: crate::desktop_navigation::Route {
                    workspace: "1".into(),
                    strip: "2".into(),
                    group: "3".into(),
                    tab: tab.to_string(),
                },
            });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok()
        );
        assert_eq!(
            handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
            tab
        );
    }
    assert_eq!(
        unsafe {
            (&*root_ptr).surface.row(StripId(2)).unwrap().groups[0]
                .children()
                .len()
        },
        1,
        "external navigation omits native tab semantics"
    );
    let chat = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.chat")
        .unwrap()
        .clone();
    let chat_before = chat
        .store
        .read(&chat.checkout.id, |doc| doc.clone().save())
        .unwrap();
    fn writer_editable_rect(node: &ui::headless::InspectionNode) -> Option<Rect> {
        if node.test_id.as_deref() == Some("word_processor") && node.focusable {
            return Some(Rect::new(
                node.rect[0],
                node.rect[1],
                node.rect[2],
                node.rect[3],
            ));
        }
        node.children.iter().find_map(writer_editable_rect)
    }
    let writer_rect =
        writer_editable_rect(&frame.inspection().root).expect("real Writer editable coordinate");
    frame.click_at(writer_rect.center().x, writer_rect.center().y);
    assert!(
        frame.frame_output().ime.is_some(),
        "clicked Writer must own the native IME route"
    );
    frame.send_host_event(&winit::event::WindowEvent::Focused(true));
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Enabled));
    frame.send_host_event(&winit::event::WindowEvent::ModifiersChanged(
        winit::event::Modifiers::default(),
    ));
    // Browser beforeinput forwards committed text through this native event.
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "Two-group pointer Writer".into(),
    )));
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    assert!(
        writer
            .store
            .read(&writer.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("Two-group pointer Writer")
    );
    assert_eq!(
        chat.store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        chat_before,
        "Writer input must not mutate Chat"
    );
    let writer_before = writer
        .store
        .read(&writer.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let (composer, composer_target) = browser_semantic_route(&frame, "Message composer");
    frame
        .perform_semantic_named(&composer, Some(&composer_target), &ui::a11y::Action::Focus)
        .unwrap();
    frame
        .perform_semantic_named(
            &composer,
            Some(&composer_target),
            &ui::a11y::Action::SetValue("Two-group Chat draft".into()),
        )
        .unwrap();
    frame.key("Enter");
    assert!(
        chat.store
            .read(&chat.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("Two-group Chat draft")
    );
    assert_eq!(
        writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        writer_before,
        "Chat input must not mutate Writer"
    );
    *handle.navigation_command.borrow_mut() = Some(crate::desktop_navigation::Command::Select {
        generation: handle.generation.clone(),
        route: crate::desktop_navigation::Route {
            workspace: "1".into(),
            strip: "2".into(),
            group: "3".into(),
            tab: ((5_u64 << 48) | 12).to_string(),
        },
    });
    frame.relayout();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    frame.poll_frame().unwrap();
    assert_eq!(
        handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
        (5 << 48) | 12,
        "external Notes selection"
    );
}
#[test]
fn actual_desktop_hosts_checked_writer_chat_notes_calculator_and_preserves_writer_route() {
    let (root, handle, cache) = fixture();
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(root), (1280, 720));
    frame.poll_frame().unwrap();
    assert_eq!(handle.records.borrow().len(), 4);
    assert!(
        frame
            .resources()
            .iter()
            .any(|r| r.kind == "word_processor.document")
    );
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.type_text("Retained canonical desktop");
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    assert_eq!(writer.checkout.id.as_str(), "tab:0000000000000001");
    assert!(
        writer
            .store
            .read(&writer.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("Retained canonical desktop")
    );
    for (instance, resource, action, value) in [
        (
            (5 << 48) | 11,
            "chat.channel:c_general",
            "post",
            "Desktop message",
        ),
        ((5 << 48) | 12, "notes.notebook", "new-page", "Desktop page"),
        (
            (5 << 48) | 13,
            "calculator.workspace",
            "add-function",
            "sin(x)",
        ),
    ] {
        frame
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: tilo_actions::ActionAddress::new(resource, action),
                },
                serde_json::json!({"value":value}).as_object().unwrap(),
            )
            .unwrap();
    }
    frame.close_components();
    let mut reports = frame.take_component_close_reports();
    assert_eq!(reports.len(), 4);
    for write in cache.pending_writes() {
        cache.acknowledge(&write, Ok(())).unwrap();
    }
    for report in &mut reports {
        report.retry_record().unwrap();
        report.retry_view().unwrap();
    }
}
/// Mount the desktop fixture with the framing animation settled and the
/// Writer focused, so the next frame is an ordinary retained one.
pub(super) fn settled_writer_frame() -> (ui::headless::FrameHarness, DesktopHandle) {
    let (root, handle, _) = fixture();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    settle_external_navigation(root_ptr, &mut frame);
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.type_text("a");
    frame.relayout();
    (frame, handle)
}

/// LIVE_V1: the real desktop publishes the focused Writer's live
/// record, window rectangle and caret anchor, and a remote caret on that
/// anchor is drawn by the Writer on the next frame without a synchronous
/// relayout.
#[test]
fn live_view_publishes_writer_focus_and_draws_a_remote_caret_without_relayout() {
    let (mut frame, handle) = settled_writer_frame();
    let layout = handle.live.borrow().clone();
    assert!(
        !layout.groups.is_empty() && layout.focused_group.is_some(),
        "{layout:?}"
    );
    assert!(layout.groups.iter().all(|g| g.rect.width() > 0.0));
    let snapshot = crate::live_view::snapshot_json(
        &layout,
        &handle.tab_records.borrow(),
        frame.focus_presence_payload(),
    );
    assert_eq!(
        snapshot["focus"]["app"], "so.tilo.word-processor",
        "{snapshot}"
    );
    assert_eq!(snapshot["focus"]["record"], "tab:0000000000000001");
    let rect = snapshot["focusRect"]
        .as_array()
        .expect("focused window rect");
    assert!(rect[2].as_f64().unwrap() > 100.0 && rect[3].as_f64().unwrap() > 100.0);
    let anchor =
        crate::live_view::unhex(snapshot["selection"]["anchor"].as_str().unwrap()).unwrap();
    let head = crate::live_view::unhex(snapshot["selection"]["head"].as_str().unwrap()).unwrap();
    let (key, start, end) = crate::live_view::decode_selection(&anchor, &head).unwrap();
    assert_eq!((start, end), (1, 1), "caret after the typed character");
    assert!(matches!(&key, ui::presence::PresenceKey::Widget { path } if path.len() == 2));

    // The window's editor caret is published even without keyboard focus
    // (focus left behind elsewhere, or an action moved the caret).
    assert!(
        layout
            .window_presence
            .iter()
            .any(|(window_key, _)| *window_key == key),
        "{:?}",
        layout.window_presence
    );
    let unfocused = crate::live_view::snapshot_json(
        &layout,
        &handle.tab_records.borrow(),
        crate::live_view::published_selection(None, &layout.window_presence),
    );
    assert_eq!(unfocused["selection"], snapshot["selection"]);

    let paths_before = frame.frame_render_input().scene.encoding().path_tags.len();
    let relayouts = frame.relayout_frames();
    let mut projection = crate::live_view::RemoteProjection::default();
    let changed = frame.update_presence(|presence| {
        projection.apply(
            presence,
            vec![crate::live_view::RemoteInput {
                publisher: "ab12".into(),
                name: Some("Ada".into()),
                color: Some("#ec4899".into()),
                selection: Some(crate::live_view::RemoteSelectionInput {
                    anchor: crate::live_view::hex(&anchor),
                    head: crate::live_view::hex(&head),
                }),
                typing: None,
            }],
        )
    });
    assert!(changed);
    assert_eq!(
        frame.relayout_frames(),
        relayouts,
        "no synchronous relayout"
    );
    assert!(
        frame.frame_schedule().needs_redraw,
        "the next frame redraws"
    );
    assert!(frame.poll_frame().unwrap());
    let user = ui::permissions::UserId("ab12".into());
    assert_eq!(frame.presence().caret_for(&key, &user), Some(1));
    let paths_after = frame.frame_render_input().scene.encoding().path_tags.len();
    assert!(
        paths_after > paths_before,
        "the Writer painted the remote caret and label ({paths_before} -> {paths_after})"
    );
    // Same input again: nothing to redraw.
    let unchanged = frame.update_presence(|presence| {
        projection.apply(
            presence,
            vec![crate::live_view::RemoteInput {
                publisher: "ab12".into(),
                name: Some("Ada".into()),
                color: Some("#ec4899".into()),
                selection: Some(crate::live_view::RemoteSelectionInput {
                    anchor: crate::live_view::hex(&anchor),
                    head: crate::live_view::hex(&head),
                }),
                typing: None,
            }],
        )
    });
    assert!(!unchanged);
    // Leaving clears the caret on the next frame.
    assert!(frame.update_presence(|presence| projection.apply(presence, vec![])));
    frame.poll_frame().unwrap();
    assert_eq!(frame.presence().caret_for(&key, &user), None);
    assert_eq!(
        frame.frame_render_input().scene.encoding().path_tags.len(),
        paths_before
    );
}

#[test]
fn writer_typing_retains_the_desktop_layout_tree() {
    let (mut frame, handle) = settled_writer_frame();
    let ids = frame.layout_node_ids();
    let focused = frame.focused();
    assert!(focused.is_some(), "the Writer owns keyboard focus");
    frame.set_retained_geometry_check(1);
    frame.type_text("b");
    let stats = frame.last_relayout();
    assert!(
        !stats.rebuilt,
        "a typed key must not recompose the desktop: {stats:?}"
    );
    assert_eq!(frame.layout_node_ids(), ids, "node ids survive a typed key");
    assert_eq!(frame.focused(), focused, "focus survives a typed key");
    assert!(
        frame
            .perform_named("word_processor", &ui::a11y::Action::Focus)
            .is_ok(),
        "bindings still resolve the Writer after a retained frame"
    );
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    assert!(
        writer
            .store
            .read(&writer.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("ab"),
        "the retained frame still committed the typed text"
    );
    frame.verify_retained_geometry().unwrap();
}

#[test]
fn writer_typing_re_measures_no_desktop_leaf() {
    // The Writer fills its tile and draws itself, the tab bar has a fixed
    // height and every other host is inactive: a typed key changes no
    // size anywhere, so taffy has nothing to re-measure.
    let (mut frame, _handle) = settled_writer_frame();
    frame.type_text("b");
    let stats = frame.last_relayout();
    assert!(!stats.rebuilt, "{stats:?}");
    assert_eq!(
        stats.marked_leaves, 0,
        "a typed key re-measures no leaf: {stats:?}"
    );
    assert!(
        !stats.measured,
        "a typed key needs no taffy re-solve: {stats:?}"
    );
}

#[test]
fn pointer_motion_over_each_app_body_is_inert_once_settled() {
    // A pointer resting over any app must not keep
    // the desktop repainting. The first move may change hover; a second
    // move inside the same spot must be skipped, and when it is not the
    // dirty paths name the widget that still marks itself.
    let (mut frame, handle) = settled_writer_frame();
    frame.set_pointer_redraw_diagnostics(true);
    let device_id = winit::event::DeviceId::dummy();
    let move_to = |x: f64, y: f64| winit::event::WindowEvent::CursorMoved {
        device_id,
        position: winit::dpi::PhysicalPosition::new(x, y),
    };
    for (label, tab) in [
        ("word_processor", (5u64 << 48) | 10),
        ("chat", (5u64 << 48) | 11),
        ("notes", (5u64 << 48) | 12),
        ("calculator", (5u64 << 48) | 13),
    ] {
        *handle.navigation_command.borrow_mut() =
            Some(crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: crate::desktop_navigation::Route {
                    workspace: "1".into(),
                    strip: "2".into(),
                    group: "3".into(),
                    tab: tab.to_string(),
                },
            });
        frame.relayout();
        assert!(
            handle
                .navigation_result
                .borrow_mut()
                .take()
                .unwrap()
                .is_ok(),
            "{label}: selection accepted"
        );
        // Selection animations (group framing, the Notes status bubble)
        // are finite; let them run out before judging idleness.
        std::thread::sleep(
            ui::status_bubble::DEFAULT_LINGER + ui::clock::Duration::from_millis(10),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        // Well inside the tile body, below the tab bar.
        frame.send_host_event(&move_to(640.0, 420.0));
        frame.take_pointer_redraw_sources();
        frame.send_host_event(&move_to(643.0, 422.0));
        let sources = frame.take_pointer_redraw_sources();
        assert!(
            frame.last_send_skipped_relayout(),
            "{label}: settled pointer motion over the app body still produced a frame; dirty paths: {sources:?}"
        );
    }
}

#[test]
fn desktop_resize_rebuilds_and_matches_a_fresh_composition() {
    let (mut frame, _handle) = settled_writer_frame();
    let ids = frame.layout_node_ids();
    frame.resize((1100, 700)).unwrap();
    assert!(
        frame.last_relayout().rebuilt,
        "a viewport change recomposes the desktop"
    );
    assert_eq!(
        frame.layout_node_ids().len(),
        ids.len(),
        "the same composition at a new size"
    );
    let rebuilt = frame.layout_node_ids();
    frame.type_text("c");
    assert!(
        !frame.last_relayout().rebuilt,
        "typing after the resize retains the new tree"
    );
    assert_eq!(frame.layout_node_ids(), rebuilt);
    frame.verify_retained_geometry().unwrap();
}

#[test]
fn desktop_tab_selection_rebuilds_the_composition() {
    let (mut frame, handle) = settled_writer_frame();
    let ids = frame.layout_node_ids();
    *handle.navigation_command.borrow_mut() = Some(crate::desktop_navigation::Command::Select {
        generation: handle.generation.clone(),
        route: crate::desktop_navigation::Route {
            workspace: "1".into(),
            strip: "2".into(),
            group: "3".into(),
            tab: ((5u64 << 48) | 12).to_string(),
        },
    });
    frame.relayout();
    assert!(
        handle
            .navigation_result
            .borrow_mut()
            .take()
            .unwrap()
            .is_ok()
    );
    // Selection may settle over more than one frame; the composition must
    // have been rebuilt by the time Notes is the active host.
    let mut rebuilt = frame.last_relayout().rebuilt;
    for _ in 0..3 {
        if rebuilt {
            break;
        }
        frame.relayout();
        rebuilt |= frame.last_relayout().rebuilt;
    }
    assert!(rebuilt, "activating another tab is a structural change");
    assert_ne!(
        frame.layout_node_ids().len(),
        ids.len(),
        "the active host swapped its subtree for a differently shaped one"
    );
    frame.relayout();
    frame.verify_retained_geometry().unwrap();
}

#[test]
fn admitted_tab_close_retains_unflushed_component_owner_until_retry() {
    let (root, handle, cache) = fixture();
    let mut topology = handle.tree.borrow().clone();
    let routes = root.resolver.host.routes.clone();
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(root), (1280, 720));
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.type_text("Unflushed close custody");
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    topology.workspaces[0].strips[0].window_groups[0]
        .tabs
        .remove(0);
    topology.workspaces[0].strips[0].window_groups[0].active_tab = (5 << 48) | 11;
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&topology),
            source_heads: vec![[8; 32]],
            tree: topology,
            routes,
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    frame.poll_frame().unwrap();
    let mut reports = frame.take_component_close_reports();
    assert!(
        !reports.is_empty(),
        "admitted close transfers real component custody"
    );
    for write in cache.pending_writes() {
        cache
            .acknowledge(&write, Err("disk denied".into()))
            .unwrap();
    }
    assert!(
        reports.iter_mut().any(|r| r.retry_record().is_err()),
        "failed durable flush remains retryable"
    );
    assert!(
        writer
            .store
            .read(&writer.checkout.id, |doc| serde_json::to_string(
                &automerge::AutoSerde::from(doc)
            )
            .unwrap())
            .unwrap()
            .contains("Unflushed close custody")
    );
    for write in cache.pending_writes() {
        cache.acknowledge(&write, Ok(())).unwrap();
    }
    for report in &mut reports {
        report.retry_record().unwrap();
    }
    assert!(reports.iter().all(|r| r.record_retry.is_none()));
}
#[test]
fn admitted_same_workspace_refresh_keeps_pending_selection_and_checked_owner() {
    let (mut root, handle, _) = fixture();
    let before = match root.surface.row_mut(StripId(2)).unwrap().groups[0]
        .host_for_tab_mut((5 << 48) | 10)
        .unwrap()
        .content()
    {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()) as *const (),
        _ => panic!("checked owner"),
    };
    let mut refreshed = handle.tree.borrow().clone();
    refreshed.workspaces[0].strips.push(Strip {
        id: StripId(99),
        name: "Remote strip".into(),
        window_groups: vec![],
    });
    let admission = Admission {
        admitted_room: history_document(&refreshed),
        source_heads: vec![[7; 32]],
        tree: refreshed,
        routes: root.resolver.host.routes.clone(),
        required: vec![],
        missing: vec![],
        unsupported: vec![],
        pending: 0,
    };
    *handle.inbox.borrow_mut() = Some((admission, handle.permissions.borrow().clone()));
    root.surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab((5 << 48) | 12);
    root.reconcile();
    assert_eq!(
        handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
        (5 << 48) | 12
    );
    let after = match root.surface.row_mut(StripId(2)).unwrap().groups[0]
        .host_for_tab_mut((5 << 48) | 10)
        .unwrap()
        .content()
    {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()) as *const (),
        _ => panic!("checked owner"),
    };
    assert_eq!(before, after);
    assert!(ui::component_lifecycle::take_component_close_reports(&mut root.surface).is_empty());
}
#[test]
fn pending_tab_choice_does_not_undo_a_newer_workspace_switch() {
    let (mut root, handle, _) = fixture();
    let new = Workspace::new(WorkspaceId(9), "Other");
    {
        let mut tree = handle.tree.borrow_mut();
        tree.workspaces.push(new);
        tree.active = WorkspaceId(9);
    }
    root.surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab((5 << 48) | 12);
    root.reconcile();
    let tree = handle.tree.borrow();
    assert_eq!(tree.active, WorkspaceId(9));
    assert_eq!(
        tree.workspaces[0].strips[0].window_groups[0].active_tab,
        (5 << 48) | 12
    );
    assert!(
        !tree.active_group.contains_key(&StripId(2)),
        "outgoing selection must not steal focus"
    );
}
#[test]
fn global_search_real_catalog_launch_uses_bound_queue() {
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(root),
        (1000, 700),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    handle.sync_global_search(&mut frame);
    let data = handle.search.borrow().data.clone();
    assert_eq!(
        data.applications
            .iter()
            .map(|a| a.app_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "so.tilo.word-processor",
            "so.tilo.chat",
            "so.tilo.notes",
            "so.tilo.calculator",
            "so.tilo.ebook",
        ]
    );
    assert_eq!(data.tabs.len(), 4);
    assert_eq!(
        handle.navigation.borrow()["capabilities"]["globalSearch"],
        true
    );
    let before = serde_json::to_value(&handle.tree.borrow().workspaces).unwrap();
    let record_heads = handle
        .records
        .borrow()
        .values()
        .map(|record| {
            record
                .store
                .with(&record.checkout.id, |doc| doc.get_heads())
        })
        .collect::<Vec<_>>();
    handle
        .open_global_search(&mut frame, &handle.generation, "Notes")
        .unwrap();
    assert_eq!(
        frame.global_search_scope(),
        Some(ui::search::system::SearchScope::All)
    );
    let node = frame.inspection().root.semantics.unwrap();
    let target = node.children[1].children[0]
        .action_target
        .as_deref()
        .unwrap();
    frame
        .perform_semantic_named("system-search", Some(target), &ui::a11y::Action::Activate)
        .unwrap();
    assert!(handle.session_requests.borrow().is_empty());
    handle.process_global_search(&mut frame).unwrap();
    let requests = handle.session_requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1.source.heads, vec![[7; 32]]);
    assert!(
        matches!(&requests[0].1.command,tilo_desktop_model::commands::DesktopCommand::OpenApp{app} if app=="so.tilo.notes")
    );
    assert_eq!(
        serde_json::to_value(&handle.tree.borrow().workspaces).unwrap(),
        before
    );
    assert_eq!(
        handle
            .records
            .borrow()
            .values()
            .map(|record| record
                .store
                .with(&record.checkout.id, |doc| doc.get_heads()))
            .collect::<Vec<_>>(),
        record_heads
    );
    assert!(handle.open_global_search(&mut frame, "stale", "").is_err());
}

#[test]
fn global_search_real_notes_and_chat_body_hits_select_exact_admitted_tab() {
    use notes::{doc::NotesDoc, model::PageMeta};
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(i, tab)| WindowGroup {
                id: 100 + i as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
    });
    for record in handle.records.borrow().values() {
        record.store.with(&record.checkout.id, |doc| {
            if record.app == "so.tilo.notes" {
                let page = uuid::Uuid::new_v4();
                NotesDoc::upsert_page_meta(
                    doc,
                    &PageMeta {
                        id: page,
                        title: "Ordinary notebook page".into(),
                        parent: None,
                        order: 0,
                    },
                );
                NotesDoc::fill_page_body(
                    doc,
                    page,
                    &[ui::widget::BlockContent::paragraph(
                        "",
                        "quasar-notes-body-only",
                    )],
                )
                .unwrap();
                notes::notebook::validate(doc).unwrap();
            } else if record.app == "so.tilo.chat" {
                chat::doc::ChatDoc::push_message(
                    doc,
                    "c_general",
                    chat::doc::NewMessage {
                        author: "author".into(),
                        ts: 1,
                        text: "nebula-chat-body-only".into(),
                        ..Default::default()
                    },
                );
            }
        });
    }
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1100, 700),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    for (app, query) in [
        ("so.tilo.notes", "quasar-notes-body-only"),
        ("so.tilo.chat", "nebula-chat-body-only"),
    ] {
        handle
            .open_global_search(&mut frame, &handle.generation, query)
            .unwrap();
        let data = handle.search.borrow().data.clone();
        let expected = data.tabs.iter().find(|tab| tab.app_id == app).unwrap();
        assert!(!expected.title.contains(query));
        let node = frame.inspection().root.semantics.unwrap();
        let results = &node.children[1].children;
        assert!(
            !results.is_empty(),
            "no real provider body hit for {app}: {}",
            frame.inspection_json().unwrap()
        );
        let target = results[0].action_target.as_deref().unwrap();
        frame
            .perform_semantic_named("system-search", Some(target), &ui::a11y::Action::Activate)
            .unwrap();
        handle.process_global_search(&mut frame).unwrap();
        assert_eq!(
            handle.navigation.borrow()["active"]["tab"],
            expected.instance_id.to_string()
        );
        assert!(handle.session_requests.borrow().is_empty());
    }
    handle
        .open_global_search(&mut frame, &handle.generation, "nebula-chat-body-only")
        .unwrap();
    let old_target = frame.inspection().root.semantics.unwrap().children[1].children[0]
        .action_target
        .clone()
        .unwrap();
    *handle.permissions.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(None, "outsider"),
        vec![],
    );
    frame.relayout();
    assert!(
        frame
            .perform_semantic_named(
                "system-search",
                Some(&old_target),
                &ui::a11y::Action::Activate
            )
            .is_err()
    );
    assert!(frame.take_global_search_intent().is_none());
}

#[test]
fn global_search_keeps_its_revision_when_only_room_heads_move() {
    // Every admitted publication (including this peer's own echoed edits)
    // moves the room heads. Unchanged tabs must not reinstall the search
    // data, which relayouts the whole desktop.
    let (_, handle, _) = fixture();
    let tree = handle.tree.borrow().clone();
    let routes = BTreeMap::new();
    let mut search = crate::system_search::SearchState::default();
    search.refresh("g1", &[[1; 32]], &tree, &routes, None);
    let first = search.data.revision.clone();
    search.refresh("g1", &[[2; 32]], &tree, &routes, None);
    assert_eq!(search.data.revision, first);
    search.refresh("g2", &[[2; 32]], &tree, &routes, None);
    assert_ne!(search.data.revision, first);
}
#[test]
fn global_search_refresh_revokes_exact_routes_and_stale_semantics() {
    let (root, handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 700),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    handle
        .open_global_search(&mut frame, &handle.generation, "Notes")
        .unwrap();
    let old = handle.search.borrow().data.clone();
    let tab = old
        .tabs
        .iter()
        .find(|t| t.app_id == "so.tilo.notes")
        .unwrap();
    let row = frame.inspection().root.semantics.unwrap().children[1].children[0]
        .action_target
        .clone()
        .unwrap();
    let mut tree = handle.tree.borrow().clone();
    for w in &mut tree.workspaces {
        for s in &mut w.strips {
            for g in &mut s.window_groups {
                g.tabs.retain(|t| t.id != tab.instance_id);
            }
        }
    }
    let routes = BTreeMap::from([(
        old.tabs
            .iter()
            .find(|t| t.app_id == "so.tilo.word-processor")
            .unwrap()
            .instance_id,
        "tab:1".into(),
    )]);
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&tree),
            source_heads: vec![[8; 32]],
            tree,
            routes,
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    frame.relayout();
    handle.sync_global_search(&mut frame);
    assert!(
        !handle
            .search
            .borrow()
            .data
            .tabs
            .iter()
            .any(|t| t.instance_id == tab.instance_id)
    );
    assert!(
        handle
            .search
            .borrow()
            .resolve(&old.revision, &tab.target)
            .is_err()
    );
    let current = handle.search.borrow().data.revision.clone();
    assert!(
        handle
            .search
            .borrow()
            .resolve(&current, &tab.target)
            .is_err()
    );
    assert!(
        frame
            .perform_semantic_named("system-search", Some(&row), &ui::a11y::Action::Activate)
            .is_err()
    );
    assert!(handle.session_requests.borrow().is_empty());
    handle
        .open_global_search(
            &mut frame,
            &handle.generation,
            &"é".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS),
        )
        .unwrap();
    assert!(
        handle
            .open_global_search(
                &mut frame,
                &handle.generation,
                &"x".repeat(ui::search::MAX_SEARCH_QUERY_SCALARS + 1)
            )
            .is_err()
    );
}
#[test]
fn explicit_selection_routes_ime_to_chat_and_restores_writer() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 100 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1800, 900),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.click_named("word_processor").unwrap();
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "retained draft".into(),
    )));
    frame.key("ArrowLeft");
    let writer_owner = frame.inspection().keyboard_focus_owner.unwrap();
    let writer = handle
        .records
        .borrow()
        .values()
        .find(|r| r.app == "so.tilo.word-processor")
        .unwrap()
        .clone();
    let writer_before = writer
        .store
        .read(&writer.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let (chat_owner, _) = browser_semantic_route(&frame, "Message composer");
    let route = |label: &str| {
        serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap()["route"]
                .clone(),
        )
        .unwrap()
    };
    let chat = route("Chat");
    let writer_route = route("Writer");
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: chat.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(chat_owner.as_str())
    );
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "destination input".into(),
    )));
    assert_eq!(
        writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        writer_before
    );
    assert!(
        frame
            .inspection_json()
            .unwrap()
            .contains("destination input")
    );
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: writer_route.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(writer_owner.as_str())
    );
    let sequence = frame.input_ownership_sequence();
    let activation = handle.activation_sequence.get();
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: writer_route.clone(),
            },
        )
        .unwrap();
    assert!(frame.input_ownership_sequence() > sequence);
    assert_eq!(handle.activation_sequence.get(), activation + 1);
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "X".into(),
    )));
    let writer_text = writer
        .store
        .read(&writer.checkout.id, |doc| {
            serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
        })
        .unwrap();
    assert!(
        writer_text.contains("retained drafXt"),
        "restored and reselected caret: {writer_text}"
    );
    let before = handle.navigation.borrow()["active"].clone();
    let activation = handle.activation_sequence.get();
    assert!(
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: "stale".into(),
                    route: chat.clone(),
                }
            )
            .is_err()
    );
    assert_eq!(handle.navigation.borrow()["active"], before);
    assert_eq!(handle.activation_sequence.get(), activation);
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(writer_owner.as_str())
    );
    handle
        .open_global_search(&mut frame, &handle.generation, "")
        .unwrap();
    assert!(
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: chat.clone(),
                }
            )
            .is_err(),
        "Search remains a modal owner"
    );
    assert_eq!(handle.navigation.borrow()["active"], before);
    frame.key("Escape");
    handle.process_global_search(&mut frame).unwrap();
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(writer_owner.as_str())
    );
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: chat.clone(),
            },
        )
        .unwrap();
    let writer_before_removal = writer
        .store
        .read(&writer.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let mut tree = handle.tree.borrow().clone();
    tree.workspaces[0].strips[0]
        .window_groups
        .retain(|group| group.id.to_string() != chat.group);
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&tree),
            source_heads: vec![[8; 32]],
            tree,
            routes: BTreeMap::from([(
                writer_route.ids().unwrap().3,
                writer.checkout.id.as_str().into(),
            )]),
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    frame.relayout();
    assert!(
        frame.inspection().keyboard_focus_owner.is_none(),
        "removed target cannot resurrect Writer"
    );
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "must not leak".into(),
    )));
    assert_eq!(
        writer
            .store
            .read(&writer.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        writer_before_removal
    );
    assert!(
        handle
            .navigate(
                &mut frame,
                crate::desktop_navigation::Command::Select {
                    generation: handle.generation.clone(),
                    route: chat,
                }
            )
            .is_err()
    );
}

#[test]
fn local_tab_selection_transfers_keyboard_ownership_from_chat_to_notes() {
    let (root, handle, _) = fixture();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1280, 720),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let route = |label: &str| {
        serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap()["route"]
                .clone(),
        )
        .unwrap()
    };
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: route("Chat"),
            },
        )
        .unwrap();
    let (composer, _) = browser_semantic_route(&frame, "Message composer");
    frame
        .perform_semantic_named(&composer, None, &ui::a11y::Action::Focus)
        .unwrap();
    let chat_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("Chat composer owns input");
    let chat = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.chat")
        .unwrap()
        .clone();
    let chat_before = chat
        .store
        .read(&chat.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let notes = route("Notes").ids().unwrap().3;
    // SAFETY: frame retains this root and the temporary access ends before
    // the harness runs the ordinary update/reconcile/layout pipeline.
    unsafe {
        (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab(notes);
    }
    frame.relayout();
    assert_eq!(
        handle.tree.borrow().workspaces[0].strips[0].window_groups[0].active_tab,
        notes
    );
    handle.finish_local_activation(&mut frame);
    let notes_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("selected Notes owns input");
    assert_ne!(
        notes_owner, chat_owner,
        "the retained Chat composer must lose keyboard ownership"
    );
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "must not reach Chat".into(),
    )));
    assert_eq!(
        chat.store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        chat_before
    );

    let chat_tab = route("Chat").ids().unwrap().3;
    unsafe {
        (&mut *root_ptr).surface.row_mut(StripId(2)).unwrap().groups[0].set_active_tab(chat_tab);
    }
    frame.relayout();
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: route("Calculator"),
            },
        )
        .unwrap();
    let calculator_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("Calculator owns input");
    handle.finish_local_activation(&mut frame);
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(calculator_owner.as_str()),
        "a stale local Chat request must not override a newer external Calculator selection"
    );
}

#[test]
fn ordinary_app_modal_rejects_background_group_semantic_focus() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 100 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1800, 900),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let route = |label: &str| {
        serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap()["route"]
                .clone(),
        )
        .unwrap()
    };
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: route("Writer"),
            },
        )
        .unwrap();
    fn widget_rect(node: &ui::headless::InspectionNode, widget: &str) -> Option<Rect> {
        if node.widget == widget {
            return Some(Rect::new(
                node.rect[0],
                node.rect[1],
                node.rect[2],
                node.rect[3],
            ));
        }
        node.children
            .iter()
            .find_map(|child| widget_rect(child, widget))
    }
    let point = widget_rect(&frame.inspection().root, "WordProcessor")
        .unwrap()
        .center();
    let device_id = winit::event::DeviceId::dummy();
    frame.send_host_event(&winit::event::WindowEvent::CursorMoved {
        device_id,
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
    for state in [
        winit::event::ElementState::Pressed,
        winit::event::ElementState::Released,
    ] {
        frame.send_host_event(&winit::event::WindowEvent::MouseInput {
            device_id,
            state,
            button: winit::event::MouseButton::Right,
        });
    }
    assert!(handle.check_local_activation(&frame).is_err());
    let before_group = handle.tree.borrow().active_group[&StripId(2)];
    let before_owner = frame.inspection().keyboard_focus_owner;
    let (composer, _) = browser_semantic_route(&frame, "Message composer");
    assert!(
        handle
            .perform_semantic_input(&mut frame, &composer, None, &ui::a11y::Action::Focus)
            .is_err()
    );
    assert_eq!(
        handle.tree.borrow().active_group[&StripId(2)],
        before_group,
        "modal preflight must reject background-group focus before selection changes"
    );
    assert_eq!(frame.inspection().keyboard_focus_owner, before_owner);
}

#[test]
fn local_group_selection_transfers_keyboard_ownership_to_its_active_notes_tab() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 100 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
    });
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1800, 900),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let route = |label: &str| {
        serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap()["route"]
                .clone(),
        )
        .unwrap()
    };
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route: route("Chat"),
            },
        )
        .unwrap();
    let (composer, _) = browser_semantic_route(&frame, "Message composer");
    frame
        .perform_semantic_named(&composer, None, &ui::a11y::Action::Focus)
        .unwrap();
    let chat_owner = frame
        .inspection()
        .keyboard_focus_owner
        .expect("Chat composer owns input");
    let notes = route("Notes").ids().unwrap();
    {
        let mut tree = handle.tree.borrow_mut();
        DesktopRoot::apply_local(
            &handle,
            &mut tree,
            LocalViewIntent::Group {
                strip: notes.1,
                group: notes.2,
            },
        );
    }
    frame.relayout();
    handle.finish_local_activation(&mut frame);
    assert_ne!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(chat_owner.as_str())
    );
    assert_eq!(handle.tree.borrow().active_group[&notes.1], notes.2);
}

#[derive(Clone, Copy, Debug)]
enum LocalSelectionCase {
    Workspace,
    Strip,
    Group,
    Tab,
    ChromeWorkspace,
}

fn arrange_local_selection_case(tree: &mut DesktopTree, case: LocalSelectionCase) {
    let workspace = &mut tree.workspaces[0];
    let strip = &mut workspace.strips[0];
    let mut tabs = std::mem::take(&mut strip.window_groups[0].tabs);
    let first = tabs.remove(0);
    let second = tabs.remove(2);
    strip.window_groups[0].tabs = vec![first];
    strip.window_groups[0].active_tab = strip.window_groups[0].tabs[0].id;
    tree.active_strip.insert(workspace.id, strip.id);
    tree.active_group
        .insert(strip.id, strip.window_groups[0].id);

    match case {
        LocalSelectionCase::Tab => {
            strip.window_groups[0].tabs.push(second);
        }
        LocalSelectionCase::Group => {
            strip.window_groups.push(WindowGroup {
                id: 4,
                active_tab: second.id,
                tabs: vec![second],
                width: WindowWidth::Normal,
                maximized: false,
            });
        }
        LocalSelectionCase::Strip => {
            workspace.strips.push(Strip {
                id: StripId(9),
                name: "Second".into(),
                window_groups: vec![WindowGroup {
                    id: 90,
                    active_tab: second.id,
                    tabs: vec![second],
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            });
            tree.active_group.insert(StripId(9), 90);
        }
        LocalSelectionCase::Workspace | LocalSelectionCase::ChromeWorkspace => {
            let mut second_workspace = workspace.clone();
            second_workspace.id = WorkspaceId(9);
            second_workspace.name = "Second".into();
            second_workspace.strips = vec![Strip {
                id: StripId(9),
                name: "Second".into(),
                window_groups: vec![WindowGroup {
                    id: 90,
                    active_tab: second.id,
                    tabs: vec![second],
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            }];
            tree.active_strip.insert(second_workspace.id, StripId(9));
            tree.active_group.insert(StripId(9), 90);
            tree.workspaces.push(second_workspace);
        }
    }
}

fn focused_controls_for_instance(
    frame: &ui::headless::FrameHarness,
    node: &ui::headless::InspectionNode,
    instance: ui::application::InstanceId,
) -> usize {
    fn focused(node: &ui::a11y::AccessNode) -> usize {
        usize::from(node.state.focused) + node.children.iter().map(focused).sum::<usize>()
    }
    let here = if frame.semantic_owner_instance(&node.path) == Some(instance) {
        node.semantics.as_ref().map_or(0, focused)
    } else {
        0
    };
    here + node
        .children
        .iter()
        .map(|child| focused_controls_for_instance(frame, child, instance))
        .sum::<usize>()
}

fn focusable_control_for_instance(
    frame: &ui::headless::FrameHarness,
    node: &ui::headless::InspectionNode,
    instance: ui::application::InstanceId,
) -> Option<String> {
    if node.focusable && frame.semantic_owner_instance(&node.path) == Some(instance) {
        return Some(node.path.clone());
    }
    node.children
        .iter()
        .find_map(|child| focusable_control_for_instance(frame, child, instance))
}

#[test]
fn visible_local_selections_bind_input_to_the_selected_active_instance() {
    for case in [
        LocalSelectionCase::Strip,
        LocalSelectionCase::Group,
        LocalSelectionCase::Tab,
    ] {
        let (root, handle, _) = fixture_with_topology(|tree| {
            arrange_local_selection_case(tree, case);
        });
        let mut root = Box::new(root);
        let root_ptr = std::ptr::from_mut(root.as_mut());
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            root,
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let initial_instance = active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3;
        let initial_control =
            focusable_control_for_instance(&frame, &frame.inspection().root, initial_instance)
                .expect("initial app exposes a focusable control");
        frame
            .perform_semantic_named(&initial_control, None, &ui::a11y::Action::Focus)
            .unwrap();
        frame.relayout();
        handle.finish_local_activation(&mut frame);
        assert!(
            focused_controls_for_instance(&frame, &frame.inspection().root, initial_instance) > 0,
            "{case:?}: precondition requires a focused control in the initial app"
        );

        match case {
            LocalSelectionCase::Workspace => {
                let mut tree = handle.tree.borrow_mut();
                DesktopRoot::apply_local(
                    &handle,
                    &mut tree,
                    LocalViewIntent::Workspace(WorkspaceId(9)),
                );
                unsafe {
                    (&mut *root_ptr).view_changed = true;
                }
            }
            LocalSelectionCase::Strip => {
                let mut tree = handle.tree.borrow_mut();
                DesktopRoot::apply_local(&handle, &mut tree, LocalViewIntent::Strip(StripId(9)));
                unsafe {
                    (&mut *root_ptr).view_changed = true;
                }
            }
            LocalSelectionCase::Group => {
                let mut tree = handle.tree.borrow_mut();
                DesktopRoot::apply_local(
                    &handle,
                    &mut tree,
                    LocalViewIntent::Group {
                        strip: StripId(2),
                        group: 4,
                    },
                );
                unsafe {
                    (&mut *root_ptr).view_changed = true;
                }
            }
            LocalSelectionCase::Tab => {
                let target =
                    handle.tree.borrow().workspaces[0].strips[0].window_groups[0].tabs[1].id;
                let mut tree = handle.tree.borrow_mut();
                DesktopRoot::apply_local(
                    &handle,
                    &mut tree,
                    LocalViewIntent::Tab {
                        group: 3,
                        tab: target,
                    },
                );
                unsafe {
                    (&mut *root_ptr).view_changed = true;
                }
            }
            LocalSelectionCase::ChromeWorkspace => {
                let mut tree = handle.tree.borrow_mut();
                DesktopRoot::switch_workspace(&handle, &mut tree, 1);
                unsafe {
                    (&mut *root_ptr).view_changed = true;
                }
            }
        }
        frame.relayout();
        handle.finish_local_activation(&mut frame);

        let selected_instance = active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3;
        assert_ne!(
            selected_instance, initial_instance,
            "{case:?}: route changed app"
        );
        let owner = frame
            .inspection()
            .keyboard_focus_owner
            .unwrap_or_else(|| panic!("{case:?}: selected app owns keyboard input"));
        assert_eq!(
            frame.semantic_owner_instance(&owner),
            Some(selected_instance),
            "{case:?}: keyboard owner must belong to the active route instance"
        );
        assert_eq!(
            focused_controls_for_instance(&frame, &frame.inspection().root, initial_instance),
            0,
            "{case:?}: the previous app must not retain a focused control"
        );
    }
}

#[test]
fn workspace_selectors_queue_the_selected_instance_as_next_keyboard_owner() {
    for case in [
        LocalSelectionCase::Workspace,
        LocalSelectionCase::ChromeWorkspace,
    ] {
        let (_root, handle, _) = fixture_with_topology(|tree| {
            arrange_local_selection_case(tree, case);
        });
        let previous = active_route(&handle.tree.borrow()).unwrap();
        match case {
            LocalSelectionCase::Workspace => DesktopRoot::apply_local(
                &handle,
                &mut handle.tree.borrow_mut(),
                LocalViewIntent::Workspace(WorkspaceId(9)),
            ),
            LocalSelectionCase::ChromeWorkspace => {
                DesktopRoot::switch_workspace(&handle, &mut handle.tree.borrow_mut(), 1);
            }
            _ => unreachable!(),
        }
        let selected = active_route(&handle.tree.borrow()).unwrap();
        assert_ne!(
            selected.ids().unwrap().3,
            previous.ids().unwrap().3,
            "{case:?}"
        );
        assert_eq!(
            handle.pending_local_activation.borrow().as_ref(),
            Some(&selected),
            "{case:?}: queue the exact route whose instance became active"
        );
        assert_eq!(
            handle
                .pending_local_activation
                .borrow()
                .as_ref()
                .and_then(|route| route.ids().ok())
                .map(|ids| ids.3),
            Some(selected.ids().unwrap().3),
            "{case:?}: the queued keyboard owner must be the selected active instance"
        );
    }
}

#[test]
fn non_selection_local_changes_do_not_queue_activation() {
    let (_root, handle, _) = fixture();
    for intent in [
        LocalViewIntent::Width {
            group: 3,
            width: WindowWidth::Fraction(500),
        },
        LocalViewIntent::Maximized {
            group: 3,
            maximized: true,
        },
    ] {
        DesktopRoot::apply_local(&handle, &mut handle.tree.borrow_mut(), intent);
        assert!(handle.pending_local_activation.borrow().is_none());
    }
}

#[test]
fn admission_only_refresh_preserves_the_current_keyboard_owner() {
    let (root, handle, _) = fixture();
    let routes = root.resolver.host.routes.clone();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1280, 720),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let route = serde_json::from_value::<crate::desktop_navigation::Route>(
        handle.navigation.borrow()["entries"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["route"]
            .clone(),
    )
    .unwrap();
    handle
        .navigate(
            &mut frame,
            crate::desktop_navigation::Command::Select {
                generation: handle.generation.clone(),
                route,
            },
        )
        .unwrap();
    let owner = frame.inspection().keyboard_focus_owner.unwrap();
    let activation = handle.activation_sequence.get();

    let tree = handle.tree.borrow().clone();
    *handle.inbox.borrow_mut() = Some((
        Admission {
            admitted_room: history_document(&tree),
            source_heads: vec![[8; 32]],
            tree,
            routes,
            required: vec![],
            missing: vec![],
            unsupported: vec![],
            pending: 0,
        },
        handle.permissions.borrow().clone(),
    ));
    frame.relayout();
    handle.finish_local_activation(&mut frame);

    assert!(handle.pending_local_activation.borrow().is_none());
    assert_eq!(handle.activation_sequence.get(), activation);
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(owner.as_str())
    );
}

#[test]
fn external_strip_and_workspace_selection_activate_the_resulting_route() {
    for case in [LocalSelectionCase::Strip, LocalSelectionCase::Workspace] {
        let (root, handle, _) =
            fixture_with_topology(|tree| arrange_local_selection_case(tree, case));
        let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
            ui::State::new(),
            Box::new(root),
            (1800, 900),
            Theme::dark(),
            ui::TextRenderer::new(),
            ui::permissions::UserId("author".into()),
        );
        let initial = active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3;
        let control = focusable_control_for_instance(&frame, &frame.inspection().root, initial)
            .expect("initial app exposes a focusable control");
        frame
            .perform_semantic_named(&control, None, &ui::a11y::Action::Focus)
            .unwrap();
        frame.relayout();
        handle.finish_local_activation(&mut frame);
        let activation = handle.activation_sequence.get();
        let command = match case {
            LocalSelectionCase::Strip => crate::desktop_navigation::Command::Strip {
                generation: handle.generation.clone(),
                workspace: "1".into(),
                strip: "9".into(),
            },
            LocalSelectionCase::Workspace => crate::desktop_navigation::Command::Workspace {
                generation: handle.generation.clone(),
                workspace: "9".into(),
            },
            _ => unreachable!(),
        };
        handle.navigate(&mut frame, command).unwrap();
        let selected = active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3;
        assert_ne!(selected, initial, "{case:?}: route changes instance");
        assert_eq!(handle.activation_sequence.get(), activation + 1, "{case:?}");
        if matches!(case, LocalSelectionCase::Strip) {
            let owner = frame.inspection().keyboard_focus_owner.unwrap();
            assert_eq!(
                frame.semantic_owner_instance(&owner),
                Some(selected),
                "{case:?}"
            );
        }
        assert_eq!(
            focused_controls_for_instance(&frame, &frame.inspection().root, initial),
            0,
            "{case:?}"
        );
        frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
            "must not reach the hidden app".into(),
        )));
        assert_eq!(
            focused_controls_for_instance(&frame, &frame.inspection().root, initial),
            0,
            "{case:?}: IME must not restore the inactive retained owner"
        );
    }
}

#[test]
fn pointer_press_on_notes_nonfocusable_surface_clears_chat_composer_focus() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let active_workspace = tree.active;
        let strip = &mut tree.workspaces[0].strips[0];
        let strip_id = strip.id;
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        let mut chat = None;
        let mut notes = None;
        for tab in tabs {
            match tab.title.as_str() {
                "Chat" => chat = Some(tab),
                "Notes" => notes = Some(tab),
                _ => {}
            }
        }
        let chat = chat.unwrap();
        let notes = notes.unwrap();
        strip.window_groups = vec![
            WindowGroup {
                id: 4,
                active_tab: notes.id,
                tabs: vec![notes],
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: 3,
                active_tab: chat.id,
                tabs: vec![chat],
                width: WindowWidth::Normal,
                maximized: false,
            },
        ];
        tree.active_strip.insert(active_workspace, strip_id);
        tree.active_group.insert(strip_id, 3);
    });
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (1800, 900),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    let route = |label: &str| {
        serde_json::from_value::<crate::desktop_navigation::Route>(
            handle.navigation.borrow()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap()["route"]
                .clone(),
        )
        .unwrap()
    };
    let notes_instance = route("Notes").ids().unwrap().3;
    let (composer, composer_target) = browser_semantic_route(&frame, "Message composer");
    frame
        .perform_semantic_named(&composer, Some(&composer_target), &ui::a11y::Action::Focus)
        .unwrap();
    frame.relayout();
    handle.finish_local_activation(&mut frame);
    let activation = handle.activation_sequence.get();
    assert_eq!(
        active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3,
        route("Chat").ids().unwrap().3,
        "Chat must be the canonical active app before the click"
    );
    let composer_focused = |frame: &ui::headless::FrameHarness| {
        fn find(node: &ui::a11y::AccessNode) -> Option<bool> {
            if node.name.as_deref() == Some("Message composer") {
                return Some(node.state.focused);
            }
            node.children.iter().find_map(find)
        }
        fn walk(node: &ui::headless::InspectionNode) -> Option<bool> {
            node.semantics
                .as_ref()
                .and_then(find)
                .or_else(|| node.children.iter().find_map(walk))
        }
        walk(&frame.inspection().root).unwrap()
    };
    assert!(composer_focused(&frame), "Chat composer precondition");
    let chat = handle
        .records
        .borrow()
        .values()
        .find(|record| record.app == "so.tilo.chat")
        .unwrap()
        .clone();
    let chat_before = chat
        .store
        .read(&chat.checkout.id, |doc| doc.clone().save())
        .unwrap();
    let (group_rect, viewport, visible) = unsafe {
        let row = (&*root_ptr).surface.row(StripId(2)).unwrap();
        let group = row.groups.iter().find(|group| group.id == 4).unwrap();
        (
            group.get_rect(),
            row.get_rect(),
            row.children()
                .iter()
                .any(|widget| std::ptr::addr_eq(*widget, group as &dyn Widget)),
        )
    };
    let mut focusable = Vec::new();
    fn collect_focusable(
        frame: &ui::headless::FrameHarness,
        node: &ui::headless::InspectionNode,
        instance: ui::application::InstanceId,
        out: &mut Vec<Rect>,
    ) {
        if node.focusable && frame.semantic_owner_instance(&node.path) == Some(instance) {
            out.push(Rect::new(
                node.rect[0],
                node.rect[1],
                node.rect[2],
                node.rect[3],
            ));
        }
        for child in &node.children {
            collect_focusable(frame, child, instance, out);
        }
    }
    collect_focusable(
        &frame,
        &frame.inspection().root,
        notes_instance,
        &mut focusable,
    );
    let hit = Rect::new(
        group_rect.x0.max(viewport.x0),
        group_rect.y0.max(viewport.y0),
        group_rect.x1.min(viewport.x1),
        group_rect.y1.min(viewport.y1),
    );
    let point = (0..=((hit.height() / 8.0) as usize))
        .find_map(|y| {
            (0..=((hit.width() / 8.0) as usize)).find_map(|x| {
                let point =
                    Point::new(hit.x0 + 2.0 + x as f64 * 8.0, hit.y0 + 2.0 + y as f64 * 8.0);
                (hit.contains(point) && !focusable.iter().any(|bounds| bounds.contains(point)))
                    .then_some(point)
            })
        })
        .expect("visible Notes group exposes nonfocusable chrome or surface");
    assert!(visible && group_rect.contains(point) && viewport.contains(point));
    assert!(
        !focusable.iter().any(|bounds| bounds.contains(point)),
        "pointer must avoid every focusable Notes control: point={point:?} focusable={focusable:?}"
    );
    let device_id = winit::event::DeviceId::dummy();
    for event in [
        winit::event::WindowEvent::CursorMoved {
            device_id,
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        },
        winit::event::WindowEvent::MouseInput {
            device_id,
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        },
        winit::event::WindowEvent::MouseInput {
            device_id,
            state: winit::event::ElementState::Released,
            button: winit::event::MouseButton::Left,
        },
    ] {
        frame.send_host_event(&event);
        handle.finish_local_activation(&mut frame);
    }
    assert_eq!(
        active_route(&handle.tree.borrow())
            .unwrap()
            .ids()
            .unwrap()
            .3,
        notes_instance
    );
    assert_eq!(
        handle.activation_sequence.get(),
        activation + 1,
        "group pointer selection must cross the app activation boundary"
    );
    frame.relayout();
    let owner = frame.inspection().keyboard_focus_owner.unwrap();
    assert_eq!(frame.semantic_owner_instance(&owner), Some(notes_instance));
    assert!(
        !composer_focused(&frame),
        "Chat composer must clear its semantic focused flag"
    );
    for action in [
        ui::a11y::Action::SetValue("stale browser textarea value".into()),
        ui::a11y::Action::SetSelection {
            anchor: 0,
            focus: 0,
        },
    ] {
        assert!(
            handle
                .perform_semantic_input(&mut frame, &composer, Some(&composer_target), &action,)
                .is_err(),
            "a stale semantic text route must not operate after another control owns focus: {action:?}",
        );
        assert_eq!(
            chat.store
                .read(&chat.checkout.id, |doc| doc.clone().save())
                .unwrap(),
            chat_before,
            "rejected stale semantic text input must not mutate Chat: {action:?}",
        );
    }
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "must not reach previous app".into(),
    )));
    assert_eq!(
        chat.store
            .read(&chat.checkout.id, |doc| doc.clone().save())
            .unwrap(),
        chat_before
    );
}

#[test]
fn global_search_escape_preserves_selected_calculator_route_and_pan() {
    let (root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
        strip.window_groups = tabs
            .into_iter()
            .filter(|tab| tab.title == "Writer" || tab.title == "Calculator")
            .enumerate()
            .map(|(index, tab)| WindowGroup {
                id: 100 + index as u64,
                active_tab: tab.id,
                tabs: vec![tab],
                width: WindowWidth::Normal,
                maximized: false,
            })
            .collect();
    });
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        root,
        (740, 720),
        Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.click_named("word_processor").unwrap();
    settle_external_navigation(root_ptr, &mut frame);
    let before_heads = handle
        .records
        .borrow()
        .values()
        .map(|record| {
            record
                .store
                .read(&record.checkout.id, |doc| doc.clone().get_heads())
                .unwrap()
        })
        .collect::<Vec<_>>();

    handle
        .open_global_search(&mut frame, &handle.generation, "Calculator")
        .unwrap();
    let calculator = handle
        .search
        .borrow()
        .data
        .tabs
        .iter()
        .find(|tab| tab.app_id == "so.tilo.calculator")
        .unwrap()
        .instance_id;
    let (owner, filter) = browser_semantic_route(&frame, "Filter: Records");
    frame
        .perform_semantic_named(&owner, Some(&filter), &ui::a11y::Action::Activate)
        .unwrap();
    let semantics = frame.inspection().root.semantics.unwrap();
    let rows = &semantics.children[1].children;
    assert_eq!(
        rows.len(),
        1,
        "Records filter must select the admitted Calculator tab, not launch an app"
    );
    let bounds = rows[0].bounds.unwrap().to_rect();
    frame.click_at(bounds.center().x, bounds.center().y);
    handle.process_global_search(&mut frame).unwrap();
    settle_external_navigation(root_ptr, &mut frame);
    let selected = handle.navigation.borrow().clone();
    assert_eq!(selected["active"]["tab"], calculator.to_string());
    let pan = selected["desktopPan"]["offset"].as_f64().unwrap();
    assert!(pan > 0.0, "narrow desktop must have panned to Calculator");
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(browser_semantic_route(&frame, "Calculator").0.as_str()),
        "accepted selection must focus Calculator"
    );

    handle
        .open_global_search(&mut frame, &handle.generation, "")
        .unwrap();
    frame.send_host_event(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "Writer".into(),
    )));
    assert_eq!(frame.global_search_query(), Some("Writer"));
    frame.key("Escape");
    handle.process_global_search(&mut frame).unwrap();
    settle_external_navigation(root_ptr, &mut frame);
    let after = handle.navigation.borrow();
    assert_eq!(
        after["active"], selected["active"],
        "dismissing Search must not reinterpret restored Writer focus as a new navigation gesture"
    );
    assert_eq!(
        after["desktopPan"]["offset"],
        selected["desktopPan"]["offset"]
    );
    assert_eq!(
        frame.inspection().keyboard_focus_owner.as_deref(),
        Some(browser_semantic_route(&frame, "Calculator").0.as_str())
    );
    assert_eq!(
        handle
            .records
            .borrow()
            .values()
            .map(|record| {
                record
                    .store
                    .read(&record.checkout.id, |doc| doc.clone().get_heads())
                    .unwrap()
            })
            .collect::<Vec<_>>(),
        before_heads
    );
    assert!(handle.session_requests.borrow().is_empty());
    assert!(frame.take_caller_operation().is_none());
    assert!(frame.take_global_search_intent().is_none());
}

/// Manual CPU microbenchmark for the semantics status gate on the real
/// desktop fixture while typing into the Writer: tree serialization (paid
/// on every frame for the change hash), the structure signature, the gate
/// decision, and what parsing the bytes back into a Value and serializing
/// them again inside the status would add on top.
#[test]
#[ignore = "manual CPU microbenchmark, not a browser frame-rate gate"]
fn semantics_serialization_cost_per_typed_key() {
    use crate::semantics_status::{SemanticsDecision, SemanticsGate, structure_signature};
    fn count(node: &ui::headless::InspectionNode) -> usize {
        1 + node.children.iter().map(count).sum::<usize>()
            + node.semantics.as_ref().map_or(0, count_access)
    }
    fn count_access(node: &ui::a11y::AccessNode) -> usize {
        1 + node.children.iter().map(count_access).sum::<usize>()
    }
    let (root, _handle, _) = fixture();
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(root),
        (1000, 700),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.click_at(200.0, 180.0);
    frame.relayout();
    assert!(
        frame.inspection().keyboard_focus_owner.is_some(),
        "the Writer editor must own keyboard focus"
    );
    let mut gate = SemanticsGate::new(std::time::Duration::from_millis(100));
    let (mut serialize, mut structure, mut decide, mut stage4_extra) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut nodes, mut bytes, mut delivered, mut deferred, mut unchanged) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let ms = |start: std::time::Instant| start.elapsed().as_secs_f64() * 1000.0;
    for i in 0..200 {
        frame.type_text(if i % 7 == 6 { " " } else { "a" });
        frame.relayout();
        let output = frame.frame_output();
        let snapshot = &output.semantics;
        nodes = count(&snapshot.root);
        let start = std::time::Instant::now();
        let text = serde_json::to_vec(snapshot).unwrap();
        serialize.push(ms(start));
        bytes = text.len();
        let start = std::time::Instant::now();
        let _ = structure_signature(snapshot);
        structure.push(ms(start));
        let start = std::time::Instant::now();
        let decision = gate.decide(snapshot, output.ime.as_ref(), ui::clock::Instant::now());
        decide.push(ms(start));
        match decision {
            SemanticsDecision::Deliver { .. } => delivered += 1,
            SemanticsDecision::Deferred(_) => deferred += 1,
            SemanticsDecision::Unchanged => unchanged += 1,
        }
        let start = std::time::Instant::now();
        let value: serde_json::Value = serde_json::from_slice(&text).unwrap();
        let _ = value.to_string();
        stage4_extra.push(ms(start));
    }
    let percentiles = |samples: &mut Vec<f64>| {
        samples.sort_by(f64::total_cmp);
        (
            samples[samples.len() / 2],
            samples[samples.len() * 99 / 100],
        )
    };
    let (serialize_p50, serialize_p99) = percentiles(&mut serialize);
    let (structure_p50, structure_p99) = percentiles(&mut structure);
    let (decide_p50, decide_p99) = percentiles(&mut decide);
    let (extra_p50, extra_p99) = percentiles(&mut stage4_extra);
    eprintln!(
        "semantics per typed key (200 keys, desktop fixture, {nodes} nodes, {bytes} bytes JSON, native {}): serialize p50={serialize_p50:.3} p99={serialize_p99:.3} ms; structure signature p50={structure_p50:.3} p99={structure_p99:.3} ms; gate decide p50={decide_p50:.3} p99={decide_p99:.3} ms; stage-4 parse+reserialize extra p50={extra_p50:.3} p99={extra_p99:.3} ms; decisions: delivered={delivered} deferred={deferred} unchanged={unchanged}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
}

fn split_groups(tree: &mut DesktopTree) {
    let strip = &mut tree.workspaces[0].strips[0];
    let tabs = std::mem::take(&mut strip.window_groups[0].tabs);
    strip.window_groups = tabs
        .into_iter()
        .enumerate()
        .map(|(index, tab)| WindowGroup {
            id: 3 + index as u64,
            active_tab: tab.id,
            tabs: vec![tab],
            width: WindowWidth::Normal,
            maximized: false,
        })
        .collect();
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
}
fn group_order(tree: &DesktopTree) -> Vec<u64> {
    tree.workspaces[0].strips[0]
        .window_groups
        .iter()
        .map(|group| group.id)
        .collect()
}
fn navigation_groups(handle: &DesktopHandle) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for entry in handle.navigation.borrow()["entries"].as_array().unwrap() {
        let group = entry["route"]["group"].as_str().unwrap().to_string();
        if !out.contains(&group) {
            out.push(group);
        }
    }
    out
}
fn readmit(root: &DesktopRoot, handle: &DesktopHandle, tree: DesktopTree) {
    let admission = Admission {
        admitted_room: history_document(&tree),
        source_heads: vec![[8; 32]],
        tree,
        routes: root.resolver.host.routes.clone(),
        required: vec![],
        missing: vec![],
        unsupported: vec![],
        pending: 0,
    };
    *handle.inbox.borrow_mut() = Some((admission, handle.permissions.borrow().clone()));
}

#[test]
fn palette_group_move_renders_next_frame_and_reconciles_with_durable_state() {
    let (root, handle, _) = fixture_with_topology(split_groups);
    let admitted = handle.tree.borrow().clone();
    let base = group_order(&admitted);
    assert!(base.len() >= 3);
    handle.speculation.borrow_mut().enabled = true;
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        root,
        (1400, 768),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    frame.relayout();
    frame.set_modifiers(&["ctrl".into()]);
    frame.key("k");
    frame.set_modifiers(&[]);
    frame.type_text("Move group right");
    frame.key("Enter");
    frame.relayout();
    let invocation = {
        let requests = handle.session_requests.borrow();
        assert_eq!(requests.len(), 1, "the palette queues one durable command");
        assert!(matches!(
            requests[0].1.command,
            tilo_desktop_model::commands::DesktopCommand::ReorderGroup { group: 3, .. }
        ));
        requests[0].0
    };
    let mut moved = base.clone();
    moved.swap(0, 1);
    assert_eq!(
        group_order(&handle.tree.borrow()),
        moved,
        "local order is optimistic"
    );
    assert_eq!(
        navigation_groups(&handle),
        moved.iter().map(u64::to_string).collect::<Vec<_>>(),
        "public navigation shows the move without waiting for the host"
    );
    // The overlay ghost for remote members was measured before the move.
    let preview = handle.speculation.borrow().preview(invocation).cloned();
    assert!(preview.is_some_and(|preview| preview.group == 3 && preview.dx > 0.0));
    // SAFETY: the harness owns the root for the whole test; no frame is
    // being driven while these direct calls run.
    let root = unsafe { &mut *root_ptr };
    // An unrelated admission (durable order unchanged) keeps the move.
    readmit(root, &handle, admitted.clone());
    root.reconcile();
    assert_eq!(group_order(&handle.tree.borrow()), moved);
    // The owner refuses: snap back to the durable order.
    assert!(handle.speculation.borrow_mut().settle(
        invocation,
        Some(false),
        None,
        web_time::Instant::now()
    ));
    assert!(root.reconcile());
    assert_eq!(
        group_order(&handle.tree.borrow()),
        base,
        "refusal snaps back"
    );
    assert_eq!(
        navigation_groups(&handle),
        base.iter().map(u64::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn group_move_echo_is_not_applied_twice_and_speculation_is_opt_in() {
    let (mut root, handle, _) = fixture_with_topology(split_groups);
    let admitted = handle.tree.borrow().clone();
    let base = group_order(&admitted);
    let room = admitted.workspaces[0].room.to_string();
    let request = |group: u64, before: Option<u64>| {
        serde_json::to_vec(&tilo_desktop_model::commands::BoundCommand {
            source: tilo_desktop_model::commands::CommandSource {
                room: room.clone(),
                heads: vec![[7; 32]],
            },
            command: tilo_desktop_model::commands::DesktopCommand::ReorderGroup {
                strip: 2,
                group,
                before,
            },
        })
        .unwrap()
    };
    let invoke = |root: &mut DesktopRoot, payload: &[u8]| {
        let output = root
            .start_operation(
                &ui::widget::ResourceOperation::local(
                    tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::View,
                    "tilo.desktop.request",
                    payload,
                ),
                &mut AutoCommit::new(),
                &automerge::ROOT,
            )
            .unwrap()
            .and_then(ui::widget::ResourceOperationStart::completed)
            .unwrap();
        output
            .value
            .unwrap()
            .as_str()
            .unwrap()
            .parse::<uuid::Uuid>()
            .unwrap()
    };
    // Disabled (no command bridge reports outcomes): durable-only.
    invoke(&mut root, &request(3, None));
    root.reconcile();
    assert_eq!(group_order(&handle.tree.borrow()), base);
    handle.session_requests.borrow_mut().clear();

    handle.speculation.borrow_mut().enabled = true;
    let invocation = invoke(&mut root, &request(3, None));
    root.reconcile();
    let mut moved = base.clone();
    let first = moved.remove(0);
    moved.push(first);
    assert_eq!(group_order(&handle.tree.borrow()), moved);
    // The committed echo lands, then the success: no double apply, no
    // flicker, and the entry retires.
    let mut echoed = admitted.clone();
    tilo_desktop_model::commands::apply_command(
        &mut echoed.workspaces[0],
        &tilo_desktop_model::commands::DesktopCommand::ReorderGroup {
            strip: 2,
            group: 3,
            before: None,
        },
        &mut || Err("no mint".into()),
        &mut |_, _| Err("no create".into()),
    )
    .unwrap();
    readmit(&root, &handle, echoed);
    root.reconcile();
    assert_eq!(group_order(&handle.tree.borrow()), moved);
    handle
        .speculation
        .borrow_mut()
        .settle(invocation, Some(true), None, web_time::Instant::now());
    root.reconcile();
    assert!(handle.speculation.borrow().is_empty());
    assert_eq!(group_order(&handle.tree.borrow()), moved);
    assert_eq!(
        navigation_groups(&handle),
        moved.iter().map(u64::to_string).collect::<Vec<_>>()
    );
}

/// The owner's room registers salt 5 as its topology namespace.
fn readmit_owned(root: &DesktopRoot, handle: &DesktopHandle, tree: DesktopTree, counter: u64) {
    use automerge::transaction::Transactable;
    let mut doc = history_document(&tree);
    let owners = doc
        .put_object(
            automerge::ROOT,
            "contents_owners_v1",
            automerge::ObjType::Map,
        )
        .unwrap();
    doc.put(&owners, "0005", "ab".repeat(32)).unwrap();
    doc.put(automerge::ROOT, "desktop_counter_v1", counter)
        .unwrap();
    doc.commit();
    let admission = Admission {
        admitted_room: doc,
        source_heads: vec![[8; 32]],
        tree,
        routes: root.resolver.host.routes.clone(),
        required: vec![],
        missing: vec![],
        unsupported: vec![],
        pending: 0,
    };
    *handle.inbox.borrow_mut() = Some((admission, handle.permissions.borrow().clone()));
}
fn open_fixture() -> (DesktopRoot, DesktopHandle, DesktopTree) {
    let (mut root, handle, _) = fixture_with_topology(|tree| {
        let strip = &mut tree.workspaces[0].strips[0];
        strip.id = StripId((5 << 48) | 2);
        strip.window_groups[0].id = (5 << 48) | 3;
    });
    let admitted = handle.tree.borrow().clone();
    readmit_owned(&root, &handle, admitted.clone(), 60);
    root.reconcile();
    handle.speculation.borrow_mut().enabled = true;
    (root, handle, admitted)
}
fn invoke_open(root: &mut DesktopRoot, room: &str, app: &str) -> uuid::Uuid {
    let payload = serde_json::to_vec(&tilo_desktop_model::commands::BoundCommand {
        source: tilo_desktop_model::commands::CommandSource {
            room: room.into(),
            heads: vec![[8; 32]],
        },
        command: tilo_desktop_model::commands::DesktopCommand::OpenApp { app: app.into() },
    })
    .unwrap();
    let output = root
        .start_operation(
            &ui::widget::ResourceOperation::local(
                tilo_actions::InvocationId::new(),
                tilo_actions::OperationKind::View,
                "tilo.desktop.request",
                &payload,
            ),
            &mut AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap()
        .and_then(ui::widget::ResourceOperationStart::completed)
        .unwrap();
    output.value.unwrap().as_str().unwrap().parse().unwrap()
}
fn entries_at(handle: &DesktopHandle, tab: u64) -> Vec<serde_json::Value> {
    handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["route"]["tab"].as_str() == Some(tab.to_string().as_str()))
        .cloned()
        .collect()
}

#[test]
fn an_optimistic_open_draws_its_window_next_frame_and_the_echo_keeps_one_window() {
    let (mut root, handle, admitted) = open_fixture();
    let room = admitted.workspaces[0].room.to_string();
    let before = handle.navigation.borrow()["entries"]
        .as_array()
        .unwrap()
        .len();
    let invocation = invoke_open(&mut root, &room, "so.tilo.notes");
    root.reconcile();
    let predicted = (5 << 48) | 60;
    let drawn = entries_at(&handle, predicted);
    assert_eq!(drawn.len(), 1, "the window is drawn without the host");
    assert_eq!(drawn[0]["label"], "Notes");
    assert_eq!(drawn[0]["opening"], true, "marked until committed");
    assert_eq!(
        handle.navigation.borrow()["active"]["tab"],
        predicted.to_string(),
        "the new window has local focus"
    );
    assert!(
        !handle.tab_records.borrow().contains_key(&predicted),
        "no record is mounted for the placeholder"
    );
    // The owner's echo commits the same window.
    let mut echoed = admitted.clone();
    let mut counter = 60;
    tilo_desktop_model::commands::apply_command(
        &mut echoed.workspaces[0],
        &tilo_desktop_model::commands::DesktopCommand::OpenTab {
            app: "so.tilo.notes".into(),
            placement: tilo_desktop_model::commands::TabPlacement::NewGroup {
                strip: (5 << 48) | 2,
                after: None,
            },
        },
        &mut || tilo_desktop_model::mint_in_namespace(5, &mut counter),
        &mut |app, _| {
            Ok((
                "Notes".into(),
                TileContent::AppPlaceholder {
                    kind: AppKind::Other(app.into()),
                    label: "Notes".into(),
                },
            ))
        },
    )
    .unwrap();
    readmit_owned(&root, &handle, echoed, counter);
    root.reconcile();
    assert_eq!(
        entries_at(&handle, predicted).len(),
        1,
        "adopted, not duplicated"
    );
    assert!(entries_at(&handle, predicted)[0].get("opening").is_none());
    assert_eq!(
        entries_at(&handle, predicted)[0]["route"],
        drawn[0]["route"],
        "the committed window fills the placeholder in place (same strip, group, tab)"
    );
    assert_eq!(
        handle.navigation.borrow()["entries"]
            .as_array()
            .unwrap()
            .len(),
        before + 1
    );
    assert_eq!(
        handle.navigation.borrow()["active"]["tab"],
        predicted.to_string(),
        "focus stays on the committed window"
    );
    assert!(handle.speculation.borrow().placeholders().is_empty());
    handle
        .speculation
        .borrow_mut()
        .settle(invocation, Some(true), None, web_time::Instant::now());
    root.reconcile();
    assert!(handle.speculation.borrow().is_empty());
    assert_eq!(entries_at(&handle, predicted).len(), 1);
}

#[test]
fn the_placeholder_window_is_drawn_on_the_canvas_as_opening() {
    let (root, handle, admitted) = open_fixture();
    let room = admitted.workspaces[0].room.to_string();
    let mut root = Box::new(root);
    let root_ptr = std::ptr::from_mut(root.as_mut());
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        root,
        (1400, 768),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.relayout();
    // SAFETY: the harness owns the root; no frame is driven during this call.
    invoke_open(unsafe { &mut *root_ptr }, &room, "so.tilo.notes");
    frame.relayout();
    fn names(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        out.extend(node.name.clone());
        out.extend(node.value.clone());
        out.extend(node.description.clone());
        for child in &node.children {
            names(child, out);
        }
    }
    fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<String>) {
        if let Some(semantics) = &node.semantics {
            names(semantics, out);
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut text = vec![];
    walk(&frame.inspection().root, &mut text);
    assert!(
        !text
            .iter()
            .any(|name| name.contains("Document unavailable")),
        "the placeholder binds its scratch record: {text:?}"
    );
    assert!(
        text.iter().any(|name| name.contains("Opening")),
        "the window body says Opening…: {text:?}"
    );
    assert_eq!(entries_at(&handle, (5 << 48) | 60).len(), 1);
}

#[test]
fn a_member_who_cannot_write_the_room_does_not_speculate() {
    let (mut root, handle, admitted) = open_fixture();
    *handle.permissions.borrow_mut() = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(ui::permissions::Role::Viewer), "author"),
        vec![],
    );
    let room = admitted.workspaces[0].room.to_string();
    invoke_open(&mut root, &room, "so.tilo.notes");
    root.reconcile();
    assert!(handle.speculation.borrow().is_empty(), "no viewer flash");
    assert!(entries_at(&handle, (5 << 48) | 60).is_empty());
    assert_eq!(
        handle.session_requests.borrow().len(),
        1,
        "still sent durably"
    );
}

#[test]
fn a_refused_optimistic_open_snaps_back_and_focus_returns() {
    let (mut root, handle, admitted) = open_fixture();
    let room = admitted.workspaces[0].room.to_string();
    let active = handle.navigation.borrow()["active"].clone();
    let invocation = invoke_open(&mut root, &room, "so.tilo.calculator");
    root.reconcile();
    let predicted = (5 << 48) | 60;
    assert_eq!(entries_at(&handle, predicted).len(), 1);
    assert!(handle.speculation.borrow_mut().settle(
        invocation,
        Some(false),
        None,
        web_time::Instant::now()
    ));
    assert!(root.reconcile());
    assert!(
        entries_at(&handle, predicted).is_empty(),
        "refusal snaps back"
    );
    assert_eq!(handle.navigation.borrow()["active"], active);
    // The durable request is still the one the bridge sends.
    assert!(matches!(
        &handle.session_requests.borrow()[0].1.command,
        tilo_desktop_model::commands::DesktopCommand::OpenApp { app } if app == "so.tilo.calculator"
    ));
}

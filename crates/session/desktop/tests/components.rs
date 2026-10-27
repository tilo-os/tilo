#![allow(
    clippy::unwrap_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::{cell::RefCell, rc::Rc};
use tilo_desktop::composition::{DesktopSurface, SurfaceChrome};
use tilo_desktop::resolver::{ApplicationCatalogTileResolver, ComponentContextSource, TileRequest};
use tilo_desktop_model::{
    AppKind, DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use ui::application::{
    ApplicationCatalog, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget,
    RestoreInput,
};
use ui::application_view::InstanceViewStore;
use ui::record::{RecordOwners, RecordStore};
use ui::widget::Widget;

struct Host {
    owners: RecordOwners,
    views: Rc<InstanceViewStore>,
    wakes: Vec<ui::wake::WakeHandle>,
    cache: ui::portable_storage::PortableStorage,
}
impl ComponentContextSource for Host {
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(HostContext, RestoreInput), ui::launch::LaunchError> {
        let records = self.owners.open(definition.app_id(), "room").unwrap();
        let context = HostContext::prepare(
            definition,
            HostContextInputs {
                records,
                views: self.views.clone(),
                device_id: "device".into(),
                instance_id: request.tab.id,
                permissions: Rc::new(RefCell::new(ui::permissions::PermissionContext::member(
                    "author",
                    ui::permissions::Role::Owner,
                ))),
                author: ui::permissions::UserId("author".into()),
                action_context: tilo_actions::ActionContext::default(),
            },
            target,
        )?;
        self.wakes.push(context.wake_handle());
        Ok((context, RestoreInput::None))
    }
}
fn fixture() -> (
    DesktopTree,
    ApplicationCatalogTileResolver<Host>,
    RecordStore,
    Vec<RecordStore>,
) {
    let cache = ui::portable_storage::PortableStorage::new("profile", "device").unwrap();
    let mut owners = RecordOwners::portable(cache.clone());
    let views = Rc::new(InstanceViewStore::portable(cache.clone(), "room").unwrap());
    let mut catalog = ApplicationCatalog::new();
    let definitions = [
        word_processor::definition(),
        chat::definition(),
        notes::definition(),
    ];
    let mut tabs = vec![];
    let mut retained_records = vec![];
    for (i, definition) in definitions.into_iter().enumerate() {
        catalog.register(definition).unwrap();
        let id = u64::try_from(i).unwrap() + 10;
        let store = owners.open(definition.app_id(), "room").unwrap();
        let checkout = tilo_desktop::records::component_record(
            definition.app_id(),
            id,
            definition.record_kind.clone(),
        );
        store.create_record(&checkout).unwrap();
        store.with(&checkout.id, |doc| match i {
            0 => word_processor::initialize_empty_document(doc).unwrap(),
            1 => {
                chat::demo::start(doc, "author", "Author", "AU").unwrap();
            }
            _ => notes::read::initialize(doc),
        });
        retained_records.push(store);
        tabs.push(Tab {
            id,
            title: format!("Renamed {i}"),
            tile: TileContent::AppPlaceholder {
                kind: AppKind::Other(definition.app_id().into()),
                label: "display-only".into(),
            },
            app_title: None,
        });
    }
    let mut ws = Workspace::new(WorkspaceId(1), "Work");
    ws.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: 3,
            tabs,
            active_tab: 10,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }];
    let tree = DesktopTree {
        active: ws.id,
        workspaces: vec![ws],
        ..Default::default()
    };
    let resolver = ApplicationCatalogTileResolver {
        catalog,
        host: Host {
            owners,
            views,
            wakes: vec![],
            cache,
        },
    };
    let records = RecordStore::in_memory();
    (tree, resolver, records, retained_records)
}
#[test]
fn checked_components_keep_exact_records_and_close_owners_inside_groups() {
    let (mut tree, mut resolver, records, _retained) = fixture();
    let mut surface =
        DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    let failures = surface.take_setup_failures();
    assert!(
        failures.is_empty(),
        "{:?}",
        failures.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    let group = &mut surface.row_mut(StripId(2)).unwrap().groups[0];
    for tab in [10, 11, 12] {
        assert!(
            group
                .host_for_tab_mut(tab)
                .unwrap()
                .history_owner()
                .is_some(),
            "component owner lost for {tab}"
        );
    }
    tree.workspaces[0].strips[0].window_groups.clear();
    surface.reconcile(&tree, &records, &mut resolver);
    let reports = ui::component_lifecycle::take_component_close_reports(&mut surface);
    assert_eq!(
        reports.len(),
        3,
        "every removed component must transfer close/persistence custody, including inactive tabs"
    );
    assert!(ui::component_lifecycle::take_component_close_reports(&mut surface).is_empty());
    assert!(
        reports
            .iter()
            .all(|report| report.record_retry.is_some() || report.record_error.is_none())
    );
}

#[test]
fn real_checked_writer_input_and_resources_work_inside_shared_groups() {
    let (tree, mut resolver, records, retained) = fixture();
    let surface = DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 720));
    frame.poll_frame().unwrap();
    assert!(
        frame.resources().iter().any(|r| {
            r.kind == "tilo.rich-text"
                || r.id.as_str() == "word-processor.document"
                || r.id.as_str().starts_with("word-processor.document:")
        }),
        "checked Writer resources must remain reachable inside desktop rows: {:?}",
        frame
            .resources()
            .iter()
            .map(|r| format!("{} ({})", r.id.as_str(), r.kind))
            .collect::<Vec<_>>(),
    );
    frame
        .perform_named("word_processor", &ui::a11y::Action::Focus)
        .unwrap();
    frame.type_text("Shared desktop record");
    let checkout = tilo_desktop::records::component_record(
        "so.tilo.word-processor",
        10,
        word_processor::definition().record_kind.clone(),
    );
    let body = retained[0]
        .read(&checkout.id, |doc| {
            serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
        })
        .unwrap();
    assert!(body.contains("Shared desktop record"), "{body}");
    frame.close_components();
    assert_eq!(frame.component_close_reports().len(), 3);
}

#[test]
fn moving_checked_tabs_between_groups_preserves_record_owner() {
    let (mut tree, mut resolver, records, _retained) = fixture();
    let mut surface =
        DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    let owner = match surface.row_mut(StripId(2)).unwrap().groups[0]
        .host_for_tab_mut(10)
        .unwrap()
        .content()
    {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()).cast::<()>(),
        _ => panic!("checked Writer expected"),
    };
    let first = &mut tree.workspaces[0].strips[0].window_groups[0];
    let tab = first.tabs.remove(0);
    first.active_tab = 11;
    tree.workspaces[0].strips[0].window_groups.insert(
        0,
        WindowGroup {
            id: 4,
            tabs: vec![tab],
            active_tab: 10,
            width: WindowWidth::Wide,
            maximized: true,
            focus_history: Vec::new(),
        },
    );
    surface.reconcile(&tree, &records, &mut resolver);
    let row = surface.row_mut(StripId(2)).unwrap();
    assert_eq!(row.group_ids(), vec![4, 3]);
    assert!(row.groups[0].maximized);
    let retained = match row.groups[0].host_for_tab_mut(10).unwrap().content() {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()).cast::<()>(),
        _ => panic!("checked Writer expected"),
    };
    assert_eq!(
        retained, owner,
        "reparenting must retain the actual checked component allocation"
    );
    assert!(ui::component_lifecycle::take_component_close_reports(&mut surface).is_empty());
    assert!(surface.take_setup_failures().is_empty());
    assert_eq!(
        ui::component_lifecycle::close_components(&mut surface).len(),
        3
    );
}

#[test]
fn grouped_component_wakes_schedule_host_until_the_exact_owner_closes() {
    let (mut tree, mut resolver, records, _retained) = fixture();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink = calls.clone();
    let chrome = SurfaceChrome {
        notify: Some(std::sync::Arc::new(move || {
            sink.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })),
        ..SurfaceChrome::default()
    };
    let mut surface = DesktopSurface::build(&tree, &records, &mut resolver, chrome);
    surface.take_component_wake();
    let before = calls.load(std::sync::atomic::Ordering::Relaxed);
    resolver.host.wakes[0].wake();
    assert!(surface.take_component_wake());
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), before + 1);
    tree.workspaces[0].strips[0].window_groups.clear();
    surface.reconcile(&tree, &records, &mut resolver);
    surface.take_component_wake();
    resolver.host.wakes[0].wake();
    assert!(
        !surface.take_component_wake(),
        "closed component wake must not reanimate removed content"
    );
}

#[test]
fn inactive_chat_and_notes_actions_reach_their_actual_records_and_settle() {
    let (tree, mut resolver, records, retained) = fixture();
    let surface = DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 720));
    for (instance, resource, action, arguments) in [
        (
            11,
            tilo_app::framework::chain_resource_id(&chat::parts::chain()),
            "post",
            serde_json::json!({ "channel": "c_chat", "text": "Shared desktop message" }),
        ),
        (
            12,
            "notes.notebook".to_owned(),
            "new-note",
            serde_json::json!({ "title": "Shared desktop page" }),
        ),
    ] {
        frame
            .invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: instance,
                    address: tilo_actions::ActionAddress::new(resource, action),
                },
                arguments.as_object().unwrap(),
            )
            .unwrap();
    }
    for (store, app, id, kind, needle) in [
        (
            &retained[1],
            "so.tilo.chat",
            11,
            chat::definition().record_kind.clone(),
            "Shared desktop message",
        ),
        (
            &retained[2],
            "so.tilo.notes",
            12,
            notes::definition().record_kind.clone(),
            "Shared desktop page",
        ),
    ] {
        let record = if app == "so.tilo.chat" {
            chat::parts::part(0)
        } else {
            tilo_desktop::records::component_record(app, id, kind).id
        };
        let body = store
            .read(&record, |doc| {
                serde_json::to_string(&automerge::AutoSerde::from(doc)).unwrap()
            })
            .unwrap();
        assert!(body.contains(needle), "{body}");
    }
    frame.close_components();
    assert_eq!(frame.component_close_reports().len(), 3);
    // Portable commits are durable only after the host acknowledges its actual write queue.
    // The close report must retain custody while that acknowledgement is outstanding.
    let mut reports = frame.take_component_close_reports();
    assert!(
        reports
            .iter()
            .filter(|report| report.record_error.is_some())
            .all(|report| report.record_retry.is_some())
    );
    for write in resolver.host.cache.pending_writes() {
        resolver.host.cache.acknowledge(&write, Ok(())).unwrap();
    }
    for report in &mut reports {
        report.retry_record().unwrap();
    }
    assert!(reports.iter().all(|report| report.record_error.is_none()));
    for store in retained {
        store.try_flush().unwrap();
    }
}

#[test]
fn hierarchy_rebuild_retains_checked_component_custody_for_unchanged_tabs() {
    let (mut tree, mut resolver, records, _retained) = fixture();
    let mut surface =
        DesktopSurface::build(&tree, &records, &mut resolver, SurfaceChrome::default());
    let before = match surface.row_mut(StripId(2)).unwrap().groups[0]
        .host_for_tab_mut(10)
        .unwrap()
        .content()
    {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()).cast::<()>(),
        _ => panic!("checked component expected"),
    };
    tree.workspaces[0].strips.push(Strip {
        id: StripId(99),
        name: "Added".into(),
        window_groups: vec![],
    });
    surface.rebuild_retaining(&tree, &records, &mut resolver, SurfaceChrome::default());
    let after = match surface.row_mut(StripId(2)).unwrap().groups[0]
        .host_for_tab_mut(10)
        .unwrap()
        .content()
    {
        ui::widget::TileContent::App(app) => std::ptr::from_ref(app.as_ref()).cast::<()>(),
        _ => panic!("checked component expected"),
    };
    assert_eq!(
        before, after,
        "adding a strip must retain the actual component allocation"
    );
    assert!(ui::component_lifecycle::take_component_close_reports(&mut surface).is_empty());
    assert_eq!(surface.strip_ids(), vec![StripId(2), StripId(99)]);
    surface.take_component_wake();
    resolver.host.wakes[0].wake();
    assert!(
        surface.take_component_wake(),
        "retained owner must keep its original wake binding"
    );
    ui::component_lifecycle::close_components(&mut surface);
    surface.take_component_wake();
    resolver.host.wakes[0].wake();
    assert!(
        !surface.take_component_wake(),
        "closed retained owner must stop waking the surface"
    );
}

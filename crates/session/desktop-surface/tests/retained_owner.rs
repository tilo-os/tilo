//! Real checked mounts in a retained desktop workspace, with no paint needed for service.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures pin original checked admission and owner custody"
)]
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};
use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget, RestoreInput,
};
use ui::automerge::{ROOT, transaction::Transactable};
use ui::owner_wake::OwnerWakeQueue;
use ui::widget::{AppKind, AppWidget, IconDescriptor, TileHost, Widget};
const APP: &str = "so.tilo.retained-owner-test";
#[derive(Clone, Default)]
struct Signal {
    syncs: Rc<Cell<usize>>,
    wake: Rc<RefCell<Option<ui::WakeHandle>>>,
}
thread_local! {static NEXT:RefCell<Option<Signal>>=const{RefCell::new(None)};}
struct Root(Signal);
impl Widget for Root {
    fn name(&self) -> &'static str {
        "RetainedModelOwner"
    }
    fn sync(&mut self, model: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        let (doc, _) = ui::doc::document_ref(model, path);
        assert!(ui::record::identity(doc).is_some());
        self.0.syncs.set(
            self.0
                .syncs
                .get()
                .checked_add(1)
                .expect("bounded sync count"),
        );
    }
    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        _: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = tree.new_leaf(ui::taffy::Style::default()).unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
}
impl AppWidget for Root {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Retained owner"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("R")
    }
}
fn factory(
    mut context: HostContext,
    _: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, ui::launch::LaunchError> {
    context
        .initialize_new_record(|doc| {
            doc.put(ROOT, "body", "current retained owner")
                .map_err(|e| e.to_string())?;
            doc.commit();
            Ok(())
        })
        .map_err(ui::launch::LaunchError::Record)?;
    let signal = NEXT.with_borrow_mut(|next| next.take().unwrap());
    *signal.wake.borrow_mut() = Some(context.wake_handle());
    Ok(AppComponent::new(
        context,
        Box::new(Root(signal)),
        tilo_actions::ActionRegistry::new(),
    ))
}
struct Source {
    owners: ui::record::RecordOwners,
    root: std::path::PathBuf,
    queue: OwnerWakeQueue,
    signals: Rc<RefCell<HashMap<u64, Signal>>>,
}
impl TileSource for Source {
    fn tile(&mut self, workspace: &Workspace, tab: &Tab) -> TileHost {
        let room = workspace.room().to_string();
        let signal = Signal::default();
        self.signals.borrow_mut().insert(tab.id, signal.clone());
        NEXT.with_borrow_mut(|slot| *slot = Some(signal));
        let definition = ApplicationDefinition {
            app_id: APP.into(),
            title: "Retained owner".into(),
            icon: IconDescriptor::Glyph("R"),
            record_kind: ui::record::RecordKind::new("retained-owner"),
            room_record: false,
            required: vec![],
            optional: vec![],
            opens: &[],
            create: factory,
        };
        let records = self.owners.open(APP, &room).unwrap();
        let views = Rc::new(
            ui::application_view::InstanceViewStore::new(self.root.clone(), "device", &room)
                .unwrap(),
        );
        let permissions = Rc::new(RefCell::new(
            ui::permissions::PermissionContext::stranger("retained-author").at_unix_time(100),
        ));
        let context = HostContext::prepare(
            &definition,
            HostContextInputs {
                records,
                views,
                device_id: "device".into(),
                instance_id: tab.id,
                permissions,
                author: ui::permissions::UserId("retained-author".into()),
                action_context: tilo_actions::ActionContext::default()
                    .with_app(APP, "Retained owner"),
            },
            &LaunchTarget::New,
        )
        .unwrap();
        let mut component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        component
            .mount_with_owner_queue(&self.queue, |_| {})
            .unwrap();
        TileHost::from_component(component)
    }
}
fn tab(minter: &mut IdMinter) -> Tab {
    Tab {
        id: minter.mint(),
        title: "Retained owner".into(),
        tile: TileContent::LocalApp {
            app_id: APP.into(),
            label: "Retained owner".into(),
            note: String::new(),
        },
        app_title: None,
    }
}
#[test]
fn inactive_retained_workspace_services_only_its_exact_current_checked_owner_before_paint() {
    let root = std::env::temp_dir().join(format!("tilo-retained-owner-{}", uuid::Uuid::new_v4()));
    let mut minter = IdMinter::new(23);
    let a = tab(&mut minter);
    let aid = a.id;
    let mut tree = seed_tree(&mut minter, "First", vec![a]);
    let b = tab(&mut minter);
    let bid = b.id;
    let second = seed_tree(&mut minter, "Second", vec![b]);
    tree.workspaces.extend(second.workspaces);
    tree.active_strip.extend(second.active_strip);
    tree.active_group.extend(second.active_group);
    let signals = Rc::new(RefCell::new(HashMap::new()));
    let queue = OwnerWakeQueue::default();
    let source = Source {
        owners: ui::record::RecordOwners::new(root.clone()),
        root: root.clone(),
        queue: queue.clone(),
        signals: signals.clone(),
    };
    let (surface, handle) = DesktopSurface::new(tree, 23, Box::new(source));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("retained-author".into()),
    );
    driver.attach_owner_wake_queue(queue.clone()).unwrap();
    driver.relayout();
    driver.service_pending_owner_models().unwrap();
    let route = handle
        .snapshot("g", vec![])
        .entries
        .into_iter()
        .find(|entry| entry.route.tab == bid.to_string())
        .unwrap()
        .route;
    let command =
        serde_json::from_value(serde_json::json!({"kind":"select","generation":"g","route":route}))
            .unwrap();
    handle.dispatch(&command, "g").unwrap();
    driver.relayout();
    driver.service_pending_owner_models().unwrap();
    let a = signals.borrow()[&aid].clone();
    let b = signals.borrow()[&bid].clone();
    let before = (a.syncs.get(), b.syncs.get());
    let layouts = driver.relayout_frames();
    a.wake.borrow().as_ref().unwrap().wake();
    let serviced = driver.service_pending_owner_models();
    let after = (a.syncs.get(), b.syncs.get());
    let no_paint = layouts == driver.relayout_frames();
    let empty = !queue.has_pending();
    driver.close_components();
    let reports = driver.take_component_close_reports();
    let late = a.wake.borrow().as_ref().unwrap().clone();
    late.wake();
    let closed_empty = !queue.has_pending();
    drop(driver);
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(serviced.unwrap(), 1);
    assert_eq!(after, (before.0 + 1, before.1));
    assert!(no_paint && empty);
    assert_eq!(reports.len(), 2);
    assert!(
        reports.iter().all(|r| r.record_error.is_none()
            && r.view_error.is_none()
            && r.teardown_error.is_none())
    );
    assert!(closed_empty);
}

/// Resizing a window whose tab holds a checked component is ordinary input:
/// the press on its edge and the drag after it reach every app in the window
/// (to say which edge holds still) without handing its content out to be
/// replaced, which a checked component refuses with a panic. The browser
/// turns that panic into a desktop that stopped working.
#[test]
fn dragging_the_edge_of_a_window_with_a_checked_component_resizes_it() {
    let root = std::env::temp_dir().join(format!("tilo-edge-drag-{}", uuid::Uuid::new_v4()));
    let mut minter = IdMinter::new(29);
    let tabs = vec![tab(&mut minter), tab(&mut minter)];
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let width_of = |tree: &tilo_desktop_model::DesktopTree| {
        tree.workspaces[0].strips[0].window_groups[0].width
    };
    let before = width_of(&tree);
    let queue = OwnerWakeQueue::default();
    let source = Source {
        owners: ui::record::RecordOwners::new(root.clone()),
        root: root.clone(),
        queue: queue.clone(),
        signals: Rc::default(),
    };
    let (surface, handle) = DesktopSurface::new(tree, 29, Box::new(source));
    handle.set_viewport_width(1440.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1440, 900),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("retained-author".into()),
    );
    driver.attach_owner_wake_queue(queue).unwrap();
    driver.relayout();
    driver.relayout();
    let shown = handle.shown_tabs();
    assert_eq!(shown.len(), 2, "both windows are shown: {shown:?}");
    let left = shown[0].1;
    let edge = (left.x1 + 1.0, left.center().y);
    let button =
        |state| ui::synthetic_input::mouse_input(state, ui::winit::event::MouseButton::Left);
    // The press alone begins the edge drag.
    driver.send(&ui::synthetic_input::cursor_moved(edge.0, edge.1));
    driver.send(&button(ui::winit::event::ElementState::Pressed));
    driver.relayout();
    for step in 1..=4 {
        let x = edge.0 - 30.0 * f64::from(step);
        driver.send(&ui::synthetic_input::cursor_moved(x, edge.1));
        driver.relayout();
    }
    driver.send(&button(ui::winit::event::ElementState::Released));
    driver.relayout();
    let after = handle.read(width_of);
    driver.close_components();
    let _ = driver.take_component_close_reports();
    drop(driver);
    std::fs::remove_dir_all(root).unwrap();
    assert_ne!(after, before, "the drag resized the window");
}

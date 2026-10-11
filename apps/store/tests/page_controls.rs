//! The controls of the Store's pages reach the same typed actions an agent
//! calls: a remote's row action, the filter reset, and the catalogue's
//! search launcher.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use store::StoreApp;
use ui::headless::Framed;
use ui::tilo_resource::Resource as _;
use ui::widget::{Widget, find_test_id};
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};

fn lay_out(app: StoreApp) -> Framed<StoreApp> {
    Framed::new(
        ui::State::new(),
        app,
        ui::headless::viewport((1200.0, 800.0)),
        ui::Theme::dark(),
    )
}

fn staged(scenario: &str) -> StoreApp {
    let mut app = store::fixture::seeded_app();
    app.stage_scene(Some(scenario));
    app
}

fn rect_of(root: &dyn Widget, id: &str) -> Rect {
    find_test_id(root, id)
        .unwrap_or_else(|| panic!("no widget with test id {id:?}"))
        .get_rect()
}

fn has_test_id(w: &dyn Widget, id: &str) -> bool {
    find_test_id(w, id).is_some()
}

fn cursor(app: &mut Framed<StoreApp>, at: Point) {
    Framed::harness_mut(app).dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: PhysicalPosition::new(at.x, at.y),
    });
}

fn click(app: &mut Framed<StoreApp>, at: Point) {
    cursor(app, at);
    for state in [ElementState::Pressed, ElementState::Released] {
        Framed::harness_mut(app).dispatch_to_root(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        });
    }
}

fn row_action_target(root: &dyn Widget, id: &str, label: &str) -> Option<String> {
    if root.test_id() == Some(id) {
        return root.accessibility()?.children.into_iter().find_map(|node| {
            (node.name.as_deref() == Some(label))
                .then_some(node.action_target)
                .flatten()
        });
    }
    root.children()
        .into_iter()
        .find_map(|child| row_action_target(child, id, label))
}

fn activate_row_action(
    root: &mut dyn Widget,
    id: &str,
    target: &str,
    state: &mut ui::State,
) -> bool {
    if root.test_id() == Some(id) {
        return root.perform_semantic_action(
            target,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut state.doc),
            &state.root,
        );
    }
    root.children_mut()
        .into_iter()
        .any(|child| activate_row_action(child, id, target, state))
}

/// Remove names its published row action rather than a coordinate now occupied by Disable.
#[test]
fn a_remote_rows_action_invokes_the_remotes_own_remove_action() {
    let mut app = lay_out(staged("remotes"));
    let row = "store.remote:flathub";
    let target = row_action_target(&*app, row, "Remove remote")
        .expect("the remote row publishes its own Remove control");
    let (root, state) = Framed::split(&mut app);
    assert!(
        activate_row_action(root, row, &target, state),
        "the published Remove action activates its production widget binding"
    );
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("the row action queued an invocation");
    assert_eq!(invocation.address.resource, store::Store::KIND);
    assert_eq!(invocation.address.action, "remove-remote");
    assert_eq!(invocation.arguments["name"], "flathub");
}

/// The persistent account toggle stays visible and invokes Disable independently of Remove.
#[test]
fn a_remote_rows_persistent_toggle_invokes_its_own_disable_action() {
    let mut app = lay_out(staged("remotes"));
    let toggle = rect_of(&*app, "store.remote:flathub.enabled");
    click(&mut app, toggle.center());
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("the persistent toggle queued an invocation");
    assert_eq!(invocation.address.resource, store::Store::KIND);
    assert_eq!(invocation.address.action, "disable-remote");
    assert_eq!(invocation.arguments["remote"], "remote:flathub");
}

/// While a query narrows the catalogue the page offers one way back, bound to the browse action.
#[test]
fn a_narrowed_catalogue_offers_to_clear_its_filters() {
    let mut app = lay_out(staged("search"));
    let clear = rect_of(&*app, "store.filters.clear");
    click(&mut app, clear.center());
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("clearing filters is an exported action");
    assert_eq!(invocation.address.resource, store::Store::KIND);
    assert_eq!(invocation.address.action, "clear-filters");

    let plain = lay_out(store::fixture::seeded_app());
    assert!(
        !has_test_id(&*plain, "store.filters.clear"),
        "an unfiltered catalogue has nothing to clear"
    );
}

/// The magnifier opens the platform search for this app instead of a field of the Store's own.
#[test]
fn the_catalogue_searches_through_the_platform_launcher() {
    let mut app = lay_out(store::fixture::seeded_app());
    let search = rect_of(&*app, "search-open");
    click(&mut app, search.center());
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("the launcher is an exported action");
    assert_eq!(invocation.address.resource, "tilo.host:search");
    assert_eq!(invocation.address.action, "open");
}

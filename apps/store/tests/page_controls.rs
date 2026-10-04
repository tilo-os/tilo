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
use ui::widget::Widget;
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
    let mut app = StoreApp::new();
    store::shots::stage(&mut app, Some(scenario));
    app
}

fn rect_of(root: &dyn Widget, id: &str) -> Rect {
    fn find(w: &dyn Widget, id: &str) -> Option<Rect> {
        if w.test_id() == Some(id) {
            return Some(w.get_rect());
        }
        w.children().into_iter().find_map(|child| find(child, id))
    }
    find(root, id).unwrap_or_else(|| panic!("no widget with test id {id:?}"))
}

fn has_test_id(w: &dyn Widget, id: &str) -> bool {
    w.test_id() == Some(id) || w.children().into_iter().any(|child| has_test_id(child, id))
}

fn cursor(app: &mut Framed<StoreApp>, at: Point) {
    Framed::harness_mut(app).dispatch_to_root(&WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(at.x, at.y),
    });
}

fn click(app: &mut Framed<StoreApp>, at: Point) {
    cursor(app, at);
    for state in [ElementState::Pressed, ElementState::Released] {
        Framed::harness_mut(app).dispatch_to_root(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state,
            button: MouseButton::Left,
        });
    }
}

/// A remote's trailing row action asks the host to remove that remote, through its published address.
#[test]
fn a_remote_rows_action_invokes_the_remotes_own_remove_action() {
    let mut app = lay_out(staged("remotes"));
    let row = rect_of(&*app, "store.remote.flathub");
    // The row reveals its action while the pointer is on it.
    cursor(&mut app, row.center());
    let action = Point::new(row.x1 - 24.0, row.center().y);
    click(&mut app, action);
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("the row action queued an invocation");
    assert_eq!(invocation.address.resource, "remote:flathub");
    assert_eq!(invocation.address.action, "remove");
}

/// While a query narrows the catalogue the page offers one way back, bound to the browse action.
#[test]
fn a_narrowed_catalogue_offers_to_clear_its_filters() {
    let mut app = lay_out(staged("search"));
    let clear = rect_of(&*app, "store.filters.clear");
    click(&mut app, clear.center());
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("clearing filters is an exported action");
    assert_eq!(
        invocation.address.resource,
        store::browse_actions::BROWSE_RESOURCE_ID
    );
    assert_eq!(invocation.address.action, "clear");

    let plain = lay_out(store::shots::seeded_app());
    assert!(
        !has_test_id(&*plain, "store.filters.clear"),
        "an unfiltered catalogue has nothing to clear"
    );
}

/// The magnifier opens the platform search for this app instead of a field of the Store's own.
#[test]
fn the_catalogue_searches_through_the_platform_launcher() {
    let mut app = lay_out(store::shots::seeded_app());
    let search = rect_of(&*app, "search-open");
    click(&mut app, search.center());
    let invocation = ui::widget::controls::take_control_invocation(&mut *app)
        .expect("the launcher is an exported action");
    assert_eq!(invocation.address.resource, "tilo.host:search");
    assert_eq!(invocation.address.action, "open");
}

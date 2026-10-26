#![allow(clippy::unwrap_used, reason = "test assertions")]

//! The room's record an app keeps one of, not on this device when its
//! window opens: the room may hold it, so the window shows what is on its
//! way until the room's first sync, then the app's view. A window no shell
//! hosts has only what this device has, and shows the view at once.

use std::cell::Cell;

use tilo_app::advanced::Scene;
use tilo_app::app::AppSurface;
use tilo_app::framework::testing::{TestApp, drive_live_window};
use tilo_app::prelude::*;
use tilo_app::room_sync::{self, RoomSync, RoomSyncState};
use tilo_app::winit::dpi::PhysicalSize;
use tilo_app::winit::window::CursorIcon;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub cards: Vec<String>,
}

#[resource(kind = "test.room-arrival-board", label = "Board")]
impl Board {
    /// Add a card.
    #[action]
    pub fn add(&mut self, card: String) {
        self.cards.push(card);
    }
}

thread_local! {
    /// How many times the app's view was built.
    static VIEWS: Cell<usize> = const { Cell::new(0) };
}

struct Boards;

impl ResourceApp for Boards {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-room-arrival";
    const TITLE: &'static str = "Boards";
    const ROOM_RECORD: bool = true;

    fn view(cx: &mut ViewCx<Board>) -> Element {
        VIEWS.with(|views| views.set(views.get().saturating_add(1)));
        Box::new(cx.text(|board| board.cards.join(", ")))
    }
}

fn sync(state: RoomSyncState) -> RoomSync {
    RoomSync {
        state,
        workspace: "Lounge".into(),
        writable: None,
    }
}

#[test]
fn the_rooms_record_waits_for_the_rooms_first_sync() {
    room_sync::hosted_by_shell();
    room_sync::set(Some(sync(RoomSyncState::Connecting)));
    let mut app = TestApp::<Boards>::new().unwrap();
    app.harness().relayout();
    assert_eq!(
        VIEWS.with(Cell::get),
        0,
        "the room may hold the record: the view waits for it"
    );
    assert!(
        room_sync::set(Some(sync(RoomSyncState::Synced))),
        "the first sync changes what the window shows"
    );
    app.harness().relayout();
    assert!(
        VIEWS.with(Cell::get) > 0,
        "after the first sync the room's record is here"
    );
}

#[test]
fn a_window_no_shell_hosts_shows_the_view_at_once() {
    let mut app = TestApp::<Boards>::new().unwrap();
    app.harness().relayout();
    assert!(VIEWS.with(Cell::get) > 0, "nothing to wait for");
}

struct Window;

impl AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> PhysicalSize<u32> {
        PhysicalSize::new(800, 600)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _: CursorIcon) {}
}

#[test]
fn a_window_shows_the_rooms_record_in_the_frame_after_its_first_sync() {
    room_sync::hosted_by_shell();
    room_sync::set(Some(sync(RoomSyncState::Connecting)));
    drive_live_window::<Boards, _>(|app| {
        let mut scene = Scene::new();
        app.surface_frame(&Window, &mut scene);
        assert_eq!(VIEWS.with(Cell::get), 0, "the view waits for the room");
        room_sync::set(Some(sync(RoomSyncState::Synced)));
        scene.reset();
        app.surface_frame(&Window, &mut scene);
        assert!(
            VIEWS.with(Cell::get) > 0,
            "the frame after the first sync shows the room's record"
        );
    })
    .unwrap();
}

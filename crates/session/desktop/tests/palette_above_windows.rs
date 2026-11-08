//! The palette's host stands above the windows: while it is up the compositor gives it the pointer
//! across its card, and once it closes nothing of the palette is painted there any more.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use tilo_compositor::SurfaceHost as _;
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::desktop_host::{DesktopChrome, DesktopRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::commands::DesktopCommand;
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use tilo_surface_host::{Placement, WidgetHost};
use ui::widget::TileHost;
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState};

/// The window of the test desk, with its content as a plain notice.
struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

struct Rules {
    handle: DesktopHandle,
}

impl DesktopRules for Rules {
    fn tree(&self) -> DesktopTree {
        self.handle.read(Clone::clone)
    }

    fn author(&self) -> String {
        "Ada".into()
    }

    fn may(&self, _: &DesktopCommand) -> Result<(), String> {
        Ok(())
    }

    fn may_create_workspace(&self) -> Result<(), String> {
        Ok(())
    }
}

fn desk_tree() -> DesktopTree {
    let tabs = vec![Tab {
        id: 20,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }];
    let group = WindowGroup {
        id: 3,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree.active_group.insert(StripId(2), 3);
    tree
}

const SIZE: (u32, u32) = (1280, 800);

/// The chrome as the session's own host shows it: over the windows, covering the output.
fn chrome_host() -> (WidgetHost, DesktopHandle, ui::headless::FrameHarness) {
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), SIZE);
    desk.relayout();
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(Rules {
            handle: handle.clone(),
        }),
        TopBarChrome::from_tree(&tree),
        TiloMenu::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let keyboard = handle.clone();
    let host = WidgetHost::new(
        Box::new(chrome),
        Placement {
            origin: (0, 0),
            size: SIZE,
            scale: 1.0,
        },
        false,
    )
    .with_keyboard(Rc::new(move || keyboard.search_open()));
    (host, handle, desk)
}

/// The search chord, as a person presses it.
fn send_chord(host: &mut WidgetHost) {
    host.host_mut()
        .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::SUPER,
        )));
    host.host_mut()
        .send_host_event(&ui::synthetic_input::key_press(
            Key::Character("k".into()),
            None,
        ));
    host.host_mut()
        .send_host_event(&WindowEvent::ModifiersChanged(Modifiers::from(
            ModifiersState::empty(),
        )));
}

#[test]
fn the_palettes_host_stands_above_the_windows_and_paints_nothing_once_closed() {
    let (mut host, handle, mut desk) = chrome_host();
    assert!(!host.below_windows(), "the chrome stands above the windows");
    host.relayout();
    host.prepare_frame();
    // The window is in front, where the person left it.
    assert!(
        !host.accepts_pointer(640.0, 400.0),
        "nothing of the chrome there"
    );

    send_chord(&mut host);
    desk.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(handle.search_open(), "the chord opened the palette");
    let card = {
        let search = handle.search_palette();
        let search = search.borrow();
        search.palette().expect("the palette is up").get_rect()
    };
    let centre = card.center();
    assert!(
        host.accepts_pointer(centre.x, centre.y),
        "the palette is painted at {centre:?}, over the window"
    );
    // The dim behind the card reaches the rest of the display too: what the palette stands over is
    // still drawn, under it.
    assert!(
        host.accepts_pointer(640.0, 400.0),
        "the scrim covers the window"
    );

    // Closing it takes the keys back and leaves nothing painted.
    handle.toggle_search();
    desk.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(!handle.search_open());
    assert!(
        !host.accepts_pointer(centre.x, centre.y),
        "nothing of the closed palette is painted at {centre:?}"
    );
    assert!(!host.accepts_pointer(640.0, 400.0), "nor its scrim");
}

//! A floating card drawn over the windows reaches the composited output: the pixels the card was
//! drawn with show where the card stands, in the software path a headless session composes with.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use tilo_compositor::{Compositor, OutputConfig, StackPolicy, SurfaceHost as _};
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

/// The chrome over the desk at `scale` : built after `_compositor` opened its backend, as the
/// session builds its surfaces after its compositor. A host built before the backend is open draws
/// its frames for a compositor that samples textures rather than one that reads pixels, which is
/// not the path a headless session takes.
fn chrome_host(
    _compositor: &mut Compositor,
    scale: f64,
    size: (u32, u32),
) -> (WidgetHost, DesktopHandle, ui::headless::FrameHarness) {
    let tree = desk_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(Notices));
    let mut desk =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), size);
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
            size: (
                ui::num::u32_of_f64((f64::from(size.0) / scale).round()),
                ui::num::u32_of_f64((f64::from(size.1) / scale).round()),
            ),
            scale,
        },
        false,
    )
    .with_keyboard(Rc::new(move || keyboard.search_open()));
    (host, handle, desk)
}

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

/// How far apart, in card pixels, the card's samples stand.
const SAMPLE_STEP: f64 = 8.0;

/// The card the palette stands in, drawn over the output at `scale` .
fn assert_palette_shows(scale: f64, size: (u32, u32)) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).unwrap();
    compositor
        .add_output(
            &OutputConfig::new(
                "HEADLESS-1",
                i32::try_from(size.0).unwrap_or(i32::MAX),
                i32::try_from(size.1).unwrap_or(i32::MAX),
            )
            .with_scale(scale),
        )
        .unwrap();
    let (mut host, handle, mut desk) = chrome_host(&mut compositor, scale, size);
    host.relayout();
    host.prepare_frame();
    send_chord(&mut host);
    desk.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(handle.search_open(), "the chord opened the palette");
    let card = {
        let search = handle.search_palette();
        let search = search.borrow();
        search.palette().unwrap().get_rect()
    };
    compositor.add_host(Box::new(host));
    let mut shot = None;
    for _ in 0..8 {
        compositor.draw_frame().unwrap();
        shot = Some(compositor.screenshot("HEADLESS-1").unwrap());
    }
    let shot = shot.unwrap();
    // The card's own ground is opaque, so somewhere inside it the output differs from the scrim at
    // the edge of the display.
    let corner = shot.pixel(4, 4).unwrap();
    let mut differing = 0u32;
    let mut samples = 0u32;
    let inner = card.inflate(-24.0, -24.0);
    let columns = ui::num::usize_of_f64((inner.width() / SAMPLE_STEP).ceil());
    let rows = ui::num::usize_of_f64((inner.height() / SAMPLE_STEP).ceil());
    for row in 0..rows {
        let y = inner.y0 + ui::num::f64_of_usize(row) * SAMPLE_STEP;
        for column in 0..columns {
            let x = inner.x0 + ui::num::f64_of_usize(column) * SAMPLE_STEP;
            let pixel = shot
                .pixel(
                    ui::num::u32_of_f64(x * scale),
                    ui::num::u32_of_f64(y * scale),
                )
                .unwrap();
            if pixel != corner {
                differing = differing.saturating_add(1);
            }
            samples = samples.saturating_add(1);
        }
    }
    assert!(
        samples > 100,
        "the card is {card:?}, large enough to sample"
    );
    assert!(
        differing.saturating_mul(2) > samples,
        "the card shows over the scrim at scale {scale}: {differing} of {samples} pixels \
         differ from {corner:?}"
    );
}

#[test]
fn the_open_palettes_card_reaches_the_composited_output() {
    let _settled = ui::motion::testing::settled();
    assert_palette_shows(1.0, (1280, 800));
}

#[test]
fn the_open_palettes_card_reaches_the_composited_output_at_twice_the_pixels() {
    let _settled = ui::motion::testing::settled();
    assert_palette_shows(2.0, (2560, 1600));
}

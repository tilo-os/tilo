//! Pictures of sharing a window with its workspace, from both ends.
//!
//! The device that has the window: one shell with a window this device maps, its menu open on the
//! row that shares it with the workspace, on the row that stops once the session reads it as
//! shared, and, while a device asked to see it and nothing of it went out yet, on the row that
//! says the device waits, under a bar that says the same.
//!
//! Every other device: the tab the workspace's layout lists that window in, as one that runs on
//! the device that has it. Not shared; shared and still on its way; on a device nothing is heard
//! of, or one the workspace no longer admits; on a device nothing names; with its share starting;
//! and after this device let it go. None of them is an empty pane.
//!
//! Captured in both themes and both locales, through the production frame path
//! ([`ui::headless::FrameHarness`]) and the menu a host without a session mounts over its shell.
//!
//! ```text
//! cargo run -p tilo-desktop --example window_share_shots -- ~/.cache/window-share-shots
//! ```

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use tilo_desktop::chrome::{
    ActiveExposure, ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource,
};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{IdMinter, Layout, Tab, TabId, TileContent, WindowWidth, Workspace};
use tilo_desktop_surface::surface::window_menu::popup::WindowMenuPopup;
use tilo_desktop_surface::{
    DesktopHandle, DesktopSurface, Elsewhere, NeighbourMsg, PeerStreamStart, StreamState,
    TileSource, WorkspaceShare,
};
use tilo_session_core::desktop::i18n::{t, t_args};
use tilo_session_core::desktop::seed_tree;
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::state::State;
use ui::taffy::{Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::widget::{
    AppModel, DrawChild as _, EventCtx, ModelPath, TaffyTreeExt as _, TileContent as Shown,
    TileHost, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

/// The windows' own pictures: a note on this device.
struct Windows;

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(Shown::Notice {
            title: format!(
                "{}\n\nShopping list\nmilk, bread, coffee beans, a new lamp for the hall",
                tab.title
            ),
            detail: "Yesterday, 18:42".to_owned(),
        })
    }

    /// This device's window is on a record, so the sharing app lists it and its menu offers to
    /// post it in a channel too: the two shares are pictured side by side.
    fn window_resource(&self, tab: tilo_desktop_model::TabId) -> Option<(String, String)> {
        (tilo_desktop_model::peer_salt_of(tab) == 7)
            .then(|| ("notes-record".to_owned(), "notes".to_owned()))
    }

    fn draws_embeds(&self) -> bool {
        true
    }
}

/// What the bar is told of who asked to see this device's windows: the devices that wait for one.
struct Viewers {
    tree: Layout,
    waiting: Vec<ActiveExposure>,
}

impl DesktopChromeSource for Viewers {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        DesktopChromeSnapshot {
            clock: clock(),
            active_exposures: self.waiting.clone(),
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> Layout {
        self.tree.clone()
    }

    fn intent(&self, _: ChromeIntent) -> bool {
        false
    }
}

/// The time every picture is taken at.
fn clock() -> ClockSnapshot {
    ClockSnapshot {
        unix_seconds: 1_794_250_800,
        clock: "9:41".into(),
        weekday: "Mon".into(),
        date: "Nov 9".into(),
    }
}

/// The shell under the menu a host without a session mounts over it, and under the bar where the
/// picture is of what the bar says.
struct Stage {
    bar: Option<TopBarChrome>,
    shell: DesktopSurface,
    menu: WindowMenuPopup,
    rect: Rect,
}

impl Widget for Stage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut nodes = Vec::new();
        if let Some(bar) = &mut self.bar {
            nodes.push(bar.build_layout(taffy, theme, map));
        }
        nodes.push(self.shell.build_layout(taffy, theme, map));
        nodes.push(self.menu.build_layout(taffy, theme, map));
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &nodes, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let mut children = taffy.children(node).unwrap_or_default().into_iter();
        if let Some(bar) = &mut self.bar
            && let Some(node) = children.next()
        {
            bar.apply_layout(taffy, node, inner);
        }
        if let Some(shell) = children.next() {
            self.shell.apply_layout(taffy, shell, inner);
        }
        if let Some(menu) = children.next() {
            self.menu.apply_layout(taffy, menu, inner);
        }
    }

    /// The shell builds a tab's tile again in its own pass, when what the tab shows changed.
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let bar = self.bar.as_mut().is_some_and(|bar| bar.update(state, path));
        let shell = self.shell.update(state, path);
        let menu = self.menu.update(state, path);
        bar || shell || menu
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.shell
            .draw_child(scene, transform, text, theme, presence);
        if let Some(bar) = &mut self.bar {
            bar.draw_child(scene, transform, text, theme, presence);
        }
        self.menu
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let first = usize::from(self.bar.is_some());
        let (Some(&shell), Some(&menu)) =
            (children.get(first), children.get(first.wrapping_add(1)))
        else {
            return;
        };
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        } else {
            self.shell
                .handle_event(event, &mut ctx.reborrow(ctx.path, shell));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![&self.shell, &self.menu];
        if let Some(bar) = &self.bar {
            children.push(bar);
        }
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![&mut self.shell, &mut self.menu];
        if let Some(bar) = &mut self.bar {
            children.push(bar);
        }
        children
    }

    fn name(&self) -> &'static str {
        "WindowShareStage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// A window this device maps, minted here.
fn mapped(minter: &mut IdMinter) -> Tab {
    Tab {
        id: minter.mint(),
        title: "Notes".to_owned(),
        tile: TileContent::Foreign {
            app_id: "so.tilo.notes".to_owned(),
            surface_id: 41,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// The app whose window runs on another device.
const BROWSER: &str = "so.tilo.browser";
/// The surface that device names its window by, and the devices at each end of its stream.
const SURFACE: u64 = 42;
const SHARER: u64 = 7;
const LOCAL: u64 = 11;

/// The tab every other device lists the Browser's window in: it runs on the device the shell calls
/// `device` , and only there.
fn hosted(device: &str) -> Tab {
    Tab {
        id: IdMinter::new(9).mint(),
        title: "Browser".to_owned(),
        tile: TileContent::HostedWindow {
            app_id: BROWSER.to_owned(),
            label: "Browser".to_owned(),
            host: "ab".repeat(32),
            host_label: device.to_owned(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// What the device running the window announces when its share starts, as this session reads it.
fn stream_start(tab: TabId, device: &str) -> PeerStreamStart {
    let announcement = NeighbourMsg::SurfaceStreamStart {
        surface: SURFACE,
        source_peer: SHARER,
        width: 1280,
        height: 800,
        codec: "vp9".into(),
        ts: 0,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 0,
        app_id: BROWSER.into(),
        tab,
    };
    PeerStreamStart::announced(&announcement, LOCAL, device, "Browser", false)
        .expect("a stream start is what the announcement is")
}

/// What a device that lists the window shows in its tab.
enum Tabbed {
    /// No share stands in the tab, and the session reads the window so.
    Elsewhere(Elsewhere),
    /// Its share started, and nothing of the window is here yet.
    Starting,
    /// Its share was here, and this device let the window go.
    LetGo,
}

/// The shell of a device that lists the window of the device called `device` , its tab showing
/// what `tabbed` says, beside a window of its own.
fn watching(
    theme: Theme,
    minter: &mut IdMinter,
    device: &str,
    tabbed: &Tabbed,
) -> Result<FrameHarness, String> {
    let tab = hosted(device);
    let id = tab.id;
    let (mut frame, handle) = shell(
        theme,
        (1280, 800),
        vec![mapped(minter), tab],
        WorkspaceShare::default(),
        None,
    );
    let state = match tabbed {
        Tabbed::Elsewhere(state) => state.clone(),
        Tabbed::Starting | Tabbed::LetGo => Elsewhere::Awaited {
            waiting_for: String::new(),
        },
    };
    handle.set_elsewhere(vec![(id, state)]);
    if matches!(tabbed, Tabbed::Starting | Tabbed::LetGo) {
        handle
            .show_peer_surface(&stream_start(id, device))
            .ok_or("the window's tab takes no share")?;
    }
    settle(&mut frame);
    let app = tilo_desktop_search::names::app_name(BROWSER);
    let where_it_runs = if device.is_empty() {
        t_args("peer-window-elsewhere-unnamed", &[("app", app.into())])
    } else {
        t_args(
            "peer-window-elsewhere",
            &[("app", app.into()), ("device", device.into())],
        )
    };
    match tabbed {
        Tabbed::Elsewhere(_) => {}
        Tabbed::Starting => {
            if handle.peer_surface_read(id).map(|read| read.state) != Some(StreamState::Started) {
                return Err("the share did not start in the window's tab".into());
            }
            if shows(&frame, &where_it_runs) {
                return Err("a tab whose share started still says where its window runs".into());
            }
            // The card of something arriving names where it comes from once it has waited a
            // moment: the picture is of the card as a person waiting sees it.
            std::thread::sleep(std::time::Duration::from_millis(2_200));
            settle(&mut frame);
            return Ok(frame);
        }
        Tabbed::LetGo => {
            frame
                .invoke_published(
                    &format!("desktop.peer-surface:{SURFACE}"),
                    "end",
                    &serde_json::Map::new(),
                )
                .map_err(|refused| format!("the tile does not let the window go: {refused:?}"))?;
            settle(&mut frame);
        }
    }
    if !shows(&frame, &where_it_runs) {
        return Err(format!("the window's tab does not say “{where_it_runs}”"));
    }
    Ok(frame)
}

/// A shell showing `tabs` , whose session reads every window it can share as `share` . Where a
/// device called `waiting` asked to see the first window and nothing of it went out yet, the bar
/// stands over the shell and says so.
fn shell(
    theme: Theme,
    viewport: (u32, u32),
    tabs: Vec<Tab>,
    share: WorkspaceShare,
    waiting: Option<&str>,
) -> (FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs = tabs
        .into_iter()
        .map(|tab| match tab.tile {
            TileContent::Foreign { .. } => Tab {
                id: minter.mint(),
                ..tab
            },
            _ => tab,
        })
        .collect();
    let mut tree = seed_tree(&mut minter, "Design", tabs);
    // Side by side where there are two, so both are read whole; alone, the window fills the
    // strip as it does on a phone.
    let groups = &mut tree.workspaces[0].strips[0].window_groups;
    let alone = groups.len() == 1;
    for group in groups {
        group.width = WindowWidth::Fraction(480);
        group.maximized = alone;
    }
    let bar = waiting.map(|name| {
        let first = tree.workspaces[0].strips[0].window_groups[0].tabs[0].id;
        TopBarChrome::from_tree(&tree).with_chrome(Rc::new(Viewers {
            tree: tree.clone(),
            waiting: vec![ActiveExposure {
                peer: LOCAL,
                surface: first,
                can_control: false,
                streaming: false,
                name: name.to_owned(),
            }],
        }))
    });
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Windows));
    handle.set_viewport_width(f64::from(viewport.0));
    handle.reads_workspace_shares(Rc::new(move |_, _| Some(share)));
    let stage = Stage {
        bar,
        shell: surface,
        menu: WindowMenuPopup::new(handle.clone()),
        rect: Rect::ZERO,
    };
    let mut frame = FrameHarness::for_root(State::new(), stage, viewport, theme);
    settle(&mut frame);
    (frame, handle)
}

/// The passes a change takes to reach the frame, over the time an entrance takes to come to
/// rest: a menu is captured as it stands, not as it arrives.
fn settle(frame: &mut FrameHarness) {
    for _ in 0..12 {
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the shell");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    frame.request_layout();
    frame.poll_frame().expect("a frame draws the shell");
}

/// Whether the frame shows something reading `label` .
fn shows(frame: &FrameHarness, label: &str) -> bool {
    frame
        .a11y_nodes()
        .into_iter()
        .any(|(_, node)| node.label() == Some(label))
}

/// Press the secondary button on the first window's bar, as a person asks for its menu. Whether
/// the menu came up with the row reading `row` .
fn open_menu(frame: &mut FrameHarness, row: &str) -> bool {
    for y in (10..=120).step_by(4) {
        frame.send(&ui::synthetic_input::cursor_moved(180.0, f64::from(y)));
        frame.send(&ui::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Right,
        ));
        frame.send(&ui::synthetic_input::mouse_input(
            ElementState::Released,
            MouseButton::Right,
        ));
        settle(frame);
        if shows(frame, row) {
            return true;
        }
    }
    false
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("window-share-shots"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    let mut minter = IdMinter::new(7);
    // The desktop's catalogue is on this thread before a locale is chosen in it.
    tilo_session_core::desktop::i18n::ensure();
    for locale in ["en-US", "de-DE"] {
        ui::i18n::I18nManager::set_locale(locale);
        for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            // The device that has the window: not shared, shared and going out to the one device
            // that asked for it, and shared with that device still waiting for it.
            let states = [
                ("share", WorkspaceShare::default(), None),
                (
                    "shared",
                    WorkspaceShare {
                        shared: true,
                        viewers: 1,
                        streaming: true,
                    },
                    None,
                ),
                (
                    "waiting",
                    WorkspaceShare {
                        shared: true,
                        viewers: 1,
                        streaming: false,
                    },
                    Some("Ada"),
                ),
            ];
            for (state, share, waiting) in states {
                let row = match (share.shared, share.streaming) {
                    (false, _) => t("window-menu-share-with-workspace"),
                    (true, true) => {
                        t_args("window-menu-stop-sharing-watched", &[("viewers", 1.into())])
                    }
                    (true, false) => {
                        t_args("window-menu-stop-sharing-waiting", &[("viewers", 1.into())])
                    }
                };
                let (mut frame, _) = shell(
                    theme.clone(),
                    (1280, 800),
                    vec![mapped(&mut minter)],
                    share,
                    waiting,
                );
                if !open_menu(&mut frame, &row) {
                    return Err(format!("the window's menu does not offer “{row}”").into());
                }
                let path = output.join(format!("{locale}-{theme_name}-{state}.png"));
                frame.render_png(&path);
                println!("{}", path.display());
            }
            // Every other device: the tab that lists the window, which runs on the first one.
            let tabs = [
                (
                    "not-shared",
                    "Laptop",
                    Tabbed::Elsewhere(Elsewhere::NotShared),
                ),
                ("unnamed", "", Tabbed::Elsewhere(Elsewhere::NotShared)),
                (
                    "awaited",
                    "Laptop",
                    Tabbed::Elsewhere(Elsewhere::Awaited {
                        waiting_for: String::new(),
                    }),
                ),
                ("away", "Laptop", Tabbed::Elsewhere(Elsewhere::Away)),
                ("left", "Laptop", Tabbed::Elsewhere(Elsewhere::Left)),
                ("starting", "Laptop", Tabbed::Starting),
                ("let-go", "Laptop", Tabbed::LetGo),
            ];
            for (state, device, tabbed) in tabs {
                let frame = watching(theme.clone(), &mut minter, device, &tabbed)
                    .map_err(|why| format!("{locale} {theme_name} {state}: {why}"))?;
                let path = output.join(format!("{locale}-{theme_name}-{state}.png"));
                frame.render_png(&path);
                println!("{}", path.display());
            }
        }
    }
    ui::i18n::I18nManager::set_locale("en-US");
    Ok(())
}

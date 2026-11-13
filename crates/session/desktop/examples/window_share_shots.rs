//! Pictures of sharing a window with its workspace.
//!
//! One desk with a window this device maps, its menu open on the row that shares it with the
//! workspace (and, once the session reads it as shared, the row that stops). Captured in both
//! themes and both locales, through the production frame path ([`ui::headless::FrameHarness`]) and
//! the menu a host without a session mounts over its desk.
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

use tilo_desktop_model::{IdMinter, Tab, TileContent, WindowWidth, Workspace};
use tilo_desktop_surface::surface::window_menu::popup::WindowMenuPopup;
use tilo_desktop_surface::{DesktopSurface, TileSource, WorkspaceShare, seed_tree};
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::state::State;
use ui::taffy::{Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::widget::{
    DrawChild as _, EventCtx, TaffyTreeExt as _, TileContent as Shown, TileHost, Widget,
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
    fn window_record(&self, tab: tilo_desktop_model::TabId) -> Option<(String, String)> {
        (tilo_desktop_model::peer_salt_of(tab) == 7)
            .then(|| ("notes-record".to_owned(), "notes".to_owned()))
    }

    fn draws_embeds(&self) -> bool {
        true
    }
}

/// The desk under the menu a host without a session mounts over it.
struct Stage {
    desk: DesktopSurface,
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
        let desk = self.desk.build_layout(taffy, theme, map);
        let menu = self.menu.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[desk, menu], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap_or_default();
        if let Some(&desk) = children.first() {
            self.desk.apply_layout(taffy, desk, inner);
        }
        if let Some(&menu) = children.get(1) {
            self.menu.apply_layout(taffy, menu, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.desk
            .draw_child(scene, transform, text, theme, presence);
        self.menu
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let (Some(&desk), Some(&menu)) = (children.first(), children.get(1)) else {
            return;
        };
        if self.menu.is_open() {
            self.menu
                .handle_event(event, &mut ctx.reborrow(ctx.path, menu));
        } else {
            self.desk
                .handle_event(event, &mut ctx.reborrow(ctx.path, desk));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.desk, &self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.desk, &mut self.menu]
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

/// A desk showing `tabs` , whose session reads every window it can share as `share` .
fn desk(theme: Theme, viewport: (u32, u32), tabs: Vec<Tab>, share: WorkspaceShare) -> FrameHarness {
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
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Windows));
    handle.set_viewport_width(f64::from(viewport.0));
    handle.reads_workspace_shares(Rc::new(move |_, _| Some(share)));
    let stage = Stage {
        desk: surface,
        menu: WindowMenuPopup::new(handle),
        rect: Rect::ZERO,
    };
    let mut frame = FrameHarness::for_root(State::new(), stage, viewport, theme);
    settle(&mut frame);
    frame
}

/// The passes a change takes to reach the frame, over the time an entrance takes to come to
/// rest: a menu is captured as it stands, not as it arrives.
fn settle(frame: &mut FrameHarness) {
    for _ in 0..12 {
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the desk");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    frame.request_layout();
    frame.poll_frame().expect("a frame draws the desk");
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
    for y in (10..=70).step_by(4) {
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
    tilo_desktop_surface::i18n::ensure();
    for locale in ["en-US", "de-DE"] {
        ui::i18n::I18nManager::set_locale(locale);
        for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let states = [
                ("share", WorkspaceShare::default()),
                (
                    "shared",
                    WorkspaceShare {
                        shared: true,
                        viewers: 1,
                    },
                ),
            ];
            for (state, share) in states {
                let row = if share.shared {
                    tilo_desktop_surface::i18n::t_args(
                        "window-menu-stop-sharing-watched",
                        &[("viewers", 1.into())],
                    )
                } else {
                    tilo_desktop_surface::i18n::t("window-menu-share-with-workspace")
                };
                let mut frame = desk(theme.clone(), (1280, 800), vec![mapped(&mut minter)], share);
                if !open_menu(&mut frame, &row) {
                    return Err(format!("the window's menu does not offer “{row}”").into());
                }
                let path = output.join(format!("{locale}-{theme_name}-{state}.png"));
                frame.render_png(&path);
                println!("{}", path.display());
            }
        }
    }
    ui::i18n::I18nManager::set_locale("en-US");
    Ok(())
}

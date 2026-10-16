//! The top bar's Tilo mark, its menu and the About sheet, as a browser
//! desktop shows them on a room's workspace: the bar with the mark and the
//! host pill, the menu open under the mark, and About open, each in dark,
//! light and on a narrow (responsive) bar.
//!
//! Run:  `cargo run -p tilo-desktop --example tilo_menu_shots -- [OUT_DIR]`
//! Out:  `OUT_DIR` (default `target/look/tilo-menu`):
//!       `<bar|menu|about>-<dark|light|narrow>.png`

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture run reports where it wrote and treats a failed setup as fatal"
)]

use std::collections::HashMap;
use std::path::PathBuf;

use taffy::{Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};
use ui::State;
use ui::headless::FrameHarness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Duration;
use ui::widget::{TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

/// The bar over an empty desktop with the Tilo menu over both, as the
/// browser shell stacks them.
struct Stage {
    bar: TopBarChrome,
    menu: TiloMenu,
    rect: Rect,
}

impl Widget for Stage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let bar = self.bar.build_layout(taffy, theme, map);
        let menu = self.menu.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[bar, menu], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [bar, menu] = taffy.children(node).unwrap_or_default()[..] {
            self.bar.apply_layout(taffy, bar, inner);
            self.menu.apply_layout(taffy, menu, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The menu and the sheet are overlays the frame draws above this.
        self.bar.draw(scene, transform, text, theme, presence);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.bar, &self.menu]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.bar, &mut self.menu]
    }

    fn name(&self) -> &'static str {
        "TiloMenuStage"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn stage() -> Stage {
    let workspace = Workspace::new(WorkspaceId(1), "Commons");
    let tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace, Workspace::new(WorkspaceId(2), "Home")],
        ..DesktopTree::default()
    };
    let mut bar = TopBarChrome::from_tree(&tree).with_tilo_mark();
    bar.set_host_label(Some("example.org".into()));
    let links = AboutLinks::new(
        "https://example.org/notify",
        "https://example.org/source",
        "https://example.org/community",
    );
    Stage {
        bar,
        menu: TiloMenu::new(AboutVariant::Browser, &links),
        rect: Rect::ZERO,
    }
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/look/tilo-menu"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("output directory");
    let settle = Duration::from_secs(2);
    for (variant, theme, viewport) in [
        ("dark", Theme::dark(), (1280, 640)),
        ("light", Theme::light(), (1280, 640)),
        ("narrow", Theme::dark(), (360, 640)),
    ] {
        let mut harness = FrameHarness::for_root(State::new(), stage(), viewport, theme);
        harness.settle_for_capture(settle);
        let path = out.join(format!("bar-{variant}.png"));
        harness.render_png(&path);
        println!("{}", path.display());

        let stage = harness.root_mut::<Stage>();
        let (anchor, area) = (stage.bar.tilo_mark_rect(), stage.rect);
        stage.menu.toggle(anchor, area);
        stage.bar.set_tilo_menu_open(true);
        harness.settle_for_capture(settle);
        let path = out.join(format!("menu-{variant}.png"));
        harness.render_png(&path);
        println!("{}", path.display());

        let stage = harness.root_mut::<Stage>();
        stage.menu.show_about();
        stage.bar.set_tilo_menu_open(false);
        harness.settle_for_capture(settle);
        let path = out.join(format!("about-{variant}.png"));
        harness.render_png(&path);
        println!("{}", path.display());
    }
}

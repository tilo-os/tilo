//! Pictures of the desktop's control hints over real windows.
//!
//! The shell under its bar as a page stacks them (the windows' layer, then the chrome with its
//! hints), at a desktop's size, with windows that show text and a field where everyday apps do:
//! Notes and Chat side by side with Chat in front, its edge at the screen's leaving nothing past
//! it, and a lone window. Each in both themes. The program fails when a hint's pill is not small,
//! or covers a window's text, controls or content.
//!
//! Its one argument is the directory the pictures are written to.

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture binary reports the files it wrote"
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::control_hints::{HINTS, HintsFeed};
use tilo_desktop::desktop_host::{BackgroundLayer, DesktopChrome, PermissiveRules};
use tilo_desktop::launchpad::{CollaborationLaunchpad, LaunchpadAction};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{
    Layout, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use ui::control_hints::Learned;
use ui::headless::FrameHarness;
use ui::kurbo::{Affine, Point, Rect};
use ui::presence::PresenceCtx;
use ui::state::State;
use ui::taffy::{Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use ui::theme::Theme;
use ui::vello::Scene;
use ui::widget::{AppModel, DrawChild as _, ModelPath, TaffyTreeExt as _, Widget};

#[path = "../tests/hint_app.rs"]
mod hint_app;

/// The page's stack: the windows' layer under the chrome that holds the bar and the hints.
struct Stage {
    under: BackgroundLayer,
    top: DesktopChrome,
    rect: Rect,
}

impl Widget for Stage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let under = self.under.build_layout(taffy, theme, map);
        let top = self.top.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[under, top], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap_or_default();
        if let (Some(&under), Some(&top)) = (children.first(), children.get(1)) {
            self.under.apply_layout(taffy, under, inner);
            self.top.apply_layout(taffy, top, inner);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let top = self.top.update(state, path);
        let under = self.under.update(state, path);
        top || under
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.under
            .draw_child(scene, transform, text, theme, presence);
        self.top.draw_child(scene, transform, text, theme, presence);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.under, &self.top]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.under, &mut self.top]
    }

    fn name(&self) -> &'static str {
        "ControlHintStage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn window(id: u64, title: &str, app: &str, width: WindowWidth) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: id,
        tabs: vec![Tab {
            id,
            title: title.into(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        width,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A strip holding `windows`, the window `front` in front.
fn tree(windows: Vec<WindowGroup>, front: u64) -> Layout {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: windows,
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), front);
    tree
}

/// What someone learned: every hint but `unlearned`.
fn learning_all_but(unlearned: &str) -> Learned {
    let mut learned = Learned::default();
    for spec in HINTS.iter().filter(|spec| spec.id != unlearned) {
        learned.learn(spec.id);
    }
    learned
}

fn stage(tree: &Layout, feed: &HintsFeed) -> (Stage, DesktopHandle) {
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(hint_app::SketchTiles));
    let under = BackgroundLayer::new(
        Box::new(surface),
        handle.clone(),
        CollaborationLaunchpad::new(),
        Rc::new(|| LaunchpadAction::ALL.into_iter().collect()),
        Rc::default(),
        Vec::new(),
    );
    let mut top = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    top.set_hints_feed(feed.clone());
    (
        Stage {
            under,
            top,
            rect: Rect::ZERO,
        },
        handle,
    )
}

/// Frames until the shell comes to rest.
fn settle(frame: &mut FrameHarness) {
    for _ in 0..40 {
        frame.request_layout();
        frame.poll_frame().expect("a frame draws the shell");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// The tallest a hint's pill may be.
const PILL_HEIGHT_MAX: f64 = 34.0;

/// The hint's pill, as the chrome last drew it, and where the windows show text, controls and
/// content, as the shell last drew them: the program stops when the pill is not small, when it
/// covers any of that, or when no pill shows.
fn check(frame: &mut FrameHarness, handle: &DesktopHandle, what: &str) -> Result<(), String> {
    let card = frame.root_mut::<Stage>().top.hints().card_rect();
    let places = handle.hint_places();
    let ink = places
        .ink
        .clone()
        .ok_or_else(|| format!("{what}: the strip has not read its ink"))?;
    if places.windows_on_strip.is_empty() {
        return Err(format!("{what}: the strip shows no windows"));
    }
    println!("{what}: hint {card:?}");
    let card = card.ok_or_else(|| format!("{what}: no hint shows"))?;
    if card.height() > PILL_HEIGHT_MAX {
        return Err(format!("{what}: the hint is a card, not a pill: {card:?}"));
    }
    if let Some(ink) = ink.iter().find(|ink| ink.overlaps(card)) {
        return Err(format!("{what}: the hint {card:?} covers {ink:?}"));
    }
    for window in &places.windows_on_strip {
        let bar = places
            .bars
            .iter()
            .find(|bar| (bar.x0 - window.x0).abs() < 0.5 && (bar.x1 - window.x1).abs() < 0.5)
            .copied()
            .unwrap_or(*window);
        let body = Rect::new(window.x0, bar.y1.max(window.y0), window.x1, window.y1);
        if !body.is_zero_area() && card.overlaps(body) {
            return Err(format!("{what}: the hint {card:?} stands over {body:?}"));
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("control-hint-shots"), PathBuf::from);
    std::fs::create_dir_all(&output)?;
    ui::i18n::I18nManager::init("en-US");
    tilo_session_core::desktop::i18n::ensure();
    let viewport = (1400, 900);
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        // Two windows side by side, Chat in front at the screen's edge. Notes was closed and comes
        // back: the frame that lays the two out again is the one the hint is placed in.
        let feed = HintsFeed::default();
        feed.set_record(learning_all_but("resize-window"));
        feed.set_context(None, true, Vec::new());
        let chat = || window(5, "Chat", "so.tilo.chat", WindowWidth::Fraction(500));
        let notes = || window(4, "Notes", "so.tilo.notes", WindowWidth::Fraction(500));
        let (stage_root, handle) = stage(&tree(vec![chat()], 5), &feed);
        let mut frame = FrameHarness::for_root(State::new(), stage_root, viewport, theme.clone());
        settle(&mut frame);
        handle.replace_tree(tree(vec![notes(), chat()], 5));
        settle(&mut frame);
        check(&mut frame, &handle, "side by side")?;
        let path = output.join(format!("side-by-side-{theme_name}.png"));
        frame.render_png(&path);
        println!("{}", path.display());

        // A lone window taking its strip: search, which no control on screen opens, comes down
        // from the top of the desktop onto the window's empty ground.
        let feed = HintsFeed::default();
        feed.set_record(learning_all_but("open-from-search"));
        feed.set_context(None, true, vec!["Ctrl".to_owned(), "K".to_owned()]);
        let (stage_root, handle) = stage(
            &tree(
                vec![window(4, "Notes", "so.tilo.notes", WindowWidth::Normal)],
                4,
            ),
            &feed,
        );
        let mut frame = FrameHarness::for_root(State::new(), stage_root, viewport, theme.clone());
        settle(&mut frame);
        // Its moment is a while into the session.
        std::thread::sleep(std::time::Duration::from_secs(18));
        settle(&mut frame);
        check(&mut frame, &handle, "one window")?;
        let path = output.join(format!("one-window-{theme_name}.png"));
        frame.render_png(&path);
        println!("{}", path.display());
    }
    Ok(())
}

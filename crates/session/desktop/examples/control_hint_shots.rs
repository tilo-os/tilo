//! The desktop's control hints as the browser shell stacks them: the bar,
//! the strip's windows, and each hint at its moment, beside where its
//! gesture is done. For each hint: its entrance frame by frame, at rest,
//! its gesture picture through one loop, and the acknowledgement when the
//! gesture is done (the check, the fold, the dot drifting to the avatar).
//! Dark and light, on a 1440x900 page and on a 2560x1440 screen at scale 2.
//!
//! Run:  `cargo run -p tilo-desktop --example control_hint_shots -- [OUT_DIR]`
//! Out:  `OUT_DIR` (default `target/look/control-hints`):
//!       `<theme>-<size>/<hint>-<moment>-<ms>.png`

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    reason = "a capture run reports where it wrote, treats a failed setup as fatal, and steps a clock it owns"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use taffy::{Dimension, Display, FlexDirection, NodeId, Position, Size, Style, TaffyTree};
use tilo_desktop::chrome::{
    ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource, OwnChip,
};
use tilo_desktop::control_hints::{ControlHints, HintFrame};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_model::{DesktopTree, IdMinter, Tab, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::State;
use ui::control_hints::Learned;
use ui::control_hints::performed::Performed;
use ui::headless::FrameHarness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::{TaffyTreeExt as _, TileContent, TileHost, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

const COLOUR: (u8, u8, u8) = (214, 140, 96);

/// Windows that say what they are, standing in for applications.
struct Windows;

impl TileSource for Windows {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

struct Room {
    tree: DesktopTree,
}

impl DesktopChromeSource for Room {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        DesktopChromeSnapshot {
            revision: 1,
            clock: ClockSnapshot {
                unix_seconds: 1_760_000_000,
                clock: "14:32".into(),
                weekday: "Thu".into(),
                date: "16 October".into(),
            },
            own: Some(OwnChip {
                name: "Quinn Davis".into(),
                rgb: COLOUR,
            }),
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        self.tree.clone()
    }

    fn intent(&self, _intent: ChromeIntent) -> bool {
        true
    }
}

/// The backdrop, the bar over the strip, and the hints above both.
struct Stage {
    backdrop: tilo_desktop::backdrop::DesktopBackdrop,
    bar: TopBarChrome,
    surface: DesktopSurface,
    handle: DesktopHandle,
    hints: ControlHints,
    /// The clock the hints read, stepped by the capture.
    now: Instant,
    rect: Rect,
}

impl Stage {
    fn frame(&self) -> HintFrame {
        let avatar = self.bar.own_avatar_rect();
        HintFrame {
            row: self.handle.hint_places(),
            area: self.surface.get_rect(),
            search_keys: ui::keycaps::active().chord_keys(["Ctrl"], "k"),
            avatar: (!avatar.is_zero_area()).then_some(avatar),
            colour: Some(COLOUR),
            quiet: None,
            presented: true,
        }
    }
}

impl Widget for Stage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let backdrop = self.backdrop.build_layout(taffy, theme, map);
        let mut style = taffy.style(backdrop).cloned().unwrap_or_default();
        style.position = Position::Absolute;
        let _ = taffy.set_style(backdrop, style);
        let bar = self.bar.build_layout(taffy, theme, map);
        let area = self.surface.build_layout(taffy, theme, map);
        let mut style = taffy.style(area).cloned().unwrap_or_default();
        style.flex_grow = 1.0;
        style.flex_basis = Dimension::length(0.0);
        style.size.height = Dimension::auto();
        style.min_size.height = Dimension::length(0.0);
        let _ = taffy.set_style(area, style);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[backdrop, bar, area], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [backdrop, bar, area] = taffy.children(node).unwrap_or_default()[..] {
            self.backdrop.apply_layout(taffy, backdrop, inner);
            self.bar.apply_layout(taffy, bar, inner);
            self.surface.apply_layout(taffy, area, inner);
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
        self.backdrop.draw(scene, transform, text, theme, presence);
        ui::widget::draw_child(&mut self.surface, scene, transform, text, theme, presence);
        self.bar.draw(scene, transform, text, theme, presence);
        let frame = self.frame();
        self.hints.update(&frame, self.now);
        self.hints
            .draw(&frame, scene, transform, text, theme, self.now);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.backdrop, &self.bar, &self.surface]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.backdrop, &mut self.bar, &mut self.surface]
    }

    fn name(&self) -> &'static str {
        "ControlHintStage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// One hint's scene: the windows it is shown over, the hints the person
/// already knows (so only this one comes), and what cues it.
struct Moment {
    id: &'static str,
    windows: &'static [&'static str],
    cue: Option<Performed>,
    /// When it comes after the desktop opened.
    at: Duration,
    done_by: Performed,
}

const MOMENTS: &[Moment] = &[
    Moment {
        id: "open-from-search",
        windows: &[],
        cue: None,
        at: Duration::from_secs(17),
        done_by: Performed::OpenedSearch,
    },
    Moment {
        id: "move-window",
        windows: &["Chat", "Notes"],
        cue: Some(Performed::OpenedWindow),
        at: Duration::ZERO,
        done_by: Performed::MovedWindow,
    },
    Moment {
        id: "resize-window",
        windows: &["Chat", "Notes"],
        cue: None,
        at: Duration::ZERO,
        done_by: Performed::ResizedWindow,
    },
    Moment {
        id: "pan-view",
        windows: &["Chat", "Notes", "Files", "Music", "Calendar"],
        cue: None,
        at: Duration::ZERO,
        done_by: Performed::PannedView,
    },
];

fn stage(moment: &Moment) -> Stage {
    let mut minter = IdMinter::new(7);
    let tabs = moment
        .windows
        .iter()
        .map(|title| Tab {
            id: minter.mint(),
            title: (*title).into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.{}", title.to_lowercase()),
                label: (*title).into(),
                note: String::new(),
            },
            app_title: None,
        })
        .collect();
    let tree = seed_tree(&mut minter, "Lounge", tabs);
    let bar = TopBarChrome::from_tree_with_local_user(&tree, "me".into())
        .with_chrome(Rc::new(Room { tree: tree.clone() }))
        .with_tilo_mark();
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Windows));
    // Only this scene's hint is still to learn.
    let mut learned = Learned::default();
    for other in MOMENTS.iter().filter(|other| other.id != moment.id) {
        learned.learn(other.id);
    }
    Stage {
        backdrop: tilo_desktop::backdrop::DesktopBackdrop::new(),
        bar,
        surface,
        handle,
        hints: ControlHints::new(learned),
        now: ui::time::now(),
        rect: Rect::ZERO,
    }
}

fn shoot(harness: &mut FrameHarness, now: Instant, path: &Path) {
    harness.root_mut::<Stage>().now = now;
    harness.relayout();
    harness.render_png(path);
}

fn after(at: Instant, ms: u64) -> Instant {
    at.checked_add(Duration::from_millis(ms))
        .expect("a capture's clock stays in range")
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/look/control-hints"), PathBuf::from);
    for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (size_name, viewport, scale) in [("1440", (1440, 900), 1), ("2560", (1280, 720), 2)] {
            // Every frame for the page in dark; the moments that matter
            // elsewhere.
            let full = theme_name == "dark" && scale == 1;
            ui::offscreen::set_capture_device_scale(scale);
            let dir = out.join(format!("{theme_name}-{size_name}"));
            std::fs::create_dir_all(&dir).expect("output directory");
            for moment in MOMENTS {
                let mut harness =
                    FrameHarness::for_root(State::new(), stage(moment), viewport, theme.clone());
                // The windows open and settle before the hint comes.
                harness.settle_for_capture(Duration::from_secs(2));
                let start = ui::time::now();
                {
                    let stage = harness.root_mut::<Stage>();
                    stage.now = start;
                    if let Some(cue) = moment.cue {
                        stage.hints.observe(cue, start);
                    }
                }
                harness.relayout();
                let shown = start
                    .checked_add(moment.at)
                    .expect("a capture's clock stays in range");
                let id = moment.id;
                let entrance: Vec<u64> = if full {
                    (0..=18).map(|k| k * 33).collect()
                } else {
                    vec![0, 120, 240]
                };
                for ms in entrance {
                    shoot(
                        &mut harness,
                        after(shown, ms),
                        &dir.join(format!("{id}-arrive-{ms:04}.png")),
                    );
                }
                shoot(
                    &mut harness,
                    after(shown, 1400),
                    &dir.join(format!("{id}-rest.png")),
                );
                let loop_steps: Vec<u64> = if full {
                    (0..8).map(|k| 1400 + k * 325).collect()
                } else {
                    Vec::new()
                };
                for ms in loop_steps {
                    shoot(
                        &mut harness,
                        after(shown, ms),
                        &dir.join(format!("{id}-gesture-{ms:04}.png")),
                    );
                }
                let done = after(shown, 4200);
                harness
                    .root_mut::<Stage>()
                    .hints
                    .observe(moment.done_by, done);
                let completion: Vec<u64> = if full {
                    (0..=19).map(|k| k * 100).collect()
                } else {
                    vec![200, 700, 1000, 1300]
                };
                for ms in completion {
                    shoot(
                        &mut harness,
                        after(done, ms),
                        &dir.join(format!("{id}-done-{ms:04}.png")),
                    );
                }
                println!("{}", dir.join(id).display());
            }
        }
    }
    ui::offscreen::set_capture_device_scale(1);
}

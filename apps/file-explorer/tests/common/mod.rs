//! Shared harness for the file-explorer integration tests: it drives the real
//! `FileExplorerWidget` through the host's `sync → build_layout → compute →
//! apply_layout` cycle and dispatches real winit events, so tests exercise the
//! production input + layout paths rather than a mock.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::path::Path;

use file_explorer::FileExplorerWidget;
use taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, MouseButton, WindowEvent};

pub const VIEWPORT: (f32, f32) = (1200.0, 760.0);

/// The locale every test runs under: the catalog's source locale.
pub const TEST_LOCALE: &str = "en-US";

/// Pin this test thread's UI locale. Files otherwise selects its catalog from
/// the host's `LC_ALL` / `LC_MESSAGES` / `LANGUAGE` / `LANG` the first time a
/// thread resolves a string, so assertions on copy would pass or fail by the
/// machine they run on. Call it before the test resolves any string.
pub fn pin_locale() {
    file_explorer::i18n::set_locale(TEST_LOCALE);
}

/// The Files binary with a pinned locale environment, so a spawned run
/// renders the same copy on every host.
pub fn app_command() -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_file-explorer"));
    command
        .env("LC_ALL", "en_US.UTF-8")
        .env("LANG", "en_US.UTF-8")
        .env_remove("LANGUAGE")
        .env_remove("LC_MESSAGES");
    command
}

pub struct Harness {
    pub w: FileExplorerWidget,
    state: ui::State,
    tr: TextRenderer,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    child: NodeId,
}

impl Harness {
    pub fn new(start_dir: &Path) -> Self {
        // No persistence dir / home so the test never touches the real user's
        // favourites or config.
        pin_locale();
        let mut w = FileExplorerWidget::with_persistence(None, None);
        w.navigate(start_dir.to_path_buf());
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let placeholder = taffy.new_leaf(Style::default()).unwrap();
        let mut h = Harness {
            w,
            state: ui::State::new(),
            tr: TextRenderer::new(),
            taffy,
            node_map: HashMap::new(),
            child: placeholder,
        };
        // Take the double-click window out of play. These tests drive two
        // presses around a full relayout, and on a loaded machine that costs
        // more than the real 450 ms — at which point a deliberate double-click
        // reads as two singles and the test fails for a reason it is not
        // about. Nothing here asserts on the window itself.
        ui::gesture::set_multi_click_interval(std::time::Duration::from_secs(60));
        // Two frames: the first lets the responsive `nav` learn its width and
        // settle to its docked (wide) mode; the second is the steady-state
        // geometry the user actually interacts with.
        h.layout();
        h.layout();
        h
    }

    /// Run the host's per-frame `sync → build_layout → compute → apply_layout`
    /// cycle. Calling this between events reproduces the structural rebuild that
    /// discards the previous frame's view instances.
    pub fn layout(&mut self) {
        self.w.sync(&mut self.state.doc, &self.state.root);
        self.taffy = TaffyTree::new();
        self.node_map.clear();
        let child = self
            .w
            .build_layout(&mut self.taffy, &ui::Theme::dark(), &mut self.node_map);
        let (vw, vh) = VIEWPORT;
        let root = self
            .taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(vw),
                        height: Dimension::length(vh),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        let measure_map = self.node_map.clone();
        let tr = &mut self.tr;
        self.taffy
            .compute_layout_with_measure(
                root,
                Size {
                    width: AvailableSpace::Definite(vw),
                    height: AvailableSpace::Definite(vh),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, tr) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        self.w.apply_layout(&self.taffy, child, Point::ORIGIN);
        // `FileView` publishes only the folders intersecting Scrollable's
        // viewport as drop targets.  A production frame always draws after
        // layout; mirror that phase here before tests begin a drag gesture.
        let mut scene = vello::Scene::new();
        self.w.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut self.tr,
            &ui::Theme::dark(),
            &ui::presence::PresenceCtx::empty(),
            &ui::permissions::PermissionContext::permissive(),
        );
        self.child = child;
    }

    pub fn dispatch(&mut self, ev: &WindowEvent) {
        let mut handled = false;
        let mut focus = None;
        let mods = winit::event::Modifiers::default();
        self.w.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut self.state.doc,
                path: &self.state.root,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: self.child,
                taffy: &self.taffy,
            },
        );
    }

    pub fn move_cursor(&mut self, p: Point) {
        self.dispatch(&WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (p.x, p.y).into(),
        });
    }
    pub fn press(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }
    pub fn release(&mut self) {
        self.dispatch(&WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }
}

/// Locate the on-screen centre of displayed row `index` by probing throwaway
/// harnesses: for each candidate y, a single click that selects exactly `index`
/// marks a hit. Returns the centre of the contiguous hit band, robust to the
/// exact header / row metrics. `x` is the middle of the content pane.
pub fn row_center(dir: &Path, index: usize) -> Point {
    let content = Harness::new(dir).w.debug_content_rect();
    let x = content.center().x;
    let mut hits: Vec<f64> = Vec::new();
    let ys = std::iter::successors(Some(content.y0 + 2.0), |&y| Some(y + 3.0))
        .take_while(|&y| y < content.y1);
    for y in ys {
        let mut h = Harness::new(dir);
        h.move_cursor(Point::new(x, y));
        h.press();
        h.release();
        if h.w.selected_indices() == vec![index] && h.w.current_path() == dir {
            hits.push(y);
        } else if !hits.is_empty() {
            break;
        }
    }
    assert!(
        !hits.is_empty(),
        "row {index} was never hit while probing the content pane"
    );
    let mid = hits[hits.len() / 2];
    Point::new(x, mid)
}

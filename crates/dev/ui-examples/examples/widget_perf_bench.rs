//! Headless redraw-cycle benchmark over a representative widget tree.
//!
//! Builds a Column of common widgets (no overlays or percent-height
//! widgets, which don't compose into a bare Column), then runs N synthetic click → redraw cycles on the
//! Day/Week/Month `ToggleGroup`. Each cycle measures the cost of:
//!
//!   - `handle_event` (click + hover)
//!   - `compute_layout_with_measure` (taffy re-flow + measure callbacks)
//!   - `apply_layout` (writes rects into widgets)
//!   - `draw` (widget → vello scene)
//!   - `OffscreenRenderer::render`     (vello → wgpu, includes GPU submit)
//!
//! Output is a per-phase mean / min / max across iterations, plus a
//! sum that approximates the click-to-pixels latency a user feels
//! when they switch tab segments in a long widget gallery.
//!
//! Run with:
//! ```
//! cargo run -p ui-examples --release --example ui-perf -- widget_perf_bench
//! ```
//! Iterations override: `TILO_UI_PERF_ITERS=50`.

use std::collections::HashMap;

use automerge::AutoCommit;
use chrono::NaiveDate;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::i18n::I18nManager;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::toolbar::Orientation as ToolbarOrientation;
use tilo_ui::widget::tree_view::TreeNode;
use tilo_ui::widget::{
    Avatar, Badge, Breadcrumb, ColorPicker, Column, DatePicker, ProgressBar, Row, ScrollAxis,
    Scrollable, Spinner, Text, TimePicker, ToggleGroup, Toolbar, TreeView, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::{Color, ImageData as PenikoImage};
use winit::dpi::PhysicalPosition;
use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

fn placeholder_image() -> PenikoImage {
    let buf = vec![99u8; 4 * 16 * 16];
    tilo_ui::raster::rgba8(buf, 16, 16)
}

fn caption(key: &'static str) -> Box<dyn Widget> {
    Box::new(Text::new(key))
}

/// Three avatars (two initials, one picture) and three badges.
fn avatar_badge_row() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(Avatar::person("Quinn Daley", "quinn.daley")))
            .push(Box::new(Avatar::person("Ada Lovelace", "ada.lovelace")))
            .push(Box::new(
                Avatar::person("Sam Okafor", "sam.okafor").with_image(placeholder_image()),
            ))
            .push(Box::new(Badge::dot()))
            .push(Box::new(Badge::count(7)))
            .push(Box::new(Badge::count(120))),
    )
}

fn build_tree() -> Box<dyn Widget> {
    let tree_root = TreeNode::branch(
        1u64,
        "project",
        (),
        vec![
            TreeNode::branch(
                2u64,
                "src",
                (),
                vec![
                    TreeNode::leaf(3u64, "main.rs", ()),
                    TreeNode::leaf(4u64, "lib.rs", ()),
                ],
            ),
            TreeNode::leaf(5u64, "README.md", ()),
        ],
    );
    let mut tree = TreeView::new(tree_root);
    tree.toggle_expand(1);
    tree.toggle_expand(2);

    Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(caption("title"))
                .push(caption("row_basics"))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(tilo_ui::widget::Button::new("btn", "btn_save")))
                        .push(Box::new(tilo_ui::widget::Toggle::new("tog", "toggle_1")))
                        .push(Box::new(tilo_ui::widget::Checkbox::new("chk", "check_1")))
                        .push(Box::new(tilo_ui::widget::Slider::new("sld", "slider_1")))
                        .push(Box::new(
                            tilo_ui::widget::TextInput::local()
                                .with_initial_value("Edit me")
                                .with_id("txt"),
                        )),
                ))
                .push(caption("row_indicators"))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(ProgressBar::new(0.5)))
                        .push(Box::new(ProgressBar::indeterminate()))
                        .push(Box::new(Spinner::with_size(32.0))),
                ))
                .push(caption("row_avatar_badge"))
                .push(avatar_badge_row())
                .push(caption("row_nav"))
                .push(Box::new(Breadcrumb::new(vec![
                    "home".into(),
                    "docs".into(),
                    "specs".into(),
                    "readme.md".into(),
                ])))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(ToggleGroup::new(
                            vec![
                                Segment::new("Day"),
                                Segment::new("Week"),
                                Segment::new("Month"),
                            ],
                            1,
                        )))
                        .push(Box::new(
                            Toolbar::new(ToolbarOrientation::Horizontal)
                                .push_button("Cut", || {})
                                .push_separator()
                                .push_button("Copy", || {})
                                .push_button("Paste", || {}),
                        )),
                ))
                .push(caption("row_datetime"))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(DatePicker::new(
                            NaiveDate::from_ymd_opt(2026, 5, 22).unwrap_or_default(),
                        )))
                        .push(Box::new(TimePicker::new(14, 30)))
                        .push(Box::new(TimePicker::new(9, 15).with_seconds(45))),
                ))
                .push(caption("row_picker"))
                .push(Box::new(ColorPicker::new(Color::from_rgba8(
                    99, 102, 241, 255,
                ))))
                .push(caption("row_struct"))
                .push(Box::new(tree)),
        )),
    )
}

/// Everything one benchmark frame touches: the widget tree and its layout,
/// the document events mutate, and the render targets.
struct Bench {
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    doc: AutoCommit,
    text_renderer: TextRenderer,
    presence: PresenceCtx,
    theme: Theme,
    scene: Scene,
    renderer: OffscreenRenderer,
    canvas_w: u32,
    canvas_h: u32,
}

impl Bench {
    /// Run one entire frame — layout, apply, draw, render — timing each phase
    /// into `times`.
    fn full_redraw(&mut self, times: &mut Phases) {
        let Self {
            root,
            taffy,
            node_map,
            root_node,
            text_renderer,
            presence,
            theme,
            scene,
            renderer,
            canvas_w,
            canvas_h,
            ..
        } = self;
        let (root_node, canvas_w, canvas_h) = (*root_node, *canvas_w, *canvas_h);
        let t_compute = tilo_ui::time::now();
        // Repeat the App's lambda exactly — taffy borrows the node_map
        // immutably while letting the measure closure mutate the text
        // renderer.
        let tr = &mut *text_renderer;
        #[allow(
            clippy::cast_precision_loss,
            reason = "canvas_w/canvas_h are small fixed pixel dimensions, exact in f32"
        )]
        let viewport = Size {
            width: AvailableSpace::Definite(canvas_w as f32),
            height: AvailableSpace::Definite(canvas_h as f32),
        };
        let layout_result = taffy.compute_layout_with_measure(
            root_node,
            viewport,
            |known, available, node_id, _, _| {
                if let Some(&ptr) = node_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    Size::ZERO
                }
            },
        );
        if let Err(e) = layout_result {
            eprintln!("layout failed: {e}");
            return;
        }
        times.compute_us = t_compute.elapsed().as_micros();

        let t_apply = tilo_ui::time::now();
        root.apply_layout(taffy, root_node, Point::ZERO);
        times.apply_us = t_apply.elapsed().as_micros();

        let t_draw = tilo_ui::time::now();
        scene.reset();
        root.draw(scene, Affine::IDENTITY, text_renderer, theme, presence);
        times.draw_us = t_draw.elapsed().as_micros();

        let t_render = tilo_ui::time::now();
        let _ = renderer.render(scene, theme.tokens.colors.surfaces.base);
        times.render_us = t_render.elapsed().as_micros();
    }
}

#[derive(Default, Clone)]
#[allow(
    clippy::struct_field_names,
    reason = "the shared `_us` postfix states the unit (microseconds), matching every \
              other timing field in this file; stripping it would lose that"
)]
struct Phases {
    handle_us: u128,
    compute_us: u128,
    apply_us: u128,
    draw_us: u128,
    render_us: u128,
}

pub fn run(_args: &[String]) {
    I18nManager::init("en-US");
    I18nManager::add_resource(
        "en-US",
        [
            "title = perf bench",
            "btn_save = Save",
            "row_basics = basics",
            "row_indicators = indicators",
            "row_avatar_badge = avatars",
            "row_nav = nav",
            "row_datetime = datetime",
            "row_picker = picker",
            "row_struct = struct",
        ]
        .join("\n"),
    );

    let iters: usize = tilo_ui::env::var("TILO_UI_PERF_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let (canvas_w, canvas_h) = (1100u32, 800u32);

    let renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    let theme = Theme::dark();
    let text_renderer = TextRenderer::new();
    let presence = PresenceCtx::default();
    let doc = AutoCommit::new();

    let mut root = build_tree();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
    let scene = Scene::new();
    let mut bench = Bench {
        root,
        taffy,
        node_map,
        root_node,
        doc,
        text_renderer,
        presence,
        theme,
        scene,
        renderer,
        canvas_w,
        canvas_h,
    };

    // Warm-up frame: first compute_layout + first GPU pipeline build are
    // outliers we don't want polluting the steady-state numbers.
    let mut warm = Phases::default();
    bench.full_redraw(&mut warm);
    bench.root.sync(&bench.doc, &automerge::ObjId::Root);
    eprintln!(
        "warm-up: compute={}us apply={}us draw={}us render={}us",
        warm.compute_us, warm.apply_us, warm.draw_us, warm.render_us
    );

    // The Day/Week/Month ToggleGroup sits inside Row 4 of the column.
    // Apply_layout has populated its segment_rects. Find a point inside
    // the second segment ("Week") for our synthetic click.
    //
    // We don't have a clean way to reach in and grab the rect, so we
    // hard-code coordinates derived from a known layout — adjust if
    // the canvas size or column structure changes.
    let click_x = 240.0;
    let click_y = 280.0;
    let modifiers = Modifiers::default();
    let device_id = winit::event::DeviceId::dummy();

    let mut hist: Vec<Phases> = Vec::with_capacity(iters);
    for _ in 0..iters {
        let phases = bench.run_one_cycle((click_x, click_y), modifiers, device_id);
        hist.push(phases);
    }

    print_results(&hist, iters);
}

impl Bench {
    /// One click → redraw cycle: a cursor-moved + mouse-press + mouse-release
    /// sequence through `handle_event` (what the App calls in the live window
    /// on real input), then a full `full_redraw`. Split out of `run` to keep
    /// that under the line cap.
    fn run_one_cycle(
        &mut self,
        click: (f64, f64),
        modifiers: Modifiers,
        device_id: DeviceId,
    ) -> Phases {
        let Self {
            root,
            taffy,
            root_node,
            doc,
            ..
        } = self;
        let root_node = *root_node;
        // Drive a cursor-moved + mouse-press + mouse-release cycle so the
        // ToggleGroup latches the click.
        let mut phases = Phases::default();
        let t_handle = tilo_ui::time::now();
        let mut handled = false;
        let mut focus_target: Option<NodeId> = None;
        let (click_x, click_y) = click;
        let move_evt = WindowEvent::CursorMoved {
            device_id,
            position: PhysicalPosition::new(click_x, click_y),
        };
        root.handle_event(
            &move_evt,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: tilo_ui::widget::Edits::lend(doc),
                path: &automerge::ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy,
            },
        );
        handled = false;
        let press_evt = WindowEvent::MouseInput {
            device_id,
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        root.handle_event(
            &press_evt,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: tilo_ui::widget::Edits::lend(doc),
                path: &automerge::ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy,
            },
        );
        handled = false;
        let release_evt = WindowEvent::MouseInput {
            device_id,
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        root.handle_event(
            &release_evt,
            &mut tilo_ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: tilo_ui::widget::Edits::lend(doc),
                path: &automerge::ObjId::Root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: root_node,
                taffy,
            },
        );
        phases.handle_us = t_handle.elapsed().as_micros();

        self.full_redraw(&mut phases);
        phases
    }
}

fn stats<F: Fn(&Phases) -> u128>(hist: &[Phases], f: F) -> (u128, u128, u128) {
    let xs: Vec<u128> = hist.iter().map(&f).collect();
    let min = xs.iter().min().copied().unwrap_or(0);
    let max = xs.iter().max().copied().unwrap_or(0);
    let mean = xs
        .iter()
        .sum::<u128>()
        .checked_div(xs.len().max(1) as u128)
        .unwrap_or(0);
    (min, mean, max)
}

/// Prints the min/mean/max table plus the approximate click-to-pixels
/// latency. Split out of `run` to keep that under the line cap.
fn print_results(hist: &[Phases], iters: usize) {
    let (hmin, hmean, hmax) = stats(hist, |p| p.handle_us);
    let (cmin, cmean, cmax) = stats(hist, |p| p.compute_us);
    let (amin, amean, amax) = stats(hist, |p| p.apply_us);
    let (dmin, dmean, dmax) = stats(hist, |p| p.draw_us);
    let (rmin, rmean, rmax) = stats(hist, |p| p.render_us);
    let total_mean = hmean
        .saturating_add(cmean)
        .saturating_add(amean)
        .saturating_add(dmean)
        .saturating_add(rmean);

    println!();
    println!("Steady-state phase timings ({iters} iterations, microseconds):");
    println!("{:<18} {:>8} {:>8} {:>8}", "phase", "min", "mean", "max");
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "handle_event", hmin, hmean, hmax
    );
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "compute_layout", cmin, cmean, cmax
    );
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "apply_layout", amin, amean, amax
    );
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "draw_to_scene", dmin, dmean, dmax
    );
    println!(
        "{:<18} {:>8} {:>8} {:>8}",
        "vello render", rmin, rmean, rmax
    );
    println!();
    println!(
        "approx click-to-pixels (sum of means): {} us = {} ms",
        total_mean,
        total_mean / 1000
    );
}

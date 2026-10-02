#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Calculator — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default        — run the live winit event loop via `ui::App`.
//!   * `--shots PATH` — render one offscreen frame to a PNG and exit
//!     (headless screenshot; identical passes to the live app).

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "thin")]
extern crate runtime_dylib;

use std::collections::HashMap;

use taffy::prelude::*;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame, ShotLayout};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, MouseButton, WindowEvent};

const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);

ui::android_main!();

/// Calculator binary entry point — see the module doc for the mode matrix.
///
/// # Errors
///
/// Returns `Err` when `--bench-clicks` fails to build/compute a taffy layout
/// frame, or when `--shots` fails to lay out or write the rendered PNG.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    // `--bench-clicks [N]` drives real Calc-keypad button clicks through the
    // production `handle_event` path and times each event + repaint frame,
    // printing per-click the expression length, handle-event µs, frame µs and
    // CRDT document size — so a runaway (freeze) shows up as a climbing number.
    if args
        .iter()
        .any(|a| a == "--bench-clicks" || a.starts_with("--bench-clicks="))
    {
        let n = parse_bench_n(&args).unwrap_or(400);
        return run_clicks_bench(shots_viewport(), n);
    }
    // Shots keep the standalone renderer (seeded fixtures, TILO_CALC_* knobs).
    // The live window runs on the resource/action framework.
    let standalone = args.iter().any(|arg| {
        [
            "--shots",
            "--responsive-matrix",
            "--native-audit",
            "--headless",
        ]
        .iter()
        .any(|mode| arg == *mode || arg.starts_with(&format!("{mode}=")))
    });
    if standalone {
        ui::launch::run(&launch_spec())?;
    } else {
        ui::framework::run::<calculator::CalculatorShell>();
    }
    Ok(())
}

fn configure_app(app: &mut ui::App) {
    calculator::actions::register(app.action_registry_mut());
    #[cfg(not(debug_assertions))]
    {
        app.debug_mode = false;
    }
}

fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or(LaunchError::ShotsUnsupported)?;
    // The live app starts empty; screenshots default to a populated state.
    let root: Box<dyn Widget> = if std::env::var_os("TILO_CALC_EMPTY").is_some() {
        calculator::app_root()
    } else {
        let mut app = calculator::CalculatorApp::seeded();
        match std::env::var("TILO_CALC_MODE").ok().as_deref() {
            Some("2d" | "graph2d") => app.shots_set_mode(calculator::model::Mode::Graph2d),
            Some("3d" | "graph3d") => app.shots_set_mode(calculator::model::Mode::Graph3d),
            _ => {}
        }
        match std::env::var("TILO_CALC_HISTORY").ok().as_deref() {
            Some("open") => app.shots_set_history_open(true),
            Some("closed" | "collapsed") => app.shots_set_history_open(false),
            _ => {}
        }
        Box::new(app)
    };
    // The shared screenshot policy resolves `TILO_SHOTS_THEME`,
    // `TILO_SHOTS_TEXT_SCALE` / `--text-scale` and the motion preference
    // exactly as every other first-party renderer does, so a light-theme
    // request cannot be honoured here and ignored there.
    render_shot_to_png(
        root,
        args.viewport.unwrap_or_else(shots_viewport),
        out,
        ui::launch::shot_theme(args),
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Parse the optional click count after `--bench-clicks`.
fn parse_bench_n(args: &[String]) -> Option<usize> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--bench-clicks" {
            return iter.next().and_then(|s| s.parse().ok());
        }
        if let Some(rest) = arg.strip_prefix("--bench-clicks=") {
            return rest.parse().ok();
        }
    }
    None
}

fn synth_cursor(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}
fn synth_click(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}

/// Rendering context threaded through each bench frame, grouped to keep
/// `bench_frame`'s argument count within the lint limit.
struct RenderCtx<'a> {
    theme: &'a Theme,
    presence: &'a PresenceCtx,
    permission: &'a PermissionContext,
}

/// One full frame pass (sync → build → compute → apply → draw), mirroring
/// `ui::App`. Returns the live taffy tree + root child node and the total
/// frame microseconds. Kept concrete over `CalculatorApp` so the bench can
/// call the app's `bench_*` accessors between frames.
///
/// # Errors
///
/// Returns `Err` if taffy fails to build or compute the layout tree (it
/// can't for this fixed, single-child tree in practice, but the bench
/// propagates rather than panicking on a future regression).
fn bench_frame(
    app: &mut calculator::CalculatorApp,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    ctx: &RenderCtx<'_>,
    w: u32,
    h: u32,
) -> Result<(TaffyTree<()>, NodeId, f64), Box<dyn std::error::Error>> {
    let t0 = ui::time::now();
    app.sync(&mut state.doc, &state.root);

    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = app.build_layout(&mut taffy, ctx.theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;
    {
        let measure_map = node_map.clone();
        let tr = &mut *text_renderer;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        text_renderer,
        ctx.theme,
        ctx.presence,
        ctx.permission,
    );
    let us = t0.elapsed().as_secs_f64() * 1e6;
    Ok((taffy, child, us))
}

/// Move the cursor / press / release through the real `handle_event` path
/// and return the elapsed microseconds.
fn bench_dispatch(
    app: &mut calculator::CalculatorApp,
    state: &mut ui::State,
    taffy: &TaffyTree<()>,
    child: NodeId,
    ev: &WindowEvent,
) -> f64 {
    let mut handled = false;
    let mut focus = None;
    let t = ui::time::now();
    app.handle_event(
        ev,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &winit::event::Modifiers::default(),
            state: &mut state.doc,
            path: &state.root,
            handled: &mut handled,
            focus_target: &mut focus,
            node_id: child,
            taffy,
        },
    );
    t.elapsed().as_secs_f64() * 1e6
}

/// Drive real Calc-keypad clicks through `handle_event` and report per-click
/// cost. This is the reproduction harness for the "freezes after a few button
/// presses" report — headless, no window, identical passes to the live app.
///
/// # Errors
///
/// Returns `Err` if the taffy layout tree fails to build (see
/// [`bench_frame`]), or if the bench somehow fails to switch the app into
/// Calc mode before the click loop starts.
fn run_clicks_bench(viewport: (u32, u32), n: usize) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let ctx = RenderCtx {
        theme: &theme,
        presence: &presence,
        permission: &permission,
    };
    let mut text_renderer = TextRenderer::new();
    let mut state = ui::State::new();
    let mut app = calculator::CalculatorApp::new();

    let layout = bench_switch_to_calc_mode(&mut app, &mut state, &mut text_renderer, &ctx, w, h)?;

    bench_run_click_loop(
        &mut app,
        &mut state,
        &mut text_renderer,
        &ctx,
        (w, h),
        n,
        layout,
    )
}

/// Warm caches, then switch to Calc mode by clicking its header segment (the
/// production path) and settle on a fresh frame in that mode.
fn bench_switch_to_calc_mode(
    app: &mut calculator::CalculatorApp,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    ctx: &RenderCtx<'_>,
    w: u32,
    h: u32,
) -> Result<(TaffyTree<()>, NodeId), Box<dyn std::error::Error>> {
    // Warm caches.
    for _ in 0..3 {
        bench_frame(app, state, text_renderer, ctx, w, h)?;
    }

    // Switch to Calc mode by clicking its header segment (production path).
    let (taffy, child, _) = bench_frame(app, state, text_renderer, ctx, w, h)?;
    let seg = app.bench_seg_center(0);
    bench_dispatch(app, state, &taffy, child, &synth_cursor(seg.x, seg.y));
    bench_dispatch(
        app,
        state,
        &taffy,
        child,
        &synth_click(ElementState::Pressed),
    );
    bench_dispatch(
        app,
        state,
        &taffy,
        child,
        &synth_click(ElementState::Released),
    );
    let (taffy, child, _) = bench_frame(app, state, text_renderer, ctx, w, h)?;

    if !app.bench_mode_is_calc() {
        return Err("bench should have switched to Calc mode".into());
    }
    Ok((taffy, child))
}

/// Drive `n` clicks through a never-ending expression — digits + operators,
/// no `=`, so it grows exactly like the user's "1 2 3 + 4 5 6 * 7 ..."
/// mashing report — and print per-click timings.
fn bench_run_click_loop(
    app: &mut calculator::CalculatorApp,
    state: &mut ui::State,
    text_renderer: &mut TextRenderer,
    ctx: &RenderCtx<'_>,
    viewport: (u32, u32),
    n: usize,
    layout: (TaffyTree<()>, NodeId),
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let (mut taffy, mut child) = layout;
    let cycle = ["1", "2", "3", "+", "4", "5", "6", "×", "7", "8", "9", "−"];

    println!("calculator --bench-clicks @ {w}x{h}  ({n} clicks; µs, compute-only)");
    println!(
        "{:>5}  {:<4} {:>8} {:>10} {:>10} {:>12}",
        "click", "key", "exprlen", "handle_us", "frame_us", "doc_bytes"
    );

    let mut max_handle = 0.0f64;
    let mut max_frame = 0.0f64;
    for idx in 0..n {
        let Some(&label) = cycle.get(idx % cycle.len()) else {
            continue;
        };
        let Some(pt) = app.bench_calc_key_center(label) else {
            return Err(format!("click {idx}: key {label:?} not found in layout").into());
        };
        let handle_us = bench_dispatch(app, state, &taffy, child, &synth_cursor(pt.x, pt.y))
            + bench_dispatch(
                app,
                state,
                &taffy,
                child,
                &synth_click(ElementState::Pressed),
            )
            + bench_dispatch(
                app,
                state,
                &taffy,
                child,
                &synth_click(ElementState::Released),
            );
        let (t2, c2, frame_us) = bench_frame(app, state, text_renderer, ctx, w, h)?;
        taffy = t2;
        child = c2;
        max_handle = max_handle.max(handle_us);
        max_frame = max_frame.max(frame_us);

        // Print every click for the first 20, then sample, plus the doc size.
        if idx < 20 || idx % 20 == 0 || idx == n - 1 {
            let doc_bytes = state.doc.save().len();
            println!(
                "{:>5}  {:<4} {:>8} {:>10.0} {:>10.0} {:>12}",
                idx,
                label,
                app.bench_expr_len(),
                handle_us,
                frame_us,
                doc_bytes
            );
        }
    }
    println!("---");
    println!("max handle_us = {max_handle:.0}   max frame_us = {max_frame:.0}");
    Ok(())
}

/// Viewport for the headless screenshot. Defaults to a wide desktop frame;
/// `TILO_CALC_SHOTS_SIZE=WxH` overrides it (e.g. to capture the compact keypad
/// + overlay-history behaviour at a narrow size).
fn shots_viewport() -> (u32, u32) {
    if let Some(s) = std::env::var_os("TILO_CALC_SHOTS_SIZE") {
        let s = s.to_string_lossy();
        if let Some((w, h)) = s.split_once(['x', 'X', '*'])
            && let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse())
        {
            return (w, h);
        }
    }
    DEFAULT_SHOTS_VIEWPORT
}

/// Run one offscreen frame against `widget` and write RGBA to `out` as PNG.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &std::path::Path,
    theme: Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme)
        .with_unscaled_text()
        .with_layout(ShotLayout::Measured);
    // Shots-only: seed a remote collaborator's cursor at `x,y` so the headless
    // frame shows a peer caret mapped into the active math field's 2D layout.
    if let Some(spec) = std::env::var_os("TILO_CALC_REMOTE_CURSOR") {
        let spec = spec.to_string_lossy();
        if let Some((sx, sy)) = spec.split_once(',')
            && let (Ok(x), Ok(y)) = (sx.trim().parse::<f32>(), sy.trim().parse::<f32>())
        {
            frame
                .presence
                .upsert_remote_cursor("alice".into(), [x, y], Some("Alice"));
        }
    }
    frame.sync(widget.as_mut());
    frame.layout(widget.as_mut())?;
    let scene = frame.draw(widget.as_mut());
    frame.write_png(&scene, out);
    println!("calculator: wrote shot to {}", out.display());
    Ok(())
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new("so.tilo.calculator", calculator::app_root)
        .history(&calculator::framework_history::HISTORY)
        .shot_handler(render_shot)
        .configure_app(configure_app)
}

#[test]
fn production_launch_registers_history() {
    launch_spec().validate_history_declaration().unwrap();
}

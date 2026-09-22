#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Calendar — thin binary entry point. All logic lives in the library.
//!
//! Modes:
//!   * default — run the live winit event loop via `ui::App` (loads the
//!     user's persisted calendar; no sample content).
//!   * `--shots PATH` — render one offscreen frame of the fully-populated
//!     demo build to a PNG and exit. Shot-only flags:
//!       * `--viewport WxH` pick the viewport (drives the responsive layout).
//!       * `--text-scale N` render with an accessibility text scale from
//!         0.75–2.0, including the layout pass (not a post-process zoom).
//!       * `--view V` month | week | day | agenda.
//!       * `--screen S` calendar | detail (which pane to show).
//!       * `--flap` open the overlay sidebar (narrow only).
//!       * `--collapse` collapse the persistent sidebar (wide only).
//!       * `--schedule-conflict` stage a local overlap in the demo editor.
//!       * `--resize-from WxH` lay the SAME instance out at this size first,
//!         then render at `--viewport`. Proves the layout reflows on a live
//!         resize (rather than caching the first frame's breakpoint).
//!   * `--shots-narrow PATH` — legacy alias: renders at the narrow viewport.
//!
//! The offscreen pass is identical to the live app's frame cycle — only the
//! sample seed and viewport differ, which is exactly what drives the layout.

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

use std::collections::HashMap;

use calendar::{CalendarApp, View};
use taffy::prelude::*;
use ui::launch::LaunchSpec;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Comfortably above the responsive breakpoint → full side-by-side layout.
const WIDE_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// Below the breakpoint → narrow single-pane.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 900);

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// Entry point: dispatches to the offscreen `--shots`/`--shots-narrow`
/// renderer, the headless `--headless-drive` driver, or the live windowed
/// event loop.
///
/// # Errors
///
/// Returns an error when a `--shots`/`--shots-narrow` pass fails to lay out
/// or render the offscreen frame (a taffy layout error, or an I/O failure
/// writing the PNG).
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let wide = parse_flag_value(&args, "--shots");
    let narrow = parse_flag_value(&args, "--shots-narrow");
    if wide.is_some() || narrow.is_some() {
        let view =
            parse_flag_value(&args, "--view").and_then(|p| view_from_str(p.to_str().unwrap_or("")));
        let screen =
            parse_flag_value(&args, "--screen").map(|p| p.to_str().unwrap_or("").to_string());
        let flap = args.iter().any(|a| a == "--flap");
        // `--collapse` collapses the persistent sidebar (wide layout) so a shot
        // can prove the three-abreast layout folds down to calendar + detail.
        let collapse = args.iter().any(|a| a == "--collapse");
        // `--empty` is a hermetic first-run state, never the developer's saved
        // calendar. This makes empty/error screenshots meaningful and avoids
        // a prior local draft leaking into visual review.
        let empty = args.iter().any(|a| a == "--empty");
        let sync_state = parse_flag_value(&args, "--sync-state")
            .and_then(|value| value.to_str().map(ToOwned::to_owned));
        let schedule_conflict = args.iter().any(|arg| arg == "--schedule-conflict");
        // Event-form screenshots: `--new-event` opens the New-Event edit form;
        // `--date-popover` / `--calendar-popover` additionally float the date
        // grid / calendar selector over it.
        let new_event =
            args.iter().any(|a| a == "--new-event") || screen.as_deref() == Some("new-event");
        let date_popover = args.iter().any(|a| a == "--date-popover");
        let calendar_popover = args.iter().any(|a| a == "--calendar-popover");
        let task_detail =
            args.iter().any(|a| a == "--task-detail") || screen.as_deref() == Some("task");
        // `--task-picker` opens one direct task-detail chooser for visual
        // review: `due`, `priority`, or `project`.
        let task_picker = parse_flag_value(&args, "--task-picker")
            .and_then(|value| value.to_str().map(ToOwned::to_owned));
        let sheet_scroll_end = args.iter().any(|arg| arg == "--sheet-scroll-end");
        // `--scroll-hour N` pins the week/day timeline scroll to hour N (0 = top
        // of the day, 24 = bottom) so a shot can prove the full 24h scrolls.
        let scroll_hour = parse_flag_value(&args, "--scroll-hour")
            .and_then(|p| p.to_str().and_then(|s| s.trim().parse::<f64>().ok()));
        let build = || {
            if empty {
                let mut app = CalendarApp::new_empty_for_shots();
                if let Some(v) = view {
                    app.shots_set_view(v);
                }
                if let Some(hr) = scroll_hour {
                    app.shots_scroll_timeline(hr);
                }
                if let Some(state) = &sync_state {
                    app.shots_set_sync_state(state);
                }
                return Box::new(app) as Box<dyn Widget>;
            }
            let mut app = CalendarApp::new_demo();
            if let Some(v) = view {
                app.shots_set_view(v);
            }
            if let Some(hr) = scroll_hour {
                app.shots_scroll_timeline(hr);
            }
            if let Some(state) = &sync_state {
                app.shots_set_sync_state(state);
            }
            if schedule_conflict {
                app.shots_show_schedule_conflict();
            }
            if screen.as_deref() == Some("calendar") {
                app.shots_show_calendar();
            }
            if task_detail {
                app.shots_show_task_detail();
            }
            if let Some(picker) = &task_picker {
                app.shots_open_task_picker(picker);
            }
            if flap {
                app.shots_open_flap();
            }
            if collapse {
                app.shots_collapse_sidebar();
            }
            if sheet_scroll_end {
                app.shots_scroll_detail_to_end();
            }
            if date_popover {
                app.shots_open_date_popover();
            } else if calendar_popover {
                app.shots_open_calendar_popover();
            } else if new_event {
                app.shots_new_event();
            }
            Box::new(app) as Box<dyn Widget>
        };
        let resize_from = parse_wxh_flag(&args, "--resize-from");
        let text_scale = parse_text_scale_arg(&args);
        if let Some(out) = wide {
            let vp = parse_viewport(&args).unwrap_or(WIDE_SHOTS_VIEWPORT);
            render_shot_to_png(build(), vp, resize_from, text_scale, &out)?;
        }
        if let Some(out) = narrow {
            let vp = parse_viewport(&args).unwrap_or(NARROW_SHOTS_VIEWPORT);
            render_shot_to_png(build(), vp, resize_from, text_scale, &out)?;
        }
        return Ok(());
    }

    ui::launch::run(
        &LaunchSpec::new_with_args("so.tilo.calendar", live_root)
            .max_positionals(1)
            .prepare_legacy_with_args(prepare_launch),
    )?;
    Ok(())
}

fn prepare_launch(
    args: &ui::launch::LaunchArgs,
) -> Result<ui::launch::LegacyLaunchInputs, ui::launch::LaunchError> {
    let (state, autosave) = calendar::store::open_state("so.tilo.calendar");
    let root = live_root(args)?;
    let mut registry = tilo_actions::ActionRegistry::new();
    calendar::actions::register(&mut registry);
    Ok(ui::launch::LegacyLaunchInputs::new(state, root, autosave).with_registry(registry))
}

fn live_root(args: &ui::launch::LaunchArgs) -> Result<Box<dyn Widget>, ui::launch::LaunchError> {
    let mut app = CalendarApp::new();
    if let Some(target) = args.positionals().first() {
        app.queue_launch_ics(target)
            .map_err(ui::launch::LaunchError::Operation)?;
    }
    Ok(Box::new(app))
}

fn view_from_str(s: &str) -> Option<View> {
    match s.to_ascii_lowercase().as_str() {
        "month" => Some(View::Month),
        "week" => Some(View::Week),
        "day" => Some(View::Day),
        "agenda" => Some(View::Agenda),
        _ => None,
    }
}

/// Parse `--viewport WxH` (e.g. `--viewport 560x720`).
fn parse_viewport(args: &[String]) -> Option<(u32, u32)> {
    parse_wxh_flag(args, "--viewport")
}

/// Parse a `WxH` value for the named flag (e.g. `--resize-from 1280x720`).
fn parse_wxh_flag(args: &[String], flag: &str) -> Option<(u32, u32)> {
    let raw = parse_flag_value(args, flag)?;
    let s = raw.to_str()?;
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Parse a bounded `--text-scale N` accessibility factor. A malformed value
/// deliberately falls back to the system/default scale, matching the live
/// app's resilient preference handling instead of making a visual audit fail.
fn parse_text_scale_arg(args: &[String]) -> Option<f32> {
    let raw = parse_flag_value(args, "--text-scale")?;
    let scale = raw.to_str()?.parse::<f32>().ok()?;
    (scale.is_finite() && (0.75..=2.0).contains(&scale)).then_some(scale)
}

/// Parse `--flag VALUE` or `--flag=VALUE` for the given flag name.
fn parse_flag_value(args: &[String], flag: &str) -> Option<std::path::PathBuf> {
    let eq_prefix = format!("{flag}=");
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix(&eq_prefix) {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one full layout pass (build → compute → apply) for `widget` at the given
/// size. Extracted so a shot can drive several passes — including a
/// resize-simulating pre-pass at a different size — through the identical cycle
/// the live event loop uses.
fn layout_once(
    widget: &mut dyn Widget,
    w: u32,
    h: u32,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
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
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    Ok(())
}

/// Render `widget` offscreen and write RGBA to `out` as PNG.
///
/// Mirrors `ui::App`'s cycle (sync → `build_layout` → `compute_layout` →
/// `apply_layout` → draw) but runs the layout pass **twice**. The live event
/// loop feeds each frame's allocated rect back into the next frame's
/// `build_layout`, which is how responsive widths (here: the detail/notes
/// column) settle. A single offscreen frame would build its layout before the
/// widget knows its own size; the second pass gives it that feedback so the
/// screenshot matches steady state.
fn render_shot_to_png(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    resize_from: Option<(u32, u32)>,
    text_scale: Option<f32>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = shot_theme(
        std::env::var("TILO_SHOTS_THEME").ok().as_deref(),
        text_scale,
        shot_motion_reduced(),
    );
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    // Optional resize simulation: lay the SAME widget out at `resize_from`
    // first, then at the target viewport. A widget that cached its responsive
    // breakpoint from the first frame would render the wide layout even at a
    // narrow target; a correct one re-derives from the target width here.
    if let Some((rw, rh)) = resize_from {
        layout_once(widget.as_mut(), rw, rh, &theme, &mut text_renderer)?;
    }
    for _ in 0..2 {
        layout_once(widget.as_mut(), w, h, &theme, &mut text_renderer)?;
    }

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("calendar: wrote shot to {}", out.display());
    Ok(())
}

/// Resolve Calendar's offscreen theme through the same accessibility choices
/// as its launched app. Keeping it pure makes visual audit flags testable and
/// prevents a PNG from silently claiming a high-contrast or reduced-motion
/// pass that never reached the widget tree.
fn shot_theme(theme_name: Option<&str>, text_scale: Option<f32>, reduce_motion: bool) -> Theme {
    // `TILO_SHOTS_THEME=light|high-contrast` renders the shot under that theme so
    // theme-driven chrome can be proven across palettes; defaults to dark.
    let theme = match theme_name {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let theme = text_scale.map_or_else(
        || theme.clone(),
        |scale| theme.clone().with_text_scale(scale),
    );
    theme.with_reduce_motion(reduce_motion)
}

/// The custom offscreen renderer does not instantiate [`ui::App`], so it must
/// read the same preference names explicitly before rendering a review frame.
fn shot_motion_reduced() -> bool {
    ["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .any(|value| motion_env_value_enabled(&value))
}

fn motion_env_value_enabled(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_text_scale_is_bounded_and_accepts_both_flag_forms() {
        let split = vec!["calendar".into(), "--text-scale".into(), "1.75".into()];
        assert_eq!(parse_text_scale_arg(&split), Some(1.75));
        let equals = vec!["calendar".into(), "--text-scale=2".into()];
        assert_eq!(parse_text_scale_arg(&equals), Some(2.0));
        for value in ["0.74", "2.01", "NaN", "bad"] {
            let args = vec!["calendar".into(), format!("--text-scale={value}")];
            assert_eq!(parse_text_scale_arg(&args), None, "rejected {value}");
        }
    }

    #[test]
    fn offscreen_a11y_theme_inputs_reach_calendar_widgets() {
        let theme = shot_theme(Some("high-contrast"), Some(2.0), true);
        assert_eq!(theme.name, Theme::high_contrast().name);
        assert_eq!(theme.text_scale, 2.0);
        assert!(
            theme.reduce_motion,
            "the reduced-motion review frame must carry the actual preference"
        );
        for value in ["1", "true", " yes "] {
            assert!(motion_env_value_enabled(value), "accepted {value:?}");
        }
        for value in ["", "0", "false", " FALSE "] {
            assert!(!motion_env_value_enabled(value), "rejected {value:?}");
        }
    }
}

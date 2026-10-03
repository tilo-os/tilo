#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Calendar — thin binary entry point. All logic lives in the library.
//!
//! Modes (all through the standard [`ui::launch`] runner):
//!   * default — run the live winit event loop via `ui::App` (loads the
//!     user's persisted calendar; no sample content).
//!   * `--shots PATH [--scenario NAME]` — render one offscreen frame of the
//!     hermetic demo build to a PNG and exit. `--scenario` names one of
//!     [`SHOT_SCENARIOS`] (an unknown name lists them); the legacy shot flags
//!     below refine or override the scenario's state:
//!       * `--viewport WxH` pick the viewport (drives the responsive layout).
//!       * `--text-scale N` render with an accessibility text scale from
//!         0.75–2.0, including the layout pass (not a post-process zoom).
//!       * `--view V` month | week | day | agenda.
//!       * `--screen S` calendar | detail | new-event | task.
//!       * `--flap` open the overlay sidebar (narrow only).
//!       * `--collapse` collapse the persistent sidebar (wide only).
//!       * `--empty` a hermetic first-run calendar instead of the demo.
//!       * `--sync-state S` syncing | offline | conflict | error.
//!       * `--new-event`, `--date-popover`, `--calendar-popover`,
//!         `--task-detail`, `--task-picker due|priority|project`,
//!         `--scroll-hour N`.
//!       * `--schedule-conflict` stage a local overlap in the demo editor.
//!       * `--resize-from WxH` lay the SAME instance out at this size first,
//!         then render at `--viewport`. Proves the layout reflows on a live
//!         resize (rather than caching the first frame's breakpoint).
//!
//!     A malformed shot option (`--view tomorrow`, `--scroll-hour 99`,
//!     `--sync-state lost`) is an **error**, never a silent fallback to the
//!     default state: a review capture that quietly rendered the wrong
//!     screen would pass a visual audit nobody actually looked at.
//!     `--text-scale` and `--viewport` fail the same way inside
//!     [`ui::launch`]. The narrow presentations that `--shots-narrow` once
//!     rendered are the `narrow_drawer` / `narrow_editor` scenarios (or any
//!     `--viewport` below the breakpoint).
//!   * `--responsive-matrix DIR` — the four product viewports.
//!   * `--headless-drive SPEC` — geometry and semantics; a `"scenario"` in
//!     the spec drives the same fixture `--shots --scenario` renders.
//!
//! The offscreen pass is identical to the live app's frame cycle — only the
//! sample seed and viewport differ, which is exactly what drives the layout.
//! Theme and text scale come from [`ui::launch::shot_theme`], so
//! `TILO_SHOTS_THEME` / `TILO_SHOTS_TEXT_SCALE` mean the same here as in every
//! other app.

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

use calendar::{CalendarApp, View};
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame, ShotLayout};
use ui::theme::Theme;
use ui::widget::Widget;

/// Comfortably above the responsive breakpoint → full side-by-side layout.
const WIDE_SHOTS_VIEWPORT: (u32, u32) = (1280, 800);
/// Below the breakpoint → narrow single-pane.
const NARROW_SHOTS_VIEWPORT: (u32, u32) = (720, 900);

/// Every accepted `--scenario`. Each is a primary screen or a mid-interaction
/// state of one, populated (the demo seed) or empty (the hermetic first run):
/// the four views with the editor closed, the event editor and each of its
/// popovers, the task detail and its pickers, the empty and transport states,
/// and the two narrow presentations.
const SHOT_SCENARIOS: &[&str] = &[
    "month",
    "week",
    "day",
    "agenda",
    "event_editor",
    "new_event",
    "date_popover",
    "time_popover",
    "calendar_popover",
    "task_detail",
    "task_due_picker",
    "task_priority_picker",
    "task_project_picker",
    "empty",
    "empty_week",
    "empty_agenda",
    "sync_error",
    "sync_offline",
    "sync_syncing",
    "sync_conflict",
    "collapsed_sidebar",
    "narrow_drawer",
    "narrow_editor",
];

/// Legacy boolean shot flags, still accepted on top of a scenario.
const SHOT_FLAGS: &[&str] = &[
    "--flap",
    "--collapse",
    "--empty",
    "--new-event",
    "--date-popover",
    "--calendar-popover",
    "--task-detail",
    "--schedule-conflict",
    "--sheet-scroll-end",
];

/// Legacy valued shot options, still accepted on top of a scenario.
const SHOT_OPTIONS: &[&str] = &[
    "--view",
    "--screen",
    "--sync-state",
    "--task-picker",
    "--scroll-hour",
    "--resize-from",
];

/// Accepted `--sync-state` values (the transport states the status pill
/// shows); the `sync_*` scenarios name the same four.
const SYNC_STATES: &[&str] = &["syncing", "offline", "conflict", "error"];

/// Accepted `--task-picker` values: the three task-detail choosers.
const TASK_PICKERS: &[&str] = &["due", "priority", "project"];

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// Entry point: the standard lifecycle owns argument parsing, mode validation
/// and the live/headless paths; the screenshot pipeline below only supplies
/// the fixture and the two-pass render.
///
/// # Errors
///
/// Returns an error when a `--shots` pass fails to lay out or render the
/// offscreen frame (a taffy layout error, or an I/O failure writing the
/// PNG), or when the arguments are invalid — including a malformed shot
/// option, which is never silently ignored.
pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    // This spec imports an older calendar file once. The framework shell
    // opens an empty record and would leave that history behind.
    ui::launch::run(&launch_spec())?;
    Ok(())
}

fn launch_spec() -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.calendar", live_root)
        .max_positionals(1)
        .shot_root(shot_root)
        .shot_handler(render_shot)
        .shot_viewport(shot_viewport)
        .allowed_flags(SHOT_FLAGS)
        .allowed_options(SHOT_OPTIONS)
        .scenarios(SHOT_SCENARIOS)
        .configure_app(configure_app)
}

fn configure_app(app: &mut ui::App) {
    calendar::actions::register(app.action_registry_mut());
}

fn live_root(args: &ui::launch::LaunchArgs) -> Result<Box<dyn Widget>, ui::launch::LaunchError> {
    let mut app = CalendarApp::new();
    if let Some(target) = args.positionals().first() {
        app.queue_launch_ics(target)
            .map_err(ui::launch::LaunchError::Operation)?;
    }
    Ok(Box::new(app))
}

/// Which popover a scenario opens over the New-Event form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormPopoverShot {
    Date,
    Time,
    Calendar,
}

/// The fixture one screenshot renders: a scenario's state, refined by the
/// legacy flags. `build` turns it into the widget the harness lays out.
#[derive(Clone, Debug, Default, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent screenshot toggle mirroring one CLI flag"
)]
struct ShotState {
    empty: bool,
    view: Option<View>,
    /// Close the detail editor and show the calendar screen.
    calendar_screen: bool,
    new_event: bool,
    popover: Option<FormPopoverShot>,
    task_detail: bool,
    schedule_conflict: bool,
    sheet_scroll_end: bool,
    task_picker: Option<String>,
    sync_state: Option<String>,
    flap: bool,
    collapse: bool,
    scroll_hour: Option<f64>,
    narrow: bool,
    resize_from: Option<(u32, u32)>,
}

impl ShotState {
    /// The state a named scenario starts from.
    fn scenario(name: &str) -> Option<Self> {
        let mut s = Self::default();
        match name {
            "month" | "week" | "day" | "agenda" => {
                s.view = view_from_str(name);
                s.calendar_screen = true;
            }
            "event_editor" => {}
            "new_event" => s.new_event = true,
            "date_popover" => {
                s.new_event = true;
                s.popover = Some(FormPopoverShot::Date);
            }
            "time_popover" => {
                s.new_event = true;
                s.popover = Some(FormPopoverShot::Time);
            }
            "calendar_popover" => {
                s.new_event = true;
                s.popover = Some(FormPopoverShot::Calendar);
            }
            "task_detail" => s.task_detail = true,
            "task_due_picker" => s.task_picker = Some("due".into()),
            "task_priority_picker" => s.task_picker = Some("priority".into()),
            "task_project_picker" => s.task_picker = Some("project".into()),
            "empty" => s.empty = true,
            "empty_week" => {
                s.empty = true;
                s.view = Some(View::Week);
            }
            "empty_agenda" => {
                s.empty = true;
                s.view = Some(View::Agenda);
            }
            "sync_error" | "sync_offline" | "sync_syncing" | "sync_conflict" => {
                s.calendar_screen = true;
                s.sync_state = name.strip_prefix("sync_").map(str::to_owned);
            }
            "collapsed_sidebar" => {
                s.calendar_screen = true;
                s.collapse = true;
            }
            "narrow_drawer" => {
                s.calendar_screen = true;
                s.flap = true;
                s.narrow = true;
            }
            "narrow_editor" => s.narrow = true,
            _ => return None,
        }
        Some(s)
    }

    /// The scenario (or the plain demo) with every legacy flag applied on
    /// top.
    fn from_args(args: &LaunchArgs) -> Result<Self, LaunchError> {
        Self::resolve(args.scenario.as_deref(), &ShotOptions::from_args(args))
    }

    /// `scenario`'s state (or the plain demo) refined by the raw legacy
    /// options. A malformed option value is an error (see the module docs):
    /// the old renderer fell back to the default state, which made a wrong
    /// capture indistinguishable from a right one.
    fn resolve(scenario: Option<&str>, opts: &ShotOptions<'_>) -> Result<Self, LaunchError> {
        let mut s = match scenario {
            Some(name) => Self::scenario(name).ok_or_else(|| {
                LaunchError::Scenario(format!("unknown screenshot scenario {name:?}"))
            })?,
            None => Self::default(),
        };
        if let Some(view) = opts.view {
            s.view = Some(view_from_str(view).ok_or_else(|| {
                LaunchError::Operation(format!(
                    "unknown --view {view:?}; expected month, week, day or agenda"
                ))
            })?);
        }
        match opts.screen {
            Some("calendar") => s.calendar_screen = true,
            Some("new-event") => s.new_event = true,
            Some("task") => s.task_detail = true,
            Some("detail") | None => {}
            Some(other) => {
                return Err(LaunchError::Operation(format!(
                    "unknown --screen {other:?}; expected calendar, detail, new-event or task"
                )));
            }
        }
        s.flap |= opts.flap;
        s.collapse |= opts.collapse;
        s.empty |= opts.empty;
        s.new_event |= opts.new_event;
        // A popover flag overrides the scenario's popover; with both flags
        // given the date grid wins, as it always has.
        if opts.date_popover {
            s.new_event = true;
            s.popover = Some(FormPopoverShot::Date);
        } else if opts.calendar_popover {
            s.new_event = true;
            s.popover = Some(FormPopoverShot::Calendar);
        }
        s.task_detail |= opts.task_detail;
        s.schedule_conflict |= opts.schedule_conflict;
        s.sheet_scroll_end |= opts.sheet_scroll_end;
        if let Some(picker) = opts.task_picker {
            if !TASK_PICKERS.contains(&picker) {
                return Err(LaunchError::Operation(format!(
                    "unknown --task-picker {picker:?}; expected due, priority or project"
                )));
            }
            s.task_picker = Some(picker.to_owned());
        }
        if let Some(state) = opts.sync_state {
            if !SYNC_STATES.contains(&state) {
                return Err(LaunchError::Operation(format!(
                    "unknown --sync-state {state:?}; expected syncing, offline, conflict or error"
                )));
            }
            s.sync_state = Some(state.to_owned());
        }
        if let Some(hour) = opts.scroll_hour {
            let parsed = hour
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|h| h.is_finite() && (0.0..=24.0).contains(h))
                .ok_or_else(|| {
                    LaunchError::Operation(format!(
                        "invalid --scroll-hour {hour:?}; expected a number from 0 to 24"
                    ))
                })?;
            s.scroll_hour = Some(parsed);
        }
        if let Some(from) = opts.resize_from {
            s.resize_from =
                Some(parse_wxh(from).ok_or_else(|| LaunchError::InvalidViewport(from.to_owned()))?);
        }
        Ok(s)
    }

    fn default_viewport(&self) -> (u32, u32) {
        if self.narrow {
            NARROW_SHOTS_VIEWPORT
        } else {
            WIDE_SHOTS_VIEWPORT
        }
    }

    /// Build the fixture. Sample content exists only in the demo build; the
    /// empty state is a hermetic first run, never the developer's saved
    /// calendar.
    fn build(&self) -> Box<dyn Widget> {
        let mut app = if self.empty {
            CalendarApp::new_empty_for_shots()
        } else {
            CalendarApp::new_demo()
        };
        if let Some(v) = self.view {
            app.shots_set_view(v);
        }
        if let Some(hr) = self.scroll_hour {
            app.shots_scroll_timeline(hr);
        }
        if let Some(state) = &self.sync_state {
            app.shots_set_sync_state(state);
        }
        if self.empty {
            return Box::new(app);
        }
        if self.schedule_conflict {
            app.shots_show_schedule_conflict();
        }
        if self.calendar_screen {
            app.shots_show_calendar();
        }
        if self.task_detail || self.task_picker.is_some() {
            app.shots_show_task_detail();
        }
        if let Some(picker) = &self.task_picker {
            app.shots_open_task_picker(picker);
        }
        if self.flap {
            app.shots_open_flap();
        }
        if self.collapse {
            app.shots_collapse_sidebar();
        }
        if self.sheet_scroll_end {
            app.shots_scroll_detail_to_end();
        }
        match self.popover {
            Some(FormPopoverShot::Date) => app.shots_open_date_popover(),
            Some(FormPopoverShot::Time) => app.shots_open_time_popover(),
            Some(FormPopoverShot::Calendar) => app.shots_open_calendar_popover(),
            None if self.new_event => app.shots_new_event(),
            None => {}
        }
        Box::new(app)
    }
}

/// The legacy shot flags and options as the command line carried them,
/// before validation — one value per entry of [`SHOT_FLAGS`] and
/// [`SHOT_OPTIONS`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per independent legacy CLI flag, exactly as parsed"
)]
struct ShotOptions<'a> {
    view: Option<&'a str>,
    screen: Option<&'a str>,
    flap: bool,
    collapse: bool,
    empty: bool,
    new_event: bool,
    date_popover: bool,
    calendar_popover: bool,
    task_detail: bool,
    schedule_conflict: bool,
    sheet_scroll_end: bool,
    task_picker: Option<&'a str>,
    sync_state: Option<&'a str>,
    scroll_hour: Option<&'a str>,
    resize_from: Option<&'a str>,
}

impl<'a> ShotOptions<'a> {
    fn from_args(args: &'a LaunchArgs) -> Self {
        Self {
            view: args.value("--view"),
            screen: args.value("--screen"),
            flap: args.has("--flap"),
            collapse: args.has("--collapse"),
            empty: args.has("--empty"),
            new_event: args.has("--new-event"),
            date_popover: args.has("--date-popover"),
            calendar_popover: args.has("--calendar-popover"),
            task_detail: args.has("--task-detail"),
            schedule_conflict: args.has("--schedule-conflict"),
            sheet_scroll_end: args.has("--sheet-scroll-end"),
            task_picker: args.value("--task-picker"),
            sync_state: args.value("--sync-state"),
            scroll_hour: args.value("--scroll-hour"),
            resize_from: args.value("--resize-from"),
        }
    }
}

/// The screenshot fixture for `--headless-drive --scenario`.
fn shot_root(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    Ok(ShotState::from_args(args)?.build())
}

fn shot_viewport(args: &LaunchArgs) -> (u32, u32) {
    ShotState::from_args(args).map_or(WIDE_SHOTS_VIEWPORT, |s| s.default_viewport())
}

/// `--shots PATH`: the app-specific two-pass render under the standard
/// screenshot theme policy.
fn render_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let out = args.shots.as_deref().ok_or_else(|| {
        LaunchError::Operation("Calendar screenshot handler called without --shots".into())
    })?;
    let state = ShotState::from_args(args)?;
    let viewport = args.viewport.unwrap_or_else(|| state.default_viewport());
    render_shot_to_png(
        state.build(),
        viewport,
        state.resize_from,
        ui::launch::shot_theme(args),
        out,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
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

/// Parse a `WxH` value (e.g. `1280x720`).
fn parse_wxh(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
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
    theme: Theme,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme).with_layout(ShotLayout::Measured);
    frame.sync(widget.as_mut());

    // Optional resize simulation: lay the SAME widget out at `resize_from`
    // first, then at the target viewport. A widget that cached its responsive
    // breakpoint from the first frame would render the wide layout even at a
    // narrow target; a correct one re-derives from the target width here.
    if let Some(from) = resize_from {
        frame.layout_at(widget.as_mut(), from)?;
    }
    for _ in 0..2 {
        frame.layout(widget.as_mut())?;
    }

    let scene = frame.draw(widget.as_mut());
    frame.write_png(&scene, out);
    println!("calendar: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_scenarios_and_popover_flags_resolve_to_their_states() {
        // Case `every_declared_scenario_resolves_to_a_state`.
        {
            for name in SHOT_SCENARIOS {
                assert!(ShotState::scenario(name).is_some(), "{name} has no state");
            }
            assert!(ShotState::scenario("nope").is_none());
        }
        // Case `scenarios_cover_every_view_the_editor_and_the_empty_and_error_states`.
        {
            let month = ShotState::scenario("month").unwrap();
            assert_eq!(month.view, Some(View::Month));
            assert!(month.calendar_screen, "the view scenarios close the editor");
            let popover = ShotState::scenario("calendar_popover").unwrap();
            assert!(popover.new_event);
            assert_eq!(popover.popover, Some(FormPopoverShot::Calendar));
            let empty = ShotState::scenario("empty_week").unwrap();
            assert!(empty.empty);
            assert_eq!(empty.view, Some(View::Week));
            let error = ShotState::scenario("sync_error").unwrap();
            assert_eq!(error.sync_state.as_deref(), Some("error"));
            assert_eq!(
                ShotState::scenario("narrow_drawer")
                    .unwrap()
                    .default_viewport(),
                NARROW_SHOTS_VIEWPORT
            );
        }
        // Case `the_date_grid_wins_when_both_popover_flags_are_given`.
        {
            let both = ShotState::resolve(
                None,
                &ShotOptions {
                    calendar_popover: true,
                    date_popover: true,
                    ..ShotOptions::default()
                },
            )
            .unwrap();
            assert_eq!(both.popover, Some(FormPopoverShot::Date));
            assert!(both.new_event);
            let calendar = ShotState::resolve(
                None,
                &ShotOptions {
                    calendar_popover: true,
                    ..ShotOptions::default()
                },
            )
            .unwrap();
            assert_eq!(calendar.popover, Some(FormPopoverShot::Calendar));
            // A flag overrides the scenario's own popover.
            let overridden = ShotState::resolve(
                Some("time_popover"),
                &ShotOptions {
                    date_popover: true,
                    ..ShotOptions::default()
                },
            )
            .unwrap();
            assert_eq!(overridden.popover, Some(FormPopoverShot::Date));
        }
    }

    #[test]
    fn malformed_shot_options_and_viewport_sizes_are_rejected() {
        // Case `malformed_shot_options_are_errors_not_silent_fallbacks`.
        {
            let d = ShotOptions::default;
            let bad = [
                ShotOptions {
                    view: Some("tomorrow"),
                    ..d()
                },
                ShotOptions {
                    screen: Some("settings"),
                    ..d()
                },
                ShotOptions {
                    sync_state: Some("lost"),
                    ..d()
                },
                ShotOptions {
                    task_picker: Some("colour"),
                    ..d()
                },
                ShotOptions {
                    scroll_hour: Some("99"),
                    ..d()
                },
                ShotOptions {
                    scroll_hour: Some("noon"),
                    ..d()
                },
                ShotOptions {
                    resize_from: Some("wide"),
                    ..d()
                },
            ];
            for opts in bad {
                assert!(
                    ShotState::resolve(None, &opts).is_err(),
                    "{opts:?} was accepted"
                );
            }
            assert!(ShotState::resolve(Some("nope"), &d()).is_err());
            let good = ShotState::resolve(
                None,
                &ShotOptions {
                    view: Some("week"),
                    sync_state: Some("offline"),
                    task_picker: Some("priority"),
                    scroll_hour: Some("9"),
                    resize_from: Some("390x844"),
                    ..d()
                },
            )
            .unwrap();
            assert_eq!(good.view, Some(View::Week));
            assert_eq!(good.sync_state.as_deref(), Some("offline"));
            assert_eq!(good.task_picker.as_deref(), Some("priority"));
            assert_eq!(good.scroll_hour, Some(9.0));
            assert_eq!(good.resize_from, Some((390, 844)));
        }
        // Case `wxh_parses_both_separators_and_rejects_garbage`.
        {
            assert_eq!(parse_wxh("1280x720"), Some((1280, 720)));
            assert_eq!(parse_wxh("390X844"), Some((390, 844)));
            assert_eq!(parse_wxh("wide"), None);
        }
    }
}

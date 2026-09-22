// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
// Thin binary entry point — all logic lives in the library.
//
// Supports two modes:
//   * default — build a `TerminalWidget`, hand it to `ui::App`, run
//     the live event loop (winit window).
//   * `--shots <PATH>` — render the terminal at a default size into a
//     PNG at `<PATH>` and exit. No window, no event loop. Useful for
//     headless screenshot tests and CI snapshots of the cold-start UI.
//
// The `--shots` flow mirrors the same layout/draw passes the live
// `App::run` does, just funnels into `OffscreenRenderer::render_png`
// instead of a wgpu surface.
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
use std::fmt::Write as _;

use taffy::prelude::*;
use terminal::TerminalWidget;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`. Big enough to show a few
/// rows of terminal output at the toolkit's default font size; small
/// enough that the readback buffer stays under a few MB.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

/// # Errors
///
/// Returns an error when the terminal widget fails to spawn (no PTY, no
/// shell), when building the offscreen render tree for `--shots` fails, or
/// when the live event loop's own setup fails.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

pub fn main() {
    ui::launch::run_or_exit(
        &LaunchSpec::new_with_args("so.tilo.terminal", live_root)
            .live_handler(run_live)
            .shot_handler(run_shot)
            .shot_viewport(|_| DEFAULT_SHOTS_VIEWPORT)
            .allowed_options(&["--size"])
            .scenarios(&[
                "shell",
                "shell-select",
                "colors",
                "tui",
                "scrollback",
                "find",
                "empty",
                "empty-menu",
                "ended",
            ]),
    );
}

fn live_root(_args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    TerminalWidget::spawn_default()
        .map(|widget| Box::new(widget) as Box<dyn Widget>)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

fn run_live(_args: &LaunchArgs) -> Result<(), LaunchError> {
    let wake_scope = ui::wake::WakeScope::new();
    let _wake_entry = wake_scope.enter();
    let widget = TerminalWidget::spawn_default()
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    // Arm the PTY-reader waker so the event loop idles between output instead of
    // polling at ~60 Hz: grab the handle before moving the widget into the App,
    // then store the loop's proxy once it exists.
    let waker = widget.waker_handle();
    let (state, autosave) = ui::persist::open("so.tilo.terminal");
    let app =
        ui::App::from_scoped_parts(wake_scope, state, Box::new(widget)).with_autosave(autosave);
    app.run_with_setup(move |proxy| {
        if let Ok(mut g) = waker.lock() {
            *g = Some(proxy);
        }
    });
    Ok(())
}

fn parse_size(raw: &str) -> Option<(u32, u32)> {
    let (w, h) = raw.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn run_shot(args: &LaunchArgs) -> Result<(), LaunchError> {
    let scenario = args.scenario.as_deref();
    let mut widget = match scenario {
        Some("shell" | "shell-select") | None => TerminalWidget::spawn_default(),
        Some("ended") => {
            #[cfg(unix)]
            let command = terminal::CommandBuilder::new("/bin/true");
            #[cfg(windows)]
            let command = {
                let mut command = terminal::CommandBuilder::new("cmd.exe");
                command.args(["/d", "/c", "exit /b 0"]);
                command
            };
            TerminalWidget::spawn_command(command)
        }
        Some(_) => TerminalWidget::spawn_headless(),
    }
    .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let viewport = args
        .viewport
        .or_else(|| args.value("--size").and_then(parse_size))
        .unwrap_or(DEFAULT_SHOTS_VIEWPORT);
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| LaunchError::Operation("missing screenshot output".into()))?;
    render_shot_to_png(&mut widget, viewport, out, scenario)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG.
///
/// Mirrors `ui::App`'s layout/draw cycle for a single redraw:
/// `sync` → `build_layout` → `compute_layout` → `apply_layout` → `draw`.
/// Skips the event loop entirely because we render exactly one frame
/// before exiting. When `scenario` is set, canned VT byte sequences are
/// fed through the widget *after* layout (so the grid is sized to the
/// viewport) and then re-synced before the draw.
fn render_shot_to_png(
    widget: &mut TerminalWidget,
    viewport: (u32, u32),
    out: &std::path::Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let mut theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    theme.reduce_motion = matches!(
        std::env::var("TILO_SHOTS_REDUCE_MOTION").ok().as_deref(),
        Some("1" | "true" | "yes")
    );
    if let Some(scale) = std::env::var("TILO_SHOTS_TEXT_SCALE")
        .ok()
        .and_then(|raw| raw.parse::<f32>().ok())
        .filter(|scale| scale.is_finite())
    {
        theme = theme.with_text_scale(scale);
    }
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    // Sync the widget's view-state from its CRDT once.
    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    // Build the layout tree, then wrap it in a viewport-sized root so
    // taffy has a definite extent to size from.
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
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
        let tr = &mut text_renderer;
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

    // Drive the scenario now that the grid is sized to the viewport, then
    // re-sync so the mirrored grid reflects the fed sequences. The `shell`
    // scenario drives the real PTY instead of feeding canned bytes.
    if let Some(name) = scenario {
        if name == "shell" || name == "shell-select" {
            drive_live_shell(widget, &mut state.doc, &state.root)?;
        } else {
            run_scenario(widget, &mut state.doc, &state.root, name);
        }
        widget.sync(&mut state.doc, &state.root);
        // `shell-select` additionally highlights a line of real shell output so
        // the shot demonstrates the selection highlight over a live session.
        if name == "shell-select" {
            apply_demo_selection(widget);
        }
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
    println!("terminal: wrote shot to {}", out.display());
    Ok(())
}

/// Drive the real login shell for the `shell` shot: let the prompt settle,
/// send a short demo command exactly as a keystroke would, and pump `sync` so
/// the live PTY output lands in the captured frame. This exercises the same
/// reader → `sync` → vte → CRDT path the live terminal uses every frame.
fn drive_live_shell(
    widget: &mut TerminalWidget,
    doc: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
) -> Result<(), &'static str> {
    use std::time::Duration;
    // Let the shell reach its first prompt.
    for _ in 0..8 {
        widget.sync(doc, root);
        std::thread::sleep(Duration::from_millis(40));
    }
    // A short demo: identify the shell, print some content, and end with a
    // green "done" so the shot shows real SGR-coloured output.
    #[cfg(unix)]
    widget.write_input(
        b"clear; printf 'Tilo terminal - live PTY session\\n'; \
          printf 'primes below 20: 2 3 5 7 11 13 17 19\\n'; \
          printf '\\033[1;32mdone\\033[0m\\n'\r",
    );
    #[cfg(windows)]
    widget.write_input(
        b"@echo off\r\ncls\r\necho Tilo terminal - live PTY session\r\necho primes below 20: 2 3 5 7 11 13 17 19\r\necho \x1b[1;32mdone\x1b[0m\r\n",
    );
    for _ in 0..150 {
        widget.sync(doc, root);
        if widget.grid_text().lines().any(|line| line.trim() == "done") {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("the live shell did not execute the screenshot command")
}

/// Highlight a line (and a word) of the live shell output so the `shell-select`
/// shot shows the selection highlight painted over a real session. Selects the
/// "primes" demo line if present, otherwise the first non-empty row. Uses the
/// same public selection API a host would.
fn apply_demo_selection(widget: &mut TerminalWidget) {
    use terminal::GridPos;
    let (cols, _) = widget.grid_size();
    // Target the printed OUTPUT line, not the shell's command echo (which also
    // contains the text). `find_row_last` returns the later, clean output row.
    if let Some(row) = widget.find_row_last("primes below 20:") {
        // Whole-line selection (triple-click equivalent).
        widget.set_selection_cells(GridPos { row, col: 0 }, GridPos { row, col: cols });
    } else if let Some(row) = widget.find_row_last("done") {
        widget.set_selection_cells(GridPos { row, col: 0 }, GridPos { row, col: cols });
    }
}

/// Feed a named demo scenario's VT byte sequences through the widget's
/// production `feed` path. Used only by `--shots --scenario`.
fn run_scenario(
    widget: &mut TerminalWidget,
    doc: &mut automerge::AutoCommit,
    root: &automerge::ObjId,
    name: &str,
) {
    match name {
        // A palette of SGR effects: 16 ANSI colors, a 256-cube strip, a
        // truecolor gradient, and the four style bits.
        "colors" => {
            let mut s = String::new();
            s.push_str("\x1b[1mTilo terminal — SGR palette\x1b[0m\r\n\r\n");
            s.push_str(" 16-color ANSI:  ");
            for n in 0..8 {
                let _ = write!(s, "\x1b[4{n}m  \x1b[0m");
            }
            s.push(' ');
            for n in 0..8 {
                let _ = write!(s, "\x1b[48;5;{}m  \x1b[0m", n + 8);
            }
            s.push_str("\r\n\r\n 256-color:      ");
            for n in (16..232).step_by(6) {
                let _ = write!(s, "\x1b[48;5;{n}m \x1b[0m");
            }
            s.push_str("\r\n\r\n truecolor:      ");
            for i in 0..48 {
                let r = (i * 255 / 47) as u8;
                let g = (255 - i * 255 / 47) as u8;
                let _ = write!(s, "\x1b[48;2;{r};{g};128m \x1b[0m");
            }
            s.push_str("\r\n\r\n styles:         ");
            s.push_str("\x1b[1mbold\x1b[0m  \x1b[2mdim\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[9mstrike\x1b[0m  \x1b[7mreverse\x1b[0m\r\n");
            s.push_str("\r\n foreground:     ");
            s.push_str("\x1b[38;2;255;90;90mred \x1b[38;2;90;220;120mgreen \x1b[38;2;110;160;255mblue\x1b[0m\r\n");
            s.push_str("\r\n$ \x1b[38;5;120mecho\x1b[0m \x1b[38;5;223m\"hello, Tilo\"\x1b[0m\r\n");
            widget.feed(doc, root, s.as_bytes());
        }
        // A full-screen alternate-screen redraw resembling a text editor.
        "tui" => {
            let mut s = String::new();
            s.push_str("\x1b[?1049h\x1b[2J\x1b[H");
            // Reverse-video title bar across the top.
            s.push_str("\x1b[1;1H\x1b[7m Tilo EDIT  main.rs  [+]                                      unix  utf-8 \x1b[0m");
            #[allow(
                clippy::literal_string_with_formatting_args,
                reason = "demo source text shown verbatim in the tui shot; the braces are \
                          the sample code's own println! syntax, not a format template here"
            )]
            let lines = [
                "fn main() {",
                "    // alternate-screen redraw via CSI ?1049h",
                "    let greeting = \"hello, Tilo\";",
                "    for i in 0..3 {",
                "        println!(\"{i}: {greeting}\");",
                "    }",
                "}",
            ];
            for (i, line) in lines.iter().enumerate() {
                // Line-number gutter in a dim color, then source text.
                let _ = write!(
                    s,
                    "\x1b[{};1H\x1b[38;5;240m{:>3} \x1b[0m{}",
                    i + 3,
                    i + 1,
                    line
                );
            }
            // A status line near the bottom in reverse video.
            s.push_str("\x1b[23;1H\x1b[7m NORMAL                                              3,17          Top \x1b[0m");
            s.push_str("\x1b[24;1H:wq");
            widget.feed(doc, root, s.as_bytes());
        }
        // Long output, then scrolled back so history is on screen.
        "scrollback" => {
            let mut s = String::new();
            for i in 0..80 {
                let _ = writeln!(
                    s,
                    "\x1b[38;5;250m{i:>4}\x1b[0m  log entry {i:03}: the quick brown fox jumps over the lazy dog\r"
                );
            }
            widget.feed(doc, root, s.as_bytes());
            widget.sync(doc, root);
            // Scroll up so retained history (not the live tail) is visible.
            widget.scroll_up(20);
        }
        // Search state is explicitly local and is shown over real retained
        // scrollback, not a fabricated remote/agent transcript operation.
        "find" => {
            let mut s = String::new();
            for i in 0..80 {
                // A terminal LF preserves the current column. Use the shell's
                // normal CRLF output so this models a readable transcript.
                let _ = writeln!(s, "{i:>4}  build step {i:03}: compiling terminal search\r");
            }
            widget.feed(doc, root, s.as_bytes());
            widget.sync(doc, root);
            widget.open_find();
            widget.set_find_query("build step 042");
        }
        // A command that exits immediately exercises the persistent terminal
        // lifecycle status, including the reader's EOF transition.
        "ended" => std::thread::sleep(std::time::Duration::from_millis(50)),
        // Context-menu rendering must work before the first PTY byte arrives;
        // this is deliberately distinct from a content-filled menu shot.
        "empty-menu" => widget.open_context_menu_at(Point::new(24.0, 24.0)),
        _ => {}
    }
}

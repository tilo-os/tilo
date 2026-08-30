// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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

use std::collections::HashMap;

use taffy::prelude::*;
use terminal::TerminalWidget;
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

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Parse a tiny ad-hoc flag set so we don't pull in `clap` for one
    // boolean-with-string. Anything other than `--shots PATH` is treated
    // as "open a window the same way it always did".
    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let scenario = parse_scenario_arg(&args);
        // Optional `--size WxH` overrides the default viewport so a scenario can
        // be captured at, e.g., a narrow phone-ish width to check the chrome
        // reflows.
        let viewport = parse_size_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        // The `shell` scenario (and the plain cold-start shot) run a real login
        // shell so the frame captures a live session. The canned scenarios
        // (`colors`/`tui`/`scrollback`) drive the grid entirely through `feed`
        // on a null backend to keep the frame deterministic. `empty` captures
        // the first-run watermark (a headless widget that never gets output).
        let mut widget = match scenario.as_deref() {
            Some("shell" | "shell-select") | None => TerminalWidget::spawn_default()?,
            Some(_) => TerminalWidget::spawn_headless()?,
        };
        render_shot_to_png(&mut widget, viewport, &out, scenario.as_deref())?;
        return Ok(());
    }

    let widget = TerminalWidget::spawn_default()?;
    // Arm the PTY-reader waker so the event loop idles between output instead of
    // polling at ~60 Hz: grab the handle before moving the widget into the App,
    // then store the loop's proxy once it exists.
    let waker = widget.waker_handle();
    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(
            spec,
            Box::new(widget),
            "dev.qos.terminal",
        ));
    }

    let (state, autosave) = ui::persist::open("dev.qos.terminal");
    let app = ui::App::new(state, Box::new(widget)).with_autosave(autosave);
    app.run_with_setup(move |proxy| {
        if let Ok(mut g) = waker.lock() {
            *g = Some(proxy);
        }
    });
    Ok(())
}

/// Returns `Some(name)` if `--scenario <name>` (or `--scenario=<name>`)
/// appears in `args`.
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Returns `Some((w, h))` if `--size <W>x<H>` (or `--size=<W>x<H>`) appears in
/// `args` and parses, otherwise `None`.
fn parse_size_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let raw = loop {
        let arg = iter.next()?;
        if arg == "--size" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--size=") {
            break rest.to_string();
        }
    };
    let (w, h) = raw.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
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
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
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
            drive_live_shell(widget, &mut state.doc, &state.root);
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
) {
    use std::time::Duration;
    // Let the shell reach its first prompt.
    for _ in 0..8 {
        widget.sync(doc, root);
        std::thread::sleep(Duration::from_millis(40));
    }
    // A short demo: identify the shell, print some content, and end with a
    // green "done" so the shot shows real SGR-coloured output.
    widget.write_input(
        b"printf 'qOS terminal (real PTY) - shell: %s\\n' \"$0\"; \
          printf 'primes below 20: 2 3 5 7 11 13 17 19\\n'; \
          printf '\\033[1;32mdone\\033[0m\\n'\n",
    );
    for _ in 0..30 {
        widget.sync(doc, root);
        std::thread::sleep(Duration::from_millis(50));
    }
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
            s.push_str("\x1b[1mqOS terminal — SGR palette\x1b[0m\r\n\r\n");
            s.push_str(" 16-color ANSI:  ");
            for n in 0..8 {
                s.push_str(&format!("\x1b[4{n}m  \x1b[0m"));
            }
            s.push(' ');
            for n in 0..8 {
                s.push_str(&format!("\x1b[48;5;{}m  \x1b[0m", n + 8));
            }
            s.push_str("\r\n\r\n 256-color:      ");
            for n in (16..232).step_by(6) {
                s.push_str(&format!("\x1b[48;5;{n}m \x1b[0m"));
            }
            s.push_str("\r\n\r\n truecolor:      ");
            for i in 0..48 {
                let r = (i * 255 / 47) as u8;
                let g = (255 - i * 255 / 47) as u8;
                s.push_str(&format!("\x1b[48;2;{r};{g};128m \x1b[0m"));
            }
            s.push_str("\r\n\r\n styles:         ");
            s.push_str("\x1b[1mbold\x1b[0m  \x1b[2mdim\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[4munderline\x1b[0m  \x1b[9mstrike\x1b[0m  \x1b[7mreverse\x1b[0m\r\n");
            s.push_str("\r\n foreground:     ");
            s.push_str("\x1b[38;2;255;90;90mred \x1b[38;2;90;220;120mgreen \x1b[38;2;110;160;255mblue\x1b[0m\r\n");
            s.push_str("\r\n$ \x1b[38;5;120mecho\x1b[0m \x1b[38;5;223m\"hello, qOS\"\x1b[0m\r\n");
            widget.feed(doc, root, s.as_bytes());
        }
        // A full-screen alternate-screen redraw resembling a text editor.
        "tui" => {
            let mut s = String::new();
            s.push_str("\x1b[?1049h\x1b[2J\x1b[H");
            // Reverse-video title bar across the top.
            s.push_str("\x1b[1;1H\x1b[7m qOS EDIT  main.rs  [+]                                      unix  utf-8 \x1b[0m");
            let lines = [
                "fn main() {",
                "    // alternate-screen redraw via CSI ?1049h",
                "    let greeting = \"hello, qOS\";",
                "    for i in 0..3 {",
                "        println!(\"{i}: {greeting}\");",
                "    }",
                "}",
            ];
            for (i, line) in lines.iter().enumerate() {
                // Line-number gutter in a dim color, then source text.
                s.push_str(&format!(
                    "\x1b[{};1H\x1b[38;5;240m{:>3} \x1b[0m{}",
                    i + 3,
                    i + 1,
                    line
                ));
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
                s.push_str(&format!("\x1b[38;5;250m{i:>4}\x1b[0m  log entry {i:03}: the quick brown fox jumps over the lazy dog\r\n"));
            }
            widget.feed(doc, root, s.as_bytes());
            widget.sync(doc, root);
            // Scroll up so retained history (not the live tail) is visible.
            widget.scroll_up(20);
        }
        _ => {}
    }
}

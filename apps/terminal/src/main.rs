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

use std::fmt::Write as _;

use terminal::TerminalWidget;
use ui::launch::{LaunchArgs, LaunchError, LaunchSpec, ShotFrame};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;

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
    // The live handler spawns the login shell and wakes the window when the
    // PTY writes. The ResourceApp shell has no such waker, so output would
    // sit until some other event.
    ui::launch::run_or_exit(&launch_spec());
}

fn launch_spec_with_root(
    root: fn(&LaunchArgs) -> Result<Box<dyn Widget>, LaunchError>,
) -> LaunchSpec {
    LaunchSpec::new_with_args("so.tilo.terminal", root)
        .history(&terminal::framework_history::HISTORY)
        .configure_app(|app| terminal::actions::register(app.action_registry_mut()))
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
        ])
}

fn launch_spec() -> LaunchSpec {
    launch_spec_with_root(live_root)
}

fn live_root(_args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
    TerminalWidget::spawn_default()
        .map(|widget| Box::new(widget) as Box<dyn Widget>)
        .map_err(|error| LaunchError::Operation(error.to_string()))
}

#[cfg(test)]
fn build_prepared_terminal_app(
    spec: &LaunchSpec,
    args: &LaunchArgs,
    wake_scope: ui::WakeScope,
    root: Box<dyn Widget>,
    build: impl FnOnce(
        &LaunchSpec,
        &LaunchArgs,
        ui::WakeScope,
        Box<dyn Widget>,
    ) -> Result<ui::App, LaunchError>,
) -> Result<ui::App, LaunchError> {
    build(spec, args, wake_scope, root)
}

fn run_live(args: &LaunchArgs) -> Result<(), LaunchError> {
    let spec = launch_spec();
    let prepared = spec.prepare_native(args)?;
    let mut waker = None;
    let app = prepared.build_root(|| {
        let widget = TerminalWidget::spawn_default()
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        waker = Some(widget.waker_handle());
        Ok(Box::new(widget))
    })?;
    let waker = waker.ok_or_else(|| {
        LaunchError::Operation("terminal factory did not retain its PTY waker".into())
    })?;
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
    // A plain `--shots` is the live shell, which is the app's main screen.
    let scenario = Some(args.scenario.as_deref().unwrap_or("shell"));
    let mut widget = match scenario {
        Some("shell" | "shell-select") | None => TerminalWidget::spawn_default(),
        Some("ended") => {
            #[cfg(unix)]
            let command = {
                // Spawn the portable shell path: macOS has no /bin/true.
                let mut command = terminal::CommandBuilder::new("/bin/sh");
                command.args(["-c", "exit 0"]);
                command
            };
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
    render_shot_to_png(
        &mut widget,
        viewport,
        ui::launch::shot_theme(args),
        out,
        scenario,
    )
    .map_err(|error| LaunchError::Operation(error.to_string()))
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG.
///
/// When `scenario` is set, canned VT byte sequences are fed through the
/// widget *after* layout (so the grid is sized to the viewport) and then
/// re-synced before the draw.
fn render_shot_to_png(
    widget: &mut TerminalWidget,
    viewport: (u32, u32),
    theme: Theme,
    out: &std::path::Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut frame = ShotFrame::new(viewport, theme);
    frame.sync(widget);
    frame.layout(widget)?;

    // Drive the scenario now that the grid is sized to the viewport, then
    // re-sync so the mirrored grid reflects the fed sequences. The `shell`
    // scenario drives the real PTY instead of feeding canned bytes.
    if let Some(name) = scenario {
        let state = &mut frame.state;
        if name == "shell" || name == "shell-select" {
            drive_live_shell(widget, &mut state.doc, &state.root)?;
        } else {
            run_scenario(widget, &mut state.doc, &state.root, name);
        }
        frame.sync(widget);
        // `shell-select` additionally highlights a line of real shell output so
        // the shot demonstrates the selection highlight over a live session.
        if name == "shell-select" {
            apply_demo_selection(widget);
        }
    }

    let scene = frame.draw(widget);
    frame.write_png(&scene, out);
    println!("terminal: wrote shot to {}", out.display());
    Ok(())
}

/// Drive the real login shell for the `shell` shot: give it a fixed demo
/// prompt, send a few short commands exactly as keystrokes would, and pump
/// `sync` so the live PTY output lands in the captured frame. This exercises
/// the same reader → `sync` → vte → CRDT path the live terminal uses every
/// frame.
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
    // Replace the host's prompt (it differs per machine) with a fixed one and
    // start from a clean screen, so the shot reads the same everywhere.
    #[cfg(unix)]
    {
        widget.write_input(b"PS1='user@tilo ~ $ '; clear\r");
        for _ in 0..10 {
            widget.sync(doc, root);
            std::thread::sleep(Duration::from_millis(40));
        }
        // A short demo: print some content and end with a green "done" so the
        // shot shows real SGR-coloured output.
        widget.write_input(
            b"echo 'Tilo terminal - live PTY session'\r\
              echo 'primes below 20: 2 3 5 7 11 13 17 19'\r\
              tput bold; tput setaf 2; echo done; tput sgr0\r",
        );
    }
    #[cfg(windows)]
    widget.write_input(
        b"@echo off\r\ncls\r\necho Tilo terminal - live PTY session\r\necho primes below 20: 2 3 5 7 11 13 17 19\r\necho \x1b[1;32mdone\x1b[0m\r\n",
    );
    for _ in 0..150 {
        widget.sync(doc, root);
        if widget.grid_text().lines().any(|line| line.trim() == "done") {
            // One more beat so the prompt that follows the last command is in
            // the frame too.
            for _ in 0..5 {
                widget.sync(doc, root);
                std::thread::sleep(Duration::from_millis(40));
            }
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

/// Feed the `tui` scenario: an alternate-screen redraw sized to the grid.
fn feed_tui(widget: &mut TerminalWidget, doc: &mut automerge::AutoCommit, root: &automerge::ObjId) {
    let (cols, rows) = widget.grid_size();
    let mut s = String::new();
    s.push_str("\x1b[?1049h\x1b[2J\x1b[H");
    // Reverse-video title bar across the top.
    let title = format!(
        " {:<width$}",
        "Tilo EDIT  main.rs  [+]",
        width = cols.saturating_sub(1)
    );
    let _ = write!(s, "\x1b[1;1H\x1b[7m{title}\x1b[0m");
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "demo source text shown verbatim in the tui shot; the braces are \
                      the sample code's own println! syntax, not a format template here"
    )]
    let lines = [
        "fn start() {",
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
    // A status line on the second-to-last row in reverse video, and
    // the command line on the last.
    let status = format!(
        " {:<width$}",
        "NORMAL  main.rs  3,17  Top",
        width = cols.saturating_sub(1)
    );
    let _ = write!(
        s,
        "\x1b[{};1H\x1b[7m{status}\x1b[0m",
        rows.saturating_sub(1)
    );
    let _ = write!(s, "\x1b[{rows};1H:wq");
    widget.feed(doc, root, s.as_bytes());
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
            s.push_str("\x1b[38;2;214;62;62mred \x1b[38;2;34;150;78mgreen \x1b[38;2;52;108;220mblue\x1b[0m\r\n");
            s.push_str("\r\n$ \x1b[32mecho\x1b[0m \x1b[33m\"hello, Tilo\"\x1b[0m\r\n");
            widget.feed(doc, root, s.as_bytes());
        }
        // A full-screen alternate-screen redraw resembling a text editor,
        // sized to the grid so its bars span the window and its status line
        // sits on the last rows, as a real editor draws them.
        "tui" => feed_tui(widget, doc, root),
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

#[cfg(test)]
mod launch_tests {
    use super::*;
    use portable_pty::{MasterPty, PtySize};
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    thread_local! {
        static CONTROLLED_ARGS: RefCell<Option<LaunchArgs>> = const { RefCell::new(None) };
    }
    static OPEN_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[expect(
        clippy::unnecessary_wraps,
        reason = "matches the launch spec's fallible root-factory signature"
    )]
    fn capture_args(args: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        CONTROLLED_ARGS.with(|captured| *captured.borrow_mut() = Some(args.clone()));
        Ok(Box::new(ui::widget::Text::new("argument capture")))
    }

    fn controlled_open(_: &'static str) -> (ui::State, ui::persist::DocWriter) {
        OPEN_COUNT.fetch_add(1, Ordering::SeqCst);
        (
            ui::State::new(),
            ui::persist::DocWriter::disabled("Terminal controlled launch"),
        )
    }

    fn forbidden_root(_: &LaunchArgs) -> Result<Box<dyn Widget>, LaunchError> {
        Err(LaunchError::Operation(
            "prepared Terminal construction invoked the spec root factory".into(),
        ))
    }

    fn controlled_args() -> LaunchArgs {
        CONTROLLED_ARGS.with(|captured| *captured.borrow_mut() = None);
        let app = LaunchSpec::new_with_args("dev.tilo.terminal-args", capture_args)
            .open_state(controlled_open)
            .build_controlled_app_for_test()
            .unwrap();
        drop(app);
        CONTROLLED_ARGS.with(|captured| {
            captured
                .borrow_mut()
                .take()
                .expect("controlled builder supplies parsed launch arguments")
        })
    }

    struct MockMaster;
    impl MasterPty for MockMaster {
        fn get_size(&self) -> Result<PtySize, anyhow::Error> {
            Ok(PtySize::default())
        }
        fn resize(&self, _size: PtySize) -> Result<(), anyhow::Error> {
            Ok(())
        }
        fn take_writer(&self) -> Result<Box<dyn Write + Send>, anyhow::Error> {
            Ok(Box::new(std::io::sink()))
        }
        fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, anyhow::Error> {
            Ok(Box::new(std::io::empty()))
        }
        fn process_group_leader(&self) -> Option<i32> {
            None
        }
        fn as_raw_fd(&self) -> Option<i32> {
            None
        }
        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    struct ChanWriter(std::sync::mpsc::Sender<Vec<u8>>);
    impl Write for ChanWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn actual_custom_helper_installs_canonical_history_on_the_supplied_terminal() {
        OPEN_COUNT.store(0, Ordering::SeqCst);
        let args = controlled_args();
        OPEN_COUNT.store(0, Ordering::SeqCst);
        let (write_tx, writes) = std::sync::mpsc::channel();
        let (output, output_rx) = std::sync::mpsc::channel();
        let widget = TerminalWidget::new(
            Arc::new(Mutex::new(Box::new(ChanWriter(write_tx)))),
            Arc::new(Mutex::new(Box::new(MockMaster))),
            output_rx,
            tilo_content::ContentStore::memory(),
        );
        let waker = widget.waker_handle();
        let wake_scope = ui::WakeScope::new();
        let _entry = wake_scope.enter();
        let spec = launch_spec_with_root(forbidden_root).open_app_global_state(controlled_open);
        spec.validate_history_declaration().unwrap();
        let app = build_prepared_terminal_app(
            &spec,
            &args,
            wake_scope,
            Box::new(widget),
            |spec, args, wake_scope, root| {
                spec.build_controlled_prepared_root_for_test(args, wake_scope, root)
            },
        )
        .unwrap();

        assert_eq!(OPEN_COUNT.load(Ordering::SeqCst), 1);
        let (installed, root) = app.controlled_history_definitions_for_test();
        assert!(installed.is_some_and(|definition| {
            std::ptr::eq(
                definition,
                std::ptr::from_ref(&terminal::framework_history::HISTORY),
            )
        }));
        assert!(root.is_some_and(|definition| {
            std::ptr::eq(
                definition,
                std::ptr::from_ref(&terminal::framework_history::HISTORY),
            )
        }));
        assert_eq!(Arc::strong_count(&waker), 2);
        assert!(matches!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        output.send(b"retained output\r\n".to_vec()).unwrap();
        drop(app);
        assert_eq!(Arc::strong_count(&waker), 1);
        assert!(output.send(b"after drop".to_vec()).is_err());
        assert!(matches!(
            writes.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }
}

// Thin binary entry point — all logic lives in the library.
//
// Two modes (mirroring the sibling terminal / file-explorer binaries):
//   * default — build a `SettingsApp`, hand it to `ui::App`, run the live
//     event loop (winit window).
//   * `--shots <PATH>` — render the settings app into a PNG at `<PATH>`
//     and exit. The scenario (wide vs. narrow) is derived from the output
//     filename (any name containing `narrow` selects the responsive
//     single-pane shot), so one binary produces both required snapshots.

use settings::shots::render_for_path;
use settings::SettingsApp;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        render_for_path(&out)?;
        return Ok(());
    }

    let widget = SettingsApp::new();
    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, Box::new(widget), "dev.qos.settings"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.settings");
    let mut app = ui::App::new(state, Box::new(widget)).with_autosave(autosave);
    // Filtering the sidebar as the user types rebuilds the widget tree each
    // keystroke. Preserve keyboard focus across that rebuild so the search
    // field keeps the caret (it is the first focusable, so focus returns to
    // it by index); without this, typing would drop focus after one letter.
    app.autofocus_on_structure_change = true;
    // Let the Appearance pane re-scale / re-theme the running window live: the
    // App polls the tree for the pane's appearance intent each tick and applies
    // any change (UI scale, text size, theme, contrast, motion) immediately.
    app.appearance_reconcile = true;
    app.run();
    Ok(())
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`. Same ad-hoc parser the other native apps
/// use; avoids pulling in `clap` for a single string flag.
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

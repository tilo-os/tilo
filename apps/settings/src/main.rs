// Thin binary entry point — all logic lives in the library.
//
// Supports two modes (mirroring the sibling terminal / file-explorer
// binaries):
//   * default — build a `SettingsApp`, hand it to `ui::App`, run the
//     live event loop (winit window).
//   * `--shots <PATH>` — render the settings app at a default size into
//     a PNG at `<PATH>` and exit. No window, no event loop. Used for
//     headless screenshot tests and CI snapshots of the cold-start UI.

use settings::{render_to_path, SettingsApp};
use settings::shots::DEFAULT_SHOTS_VIEWPORT;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let widget = SettingsApp::new();
        render_to_path(Box::new(widget), DEFAULT_SHOTS_VIEWPORT, &out)?;
        return Ok(());
    }

    let widget = SettingsApp::new();
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
    app.run();
    Ok(())
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`. Same ad-hoc parser the other native apps
/// use; we avoid pulling in `clap` for a single boolean-with-string.
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

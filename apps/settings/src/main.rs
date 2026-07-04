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
    let state = ui::State::new();
    let app = ui::App::new(state, Box::new(widget));
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

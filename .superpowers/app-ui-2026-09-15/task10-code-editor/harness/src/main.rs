use std::path::{Path, PathBuf};
use std::time::Duration;

use code_editor::CodeApp;
use ui::headless::Driver;

fn capture(out: &Path, route: &str, viewport: (u32, u32)) -> Result<(), String> {
    let mut app = CodeApp::with_sample();
    if route == "terminal" {
        app.shots_open_terminal();
        app.shots_terminal_input("echo 'qOS embedded terminal (live PTY)'; echo ready\n");
        std::thread::sleep(Duration::from_millis(180));
    }

    let mut driver = Driver::new(ui::State::new(), Box::new(app), viewport);
    if route == "problems" {
        driver.click_named("code.bottom-panel-toggle")?;
    }
    if route == "terminal" {
        driver.relayout();
    }

    let panel_visible = driver.targets().contains_key("bottom_panel");
    match route {
        "fresh" if panel_visible => return Err("fresh route unexpectedly shows dock".into()),
        "problems" | "terminal" if !panel_visible => {
            return Err(format!("{route} route did not show dock"));
        }
        _ => {}
    }

    let stem = format!("{route}-{}x{}", viewport.0, viewport.1);
    driver.render_window_png(&out.join(format!("{stem}.png")));
    let inspection = driver.inspection_json().map_err(|error| error.to_string())?;
    std::fs::write(out.join(format!("{stem}.inspection.json")), inspection)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn main() -> Result<(), String> {
    let out = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "usage: task10-code-editor-capture OUTPUT_DIR".to_string())?;
    std::fs::create_dir_all(&out).map_err(|error| error.to_string())?;
    for viewport in [(800, 700), (480, 900)] {
        for route in ["fresh", "problems", "terminal"] {
            capture(&out, route, viewport)?;
        }
    }
    Ok(())
}

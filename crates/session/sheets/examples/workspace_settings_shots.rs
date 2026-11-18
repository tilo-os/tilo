//! The workspace settings sheet as a person sees it, one PNG per scenario:
//! what a review of its words and layout looks at.
//!
//!     cargo build -p tilo-sheets --example workspace_settings_shots
//!     target/debug/examples/workspace_settings_shots <dir>

use tilo_sheets::workspace_settings::WorkspaceSettingsApp;
use ui::framework::testing::HeadlessApp;

fn main() -> Result<(), String> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "workspace-settings-shots".to_owned());
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    for name in ["people", "people-member"] {
        let mut app = HeadlessApp::<WorkspaceSettingsApp>::capture_at(Some(name), (1280, 900))
            .map_err(|error| error.to_string())?;
        app.settle();
        let path = std::path::Path::new(&dir).join(format!("workspace-settings-{name}.png"));
        app.harness().render_png(&path);
        println!("{}", path.display());
    }
    Ok(())
}

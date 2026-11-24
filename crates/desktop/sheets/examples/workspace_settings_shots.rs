//! The workspace settings sheet as a person sees it, one PNG per scenario:
//! what a review of its words and layout looks at.
//!
//!     cargo build -p tilo-sheets --example workspace_settings_shots
//!     target/debug/examples/workspace_settings_shots <dir> [scenario…]

use tilo_sheets::workspace_settings::WorkspaceSettingsApp;
use ui::framework::testing::HeadlessApp;

fn main() -> Result<(), String> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "workspace-settings-shots".to_owned());
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mut names: Vec<String> = std::env::args().skip(2).collect();
    if names.is_empty() {
        names = vec![
            "people".to_owned(),
            "people-member".to_owned(),
            "people-revoking".to_owned(),
        ];
    }
    for name in &names {
        let file = format!("workspace-settings-{name}.png");
        let path = std::path::Path::new(&dir).join(file);
        shoot::<WorkspaceSettingsApp>(name, &path)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn shoot<A: ui::framework::ResourceApp>(name: &str, path: &std::path::Path) -> Result<(), String> {
    let mut app =
        HeadlessApp::<A>::capture_at(Some(name), (1280, 900)).map_err(|error| error.to_string())?;
    app.settle();
    app.harness().render_png(path);
    Ok(())
}

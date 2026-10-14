//! Capture the real hosted editor with the complete UI scaled together.
//!
//! cargo run --locked -p typst-editor --example showcase -- output.png 1.5 dark
//! Put a symlink to crates/sdk/ui-core/assets/fonts beside this binary first.

use typst_editor::TypstEditorShell;
use ui::framework::testing::TestApp;
use ui::theme::Theme;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let output = args.next().ok_or("expected output PNG path")?;
    let scale: f32 = args.next().as_deref().unwrap_or("1.5").parse()?;
    let name = args.next().unwrap_or_else(|| "dark".to_owned());
    let theme = Theme::named(&name)
        .ok_or("unknown theme")?
        .with_ui_scale(scale);
    let mut app = TestApp::<TypstEditorShell>::capture_at(None, (1440, 810))?;
    app.harness().set_theme(theme);
    app.harness().render_png(std::path::Path::new(&output));
    app.harness().close_components();
    Ok(())
}

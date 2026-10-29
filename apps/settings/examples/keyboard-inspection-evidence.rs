//! Actual narrow Settings page with native configuration fixture; never a desktop capture.
use settings::{SettingsShell, categories::Category};
use ui::{framework::testing::TestApp, theme::Theme};
use winit::keyboard::{KeyboardLayout, KeyboardLayoutKey};

fn main() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments.next().map_or_else(
        || std::path::PathBuf::from("target/keyboard-inspection"),
        std::path::PathBuf::from,
    );
    if arguments.next().is_some() {
        return Err("usage: keyboard-inspection-evidence [output directory]".into());
    }
    let geometry: serde_json::Value =
        serde_json::from_str(pc_keyboard_geometry::SOURCE).map_err(|error| error.to_string())?;
    let names = geometry
        .get("pc105")
        .and_then(|value| value.get("keys"))
        .and_then(serde_json::Value::as_array)
        .ok_or("geometry has no ISO keys")?
        .iter()
        .filter_map(|key| key.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let native = tilo_keyboard_map::compile("gr", "", "pc105", "", &names)
        .map_err(|error| error.to_string())?;
    let snapshot = std::sync::Arc::new(KeyboardLayout {
        depressed: 0,
        latched: 0,
        locked: 0,
        group: 0,
        keys: native
            .into_iter()
            .map(|key| KeyboardLayoutKey {
                physical: key.physical,
                symbols: key
                    .levels
                    .first()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|symbol| winit::keyboard::KeyboardLayoutSymbol {
                        name: symbol.name,
                        text: symbol.text,
                    })
                    .collect(),
            })
            .collect(),
    });
    for (theme_name, theme) in [
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for width in [600, 1100] {
            // The Keyboard page is view code, built only in the app's view.
            let mut app = TestApp::<SettingsShell>::new().map_err(|error| format!("{error:?}"))?;
            app.effect(&settings::effects::open_pane(Category::Keyboard))
                .map_err(|error| format!("{error:?}"))?;
            let root = app.harness().root_widget_mut();
            root.observe_physical_input(ui::widget::PhysicalInput::Focused(true));
            root.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
                std::sync::Arc::clone(&snapshot),
            )));
            let frame = app.harness();
            frame
                .relayout_themed((width, 1200), theme.clone())
                .map_err(|error| format!("{error:?}"))?;
            // The keyboard is drawn off the UI thread.
            if !frame.settle_for_capture(ui::time::Duration::from_secs(30)) {
                return Err("the Keyboard page did not come to rest".into());
            }
            let path = output.join(format!("settings-key-inspection-{theme_name}-{width}.png"));
            frame.render_png(&path);
            if !path.is_file() {
                return Err("offscreen renderer produced no artifact".into());
            }
            println!("keyboard-inspection-evidence {}", path.display());
        }
    }
    Ok(())
}

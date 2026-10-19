//! Actual narrow Settings page with native configuration fixture; never a desktop capture.
use settings::{SettingsApp, categories::Category};
use ui::{State, headless::FrameHarness, theme::Theme, widget::Widget as _};
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
        serde_json::from_str(pc_keyboard_geometry::SOURCE)
            .map_err(|error| error.to_string())?;
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
        for (width, logical_width) in [(600, 600.0), (1100, 1100.0)] {
            let mut app = SettingsApp::with_config_root(output.join("private-config"))
                .with_category(Category::Keyboard)
                .with_width_hint(logical_width);
            app.observe_physical_input(ui::widget::PhysicalInput::Focused(true));
            app.observe_physical_input(ui::widget::PhysicalInput::Layout(Some(
                std::sync::Arc::clone(&snapshot),
            )));
            let frame = FrameHarness::for_root(State::new(), app, (width, 1200), theme.clone());
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

//! Offscreen evidence using the production XKB provider and retained input path.
use settings::keyboard_preview;
use ui::{KeyboardMap, State, headless::FrameHarness, theme::Theme};
use winit::{
    event::{ElementState, KeyEvent, RawKeyEvent, WindowEvent},
    keyboard::{Key, KeyCode, KeyLocation, PhysicalKey},
};
fn event(code: KeyCode, logical: &str, state: ElementState) -> WindowEvent {
    let key = Key::Character(logical.into());
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
        is_synthetic: false,
        event: KeyEvent::synthetic(
            RawKeyEvent {
                physical_key: PhysicalKey::Code(code),
                state,
            },
            key.clone(),
            Some(logical.into()),
            KeyLocation::Standard,
            false,
            Some(logical.into()),
            key,
        ),
    }
}
fn main() -> Result<(), String> {
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for (width, height) in [(600, 240), (1100, 440)] {
            for (source, model, pressed) in [("us", "pc104", false), ("de", "pc105", true)] {
                let map = keyboard_preview::preview_layout(
                    source,
                    model,
                    &settings_store::hardware::RichInputConfig::default(),
                    format!("{source} {model}"),
                )?;
                let mut frame =
                    FrameHarness::for_root(State::new(), map, (width, height), theme.clone());
                frame.click_named("input.keyboard_map")?;
                if pressed {
                    // German Z arrives at the physical US Y position. Neither the
                    // logical character nor current layout controls the highlight.
                    frame.send(&event(KeyCode::KeyY, "z", ElementState::Pressed));
                    frame.send(&event(KeyCode::ShiftLeft, "", ElementState::Pressed));
                    if !frame
                        .root::<KeyboardMap>()
                        .is_pressed(PhysicalKey::Code(KeyCode::KeyY))
                    {
                        return Err("the retained physical key did not highlight".into());
                    }
                }
                let path = std::path::PathBuf::from(format!(
                    "target/keyboard-maps/{source}-{model}-{theme_name}-{width}.png"
                ));
                frame.render_png(&path);
                if !path.is_file() {
                    return Err("offscreen renderer produced no artifact".into());
                }
                println!("keyboard-map-evidence {}", path.display());
                frame.send(&WindowEvent::Focused(false));
                if frame
                    .root::<KeyboardMap>()
                    .is_pressed(PhysicalKey::Code(KeyCode::KeyY))
                {
                    return Err("window blur retained physical input feedback".into());
                }
            }
        }
    }
    Ok(())
}

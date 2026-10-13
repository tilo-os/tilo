//! Offscreen evidence using the production XKB provider and retained input path.
use settings::keyboard_preview;
use ui::{KeyboardMap, State, headless::FrameHarness, theme::Theme};
use winit::{
    event::{ElementState, KeyEvent, RawKeyEvent, WindowEvent},
    keyboard::{Key, KeyCode, KeyLocation, PhysicalKey},
};
fn event(code: KeyCode, logical: &str, state: ElementState) -> WindowEvent {
    let key = Key::Character(logical.into());
    ui::synthetic_input::keyboard_input(KeyEvent::synthetic(
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
    ))
}
fn main() -> Result<(), String> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments.next().map_or_else(
        || std::path::PathBuf::from("target/keyboard-maps"),
        std::path::PathBuf::from,
    );
    if arguments.next().is_some() {
        return Err("usage: keyboard-map-evidence [output directory]".into());
    }
    for (theme_name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        for (width, height) in [(600, 240), (1100, 440)] {
            for (source, model, pressed) in [
                ("us", "pc104", false),
                ("de", "pc105", true),
                ("jp", "jp106", true),
                ("kr", "kr106", true),
            ] {
                let map = keyboard_preview::preview_layout(
                    source,
                    model,
                    &settings_store::hardware::RichInputConfig::default(),
                    format!("{source} {model}"),
                )?;
                let mut frame =
                    FrameHarness::for_root(State::new(), map, (width, height), theme.clone());
                frame.click_named("input.keyboard_map")?;
                let code = match model {
                    "jp106" => KeyCode::IntlYen,
                    "kr106" => KeyCode::Lang1,
                    _ => KeyCode::KeyY,
                };
                if pressed {
                    // The physical position, rather than logical text, controls highlighting.
                    frame.send(&event(code, "z", ElementState::Pressed));
                    frame.send(&event(KeyCode::ShiftLeft, "", ElementState::Pressed));
                    if !frame
                        .root::<KeyboardMap>()
                        .is_pressed(PhysicalKey::Code(code))
                    {
                        return Err("the retained physical key did not highlight".into());
                    }
                }
                let path = output.join(format!("{source}-{model}-{theme_name}-{width}.png"));
                frame.render_png(&path);
                if !path.is_file() {
                    return Err("offscreen renderer produced no artifact".into());
                }
                println!("keyboard-map-evidence {}", path.display());
                frame.send(&WindowEvent::Focused(false));
                if frame
                    .root::<KeyboardMap>()
                    .is_pressed(PhysicalKey::Code(code))
                    || frame
                        .root::<KeyboardMap>()
                        .is_pressed(PhysicalKey::Code(KeyCode::ShiftLeft))
                {
                    return Err("window blur retained physical input feedback".into());
                }
            }
        }
    }
    Ok(())
}

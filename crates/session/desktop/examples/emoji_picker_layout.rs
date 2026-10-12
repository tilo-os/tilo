//! Production captures of emoji picker footer and group layout.
//! Run with an output directory; a missing renderer fails the capture.
use std::error::Error;
use std::path::Path;
use tilo_desktop::emoji_picker::EmojiPicker;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{DeviceId, ElementState, KeyEvent, RawKeyEvent, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

fn key(picker: &mut EmojiPicker, logical: Key, modifiers: ModifiersState) {
    let event = KeyEvent::synthetic(
        RawKeyEvent {
            physical_key: PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            state: ElementState::Pressed,
        },
        logical.clone(),
        None,
        KeyLocation::Standard,
        false,
        None,
        logical,
    );
    picker.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: DeviceId::dummy(),
            event,
            is_synthetic: false,
        },
        1.0,
        modifiers,
        None,
        Instant::now(),
    );
}

fn search(picker: &mut EmojiPicker, query: &str) {
    key(picker, Key::Character("a".into()), ModifiersState::CONTROL);
    for character in query.chars() {
        key(
            picker,
            Key::Character(character.to_string().into()),
            ModifiersState::empty(),
        );
    }
}

fn capture(
    picker: &mut EmojiPicker,
    theme: &Theme,
    viewport: Rect,
    renderer: &mut ui::offscreen::OffscreenRenderer,
    path: &Path,
) {
    let mut scene = Scene::new();
    picker.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::default(),
        viewport,
    );
    renderer.render_png(&scene, theme.tokens.colors.surfaces.base, path);
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("provide output directory")?;
    let groups_only = match std::env::args().nth(2).as_deref() {
        None => false,
        Some("--groups-only") => true,
        Some(_) => return Err("unknown capture option".into()),
    };
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    for (name, width, height, theme) in [
        ("dark200", 800, 700, Theme::dark().with_text_scale(2.0)),
        ("narrow200", 320, 700, Theme::light().with_text_scale(2.0)),
        ("normal", 800, 700, Theme::light()),
    ] {
        let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
        let mut picker = EmojiPicker::new();
        picker.open(viewport, "en-US");
        search(&mut picker, "thumbs up");
        if !groups_only {
            capture(
                &mut picker,
                &theme,
                viewport,
                &mut renderer,
                &out.join(format!("{name}-hint.png")),
            );
        }
        key(
            &mut picker,
            Key::Character("a".into()),
            ModifiersState::CONTROL,
        );
        key(
            &mut picker,
            Key::Named(winit::keyboard::NamedKey::Backspace),
            ModifiersState::empty(),
        );
        for _ in 0..8 {
            key(
                &mut picker,
                Key::Named(winit::keyboard::NamedKey::Tab),
                ModifiersState::empty(),
            );
        }
        capture(
            &mut picker,
            &theme,
            viewport,
            &mut renderer,
            &out.join(format!("{name}-groups.png")),
        );
    }
    Ok(())
}

//! Fixture-only production chooser captures; no application receives commits.
use std::error::Error;
use std::path::Path;
use tilo_desktop::emoji_picker::EmojiPicker;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::ElementState;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn send(picker: &mut EmojiPicker, key: Key, repeat: bool, modifiers: ModifiersState) {
    let mut event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
    event.repeat = repeat;
    picker.handle_event(
        &ui::synthetic_input::keyboard_input(event),
        1.0,
        modifiers,
        None,
        Instant::now(),
    );
}

fn capture(
    picker: &mut EmojiPicker,
    theme: &Theme,
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
        Rect::new(0.0, 0.0, 800.0, 700.0),
    );
    renderer.render_png(&scene, theme.tokens.colors.surfaces.base, path);
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("provide output directory")?;
    let expected_open = match std::env::args().nth(2).as_deref() {
        Some("before") => false,
        Some("after") => true,
        _ => return Err("provide before or after expectation".into()),
    };
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("contrast", Theme::high_contrast()),
        ("text200", Theme::dark().with_text_scale(2.0)),
        (
            "combined",
            Theme::light().with_ui_scale(1.5).with_text_scale(1.3),
        ),
    ] {
        let mut picker = EmojiPicker::new();
        picker.open(Rect::new(0.0, 0.0, 800.0, 700.0), "en-US");
        for c in "thumbs up".chars() {
            send(
                &mut picker,
                Key::Character(c.to_string().into()),
                false,
                ModifiersState::empty(),
            );
        }
        if picker.selected() != Some("👍") {
            return Err("fixture search did not select thumbs up".into());
        }
        send(
            &mut picker,
            Key::Named(NamedKey::Enter),
            false,
            ModifiersState::SHIFT,
        );
        if picker.tone_forms().is_none() {
            return Err("first press did not open the tone chooser".into());
        }
        let mut renderer = ui::offscreen::OffscreenRenderer::new(800, 700);
        capture(
            &mut picker,
            &theme,
            &mut renderer,
            &out.join(format!("{name}-opened.png")),
        );
        send(
            &mut picker,
            Key::Named(NamedKey::Enter),
            true,
            ModifiersState::SHIFT,
        );
        if picker.is_open() != expected_open {
            return Err("unexpected production open state after held Enter".into());
        }
        if picker.take_commits().len() != usize::from(!expected_open) {
            return Err("unexpected queued commit count after held Enter".into());
        }
        capture(
            &mut picker,
            &theme,
            &mut renderer,
            &out.join(format!("{name}-held.png")),
        );
        println!(
            "{name}: open={expected_open}, queued={}",
            usize::from(!expected_open)
        );
    }
    Ok(())
}

//! Fixture-only production captures of constrained picker states.
use std::error::Error;
use std::path::Path;
use tilo_desktop::emoji_picker::EmojiPicker;
use ui::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme, time::Instant};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
};
use winit::{
    event::ElementState,
    keyboard::{Key, ModifiersState, NamedKey},
};

fn send(picker: &mut EmojiPicker, key: Key, modifiers: ModifiersState) {
    picker.handle_event(
        &ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            key,
            None,
            ElementState::Pressed,
        )),
        1.0,
        modifiers,
        None,
        Instant::now(),
    );
}

fn capture(
    out: &Path,
    (name, width, height, theme, tones): (&str, u32, u32, Theme, bool),
) -> Result<(), Box<dyn Error>> {
    let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut picker = EmojiPicker::new();
    picker.open(viewport, "en-US");
    let query = if name == "short-no-results" {
        "zzzz-no-emoji-results"
    } else {
        "thumbs up"
    };
    for c in query.chars() {
        send(
            &mut picker,
            Key::Character(c.to_string().into()),
            ModifiersState::empty(),
        );
    }
    if name == "short-no-results" {
        if picker.selected().is_some() {
            return Err("fixture empty search failed".into());
        }
    } else if picker.selected() != Some("👍") {
        return Err("fixture search failed".into());
    }
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    picker.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
        viewport,
    );
    if tones {
        send(
            &mut picker,
            Key::Named(NamedKey::Enter),
            ModifiersState::SHIFT,
        );
        if picker.tone_forms().is_none() {
            return Err("fixture tone chooser failed".into());
        }
    }
    if name == "narrow-last-tone" {
        send(
            &mut picker,
            Key::Named(NamedKey::End),
            ModifiersState::empty(),
        );
    }
    let mut scene = Scene::new();
    picker.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::default(),
        viewport,
    );
    let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
    renderer.render_png(
        &scene,
        theme.tokens.colors.surfaces.base,
        out.join(format!("{name}.png")),
    );
    println!(
        "{name}: card={:?}, selected_target={:?}",
        picker.card_rect(),
        picker.emoji_rect(0)
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("provide output directory")?;
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    for variant in vec![
        ("narrow-light", 240, 360, Theme::light(), true),
        (
            "short-text200",
            320,
            240,
            Theme::dark().with_text_scale(2.0),
            false,
        ),
        (
            "short-combined",
            320,
            240,
            Theme::light().with_ui_scale(1.5).with_text_scale(1.3),
            true,
        ),
        ("normal-dark", 800, 700, Theme::dark(), true),
        ("short-contrast", 320, 240, Theme::high_contrast(), true),
        ("narrow-last-tone", 240, 360, Theme::light(), true),
        (
            "short-no-results",
            320,
            240,
            Theme::dark().with_text_scale(2.0),
            false,
        ),
    ]
    .into_boxed_slice()
    {
        capture(out, variant)?;
    }
    Ok(())
}

//! Source-only production captures of an interrupted emoji pick.
//! Run with an output directory; a missing renderer fails the capture.
use std::error::Error;
use std::path::Path;
use tilo_desktop::emoji_picker::EmojiPicker;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, RawKeyEvent, WindowEvent};
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
        &ui::synthetic_input::keyboard_input(event),
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

fn pointer(picker: &mut EmojiPicker, state: ElementState, point: Point) {
    picker.handle_event(
        &ui::synthetic_input::mouse_input(state, MouseButton::Left),
        1.0,
        ModifiersState::empty(),
        Some(point),
        Instant::now(),
    );
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
    let out = Path::new(&directory);
    std::fs::create_dir_all(out)?;
    #[allow(
        clippy::useless_vec,
        reason = "seven themes are too large to sit on the stack as an array"
    )]
    for (name, width, height, theme) in vec![
        ("dark", 800, 700, Theme::dark()),
        ("light", 800, 700, Theme::light()),
        ("large-text", 800, 700, Theme::dark().with_text_scale(2.0)),
        (
            "contrast",
            800,
            700,
            Theme::high_contrast().with_text_scale(2.0),
        ),
        (
            "combined",
            800,
            700,
            Theme::dark().with_ui_scale(1.5).with_text_scale(1.3),
        ),
        ("narrow", 320, 700, Theme::light().with_text_scale(2.0)),
    ] {
        let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let mut renderer = ui::offscreen::OffscreenRenderer::new(width, height);
        for state in ["idle", "focus-cancel", "search-cancel"] {
            let mut picker = EmojiPicker::new();
            picker.open(viewport, "en-US");
            search(&mut picker, "pizza");
            // Install the actual theme/viewport geometry before input.
            capture(
                &mut picker,
                &theme,
                viewport,
                &mut renderer,
                &out.join(format!("{name}-idle.png")),
            );
            let cell = picker
                .emoji_rect(0)
                .ok_or("search result must be visible")?
                .center();
            if state != "idle" {
                pointer(&mut picker, ElementState::Pressed, cell);
                if state == "focus-cancel" {
                    picker.handle_event(
                        &WindowEvent::Focused(false),
                        1.0,
                        ModifiersState::empty(),
                        Some(cell),
                        Instant::now(),
                    );
                } else {
                    search(&mut picker, "thumbs up");
                }
                pointer(&mut picker, ElementState::Released, cell);
                capture(
                    &mut picker,
                    &theme,
                    viewport,
                    &mut renderer,
                    &out.join(format!("{name}-{state}.png")),
                );
            }
        }
    }
    Ok(())
}

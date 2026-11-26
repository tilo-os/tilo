use super::*;

fn app() -> App {
    App::new(State::new(), Box::new(crate::widget::Text::new("t")))
}

/// An empty settings document for this test's thread, so the app it builds
/// starts from the stock theme whatever the person's profile, or a test
/// running beside it, holds.
fn own_settings() -> settings_store::SourceScope {
    settings_store::scoped_source(Some(std::sync::Arc::new(
        settings_store::HostDocument::new(serde_json::Map::new(), |_| Ok(())),
    )))
}

#[test]
fn host_appearance_rethemes_through_the_appearance_intent() {
    use tilo_ui_base::{host_appearance::HostAppearance, host_appearance::Scheme};
    // A process the shell supplies no settings to: the host's appearance
    // is the one it follows.
    let _settings = settings_store::scoped_source(None);
    let mut a = app();
    let light = HostAppearance {
        scheme: Some(Scheme::Light),
        ..HostAppearance::default()
    };
    assert!(a.apply_host_appearance(light));
    assert_eq!(a.theme.name, "Tilo light");
    assert_eq!(
        a.theme.tokens.colors.surfaces.content,
        Theme::light().tokens.colors.surfaces.content
    );
    assert!(
        !a.apply_host_appearance(light),
        "an unchanged host is a no-op"
    );

    let accented = HostAppearance {
        accent: Some([0xE0, 0x40, 0x80]),
        ..light
    };
    assert!(
        a.apply_host_appearance(accented),
        "an accent change alone re-themes"
    );
    assert_eq!(
        a.theme.tokens.colors.intent.you,
        Theme::light().tokens.colors.presence.hue(
            tilo_ui_base::tokens::nearest_presence_hue(vello::peniko::Color::from_rgb8(0xE0, 0x40, 0x80))
                .expect("a pink has a hue")
        ),
        "the host accent is the person's own colour"
    );

    assert!(a.apply_host_appearance(HostAppearance {
        high_contrast: true,
        reduce_motion: true,
        ..accented
    }));
    assert!(a.high_contrast());
    assert!(a.reduce_motion());
    assert_eq!(a.theme.name, "High contrast light");
}

tilo_test_env::scenario! {
    fn low_vision_and_window_appearance_reach_the_theme_and_stay;

    phase the_shell_s_window_appearance_dresses_the_theme_and_survives_rebuilds;
    {
        use tilo_ui_base::theme::WindowAppearance;
        use tilo_ui_base::tokens::PresenceHue;
        let _settings = own_settings();

        let mut a = app();
        a.set_theme(Theme::light());
        let neutral = a.theme.tokens.colors.surfaces.content;
        assert_eq!(
            neutral,
            Theme::light().tokens.colors.surfaces.content,
            "a window nobody told keeps the stock temperature"
        );

        let told = WindowAppearance {
            workspace_hue: Some(40.0),
            your_colour: Some(PresenceHue::Violet),
            corner_radius: None,
        };
        let message = crate::floating_surface::FloatingSurfaceHostMessage::appearance(&told);
        assert!(a.set_window_appearance(message.window_appearance().unwrap()));
        assert!(!a.set_window_appearance(told), "the same appearance again");
        assert_eq!(a.window_appearance(), told);
        assert_eq!(a.theme.document.palette.hue, Some(40.0));
        assert_eq!(a.theme.tokens.colors.you, PresenceHue::Violet);
        // The focus ring is the accent's ink: the colour's hue at the
        // lightness its contrast floor needs.
        assert_eq!(
            a.theme.border_focus,
            a.theme.tokens.colors.intent.accent_ink
        );
        let ring = crate::color::Oklch::from_color(a.theme.border_focus).h;
        let violet =
            crate::color::Oklch::from_color(a.theme.tokens.colors.presence.hue(PresenceHue::Violet)).h;
        let d = (ring - violet).rem_euclid(360.0);
        assert!(
            d.min(360.0 - d) <= 15.0,
            "focus ring, caret and selection follow"
        );
        assert_ne!(a.theme.tokens.colors.surfaces.content, neutral);

        // A settings change rebuilds from the base theme and keeps the window's.
        a.set_theme(Theme::light());
        assert_eq!(a.theme.document.palette.hue, Some(40.0));
        a.toggle_high_contrast();
        assert_eq!(a.theme.tokens.colors.you, PresenceHue::Violet);
        a.toggle_high_contrast();

        // Moving to a neutral workspace takes the temperature away again.
        a.set_window_appearance(WindowAppearance {
            workspace_hue: None,
            your_colour: Some(PresenceHue::Violet),
            corner_radius: None,
        });
        assert_eq!(a.theme.tokens.colors.surfaces.content, neutral);
    }

    phase shell_palette_message_rethemes_the_open_app_without_settings_file_access;
    {
        use crate::floating_surface::FloatingSurfaceHostMessage;
        let _settings = own_settings();

        let mut app = app();
        app.set_theme(Theme::dark());
        app.set_text_scale(1.5);
        let appearance = tilo_ui_base::theme::WindowAppearance::default();
        for theme in [Theme::light(), Theme::high_contrast(), Theme::dark()] {
            let message = FloatingSurfaceHostMessage::appearance_with_theme(&appearance, &theme, None);
            let wire = serde_json::to_vec(&message).unwrap();
            let received = serde_json::from_slice(&wire).unwrap();
            assert!(app.apply_floating_host_messages(vec![received]));
            assert_eq!(app.theme.document, theme.document);
            assert_eq!(
                app.text_scale(),
                1.5,
                "palette changes preserve text scaling"
            );
            app.apply_configured_theme();
            app.rebuild_theme();
            assert_eq!(
                app.theme.document, theme.document,
                "private settings reloads preserve the host palette"
            );
        }
        let mut wire = serde_json::to_value(FloatingSurfaceHostMessage::appearance_with_theme(
            &appearance,
            &Theme::light(),
            None,
        ))
        .unwrap();
        wire["theme"]["schema_version"] = serde_json::json!(0);
        let before = app.theme.clone();
        let received = serde_json::from_value(wire).unwrap();
        app.apply_floating_host_messages(vec![received]);
        assert_eq!(
            app.theme, before,
            "an invalid host palette must not replace the live theme"
        );
    }
}

/// A window the shell floats as a card draws on the card's ground, the one its header and every
/// other card in the column stand on, through a theme rebuild; back in its strip it draws on the
/// window ground again.
#[cfg(feature = "native")]
#[test]
fn a_window_floating_as_a_card_draws_on_the_cards_ground() {
    use crate::window_toolbar::ToolbarEvent;
    let _settings = own_settings();
    let mut a = app();
    a.set_theme(Theme::dark());
    let window = Theme::dark().tokens.colors.surfaces.content;
    let card = crate::widget::floating_surface::card_ground(&Theme::dark());
    assert_ne!(window, card, "a card stands on a ground of its own");
    let ground = |a: &App| a.theme.tokens.colors.surfaces.content;
    a.receive_bar_event(ToolbarEvent::Floating { floating: true }, None);
    assert_eq!(ground(&a), card, "floating");
    a.set_theme(Theme::dark());
    assert_eq!(ground(&a), card, "a new theme keeps it on the card");
    a.receive_bar_event(ToolbarEvent::Floating { floating: false }, None);
    assert_eq!(ground(&a), window, "back in its strip");
}

/// A window of 320 × 200 logical pixels, and its frame's damage.
#[cfg(feature = "native")]
struct Window;

#[cfg(feature = "native")]
impl crate::app::surface::AppSurface for Window {
    fn scale_factor(&self) -> f64 {
        1.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(320, 200)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: winit::window::CursorIcon) {}
}

#[cfg(feature = "native")]
fn frame(app: &mut App) -> crate::app::surface::SurfaceDamage {
    let mut scene = crate::vello::Scene::new();
    let mut damage = crate::app::surface::SurfaceDamage::default();
    app.surface_frame_with_damage(&Window, &mut scene, &mut damage);
    damage
}

/// A window the shell draws in another theme repaints every pixel of it: the
/// theme decides the colour under every widget, and no widget reports that in
/// its damage. The frame after a theme message is whole, and the frame after
/// that, with nothing else changed, is not: the repaint is the theme's.
///
/// A tree built between two frames — by the state tick that runs a window's
/// passes while its last frame is still owed — paints the next frame whole too:
/// the pixels on screen are the tree before it, and no widget's damage says so.
#[cfg(feature = "native")]
#[test]
fn a_theme_message_or_a_rebuild_between_frames_repaints_the_whole_surface() {
    use crate::floating_surface::FloatingSurfaceHostMessage;

    {
        let _settings = own_settings();
        let mut app = app();
        app.set_theme(Theme::light());
        // The window stands in the light theme, with a frame behind it.
        let _ = frame(&mut app);
        let _ = frame(&mut app);

        let appearance = tilo_ui_base::theme::WindowAppearance::default();
        let message =
            FloatingSurfaceHostMessage::appearance_with_theme(&appearance, &Theme::dark(), None);
        assert!(app.apply_floating_host_messages(vec![message]));
        let themed = frame(&mut app);
        assert!(
            themed.repainted().is_some(),
            "the frame after a theme message repaints every pixel"
        );

        // A theme made again with nothing else asked to rebuild repaints the
        // whole surface too: no widget reports that its colours changed.
        let _ = frame(&mut app);
        app.rebuild_theme();
        let remade = frame(&mut app);
        assert_eq!(
            remade.repainted(),
            Some("a new theme"),
            "a theme made again repaints every pixel"
        );

        let after = frame(&mut app);
        assert_ne!(
            after.repainted(),
            Some("a new theme"),
            "a frame with nothing else changed is not whole: the repaint was the theme's"
        );
    }

    {
        let _settings = own_settings();
        let mut app = app();
        app.set_theme(Theme::light());
        let _ = frame(&mut app);
        let _ = frame(&mut app);
        // What `service_native_idle_model` does between frames: the rebuild the
        // change asked for runs here, and the redraw that follows sees no
        // structure change left to report.
        app.layout.request_rebuild();
        app.rebuild_structure();
        let rebuilt = frame(&mut app);
        assert_eq!(
            rebuilt.repainted(),
            Some("a rebuilt tree"),
            "a frame after a rebuild between frames repaints every pixel"
        );
    }
}

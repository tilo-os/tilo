use super::*;
use crate::a11y::contrast_ratio;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;

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

struct AppearanceRoot {
    intent: crate::theme::AppearanceIntent,
}

impl Widget for AppearanceRoot {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        Some(self.intent.clone())
    }
    fn name(&self) -> &'static str {
        "AppearanceRoot"
    }
}

fn intent(
    theme: Theme,
    ui_scale: f32,
    text_scale: f32,
    high_contrast: bool,
) -> crate::theme::AppearanceIntent {
    crate::theme::AppearanceIntent {
        base_theme: theme,
        high_contrast,
        reduce_motion: false,
        text_scale,
        ui_scale,
    }
}

#[test]
fn host_appearance_rethemes_through_the_appearance_intent() {
    use crate::host_appearance::{HostAppearance, Scheme};
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
            crate::tokens::nearest_presence_hue(vello::peniko::Color::from_rgb8(0xE0, 0x40, 0x80))
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

    phase low_vision_setters_thread_onto_the_theme_and_survive_toggles;
    {
        let _settings = own_settings();
        {
            let a = app();
            assert!(!a.high_contrast());
            assert!(!a.reduce_motion());
            assert_eq!(a.text_scale(), 1.0);
            assert!(!a.theme.reduce_motion);
            assert_eq!(a.theme.text_scale, 1.0);
        }

        {
            let mut a = app();
            let plain_ratio =
                contrast_ratio(a.theme.text_primary, a.theme.tokens.colors.surfaces.content);

            let now_on = a.toggle_high_contrast();
            assert!(now_on);
            assert!(a.high_contrast());
            let hc_ratio = contrast_ratio(a.theme.text_primary, a.theme.tokens.colors.surfaces.content);
            assert!(
                hc_ratio > plain_ratio,
                "high contrast must raise fg/bg contrast ({hc_ratio:.1} vs {plain_ratio:.1})",
            );
            assert_eq!(a.theme.name, "High contrast");

            let now_off = a.toggle_high_contrast();
            assert!(!now_off);
            assert!(!a.high_contrast());
            let restored = contrast_ratio(a.theme.text_primary, a.theme.tokens.colors.surfaces.content);
            assert!(
                (restored - plain_ratio).abs() < 1e-6,
                "off restores base colours"
            );
        }

        {
            let mut a = app();
            let base = a.theme.tokens.typography.size(TextRole::Body);
            a.set_text_scale(2.0);
            assert_eq!(a.text_scale(), 2.0);
            assert!((a.theme.tokens.typography.size(TextRole::Body) - base * 2.0).abs() < 1e-3);
            // A scale change must re-run layout so the larger text doesn't clip.
            assert!(a.layout.layout_dirty);
        }

        {
            let mut a = app();
            a.set_text_scale(1.5);
            let hc_base = Theme::high_contrast()
                .tokens
                .typography
                .size(TextRole::Body);
            a.set_high_contrast(true);
            assert_eq!(
                a.text_scale(),
                1.5,
                "scale preference is preserved across the swap"
            );
            assert!((a.theme.tokens.typography.size(TextRole::Body) - hc_base * 1.5).abs() < 1e-3);
            assert_eq!(a.theme.name, "High contrast");
        }

        {
            let mut a = app();
            assert!(!a.theme.reduce_motion);
            a.set_reduce_motion(true);
            assert!(a.reduce_motion());
            assert!(
                a.theme.reduce_motion,
                "the flag reaches the theme widgets draw against"
            );
            a.set_reduce_motion(false);
            assert!(!a.theme.reduce_motion);
        }

        {
            let mut a = app();
            a.set_text_scale(1.25);
            a.set_reduce_motion(true);
            a.set_high_contrast(true);
            // Swap the *base* theme — the a11y layer must re-apply on top.
            a.set_theme(Theme::light());
            assert!(a.theme.reduce_motion);
            assert_eq!(a.text_scale(), 1.25);
            assert_eq!(a.theme.name, "High contrast light");
        }

        {
            let mut a = app();
            let base_gap = a.theme.tokens.spacing.s3;
            let base_radius = a.theme.tokens.radii.control;
            let base_type = a.theme.tokens.typography.size(TextRole::Body);
            a.set_ui_scale(1.5);
            assert_eq!(a.ui_scale(), 1.5);
            assert!(
                (a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-3,
                "spacing grows"
            );
            assert!(
                (a.theme.tokens.radii.control - base_radius * 1.5).abs() < 1e-3,
                "radii grow"
            );
            assert!(
                (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.5).abs() < 1e-3,
                "type grows"
            );
            // A UI-scale change must re-run layout so the whole interface re-flows.
            assert!(a.layout.layout_dirty);
        }

        {
            let mut a = app();
            let base_type = a.theme.tokens.typography.size(TextRole::Body);
            let base_gap = a.theme.tokens.spacing.s3;
            a.set_ui_scale(1.25);
            a.set_text_scale(1.2);
            assert_eq!(a.ui_scale(), 1.25);
            assert_eq!(a.text_scale(), 1.2);
            // Type picks up BOTH factors; spacing only the UI factor.
            assert!(
                (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.25 * 1.2).abs() < 1e-2
            );
            assert!((a.theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
        }

        {
            let mut a = app();
            a.set_ui_scale(1.5);
            let hc_gap = Theme::high_contrast().tokens.spacing.s3;
            a.set_high_contrast(true);
            assert_eq!(
                a.ui_scale(),
                1.5,
                "UI-scale preference is preserved across the swap"
            );
            assert!((a.theme.tokens.spacing.s3 - hc_gap * 1.5).abs() < 1e-2);
            assert_eq!(a.theme.name, "High contrast");
        }
    }

    phase appearance_reconcile_is_a_noop_when_off_and_applies_live_otherwise;
    {
        let _settings = own_settings();
        {
            let root = AppearanceRoot {
                intent: intent(Theme::light(), 1.5, 1.3, true),
            };
            let mut a = App::new(State::new(), Box::new(root));
            assert!(!a.reconcile_appearance());
            assert_eq!(a.theme.name, "Tilo dark");
            assert_eq!(a.ui_scale(), 1.0);
        }

        {
            let base_gap = Theme::high_contrast().tokens.spacing.s3;
            let base_type = Theme::high_contrast()
                .tokens
                .typography
                .size(TextRole::Body);
            // Light base + high_contrast=true → high-contrast track wins; ui 1.5, text 1.2.
            let root = AppearanceRoot {
                intent: intent(Theme::light(), 1.5, 1.2, true),
            };
            let mut a = App::new(State::new(), Box::new(root));
            a.appearance_reconcile = true;
            assert!(
                a.reconcile_appearance(),
                "first reconcile applies the intent"
            );
            assert_eq!(a.ui_scale(), 1.5);
            assert_eq!(a.text_scale(), 1.2);
            assert!(a.high_contrast());
            assert_eq!(
                a.theme.name, "High contrast light",
                "high_contrast overrides the Light base with its own polarity"
            );
            // Spacing grew by the UI factor; type by UI × text.
            assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
            assert!(
                (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.5 * 1.2).abs() < 1e-1
            );
            assert!(
                a.layout.layout_dirty,
                "a live appearance change re-runs layout"
            );
            assert!(
                !a.reconcile_appearance(),
                "unchanged intent does not re-theme"
            );
        }
    }

    phase the_shell_s_window_appearance_dresses_the_theme_and_survives_rebuilds;
    {
        use crate::theme::WindowAppearance;
        use crate::tokens::PresenceHue;
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
        let appearance = crate::theme::WindowAppearance::default();
        for theme in [Theme::light(), Theme::high_contrast(), Theme::dark()] {
            let message = FloatingSurfaceHostMessage::appearance_with_theme(&appearance, &theme);
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

        let appearance = crate::theme::WindowAppearance::default();
        let message =
            FloatingSurfaceHostMessage::appearance_with_theme(&appearance, &Theme::dark());
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

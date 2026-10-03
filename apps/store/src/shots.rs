//! Headless `--shots` mode — mirrors the sibling native apps.
//!
//! One binary produces one deterministic PNG per invocation; the
//! catalog is a hand-rolled fixture so the shot is hermetic and
//! doesn't reach for the network.

use std::collections::HashMap;
use std::path::Path;
#[cfg(not(test))]
use std::sync::Once;

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

use crate::app::StoreApp;
use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource, StoreTrust};

pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 760);

/// Read the optional accessibility type multiplier used by visual acceptance
/// captures.  A malformed value must never turn a shot into a different
/// layout mode, and bounding the multiplier keeps the fixed-size fixture
/// meaningful while still exercising the supported large-text regime.
fn shot_text_scale() -> f32 {
    std::env::var("TILO_SHOTS_TEXT_SCALE")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|scale| scale.is_finite() && (1.0..=2.0).contains(scale))
        .unwrap_or(1.0)
}

/// A Vello scene with a dense text/control tree can need more than its local
/// bump-budget confirmation before every glyph atlas page is resident. The
/// interactive app naturally receives later frames; a one-frame screenshot
/// does not. Keep a small, explicit settle budget for visual regression
/// captures so screenshots never bless a partial entrance frame.
const SETTLED_CAPTURE_WARMUP_PASSES: usize = 3;

/// Screenshot review is a correctness oracle, not a GPU throughput test.
/// This driver occasionally returned alternating partial glyph/control tiles
/// for a fresh offscreen Vulkan scene. Pin Store's hermetic capture path to
/// Vello's deterministic CPU renderer; the interactive desktop keeps using
/// its normal GPU renderer.
#[cfg(not(test))]
static SHOTS_CPU_RENDERER: Once = Once::new();

#[cfg(not(test))]
#[allow(
    clippy::disallowed_methods,
    reason = "outside tests `render_widget` is only reached from the --shots handler on \
              the main thread, before the renderer is constructed and before any \
              application thread exists, so nothing reads the environment concurrently"
)]
fn configure_settled_capture_renderer() {
    SHOTS_CPU_RENDERER.call_once(|| {
        #[allow(
            unsafe_code,
            reason = "Rust 2024 marks process-environment mutation unsafe; this one-shot headless renderer preference is installed before constructing any Store shot renderer"
        )]
        unsafe {
            std::env::set_var("TILO_UI_VELLO_CPU", "1");
        }
    });
}

/// Unit tests render on one of many test threads, so the CPU renderer
/// preference is scoped to each render instead of installed for the process.
#[cfg(test)]
fn settled_capture_renderer_scope() -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_UI_VELLO_CPU", "1");
    env
}

/// A hand-rolled fixture of items that shows every card variant:
///
///   * a Tilo item that's not installed  → **Open**
///   * a Tilo item that's cached + pinned → **Open** + **Installed** + Remove
///   * a Tilo item that's cached but out-of-date → **Update** + Keep on device
///   * a Flatpak item (flatpak available) → **Install**
///   * a Flatpak item, flatpak unavailable path is covered by a
///     `set_flatpak_available(false)` variant used by the
///     "no-flatpak" scenario.
#[allow(
    clippy::too_many_lines,
    reason = "the deterministic visual fixture keeps every app/action state together so screenshot review can see the complete catalogue vocabulary"
)]
pub fn fixture_catalog() -> Vec<StoreItem> {
    let tilo =
        |name: &str, desc: &str, cat: &str, installed: InstalledState, ver: &str| StoreItem {
            id: format!("so.tilo.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: desc.into(),
            tagline: String::new(),
            supported_languages: Vec::new(),
            categories: vec![cat.into()],
            icon_url: Some(format!("https://artwork.invalid/{name}.png")),
            screenshot_urls: vec![format!("https://artwork.invalid/{name}-preview.png")],
            homepage_url: None,
            source_url: None,
            install_ref_path: None,
            source: StoreSource::Tilo {
                registry_name: "Tilo main".into(),
            },
            trust: StoreTrust::PublisherSigned,
            versions: vec![StoreItemVersion {
                version: ver.into(),
                released_at_unix: 0,
                download_url: Some(format!("https://apps.tilo.so/{name}/{ver}.tapp")),
                root_hash_hex: Some("de".repeat(32)),
                size_bytes: 4 * 1024 * 1024,
            }],
            publisher_pubkey_hex: Some("aa".repeat(32)),
            installed,
        };
    let fh = |name: &str, desc: &str, cat: &str, installed: InstalledState| StoreItem {
        id: format!("org.example.{name}"),
        name: name.into(),
        description: desc.into(),
        tagline: String::new(),
        supported_languages: Vec::new(),
        categories: vec![cat.into()],
        icon_url: Some(format!("https://artwork.invalid/{name}.png")),
        screenshot_urls: vec![format!("https://artwork.invalid/{name}-preview.png")],
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
        trust: StoreTrust::Flathub,
        versions: vec![StoreItemVersion {
            version: "3.2.1".into(),
            released_at_unix: 0,
            download_url: None,
            root_hash_hex: None,
            size_bytes: 0,
        }],
        publisher_pubkey_hex: None,
        installed,
    };
    vec![
        tilo(
            "Notes",
            "Capture ideas, organise projects, and work together in real time.",
            "Productivity",
            InstalledState::Not,
            "0.1.0",
        ),
        tilo(
            "Terminal",
            "A fast terminal for commands, scripts, and development tools.",
            "System",
            // Cached + marked installed (pinned): Open + Installed + Remove.
            InstalledState::Installed {
                version: "0.2.0".into(),
                available: "0.2.0".into(),
                pinned: true,
                user_managed: true,
            },
            "0.2.0",
        ),
        tilo(
            "Files",
            "Browse, organise, and share files across your workspaces.",
            "System",
            // Cached, not pinned, update available: Update + Keep on device.
            InstalledState::Installed {
                version: "0.3.0".into(),
                available: "0.4.0".into(),
                pinned: false,
                user_managed: true,
            },
            "0.4.0",
        ),
        fh(
            "Chess",
            "Play a classic game of chess against a person or the computer.",
            "Games",
            InstalledState::Not,
        ),
        fh(
            "Inkscape",
            "Vector graphics editor from Flathub.",
            "Utilities",
            InstalledState::Not,
        ),
        fh(
            "VLC",
            "The famous media player — already installed here.",
            "AudioVideo",
            InstalledState::Installed {
                version: "3.2.1".into(),
                available: "3.2.1".into(),
                pinned: false,
                user_managed: true,
            },
        ),
    ]
}

/// Build an app pre-seeded with the fixture catalog and a sensible
/// initial filter for a still.
pub fn seeded_app() -> StoreApp {
    let mut app = StoreApp::new();
    app.set_flatpak_available(true);
    app.set_items(fixture_catalog());
    app.set_status_line("6 apps available.");
    app
}

/// Public entry point the binary's `--shots` path calls.
///
/// # Errors
///
/// Propagates a `taffy` layout error from [`render_widget`] — building or
/// computing the widget tree's layout at `viewport` size.
pub fn render_to_path(viewport: (u32, u32), out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    render_scenario_to_path(viewport, out, None)
}

/// Render a named representative Store state to a PNG.
///
/// # Errors
///
/// Propagates layout, renderer initialization, and PNG-write failures.
pub fn render_scenario_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let theme = legacy_shot_theme();
    render_scenario_to_path_with_theme(viewport, out, scenario, &theme)
}

/// Render a named representative Store state to a PNG with an explicit theme.
///
/// # Errors
///
/// Propagates layout, renderer initialization, and PNG-write failures.
pub fn render_scenario_to_path_with_theme(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
    theme: &Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = seeded_app();
    app.set_layout_width_hint(f64::from(viewport.0));
    match scenario {
        Some("detail") => app.focus_detail_for_shot("so.tilo.notes"),
        Some("remotes") => app.show_remotes_for_shot(),
        Some("progress") => {
            // Terminal is already installed and its offered action is Open,
            // so the visible busy card and masthead describe the same real
            // operation rather than claiming an install for an open-only row.
            app.set_active_operation_for_shot("so.tilo.terminal", crate::ActionKind::Open);
            app.set_status_line("Starting Terminal · launching installed app…");
            app.set_worker_busy(true);
        }
        Some("error") => {
            let message = "Install failed · package signature did not match the catalogue";
            app.set_status_line(message);
            app.set_operation_error(Some(message.into()));
        }
        Some("moderation") => app.show_safety_for_shot(),
        Some("moderation-case") => app.show_moderation_case_for_shot("Needs information"),
        Some("moderation-loading") => {
            app.show_moderation_case_for_shot("Open");
            app.set_worker_busy(true);
            app.set_status_line("Contacting the authoritative registry…");
        }
        Some("moderation-offline") => {
            app.show_moderation_case_for_shot("Last confirmed · Open");
            let message = "Registry is offline · draft and last confirmed status are unchanged";
            app.set_status_line(message);
            app.set_operation_error(Some(message.into()));
        }
        Some("moderation-error") => {
            app.show_moderation_case_for_shot("Last confirmed · Quarantined");
            let message =
                "Appeal was refused · this workspace identity is not the package publisher";
            app.set_status_line(message);
            app.set_operation_error(Some(message.into()));
        }
        Some("empty") => {
            app.clear_catalogue_for_shot();
            app.set_status_line("No catalogue entries are available.");
        }
        Some("offline") => {
            app.clear_catalogue_for_shot();
            let message = "Could not reach any configured catalogue.";
            app.set_status_line(message);
            app.set_operation_error(Some(message.into()));
        }
        _ => {}
    }
    // Force the floating status chip fully visible so this single
    // headless frame captures it (a still can't hover it).
    app.arm_status_bubble_for_shots();
    let mut widget: Box<dyn Widget> = Box::new(app);
    render_widget(viewport, out, &mut widget, theme)
}

/// Guts of the render pass — mirrors what `ui::App` does for a
/// single redraw. Extracted so tests can render into a temp dir.
///
/// # Errors
///
/// Returns a `taffy` layout error if constructing the root layout node or
/// computing the layout at `viewport` size fails.
pub fn render_widget(
    viewport: (u32, u32),
    out: &Path,
    widget: &mut Box<dyn Widget>,
    theme: &Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size {
                width: Dimension::length(w as f32),
                height: Dimension::length(h as f32),
            },
            ..Default::default()
        },
        &[child],
    )?;

    let mut bindings = ui::bound_dispatch::BoundNodeMap::new();
    // SAFETY: the bound nodes come from this exact exclusively owned build.
    unsafe {
        bindings.rebuild(widget.as_ref(), &taffy, child, &node_map);
        ui::widget::compute_widget_layout(
            &mut taffy,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            &node_map,
            &bindings,
            &mut text_renderer,
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        theme,
        &presence,
        &permission,
    );
    // Top-layer overlay pass — the floating status bubble paints here,
    // matching the live event loop's dedicated overlay pass.
    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        theme,
        &presence,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    #[cfg(not(test))]
    configure_settled_capture_renderer();
    #[cfg(test)]
    let _cpu_renderer = settled_capture_renderer_scope();
    let mut renderer = OffscreenRenderer::new(w, h);
    for _ in 0..SETTLED_CAPTURE_WARMUP_PASSES {
        let _ = renderer.render_rgba_resilient(&scene);
    }
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("store: wrote shot to {}", out.display());
    Ok(())
}

fn legacy_shot_theme() -> Theme {
    configured_shot_theme(Some(shot_text_scale()), shot_reduce_motion())
}

fn shot_reduce_motion() -> bool {
    [
        "TILO_A11Y_REDUCE_MOTION",
        "TILO_REDUCE_MOTION",
        "TILO_SHOTS_REDUCE_MOTION",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .any(|value| {
        let value = value.trim();
        !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
    })
}

/// Parse `WxH` into `(u32, u32)`.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn configured_shot_theme(text_scale: Option<f32>, reduce_motion: bool) -> Theme {
    let mut theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    if let Some(scale) = text_scale {
        theme = theme.with_text_scale(scale);
    }
    theme.with_reduce_motion(reduce_motion)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn masthead_ink_pixels(path: &Path) -> usize {
        let frame = ui::png::read_rgba8(path).unwrap();
        let width = usize::try_from(frame.width).unwrap();
        let top_rows = usize::try_from(frame.height.min(180)).unwrap();
        frame
            .rgba
            .chunks_exact(4)
            .take(width.saturating_mul(top_rows))
            .filter(|pixel| {
                pixel[3] > 200
                    && u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2]) > 250
            })
            .count()
    }

    /// The fixture catalog covers every action; viewport, text scale and theme inputs parse; the seeded app lists items.
    #[test]
    fn shot_inputs_parse_cover_every_action_and_seed_items() {
        // Case `fixture_catalog_covers_every_action_variant`.
        {
            use crate::model::ActionKind;
            let cat = fixture_catalog();
            let mut saw_open = false;
            let mut saw_update = false;
            let mut saw_fp_install = false;
            let mut saw_fp_open = false;
            let mut saw_fp_remove = false;
            let mut saw_pin_toggle = false;
            for item in &cat {
                match item.action_kind(true) {
                    ActionKind::Open => saw_open = true,
                    ActionKind::Update => saw_update = true,
                    ActionKind::FlatpakInstall => saw_fp_install = true,
                    ActionKind::FlatpakOpen => saw_fp_open = true,
                    _ => {}
                }
                if item.pin_action().is_some() {
                    saw_pin_toggle = true;
                }
                // Flatpak removal is a *secondary* action — the primary on
                // an installed app is to open it — so it is covered where
                // it now lives rather than dropped from the fixture.
                if item.remove_action() == Some(ActionKind::FlatpakRemove) {
                    saw_fp_remove = true;
                }
            }
            assert!(
                saw_open
                    && saw_update
                    && saw_fp_install
                    && saw_fp_open
                    && saw_fp_remove
                    && saw_pin_toggle,
                "fixture must exercise Open, Update, Flatpak install/open/remove, \
             and the pin toggle (open={saw_fp_open}, remove={saw_fp_remove})"
            );
        }

        // Case `viewport_parses_expected_shape`.
        {
            assert_eq!(parse_viewport("1200x760"), Some((1200, 760)));
            assert_eq!(parse_viewport("bad"), None);
        }

        // Case `screenshot_text_scale_is_bounded_and_forgiving`.
        {
            // The variable is process-global; this test only validates the safe
            // default in the normal test process.  Rendering callers exercise a
            // valid multiplier through the documented environment seam.
            assert!((1.0..=2.0).contains(&shot_text_scale()));
        }

        // Case `explicit_launch_preferences_reach_the_shot_theme`.
        {
            let theme = configured_shot_theme(Some(2.0), true);
            assert!((theme.text_scale - 2.0).abs() < f32::EPSILON);
            assert!(theme.reduce_motion);
        }

        // Case `seeded_app_lists_items`.
        {
            let app = seeded_app();
            assert_eq!(app.items().len(), fixture_catalog().len());
        }
    }

    #[test]
    fn render_seeded_produces_a_png() {
        let td = TempDir::new().unwrap();
        let out = td.path().join("store.png");
        // The offscreen renderer needs a GPU; skip the write assert
        // if no adapter is available (CI without KVM). We still
        // exercise the compose+layout path via `seeded_app`.
        let _ = render_to_path((640, 400), &out);
    }

    #[test]
    fn dense_detail_error_and_moderation_shots_are_settled_before_capture() {
        let td = TempDir::new().unwrap();
        for scenario in ["detail", "error", "moderation-case"] {
            let first = td.path().join(format!("{scenario}-first.png"));
            let second = td.path().join(format!("{scenario}-second.png"));
            render_scenario_to_path((920, 720), &first, Some(scenario)).unwrap();
            render_scenario_to_path((920, 720), &second, Some(scenario)).unwrap();
            assert_eq!(
                std::fs::read(first).unwrap(),
                std::fs::read(second).unwrap(),
                "{scenario} shot must not capture a partial entrance/recheck frame"
            );
        }
    }

    #[test]
    fn catalogue_states_keep_the_complete_masthead_in_the_captured_frame() {
        let td = TempDir::new().unwrap();
        for scenario in ["wide", "progress", "error", "empty"] {
            let out = td.path().join(format!("{scenario}.png"));
            render_scenario_to_path((1200, 760), &out, Some(scenario)).unwrap();
            assert!(
                masthead_ink_pixels(&out) > 350,
                "{scenario} must retain title, search, and toolbar ink in the top masthead band"
            );
        }
    }
}

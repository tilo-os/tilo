//! Headless `--shots` mode — renders one offscreen frame of the
//! `SettingsApp` to a PNG and exits. Mirrors the sibling native apps so the
//! snapshot harness stays consistent.
//!
//! Scenarios are derived from the output filename so a single binary produces
//! every required shot:
//! - `settings.png` — the wide two-pane layout, opened on a rich panel
//!   (Appearance), so the selected sidebar row and a populated content
//!   pane are both visible.
//! - `settings-narrow.png` — the narrow collapse default: the selected panel
//!   full-screen with a back-arrow affordance (a collapse keeps the content,
//!   never the category list).
//! - `settings-narrow-list.png` — the narrow list screen: the full-screen
//!   category list with the search field on top (the back-tap-away state).
//! - `settings-narrow-content.png` — the narrow content screen: the selected
//!   panel full-screen with the back-arrow affordance.

use std::collections::HashMap;
use std::path::Path;

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

use crate::SettingsApp;
use crate::categories::Category;
use crate::settings_app::NAV_BREAKPOINT;

/// Default (wide) offscreen viewport. Matches the sibling apps' aspect.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 760);

/// Narrow viewport used for the responsive single-pane shot — phone-ish.
pub const NARROW_SHOTS_VIEWPORT: (u32, u32) = (440, 820);

/// The screenshot scenarios, derived from the output filename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// Wide two-pane, opened on Appearance.
    Wide,
    /// Narrow single-pane collapse default — the content screen for Region &
    /// Formats, full-screen with the back arrow (never the category list).
    Narrow,
}

impl Scenario {
    /// Derive the scenario from the `--shots` output path: any filename
    /// containing `narrow` selects the narrow scenario.
    pub fn from_shots_path(path: &Path) -> Scenario {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("narrow") {
            Scenario::Narrow
        } else {
            Scenario::Wide
        }
    }

    /// The viewport for this scenario.
    pub fn viewport(self) -> (u32, u32) {
        match self {
            Scenario::Wide => DEFAULT_SHOTS_VIEWPORT,
            Scenario::Narrow => NARROW_SHOTS_VIEWPORT,
        }
    }

    /// The app pre-configured for this scenario.
    pub fn app(self) -> SettingsApp {
        match self {
            // Wide: docked sidebar with a rich, visible category active so the
            // still proves selection drives the pane without placing the
            // selected row below the initial sidebar viewport.
            Scenario::Wide => SettingsApp::with_selected(Category::Appearance)
                .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32)
                .with_deterministic_hardware_for_shot(),
            // Narrow: the collapse default — Region & Formats' content
            // full-screen with a back arrow to the list. A collapse keeps the
            // content, never the category list.
            Scenario::Narrow => SettingsApp::with_selected(Category::RegionFormats)
                .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
                .with_deterministic_hardware_for_shot(),
        }
    }
}

/// Map a category slug to its [`Category`], for the per-panel capture
/// driven by the `TILO_SHOTS_PANEL` env var.
fn category_from_slug(slug: &str) -> Option<Category> {
    Category::ALL.iter().copied().find(|c| c.slug() == slug)
}

/// A `NetworkPane` populated with a deterministic scan list, a connected
/// wired link, and a secure network's password prompt pre-expanded — the
/// state the connect flow reaches after clicking "Connect" on a locked
/// network. Used by the `netdemo` capture.
fn network_demo_pane() -> Box<dyn Widget> {
    use os_actions::{WifiNetworkInfo, WiredInfo};
    let nets = vec![
        WifiNetworkInfo {
            ssid: "Home_Network".into(),
            signal: 4,
            secure: true,
            security: os_actions::WifiSecurity::Wpa3,
            connected: true,
        },
        WifiNetworkInfo {
            ssid: "Cafe_Guest".into(),
            signal: 3,
            secure: false,
            security: os_actions::WifiSecurity::Open,
            connected: false,
        },
        WifiNetworkInfo {
            ssid: "Office_5G".into(),
            signal: 2,
            secure: true,
            security: os_actions::WifiSecurity::Wpa2,
            connected: false,
        },
    ];
    let pane = crate::panes::network::NetworkPane::new_demo(nets)
        .with_wired(WiredInfo {
            connected: true,
            device: Some("enp3s0".into()),
            speed: Some("1000 Mb/s".into()),
            ipv4: Some("192.168.1.42".into()),
        })
        .with_expanded("Office_5G");
    Box::new(pane)
}

/// An `AudioPane` seeded with a deterministic `PipeWire` graph: two output
/// devices (Speakers + Headphones), and per-application streams — the music app
/// routed to the Headphones while the browser stays on the Speakers, plus a
/// meeting app on the microphone — so the still proves the per-application
/// routing surface (a picker, volume, and mute per app). Sample data lives only
/// behind `--shots`; the live pane enumerates the real graph.
fn audio_demo_pane() -> Box<dyn Widget> {
    use crate::panes::audio::{AudioDevice, AudioDevices, AudioPane, RealDevice, RealInput};
    use media::audio::{AudioDevice as GDevice, AudioGraph, AudioStream, NoopGraphControl};
    use std::sync::Arc;

    let sink = |id, serial, name: &str, def| GDevice {
        id,
        serial,
        name: name.to_string(),
        description: name.to_string(),
        is_default: def,
        volume: Some(0.7),
        muted: Some(false),
        channels: 2,
        codec: None,
    };
    let stream = |id, serial, app: &str, media: &str, target, playback, vol, mute| AudioStream {
        id,
        serial,
        app: app.to_string(),
        media_name: Some(media.to_string()),
        playback,
        target_id: Some(target),
        volume: Some(vol),
        muted: Some(mute),
    };
    let graph = AudioGraph {
        sinks: vec![
            sink(51, 51, "Speakers", true),
            sink(91, 269, "Headphones", false),
        ],
        sources: vec![sink(52, 52, "Built-in Microphone", true)],
        playback_streams: vec![
            // Routed away from the default onto the Headphones.
            stream(84, 300, "Music", "Nightcall", 91, true, 0.85, false),
            // Following the default (Speakers).
            stream(90, 320, "Browser", "Conference call", 51, true, 0.60, false),
        ],
        record_streams: vec![stream(
            95,
            340,
            "Meeting",
            "Microphone",
            52,
            false,
            0.75,
            false,
        )],
        hardware: vec![],
    };
    // The device pickers read the local device view; give it the same two sinks
    // so the output-device chooser is shown alongside the per-app rows.
    let devices = AudioDevices {
        sinks: vec![
            AudioDevice {
                id: 51,
                name: "Speakers".into(),
                default: true,
            },
            AudioDevice {
                id: 91,
                name: "Headphones".into(),
                default: false,
            },
        ],
        sources: vec![AudioDevice {
            id: 52,
            name: "Built-in Microphone".into(),
            default: true,
        }],
    };
    let disp = crate::dispatcher::dispatcher_or_real(None);
    let pane = AudioPane::with_controls(disp, Arc::new(RealInput), 0.65, 0.7)
        .with_devices(devices, Arc::new(RealDevice))
        .with_graph(graph, Arc::new(NoopGraphControl));
    Box::new(pane)
}

/// Render the scenario selected by `out`'s filename into a PNG at `out`.
///
/// When the `TILO_SHOTS_PANEL` env var is set to a category slug, the wide
/// two-pane layout is rendered with that category selected instead of the
/// default scenario — this is how the verification harness captures every
/// panel from one binary.
///
/// # Errors
///
/// The `Result` return type matches the `--shots` binary's `main` signature;
/// every path through this function currently always returns `Ok(())`.
pub fn render_for_path(out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    render_for_path_with_text_scale(out, None)
}

/// Render a named acceptance scenario with an optional accessibility text
/// scale. Settings has an app-specific shot pipeline, so it must explicitly
/// carry the standard launcher's `--text-scale` through rather than silently
/// dropping it as the old custom renderer did.
///
/// # Errors
///
/// Propagates an offscreen renderer or PNG-write failure.
#[allow(
    clippy::too_many_lines,
    reason = "the filename-routed acceptance scenarios remain auditable as one exhaustive dispatch table"
)]
pub fn render_for_path_with_text_scale(
    out: &Path,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let fname = out
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // `…narrow-list.png` proves the narrow list screen: the full-screen
    // category list (the back-tap-away state a collapse no longer defaults to).
    if fname.contains("narrow") && fname.contains("list") {
        let app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
            .with_deterministic_hardware_for_shot()
            .with_narrow_list_shot();
        return render_to_path_with_text_scale(
            Box::new(app),
            NARROW_SHOTS_VIEWPORT,
            out,
            text_scale,
        );
    }

    // `…narrow-content.png` proves the narrow content screen: the selected
    // panel full-screen with the back-arrow affordance in the top-left.
    if fname.contains("narrow") && fname.contains("content") {
        let app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
            .with_deterministic_hardware_for_shot()
            .with_narrow_content_shot();
        return render_to_path_with_text_scale(
            Box::new(app),
            NARROW_SHOTS_VIEWPORT,
            out,
            text_scale,
        );
    }

    // `…search.png` proves the sidebar search filters the category list to the
    // matching panes. Seeded with a query that spans two categories.
    if fname.contains("search") {
        // Keep this visual acceptance path independent from a live display
        // connector snapshot. Display hardware is intentionally discovered
        // at runtime and may include a transient compositor hand-off while a
        // headless renderer is starting; Appearance still gives this capture
        // a rich, deterministic content pane while the sidebar proves the
        // filtered navigation state.
        let app = SettingsApp::with_selected(Category::Appearance)
            .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32)
            .with_deterministic_hardware_for_shot()
            .with_search_shot("light");
        return render_to_path_with_text_scale(
            Box::new(app),
            DEFAULT_SHOTS_VIEWPORT,
            out,
            text_scale,
        );
    }

    // `…netdemo.png` renders the Network pane with a populated scan list and
    // an open inline password prompt, proving the connect flow's surface.
    if fname.contains("netdemo") {
        return render_to_path_with_text_scale(
            network_demo_pane(),
            DEFAULT_SHOTS_VIEWPORT,
            out,
            text_scale,
        );
    }

    // `…audiodemo.png` renders the Sound pane with two output devices and
    // per-application streams — the music app routed to headphones while the
    // browser stays on speakers — proving the per-app routing surface.
    if fname.contains("audiodemo") {
        return render_to_path_with_text_scale(
            audio_demo_pane(),
            DEFAULT_SHOTS_VIEWPORT,
            out,
            text_scale,
        );
    }

    // A populated External Accounts acceptance surface. Unlike the live pane,
    // which must honestly show an empty inventory on a fresh install, this
    // deterministic capture exercises row hierarchy, status, and maintenance
    // actions at both responsive widths without touching the user's stores.
    if fname.contains("accountdemo") || fname.contains("accountform") {
        let viewport = if fname.contains("narrow") {
            NARROW_SHOTS_VIEWPORT
        } else {
            DEFAULT_SHOTS_VIEWPORT
        };
        let mut app = SettingsApp::with_selected(Category::ExternalAccounts)
            .with_width_hint(viewport.0 as f32)
            .with_deterministic_hardware_for_shot()
            .with_account_directory_for_shot(crate::accounts::sample_directory());
        if fname.contains("accountform") {
            app = app.with_account_form_for_shot(crate::accounts::AccountKind::Nextcloud);
        }
        if fname.contains("narrow") {
            app = app.with_narrow_content_shot();
        }
        return render_to_path_with_text_scale(Box::new(app), viewport, out, text_scale);
    }

    // Per-panel capture: a `…panel-<slug>.png` filename (or the
    // `TILO_SHOTS_PANEL` env var) renders the wide layout on that category.
    let panel_slug = std::env::var("TILO_SHOTS_PANEL").ok().or_else(|| {
        let name = out.file_name().and_then(|s| s.to_str()).unwrap_or("");
        name.rsplit_once("panel-")
            .map(|(_, rest)| rest.trim_end_matches(".png").to_string())
    });
    if let Some(slug) = panel_slug {
        // A `narrow` token in the filename captures the panel in the narrow
        // content screen (the pane full-screen with the back arrow); otherwise
        // the wide two-pane layout.
        let slug = slug.trim().trim_end_matches("-narrow");
        if let Some(cat) = category_from_slug(slug) {
            if fname.contains("narrow") {
                let app = SettingsApp::with_selected(cat)
                    .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
                    .with_deterministic_hardware_for_shot()
                    .with_narrow_content_shot();
                return render_to_path_with_text_scale(
                    Box::new(app),
                    NARROW_SHOTS_VIEWPORT,
                    out,
                    text_scale,
                );
            }
            let app = SettingsApp::with_selected(cat)
                .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32)
                .with_deterministic_hardware_for_shot();
            return render_to_path_with_text_scale(
                Box::new(app),
                DEFAULT_SHOTS_VIEWPORT,
                out,
                text_scale,
            );
        }
    }
    let scenario = Scenario::from_shots_path(out);
    render_to_path_with_text_scale(
        Box::new(scenario.app()),
        scenario.viewport(),
        out,
        text_scale,
    )
}

/// Render the ordinary Settings root at an explicitly requested acceptance
/// viewport. Unlike named documentation scenarios, responsive-matrix captures
/// must preserve the caller's exact dimensions and let the real shell
/// breakpoint select docked versus drill-in navigation.
///
/// # Errors
///
/// Returns the offscreen renderer or PNG write error.
pub fn render_responsive_to_path(
    viewport: (u32, u32),
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    render_responsive_to_path_with_text_scale(viewport, out, None)
}

/// Render an exact responsive viewport while preserving the requested text
/// scale, just as the normal launcher path does.
///
/// # Errors
///
/// Propagates an offscreen renderer or PNG-write failure.
pub fn render_responsive_to_path_with_text_scale(
    viewport: (u32, u32),
    out: &Path,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = responsive_app(viewport, text_scale.unwrap_or(1.0));
    render_to_path_with_text_scale(Box::new(app), viewport, out, text_scale)
}

/// The scenario a responsive capture documents at `viewport` under
/// `text_scale`: the narrow Region formats screen below the shell's
/// breakpoint, the docked External Accounts page at or above it. The
/// breakpoint grows with the text scale exactly as the shell's does, so the
/// scenario and the layout class the shell resolves always agree.
fn responsive_app(viewport: (u32, u32), text_scale: f32) -> SettingsApp {
    if f64::from(viewport.0) < NAV_BREAKPOINT * f64::from(text_scale.max(1.0)) {
        SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(viewport.0 as f32)
            .with_deterministic_hardware_for_shot()
            .with_narrow_content_shot()
    } else {
        SettingsApp::with_selected(Category::Appearance)
            .with_width_hint(viewport.0 as f32)
            .with_deterministic_hardware_for_shot()
    }
}

/// Build the `Scene` for `widget` at `viewport` size. Mirrors the
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle
/// `ui::App` runs for a single redraw, but stops at the CPU-side `Scene` so a
/// caller can rasterise many scenes through **one** reused
/// [`OffscreenRenderer`] (which amortises the expensive vello shader
/// compilation over every pane instead of paying it per process).
pub fn build_widget_scene(widget: Box<dyn Widget>, viewport: (u32, u32)) -> Scene {
    build_widget_scene_with_text_scale(widget, viewport, None)
}

/// Build a scene with an optional accessibility type multiplier. Kept separate
/// from the viewport so responsive acceptance captures can prove both layout
/// mode and large-text behaviour at their exact requested dimensions.
pub fn build_widget_scene_with_text_scale(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    text_scale: Option<f32>,
) -> Scene {
    let (w, h) = viewport;
    // `TILO_SHOTS_THEME=<name>` renders under any named theme (e.g. `dracula`,
    // `gruvbox`, `catppuccin-mocha`, `light`, `high-contrast`); defaults to dark.
    let theme = std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|n| Theme::named(&n))
        .unwrap_or_else(Theme::dark)
        .with_text_scale(text_scale.unwrap_or(1.0))
        .with_reduce_motion(matches!(
            std::env::var("TILO_SHOTS_REDUCE_MOTION").ok().as_deref(),
            Some("1" | "true" | "yes" | "on")
        ));
    widget.set_motion_reduced(theme.reduce_motion);
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    // The live host (`ui::App`), the shared launcher and the headless
    // harness all hand the theme's text scale to the renderer, so text the
    // app paints itself through the renderer grows with the preference. A
    // capture that left the renderer at 1× would under-report every
    // custom-drawn label (the sidebar rows, for one) at 200 %.
    text_renderer.set_text_scale(theme.text_scale);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    widget.sync(&mut state.doc, &state.root);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap_or(child);
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        // Ignore the (never-hit-in-practice) error: leaving the layout cache
        // stale is safe, since every reader below tolerates a missing entry.
        let _ = taffy.compute_layout_with_measure(
            root,
            taffy::Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);
    widget.sync(&mut state.doc, &state.root);

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    scene
}

/// Render `widget` at `viewport` size into a PNG at `out`. Mirrors the
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle
/// `ui::App` runs for a single redraw.
///
/// # Errors
///
/// The `Result` return type matches the `--shots` binary's `main` signature;
/// every path through this function currently always returns `Ok(())`.
pub fn render_to_path(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    render_to_path_with_text_scale(widget, viewport, out, None)
}

/// Render a widget with an optional accessibility text multiplier.
///
/// # Errors
///
/// Propagates an offscreen renderer or PNG-write failure.
pub fn render_to_path_with_text_scale(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &Path,
    text_scale: Option<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let scene = build_widget_scene_with_text_scale(widget, viewport, text_scale);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("settings: wrote shot to {}", out.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shot scenarios derive from the file name, and each scenario (and each
    /// exact capture width) opens the navigation mode and pane it shows.
    #[test]
    fn shot_scenarios_select_their_layout_and_pane() {
        for (path, want) in [
            ("/x/settings-narrow.png", Scenario::Narrow),
            ("/x/settings.png", Scenario::Wide),
        ] {
            assert_eq!(Scenario::from_shots_path(Path::new(path)), want, "{path}");
        }

        // The width hint + shot flag are applied; the resolved narrow/overlay
        // layout is asserted in the layout-driven tests once a frame is laid
        // out (see settings_app tests). Each case gets its own config dir.
        for (scenario, want) in [
            (Scenario::Narrow, Category::RegionFormats),
            (Scenario::Wide, Category::Appearance),
        ] {
            let _config = crate::prefs::test_env::ConfigGuard::new();
            assert_eq!(scenario.app().selected(), want, "{scenario:?}");
        }

        // (capture size, narrow, selected pane)
        for (size, narrow, want) in [
            ((390, 844), true, Category::RegionFormats),
            ((1920, 1080), false, Category::Appearance),
        ] {
            let _config = crate::prefs::test_env::ConfigGuard::new();
            let app = responsive_app(size, 1.0);
            assert_eq!(app.is_narrow(), narrow, "{size:?}");
            assert_eq!(app.selected(), want, "{size:?}");
        }
    }
}

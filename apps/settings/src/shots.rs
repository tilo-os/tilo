//! Headless `--shots` mode — renders one offscreen frame of the
//! `SettingsApp` to a PNG and exits. Mirrors the sibling native apps so the
//! snapshot harness stays consistent.
//!
//! Scenarios are derived from the output filename so a single binary produces
//! every required shot:
//! - `settings.png` — the wide two-pane layout, opened on a rich panel
//!   (External Accounts), so the category sidebar and a populated content
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

/// Default (wide) offscreen viewport. Matches the sibling apps' aspect.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 760);

/// Narrow viewport used for the responsive single-pane shot — phone-ish.
pub const NARROW_SHOTS_VIEWPORT: (u32, u32) = (440, 820);

/// The screenshot scenarios, derived from the output filename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// Wide two-pane, opened on External Accounts.
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
            // Wide: docked sidebar with a NON-default category (External
            // Accounts) active — the state the nav reaches after a click — so
            // the still proves selection drives the content pane.
            Scenario::Wide => SettingsApp::with_selected(Category::ExternalAccounts)
                .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32),
            // Narrow: the collapse default — Region & Formats' content
            // full-screen with a back arrow to the list. A collapse keeps the
            // content, never the category list.
            Scenario::Narrow => SettingsApp::with_selected(Category::RegionFormats)
                .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32),
        }
    }
}

/// Map a category slug to its [`Category`], for the per-panel capture
/// driven by the `QOS_SHOTS_PANEL` env var.
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
            connected: true,
        },
        WifiNetworkInfo {
            ssid: "Cafe_Guest".into(),
            signal: 3,
            secure: false,
            connected: false,
        },
        WifiNetworkInfo {
            ssid: "Office_5G".into(),
            signal: 2,
            secure: true,
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
/// When the `QOS_SHOTS_PANEL` env var is set to a category slug, the wide
/// two-pane layout is rendered with that category selected instead of the
/// default scenario — this is how the verification harness captures every
/// panel from one binary.
pub fn render_for_path(out: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
            .with_narrow_list_shot();
        return render_to_path(Box::new(app), NARROW_SHOTS_VIEWPORT, out);
    }

    // `…narrow-content.png` proves the narrow content screen: the selected
    // panel full-screen with the back-arrow affordance in the top-left.
    if fname.contains("narrow") && fname.contains("content") {
        let app = SettingsApp::with_selected(Category::RegionFormats)
            .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
            .with_narrow_content_shot();
        return render_to_path(Box::new(app), NARROW_SHOTS_VIEWPORT, out);
    }

    // `…search.png` proves the sidebar search filters the category list to the
    // matching panes. Seeded with a query that spans two categories.
    if fname.contains("search") {
        let app = SettingsApp::with_selected(Category::Displays)
            .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32)
            .with_search_shot("light");
        return render_to_path(Box::new(app), DEFAULT_SHOTS_VIEWPORT, out);
    }

    // `…netdemo.png` renders the Network pane with a populated scan list and
    // an open inline password prompt, proving the connect flow's surface.
    if fname.contains("netdemo") {
        return render_to_path(network_demo_pane(), DEFAULT_SHOTS_VIEWPORT, out);
    }

    // `…audiodemo.png` renders the Sound pane with two output devices and
    // per-application streams — the music app routed to headphones while the
    // browser stays on speakers — proving the per-app routing surface.
    if fname.contains("audiodemo") {
        return render_to_path(audio_demo_pane(), DEFAULT_SHOTS_VIEWPORT, out);
    }

    // Per-panel capture: a `…panel-<slug>.png` filename (or the
    // `QOS_SHOTS_PANEL` env var) renders the wide layout on that category.
    let panel_slug = std::env::var("QOS_SHOTS_PANEL").ok().or_else(|| {
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
                    .with_narrow_content_shot();
                return render_to_path(Box::new(app), NARROW_SHOTS_VIEWPORT, out);
            }
            let app =
                SettingsApp::with_selected(cat).with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32);
            return render_to_path(Box::new(app), DEFAULT_SHOTS_VIEWPORT, out);
        }
    }
    let scenario = Scenario::from_shots_path(out);
    render_to_path(Box::new(scenario.app()), scenario.viewport(), out)
}

/// Build the `Scene` for `widget` at `viewport` size. Mirrors the
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle
/// `ui::App` runs for a single redraw, but stops at the CPU-side `Scene` so a
/// caller can rasterise many scenes through **one** reused
/// [`OffscreenRenderer`] (which amortises the expensive vello shader
/// compilation over every pane instead of paying it per process).
pub fn build_widget_scene(mut widget: Box<dyn Widget>, viewport: (u32, u32)) -> Scene {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=<name>` renders under any named theme (e.g. `dracula`,
    // `gruvbox`, `catppuccin-mocha`, `light`, `high-contrast`); defaults to dark.
    let theme = std::env::var("QOS_SHOTS_THEME")
        .ok()
        .and_then(|n| Theme::named(&n))
        .unwrap_or_else(Theme::dark);
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
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
        .expect("root node");
    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy
            .compute_layout_with_measure(
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
            )
            .expect("layout");
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
pub fn render_to_path(
    widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let scene = build_widget_scene(widget, viewport);

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

    #[test]
    fn scenario_derives_narrow_from_filename() {
        assert_eq!(
            Scenario::from_shots_path(Path::new("/x/settings-narrow.png")),
            Scenario::Narrow
        );
        assert_eq!(
            Scenario::from_shots_path(Path::new("/x/settings.png")),
            Scenario::Wide
        );
    }

    #[test]
    fn narrow_app_selects_region_formats() {
        // The width hint + shot flag are applied; the resolved
        // narrow/overlay layout is asserted in the layout-driven tests once a
        // frame is laid out (see settings_app tests).
        assert_eq!(Scenario::Narrow.app().selected(), Category::RegionFormats);
    }

    #[test]
    fn wide_app_selects_external_accounts() {
        assert_eq!(Scenario::Wide.app().selected(), Category::ExternalAccounts);
    }
}

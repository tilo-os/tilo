//! Headless `--shots` mode — renders one offscreen frame of the
//! `SettingsApp` to a PNG and exits. Mirrors the sibling native apps so the
//! snapshot harness stays consistent.
//!
//! Two scenarios are derived from the output filename so a single binary
//! produces both required shots:
//! - `settings.png` — the wide two-pane layout, opened on a rich panel
//!   (External Accounts), so the category sidebar and a populated content
//!   pane are both visible.
//! - `settings-narrow.png` — the responsive single-pane layout, drilled
//!   into the Region & Formats panel with the back affordance.

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

use crate::categories::Category;
use crate::SettingsApp;

/// Default (wide) offscreen viewport. Matches the sibling apps' aspect.
pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 760);

/// Narrow viewport used for the responsive single-pane shot — phone-ish.
pub const NARROW_SHOTS_VIEWPORT: (u32, u32) = (440, 820);

/// The screenshot scenarios, derived from the output filename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// Wide two-pane, opened on External Accounts.
    Wide,
    /// Narrow single-pane, drilled into Region & Formats.
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
            // Narrow: the overlay flap, forced open so the responsive sidebar
            // is visible over the Region & Formats content behind its scrim.
            Scenario::Narrow => SettingsApp::with_selected(Category::RegionFormats)
                .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32)
                .with_sidebar_open_shot(),
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
        WifiNetworkInfo { ssid: "Home_Network".into(), signal: 4, secure: true, connected: true },
        WifiNetworkInfo { ssid: "Cafe_Guest".into(), signal: 3, secure: false, connected: false },
        WifiNetworkInfo { ssid: "Office_5G".into(), signal: 2, secure: true, connected: false },
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

    // `…collapsed.png` proves the docked sidebar can be hidden, reclaiming
    // its width for the content pane.
    if fname.contains("collapsed") {
        let app = SettingsApp::with_selected(Category::Network)
            .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32)
            .with_sidebar_collapsed_shot();
        return render_to_path(Box::new(app), DEFAULT_SHOTS_VIEWPORT, out);
    }

    // `…netdemo.png` renders the Network pane with a populated scan list and
    // an open inline password prompt, proving the connect flow's surface.
    if fname.contains("netdemo") {
        return render_to_path(network_demo_pane(), DEFAULT_SHOTS_VIEWPORT, out);
    }

    // Per-panel capture: a `…panel-<slug>.png` filename (or the
    // `QOS_SHOTS_PANEL` env var) renders the wide layout on that category.
    let panel_slug = std::env::var("QOS_SHOTS_PANEL").ok().or_else(|| {
        let name = out.file_name().and_then(|s| s.to_str()).unwrap_or("");
        name.rsplit_once("panel-")
            .map(|(_, rest)| rest.trim_end_matches(".png").to_string())
    });
    if let Some(slug) = panel_slug {
        if let Some(cat) = category_from_slug(slug.trim()) {
            let app = SettingsApp::with_selected(cat)
                .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32);
            return render_to_path(Box::new(app), DEFAULT_SHOTS_VIEWPORT, out);
        }
    }
    let scenario = Scenario::from_shots_path(out);
    render_to_path(Box::new(scenario.app()), scenario.viewport(), out)
}

/// Render `widget` at `viewport` size into a PNG at `out`. Mirrors the
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle
/// `ui::App` runs for a single redraw.
pub fn render_to_path(
    mut widget: Box<dyn Widget>,
    viewport: (u32, u32),
    out: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    // First sync seeds the CRDT keys the panes drive their controls from.
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    // Second sync — what the live `ui::App` does after a structural rebuild
    // (see `App::rebuild_structure`): the widgets built above are fresh
    // instances, so this sync seeds their CRDT keys and lets a top-level pane's
    // leaf controls read them back. (A pane rendered directly, e.g. `netdemo`,
    // has its composition built here, so this alone settles its toggles.)
    widget.sync(&mut state.doc, &state.root);
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

    {
        let measure_map = node_map.clone();
        let tr = &mut text_renderer;
        taffy.compute_layout_with_measure(
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
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    // Settling sync AFTER layout. `NavSplit` (the responsive sidebar|content
    // split `SettingsApp` wraps its panes in) builds its children's inner tree
    // lazily in `apply_layout`, not `build_layout` — so a pane nested inside
    // `SettingsApp` only has its composition (and thus its Toggle/Slider leaf
    // controls) after this point. The earlier sync already seeded those panes'
    // CRDT keys; this sync is what lets the just-built leaf controls read those
    // seeded values, so a nested pane's switches and sliders render their real
    // state instead of their constructed default. The live app reaches the same
    // state after a couple of frames; a single shot needs this explicitly.
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

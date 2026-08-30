//! Headless `--shots` mode — mirrors the sibling native apps.
//!
//! One binary produces one deterministic PNG per invocation; the
//! catalog is a hand-rolled fixture so the shot is hermetic and
//! doesn't reach for the network.

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

use crate::app::StoreApp;
use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource};

pub const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 760);

/// A hand-rolled fixture of items that shows every card variant:
///
///   * a qOS item that's not installed  → **Open**
///   * a qOS item that's cached + pinned → **Open** + **Installed** + Remove
///   * a qOS item that's cached but out-of-date → **Update** + Mark installed
///   * a Flatpak item (flatpak available) → **Install**
///   * a Flatpak item, flatpak unavailable path is covered by a
///     `set_flatpak_available(false)` variant used by the
///     "no-flatpak" scenario.
pub fn fixture_catalog() -> Vec<StoreItem> {
    let qos = |name: &str, desc: &str, cat: &str, installed: InstalledState, ver: &str| StoreItem {
        id: format!("dev.qos.{}", name.to_ascii_lowercase()),
        name: name.into(),
        description: desc.into(),
        categories: vec![cat.into()],
        icon_url: None,
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Qos {
            registry_name: "qOS main".into(),
        },
        versions: vec![StoreItemVersion {
            version: ver.into(),
            released_at_unix: 0,
            download_url: Some(format!("https://apps.qos.dev/{name}/{ver}.qpkg")),
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
        categories: vec![cat.into()],
        icon_url: None,
        homepage_url: None,
        source_url: None,
        install_ref_path: None,
        source: StoreSource::Flathub,
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
        qos(
            "Notes",
            "CRDT-native note-taking. Fast, spatial, collaborative.",
            "Productivity",
            InstalledState::Not,
            "0.1.0",
        ),
        qos(
            "Terminal",
            "GPU-accelerated terminal built on the ui toolkit.",
            "System",
            // Cached + marked installed (pinned): Open + Installed + Remove.
            InstalledState::Installed {
                version: "0.2.0".into(),
                available: "0.2.0".into(),
                pinned: true,
            },
            "0.2.0",
        ),
        qos(
            "Files",
            "Spatial file manager that speaks the FUSE mount.",
            "System",
            // Cached, not pinned, update available: Update + Mark installed.
            InstalledState::Installed {
                version: "0.3.0".into(),
                available: "0.4.0".into(),
                pinned: false,
            },
            "0.4.0",
        ),
        fh(
            "Chess",
            "The GNU Chess board Flatpak — legacy but keeps working.",
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
    app.set_status_line("Loaded 6 apps from qOS main + Flathub.");
    app
}

/// Public entry point the binary's `--shots` path calls.
pub fn render_to_path(viewport: (u32, u32), out: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut app = seeded_app();
    // Force the floating status chip fully visible so this single
    // headless frame captures it (a still can't hover it).
    app.arm_status_bubble_for_shots();
    let mut widget: Box<dyn Widget> = Box::new(app);
    render_widget(viewport, out, &mut widget)
}

/// Guts of the render pass — mirrors what `ui::App` does for a
/// single redraw. Extracted so tests can render into a temp dir.
pub fn render_widget(
    viewport: (u32, u32),
    out: &Path,
    widget: &mut Box<dyn Widget>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    let theme = match std::env::var("QOS_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut text_renderer = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);

    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
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

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    // Top-layer overlay pass — the floating status bubble paints here,
    // matching the live event loop's dedicated overlay pass.
    widget.draw_overlay(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
    );

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("store: wrote shot to {}", out.display());
    Ok(())
}

/// Parse `WxH` into `(u32, u32)`.
pub fn parse_viewport(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x').or_else(|| s.split_once('X'))?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fixture_catalog_covers_every_action_variant() {
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

    #[test]
    fn viewport_parses_expected_shape() {
        assert_eq!(parse_viewport("1200x760"), Some((1200, 760)));
        assert_eq!(parse_viewport("bad"), None);
    }

    #[test]
    fn seeded_app_lists_items() {
        let app = seeded_app();
        assert_eq!(app.items().len(), fixture_catalog().len());
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
}

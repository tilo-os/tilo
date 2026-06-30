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
            Scenario::Wide => SettingsApp::with_selected(Category::ExternalAccounts)
                .with_width_hint(DEFAULT_SHOTS_VIEWPORT.0 as f32),
            Scenario::Narrow => SettingsApp::with_selected(Category::RegionFormats)
                .with_width_hint(NARROW_SHOTS_VIEWPORT.0 as f32),
        }
    }
}

/// Render the scenario selected by `out`'s filename into a PNG at `out`.
pub fn render_for_path(out: &Path) -> Result<(), Box<dyn std::error::Error>> {
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
    fn narrow_app_is_narrow_and_drilled_in() {
        let app = Scenario::Narrow.app();
        assert!(app.is_narrow());
        assert!(app.drilled_in());
    }

    #[test]
    fn wide_app_is_not_narrow() {
        assert!(!Scenario::Wide.app().is_narrow());
    }
}

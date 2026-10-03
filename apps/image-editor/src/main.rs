#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![allow(
    clippy::cast_precision_loss,
    reason = "the renderer API accepts f32 logical dimensions; launch already bounds viewport inputs"
)]

use std::collections::HashMap;
use ui::launch::{LaunchArgs, LaunchSpec};
use ui::{
    permissions::PermissionContext,
    presence::PresenceCtx,
    taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree},
    text_render::TextRenderer,
    theme::Theme,
    vello::{
        Scene,
        kurbo::{Affine, Point},
        peniko::Color,
    },
    widget::Widget,
};
#[cfg(feature = "thin")]
extern crate runtime_dylib;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !image_editor::standalone_launch() {
        ui::framework::run::<image_editor::ImageEditorShell>();
        return Ok(());
    }
    ui::launch::run(
        &LaunchSpec::new("so.tilo.image-editor", image_editor::app_root)
            .shot_handler(render_shot)
            .allowed_options(&["--size"])
            .scenarios(image_editor::SHOT_SCENARIOS)
            .configure_app(|app| image_editor::actions::register(app.action_registry_mut())),
    )?;
    Ok(())
}
fn render_shot(args: &LaunchArgs) -> Result<(), ui::launch::LaunchError> {
    let out = args
        .shots
        .as_deref()
        .ok_or_else(|| ui::launch::LaunchError::Operation("missing shot path".into()))?;
    let scenario = args.scenario.as_deref().unwrap_or("canvas-stack");
    let viewport = args
        .value("--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .or(args.viewport)
        .unwrap_or_else(|| {
            if scenario.contains("narrow") {
                (620, 760)
            } else {
                (1280, 800)
            }
        });
    render(scenario, viewport, out, &shot_theme(args))
        .map_err(|e| ui::launch::LaunchError::Operation(e.to_string()))
}
fn render(
    scenario: &str,
    (w, h): (u32, u32),
    out: &std::path::Path,
    theme: &Theme,
) -> Result<(), Box<dyn std::error::Error>> {
    let staging = out
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("image-editor-shot-source.png");
    if let Some(parent) = staging.parent() {
        create_dir_all(parent)?;
    }
    image_editor::write_shot_image(&staging)?;
    let mut state = ui::State::new();
    let mut widget = image_editor::ImageEditorApp::new();
    if scenario != "empty" {
        widget.seed_shot(&mut state.doc, &staging, scenario.starts_with("graph"));
    }
    if scenario == "narrow-long-stack" {
        widget.seed_long_stack_for_shot(&mut state.doc);
    }
    if scenario == "narrow-more" {
        widget.open_more_for_shot();
    }
    // This custom offscreen renderer is the same immediate-mode UI as the
    // live app, so accessibility captures must carry the selected palette,
    // text scale and motion preference rather than silently reviewing a
    // normal dark frame.
    let mut tr = TextRenderer::new();
    tr.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    for _ in 0..2 {
        widget.sync(&mut state.doc, &state.root);
        let mut t: TaffyTree<()> = TaffyTree::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = widget.build_layout(&mut t, theme, &mut map);
        let root = t.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        )?;
        let mm = map.clone();
        t.compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(w as f32),
                height: AvailableSpace::Definite(h as f32),
            },
            |k, a, id, _, _| {
                if let Some(&ptr) = mm.get(&id) {
                    unsafe { (*ptr).measure(k, a, &mut tr) }
                } else {
                    ui::taffy::Size::ZERO
                }
            },
        )?;
        widget.apply_layout(&t, child, Point::ORIGIN);
        scene = Scene::new();
        widget.draw_with_presence(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            theme,
            &PresenceCtx::empty(),
            &PermissionContext::permissive(),
        );
    }
    if let Some(parent) = out.parent() {
        create_dir_all(parent)?;
    }
    ui::offscreen::OffscreenRenderer::new(w, h).render_png(&scene, Color::TRANSPARENT, out);
    Ok(())
}

fn create_dir_all(path: &std::path::Path) -> Result<(), tilo_files::FsError> {
    let dir = tilo_files::Dir::open(tilo_files::Location::Native(path.to_path_buf()))?;
    tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root())
}

fn shot_theme(args: &LaunchArgs) -> Theme {
    let theme = match std::env::var("TILO_SHOTS_THEME").ok().as_deref() {
        Some("light") => Theme::light(),
        Some("high-contrast") => Theme::high_contrast(),
        _ => Theme::dark(),
    };
    let reduce_motion = ["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .any(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        });
    theme
        .with_text_scale(args.text_scale.unwrap_or(1.0))
        .with_reduce_motion(reduce_motion)
}

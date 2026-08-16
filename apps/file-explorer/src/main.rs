// Thin binary entry point — all logic lives in the library.
//
// Supports two modes:
//   * default — build a `FileExplorerWidget`, hand it to `ui::App`,
//     run the live event loop (winit window).
//   * `--shots <PATH>` — render the file explorer at a default size into
//     a PNG at `<PATH>` and exit. No window, no event loop. Used for
//     headless screenshot tests and CI snapshots of the cold-start UI.

use std::collections::HashMap;

use file_explorer::FileExplorerWidget;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use taffy::prelude::*;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;

/// Default offscreen viewport for `--shots`.
const DEFAULT_SHOTS_VIEWPORT: (u32, u32) = (1200, 720);

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    if let Some(out) = parse_shots_arg(&args) {
        let viewport = parse_size_arg(&args).unwrap_or(DEFAULT_SHOTS_VIEWPORT);
        let scenario = parse_scenario_arg(&args);
        // The `sync` scenario needs a workspace mount to exist BEFORE the widget
        // is built (so the sidebar picks up the workspace root), so seed it here.
        let sync_dir = (scenario.as_deref() == Some("sync")).then(seed_sync_demo);
        let mut widget = FileExplorerWidget::new();
        // Optional `--path <dir>` starts the shot in a specific directory
        // (e.g. to capture a deep breadcrumb trail), else the default
        // cold-start location is used.
        if let Some(d) = sync_dir {
            widget.navigate(d);
        } else if let Some(p) = parse_path_arg(&args) {
            widget.navigate(p);
        }
        // Pre-layout scenario setup (view mode + selection must be set before
        // the tree is built so the composed view captures them).
        match scenario.as_deref() {
            Some("grid") => widget.set_view_mode(file_explorer::model::ViewMode::Grid),
            Some("select") => widget.debug_select(&[7, 8]),
            // Begin an inline rename before the seeding sync so the in-place
            // editor is drawn over the row (with the name selected).
            Some("rename") => {
                widget.debug_select(&[3]);
                widget.begin_rename();
            }
            // Summon the places drawer over the file view (the narrow
            // layout's hamburger action). Set BEFORE layout so `apply_layout`
            // places the revealed drawer — snapping post-layout would leave
            // the panes at their pre-reveal rects.
            Some("places") => widget.debug_open_places_drawer(),
            _ => {}
        }
        render_shot_to_png(widget, viewport, scenario.as_deref(), &out)?;
        return Ok(());
    }

    let widget = FileExplorerWidget::new();
    // Headless drive (`--headless-drive '<json>'`): run a list of clicks and
    // keystrokes against the real widget tree with no display, persisting
    // through the same store the live app uses.
    if let Some(spec) = ui::headless::spec_from_args(&args) {
        std::process::exit(ui::headless::run(spec, Box::new(widget), "dev.qos.file-explorer"));
    }

    let (state, autosave) = ui::persist::open("dev.qos.file-explorer");
    let app = ui::App::new(state, Box::new(widget)).with_autosave(autosave);
    app.run();
    Ok(())
}

/// Seed a demo workspace directory whose files carry real `user.qos.sync`
/// extended attributes, and point `QOS_WORKSPACE_ROOTS` at it, so a `--shots
/// --scenario sync` frame exercises the genuine badge path (`read_dir_sync` →
/// `getxattr` → `Entry.sync_state`) rather than any mock. Returns the dir.
fn seed_sync_demo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("qos-files-sync-demo");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    // A believable spread across the three hydration states + a subfolder.
    let files: [(&str, &str); 6] = [
        ("Design brief.md", "local"),
        ("meeting-notes.txt", "local"),
        ("Q3 budget.ods", "projected"),
        ("architecture.pdf", "projected"),
        ("launch-video.mp4", "downloading"),
        ("dataset.parquet", "downloading"),
    ];
    for (name, state) in files {
        let p = dir.join(name);
        let _ = std::fs::write(&p, b"");
        let _ = xattr::set(&p, "user.qos.sync", state.as_bytes());
    }
    let _ = std::fs::create_dir_all(dir.join("shared-assets"));
    // SAFETY: called once at process startup on the main thread, before any
    // other thread exists, so there is no concurrent environment access.
    unsafe { std::env::set_var("QOS_WORKSPACE_ROOTS", &dir) };
    dir
}

/// Returns `Some(path)` if `--shots <path>` (or `--shots=<path>`) appears
/// in `args`, otherwise `None`.
fn parse_shots_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--shots" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--shots=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Returns `Some((w, h))` if `--size WxH` (or `--size=WxH`) appears in
/// `args`, otherwise `None`. Lets a caller render the same offscreen frame
/// at different viewport sizes (e.g. a normal 1280×800 and a narrow width)
/// without a rebuild.
fn parse_size_arg(args: &[String]) -> Option<(u32, u32)> {
    let mut iter = args.iter().skip(1);
    let spec = loop {
        let arg = iter.next()?;
        if arg == "--size" {
            break iter.next()?.clone();
        }
        if let Some(rest) = arg.strip_prefix("--size=") {
            break rest.to_string();
        }
    };
    let (w, h) = spec.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Returns `Some(name)` if `--scenario <name>` (or `--scenario=<name>`) appears
/// in `args`. Drives headless demo shots (`grid`, `menu`, `reorder`).
fn parse_scenario_arg(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--scenario" {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--scenario=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Returns `Some(dir)` if `--path <dir>` (or `--path=<dir>`) appears in
/// `args`, otherwise `None`. Only meaningful alongside `--shots`.
fn parse_path_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--path" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(rest) = arg.strip_prefix("--path=") {
            return Some(std::path::PathBuf::from(rest));
        }
    }
    None
}

/// Run one offscreen frame against `widget` at `viewport` and write the
/// resulting RGBA to `out` as a PNG. Mirrors `ui::App`'s
/// `sync → build_layout → compute_layout → apply_layout → draw` cycle, then
/// applies any post-layout `scenario` (which needs the laid-out geometry —
/// e.g. a mid-drag reorder or an open context menu) and finally composites the
/// overlay layer so context menus appear in the shot.
fn render_shot_to_png(
    mut widget: FileExplorerWidget,
    viewport: (u32, u32),
    scenario: Option<&str>,
    out: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (w, h) = viewport;
    // `QOS_SHOTS_THEME=<name>` renders the shot under any named theme (e.g.
    // `dracula`, `gruvbox`, `catppuccin-mocha`, `light`, `high-contrast`), so
    // the same frame can be graded under every palette. Defaults to dark.
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

    // Post-layout scenarios need the laid-out geometry.
    match scenario {
        Some("menu") => {
            // Right-click context menu + the "New File" template flyout, anchored
            // in the content pane.
            widget.debug_show_templates_menu(Point::new((w as f64) * 0.45, (h as f64) * 0.34));
        }
        Some("reorder") => {
            // Grab a favourite and lift it two rows up — mid-drag.
            widget.debug_begin_favorite_drag(4, 1);
        }
        Some("pindrag") => {
            // Carry a folder out of the list toward the sidebar — mid-drag, so
            // the floating ghost shows over the Favourites section.
            let folder = widget.current_path().join("Development");
            widget.debug_begin_pin_drag(folder, Point::new(150.0, 300.0));
        }
        Some("dropmove") => {
            // Drag a file onto a folder — mid-drop, so the ghost + the folder's
            // drop-target highlight both show. Pick the first published folder
            // row as the target and the first file as the dragged item.
            let target = widget.debug_folder_targets().into_iter().next().map(|(p, _)| p);
            let source = widget
                .entries()
                .iter()
                .find(|e| !e.is_dir)
                .map(|e| e.path.clone());
            if let (Some(target), Some(source)) = (target, source) {
                widget.debug_begin_move_drag(vec![source], target);
            }
        }
        Some("select") => {
            // Move the cursor over a (different) row so the hover veneer shows
            // alongside the accent selection.
            let ev = winit::event::WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new((w as f64) * 0.35, 560.0),
            };
            let mut handled = false;
            let mut focus = None;
            let mods = winit::event::Modifiers::default();
            widget.handle_event(
                &ev, 1.0, &mods, &mut state.doc, &state.root, &mut handled, &mut focus, child, &taffy,
            );
        }
        _ => {}
    }

    let mut scene = Scene::new();
    widget.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut text_renderer,
        &theme,
        &presence,
        &permission,
    );
    // Composite the overlay layer (context menu + flyout) on top.
    widget.draw_overlay(&mut scene, Affine::IDENTITY, &mut text_renderer, &theme, &presence);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut renderer = OffscreenRenderer::new(w, h);
    renderer.render_png(&scene, Color::TRANSPARENT, out);
    println!("file-explorer: wrote shot to {}", out.display());
    Ok(())
}

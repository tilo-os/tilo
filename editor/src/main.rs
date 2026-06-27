//! `word-processor` — the runnable word-processor app. Opens a document
//! (`.odt` / `.md`, or a blank doc) in a `WordProcessorWidget` and runs it as a
//! `ui` application, the same way the terminal and file-explorer apps do.
//!
//! Usage:
//!   word-processor [FILE]            run interactively (FILE optional)
//!   word-processor --shots OUT [FILE]  render one headless frame to OUT.png

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use editor::{open_document, WordProcessorWidget};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::text_render::TextRenderer;
use ui::{App, PresenceCtx, State, Theme, Widget};
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use vello::Scene;

use taffy::{AvailableSpace, Dimension, NodeId, Size, Style, TaffyTree};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();

    let mut shots: Option<String> = None;
    let mut file: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--shots" => {
                shots = args.get(i + 1).cloned();
                i += 2;
            }
            a if a.starts_with("--") => i += 1,
            a => {
                file = Some(PathBuf::from(a));
                i += 1;
            }
        }
    }

    let (state, widget) = build(file.as_deref())?;

    if let Some(out) = shots {
        render_shot(state, widget, (1000, 700), Path::new(&out))?;
    } else {
        App::new(state, widget).run();
    }
    Ok(())
}

/// Build the document state + widget: open `file` if given, else a blank doc.
fn build(file: Option<&Path>) -> Result<(State, Box<dyn Widget>), Box<dyn Error>> {
    match file {
        Some(p) => {
            let (state, widget, _fmt) = open_document(p)?;
            Ok((state, Box::new(widget)))
        }
        None => Ok((State::new(), Box::new(WordProcessorWidget::new()))),
    }
}

/// Render a single headless frame to `out` (mirrors the other apps' `--shots`).
fn render_shot(mut state: State, mut widget: Box<dyn Widget>, viewport: (u32, u32), out: &Path) -> Result<(), Box<dyn Error>> {
    let (w, h) = viewport;
    let theme = Theme::dark();
    let presence = PresenceCtx::empty();
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    widget.sync(&mut state.doc, &state.root);
    let child = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.new_with_children(
        Style {
            size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) },
            ..Default::default()
        },
        &[child],
    )?;
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy.compute_layout_with_measure(
            root,
            taffy::Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, trr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )?;
    }
    widget.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    widget.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    OffscreenRenderer::new(w, h).render_png(&scene, Color::TRANSPARENT, out);
    println!("word-processor: wrote shot to {}", out.display());
    Ok(())
}

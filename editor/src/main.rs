//! `word-processor` — the runnable word-processor app. Opens a document
//! (`.odt` / `.md`, or a blank doc) in a `WordProcessorWidget` and runs it as a
//! `ui` application, the same way the terminal and file-explorer apps do.
//!
//! Usage:
//!   word-processor [FILE]            run interactively (FILE optional)
//!   word-processor --shots OUT [FILE]  render one headless frame to OUT.png
//!
//! Collaboration: set `QUICK_COLLAB_SOCK` to a running `collab-broker` socket
//! and `QUICK_APP_ID` to a per-instance identity, and two instances share one
//! document with live remote carets/selections:
//!
//! ```bash
//! collab-broker /tmp/wp.sock &
//! QUICK_COLLAB_SOCK=/tmp/wp.sock QUICK_APP_ID=alice word-processor doc.md &
//! QUICK_COLLAB_SOCK=/tmp/wp.sock QUICK_APP_ID=bob   word-processor doc.md
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use editor::{open_document, WordProcessorWidget};
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceKey;
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

    let (mut state, widget) = build(file.as_deref())?;

    if let Some(out) = shots {
        render_shot(state, widget, (1000, 700), Path::new(&out))?;
    } else {
        // Collaboration (best-effort, opt-in via env): give this instance a
        // distinct CRDT actor so two editors are distinguishable authors, then
        // hand the App a presence-broadcast scope. The App already owns a
        // `CollaborationManager` that connects to `QUICK_COLLAB_SOCK` (when set)
        // and, while the event loop runs, pumps remote ops into `State`,
        // broadcasts local doc deltas + caret/selection presence, and feeds the
        // resulting `PresenceCtx` into the widget's draw — so wiring here is
        // just the per-instance identity + scope, mirroring the notes app. With
        // no socket set this is a no-op and single-user editing is unchanged.
        // The same id drives the Automerge actor (so two instances are distinct
        // CRDT authors) and is the colour/label key peers attribute presence to.
        // A missing QUICK_APP_ID falls back to a per-process id so two
        // un-configured instances still get distinct actors.
        let app_id = std::env::var("QUICK_APP_ID")
            .unwrap_or_else(|_| format!("wp-{}", std::process::id()));
        state
            .doc
            .set_actor(automerge::ActorId::from(app_id.as_bytes()));

        let mut app = App::new(state, widget);
        // Announce ourselves under a stable document scope: this drives the
        // late-join handshake (peers already in the room reply with full state)
        // and is the scope under which our pointer presence is broadcast. The
        // per-block caret/selection presence the widget reports rides on top.
        app.set_presence_broadcast_scope(Some(PresenceKey::Workspace {
            id: "word-processor".to_string(),
        }));
        app.run();
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

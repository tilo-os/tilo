//! Offscreen renders of the shared `RichTextEditor` in light + dark, so the
//! WYSIWYG rich-text widget can be self-inspected without opening a window.
//! Drives the real five-pass pipeline (sync → build_layout → compute_layout →
//! apply_layout → draw) on live CRDT documents seeded with Automerge native
//! marks (bold / italic / underline) and block styles.
//!
//! Run:  cargo run -p ui --example ui-shots -- rich_text_editor_shots
//! Out:  $RTE_SHOTS_OUT (default /tmp/rte_shots) — rte-dark.png, rte-light.png

use std::collections::HashMap;
use std::path::PathBuf;

use automerge::ObjType;
use automerge::transaction::Transactable;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use ui::State;
use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{RichTextEditor, Widget};

const WIDTH: u32 = 720;
const HEIGHT: u32 = 720;

/// Seed one rich-text field under `root[key]` with text, block style, and inline
/// marks, then return an editor bound to it (synced).
fn seeded(
    state: &mut State,
    key: &str,
    text: &str,
    style: &str,
    marks: &[(&str, usize, usize)],
) -> RichTextEditor {
    let field = state
        .doc
        .put_object(&state.root, key, ObjType::Map)
        .unwrap();
    let tid = state.doc.put_object(&field, "text", ObjType::Text).unwrap();
    state.doc.splice_text(&tid, 0, 0, text).unwrap();
    state.doc.put(&field, "style", style).unwrap();
    for (name, s, e) in marks {
        let m = automerge::marks::Mark::new(name.to_string(), true, *s, *e);
        state
            .doc
            .mark(&tid, m, automerge::marks::ExpandMark::None)
            .unwrap();
    }
    let mut ed = RichTextEditor::new(key);
    ed.sync(&mut state.doc, &state.root);
    ed
}

/// Lay one editor out at `(x, y)` for `width` logical px and draw it. Returns the
/// laid-out height so the caller can stack the next one below.
fn place_and_draw(
    ed: &mut RichTextEditor,
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    x: f64,
    y: f64,
    width: f32,
) -> f64 {
    let mut taffy = TaffyTree::<()>::new();
    let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = ed.build_layout(&mut taffy, theme, &mut nm);
    taffy
        .compute_layout_with_measure(
            node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            |known, available, nid, _, _| {
                nm.get(&nid)
                    .map(|&p| unsafe { (*p).measure(known, available, tr) })
                    .unwrap_or(Size::ZERO)
            },
        )
        .unwrap();
    ed.apply_layout(&taffy, node, Point::new(x, y));
    let presence = PresenceCtx::default();
    ed.draw(scene, Affine::IDENTITY, tr, theme, &presence);
    ed.get_rect().height()
}

fn shoot(theme: &Theme, out: PathBuf) {
    let mut state = State::new();

    let fox = "The quick brown fox jumps over the lazy dog.";
    let mut heading = seeded(&mut state, "h", "Rich Text Editor", "heading", &[]);
    let mut body = seeded(
        &mut state,
        "b",
        fox,
        "body",
        // "quick" bold, "brown fox" italic, "lazy dog" underlined.
        &[("bold", 4, 9), ("italic", 10, 19), ("underline", 35, 43)],
    );
    let mut bullet1 = seeded(
        &mut state,
        "l1",
        "Caret, selection & word motion",
        "bullet",
        &[],
    );
    let mut bullet2 = seeded(
        &mut state,
        "l2",
        "Bold / italic / underline runs",
        "bullet",
        &[("bold", 0, 4)],
    );
    let mut sub = seeded(
        &mut state,
        "s",
        "Backed by one CRDT document",
        "subheading",
        &[],
    );
    let mut empty = RichTextEditor::new("e").with_placeholder("Start writing…");
    empty.sync(&mut state.doc, &state.root);
    let mut focused = seeded(
        &mut state,
        "f",
        "A focused body field shows its caret",
        "body",
        &[],
    );
    focused.set_focus(true);

    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();

    // Backdrop.
    scene.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, WIDTH as f64, HEIGHT as f64),
    );

    let pad = 24.0;
    let width = (WIDTH as f32) - 2.0 * pad as f32;
    let mut y = pad;
    for ed in [
        &mut heading,
        &mut sub,
        &mut body,
        &mut bullet1,
        &mut bullet2,
        &mut focused,
        &mut empty,
    ] {
        let h = place_and_draw(ed, &mut scene, &mut tr, theme, pad, y, width);
        y += h + 10.0;
    }

    let mut r = OffscreenRenderer::new(WIDTH, HEIGHT);
    r.render_png(&scene, theme.bg_app, &out);
    println!("wrote {}", out.display());
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("RTE_SHOTS_OUT").unwrap_or_else(|_| "/tmp/rte_shots".to_string());
    std::fs::create_dir_all(&dir).unwrap();
    shoot(&Theme::dark(), PathBuf::from(&dir).join("rte-dark.png"));
    shoot(&Theme::light(), PathBuf::from(&dir).join("rte-light.png"));
}

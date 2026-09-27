#![allow(
    clippy::exit,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Offscreen renders of the shared `RichTextEditor` in light + dark, so the
//! WYSIWYG rich-text widget can be self-inspected without opening a window.
//! Drives the real five-pass pipeline (`sync` → `build_layout` → `compute_layout` →
//! `apply_layout` → `draw`) on live CRDT documents seeded with Automerge native
//! marks (bold / italic / underline) and block styles.
//!
//! Run:  `cargo run -p ui-examples --example ui-shots -- rich_text_editor_shots`
//! Out:  `$RTE_SHOTS_OUT` (default `/tmp/rte_shots`) — rte-dark.png, rte-light.png

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use automerge::ObjType;
use automerge::transaction::Transactable;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::State;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{RichTextEditor, Widget};

const WIDTH: u32 = 720;
const HEIGHT: u32 = 720;

/// Seed one rich-text field under `root[key]` with text, block style, and inline
/// marks, then return an editor bound to it (synced).
///
/// # Errors
///
/// Returns an error if any of the underlying CRDT operations (creating the
/// field object, splicing the text, setting the block style, or applying a
/// mark) fails.
fn seeded(
    state: &mut State,
    key: &str,
    text: &str,
    style: &str,
    marks: &[(&str, usize, usize)],
) -> Result<RichTextEditor, automerge::AutomergeError> {
    let field = state.doc.put_object(&state.root, key, ObjType::Map)?;
    let tid = state.doc.put_object(&field, "text", ObjType::Text)?;
    state.doc.splice_text(&tid, 0, 0, text)?;
    state.doc.put(&field, "style", style)?;
    for (name, s, e) in marks {
        let m = automerge::marks::Mark::new(name.to_string(), true, *s, *e);
        state
            .doc
            .mark(&tid, m, automerge::marks::ExpandMark::None)?;
    }
    let mut ed = RichTextEditor::new(key);
    ed.sync(&mut state.doc, &state.root);
    Ok(ed)
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
    let _ = taffy.compute_layout_with_measure(
        node,
        Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::MaxContent,
        },
        |known, available, nid, _, _| {
            nm.get(&nid).map_or(Size::ZERO, |&p| unsafe {
                (*p).measure(known, available, tr)
            })
        },
    );
    ed.apply_layout(&taffy, node, Point::new(x, y));
    let presence = PresenceCtx::default();
    ed.draw(scene, Affine::IDENTITY, tr, theme, &presence);
    ed.get_rect().height()
}

/// # Errors
///
/// Returns an error if seeding any of the demo document's rich-text fields
/// fails (see [`seeded`]).
fn shoot(theme: &Theme, out: &Path) -> Result<(), automerge::AutomergeError> {
    let mut state = State::new();

    let fox = "The quick brown fox jumps over the lazy dog.";
    let mut heading = seeded(&mut state, "h", "Rich Text Editor", "heading", &[])?;
    let mut body = seeded(
        &mut state,
        "b",
        fox,
        "body",
        // "quick" bold, "brown fox" italic, "lazy dog" underlined.
        &[("bold", 4, 9), ("italic", 10, 19), ("underline", 35, 43)],
    )?;
    let mut bullet1 = seeded(
        &mut state,
        "l1",
        "Caret, selection & word motion",
        "bullet",
        &[],
    )?;
    let mut bullet2 = seeded(
        &mut state,
        "l2",
        "Bold / italic / underline runs",
        "bullet",
        &[("bold", 0, 4)],
    )?;
    let mut sub = seeded(
        &mut state,
        "s",
        "Backed by one CRDT document",
        "subheading",
        &[],
    )?;
    let mut empty = RichTextEditor::new("e").with_placeholder("Start writing…");
    empty.sync(&mut state.doc, &state.root);
    let mut focused = seeded(
        &mut state,
        "f",
        "A focused body field shows its caret",
        "body",
        &[],
    )?;
    focused.set_focus(true);

    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();

    // Backdrop.
    scene.fill(
        vello::peniko::Fill::NonZero,
        Affine::IDENTITY,
        theme.bg_app,
        None,
        &vello::kurbo::Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
    );

    let pad = 24.0;
    // WIDTH is a small fixed logical-pixel constant, and pad a small fixed
    // layout constant — both far below f32's exact-integer range.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "WIDTH and pad are small fixed layout constants, far below f32's exact-integer range"
    )]
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
    r.render_png(&scene, theme.bg_app, out);
    println!("wrote {}", out.display());
    Ok(())
}

pub fn run(_args: &[String]) {
    let dir = std::env::var("RTE_SHOTS_OUT").unwrap_or_else(|_| "/tmp/rte_shots".to_string());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("rich_text_editor_shots: failed to create output dir {dir}: {e}");
        return;
    }
    for (theme, tag) in shot_themes() {
        if let Err(e) = shoot(&theme, &PathBuf::from(&dir).join(format!("rte-{tag}.png"))) {
            eprintln!("rich_text_editor_shots: failed to render {tag} shot: {e}");
            return;
        }
    }
}

/// The palettes to shoot: `TILO_SHOTS_THEME` names one (its tag is the name,
/// lower-cased, non-alphanumerics as `-`); unset shoots dark and light. An
/// unknown name is an error, never an empty run that exits 0.
fn shot_themes() -> Vec<(Theme, String)> {
    match std::env::var("TILO_SHOTS_THEME") {
        Ok(name) => {
            let Some(theme) = Theme::named(&name) else {
                eprintln!("ui-shots: unknown TILO_SHOTS_THEME `{name}`");
                std::process::exit(2)
            };
            let tag = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            vec![(theme, tag)]
        }
        Err(_) => vec![
            (Theme::dark(), "dark".to_string()),
            (Theme::light(), "light".to_string()),
        ],
    }
}

//! Render the peer "just wrote that" highlighter to PNG, in both a light
//! and a dark palette, so the marker can be *looked at* rather than
//! asserted about.
//!
//! Presence is fed in the way it actually arrives — `apply_remote_presence`
//! with the `Caret` payloads a peer publishes over the wire, and the block
//! key built the way `apps/agent` builds it — so what these frames show is
//! the production path end to end, not a store poked by hand.
//!
//! Two participants edit at once: a person, and an agent that has no
//! window and no pointer and publishes nothing but a caret. Both must come
//! out marked in their own colour, with the text underneath still readable.
//!
//! Each frame builds its presence immediately before rendering. The mark
//! is born at wall-clock `Instant::now()` and is fully gone one
//! `TYPING_FADE` later, so a second render reusing the first frame's
//! presence photographs an empty page — which is how the first version of
//! this harness produced a dark frame with no highlighter in it and looked
//! like a palette bug.
//!
//! ```text
//! WP_TYPING_DIR=/some/dir cargo test -p word-processor \
//!     --test peer_typing_visual -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use taffy::prelude::*;
use ui::collaboration::PresencePayload;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::{PathSegment, PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point};
use vello::peniko::Color;
use word_processor::{EditorDoc, WordProcessorWidget};

/// The presence key a word-processor block is addressed by on the wire.
///
/// Spelled out here rather than imported: this is the cross-process
/// contract `apps/agent` reproduces from the outside, and a test that
/// borrowed the app's own private helper could not tell whether the two
/// still agree.
fn block_key(block_id: &str) -> PresenceKey {
    PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".to_string()),
            PathSegment::Field(block_id.to_string()),
        ],
    }
}

/// A document with a heading and a few paragraphs of real prose, so a
/// highlight has full lines of text to sit behind.
fn document() -> (ui::State, WordProcessorWidget, Vec<String>) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);

    let paragraphs = [
        "Quarterly Review",
        "The compositor now streams window buffers to invited peers, and remote input is injected back through the same sandbox boundary that guards local clients.",
        "Sandboxing stays the security boundary. A workspace is a bubblewrap jail with its own file-access policy, and a request outside the allowlist prompts rather than resolving.",
        "The filesystem projects metadata immediately over gossip and hydrates content on first read, so opening a shared folder does not wait on a transfer.",
    ];
    for (i, p) in paragraphs.iter().enumerate() {
        if i > 0 {
            wp.model_mut().enter(&mut state.doc);
        }
        wp.model_mut().insert_str(&mut state.doc, p);
    }

    let ids = EditorDoc::bind(&mut state.doc, &state.root).blocks(&state.doc);
    (state, wp, ids)
}

/// Both peers' edits, as they arrive on the wire: a caret before the edit
/// and a caret after it, and nothing else. The run between the two is what
/// the highlighter has to find on its own.
///
/// Rebuilt per frame so every render photographs marks of a known age.
fn peers_editing(ids: &[String]) -> PresenceCtx {
    let mut presence = PresenceCtx::empty();
    presence.set_display_name(ui::permissions::UserId("alice".into()), "Alice");
    presence.set_display_name(ui::permissions::UserId("agent".into()), "Agent");

    // A person editing mid-paragraph. The caret before the edit, then the
    // delta landing, then the caret after it — a marker needs all three,
    // because a caret moving on its own is somebody reading.
    presence.apply_remote_presence(
        "alice".into(),
        block_key(&ids[1]),
        PresencePayload::Caret { pos: 4 },
    );
    presence.note_remote_text_edit(std::time::Instant::now());
    presence.apply_remote_presence(
        "alice".into(),
        block_key(&ids[1]),
        PresencePayload::Caret { pos: 60 },
    );

    // The agent, one paragraph down, replacing a line wholesale — the
    // single jump `apps/agent` makes when it splices in a reply.
    presence.apply_remote_presence(
        "agent".into(),
        block_key(&ids[3]),
        PresencePayload::Caret { pos: 0 },
    );
    presence.note_remote_text_edit(std::time::Instant::now());
    presence.apply_remote_presence(
        "agent".into(),
        block_key(&ids[3]),
        PresencePayload::Caret { pos: 96 },
    );
    presence
}

/// Drive the real five-pass draw with `presence` populated and write a PNG.
fn render(
    state: &mut ui::State,
    wp: &mut WordProcessorWidget,
    presence: &PresenceCtx,
    theme: &Theme,
    out: &str,
    w: u32,
    h: u32,
) {
    let permission = PermissionContext::permissive();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    wp.sync(&mut state.doc, &state.root);
    let child = wp.build_layout(&mut taffy, theme, &mut node_map);
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
        .unwrap();
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w as f32),
                    height: AvailableSpace::Definite(h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    wp.apply_layout(&taffy, child, Point::ORIGIN);

    let mut scene = Scene::new();
    wp.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        theme,
        presence,
        &permission,
    );
    let mut r = OffscreenRenderer::new(w, h);
    r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(out));
    eprintln!("wrote {out}");
}

/// Two peers, in both palettes, freshly marked and again half-faded.
#[test]
#[ignore = "PNG harness: run explicitly to inspect the marker"]
fn peer_typing_highlight_frames() {
    let dir = std::env::var("WP_TYPING_DIR")
        .unwrap_or_else(|_| "/tmp/wp-typing".to_string());
    std::fs::create_dir_all(&dir).unwrap();

    let (mut state, mut wp, ids) = document();
    wp.set_focus(true);
    assert!(ids.len() >= 4, "expected a heading and three paragraphs");

    for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        // Fresh: the marker at full strength.
        let presence = peers_editing(&ids);
        render(
            &mut state,
            &mut wp,
            &presence,
            &theme,
            &format!("{dir}/typing-{name}-fresh.png"),
            1100,
            760,
        );

        // Half-way through the fade, from the same wall clock the render
        // reads — the marker should be visibly lighter, not gone.
        let presence = peers_editing(&ids);
        std::thread::sleep(ui::presence::TYPING_FADE / 2);
        render(
            &mut state,
            &mut wp,
            &presence,
            &theme,
            &format!("{dir}/typing-{name}-fading.png"),
            1100,
            760,
        );
    }
    // And the case that must stay blank: a peer reading. Same forward
    // caret sweep, no edit behind it, so no marker anywhere.
    let mut reading = PresenceCtx::empty();
    reading.set_display_name(ui::permissions::UserId("alice".into()), "Alice");
    for pos in [0usize, 20, 45, 80, 120] {
        reading.apply_remote_presence(
            "alice".into(),
            block_key(&ids[1]),
            PresencePayload::Caret { pos },
        );
    }
    render(
        &mut state,
        &mut wp,
        &reading,
        &Theme::light(),
        &format!("{dir}/typing-reading-only.png"),
        1100,
        760,
    );

    eprintln!("INSPECT: ls -la {dir}");
}

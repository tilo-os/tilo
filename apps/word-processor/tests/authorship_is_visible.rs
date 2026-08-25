//! A page holding both a person's writing and an agent's, drawn the way the
//! app draws it.
//!
//! The claim a shared document makes is that a machine can write into it as a
//! participant. That claim is only legible if the page says which sentences
//! came from the machine — otherwise a reader is looking at a document that
//! appears to have edited itself.
//!
//! So these assert on the draw pass's own output (`authored_runs`), not on a
//! second calculation: the agent's paragraph comes back attributed, the
//! person's comes back untouched, and the marked rects sit on the agent's text
//! rather than somewhere near it.
//!
//! The PNG harness at the bottom is the same page in three palettes, for
//! looking at:
//!
//! ```text
//! WP_AUTHOR_DIR=/some/dir cargo test -p word-processor \
//!     --test authorship_is_visible -- --ignored --nocapture
//! ```

use std::collections::HashMap;

use qos_actions::Action;
use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::{PermissionContext, UserId};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use word_processor::actions::{MarkVal, RegionKind, Span, WordProcessorAction};
use word_processor::{EditorDoc, WordProcessorWidget};

const AGENT: &str = "Claude";
const PERSON: &str = "Xaver";

/// What the agent noticed, and wrote into the page.
const AGENT_NOTE: &str = "The prose says doors at seven but the bullet says \
                          18:00, and the sound check at 18:30 falls after \
                          doors open, which would not work in practice.";

/// A run sheet somebody typed, plus a paragraph the agent added and attributed
/// to itself the way [`qos_agent::apply::Editor`] attributes its edits — the
/// app's own `SpliceText` for the text, the app's own `SetMark` for the
/// durable `author` mark.
///
/// Returns the widget, the block ids, and the index of the agent's block.
fn a_page_two_participants_wrote() -> (ui::State, WordProcessorWidget, Vec<String>, usize) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);

    let typed = [
        "Launch night",
        "Doors at seven, and the room should be full by half past.",
        "Sound check 18:30. Doors 18:00. First act 19:15.",
    ];
    for (i, p) in typed.iter().enumerate() {
        if i > 0 {
            wp.model_mut().enter(&mut state.doc);
        }
        wp.model_mut().insert_str(&mut state.doc, p);
    }
    // The agent's paragraph: a block of its own, so the attribution has a whole
    // paragraph to sit on the way it does on the page it was written for.
    wp.model_mut().enter(&mut state.doc);
    wp.model_mut().insert_str(&mut state.doc, "");

    let ids = EditorDoc::bind(&mut state.doc, &state.root).blocks(&state.doc);
    let agent_block = ids.len() - 1;

    WordProcessorAction::SpliceText {
        region: RegionKind::Body,
        block: ids[agent_block].clone(),
        at: 0,
        del: 0,
        ins: AGENT_NOTE.to_string(),
    }
    .apply(&mut state.doc)
    .expect("the agent's text lands");

    WordProcessorAction::SetMark {
        region: RegionKind::Body,
        spans: vec![Span {
            block: ids[agent_block].clone(),
            start: 0,
            end: AGENT_NOTE.chars().count(),
        }],
        name: qdoc::schema::MARK_AUTHOR.to_string(),
        value: Some(MarkVal::Str(AGENT.to_string())),
        also_clear: Vec::new(),
    }
    .apply(&mut state.doc)
    .expect("the attribution lands");

    (state, wp, ids, agent_block)
}

/// The room, as the app knows it: two participants with names, so a run
/// attributed to one of them resolves to a colour and a label.
fn the_room() -> PresenceCtx {
    let mut presence = PresenceCtx::empty();
    presence.set_display_name(UserId("peer@14933081570182226410".into()), PERSON);
    presence.set_display_name(UserId("peer@7611448496700649778".into()), AGENT);
    presence
}

/// Drive the real five-pass draw and (optionally) write the frame out.
fn render(
    state: &mut ui::State,
    wp: &mut WordProcessorWidget,
    presence: &PresenceCtx,
    theme: &Theme,
    out: Option<&str>,
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
    if let Some(path) = out {
        let mut r = OffscreenRenderer::new(w, h);
        r.render_png(&scene, Color::TRANSPARENT, std::path::Path::new(path));
        eprintln!("wrote {path}");
    }
}

/// The union of one run's rects.
fn envelope(rects: &[Rect]) -> Rect {
    rects.iter().skip(1).fold(rects[0], |a, r| a.union(*r))
}

#[test]
fn the_agents_paragraph_comes_out_attributed() {
    let (mut state, mut wp, _ids, _agent_block) = a_page_two_participants_wrote();
    wp.set_focus(true);
    let presence = the_room();
    render(&mut state, &mut wp, &presence, &Theme::light(), None, 1100, 820);

    let runs = wp.authored_runs();
    assert_eq!(
        runs.len(),
        1,
        "exactly the agent's paragraph should be attributed, got {:?}",
        runs.iter().map(|(_, n, _)| n).collect::<Vec<_>>()
    );
    let (who, name, rects) = &runs[0];
    assert_eq!(name, AGENT, "the run must carry the agent's name");
    assert!(!rects.is_empty(), "an attributed run must cover some text");
    // Resolved to the participant the room knows by that name, so the band is
    // painted in the same colour as that participant's caret rather than in a
    // second colour derived from the string.
    assert_eq!(
        who,
        &UserId("peer@7611448496700649778".into()),
        "the author must resolve to the room's participant of that name"
    );
}

#[test]
fn what_the_person_typed_is_drawn_exactly_as_before() {
    // The over-marking guard. Unattributed prose must come out of the draw
    // pass with nothing on it — an attribution that covered the whole page
    // would be worth nothing on the page that matters.
    let (mut state, mut wp, ids, agent_block) = a_page_two_participants_wrote();
    wp.set_focus(true);
    let presence = the_room();
    render(&mut state, &mut wp, &presence, &Theme::light(), None, 1100, 820);

    let agent_run = envelope(&wp.authored_runs()[0].2);

    // Every block the person typed lies outside the attributed band.
    let typed_blocks: Vec<&String> = ids
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != agent_block)
        .map(|(_, id)| id)
        .collect();
    assert!(
        typed_blocks.len() >= 3,
        "expected the heading and two typed paragraphs"
    );
    for id in typed_blocks {
        let band = wp
            .block_band_for_test(id)
            .unwrap_or_else(|| panic!("block {id} was not laid out"));
        assert!(
            band.y1 <= agent_run.y0 + 0.5 || band.y0 >= agent_run.y1 - 0.5,
            "the attributed band {agent_run:?} overlaps block {id} at {band:?}, \
             which nobody attributed"
        );
    }
}

#[test]
fn a_document_nobody_attributed_draws_no_attribution() {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);
    wp.model_mut()
        .insert_str(&mut state.doc, "Doors at seven, and the room should be full.");
    wp.set_focus(true);

    let presence = the_room();
    render(&mut state, &mut wp, &presence, &Theme::light(), None, 1100, 820);
    assert!(
        wp.authored_runs().is_empty(),
        "prose nobody attributed must draw no attribution, got {:?}",
        wp.authored_runs()
    );
}

/// Every palette, written out to be looked at — and run every time, because a
/// frame that stopped rendering is exactly the failure a harness nobody runs
/// cannot report.
#[test]
fn authorship_frames() {
    let dir = std::env::var("WP_AUTHOR_DIR")
        .unwrap_or_else(|_| std::env::temp_dir().join("wp-author").display().to_string());
    std::fs::create_dir_all(&dir).unwrap();

    for (name, theme) in [
        ("light", Theme::light()),
        ("dark", Theme::dark()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let (mut state, mut wp, _ids, _agent) = a_page_two_participants_wrote();
        wp.set_focus(true);
        let presence = the_room();
        render(
            &mut state,
            &mut wp,
            &presence,
            &theme,
            Some(&format!("{dir}/authorship-{name}.png")),
            1100,
            820,
        );
    }
}

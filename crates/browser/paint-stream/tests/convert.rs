//! The converter and patches against snapshots recorded from a real engine
//! (`tools/record-fixture.mjs` over `fixtures/layers.html` and
//! `fixtures/form.html`, 800x600).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard; fixtures are checked in"
)]

use tilo_paint_stream::snapshot::{CHOICE_BOX, CHOICE_MARK, convert};
use tilo_paint_stream::{
    Anchor, BlobRef, Color, Frame, Item, LayerId, Patch, PatchError, Point, ROOT, Rect, decode,
    encode,
};

const VIEWPORT: Point = Point { x: 800.0, y: 600.0 };

fn snapshot(state: &str) -> serde_json::Value {
    let path = format!(
        "{}/tests/fixtures/layers.{state}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).expect("fixture");
    serde_json::from_str(&raw).expect("fixture json")
}

fn frame(state: &str, seq: u64) -> Frame {
    convert(&snapshot(state), VIEWPORT, seq).expect("convert")
}

/// The form of `fixtures/form.html`: `empty` as it loads, `filled` with every
/// kind of answer given.
fn form(state: &str, seq: u64) -> Frame {
    let path = format!(
        "{}/tests/fixtures/form.{state}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).expect("fixture");
    let snapshot: serde_json::Value = serde_json::from_str(&raw).expect("fixture json");
    convert(&snapshot, VIEWPORT, seq).expect("convert")
}

/// Every line of text a frame draws, whichever layer draws it.
fn all_texts(frame: &Frame) -> Vec<String> {
    frame
        .layers
        .keys()
        .flat_map(|id| texts(frame, *id))
        .collect()
}

/// The boxes of the items a frame draws as `part` of their element.
fn parts(frame: &Frame, part: u32) -> Vec<Rect> {
    frame
        .layers
        .values()
        .flat_map(|layer| layer.items.iter())
        .filter(|item| item.key().part == part)
        .filter_map(Item::rect)
        .collect()
}

fn texts(frame: &Frame, layer: LayerId) -> Vec<String> {
    frame.layers[&layer]
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Text { text, .. } => Some(text.trim().to_string()),
            _ => None,
        })
        .collect()
}

/// The layer whose text includes `needle`.
fn layer_with(frame: &Frame, needle: &str) -> LayerId {
    *frame
        .layers
        .keys()
        .find(|id| texts(frame, **id).iter().any(|t| t == needle))
        .unwrap_or_else(|| panic!("no layer draws {needle:?}"))
}

#[test]
fn layers_follow_scroll_containers_fixed_sticky_and_opacity() {
    let f = frame("top", 1);
    let header = layer_with(&f, "Paint stream");
    let fab = layer_with(&f, "Action");
    let side = layer_with(&f, "Alpha");
    let ghost = layer_with(&f, "half");
    assert!(
        texts(&f, ROOT).iter().any(|t| t == "Two"),
        "plain content stays in the root"
    );
    for id in [header, fab, side, ghost] {
        assert_ne!(id, ROOT);
        assert_eq!(f.layers[&id].parent, ROOT);
    }
    assert_eq!(f.layers[&fab].anchor, Anchor::Viewport, "position: fixed");
    assert!(f.layers[&side].clips, "overflow: auto clips");
    assert!(!f.layers[&header].clips, "sticky does not clip");
    assert!((f.layers[&ghost].opacity - 0.5).abs() < 0.01);
    // The fixed button sits 24px from the bottom-right corner of the viewport.
    let fab_origin = f.layers[&fab].origin;
    assert!(
        (fab_origin.x - (800.0 - 24.0 - 120.0)).abs() < 1.0,
        "{fab_origin:?}"
    );
    assert!(
        (fab_origin.y - (600.0 - 24.0 - 40.0)).abs() < 1.0,
        "{fab_origin:?}"
    );
    // Every layer is reachable from the root exactly once.
    for id in [header, fab, side, ghost] {
        let placed = f.layers[&ROOT]
            .items
            .iter()
            .filter(|i| **i == Item::Layer(id))
            .count();
        assert_eq!(placed, 1, "layer {id} placed once in the root");
    }
}

#[test]
fn form_fields_images_and_canvas_come_through() {
    let f = frame("top", 1);
    assert!(
        texts(&f, ROOT).iter().any(|t| t == "Search the page"),
        "placeholder"
    );
    let image = f.blobs().next().expect("the <img>");
    assert!(matches!(image, BlobRef::Url(u) if u.starts_with("data:image/png")));
    assert_eq!(
        f.background.0,
        [250, 250, 245, 255],
        "html background paints the canvas"
    );
    let typed = frame("typed", 2);
    assert!(
        texts(&typed, ROOT).iter().any(|t| t == "hello"),
        "typed value replaces placeholder"
    );
}

#[test]
fn scrolling_the_page_moves_headers_not_items() {
    let top = frame("top", 1);
    let scrolled = frame("scrolled", 2);
    assert!((scrolled.layers[&ROOT].scroll.y - 300.0).abs() < 0.01);
    let patch = top.diff(&scrolled);
    let root = patch
        .changes
        .iter()
        .find(|c| c.id == ROOT)
        .expect("root scroll changed");
    assert!(root.items.is_none(), "no root item moved: {:?}", root.items);
    let fab = layer_with(&top, "Action");
    assert!(
        patch.changes.iter().all(|c| c.id != fab),
        "a fixed layer ignores page scroll"
    );
    for change in &patch.changes {
        assert!(
            change.items.is_none(),
            "layer {} re-sent items on scroll",
            change.id
        );
    }
    let bytes = encode(&patch).expect("encode");
    assert!(bytes.len() < 256, "a page scroll is {} bytes", bytes.len());
}

#[test]
fn scrolling_a_sidebar_changes_only_its_header() {
    let before = frame("scrolled", 2);
    let after = frame("sidebar", 3);
    let side = layer_with(&before, "Alpha");
    let patch = before.diff(&after);
    assert_eq!(patch.changes.len(), 1, "{patch:?}");
    let change = &patch.changes[0];
    assert_eq!(change.id, side);
    assert!(
        change.items.is_none(),
        "sidebar items did not move in its content space"
    );
    assert_eq!(change.header.as_ref().map(|h| h.scroll.y), Some(100.0));
}

#[test]
fn typing_resends_only_the_field_text() {
    let before = frame("sidebar", 3);
    let after = frame("typed", 4);
    let patch = before.diff(&after);
    assert_eq!(patch.changes.len(), 1, "{patch:?}");
    let splice = patch.changes[0].items.as_ref().expect("root items changed");
    assert_eq!(splice.insert.len(), 1, "{splice:?}");
    assert!(matches!(&splice.insert[0], Item::Text { text, .. } if text == "hello"));
}

#[test]
fn patches_round_trip_through_the_wire_and_reproduce_the_next_frame() {
    let states = ["top", "scrolled", "sidebar", "typed"];
    let frames: Vec<Frame> = states
        .iter()
        .zip(1..)
        .map(|(s, seq)| frame(s, seq))
        .collect();
    let mut client: Frame = decode(&encode(&frames[0]).expect("encode")).expect("decode");
    for pair in frames.windows(2) {
        let wire = encode(&pair[0].diff(&pair[1])).expect("encode");
        let patch: Patch = decode(&wire).expect("decode");
        client.apply(&patch).expect("apply");
        assert_eq!(client, pair[1]);
    }
    // And backwards, which exercises removed and re-added state.
    let back = frames[3].diff(&frames[0]);
    client.apply(&back).expect("apply");
    assert_eq!(client, frames[0]);
}

#[test]
fn a_patch_refuses_the_wrong_base_and_leaves_the_frame_alone() {
    let top = frame("top", 1);
    let scrolled = frame("scrolled", 2);
    let mut other = frame("sidebar", 7);
    let before = other.clone();
    assert_eq!(
        other.apply(&top.diff(&scrolled)),
        Err(PatchError::WrongBase {
            have: 7,
            patch_base: 1
        })
    );
    assert_eq!(other, before);
}

#[test]
fn hit_testing_finds_what_paints_on_top() {
    let f = frame("top", 1);
    let fab = layer_with(&f, "Action");
    let header = layer_with(&f, "Paint stream");
    // The button's padding, left of its label.
    let fab_key = f
        .hit_test(Point {
            x: 800.0 - 24.0 - 112.0,
            y: 600.0 - 24.0 - 20.0,
        })
        .expect("fab");
    assert_eq!(fab_key.node, fab, "the fixed button is above the page");
    let header_key = f.hit_test(Point { x: 400.0, y: 24.0 }).expect("header");
    assert_eq!(header_key.node, header);
    // After scrolling, the sticky header still covers the top of the viewport.
    let scrolled = frame("scrolled", 2);
    assert_eq!(
        scrolled
            .hit_test(Point { x: 400.0, y: 24.0 })
            .map(|k| k.node),
        Some(header)
    );
}

#[test]
fn inline_svg_travels_as_vector_shapes_with_resolved_colours() {
    let f = frame("top", 1);
    let shapes = f
        .layers
        .values()
        .flat_map(|l| &l.items)
        .find_map(|i| match i {
            Item::Vector {
                view_box,
                shapes,
                rect,
                ..
            } => Some((view_box, shapes, rect)),
            _ => None,
        })
        .expect("the inline <svg> icon");
    let (view_box, shapes, rect) = shapes;
    assert!((view_box.w - 16.0).abs() < 0.01 && (rect.w - 32.0).abs() < 0.01);
    assert_eq!(shapes.len(), 2, "{shapes:?}");
    assert_eq!(
        shapes[0].fill.map(|c| c.0),
        Some([200, 30, 30, 255]),
        "currentColor resolved"
    );
    assert_eq!(shapes[1].fill.map(|c| c.0), Some([255, 255, 255, 255]));
    assert!(
        shapes[1].path.starts_with('M') && shapes[1].path.contains('A'),
        "circle as arcs"
    );
}

#[test]
fn a_stream_of_payloads_reproduces_the_frames_and_marks_its_keyframes() {
    use tilo_paint_stream::wire::{Payload, Receiver};
    let frames: Vec<Frame> = ["top", "scrolled", "sidebar", "typed"]
        .iter()
        .zip(1..)
        .map(|(s, seq)| frame(s, seq))
        .collect();
    let key = Payload::Key(frames[0].clone()).encode().expect("encode");
    assert!(Payload::is_key(&key));
    let mut viewer = Receiver::new();
    viewer.apply(Payload::decode(&key).expect("decode"));
    for pair in frames.windows(2) {
        let bytes = Payload::Patches(vec![pair[0].diff(&pair[1])])
            .encode()
            .expect("encode");
        assert!(!Payload::is_key(&bytes));
        viewer.apply(Payload::decode(&bytes).expect("decode"));
        assert_eq!(viewer.frame(), Some(&pair[1]));
    }
    let blob = Payload::Blob {
        hash: [7; 32],
        bytes: vec![1, 2, 3],
    }
    .encode()
    .expect("encode");
    assert!(!Payload::is_key(&blob));
    assert_eq!(
        viewer.apply(Payload::decode(&blob).expect("decode")),
        Some(([7; 32], vec![1, 2, 3]))
    );
}

#[test]
fn a_viewer_that_misses_a_patch_waits_for_the_next_keyframe() {
    use tilo_paint_stream::wire::{Payload, Receiver};
    let (a, b, c) = (frame("top", 1), frame("scrolled", 2), frame("sidebar", 3));
    let mut viewer = Receiver::new();
    viewer.apply(Payload::Key(a));
    // The patch a -> b was lost; b -> c does not apply to a.
    viewer.apply(Payload::Patches(vec![b.diff(&c)]));
    assert!(viewer.is_lost());
    viewer.apply(Payload::Patches(vec![b.diff(&c)]));
    assert!(viewer.is_lost(), "patches are ignored until a keyframe");
    viewer.apply(Payload::Key(c.clone()));
    assert!(!viewer.is_lost());
    assert_eq!(viewer.frame(), Some(&c));
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the widths and the radius are the fixture's whole CSS pixels, compared with what it states"
)]
fn decorations_belong_to_elements_and_every_border_side_shows() {
    use std::collections::HashSet;

    let snapshot = snapshot("top");
    let nodes = &snapshot["documents"][0]["nodes"];
    let elements: HashSet<u64> = nodes["nodeType"]
        .as_array()
        .expect("node types")
        .iter()
        .zip(nodes["backendNodeId"].as_array().expect("backend ids"))
        .filter(|(kind, _)| kind.as_i64() == Some(1))
        .filter_map(|(_, id)| id.as_u64())
        .collect();
    let f = convert(&snapshot, VIEWPORT, 1).expect("convert");
    let (mut cards, mut dividers) = (0, 0);
    for (id, layer) in &f.layers {
        for item in &layer.items {
            // A text box reports its parent's style, so a decoration drawn for
            // one rings every run of text with the parent's border.
            if !matches!(item, Item::Layer(_) | Item::Text { .. }) {
                assert!(
                    elements.contains(&item.key().node),
                    "layer {id} decorates {} for a node that is not an element",
                    item.key().node
                );
            }
            let Item::Border {
                widths,
                colors,
                radius,
                ..
            } = item
            else {
                continue;
            };
            if *widths == [1.0; 4] && colors[0] == Color([200, 200, 200, 255]) && *radius == 8.0 {
                cards += 1;
            }
            // `.card`'s border above; a rule that sets one side only (`#side
            // p`'s bottom divider) draws that side.
            if *widths == [0.0, 0.0, 1.0, 0.0] && colors[2] == Color([220, 220, 220, 255]) {
                dividers += 1;
            }
        }
    }
    assert_eq!(cards, 16, "the page's sixteen cards");
    assert_eq!(dividers, 12, "the sidebar's twelve list dividers");
}

#[test]
fn a_password_never_leaves_the_host() {
    let f = frame("password", 5);
    let all: Vec<String> = f.layers.keys().flat_map(|id| texts(&f, *id)).collect();
    assert!(all.iter().all(|t| !t.contains("secret")), "{all:?}");
    assert!(
        all.iter().any(|t| t == "\u{2022}".repeat(6).as_str()),
        "shown as six dots: {all:?}"
    );
}

#[test]
fn a_ticked_box_and_the_one_chosen_of_two_reach_the_viewer() {
    // The engine's theme paints a checkbox and a radio button, and a snapshot
    // carries no theme: without their marks a viewer could not tell a ticked
    // box from an empty one, which is the answer the page holds.
    let empty = form("empty", 1);
    let filled = form("filled", 2);
    let mut boxes = parts(&filled, CHOICE_BOX);
    assert_eq!(boxes.len(), 4, "two checkboxes and two radio buttons");
    assert_eq!(
        parts(&empty, CHOICE_BOX),
        boxes,
        "a box is there chosen or not"
    );
    assert!(
        parts(&empty, CHOICE_MARK).is_empty(),
        "nothing is chosen yet"
    );
    let marks = parts(&filled, CHOICE_MARK);
    assert_eq!(marks.len(), 2, "the ticked box and the chosen radio button");
    // Down the page: "I agree", "Send me news", "Yes", "No". The marks are in
    // the first and the last, and in no other.
    boxes.sort_by(|a, b| a.y.total_cmp(&b.y));
    let mut marked: Vec<usize> = marks
        .iter()
        .map(|mark| {
            boxes
                .iter()
                .position(|held| {
                    mark.x >= held.x
                        && mark.y >= held.y
                        && mark.x + mark.w <= held.x + held.w
                        && mark.y + mark.h <= held.y + held.h
                })
                .expect("a mark is inside the box it marks")
        })
        .collect();
    marked.sort_unstable();
    assert_eq!(marked, [0, 3]);
}

#[test]
fn what_a_page_only_sends_for_a_box_is_not_drawn_as_its_words() {
    // A checkbox's value ("yes", "news") is what the page sends for it, never
    // words it shows; its label is.
    let drawn = all_texts(&form("filled", 1));
    for sent in ["yes", "no", "news"] {
        assert!(
            !drawn.iter().any(|text| text == sent),
            "{sent:?} in {drawn:?}"
        );
    }
    assert!(drawn.iter().any(|text| text == "I agree"), "{drawn:?}");
}

#[test]
fn the_option_a_select_shows_and_the_lines_of_a_textarea_are_drawn() {
    let empty = all_texts(&form("empty", 1));
    let filled = all_texts(&form("filled", 2));
    assert!(
        empty.iter().any(|text| text == "Select a purpose"),
        "{empty:?}"
    );
    assert!(
        filled.iter().any(|text| text == "Visiting family"),
        "{filled:?}"
    );
    assert!(
        !filled
            .iter()
            .any(|text| text == "Select a purpose" || text == "Tourism"),
        "only the option shown is drawn: {filled:?}"
    );
    assert!(
        filled.iter().any(|text| text == "Maria Elena"),
        "{filled:?}"
    );
    for line in ["14 Canal Street, Flat 3", "Utrecht 3511 AB"] {
        assert!(
            filled.iter().any(|text| text == line),
            "{line:?} in {filled:?}"
        );
        assert!(!empty.iter().any(|text| text == line));
    }
}

#[test]
fn answering_a_form_reaches_a_viewer_as_a_patch() {
    let empty = form("empty", 1);
    let filled = form("filled", 2);
    let mut viewer: Frame = decode(&encode(&empty).expect("encode")).expect("decode");
    let patch: Patch = decode(&encode(&empty.diff(&filled)).expect("encode")).expect("decode");
    viewer.apply(&patch).expect("apply");
    assert_eq!(viewer, filled);
    assert_eq!(parts(&viewer, CHOICE_MARK).len(), 2);
}

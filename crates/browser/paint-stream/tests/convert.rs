//! The converter and patches against snapshots recorded from a real engine
//! (`tools/record-fixture.mjs` over `fixtures/layers.html`, 800x600).

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard; fixtures are checked in"
)]

use tilo_paint_stream::snapshot::convert;
use tilo_paint_stream::{
    Anchor, BlobRef, Frame, Item, LayerId, Patch, PatchError, Point, ROOT, decode, encode,
};

const VIEWPORT: Point = Point { x: 800.0, y: 600.0 };

fn frame(state: &str, seq: u64) -> Frame {
    let path = format!(
        "{}/tests/fixtures/layers.{state}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).expect("fixture");
    let snapshot: serde_json::Value = serde_json::from_str(&raw).expect("fixture json");
    convert(&snapshot, VIEWPORT, seq).expect("convert")
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
fn a_password_never_leaves_the_host() {
    let f = frame("password", 5);
    let all: Vec<String> = f.layers.keys().flat_map(|id| texts(&f, *id)).collect();
    assert!(all.iter().all(|t| !t.contains("secret")), "{all:?}");
    assert!(
        all.iter().any(|t| t == "\u{2022}".repeat(6).as_str()),
        "shown as six dots: {all:?}"
    );
}

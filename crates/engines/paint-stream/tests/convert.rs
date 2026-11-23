//! The converter and patches against snapshots recorded from a real engine
//! (`tools/record-fixture.mjs` over `fixtures/layers.html` and
//! `fixtures/form.html`, 800x600).
//!
//! `fixtures/controls.json` is the engine suite's form with every common
//! control, its first field being edited, as the suite's own engine laid it
//! out at 1280x800: the page `CONTROLS` of
//! `crates/engines/chromium-host/chromium/tests/page_share.py`, which prints the snapshot
//! when run with `TILO_PAGE_SHARE_RECORD` set. It was asked for with every
//! style of today's list, so it says what the engine's theme paints.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use tilo_paint_stream::fonts::FontClass;
use tilo_paint_stream::snapshot::{
    CHOICE_BOX, CHOICE_MARK, FIELD_MARK, FIELD_VALUE, OUTLINE, Told, convert, convert_with,
};
use tilo_paint_stream::{
    Anchor, BlobRef, Color, Frame, Item, ItemKey, LayerId, Patch, PatchError, Point, ROOT, Rect,
    decode, encode,
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

/// The snapshot of `fixtures/form.html` with every answer given, as recorded.
fn filled_form() -> serde_json::Value {
    let path = format!(
        "{}/tests/fixtures/form.filled.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture")).expect("fixture json")
}

/// Lay every box of `boxes` out `times` as large and as far from the page's corner.
fn enlarge(boxes: &mut serde_json::Value, times: f64) {
    for held in boxes.as_array_mut().expect("boxes") {
        for edge in held.as_array_mut().expect("a box") {
            *edge = serde_json::json!(edge.as_f64().expect("a number") * times);
        }
    }
}

#[test]
fn a_zoomed_page_carries_its_lengths_at_the_size_the_engine_drew_them() {
    // An engine lays a zoomed page out larger, while a style still computes to what the
    // page wrote and the page's own script still measures in the page's pixels. The same
    // form zoomed to twice its size: every box and line twice as large and as far.
    let plain = convert(&filled_form(), VIEWPORT, 1).expect("convert");
    let mut snapshot = filled_form();
    let document = &mut snapshot["documents"][0];
    enlarge(&mut document["layout"]["bounds"], 2.0);
    enlarge(&mut document["textBoxes"]["bounds"], 2.0);
    let viewport = Point {
        x: VIEWPORT.x * 2.0,
        y: VIEWPORT.y * 2.0,
    };
    let zoomed = convert(&snapshot, viewport, 1).expect("convert");
    let twice = |held: Rect| Rect::new(held.x * 2.0, held.y * 2.0, held.w * 2.0, held.h * 2.0);
    let near = |first: Rect, second: Rect| {
        [
            first.x - second.x,
            first.y - second.y,
            first.w - second.w,
            first.h - second.h,
        ]
        .iter()
        .all(|apart| apart.abs() < 0.01)
    };
    let (mut texts, mut borders) = (0, 0);
    for (id, layer) in &plain.layers {
        let large = &zoomed.layers[id].items;
        assert_eq!(layer.items.len(), large.len(), "layer {id}");
        for (was, now) in layer.items.iter().zip(large) {
            match (was, now) {
                (
                    Item::Text {
                        rect, font, text, ..
                    },
                    Item::Text {
                        rect: large_rect,
                        font: large_font,
                        text: large_text,
                        ..
                    },
                ) => {
                    texts += 1;
                    assert_eq!(text, large_text);
                    assert!(
                        (large_font.size - font.size * 2.0).abs() < 0.01,
                        "{text:?} is set at {} where the engine drew it at twice {}",
                        large_font.size,
                        font.size
                    );
                    assert!(
                        near(twice(*rect), *large_rect),
                        "{text:?} in {large_rect:?}"
                    );
                }
                (
                    Item::Border {
                        key, rect, widths, ..
                    },
                    Item::Border {
                        rect: large_rect,
                        widths: large_widths,
                        ..
                    },
                ) if key.part == 1 => {
                    borders += 1;
                    assert!(near(twice(*rect), *large_rect));
                    for (width, large_width) in widths.iter().zip(large_widths) {
                        assert!(
                            (large_width - width * 2.0).abs() < 0.01,
                            "an edge {width} wide arrives {large_width} wide"
                        );
                    }
                }
                _ => {}
            }
        }
    }
    assert!(
        texts >= 10 && borders >= 3,
        "{texts} runs, {borders} borders"
    );
}

/// The window the form with every control was laid out in.
const WINDOW: Point = Point {
    x: 1280.0,
    y: 800.0,
};

/// The engine's snapshot of the form with every common control.
fn controls() -> serde_json::Value {
    let path = format!(
        "{}/tests/fixtures/controls.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture")).expect("fixture json")
}

/// Where the element of `snapshot` whose `id` is `id` stands among its document's nodes.
fn element_of(snapshot: &serde_json::Value, id: &str) -> usize {
    let strings = snapshot["strings"].as_array().expect("strings");
    let string = |index: &serde_json::Value| {
        index
            .as_u64()
            .and_then(|at| strings.get(usize::try_from(at).ok()?))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    snapshot["documents"][0]["nodes"]["attributes"]
        .as_array()
        .expect("attributes")
        .iter()
        .position(|held| {
            held.as_array()
                .expect("an element's attributes")
                .chunks_exact(2)
                .any(|pair| string(&pair[0]) == "id" && string(&pair[1]) == id)
        })
        .unwrap_or_else(|| panic!("no element is called {id}"))
}

/// The engine's number for the element of `snapshot` whose `id` is `id`.
fn node_of(snapshot: &serde_json::Value, id: &str) -> u64 {
    let found = element_of(snapshot, id);
    snapshot["documents"][0]["nodes"]["backendNodeId"][found]
        .as_u64()
        .expect("a node")
}

/// Have the page of `snapshot` draw the field whose `id` is `id` as dots, as an engine leaves
/// a plain field it wrote a secret into.
fn draw_as_dots(snapshot: &mut serde_json::Value, id: &str) {
    let found = element_of(snapshot, id);
    let style = tilo_paint_stream::snapshot::COMPUTED_STYLES
        .iter()
        .position(|name| *name == "-webkit-text-security")
        .expect("a capture asks how a field's text is drawn");
    let disc = snapshot["strings"]
        .as_array()
        .expect("strings")
        .iter()
        .position(|text| *text == "disc")
        .expect("the page draws some field as dots");
    let layout = &mut snapshot["documents"][0]["layout"];
    let place = layout["nodeIndex"]
        .as_array()
        .expect("the nodes that are laid out")
        .iter()
        .position(|node| node.as_u64() == u64::try_from(found).ok())
        .unwrap_or_else(|| panic!("{id} is not laid out"));
    layout["styles"][place][style] = disc.into();
}

/// What a frame draws of one element.
fn drawn(frame: &Frame, node: u64) -> Vec<&Item> {
    frame
        .layers
        .values()
        .flat_map(|layer| layer.items.iter())
        .filter(|item| item.key().node == node && !matches!(item, Item::Layer(_)))
        .collect()
}

/// What a field holds as a frame shows it: its text drawn in the field's room.
fn held(frame: &Frame, node: u64) -> Vec<String> {
    drawn(frame, node)
        .into_iter()
        .filter_map(|item| match item {
            Item::Text { key, text, .. } if key.part & ItemKey::SLOT != 0 => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The parts of `node` a frame draws.
fn parts_of(frame: &Frame, node: u64) -> Vec<u32> {
    drawn(frame, node)
        .into_iter()
        .map(|item| item.key().part)
        .collect()
}

#[test]
fn a_date_is_drawn_as_the_engine_words_it_and_empty_until_the_engine_has() {
    let snapshot = controls();
    let date = node_of(&snapshot, "date");
    // A caller that asks no engine: the field is drawn with what it holds, as before.
    let unasked = convert_with(&snapshot, WINDOW, 1, &Told::default()).expect("convert");
    assert_eq!(held(&unasked.frame, date), ["2027-03-01"]);
    // What only the engine can say: the dates, the time, the number, and the words on the
    // button the page gave none. No secret is among them.
    let asked: Vec<(u64, &str)> = unasked
        .asks
        .iter()
        .map(|(node, value)| (*node, value.as_str()))
        .collect();
    for (id, value) in [
        ("date", "2027-03-01"),
        ("nodate", ""),
        ("time", "14:30"),
        ("number", "1234.5"),
        ("submit", ""),
    ] {
        assert!(
            asked.contains(&(node_of(&snapshot, id), value)),
            "{id} in {asked:?}"
        );
    }
    assert_eq!(asked.len(), 5, "{asked:?}");

    // A caller that asks: nothing is drawn of the field until the engine has said.
    let mut told = Told {
        asking: true,
        ..Told::default()
    };
    let waiting = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert!(
        held(&waiting.frame, date).is_empty(),
        "what the field holds is not what it shows"
    );
    assert!(
        parts_of(&waiting.frame, date).contains(&FIELD_MARK),
        "the field itself is there, with its picker"
    );

    told.shown.insert(
        date,
        ("2027-03-01".to_owned(), Some("03/01/2027".to_owned())),
    );
    let said = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert_eq!(held(&said.frame, date), ["03/01/2027"]);

    // What the engine said of another value is not what the field shows now.
    told.shown.insert(
        date,
        ("2026-12-31".to_owned(), Some("12/31/2026".to_owned())),
    );
    let stale = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert!(held(&stale.frame, date).is_empty());

    // An engine that could not say leaves the field drawn with what it holds.
    told.shown.insert(date, ("2027-03-01".to_owned(), None));
    let unsaid = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert_eq!(held(&unsaid.frame, date), ["2027-03-01"]);
}

#[test]
fn no_character_of_a_secret_is_drawn_whatever_kind_of_field_holds_it() {
    let snapshot = controls();
    let converted = convert_with(&snapshot, WINDOW, 1, &Told::default()).expect("convert");
    let frame = &converted.frame;
    let all = all_texts(frame);
    // A password, and a card's number in a field of plain text the page says holds one.
    for secret in ["hunter2-secret", "4111111111111111"] {
        for part in secret.as_bytes().windows(4) {
            let part = std::str::from_utf8(part).expect("letters and digits");
            assert!(
                all.iter().all(|text| !text.contains(part)),
                "{part:?} of a secret is drawn: {all:?}"
            );
        }
    }
    let (password, card) = (node_of(&snapshot, "password"), node_of(&snapshot, "card"));
    assert_eq!(held(frame, password), ["\u{2022}".repeat(14)]);
    assert_eq!(held(frame, card), ["\u{2022}".repeat(16)]);
    assert_eq!(converted.passwords, [password]);
    assert!(
        converted
            .asks
            .iter()
            .all(|(node, _)| *node != password && *node != card),
        "the engine is never asked what a secret field shows"
    );

    // A field that was a password field stays one when its page turns it into plain text.
    let name = node_of(&snapshot, "name");
    assert_eq!(held(frame, name), ["Ada Lovelace"]);
    let told = Told {
        secret: [name].into(),
        ..Told::default()
    };
    let shown = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert_eq!(held(&shown.frame, name), ["\u{2022}".repeat(12)]);

    // A field its page draws as dots is a secret the same way, with nothing told: that is how
    // an engine leaves a plain field it wrote a text released for one site into.
    let mut dotted = controls();
    draw_as_dots(&mut dotted, "name");
    let drawn = convert_with(&dotted, WINDOW, 1, &Told::default()).expect("convert");
    assert_eq!(held(&drawn.frame, name), ["\u{2022}".repeat(12)]);
    assert!(
        drawn.asks.iter().all(|(node, _)| *node != name),
        "the engine is never asked what such a field shows"
    );
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the widths and radii are whole pixels the converter sets, compared with what it sets"
)]
fn a_control_is_drawn_as_the_engines_theme_draws_it() {
    let snapshot = controls();
    let frame = convert(&snapshot, WINDOW, 1).expect("convert");
    let node = |id: &str| node_of(&snapshot, id);
    let grey = Color([118, 118, 118, 255]);
    let accent = Color([0, 117, 255, 255]);

    // A field the page left alone: the theme's hairline and small radius, not the two
    // pixels of inset border its style computes to.
    let edge = |id: &str| {
        drawn(&frame, node(id))
            .into_iter()
            .find_map(|item| match item {
                Item::Border {
                    key,
                    widths,
                    colors,
                    radius,
                    ..
                } if key.part == 1 => Some((*widths, colors[0], *radius)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} has no edge"))
    };
    assert_eq!(edge("name"), ([1.0; 4], grey, 2.0));
    assert_eq!(edge("send"), ([1.0; 4], grey, 2.0));
    assert_eq!(edge("country"), ([1.0; 4], grey, 2.0));
    assert_eq!(
        edge("disabled").1,
        Color([118, 118, 118, 77]),
        "a field that takes no answer is dimmed"
    );

    // What opens says so: a select, a date and a time; a plain field does not.
    for id in ["country", "closed", "date", "nodate", "time"] {
        let marks = drawn(&frame, node(id))
            .into_iter()
            .filter(|item| matches!(item, Item::Vector { key, .. } if key.part == FIELD_MARK))
            .count();
        assert_eq!(marks, 1, "{id}");
    }
    assert!(!parts_of(&frame, node("name")).contains(&FIELD_MARK));
    assert_eq!(held(&frame, node("country")), ["Brazil"]);
    assert_eq!(held(&frame, node("empty")), ["Type a name"]);

    // A ticked box is filled with the accent and ticked; an empty one is a grey ring.
    let ring = |id: &str| {
        drawn(&frame, node(id))
            .into_iter()
            .find_map(|item| match item {
                Item::Border { key, colors, .. } if key.part == CHOICE_BOX => Some(colors[0]),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} has no box"))
    };
    assert_eq!(ring("agree"), accent);
    assert_eq!(ring("news"), grey);
    assert_eq!(ring("no"), accent);
    assert_eq!(ring("yes"), grey);
    assert_eq!(
        ring("off"),
        Color([0, 117, 255, 77]),
        "a box that takes no answer is dimmed"
    );
    for (id, ticked) in [
        ("agree", true),
        ("news", false),
        ("off", true),
        ("yes", false),
        ("no", true),
    ] {
        assert_eq!(
            parts_of(&frame, node(id)).contains(&CHOICE_MARK),
            ticked,
            "{id}"
        );
    }
}

#[test]
#[allow(
    clippy::float_cmp,
    reason = "the widths are whole pixels the converter sets, compared with what it sets"
)]
fn a_value_and_the_field_being_edited_are_drawn_as_the_engine_marks_them() {
    let snapshot = controls();
    let frame = convert(&snapshot, WINDOW, 1).expect("convert");
    let node = |id: &str| node_of(&snapshot, id);

    // A slider's thumb stands seven tenths of the way along, and a colour shows its swatch.
    let value = |id: &str| {
        drawn(&frame, node(id))
            .into_iter()
            .find_map(|item| match item {
                Item::Fill {
                    key, rect, color, ..
                } if key.part == FIELD_VALUE => Some((*rect, *color)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{id} shows no value"))
    };
    let (thumb, _) = value("level");
    let groove = drawn(&frame, node("level"))
        .into_iter()
        .find_map(|item| match item {
            Item::Fill { key, rect, .. } if key.part == CHOICE_BOX => Some(*rect),
            _ => None,
        })
        .expect("the slider's groove");
    let along = (thumb.x + thumb.w / 2.0 - groove.x) / groove.w;
    assert!((along - 0.7).abs() < 0.08, "{along}");
    assert_eq!(value("colour").1, Color([255, 136, 0, 255]));

    // The field being edited wears the engine's ring, and no other does.
    let rings: Vec<(u64, [f32; 4], Color)> = frame
        .layers
        .values()
        .flat_map(|layer| layer.items.iter())
        .filter_map(|item| match item {
            Item::Border {
                key,
                widths,
                colors,
                ..
            } if key.part == OUTLINE => Some((key.node, *widths, colors[0])),
            _ => None,
        })
        .collect();
    assert_eq!(
        rings,
        [(node("name"), [2.0; 4], Color([16, 16, 16, 255]))],
        "one ring, around the field being edited"
    );
}

#[test]
fn text_is_sent_in_the_class_of_face_the_engine_said_it_set() {
    let snapshot = controls();
    let unasked = convert_with(&snapshot, WINDOW, 1, &Told::default()).expect("convert");
    // The lists the page's text is set in: the engine's standard face for what the page
    // named none for, its own for fields, and the fixed-width one of a date.
    assert_eq!(
        unasked.families,
        ["\"Times New Roman\"", "Arial", "monospace"]
    );
    let families = |frame: &Frame| -> Vec<String> {
        let mut families: Vec<String> = frame
            .layers
            .values()
            .flat_map(|layer| layer.items.iter())
            .filter_map(|item| match item {
                Item::Text { font, .. } => Some(font.family.clone()),
                _ => None,
            })
            .collect();
        families.sort();
        families.dedup();
        families
    };
    assert_eq!(families(&unasked.frame), unasked.families);

    // A machine that has no Times sets the page's text in its sans, and says so; of a
    // fixed-width face it said nothing.
    let told = Told {
        classes: [
            ("\"Times New Roman\"".to_owned(), Some(FontClass::Sans)),
            ("Arial".to_owned(), Some(FontClass::Sans)),
            ("monospace".to_owned(), None),
        ]
        .into(),
        ..Told::default()
    };
    let said = convert_with(&snapshot, WINDOW, 1, &told).expect("convert");
    assert!(
        said.families.is_empty(),
        "nothing is left to ask: {:?}",
        said.families
    );
    assert_eq!(families(&said.frame), ["Arial", "monospace", "sans-serif"]);
}

use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use std::cell::Cell;
use std::rc::Rc;

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

#[test]
fn justified_rows_span_the_width_and_keep_aspect() {
    let aspects = [1.5, 0.66, 1.0, 1.78, 1.33, 0.75, 1.5];
    let rects = justified_rows(&aspects, 900.0, 200.0, 4.0);
    assert_eq!(rects.len(), aspects.len());
    for (r, a) in rects.iter().zip(aspects) {
        assert!((r.width() / r.height() - a).abs() < 0.01, "aspect kept");
    }
    // Every full row ends at the width; rows do not overlap.
    let mut rows: Vec<Vec<Rect>> = Vec::new();
    for r in &rects {
        match rows.last_mut() {
            Some(row) if (row[0].y0 - r.y0).abs() < 0.01 => row.push(*r),
            _ => rows.push(vec![*r]),
        }
    }
    for row in &rows[..rows.len() - 1] {
        assert!((row.last().unwrap().x1 - 900.0).abs() < 0.5);
        for pair in row.windows(2) {
            assert!(
                (pair[1].x0 - pair[0].x1 - 4.0).abs() < 0.01,
                "one gap apart"
            );
        }
    }
    let last = rows.last().unwrap();
    assert!((last[0].height() - 200.0).abs() < 0.01 || last.last().unwrap().x1 >= 899.5);
    for pair in rows.windows(2) {
        assert!(pair[1][0].y0 >= pair[0][0].y1 + 3.99);
    }
}

#[test]
fn text_blocks_line_up_across_a_row() {
    let cards = vec![
        MediaCard::new("a", MediaSource::Placeholder)
            .title("Kind of Blue")
            .meta("Miles Davis"),
        MediaCard::new("b", MediaSource::Placeholder)
            .title("The Rise and Fall of Ziggy Stardust and the Spiders from Mars")
            .meta("David Bowie"),
        MediaCard::new("c", MediaSource::Placeholder).title("Blue"),
    ];
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(MediaGrid::new(cards)),
        (900, 800),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let rects: Vec<Rect> = root.children.iter().map(rect).collect();
    assert_eq!(rects.len(), 3);
    assert!((rects[0].y0 - rects[2].y0).abs() < 0.5, "one row");
    assert!(
        (rects[0].height() - rects[1].height()).abs() < 1.0
            && (rects[2].height() - rects[1].height()).abs() < 1.0,
        "the row shares its tallest text block: {rects:?}"
    );
    // Square art, the text under it.
    assert!(rects[0].height() > rects[0].width());
}

#[test]
fn columns_come_from_the_width_and_the_text_scale() {
    let theme = Theme::dark();
    let regular = MediaSize::Regular.extent(&theme);
    assert!(MediaSize::Small.extent(&theme) < regular);
    assert!(MediaSize::Regular.extent(&Theme::dark().with_text_scale(2.0)) > regular * 1.5);
    let grid = |width: u32| {
        let cards = (0..8)
            .map(|i| MediaCard::new(format!("m{i}"), MediaSource::Placeholder).title("Album"))
            .collect();
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(MediaGrid::new(cards)),
            (width, 2000),
            Theme::dark(),
        );
        let root = harness.inspection().root;
        let y0 = root.children[0].rect[1];
        root.children
            .iter()
            .filter(|c| (c.rect[1] - y0).abs() < 0.5)
            .count()
    };
    assert!(grid(1200) > grid(600));
    assert_eq!(grid(200), 1);
}

#[test]
fn a_card_activates_and_its_hover_action_is_its_own() {
    let opened = Rc::new(Cell::new(0));
    let played = Rc::new(Cell::new(0));
    let (o, p) = (opened.clone(), played.clone());
    let card = MediaCard::new("album", MediaSource::Placeholder)
        .title("Kind of Blue")
        .meta("Miles Davis")
        .hover_action(Icon::Play, "Play", move || p.set(p.get() + 1))
        .on_activate(move || o.set(o.get() + 1));
    let mut harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(MediaGrid::new(vec![card])),
        (300, 600),
        Theme::dark(),
    );
    let card = rect(&harness.inspection().root.children[0]);
    let art_centre = Point::new(card.center().x, card.y0 + card.width() / 2.0);
    // Away from the centred action: opens.
    harness.click_at(card.x0 + 10.0, card.y0 + 10.0);
    assert_eq!(opened.get(), 1);
    // On the action (shown while hovered): plays, does not open.
    harness.click_at(art_centre.x, art_centre.y);
    assert_eq!((opened.get(), played.get()), (1, 1));
}

#[test]
fn placeholders_and_badges_paint_without_seams() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let badges = [
        MediaBadge::Duration("12:04"),
        MediaBadge::Progress(0.4),
        MediaBadge::Live,
        MediaBadge::Downloaded,
    ];
    let face = MediaCardFace::new(MediaArt::Placeholder)
        .aspect(MediaAspect::Wide)
        .title("Building a compositor in Rust")
        .meta("Tilo Engineering")
        .meta("3 days ago")
        .badges(&badges);
    let width = 320.0;
    let height = face.height(&mut text, &theme, width);
    assert!(height > width / (16.0 / 9.0));
    let mut scene = Scene::new();
    face.paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        Rect::new(0.0, 0.0, width, height),
    );
    // The placeholder is one gradient fill, not a stack of bands.
    let gradients = scene
        .encoding()
        .draw_tags
        .iter()
        .filter(|t| **t == vello_encoding::DrawTag::LINEAR_GRADIENT)
        .count();
    assert_eq!(gradients, 1);
    let node = face.accessibility(Rect::new(0.0, 0.0, width, height));
    assert_eq!(node.name.as_deref(), Some("Building a compositor in Rust"));
    assert!(node.value.as_deref().unwrap().contains("40% watched"));
}

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
    let above = rows[rows.len() - 2][0].height();
    assert!(
        (last[0].height() - 200.0_f64.min(above)).abs() < 0.01 || last.last().unwrap().x1 >= 899.5,
        "a short last row is no taller than the row above it"
    );
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
    assert!(node.value.as_deref().unwrap().contains("40% played"));
}

#[test]
fn overlay_badges_paint_legible_ink_in_every_theme() {
    use crate::color::contrast_ratio;
    use crate::tokens::colors::ON_FILL_MIN_CONTRAST;

    let mut text = TextRenderer::new();
    let art = Rect::new(0.0, 0.0, 320.0, 180.0);
    for name in Theme::ALL {
        let theme = Theme::named(name).expect("every shipped theme resolves");
        for reduce_transparency in [false, true] {
            let theme = theme.clone().with_reduce_transparency(reduce_transparency);
            let metrics = MediaMetrics::new(&theme);
            let live_fill = theme.tokens.colors.semantic.danger;
            let live_ink =
                theme
                    .tokens
                    .colors
                    .ink_on(live_fill, theme.text_primary, ON_FILL_MIN_CONTRAST);
            // Material::Floating paints the opaque panel ground when no backdrop
            // compositor is recording, also under reduced transparency.
            let duration_fill = crate::chrome::panel_ground(&theme);
            for (badge, fill, ink) in [
                (MediaBadge::Live, live_fill, live_ink),
                (
                    MediaBadge::Duration("12:04"),
                    duration_fill,
                    theme.text_primary,
                ),
            ] {
                let mut scene = Scene::new();
                paint_badge(
                    &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                    &metrics,
                    art,
                    badge,
                );
                for color in [fill, ink] {
                    assert!(
                        scene
                            .encoding()
                            .draw_data
                            .contains(&color.premultiply().to_rgba8().to_u32()),
                        "{name}, transparency reduced {reduce_transparency}: {badge:?} paints its fill and ink"
                    );
                }
                assert!(
                    contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST,
                    "{name}: {badge:?} ink is legible on its actual plate"
                );
            }
            let mut progress = Scene::new();
            paint_badge(
                &mut PaintCtx::new(&mut progress, Affine::IDENTITY, &mut text, &theme),
                &metrics,
                art,
                MediaBadge::Progress(0.5),
            );
            assert!(
                progress.encoding().draw_data.contains(
                    &theme
                        .tokens
                        .colors
                        .intent
                        .neutral_fill
                        .premultiply()
                        .to_rgba8()
                        .to_u32()
                ),
                "{name}: playback progress is the neutral fill, not the accent"
            );
        }
    }
}

#[test]
fn equal_column_rows_use_the_remainder_of_odd_viewport_widths() {
    for width in [521, 875, 901, 1201] {
        for scale in [1.0, 2.0] {
            let cards = (0..12)
                .map(|index| {
                    MediaCard::new(format!("card-{index}"), MediaSource::Placeholder)
                        .title("A full measured title")
                })
                .collect();
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(MediaGrid::new(cards)),
                (width, 1400),
                Theme::light().with_text_scale(scale),
            );
            let root = harness.inspection().root;
            let bounds = rect(&root);
            let cards: Vec<_> = root.children.iter().map(rect).collect();
            assert_eq!(cards.len(), 12);
            let first = cards[0];
            let row: Vec<_> = cards
                .iter()
                .copied()
                .take_while(|card| (card.y0 - first.y0).abs() < 0.5)
                .collect();
            assert!((first.x0 - bounds.x0).abs() < 0.5);
            assert!(
                (row.last().unwrap().x1 - bounds.x1).abs() < 0.5,
                "{width}px at {scale}x: full row must reach its edge: {row:?}"
            );
            let narrowest = row
                .iter()
                .map(|card| card.width())
                .fold(f64::INFINITY, f64::min);
            let widest = row.iter().map(|card| card.width()).fold(0.0, f64::max);
            assert!(
                widest - narrowest <= 1.0,
                "only pixel rounding varies equal tracks"
            );
            for pair in row.windows(2) {
                assert!(pair[0].x1 <= pair[1].x0);
            }
            for card in &cards {
                assert!(card.x0 >= bounds.x0 && card.x1 <= bounds.x1 + 0.5);
            }
            for complete_row in cards.chunks_exact(row.len()) {
                assert!((complete_row[0].x0 - bounds.x0).abs() < 0.5);
                assert!((complete_row.last().unwrap().x1 - bounds.x1).abs() < 0.5);
                for pair in complete_row.windows(2) {
                    assert!((pair[0].y0 - pair[1].y0).abs() < 0.5);
                    assert!(pair[0].x1 <= pair[1].x0);
                }
            }
        }
    }
}

#[test]
fn creator_avatar_loads_through_the_shared_store_and_scales_with_metadata() {
    use std::sync::Arc;
    let bytes = crate::png::encode_rgba8(2, 2, &[180; 16]).unwrap();
    let fetch: crate::image_store::Fetcher = Arc::new(move |_| Some(bytes.clone()));
    let store = crate::ImageStore::new(&fetch);
    assert!(store.get("https://example.test/creator.png").is_none());
    store.block_until_idle(tilo_test_env::hang_bound());
    assert!(store.get("https://example.test/creator.png").is_some());
    for theme in [
        Theme::dark(),
        Theme::light(),
        Theme::dark().with_text_scale(2.0),
    ] {
        let card = MediaCard::new("creator-card", MediaSource::Placeholder)
            .aspect(MediaAspect::Wide)
            .title("A video title")
            .meta("Ada Lovelace")
            .meta("Recorded yesterday")
            .creator("Ada Lovelace", "ada")
            .creator_image_url(store.clone(), "https://example.test/creator.png");
        let mut harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(card),
            (400, 600),
            theme.clone(),
        );
        harness.recompose().unwrap();
        let image = harness
            .a11y_nodes()
            .into_iter()
            .find_map(|(_, node)| {
                (node.role() == accesskit::Role::Image && node.label() == Some("Ada Lovelace"))
                    .then_some(node)
            })
            .expect("the creator is a semantic image, rather than a duplicate action");
        let bounds = image
            .bounds()
            .expect("the attribution has measured geometry");
        let side = crate::widget::AvatarSize::Small.side(&theme);
        assert!((bounds.width() - side).abs() < 0.1);
        assert!((bounds.height() - side).abs() < 0.1);
        assert!(
            bounds.y0 > 400.0 / MediaAspect::Wide.ratio(),
            "the avatar sits below the art and title"
        );
        assert!(
            bounds.y1 <= harness.inspection().root.rect[3],
            "metadata measurement includes the whole avatar"
        );
        assert_eq!(
            harness
                .scene()
                .encoding()
                .draw_tags
                .iter()
                .filter(|tag| **tag == vello_encoding::DrawTag::IMAGE)
                .count(),
            1,
            "the real decoded creator image is painted by AvatarFace"
        );
    }
}

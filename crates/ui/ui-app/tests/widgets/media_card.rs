use std::cell::Cell;
use std::rc::Rc;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::InspectionNode;
use tilo_ui_app::widget::media_card::*;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::MediaAspect;
use vello::kurbo::Point;
use vello::kurbo::Rect;
fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

tilo_test_env::scenario! {
    fn cards_justify_their_rows_and_paint_their_badges;

    phase justified_rows_span_the_width_and_keep_aspect;
    {
        let aspects = [1.5, 0.66, 1.0, 1.78, 1.33, 0.75, 1.5];
        let rects = justified_rows(&aspects, 900.0, 200.0, 4.0);
        assert_eq!(rects.len(), aspects.len());
        for (r, a) in rects.iter().zip(aspects) {
            assert!((r.width() / r.height() - a).abs() < 0.01, "aspect kept");
        }
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

    phase overlay_badges_paint_legible_ink_in_every_theme;
    {
        use tilo_ui_app::color::contrast_ratio;
        use tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST;

        let mut text = TextRenderer::new();
        let art = Rect::new(0.0, 0.0, 320.0, 180.0);
        for name in Theme::ALL {
            let theme = Theme::named(name).expect("every shipped theme resolves");
            for reduce_transparency in [false, true] {
                let theme = theme.clone().with_reduce_transparency(reduce_transparency);
                let live_fill = theme.tokens.colors.semantic.danger;
                let live_ink =
                    theme
                        .tokens
                        .colors
                        .ink_on(live_fill, theme.text_primary, ON_FILL_MIN_CONTRAST);
                let duration_fill = tilo_ui_base::chrome::panel_ground(&theme);
                for (badge, fill, ink) in [
                    (MediaBadge::Live, live_fill, live_ink),
                    (
                        MediaBadge::Duration("12:04"),
                        duration_fill,
                        theme.text_primary,
                    ),
                ] {
                    let mut scene = vello::Scene::new();
                    paint_badge_for_theme(
                        &mut PaintCtx::new(&mut scene, vello::kurbo::Affine::IDENTITY, &mut text, &theme),
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
                let mut progress = vello::Scene::new();
                paint_badge_for_theme(
                    &mut PaintCtx::new(&mut progress, vello::kurbo::Affine::IDENTITY, &mut text, &theme),
                    art,
                    MediaBadge::Progress(0.5),
                );
                assert!(
                    progress.encoding().draw_data.contains(
                        &theme
                            .tokens
                            .colors
                            .intent
                            .value_fill
                            .premultiply()
                            .to_rgba8()
                            .to_u32()
                    ),
                    "{name}: playback progress is the neutral fill, not the accent"
                );
            }
        }
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
    let harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
        let harness = Driver::new_with_theme(
            tilo_ui_app::State::new(),
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
        .on_hover_action(Icon::Play, "Play", move || p.set(p.get() + 1))
        .on_open(move || o.set(o.get() + 1));
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(MediaGrid::new(vec![card])),
        (300, 600),
        Theme::dark(),
    );
    let card = rect(&harness.inspection().root.children[0]);
    let art_centre = Point::new(card.center().x, card.y0 + card.width() / 2.0);
    harness.click_at(card.x0 + 10.0, card.y0 + 10.0);
    assert_eq!(opened.get(), 1);
    harness.click_at(art_centre.x, art_centre.y);
    assert_eq!((opened.get(), played.get()), (1, 1));
}

#[test]
fn creator_avatar_loads_through_the_shared_store_and_scales_with_metadata() {
    use std::sync::Arc;
    let bytes = tilo_ui_app::png::encode_rgba8(2, 2, &[180; 16]).unwrap();
    let fetch: tilo_ui_base::image_store::Fetcher = Arc::new(move |_| Some(bytes.clone()));
    let store = tilo_ui_base::ImageStore::new(&fetch);
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
        let mut harness = Driver::new_with_theme(
            tilo_ui_app::State::new(),
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
        let side = tilo_ui_base::widget::AvatarSize::Small.side(&theme);
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

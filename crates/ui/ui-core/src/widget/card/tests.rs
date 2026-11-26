use super::*;
use crate::widget::ButtonRole;
use crate::widget::Container;

fn text_card(text: &str) -> Element {
    Box::new(Card::new(Box::new(Text::new(text))))
}

fn lay_out_grid(mut g: CardGrid, w: f64, h: f64) -> CardGrid {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = g.build_layout(&mut taffy, &theme, &mut map);
    let mut bindings = tilo_ui_base::widget::bound_nodes::BoundNodeMap::new();
    unsafe {
        bindings.rebuild(&g, &taffy, node, &map);
    }
    unsafe {
        tilo_ui_base::widget::compute_widget_layout(
            &mut taffy,
            node,
            Size {
                width: taffy::AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(w)),
                height: taffy::AvailableSpace::Definite(tilo_ui_base::num::f32_of_f64(h)),
            },
            &map,
            &bindings,
            &mut tr,
        )
    }
    .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);
    g
}

tilo_test_env::scenario! {
    /// Card columns, grids and heights.
    /// Each phase was a test of its own and builds its own fixture.
    fn cards_reflow_and_keep_their_content_height;


    phase card_grid_resolves_columns_from_semantic_width_and_content_rows;
    {
        let theme = Theme::dark();
        let min = CardWidth::Regular.min_width(&theme);
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let width = 3.0 * min + 2.0 * gap + 20.0;
        let g = lay_out_grid(
            CardGrid::new((0..6).map(|i| text_card(&format!("card {i}"))).collect()),
            width,
            2000.0,
        );
        let first_row: Vec<_> = g
            .cards
            .iter()
            .filter(|card| card.get_rect().y0 == g.rect.y0)
            .collect();
        assert_eq!(first_row.len(), 3);
        let gap_between = g.cards[1].get_rect().x0 - g.cards[0].get_rect().x1;
        assert!((gap_between - gap).abs() <= 1.0);
        for card in &g.cards {
            assert!(
                (card.get_rect().width() - g.cards[0].get_rect().width()).abs() <= 1.0,
                "{:?}",
                g.cards.iter().map(|c| c.get_rect()).collect::<Vec<_>>()
            );
        }
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        assert!(g.cards[0].get_rect().height() < line * 4.0);

        let narrow = lay_out_grid(
            CardGrid::new((0..4).map(|i| text_card(&format!("card {i}"))).collect()),
            min - 10.0,
            2000.0,
        );
        assert_eq!(narrow.columns_for_width(min - 10.0), 1);
        assert!((narrow.cards[0].get_rect().width() - (min - 10.0)).abs() < 0.5);

        let wide = CardGrid::new((0..6).map(|i| text_card(&format!("{i}"))).collect())
            .with_card_width(CardWidth::Wide);
        assert!(CardWidth::Wide.min_width(&theme) > min);
        assert!(CardWidth::Compact.min_width(&theme) < min);
        assert!(CardWidth::Regular.min_width(&Theme::dark().with_text_scale(2.0)) > min * 1.5);
        let wide = lay_out_grid(wide, width, 2000.0);
        assert!(wide.columns_for_width(width) < 3);
    }

    phase a_card_led_by_its_art_shows_the_art_first_and_its_caption_under_it;
    {
        let theme = Theme::dark();
        let art = || Box::new(Container::new().with_no_padding().with_height(120.0)) as Element;
        let actions = || {
            Box::new(
                Actions::new(Button::new("use", "Use").role(ButtonRole::Primary))
                    .secondary(Button::new("customize", "Customize")),
            ) as Element
        };
        let card = Card::with_media(art())
            .with_title("Placeholder")
            .with_title_action("Dark graphite", actions());
        let g = lay_out_grid(CardGrid::new(vec![Box::new(card)]), 400.0, 1000.0);
        let card = g.cards[0].get_rect();
        let parts = g.cards[0].children();
        assert_eq!(
            parts.len(),
            2,
            "one header, replaced in place, under the art"
        );
        let media = parts[0].get_rect();
        let header = parts[1].get_rect();
        let pad = f64::from(theme.tokens.spacing.rhythm.section);
        assert!((media.height() - 120.0).abs() < 0.5, "{media:?}");
        assert!(
            (media.y0 - (card.y0 + pad)).abs() < 0.5,
            "the art leads the card"
        );
        assert!(
            (media.width() - (card.width() - 2.0 * pad)).abs() < 0.5,
            "the art spans the inner width: {media:?} in {card:?}"
        );
        assert!(header.y0 >= media.y1, "the caption sits under the art");
        let caption = parts[1].children();
        assert!(
            caption[1].get_rect().x1 <= header.x1 + 0.5
                && caption[1].get_rect().x0 > caption[0].get_rect().x0,
            "the actions trail the name on the same row"
        );
    }

}

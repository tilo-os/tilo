//! The bar's title against the page's own: one title on screen, and the page's while it shows.

use super::*;

/// The column's header title, as the row laid it out.
fn title_rect(h: &FrameHarness) -> Rect {
    h.root::<NavShell>()
        .part(Part::Title(Column::Content))
        .map_or(Rect::ZERO, Widget::get_rect)
}

/// A page of `content` under the bar of `title`.
fn page(title: &str, content: Element) -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::Document)
        .content(content)
        .content_header(title, Vec::new())
}

fn heading(words: &str, role: crate::TextRole) -> Element {
    Box::new(Text::new(words).with_role(role))
}

#[test]
fn the_bar_leaves_the_title_to_the_pages_own_heading() {
    let theme = Theme::dark();
    let mut bodies = Vec::new();
    for (what, content, shown) in [
        (
            "the page shows the same words as its large title",
            Box::new(crate::widget::Column::new().push(heading(
                "Call the landlord about the boiler",
                crate::TextRole::LargeTitle,
            ))) as Element,
            false,
        ),
        (
            "the page shows other words",
            Box::new(
                crate::widget::Column::new()
                    .push(heading("Call the plumber", crate::TextRole::LargeTitle)),
            ),
            true,
        ),
        (
            "the page says the words as a paragraph",
            Box::new(crate::widget::Column::new().push(heading(
                "Call the landlord about the boiler",
                crate::TextRole::Body,
            ))),
            true,
        ),
    ] {
        let mut h = harness(
            page("Call the landlord about the boiler", content),
            390,
            844,
            theme.clone(),
        );
        settle(&mut h, 390, 844);
        let title = title_rect(&h);
        assert_eq!(
            title.width() > 0.0,
            shown,
            "{what}: the bar's title {title:?}"
        );
        bodies.push(geometry(&h).body(Column::Content));
    }
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "the row keeps its height either way, so the body does not jump: {bodies:?}"
    );
}

#[test]
fn the_bar_takes_the_title_back_once_the_heading_scrolls_away() {
    let tall = Box::new(
        crate::widget::Column::new()
            .push(heading(
                "Call the landlord about the boiler",
                crate::TextRole::LargeTitle,
            ))
            .push(Box::new(crate::widget::Spacer::fixed(1200.0))),
    ) as Element;
    let scrolled = crate::widget::Scrollable::new(crate::widget::ScrollAxis::Vertical)
        .fill_parent_height()
        .with_child(tall);
    let mut h = harness(
        page("Call the landlord about the boiler", Box::new(scrolled)),
        390,
        844,
        Theme::dark(),
    );
    settle(&mut h, 390, 844);
    assert_eq!(
        title_rect(&h).width(),
        0.0,
        "the heading is on screen, so the bar shows only the way back"
    );
    let body = geometry(&h).body(Column::Content);
    h.root_mut::<NavShell>()
        .content_mut::<crate::widget::Scrollable>()
        .expect("the page scrolls")
        .set_offset_y(400.0);
    settle(&mut h, 390, 844);
    assert!(
        title_rect(&h).width() > 0.0,
        "the heading is out of view: the bar names the page again"
    );
    assert_eq!(
        geometry(&h).body(Column::Content),
        body,
        "and the body stayed where it stood"
    );
}

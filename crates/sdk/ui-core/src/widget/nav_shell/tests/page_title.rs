//! The bar's title against the page's own: one title on screen, and the page's while it shows.

use super::*;

fn title_rect(h: &FrameHarness) -> Rect {
    h.root::<NavShell>()
        .part(Part::Title(Column::Content))
        .map_or(Rect::ZERO, Widget::get_rect)
}

fn page(title: &str, content: Element) -> NavShell {
    NavShell::new()
        .content_kind(ContentKind::Document)
        .content(content)
        .content_header(title, Vec::new())
}

fn heading(words: &str, role: crate::TextRole) -> Element {
    Box::new(Text::new(words).with_role(role))
}

/// The accessible names the frame exports, in tree order: what a page is called, and by what.
fn outcomes(h: &FrameHarness) -> Vec<String> {
    h.a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
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

/// A page that edits its title in a field shows one title, the field's; the page still has a
/// name, though: the bar's heading stands where the field does, so assistive technology reads
/// the page's title as it reads it from a heading the page sets itself.
#[test]
fn a_page_that_edits_its_title_in_a_field_still_names_itself() {
    let words = "Call the landlord about the boiler";
    let field = Box::new(
        crate::widget::Column::new().push(Box::new(
            crate::widget::TextInput::local()
                .with_initial_value(words)
                .with_text_role(crate::TextRole::LargeTitle),
        )),
    ) as Element;
    let mut h = harness(page(words, field), 390, 844, Theme::dark());
    settle(&mut h, 390, 844);
    assert_eq!(
        title_rect(&h).width(),
        0.0,
        "the page's field shows the title, so the bar draws none"
    );
    let named = h
        .a11y_node_named(words)
        .unwrap_or_else(|| panic!("the page's title stands as its name: {:?}", outcomes(&h)));
    let bounds = h
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == named)
        .and_then(|(_, node)| node.bounds())
        .expect("the page's name stands where the field does");
    let body = geometry(&h).body(Column::Content);
    assert!(
        bounds.x0 >= body.x0 - 1.0 && bounds.y0 >= body.y0 - 1.0,
        "the page's name stands inside the page: {bounds:?} in {body:?}"
    );
}

/// A page that shows its title as a heading of its own names itself by that heading, and the
/// bar's heading goes: one name, not two.
#[test]
fn a_page_that_shows_its_title_as_its_heading_names_itself_once() {
    let words = "Call the landlord about the boiler";
    let content =
        Box::new(crate::widget::Column::new().push(heading(words, crate::TextRole::LargeTitle)))
            as Element;
    let mut h = harness(page(words, content), 390, 844, Theme::dark());
    settle(&mut h, 390, 844);
    let headings = h
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some(words))
        .count();
    assert_eq!(
        headings,
        1,
        "the page's heading is the one name it has: {:?}",
        outcomes(&h)
    );
}

/// An item opened in place among a list's rows titles that item, never the page: the column
/// keeps its own title over the list.
#[test]
fn an_item_opened_among_a_lists_rows_leaves_the_column_its_title() {
    let row =
        ListRow(crate::widget::Column::new().push(heading("Today", crate::TextRole::LargeTitle)));
    let mut h = harness(
        page(
            "Today",
            Box::new(crate::widget::Column::new().push(Box::new(row))),
        ),
        390,
        844,
        Theme::dark(),
    );
    settle(&mut h, 390, 844);
    assert!(
        title_rect(&h).width() > 0.0,
        "the column's title stands over the list"
    );
}

struct ListRow(crate::widget::Column);

impl crate::widget::Decorator for ListRow {
    type Inner = crate::widget::Column;

    fn inner(&self) -> &crate::widget::Column {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut crate::widget::Column {
        &mut self.0
    }

    fn is_item_opened_in_place(&self) -> bool {
        true
    }
}

//! What a layout check and a page read off a driven window: the bar the window shows over the app,
//! the nodes its frames built, a window laid out for a person and a size in one frame, and the
//! pictures a page shows of its painted parts.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use crate::headless::Driver;
use crate::widget::NavShell;
use tilo_ui_base::interaction::InteractionEnvironment;
use tilo_ui_base::theme::Theme;

fn still() -> Theme {
    Theme::dark().with_reduce_motion(true)
}

/// A notes window whose content's header row holds a Share button.
fn notes_shell() -> NavShell {
    NavShell::new()
        .sidebar(Box::new(crate::widget::Column::new()))
        .content(Box::new(crate::widget::Column::new()))
        .sidebar_header("Notebooks", Vec::new())
        .content_header(
            "Groceries",
            vec![Box::new(crate::widget::Button::new("share", "Share"))],
        )
}

#[test]
fn a_header_row_holding_the_frames_bar_is_the_bar_and_no_bar_stands_above_it() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let mut driver = Driver::in_named_window_frame(
        crate::State::new(),
        Box::new(notes_shell()),
        "Groceries",
        (1280, 800),
        still(),
    );
    driver.relayout();
    assert!(driver.row_holds_bar(), "the notes header row is the bar");
    assert!(
        driver.window_bar_height().abs() < 0.5,
        "no bar of the frame's own stands over the row: {}",
        driver.window_bar_height()
    );
    let bar = driver.window_bar_layout();
    let share = bar
        .controls
        .iter()
        .find(|control| control.name == "Share")
        .unwrap_or_else(|| panic!("the bar shows the row's Share: {:?}", bar.controls));
    assert_eq!(share.part, "row", "it stands in the row that holds the bar");
    assert!(
        share.rect[1] < 60.0,
        "the bar is the window's top: {:?}",
        share.rect
    );
}

#[test]
fn a_body_with_no_header_row_stands_under_the_frames_own_bar_naming_the_window() {
    let _landed = tilo_ui_base::motion::testing::settled();
    let mut driver = Driver::in_named_window_frame(
        crate::State::new(),
        Box::new(crate::widget::Text::new("A long shopping list")),
        "Groceries",
        (640, 480),
        still(),
    );
    // The window as it first shows: its bar names it from the frame that lays it out at its
    // size, with nothing asking for the tree to be built again.
    driver.relayout();
    let bar = driver.window_bar_height();
    assert!(
        bar > 20.0,
        "the frame stands a bar of its own over a body that draws none: {bar}"
    );
    assert!(!driver.row_holds_bar());
    assert!(
        driver.window_header_row_height().abs() < 0.5,
        "and no row of the app's under it"
    );
    let named: Vec<(String, String, [f64; 4])> = driver
        .inspection()
        .root
        .nodes()
        .into_iter()
        .filter_map(|node| {
            let name = node.semantics.as_ref()?.name.clone()?;
            Some((node.widget.clone(), name, node.rect))
        })
        .collect();
    assert!(
        named
            .iter()
            .any(|(widget, name, _)| widget == "WindowFrame" && name == "Groceries"),
        "assistive technology reads the window by its name: {named:?}"
    );
    assert!(
        named.iter().any(|(widget, name, [_, y, _, height])| {
            widget == "Text" && name == "Groceries" && *height > 0.0 && y + height <= bar + 0.5
        }),
        "the frame's own bar shows the window's name: {named:?}"
    );
    assert!(
        driver
            .window_bar_layout()
            .controls
            .iter()
            .all(|control| control.part == "frame"),
        "what the bar shows is the frame's own"
    );
}

#[test]
fn a_window_laid_out_for_a_finger_at_a_new_size_builds_its_nodes_once() {
    let mut driver = Driver::new(crate::State::new(), Box::new(notes_shell()), (1280, 800));
    let built = driver.layout_nodes_built();
    assert!(built > 0, "the first frame builds the window's tree");
    driver.relayout();
    assert_eq!(
        driver.layout_nodes_built(),
        built,
        "a frame that keeps its tree builds nothing"
    );
    let frames = driver.relayout_frames();
    driver
        .relayout_for((390, 844), still(), InteractionEnvironment::TOUCH)
        .unwrap();
    assert_eq!(driver.viewport(), (390, 844));
    assert_eq!(
        driver.interaction_environment(),
        InteractionEnvironment::TOUCH
    );
    assert!(
        driver.layout_nodes_built() > built,
        "the window is built again for a finger"
    );
    assert_eq!(
        driver.relayout_frames(),
        frames + 1,
        "in one frame, at the size it is read at"
    );
    assert!(
        driver.pending_visual_work().is_empty(),
        "nothing in the window waits on work to replace a placeholder"
    );
}

type Picture = (String, Option<std::sync::Arc<str>>);

/// Every picture a page shows of `composition`, by the path of its part.
fn pictures(composition: &crate::composition::Composition) -> Vec<Picture> {
    fn visit(node: &crate::composition::Composed, out: &mut Vec<Picture>) {
        if let Some(picture) = &node.picture {
            out.push((node.path.clone(), picture.data.clone()));
        }
        for child in &node.children {
            visit(child, out);
        }
    }
    let mut out = Vec::new();
    for node in std::iter::once(&composition.root).chain(&composition.layers) {
        visit(node, &mut out);
    }
    out
}

#[test]
fn a_page_shows_a_charts_paint_as_its_picture_drawn_once_while_it_stays() {
    let chart = crate::widget::Chart::new(vec![crate::widget::Series::new(
        vec![1.0, 4.0, 2.0],
        vello::peniko::Color::WHITE,
    )])
    .titled("Weekly revenue");
    let mut driver = Driver::new(crate::State::new(), Box::new(chart), (400, 200));
    let mut first = driver.composition();
    driver.paint_pictures(&mut first);
    let drawn = pictures(&first);
    assert!(!drawn.is_empty(), "the chart is a picture on the page");
    for (path, picture) in &drawn {
        let picture = picture
            .as_deref()
            .unwrap_or_else(|| panic!("{path} is drawn for the page"));
        assert!(
            picture.starts_with("data:image/png;base64,"),
            "{path}'s picture is a PNG"
        );
    }
    let mut again = driver.composition();
    driver.paint_pictures(&mut again);
    let kept = pictures(&again);
    assert_eq!(kept.len(), drawn.len());
    for ((path, before), (_, after)) in drawn.iter().zip(&kept) {
        assert!(
            before
                .as_ref()
                .zip(after.as_ref())
                .is_some_and(|(before, after)| std::sync::Arc::ptr_eq(before, after)),
            "{path} is not drawn again while it shows the same"
        );
    }
}

/// A part that paints nothing is no picture: a page shows nothing where it stands, never an
/// image of nothing.
#[test]
fn a_part_that_paints_nothing_is_no_picture() {
    // A region holding nothing: a box, and no child to draw in it.
    let mut driver = Driver::new(
        crate::State::new(),
        Box::new(tilo_ui_base::widget::Spacer::fixed(24.0)),
        (200, 120),
    );
    let mut composition = driver.composition();
    assert!(
        composition.root.picture.is_some(),
        "the walk takes a part with no words for its paint"
    );
    assert!(
        composition.root.rect[2] > 0.0 && composition.root.rect[3] > 0.0,
        "it stands with an area"
    );
    driver.paint_pictures(&mut composition);
    assert!(
        composition.root.picture.is_none(),
        "a part that paints nothing is no picture: {}",
        composition.root.path
    );
    assert!(
        pictures(&composition).is_empty(),
        "nothing on the page is drawn for nothing: {:?}",
        pictures(&composition)
    );
    let html = crate::composition::page::Page::new().render(&composition);
    assert!(
        !html.contains("tilo-paint"),
        "the page shows no picture of nothing: {html}"
    );
}

/// A control shown in one state is a picture of itself whatever it paints now: the window hides it
/// from assistive technology because what it shows is a picture, not one to use.
#[test]
fn a_part_the_window_hides_from_assistive_technology_stays_a_picture() {
    // A sample holding nothing: what makes it a picture is the window's saying so, not the paint
    // it would have had.
    let sample = crate::widget::InState::new(
        crate::widget::shown::State::Rest,
        Box::new(tilo_ui_base::widget::Spacer::fixed(24.0)),
    );
    assert!(
        tilo_ui_base::widget::Widget::accessibility_hidden(&sample),
        "the window hides the sample from assistive technology"
    );
    let mut driver = Driver::new(crate::State::new(), Box::new(sample), (200, 120));
    let mut composition = driver.composition();
    driver.paint_pictures(&mut composition);
    assert!(
        composition.root.picture.is_some(),
        "the sample is a picture of itself, painted or not: {}",
        composition.root.path
    );
    assert!(
        pictures(&composition)
            .iter()
            .all(|(_, picture)| picture.is_none()),
        "nothing painted it, so no host drew it"
    );
}

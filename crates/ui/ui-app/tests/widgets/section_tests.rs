use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::InspectionNode;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::ListRow;
use tilo_ui::widget::internals::section::*;
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::chrome::RowJoin;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

fn rows_of(section: &mut Section) -> Vec<&mut ListRow> {
    section
        .parts
        .children
        .iter_mut()
        .filter_map(|c| c.as_any_mut().and_then(|a| a.downcast_mut::<ListRow>()))
        .collect()
}

#[test]
fn items_sit_a_sibling_step_apart_and_the_header_closer() {
    let theme = Theme::dark();
    let rhythm = theme.tokens.spacing.rhythm;
    let section = Section::new(
        (0..3)
            .map(|i| Box::new(Text::new(&format!("file {i}"))) as Element)
            .collect(),
    )
    .with_header(
        SectionHeader::heading("Changes")
            .count(3)
            .action(Button::new("stage-all", "Stage all")),
    );
    let harness =
        Driver::new_with_theme(tilo_ui::State::new(), Box::new(section), (500, 400), theme);
    let root = harness.inspection().root;
    let header = &root.children[0];
    let items = &root.children[1..];
    assert!((rect(&items[0]).y0 - rect(header).y1 - f64::from(rhythm.related)).abs() < 0.5);
    assert!((rect(&items[1]).y0 - rect(&items[0]).y1 - f64::from(rhythm.sibling)).abs() < 0.5);
    assert_eq!(header.children.len(), 3);
    let title = rect(&header.children[0]);
    let count = rect(&header.children[1]);
    let action = rect(&header.children[2]);
    assert!(title.x0.abs() < 0.5, "title on the edge: {title:?}");
    assert!(count.x0 > title.x1);
    assert!(
        (action.x1 - 500.0).abs() < 1.0,
        "the label, not its lift, ends on the edge: {action:?}"
    );
    assert!(
        (count.center().y - title.center().y).abs() < 1.0,
        "one centre line"
    );
}

#[test]
fn list_rows_in_a_section_are_one_list() {
    let section = Section::titled(
        "People",
        vec![
            Box::new(ListRow::new("a", "Ada")),
            Box::new(ListRow::new("b", "Grace").selected(true)),
            Box::new(ListRow::new("c", "Linus")),
            Box::new(Text::new("3 people")),
        ],
    );
    let harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(section),
        (500, 400),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let rows: Vec<Rect> = root.children[1..4].iter().map(rect).collect();
    assert!((rows[1].y0 - rows[0].y1).abs() < 0.5, "rows touch");
    assert!((rows[2].y0 - rows[1].y1).abs() < 0.5);
    let footer = rect(&root.children[4]);
    let sibling = f64::from(Theme::dark().tokens.spacing.rhythm.sibling);
    assert!((footer.y0 - rows[2].y1 - sibling).abs() < 0.5);
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace").selected(true)),
        Box::new(ListRow::new("c", "Linus")),
        Box::new(ListRow::new("d", "Mary")),
    ]);
    section.sync_row_join();
    let flags: Vec<(bool, bool)> = rows_of(&mut section)
        .into_iter()
        .map(|row| row.separator_state())
        .collect();
    assert_eq!(flags, [(false, false); 4]);
}

tilo_test_env::scenario! {
    /// Section rows and headings that are laid out without a host.
    /// Each phase was a test of its own and builds its own fixture.
    fn rows_join_share_a_column_and_headings_count;




    phase adjacent_selected_rows_join_into_one_lift;
    {
        let mut section = Section::new(vec![
            Box::new(ListRow::new("a", "Ada").selected(true)),
            Box::new(ListRow::new("b", "Grace").selected(true)),
            Box::new(ListRow::new("c", "Linus").selected(true)),
            Box::new(ListRow::new("d", "Mary")),
        ]);
        section.sync_row_join();
        let joins: Vec<RowJoin> = rows_of(&mut section)
            .into_iter()
            .map(|row| row.join())
            .collect();
        assert_eq!(
            joins,
            [
                RowJoin::new(false, true),
                RowJoin::new(true, true),
                RowJoin::new(true, false),
                RowJoin::new(true, false),
            ]
        );
    }

    phase a_heading_counts_one_row_as_nothing;
    // A heading counts its rows where the number says something they do not: one row restates
    // itself with "1", while two say how many and a run cut short says how many it left out.
    {
        assert_eq!(heading_count(1, None), None, "one row says nothing");
        assert_eq!(heading_count(0, None), None, "no rows say nothing");
        assert_eq!(heading_count(2, None), Some(2), "two rows say how many");
        assert_eq!(heading_count(9, None), Some(9));
        assert_eq!(
            heading_count(1, Some(1)),
            None,
            "a run of one, all shown, says nothing"
        );
        assert_eq!(
            heading_count(0, Some(1)),
            Some(1),
            "a run cut short says how many it left out, even at one"
        );
        assert_eq!(
            heading_count(3, Some(20)),
            Some(20),
            "and how many it holds"
        );
    }
}

/// A disclosure only shows what the page already holds, so the native app
/// contract finds its header bound as a view-local control, addressed by
/// its title or by the id a page gives it.
#[test]
fn a_disclosure_header_is_a_bound_view_local_control() {
    let harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(
            tilo_ui::widget::Column::new()
                .with_no_padding()
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("a", "Ada"))])
                        .with_header(SectionHeader::group("Details"))
                        .collapsed(),
                ))
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("b", "Grace"))])
                        .with_header(SectionHeader::group("Details"))
                        .collapsed()
                        .disclosure_id("wired-details"),
                )),
        ),
        (400, 300),
        tilo_ui_base::theme::Theme::light(),
    );
    let audit = harness.native_app_audit();
    assert!(audit.issues.is_empty(), "{audit:?}");
    fn targets(node: &InspectionNode, out: &mut Vec<String>) {
        out.extend(
            node.semantics
                .as_ref()
                .and_then(|s| s.action_target.clone()),
        );
        for child in &node.children {
            targets(child, out);
        }
    }
    let mut ids = Vec::new();
    targets(&harness.inspection().root, &mut ids);
    assert!(ids.iter().any(|id| id == "disclosure:Details"), "{ids:?}");
    assert!(ids.iter().any(|id| id == "wired-details"), "{ids:?}");
}

#[test]
fn a_collapsible_section_hides_its_items_and_toggles_from_its_header() {
    let opened = std::rc::Rc::new(std::cell::Cell::new(None));
    let seen = std::rc::Rc::clone(&opened);
    let mut section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace")),
    ])
    .with_header(SectionHeader::group("Windows").count(2))
    .collapsed()
    .on_expand(move |open| seen.set(Some(open)));
    assert!(!section.is_expanded());
    assert_eq!(Widget::children(&section).len(), 1, "only the header");
    let header = section.test_parts_mut().children[0].as_mut();
    let node = header.accessibility().unwrap();
    assert_eq!(node.role, Role::Button);
    assert_eq!(node.state.expanded, Some(false));
    assert!(header.is_focusable());
    let mut doc = AutoCommit::new();
    assert!(header.perform_action(
        &tilo_ui_base::a11y::Action::Activate,
        &[],
        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(header.take_structure_dirty(), "the section relays out");
    assert_eq!(opened.get(), Some(true));
    assert!(section.is_expanded());
    assert_eq!(Widget::children(&section).len(), 3);

    let harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(
            tilo_ui::widget::Column::new()
                .with_no_padding()
                .push(Box::new(
                    Section::new(vec![Box::new(ListRow::new("a", "Ada"))])
                        .with_header(SectionHeader::group("Windows"))
                        .collapsed(),
                )),
        ),
        (400, 300),
        Theme::dark(),
    );
    let root = harness.inspection().root;
    let section = &root.children[0];
    assert_eq!(section.children.len(), 1);
    assert!(
        (rect(section).height() - rect(&section.children[0]).height()).abs() < 0.5,
        "a collapsed section is as tall as its header"
    );
}

#[test]
fn headings_take_their_role_and_follow_the_text_scale() {
    let height = |header: SectionHeader, scale: f32| {
        let harness = Driver::new_with_theme(
            tilo_ui::State::new(),
            Box::new(Section::new(vec![Box::new(Text::new("body"))]).with_header(header)),
            (400, 300),
            Theme::dark().with_text_scale(scale),
        );
        harness.inspection().root.children[0].children[0].rect[3]
    };
    let theme = Theme::dark();
    let heading = height(SectionHeader::heading("Library"), 1.0);
    let group = height(SectionHeader::group("Recently added"), 1.0);
    assert!((heading - f64::from(theme.text(TextRole::Heading).line_height_px())).abs() < 1.0);
    assert!((group - f64::from(theme.text(TextRole::Subheading).line_height_px())).abs() < 1.0);
    assert!(height(SectionHeader::heading("Library"), 2.0) > heading * 1.8);
    let plain = Section::new(vec![Box::new(Text::new("one"))]);
    assert_eq!(plain.test_parts().children.len(), 1);
}

#[test]
fn a_section_says_whether_the_pointer_is_over_it() {
    let over = Rc::new(Cell::new(false));
    let section = Section::new(vec![
        Box::new(ListRow::new("a", "Ada")),
        Box::new(ListRow::new("b", "Grace")),
    ])
    .pointer_over(Rc::clone(&over));
    let mut harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(
            tilo_ui::widget::Column::new()
                .with_no_padding()
                .push(Box::new(section)),
        ),
        (500, 400),
        Theme::dark(),
    );
    let whole = rect(&harness.inspection().root.children[0]);
    let rows = whole;
    assert!(whole.y1 < 380.0, "the section is shorter than the window");
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
        rows.center().x,
        rows.center().y,
    ));
    assert!(over.get(), "over a row");
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
        10.0,
        whole.y1 + 10.0,
    ));
    assert!(!over.get(), "below the section");
    harness.send(&tilo_ui_base::synthetic_input::cursor_moved(
        rows.center().x,
        rows.center().y,
    ));
    harness.send(&WindowEvent::CursorLeft {
        device_id: winit::event::DeviceId::dummy(),
    });
    assert!(!over.get(), "the pointer left the window");
}

/// A player, a web page and a page rail in a section stand at their own shape and push what
/// follows them down, rather than being zero tall in the section and painting over the next.
#[test]
fn a_player_a_page_and_a_rail_push_what_follows_down_in_a_section() {
    use tilo_ui::widget::{
        DeclaredRail, DeclaredVideo, PageSidebarProps, VideoPlayerProps, WebView, WebViewProps,
    };
    use tilo_ui_base::component::Described;
    let theme = Theme::light();
    let views: Vec<(&str, Element)> = vec![
        (
            "player",
            Box::new(DeclaredVideo::from_props("v", VideoPlayerProps::default())),
        ),
        (
            "web page",
            Box::new(WebView::from_props("w", WebViewProps::default())),
        ),
        (
            "rail",
            Box::new(DeclaredRail::from_props(
                "r",
                PageSidebarProps {
                    aspect: 1.414,
                    count: 3,
                    ..PageSidebarProps::default()
                },
            )),
        ),
    ];
    for (what, view) in views {
        let section = Section::new(vec![
            Box::new(Text::new("Above")) as Element,
            view,
            Box::new(Text::new("Below")) as Element,
        ]);
        let column = tilo_ui::widget::arrange::Stack::stack()
            .push(Box::new(section))
            .push(Box::new(Text::new("Next section")));
        let harness = Driver::new_with_theme(
            tilo_ui::State::new(),
            Box::new(tilo_ui::widget::Scrollable::new_vertical().with_child(Box::new(column))),
            (640, 400),
            theme.clone(),
        );
        let root = harness.inspection().root;
        let section = section_rect(&root).expect("the section");
        let shown = section_child(&root, 1).expect("the view");
        assert!(
            shown.width() > 0.0 && shown.height() >= shown.width() * 0.5,
            "the {what} stands at its own shape: {shown:?}"
        );
        assert!(
            shown.y1 <= section.y1 + 0.5,
            "the {what} stands inside its section: {shown:?} in {section:?}"
        );
        let below = section_child(&root, 2).expect("the text below");
        assert!(
            below.y0 >= shown.y1,
            "what follows the {what} starts under it: {below:?} after {shown:?}"
        );
    }
}

/// The first section under `node`, laid out.
fn section_rect(node: &InspectionNode) -> Option<Rect> {
    if node.widget == "Section" {
        return Some(rect(node));
    }
    node.children.iter().find_map(section_rect)
}

/// Child `index` of the first section under `node`, laid out.
fn section_child(node: &InspectionNode, index: usize) -> Option<Rect> {
    if node.widget == "Section" {
        return node.children.get(index).map(rect);
    }
    node.children
        .iter()
        .find_map(|child| section_child(child, index))
}

/// A view placed straight in a section with a fit that asks for an `aspect` is as wide as the
/// section and as tall as the ratio says, as it is in a stack.
#[test]
fn an_aspect_shapes_a_view_placed_straight_in_a_section() {
    let mut fit = tilo_ui::widget::arrange::Fit::hug();
    fit.aspect = Some(2.0);
    let section = Section::new(vec![
        Box::new(Text::new("Above")) as Element,
        Box::new(tilo_ui::widget::Fitted::new(
            Box::new(tilo_ui_base::widget::Spacer::new()),
            fit,
        )),
        Box::new(Text::new("Below")) as Element,
    ]);
    let harness = Driver::new_with_theme(
        tilo_ui::State::new(),
        Box::new(tilo_ui::widget::Scrollable::new_vertical().with_child(Box::new(section))),
        (640, 400),
        Theme::light(),
    );
    let root = harness.inspection().root;
    let shaped = section_child(&root, 1).expect("the shaped view");
    let section = section_rect(&root).expect("the section");
    assert!(
        (shaped.width() - section.width()).abs() < 0.5,
        "as wide as the section: {shaped:?} in {section:?}"
    );
    assert!(
        (shaped.height() - shaped.width() / 2.0).abs() < 1.0,
        "as tall as its ratio says: {shaped:?}"
    );
    let below = section_child(&root, 2).expect("the text below");
    assert!(below.y0 >= shaped.y1, "what follows starts under it");
}

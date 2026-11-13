//! What a docked column reports when what stands in it changes.

use super::{Docked, DockedProps};
use crate::component::Described;
use crate::widget::{StructureChange, Widget};

struct InPlace {
    pending: bool,
}

impl Widget for InPlace {
    crate::testing::inert_widget_methods!(leaf_layout, name = "InPlace");

    fn take_structure_dirty(&mut self) -> bool {
        self.pending
    }

    fn take_structure_change(&mut self) -> StructureChange {
        if std::mem::take(&mut self.pending) {
            StructureChange::Children
        } else {
            StructureChange::None
        }
    }
}

fn docked(pending: bool) -> Docked {
    Docked::from_props(
        "docked",
        DockedProps {
            content: Some(Box::new(InPlace { pending })),
            ..DockedProps::default()
        },
    )
}

#[test]
fn a_docked_column_reports_what_it_holds_changing_in_place() {
    let mut docked = docked(true);
    assert_eq!(
        Widget::take_structure_change(&mut docked),
        StructureChange::Children,
        "a column adds no structure of its own, so content that needs a remount keeps the window's tree"
    );
    assert_eq!(
        Widget::take_structure_change(&mut docked),
        StructureChange::None,
        "reading clears it"
    );
}

#[test]
fn a_bar_under_a_page_column_stands_where_its_content_is_read() {
    fn widths(node: &crate::headless::InspectionNode, out: &mut Vec<f64>) {
        if node.widget == "Page"
            && let Some(row) = node.children.first()
        {
            out.push(row.rect[2]);
        }
        for child in &node.children {
            widths(child, out);
        }
    }
    let measure = f64::from(crate::widget::arrange::MEASURE_REM)
        * f64::from(crate::widget::arrange::rem_of(&crate::theme::Theme::dark()));
    let page = || {
        Box::new(
            crate::widget::arrange::Page::new().push(Box::new(crate::widget::Text::new(
                "Words read at the measure",
            ))),
        ) as crate::widget::Element
    };
    let scroll = |content: crate::widget::Element| {
        Box::new(
            crate::widget::Scrollable::new_vertical()
                .fit_content_height()
                .with_child(Box::new(crate::widget::Stack::new().push(content))),
        ) as crate::widget::Element
    };
    let held = |content: crate::widget::Element| {
        let docked = Docked::from_props(
            "docked",
            DockedProps {
                content: Some(content),
                foot: Some(Box::new(crate::widget::Text::new("Add a note"))),
            },
        );
        let mut harness = crate::headless::FrameHarness::for_root(
            crate::State::new(),
            docked,
            (1600, 900),
            crate::theme::Theme::dark(),
        );
        harness.relayout();
        let mut found = Vec::new();
        widths(&harness.inspection().root, &mut found);
        found
    };
    for (shape, content) in [
        ("a page", page()),
        (
            "a page a host reached in a decorator",
            Box::new(crate::widget::Fitted::new(
                page(),
                crate::widget::arrange::Fit::fill(1.0),
            )) as crate::widget::Element,
        ),
        ("a scroll around a page", scroll(page())),
    ] {
        let found = held(content);
        assert_eq!(
            found.len(),
            2,
            "the content's page and the bar's, {shape}: {found:?}"
        );
        for width in found {
            assert!(
                (width - measure).abs() <= 1.0,
                "the bar keeps the page's measure, {shape}: {width} against {measure}"
            );
        }
    }
    let [across] = held(Box::new(InPlace { pending: false }))[..] else {
        panic!("a column that is no page still holds one page: the bar's");
    };
    assert!(across > measure * 2.0, "a column that is no page: {across}");
}

#[test]
fn a_column_standing_in_content_put_in_later_follows_that_content() {
    fn widths(node: &crate::headless::InspectionNode, out: &mut Vec<f64>) {
        if node.widget == "Page"
            && let Some(row) = node.children.first()
        {
            out.push(row.rect[2]);
        }
        for child in &node.children {
            widths(child, out);
        }
    }
    let measure = f64::from(crate::widget::arrange::MEASURE_REM)
        * f64::from(crate::widget::arrange::rem_of(&crate::theme::Theme::dark()));
    let mut docked = Docked::from_props(
        "docked",
        DockedProps {
            content: None,
            foot: Some(Box::new(crate::widget::Text::new("Add a note"))),
        },
    );
    let content = Box::new(
        crate::widget::Scrollable::new_vertical()
            .fit_content_height()
            .with_child(Box::new(crate::widget::Stack::new().push(Box::new(
                crate::widget::arrange::Page::new().push(Box::new(crate::widget::Text::new(
                    "Words read at the measure",
                ))),
            )))),
    ) as crate::widget::Element;
    let Some(views) = crate::component::Described::slot_views(&mut docked, "content") else {
        panic!("a column changes its content in place");
    };
    views.insert(0, content);
    let mut harness = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        docked,
        (1600, 900),
        crate::theme::Theme::dark(),
    );
    harness.relayout();
    let mut found = Vec::new();
    widths(&harness.inspection().root, &mut found);
    assert_eq!(
        found.len(),
        2,
        "the content's page and the bar's: {found:?}"
    );
    for width in found {
        assert!(
            (width - measure).abs() <= 1.0,
            "the bar keeps the page's measure put in later: {width} against {measure}"
        );
    }
}

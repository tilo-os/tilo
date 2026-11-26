//! What a docked column reports when what stands in it changes.

use super::{Docked, DockedProps};
use tilo_ui_base::component::Described;
use tilo_ui_base::{widget::StructureChange, widget::Widget};

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

tilo_test_env::scenario! {
    /// A column docked in content.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_docked_column_follows_its_content;

    phase a_docked_column_reports_what_it_holds_changing_in_place;
    {
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

    phase a_bar_under_a_page_column_stands_where_its_content_is_read;
    {
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
            * f64::from(crate::widget::arrange::rem_of(&tilo_ui_base::theme::Theme::dark()));
        let page = || {
            Box::new(
                crate::widget::arrange::Page::new().push(Box::new(crate::widget::Text::new(
                    "Words read at the measure",
                ))),
            ) as tilo_ui_base::widget::Element
        };
        let scroll = |content: tilo_ui_base::widget::Element| {
            Box::new(
                crate::widget::Scrollable::new_vertical()
                    .fit_content_height()
                    .with_child(Box::new(crate::widget::Stack::new().push(content))),
            ) as tilo_ui_base::widget::Element
        };
        let held = |content: tilo_ui_base::widget::Element| {
            let docked = Docked::from_props(
                "docked",
                DockedProps {
                    content: Some(content),
                    foot: Some(Box::new(crate::widget::Text::new("Add a note"))),
                },
            );
            let mut harness = crate::app::driver::Driver::for_root(
                crate::State::new(),
                docked,
                (1600, 900),
                tilo_ui_base::theme::Theme::dark(),
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
                )) as tilo_ui_base::widget::Element,
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

    phase a_column_in_content_put_in_later_follows_that_content;
    {
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
            * f64::from(crate::widget::arrange::rem_of(&tilo_ui_base::theme::Theme::dark()));
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
        ) as tilo_ui_base::widget::Element;
        let Some(views) = tilo_ui_base::component::Described::part_views(&mut docked, "content") else {
            panic!("a column changes its content in place");
        };
        views.insert(0, content);
        let mut harness = crate::app::driver::Driver::for_root(
            crate::State::new(),
            docked,
            (1600, 900),
            tilo_ui_base::theme::Theme::dark(),
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

    phase a_dock_with_nothing_at_its_foot_paints_no_bar;
    {
        fn pages(node: &crate::headless::InspectionNode, out: &mut usize) {
            if node.widget == "Page" {
                *out += 1;
            }
            for child in &node.children {
                pages(child, out);
            }
        }
        let mut harness = crate::app::driver::Driver::for_root(
            crate::State::new(),
            docked(false),
            (1600, 900),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let mut found = 0;
        pages(&harness.inspection().root, &mut found);
        assert_eq!(
            found, 0,
            "a dock with nothing at its foot paints no bar over its content"
        );
    }

    phase a_bar_given_to_a_dock_later_stands_at_its_foot;
    {
        fn pages(node: &crate::headless::InspectionNode, out: &mut usize) {
            if node.widget == "Page" {
                *out += 1;
            }
            for child in &node.children {
                pages(child, out);
            }
        }
        let mut docked = docked(false);
        let Some(views) = tilo_ui_base::component::Described::part_views(&mut docked, "foot") else {
            panic!("a column takes a bar after it was built");
        };
        views.insert(0, Box::new(crate::widget::Text::new("Outline")));
        let mut harness = crate::app::driver::Driver::for_root(
            crate::State::new(),
            docked,
            (1600, 900),
            tilo_ui_base::theme::Theme::dark(),
        );
        harness.relayout();
        let mut found = 0;
        pages(&harness.inspection().root, &mut found);
        assert_eq!(found, 1, "the bar stands at the column's foot: {found} pages");
    }
}

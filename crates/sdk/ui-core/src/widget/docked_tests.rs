//! What a docked column reports when what stands in it changes.

use super::{Docked, DockedProps};
use crate::component::Described;
use crate::widget::{StructureChange, Widget};

/// Content whose change is one the host can carry out in place, as a list mounting
/// the rows a scroll brought near: it asks for a remount, not a rebuild.
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

/// The bar of a column read as a page stands at the measure the messages above it are read
/// at, rather than across a wide window.
#[test]
fn a_page_column_bar_stands_at_the_readable_measure() {
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
    let bar_width = |page_column: bool| {
        let docked = Docked::from_props(
            "docked",
            DockedProps {
                content: Some(Box::new(InPlace { pending: false })),
                foot: Some(Box::new(crate::widget::Text::new("Reply"))),
                page_column,
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
    let measure = f64::from(crate::widget::arrange::MEASURE_REM)
        * f64::from(crate::widget::arrange::rem_of(&crate::theme::Theme::dark()));
    let [bar] = bar_width(true)[..] else {
        panic!("the bar stands in one page");
    };
    assert!(
        (bar - measure).abs() <= 1.0,
        "the bar at the measure: {bar}"
    );
    let [across] = bar_width(false)[..] else {
        panic!("the bar stands in one page");
    };
    assert!(across > measure * 2.0, "a column that is no page: {across}");
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

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
            foot: None,
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

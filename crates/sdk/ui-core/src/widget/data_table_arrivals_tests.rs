//! A paged model's rows on their way, in a [`DataTable`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use automerge::AutoCommit;

use super::tests::{PagingModel, R, sample_cols, table_color_fills};
use super::*;

#[test]
fn a_paged_model_requests_only_the_window_and_wakes_on_loaded_rows() {
    // The rows land when asked for: a local read in its budget.
    let asked = crate::time::now();
    crate::time::testing::with_monotonic_time(asked, || {
        let loaded = Rc::new(Cell::new(false));
        let changed = Rc::new(Cell::new(false));
        let requested = Rc::new(RefCell::new(Vec::new()));
        let mut table = DataTable::from_model(
            sample_cols(),
            PagingModel {
                row: R {
                    name: "loaded row".into(),
                },
                loaded: Rc::clone(&loaded),
                changed: Rc::clone(&changed),
                requested: Rc::clone(&requested),
            },
        );
        table.set_viewport(96.0, 24.0);

        let before = Widget::accessibility(&table).expect("loading semantics");
        assert_eq!(before.children[0].name.as_deref(), Some("Loading"));
        assert!(before.children[0].state.busy);
        assert_eq!(
            before.children[3].name.as_deref(),
            Some("row failed to load")
        );
        assert!(before.children[3].state.invalid);
        assert!(table.needs_animation_frame());

        let _fills = table_color_fills(&mut table);
        let requested = requested.borrow();
        assert_eq!(requested.len(), 1);
        assert!(requested[0].start == 0 && requested[0].end < 16);
        drop(requested);

        loaded.set(true);
        changed.set(true);
        assert!(Widget::update(
            &mut table,
            &AutoCommit::new(),
            &automerge::ROOT,
        ));
        let after = Widget::accessibility(&table).expect("loaded semantics");
        assert_eq!(after.children[0].name, None);
        assert!(!table.needs_animation_frame());
    });
}

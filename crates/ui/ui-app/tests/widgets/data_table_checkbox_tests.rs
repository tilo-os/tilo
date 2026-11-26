//! A table's checkbox cells toggle instead of activating their row, and a
//! removed record's retained checkbox is refused.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui::app::driver::Driver;
use tilo_ui::headless::*;
use tilo_ui::kurbo::*;
use tilo_ui::retained_present::*;
use tilo_ui::testing::*;
use tilo_ui::vello::*;
use tilo_ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use tilo_ui::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

struct Item {
    name: String,
    depth: usize,
    open: bool,
    children: bool,
    on: bool,
}

fn item(name: &str) -> Item {
    Item {
        name: name.to_owned(),
        depth: 0,
        open: false,
        children: false,
        on: false,
    }
}

fn items(n: usize) -> Vec<Item> {
    (0..n).map(|i| item(&format!("item {i}"))).collect()
}

fn name_col() -> ColumnDef<Item> {
    ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0)).with_text(|r: &Item| r.name.clone())
}

#[test]
fn hosted_checkbox_cells_toggle_instead_of_activating_their_row() {
    struct RetainedTable(DataTable<Item>);
    impl tilo_ui_base::widget::Decorator for RetainedTable {
        type Inner = DataTable<Item>;
        fn inner(&self) -> &Self::Inner {
            &self.0
        }
        fn inner_mut(&mut self) -> &mut Self::Inner {
            &mut self.0
        }
        fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(self)
        }
    }
    let checks = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&checks);
    let activations = Rc::new(Cell::new(0));
    let activated = Rc::clone(&activations);
    let mut table = DataTable::new(vec![
        ColumnDef::checkbox("on", "On", |r: &Item| r.on),
        name_col(),
    ])
    .with_row_key(|row: &Item| u64::from(*row.name.as_bytes().last().unwrap()))
    .selectable(true)
    .on_check(move |row, on| seen.borrow_mut().push((row, on)))
    .on_activate(move |_| activated.set(activated.get() + 1));
    table.set_rows(items(2));
    let mut harness = Driver::new(
        tilo_ui::State::new(),
        Box::new(RetainedTable(table)),
        (400, 300),
    );
    let cells: Vec<_> = harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == accesskit::Role::CheckBox)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(cells.len(), 2);
    harness
        .perform_a11y_action(cells[1], &tilo_ui_base::a11y::Action::Focus)
        .unwrap();
    assert!(
        checks.borrow().is_empty(),
        "focus does not toggle a checkbox"
    );
    harness
        .perform_a11y_action(cells[1], &tilo_ui_base::a11y::Action::ToggleCheck)
        .unwrap();
    harness
        .perform_a11y_action(cells[0], &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
    assert_eq!(activations.get(), 0, "a checkbox never activates its row");
    harness
        .with_widget_mut::<RetainedTable, _>(|table| table.0.set_rows(vec![item("item 1")]))
        .unwrap();
    harness.relayout();
    assert!(
        harness
            .perform_a11y_action(cells[0], &tilo_ui_base::a11y::Action::ToggleCheck)
            .is_err(),
        "a removed record's retained checkbox must be refused"
    );
    assert_eq!(*checks.borrow(), [(1, true), (0, true)]);
}

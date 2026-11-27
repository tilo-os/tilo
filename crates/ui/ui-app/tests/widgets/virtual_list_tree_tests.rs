use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::virtual_list::test_support::*;
use tilo_ui_app::widget::internals::virtual_list::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
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
#[test]
fn a_tree_row_open_in_its_place_hands_a_reader_what_it_holds() {
    use accesskit::Role as AkRole;
    use tilo_ui_app::widget::{Checkbox, Column, TextInput};
    use tilo_ui_base::a11y::Action;

    let done = tilo_ui_base::LocalState::new(false);
    let state = done.clone();
    let model = OpenableTree {
        keys: vec![0, 1, 2],
        parents: HashMap::from([(1, 0)]),
    };
    let list = VirtualList::with_selection_delegate(model, 20.0, move |_, _, key, build| {
        if build.expanded {
            let document: Element = Box::new(
                Column::new()
                    .push(Box::new(
                        TextInput::new(Vec::new())
                            .with_text_role(tilo_ui_base::tokens::TextRole::Title)
                            .with_id("title")
                            .with_accessible_name("Title")
                            .with_initial_value("Subtasks"),
                    ))
                    .push(Box::new(
                        TextInput::new(Vec::new())
                            .with_id("notes")
                            .multiline(true)
                            .with_accessible_name("Notes"),
                    ))
                    .push(Box::new(
                        Checkbox::from_state(&state).with_accessible_name("Done"),
                    )),
            );
            row_open(document, true)
        } else {
            Box::new(ProbeRow::new(
                format!("task {key}"),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            )) as Element
        }
    })
    .with_tree(Rc::new(TreeMarks::default()))
    .with_viewport_height(300.0);
    let mut harness = Driver::for_root(tilo_ui_app::State::new(), list, (400, 500), Theme::dark());
    harness
        .root_mut::<VirtualList<OpenableTree>>()
        .set_open(Some(0));
    harness.relayout();

    let nodes = harness.a11y_nodes();
    let (row_id, row) = named(&nodes, AkRole::TreeItem, "Subtasks");
    let row_bounds = row.bounds().expect("the row stands where it is drawn");
    let controls = inside(&nodes, row_id);
    assert!(
        controls
            .iter()
            .all(|(_, node)| !matches!(node.role(), AkRole::ListItem | AkRole::TreeItem)),
        "the open row is one tree item, not one inside another"
    );
    named(&controls, AkRole::TextInput, "Title");
    named(&controls, AkRole::MultilineTextInput, "Notes");
    let (checkbox, _) = named(&controls, AkRole::CheckBox, "Done");
    for (id, _) in &controls {
        let bounds = exported(&nodes, *id)
            .bounds()
            .expect("a control stands where it is drawn");
        assert!(
            within(row_bounds, bounds),
            "{bounds:?} stands outside its row {row_bounds:?}"
        );
    }

    harness
        .perform_a11y_action(checkbox, &Action::Activate)
        .expect("the box is operated");
    assert!(done.with(|checked| *checked), "the tick reaches the box");
}

/// A branch row still on its way is its placeholder alone: it offers no disclosure to a page or a
/// reader and holds none of the rows under it, which stand after it until it arrives; then it
/// holds them under the disclosure it offers.
#[test]
fn a_branch_on_its_way_offers_nothing_to_fold_until_it_arrives() {
    use tilo_ui_app::time::testing::with_monotonic_time;
    use tilo_ui_app::widget::ListRow;
    use tilo_ui_base::{arriving::GRACE, arriving::MIN_SHOWN};

    let t0 = tilo_ui_app::time::now();
    let ready = Rc::new(Cell::new(false));
    let model = ArrivingNested {
        rows: Nested {
            keys: vec![0, 1, 2],
            parents: HashMap::from([(1, 0), (2, 1)]),
            groups: HashMap::new(),
        },
        ready: Rc::clone(&ready),
    };
    let list = VirtualList::new(model, 20.0, |_, _, key| {
        Box::new(ListRow::new(format!("note-{key}"), format!("Note {key}"))) as Element
    })
    .with_viewport_height(200.0)
    .with_tree(Rc::new(TreeMarks::default()))
    .with_semantic_key(|key| format!("row:{key}"));
    let mut harness = with_monotonic_time(t0, || {
        let mut harness =
            Driver::for_root(tilo_ui_app::State::new(), list, (400, 500), Theme::dark());
        harness.relayout();
        harness
    });
    let composition = with_monotonic_time(t0 + GRACE, || {
        harness.relayout();
        harness.composition()
    });
    let list_node = composed_at(&composition.root, "VirtualList")
        .into_iter()
        .next()
        .expect("the list composes");
    let folds_first = |node: &tilo_ui_app::composition::Composed| {
        composed_at(node, "ListRow").into_iter().any(|row| {
            row.semantics.as_ref().is_some_and(|access| {
                access
                    .nodes()
                    .into_iter()
                    .any(|part| part.action_target.as_deref() == Some("fold:row:0"))
            })
        })
    };
    assert!(
        !folds_first(list_node),
        "a placeholder offers nothing to fold: {list_node:?}"
    );
    let placeholder = composed_at(list_node, "RowReserve")
        .into_iter()
        .next()
        .expect("the row on its way holds its place");
    assert!(
        placeholder.children.is_empty(),
        "and holds no notes: {placeholder:?}"
    );
    let first = row_named(&composition.root, "Note 1").expect("the row under it is shown");
    assert!(
        list_node.children.iter().any(|row| row.path == first.path),
        "the row under the placeholder stands in the list, after it: {list_node:?}"
    );
    let disclosures = |harness: &mut Driver| {
        let mut found = 0;
        let list = harness.root_mut::<VirtualList<ArrivingNested>>();
        let mut nodes = vec![Widget::accessibility(&*list).expect("the list is read")];
        while let Some(node) = nodes.pop() {
            if node.action_target.as_deref() == Some("fold:row:0") {
                found += 1;
            }
            nodes.extend(node.children);
        }
        found
    };
    assert_eq!(disclosures(&mut harness), 0, "nor does a reader hear one");

    ready.set(true);
    let composition = with_monotonic_time(t0 + GRACE + MIN_SHOWN, || {
        harness.relayout();
        harness.composition()
    });
    let branch = row_named(&composition.root, "Note 0").expect("the row arrived");
    assert!(
        folds_first(branch),
        "the arrived branch offers its disclosure: {branch:?}"
    );
    // The branch holds the rows under it as a tree does: its node stands open at the top of the
    // tree and theirs a level in, and they stand after it in the list's page.
    let branch_node = branch
        .semantics
        .as_ref()
        .expect("a reader reads the branch");
    assert_eq!(branch_node.level, Some(1), "the branch stands at the top");
    assert_eq!(
        branch_node.state.expanded,
        Some(true),
        "and open on the rows under it"
    );
    let under = row_named(&composition.root, "Note 1").expect("the row under it");
    assert_eq!(
        under.semantics.as_ref().and_then(|node| node.level),
        Some(2),
        "the row under it stands a level in: {under:?}"
    );
    let place = |path: &str| {
        list_node
            .children
            .iter()
            .position(|row| row.path == path)
            .unwrap_or_else(|| panic!("{path} stands in the list: {list_node:?}"))
    };
    assert!(
        place(&branch.path) < place(&under.path),
        "and stands after the branch in the list: {list_node:?}"
    );
    assert_eq!(disclosures(&mut harness), 1, "and a reader hears it");
}

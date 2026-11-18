//! Tree-shape correctness: create / rename / delete leave the notebook with
//! valid parent pointers and siblings in the order they were made.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

use notes::actions;
use notes::{Notebook, PageId, PagePart, Pages, Workspace};
use ui::framework::Chain;

/// The notes as one window holds them, each action carried out as the window
/// carries it out.
fn notes() -> Chain<PagePart> {
    Chain::detached(
        ui::apps::NOTES,
        &ui::tilo_resource::MemberId::person("tree-author"),
        || Rc::new(Notebook::default()),
    )
    .unwrap()
}

fn apply(notes: &Chain<PagePart>, action: &actions::Action) {
    notes.apply(action).unwrap();
}

fn create(notes: &Chain<PagePart>, parent: Option<PageId>) -> PageId {
    let id = PageId::new_v4();
    apply(notes, &actions::create_page_action(id, parent, "Untitled"));
    id
}

fn shown(notes: &Chain<PagePart>) -> Workspace {
    Workspace::of(&Notebook::default(), &notes.parts(), &Workspace::default())
}

/// Every note's parent is a note of the notebook.
fn assert_valid(notes: &Chain<PagePart>) {
    let workspace = shown(notes);
    let parts = notes.parts();
    let pages = Pages::of(&parts);
    for (_, page) in pages.iter() {
        if let Some(parent) = &page.parent {
            assert!(pages.contains(parent), "{parent} is a note");
        }
    }
    assert_eq!(workspace.pages().len(), pages.len());
}

#[test]
fn tree_operations_keep_shape_and_order() {
    eprintln!("phase create_child_of_selected_appends_under_parent");
    {
        let notebook = notes();
        let parent = create(&notebook, None);
        let child_a = create(&notebook, Some(parent));
        let child_b = create(&notebook, Some(parent));
        let ids: Vec<_> = shown(&notebook)
            .children_of(Some(parent))
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec![child_a, child_b]);
        assert_valid(&notebook);
    }

    eprintln!("phase rename_keeps_tree_shape");
    {
        let notebook = notes();
        let id = create(&notebook, None);
        let other = create(&notebook, None);
        apply(&notebook, &actions::rename_page_action(id, "Renamed"));
        let workspace = shown(&notebook);
        assert_eq!(workspace.get(id).unwrap().title, "Renamed");
        assert_eq!(
            workspace
                .children_of(None)
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            vec![id, other]
        );
        assert_valid(&notebook);
    }

    eprintln!("phase delete_removes_descendants_and_keeps_sibling_order");
    {
        let notebook = notes();
        let a = create(&notebook, None);
        let b = create(&notebook, None);
        let c = create(&notebook, None);
        let b_child = create(&notebook, Some(b));
        let b_grand = create(&notebook, Some(b_child));

        apply(&notebook, &actions::delete_page_action(b));
        let workspace = shown(&notebook);
        for gone in [b, b_child, b_grand] {
            assert!(workspace.get(gone).is_none());
        }
        let root_ids: Vec<_> = workspace.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(root_ids, vec![a, c]);
        assert_eq!(workspace.pages().len(), 2);
        assert_valid(&notebook);
    }

    eprintln!("phase no_orphans_after_repeated_delete");
    {
        let notebook = notes();
        let root = create(&notebook, None);
        let mid = create(&notebook, Some(root));
        let _leaf = create(&notebook, Some(mid));
        apply(&notebook, &actions::delete_page_action(mid));
        assert_valid(&notebook);
        assert!(
            shown(&notebook).children_of(Some(root)).is_empty(),
            "no surviving descendants of `root` after deleting `mid`"
        );
        assert_eq!(Pages::of(&notebook.parts()).len(), 1);
    }
}

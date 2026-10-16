//! Tree-shape correctness: create / rename / delete leave the notebook with
//! valid parent pointers and siblings in the order they were made.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::actions;
use notes::{Notebook, PageId, Workspace};
use ui::tilo_resource::Replica;

fn create(notebook: &mut Replica<Notebook>, parent: Option<PageId>) -> PageId {
    let id = PageId::new_v4();
    notebook
        .apply_action(&actions::create_page_action(id, parent, "Untitled"))
        .unwrap();
    id
}

fn shown(notebook: &Replica<Notebook>) -> Workspace {
    Workspace::of(&notebook.state().unwrap(), &Workspace::default())
}

/// Every note's parent is a note of the notebook.
fn assert_valid(notebook: &Replica<Notebook>) {
    let workspace = shown(notebook);
    let state = notebook.state().unwrap();
    for (_, page) in state.pages.iter() {
        if let Some(parent) = &page.parent {
            assert!(state.pages.contains(parent), "{parent} is a note");
        }
    }
    assert_eq!(workspace.pages().len(), state.pages.len());
}

#[test]
fn tree_operations_keep_shape_and_order() {
    eprintln!("phase create_child_of_selected_appends_under_parent");
    {
        let mut notebook = Replica::<Notebook>::new();
        let parent = create(&mut notebook, None);
        let child_a = create(&mut notebook, Some(parent));
        let child_b = create(&mut notebook, Some(parent));
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
        let mut notebook = Replica::<Notebook>::new();
        let id = create(&mut notebook, None);
        let other = create(&mut notebook, None);
        notebook
            .apply_action(&actions::rename_page_action(id, "Renamed"))
            .unwrap();
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
        let mut notebook = Replica::<Notebook>::new();
        let a = create(&mut notebook, None);
        let b = create(&mut notebook, None);
        let c = create(&mut notebook, None);
        let b_child = create(&mut notebook, Some(b));
        let b_grand = create(&mut notebook, Some(b_child));

        notebook
            .apply_action(&actions::delete_page_action(b))
            .unwrap();
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
        let mut notebook = Replica::<Notebook>::new();
        let root = create(&mut notebook, None);
        let mid = create(&mut notebook, Some(root));
        let _leaf = create(&mut notebook, Some(mid));
        notebook
            .apply_action(&actions::delete_page_action(mid))
            .unwrap();
        assert_valid(&notebook);
        assert!(
            shown(&notebook).children_of(Some(root)).is_empty(),
            "no surviving descendants of `root` after deleting `mid`"
        );
        assert_eq!(notebook.state().unwrap().pages.len(), 1);
    }
}

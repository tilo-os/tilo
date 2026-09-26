//! Tree-shape correctness: create / rename / delete should leave the
//! workspace with valid parent pointers and contiguous sibling order.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use notes::Workspace;

#[test]
fn tree_operations_keep_shape_order_and_no_orphans() {
    eprintln!("phase create_child_of_selected_appends_under_parent");
    {
        let mut ws = Workspace::new();
        let parent = ws.create_page(None);
        ws.set_selected(Some(parent));

        // Simulate the "create child of currently-selected page" flow
        // used by Cmd-N in the sidebar.
        let child_a = ws.create_page(ws.selected());
        let child_b = ws.create_page(ws.selected());

        let kids = ws.children_of(Some(parent));
        let ids: Vec<_> = kids.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![child_a, child_b]);
        assert_eq!(kids[0].order, 0);
        assert_eq!(kids[1].order, 1);
        ws.check_invariants().unwrap();
    }

    eprintln!("phase rename_keeps_tree_shape");
    {
        let mut ws = Workspace::new();
        let id = ws.create_page(None);
        let _ = ws.create_page(None);
        ws.rename(id, "Renamed");
        assert_eq!(ws.get(id).unwrap().title, "Renamed");
        ws.check_invariants().unwrap();
    }

    eprintln!("phase delete_removes_descendants_and_renumbers_siblings");
    {
        let mut ws = Workspace::new();
        let a = ws.create_page(None);
        let b = ws.create_page(None);
        let c = ws.create_page(None);
        let b_child = ws.create_page(Some(b));
        let b_grand = ws.create_page(Some(b_child));

        let removed = ws.delete_page(b);
        assert_eq!(removed.len(), 3);
        assert!(removed.contains(&b));
        assert!(removed.contains(&b_child));
        assert!(removed.contains(&b_grand));

        let roots = ws.children_of(None);
        let root_ids: Vec<_> = roots.iter().map(|p| p.id).collect();
        assert_eq!(root_ids, vec![a, c]);
        assert_eq!(roots[0].order, 0);
        assert_eq!(roots[1].order, 1);
        ws.check_invariants().unwrap();
    }

    eprintln!("phase no_orphans_after_repeated_delete");
    {
        let mut ws = Workspace::new();
        let root = ws.create_page(None);
        let mid = ws.create_page(Some(root));
        let _leaf = ws.create_page(Some(mid));
        ws.delete_page(mid);
        ws.check_invariants().unwrap();
        let kids = ws.children_of(Some(root));
        assert!(
            kids.is_empty(),
            "no surviving descendants of `root` after deleting `mid`"
        );
    }
}

//! Tree-shape correctness: create / rename / delete leave the notebook with
//! valid parent pointers and siblings in the order they were made.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

use notes::actions;
use notes::notebook::page_id;
use notes::{NoteList, Notebook, PageId, Pages, Workspace};
use ui::framework::Chain;
use ui::tilo_resource::Invoke;

/// The notes as one window holds them, each action carried out as the window
/// carries it out.
fn notes() -> Chain<NoteList> {
    Chain::detached(
        ui::apps::NOTES,
        &ui::tilo_resource::MemberId::person("tree-author"),
        || Rc::new(Notebook::default()),
    )
    .unwrap()
}

fn apply(notes: &Chain<NoteList>, action: &actions::Action) {
    notes.apply(action).unwrap();
}

fn create(notes: &Chain<NoteList>, parent: Option<PageId>) -> PageId {
    let id = PageId::new_v4();
    apply(notes, &actions::create_page_action(id, parent, "Untitled"));
    id
}

fn shown(notes: &Chain<NoteList>) -> Workspace {
    Workspace::of(&Notebook::default(), &notes.parts(), &Workspace::default())
}

/// Every note's parent is a note of the notebook.
fn assert_valid(notes: &Chain<NoteList>) {
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

#[test]
fn an_imported_tree_sent_as_import_then_move_groups_keeps_its_parents() {
    use ui::framework::PartLimits;
    use ui::tilo_resource::Resource as _;
    // A part holds two notes, so the second import group starts the next part
    // and the moves below span both.
    let _parts = PartLimits::scoped(2, usize::MAX);
    let notebook = notes();
    let collection = PageId::new_v4();
    let alpha = PageId::new_v4();
    let beta = PageId::new_v4();
    let gamma = PageId::new_v4();
    let import = |note: PageId, title: &str, markdown: &str| {
        Invoke::apply(&actions::import_markdown_action(
            Some(note),
            title,
            markdown,
            Vec::new(),
        ))
        .unwrap()
    };
    // As a migration sends it: the pages are imported in groups first, then
    // nested under their parents in one group of moves.
    let _ = notebook
        .invoke(&Invoke::group([
            import(collection, "Collection", ""),
            import(alpha, "Alpha", "alpha body"),
        ]))
        .unwrap();
    let _ = notebook
        .invoke(&Invoke::group([
            import(beta, "Beta", "beta body"),
            import(gamma, "Gamma", "gamma body"),
        ]))
        .unwrap();
    // A move names the note it changes, so the engine sends it to the part
    // holding that note; an import makes a new one.
    assert_eq!(
        NoteList::action_item(&actions::move_page_action(beta, Some(alpha), None)),
        Some(page_id(beta)),
        "a move names its note"
    );
    assert_eq!(
        NoteList::action_item(&actions::import_markdown_action(
            Some(gamma),
            "Gamma",
            "gamma body",
            Vec::new()
        )),
        None,
        "an import makes a note"
    );
    let _ = notebook
        .invoke(&Invoke::group([
            Invoke::apply(&actions::move_page_action(alpha, Some(collection), None)).unwrap(),
            Invoke::apply(&actions::move_page_action(beta, Some(alpha), None)).unwrap(),
            Invoke::apply(&actions::move_page_action(gamma, Some(beta), None)).unwrap(),
        ]))
        .unwrap();

    let parts = notebook.parts();
    assert!(
        parts.len() > 1,
        "the notes span more than one part: {}",
        parts.len()
    );
    let pages = Pages::of(&parts);
    let parent = |page: PageId| pages.parent_of(&page_id(page)).cloned();
    assert_eq!(parent(alpha), Some(page_id(collection)));
    assert_eq!(parent(beta), Some(page_id(alpha)));
    assert_eq!(parent(gamma), Some(page_id(beta)));
    assert_eq!(
        pages
            .children(None)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>(),
        vec![page_id(collection)],
        "only the collection is at the top"
    );
    assert_eq!(
        pages
            .children(Some(&page_id(collection)))
            .map(|(_, page)| page.title.as_str())
            .collect::<Vec<_>>(),
        ["Alpha"]
    );
}

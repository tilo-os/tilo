use crate::component_support::*;
use automerge::transaction::Transactable;
use notes::{actions, read};
use tilo_actions::Action as _;

#[test]
fn duplicate_subtree_round_trips_rich_content_and_mints_fresh_ids() {
    // Catches metadata-only/Markdown copies, lost native marks, wrong link remaps,
    // reused identifiers and nondeterministic replay.
    eprintln!("phase duplicate_subtree_roundtrips_and_preserves_rich_native_content");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let action = actions::duplicate_action(f.root);
        let encoded = actions::encode(&action);
        let mut a = doc.clone();
        let mut b = doc.clone();
        let mut registry = tilo_actions::ActionRegistry::new();
        <notes::Notebook as ui::tilo_resource::Resource>::register(&mut registry);
        let dispatched = tilo_actions::dispatch(
            &mut a,
            &action,
            encoded,
            &mut tilo_actions::LocalTransport::new(),
        )
        .unwrap();
        tilo_actions::replay(&mut b, dispatched.envelope(), &registry).unwrap();
        assert_eq!(a.get_heads(), b.get_heads(), "replay makes the same copy");
        let projected = notes::read::project(&a).unwrap();
        assert_eq!(projected.pages().len(), 5);
        let roots = projected
            .children_of(None)
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>();
        assert_eq!(roots.len(), 3);
        assert_eq!(
            (roots[0], roots[2]),
            (f.root, f.empty),
            "the copy lands right after its source"
        );
        let copied_root = roots[1];
        assert_eq!(projected.get(copied_root).unwrap().title, "Projects copy");
        let copied_child = projected.children_of(Some(copied_root))[0].id;
        assert_eq!(projected.get(copied_child).unwrap().title, "Launch");
        assert_eq!(
            projected
                .tags_for_page(copied_child)
                .iter()
                .map(|tag| tag.id)
                .collect::<Vec<_>>(),
            vec![f.tag]
        );
        let copies = [(f.root, copied_root), (f.child, copied_child)];
        for (source, destination) in copies {
            let src = read::page_block_snapshots(&doc, source);
            let dst = read::page_block_snapshots(&a, destination);
            assert_eq!(
                src.iter().map(|(_, t)| t).collect::<Vec<_>>(),
                dst.iter().map(|(_, t)| t).collect::<Vec<_>>()
            );
            assert_remapped_blocks(
                read::page_blocks(&doc, source),
                read::page_blocks(&a, destination),
                &copies,
            );
        }
    }

    // Two copies made one after the other are two subtrees of their own.
    eprintln!("phase duplicating_twice_makes_distinct_owned_subtrees");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let before = read::list_pages(&doc)
            .into_iter()
            .map(|page| page.id)
            .collect::<std::collections::BTreeSet<_>>();
        for _ in 0..2 {
            let action = actions::duplicate_action(f.root);
            tilo_actions::apply_local(&mut doc, &action, actions::encode(&action)).unwrap();
        }
        let after = read::list_pages(&doc)
            .into_iter()
            .map(|page| page.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            after.len(),
            before.len() + 4,
            "each copy has ids of its own"
        );
        assert!(before.is_subset(&after));
    }

    eprintln!("phase duplicating_a_missing_note_is_refused_before_writes");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let before = (doc.base_heads(), doc.pending_ops());
        assert!(
            actions::duplicate_action(uuid::Uuid::new_v4())
                .apply(&mut doc)
                .is_err()
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
    }
}

/// The copy's blocks are the source's, with links into the copied subtree
/// following the copy, under ids of its own.
fn assert_remapped_blocks(
    source: Vec<ui::widget::BlockContent>,
    destination: Vec<ui::widget::BlockContent>,
    copies: &[(notes::PageId, notes::PageId)],
) {
    use ui::widget::marks::Mark;
    let follow = |page: uuid::Uuid| {
        copies
            .iter()
            .find(|(source, _)| *source == page)
            .map_or(page, |(_, copy)| *copy)
    };
    assert_eq!(source.len(), destination.len());
    for (old, new) in source.into_iter().zip(destination) {
        assert_ne!(old.id, new.id, "a copy has ids of its own");
        let mut expected = old;
        for span in &mut expected.marks {
            span.mark = match &span.mark {
                Mark::Link(page) => Mark::Link(follow(*page)),
                Mark::Transclusion(page) => Mark::Transclusion(follow(*page)),
                other => other.clone(),
            };
        }
        expected.id.clone_from(&new.id);
        // A copied fence is a fence of its own.
        assert_eq!(new.code_group.is_some(), expected.code_group.is_some());
        if new.code_group.is_some() {
            assert_ne!(new.code_group, expected.code_group);
        }
        expected.code_group.clone_from(&new.code_group);
        assert_eq!(new, expected);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn bound_edits_are_one_owned_history_entry_each() {
    eprintln!("phase bound_first_typing_split_and_block_move_keep_schema_and_one_history_entry");
    {
        let f = NotebookFixture::rich();
        let component = component(&f, 11, ui::application::LaunchTarget::New);
        let checkout = component.record().clone();
        let mut driver =
            ui::headless::FrameHarness::from_component(component, (1000, 700), runner_inputs())
                .unwrap();
        new_note(&mut driver, 11, "First");
        let page = f
            .records
            .read(&checkout.id, |doc| read::list_pages(doc)[0].id)
            .unwrap();
        open_note(&mut driver, 11, page, None);
        driver
            .expect(&ui::headless::SemanticExpectation::named("note_title").value("First"))
            .unwrap();
        driver
            .perform_named("note_title", &ui::a11y::Action::Focus)
            .unwrap();
        driver
            .expect(
                &ui::headless::SemanticExpectation::named("block_editor")
                    .role(ui::a11y::Role::Document),
            )
            .unwrap();
        driver
            .perform_named("block_editor", &ui::a11y::Action::Focus)
            .unwrap();
        let before = revision(&f, &checkout);
        assert_eq!(before, 1, "creation only; title/editor focus is local");
        driver.type_text("a");
        f.records
            .read(&checkout.id, |doc| {
                notes::read::validate(doc).unwrap();
                assert_eq!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["a"]
                );
            })
            .unwrap();
        // Typing a word is one history entry, recorded once the word ends.
        assert_eq!(revision(&f, &checkout), before);
        driver.key("Enter");
        assert_eq!(
            revision(&f, &checkout),
            before + 2,
            "the word, then the split"
        );
        driver.type_text("b");
        assert_eq!(revision(&f, &checkout), before + 2);
        let blocks = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, page))
            .unwrap();
        assert_eq!(
            blocks
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        driver
            .perform_semantic_named(
                "block_editor",
                Some(&format!("notes.block-move:{}:up", blocks[1].0)),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        assert_eq!(
            revision(&f, &checkout),
            before + 4,
            "the word, then the move"
        );
        f.records
            .read(&checkout.id, |doc| {
                notes::read::validate(doc).unwrap();
                assert_eq!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["b", "a"]
                );
            })
            .unwrap();
        assert_eq!(driver.state_mut().doc.pending_ops(), 0);
        assert!(driver.state_mut().doc.base_heads().is_empty());
        close_driver(&mut driver);
    }

    eprintln!("phase bound_tree_reorder_is_one_logical_history_entry");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut driver = driver(&f, 11, &checkout);
        let rect = page_rect(&driver, f.empty);
        driver.click_at(rect.center().x, rect.center().y);
        let before = revision(&f, &checkout);
        driver.set_modifiers(&["alt".into()]);
        driver.key("ArrowUp");
        driver.set_modifiers(&[]);
        assert_eq!(revision(&f, &checkout), before + 1);
        f.records
            .read(&checkout.id, |doc| {
                let workspace = notes::read::project(doc).unwrap();
                let roots = workspace.children_of(None);
                assert_eq!(
                    roots.iter().map(|p| p.id).collect::<Vec<_>>(),
                    vec![f.empty, f.root]
                );
            })
            .unwrap();
        close_driver(&mut driver);
    }

    eprintln!("phase bound_rich_split_preserves_both_halves_in_one_history_entry");
    {
        let f = NotebookFixture::rich();
        let component = component(&f, 11, ui::application::LaunchTarget::New);
        let checkout = component.record().clone();
        let mut a = component_driver(component);
        new_note(&mut a, 11, "Rich");
        let page = f
            .records
            .read(&checkout.id, |doc| read::list_pages(doc)[0].id)
            .unwrap();
        a.perform_named("block_editor", &ui::a11y::Action::Focus)
            .unwrap();
        a.type_text("abcd");
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(read::page_block_snapshots(doc, page)[0].1, "abcd");
            })
            .unwrap();
        a.set_modifiers(&["shift".into()]);
        a.key("Home");
        a.set_modifiers(&[]);
        let before = revision(&f, &checkout);
        a.set_modifiers(&["ctrl".into()]);
        a.key("b");
        a.set_modifiers(&[]);
        assert_eq!(
            revision(&f, &checkout),
            before + 2,
            "the typed word closes, then the format"
        );
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(
                    read::page_block_marks(doc, page)[0].1,
                    vec![actions::InlineMarkSpan {
                        start: 0,
                        end: 4,
                        mark: actions::InlineMark::Bold
                    }]
                );
            })
            .unwrap();
        a.key("ArrowLeft");
        a.key("Home");
        a.key("ArrowRight");
        a.key("ArrowRight");
        let before = revision(&f, &checkout);
        a.key("Enter");
        assert_eq!(revision(&f, &checkout), before + 1);
        f.records
            .read(&checkout.id, |doc| {
                notes::read::validate(doc).unwrap();
                assert_eq!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["ab", "cd"],
                    "each arrow press moves the caret one character"
                );
                for ((_, marks), end) in read::page_block_marks(doc, page).into_iter().zip([2, 2]) {
                    assert_eq!(
                        marks,
                        vec![actions::InlineMarkSpan {
                            start: 0,
                            end,
                            mark: actions::InlineMark::Bold
                        }]
                    );
                }
            })
            .unwrap();
        f.records
            .with_history(&checkout.id, |doc, history| {
                history
                    .undo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                assert_eq!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    vec!["abcd"]
                );
                assert_eq!(
                    read::page_block_marks(doc, page)[0].1,
                    vec![actions::InlineMarkSpan {
                        start: 0,
                        end: 4,
                        mark: actions::InlineMark::Bold
                    }]
                );
            })
            .unwrap();
        close_driver(&mut a);
    }

    eprintln!("phase bound_pointer_reparent_duplicate_and_checklist_are_owned_actions");
    {
        use winit::event::{ElementState, MouseButton};
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut a = driver(&f, 11, &checkout);
        let from = page_rect(&a, f.empty).center();
        let to = page_rect(&a, f.root).center();
        move_pointer(&mut a, from);
        mouse(&mut a, MouseButton::Left, ElementState::Pressed);
        move_pointer(&mut a, from + vello::kurbo::Vec2::new(0.0, -10.0));
        move_pointer(&mut a, to);
        mouse(&mut a, MouseButton::Left, ElementState::Released);
        assert_eq!(revision(&f, &checkout), 1);
        f.records
            .read(&checkout.id, |doc| {
                let workspace = notes::read::project(doc).unwrap();
                assert_eq!(workspace.get(f.empty).unwrap().parent, Some(f.root));
                assert_eq!(
                    workspace
                        .children_of(Some(f.root))
                        .iter()
                        .map(|p| p.id)
                        .collect::<Vec<_>>(),
                    vec![f.child, f.empty]
                );
            })
            .unwrap();
        context_menu_select(&mut a, f.root, "Duplicate");
        assert_eq!(revision(&f, &checkout), 2);
        let copied_root = f
            .records
            .read(&checkout.id, |doc| {
                let workspace = notes::read::project(doc).unwrap();
                assert_eq!(workspace.pages().len(), 6);
                let copy = workspace
                    .pages()
                    .iter()
                    .find(|p| p.title == "Projects copy")
                    .unwrap();
                assert_eq!(workspace.children_of(None)[1].id, copy.id);
                assert_eq!(workspace.children_of(Some(copy.id)).len(), 2);
                copy.id
            })
            .unwrap();
        let (block, index) = f
            .records
            .read(&checkout.id, |doc| {
                let index = read::page_blocks(doc, f.child)
                    .iter()
                    .position(|block| block.kind.stored() == "checklist:0:0")
                    .unwrap();
                (
                    read::page_block_snapshots(doc, f.child)[index].0.clone(),
                    index,
                )
            })
            .unwrap();
        open_note(&mut a, 11, f.child, Some(&block));
        assert_eq!(revision(&f, &checkout), 2);
        let rect = semantic_rect(&a, &format!("notes.block:{block}"));
        assert!(
            rect.center().y >= 0.0 && rect.center().y < 700.0,
            "revealed checklist bounds {rect:?}"
        );
        a.click_at(rect.x0 - 12.0, rect.center().y);
        assert_eq!(revision(&f, &checkout), 3);
        f.records
            .read(&checkout.id, |doc| {
                assert_eq!(
                    read::page_blocks(doc, f.child)[index].kind.stored(),
                    "checklist:0:1"
                );
                notes::read::validate(doc).unwrap();
            })
            .unwrap();
        context_menu_select(&mut a, copied_root, "Delete");
        // Deleting a page asks first; the person confirms.
        assert!(a.confirm_asked().is_some(), "the host asks first");
        assert_eq!(revision(&f, &checkout), 4);
        f.records
            .read(&checkout.id, |doc| {
                let workspace = notes::read::project(doc).unwrap();
                assert_eq!(workspace.pages().len(), 3);
                assert!(workspace.get(copied_root).is_none());
                notes::read::validate(doc).unwrap();
            })
            .unwrap();
        close_driver(&mut a);
    }

    eprintln!("phase tree_move_is_atomic_exact_and_replayable_without_touching_rich_content");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let action = actions::move_page_action(f.child, Some(f.empty), None);
        let mut a = doc.clone();
        let mut b = doc.clone();
        let dispatched = tilo_actions::dispatch(
            &mut a,
            &action,
            actions::encode(&action),
            &mut tilo_actions::LocalTransport::new(),
        )
        .unwrap();
        let mut registry = tilo_actions::ActionRegistry::new();
        <notes::Notebook as ui::tilo_resource::Resource>::register(&mut registry);
        tilo_actions::replay(&mut b, dispatched.envelope(), &registry).unwrap();
        assert_eq!(a.get_heads(), b.get_heads());
        notes::read::validate(&a).unwrap();
        assert_eq!(
            notes::read::project(&a)
                .unwrap()
                .get(f.child)
                .unwrap()
                .parent,
            Some(f.empty)
        );
        for page in [f.root, f.child, f.empty] {
            assert_eq!(
                read::page_block_snapshots(&a, page),
                read::page_block_snapshots(&doc, page)
            );
            assert_eq!(read::page_blocks(&a, page), read::page_blocks(&doc, page));
            assert_eq!(read::page_body(&a, page), read::page_body(&doc, page));
        }
        assert_eq!(
            read::list_tag_assignments(&a),
            read::list_tag_assignments(&doc)
        );
        for (invalid, action) in [
            (
                "cycle",
                actions::move_page_action(f.root, Some(f.child), None),
            ),
            (
                "itself",
                actions::move_page_action(f.root, Some(f.root), None),
            ),
            (
                "missing",
                actions::move_page_action(uuid::Uuid::new_v4(), None, None),
            ),
            (
                "missing-parent",
                actions::move_page_action(f.child, Some(uuid::Uuid::new_v4()), None),
            ),
        ] {
            let mut candidate = doc.clone();
            let untouched = (candidate.base_heads(), candidate.pending_ops());
            assert!(action.apply(&mut candidate).is_err(), "{invalid}");
            assert_eq!(
                (candidate.base_heads(), candidate.pending_ops()),
                untouched,
                "{invalid}"
            );
        }
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn bound_undo_is_per_instance_and_retypes_once() {
    eprintln!("phase bound_same_author_interleaving_undo_redo_is_per_instance");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut a = driver(&f, 11, &checkout);
        let mut b = driver(&f, 12, &checkout);
        let original = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.child)[0].clone()
            })
            .unwrap();
        assert_eq!(revision(&f, &checkout), 0);
        invoke_with(
            &mut a,
            11,
            actions::page_resource_id(f.root),
            "rename",
            serde_json::json!({ "title": "A title" }),
        );
        assert_eq!(revision(&f, &checkout), 1);
        replace_text_through(&mut b, 12, &f, &checkout, &original.0, "B text");
        assert_eq!(revision(&f, &checkout), 2);
        assign_tag_through(&mut a, 11, f.root, f.tag);
        assert_eq!(revision(&f, &checkout), 3);
        f.records
            .with_history(&checkout.id, |doc, history| {
                for instance in [11, 12] {
                    let last = history.next_undo_in(doc, &f.author, instance).unwrap();
                    assert_eq!(last.author.as_ref(), Some(&f.author));
                    assert_eq!(last.instance_id, Some(instance));
                }
                history
                    .undo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                assert!(!read::list_tag_assignments(doc).contains(&(f.root, f.tag)));
                assert_eq!(
                    read::list_pages(doc)
                        .iter()
                        .find(|p| p.id == f.root)
                        .unwrap()
                        .title,
                    "A title"
                );
                assert_eq!(read::page_block_snapshots(doc, f.child)[0].1, "B text");
                history
                    .undo_in(doc, &f.author, 12, None, None, None)
                    .unwrap();
                assert_eq!(read::page_block_snapshots(doc, f.child)[0].1, original.1);
                history
                    .redo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                history
                    .redo_in(doc, &f.author, 12, None, None, None)
                    .unwrap();
                assert!(read::list_tag_assignments(doc).contains(&(f.root, f.tag)));
                assert_eq!(read::page_block_snapshots(doc, f.child)[0].1, "B text");
            })
            .unwrap();
        // A's redo remains available after B creates a new action.
        f.records
            .with_history(&checkout.id, |doc, history| {
                history
                    .undo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
            })
            .unwrap();
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.child),
            "rename",
            serde_json::json!({ "title": "B child" }),
        );
        f.records
            .with_history(&checkout.id, |doc, history| {
                history
                    .redo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                assert!(read::list_tag_assignments(doc).contains(&(f.root, f.tag)));
                assert_eq!(
                    read::list_pages(doc)
                        .iter()
                        .find(|p| p.id == f.child)
                        .unwrap()
                        .title,
                    "B child"
                );
            })
            .unwrap();
        close_driver(&mut a);
        close_driver(&mut b);
    }

    eprintln!("phase bound_first_type_undo_to_empty_retypes_once");
    {
        let f = NotebookFixture::rich();
        let component = component(&f, 11, ui::application::LaunchTarget::New);
        let checkout = component.record().clone();
        let mut a = component_driver(component);
        new_note(&mut a, 11, "Empty again");
        let page = f
            .records
            .read(&checkout.id, |doc| read::list_pages(doc)[0].id)
            .unwrap();
        a.perform_named("block_editor", &ui::a11y::Action::Focus)
            .unwrap();
        a.type_text("a");
        a.set_modifiers(&["ctrl".into()]);
        a.key("z");
        a.set_modifiers(&[]);

        f.records
            .read(&checkout.id, |doc| {
                assert!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .all(|(_, text)| text.is_empty())
                );
            })
            .unwrap();
        assert_empty_retypes_once(&f, &checkout, page, &mut a);
        close_driver(&mut a);
    }

    eprintln!("phase bound_peer_undo_to_empty_retypes_once");
    {
        let f = NotebookFixture::rich();
        let component = component(&f, 11, ui::application::LaunchTarget::New);
        let checkout = component.record().clone();
        let mut a = component_driver(component);
        new_note(&mut a, 11, "Peer page");
        let page = f
            .records
            .read(&checkout.id, |doc| read::list_pages(doc)[0].id)
            .unwrap();
        a.perform_named("block_editor", &ui::a11y::Action::Focus)
            .unwrap();
        a.type_text("a");
        // B binds a nonempty page; A's undo of its typing empties it again.
        let mut b = driver(&f, 12, &checkout);
        a.set_modifiers(&["ctrl".into()]);
        a.key("z");
        a.set_modifiers(&[]);
        f.records
            .read(&checkout.id, |doc| {
                assert!(
                    read::page_block_snapshots(doc, page)
                        .iter()
                        .all(|(_, text)| text.is_empty())
                );
            })
            .unwrap();
        assert_empty_retypes_once(&f, &checkout, page, &mut b);
        close_driver(&mut a);
        close_driver(&mut b);
    }

    eprintln!("phase first_undo_after_import_preserves_every_imported_value_and_receipt");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let before = f.records.read(&checkout.id, content).unwrap();
        let mut a = driver(&f, 11, &checkout);
        invoke_with(
            &mut a,
            11,
            actions::page_resource_id(f.root),
            "rename",
            serde_json::json!({ "title": "first user edit" }),
        );
        f.records
            .with_history(&checkout.id, |doc, history| {
                assert_eq!(history.revision(), 1);
                history
                    .undo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                assert_eq!(content(doc), before);
                assert!(history.next_undo_in(doc, &f.author, 11).is_none());
            })
            .unwrap();
        close_driver(&mut a);
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn registry_authority_and_stale_page_views_keep_other_instances_live() {
    eprintln!("phase app_and_driver_use_real_notes_registry_and_revoked_authority_refuses");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut app =
            ui::App::from_component(component(&f, 11, existing(&checkout)), runner_inputs())
                .unwrap();
        app.init_layout();
        app.refresh_semantic_tree();
        let id = app
            .semantic_tree()
            .iter()
            .find(|node| {
                node.name.as_deref() == Some("Projects") && node.action("rename").is_some()
            })
            .unwrap()
            .id
            .clone();
        assert!(
            app.invoke_semantic_action(&id, "rename", Some("App title"))
                .unwrap()
        );
        assert_eq!(revision(&f, &checkout), 1);
        let mut b = driver(&f, 12, &checkout);
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.child),
            "rename",
            serde_json::json!({ "title": "Driver title" }),
        );
        assert_eq!(revision(&f, &checkout), 2);
        let stale = id;
        *f.authority.borrow_mut() =
            ui::permissions::PermissionContext::member(&f.author.0, ui::permissions::Role::Viewer)
                .at_unix_time(100);
        let before = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(
            app.invoke_semantic_action(&stale, "rename", Some("denied"))
                .is_err()
        );
        open_note(&mut b, 12, f.child, None);
        assert!(
            b.invoke_bound_action(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: 12,
                    address: notes::Notebook::RENAME.on(actions::page_resource_id(f.child))
                },
                serde_json::json!({"title":"denied"}).as_object().unwrap()
            )
            .is_err()
        );
        assert!(
            b.invoke_bound_action_confirmed(
                &ui::bound_dispatch::BoundActionAddress {
                    instance_id: 12,
                    address: notes::Notebook::DELETE.on(actions::page_resource_id(f.child))
                },
                &serde_json::Map::new()
            )
            .is_err()
        );
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            before
        );
        app.close_components();
        let reports = app.take_component_close_reports();
        assert!(!reports.is_empty());
        for report in reports {
            assert_closed(&report);
        }
        close_driver(&mut b);
    }

    eprintln!("phase bound_views_stale_page_refusal_and_close_reopen_keep_other_instance_live");
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut a = driver(&f, 11, &checkout);
        let mut b = driver(&f, 12, &checkout);
        let before = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        open_note(&mut a, 11, f.child, None);
        f.records
            .read_history(&checkout.id, |doc, history| {
                assert_eq!(
                    history.revision(),
                    0,
                    "after OpenPage marks {:?}",
                    read::page_block_marks(doc, f.root)
                );
            })
            .unwrap();
        let child_marks = f
            .records
            .read(&checkout.id, |doc| read::page_block_marks(doc, f.child))
            .unwrap();
        open_tag(&mut a, 11, f.tag);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| read::page_block_marks(doc, f.child))
                .unwrap(),
            child_marks
        );
        assert_eq!(revision(&f, &checkout), 0);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            before
        );
        invoke_confirmed(&mut b, 12, &actions::page_resource_id(f.child), "delete");
        let denied = ui::bound_dispatch::BoundActionAddress {
            instance_id: 11,
            address: notes::Notebook::RENAME.on(actions::page_resource_id(f.child)),
        };
        assert!(
            a.invoke_bound_action(
                &denied,
                serde_json::json!({"title":"phantom"}).as_object().unwrap()
            )
            .is_err()
        );
        a.relayout();
        assert!(
            f.records
                .read(&checkout.id, |doc| read::list_pages(doc)
                    .iter()
                    .all(|p| p.id != f.child))
                .unwrap()
        );
        close_driver(&mut a);
        let stale = ui::bound_dispatch::BoundActionAddress {
            instance_id: 11,
            address: notes::Notebook::RENAME.on(actions::page_resource_id(f.root)),
        };
        let mut reopened = driver(&f, 11, &checkout);
        assert!(
            a.invoke_bound_action_confirmed(
                &stale,
                serde_json::json!({"title":"stale lifetime"})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.root),
            "rename",
            serde_json::json!({ "title": "B survives" }),
        );
        invoke_with(
            &mut reopened,
            11,
            actions::page_resource_id(f.empty),
            "rename",
            serde_json::json!({ "title": "New A" }),
        );
        f.records
            .read(&checkout.id, |doc| {
                let workspace = notes::read::project(doc).unwrap();
                assert_eq!(workspace.get(f.root).unwrap().title, "B survives");
                assert_eq!(workspace.get(f.empty).unwrap().title, "New A");
            })
            .unwrap();
        assert_eq!(revision(&f, &checkout), 3);
        close_driver(&mut b);
        close_driver(&mut reopened);
    }
}

/// Type `text` at the start of `page`'s first paragraph.
fn prepend(doc: &mut automerge::AutoCommit, page: notes::PageId, text: &str) {
    let block = read::page_block_snapshots(doc, page)[0].0.clone();
    crate::component_support::edit_page_body(
        doc,
        page,
        &tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at: 0,
            del: 0,
            ins: text.into(),
        },
    )
    .unwrap();
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn peer_mounts_and_rejected_deltas_keep_owners_and_the_empty_cta() {
    eprintln!(
        "phase mounting_same_author_peer_preserves_pending_typing_owner_and_stale_callback_refuses"
    );
    {
        let f = NotebookFixture::rich();
        let checkout = notebook(&f);
        let mut a = BoundNotes::new(component(&f, 11, existing(&checkout)));
        let original = f
            .records
            .read(&checkout.id, |doc| {
                read::page_block_snapshots(doc, f.root)[0].1.clone()
            })
            .unwrap();
        a.pending_edit(|doc| prepend(doc, f.root, "A pending "))
            .unwrap();
        let before = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        assert!(before.1 > 0);
        let prepared = component(&f, 12, existing(&checkout));
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            before,
            "factory mount"
        );
        let mut b =
            ui::headless::FrameHarness::from_component(prepared, (1000, 700), runner_inputs())
                .unwrap();

        for _ in 0..3 {
            b.relayout();
        }
        assert_eq!(revision(&f, &checkout), 0);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.root)
                    [0]
                .1
                .clone())
                .unwrap(),
            format!("A pending {original}")
        );
        a.pending_edit(|doc| prepend(doc, f.root, "A continued "))
            .unwrap();
        assert_eq!(revision(&f, &checkout), 0);
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.child),
            "rename",
            serde_json::json!({ "title": "B action" }),
        );
        f.records
            .with_history(&checkout.id, |doc, history| {
                let pending = history.next_undo_in(doc, &f.author, 11).unwrap();
                assert_eq!(pending.instance_id, Some(11));
                assert_eq!(pending.author.as_ref(), Some(&f.author));
                assert_eq!(pending.before, before.0);
                history
                    .undo_in(doc, &f.author, 11, None, None, None)
                    .unwrap();
                assert_eq!(read::page_block_snapshots(doc, f.root)[0].1, original);
                assert_eq!(
                    read::list_pages(doc)
                        .iter()
                        .find(|p| p.id == f.child)
                        .unwrap()
                        .title,
                    "B action"
                );
            })
            .unwrap();
        a.close();
        let mut reopened = driver(&f, 11, &checkout);
        assert!(
            a.pending_edit(|_| panic!("closed callback was delivered"))
                .is_err()
        );
        invoke_with(
            &mut b,
            12,
            actions::page_resource_id(f.child),
            "rename",
            serde_json::json!({ "title": "B still live" }),
        );
        close_driver(&mut b);
        close_driver(&mut reopened);
    }

    eprintln!("phase bound_rejected_editor_delta_after_last_page_delete_shows_empty_cta");
    {
        let f = NotebookFixture::rich();
        let component = component(&f, 12, ui::application::LaunchTarget::New);
        let checkout = component.record().clone();
        let mut b =
            ui::headless::FrameHarness::from_component(component, (1000, 700), runner_inputs())
                .unwrap();
        new_note(&mut b, 12, "Last page");
        let page = f
            .records
            .read(&checkout.id, |doc| read::list_pages(doc)[0].id)
            .unwrap();
        b.perform_named("block_editor", &ui::a11y::Action::Focus)
            .unwrap();
        b.type_text("a");
        b.key("Enter");
        b.type_text("b");
        let blocks = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, page))
            .unwrap();
        let mut a = BoundNotes::new(crate::component_support::component(
            &f,
            11,
            existing(&checkout),
        ));
        let queued_before = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        let queued_revision = revision(&f, &checkout);
        a.queue_block_move(&blocks[1].0);
        assert_eq!(
            f.records
                .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
                .unwrap(),
            queued_before
        );
        assert_eq!(revision(&f, &checkout), queued_revision);
        invoke_confirmed(&mut b, 12, &actions::page_resource_id(page), "delete");
        let deleted = f
            .records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap();
        let deleted_revision = revision(&f, &checkout);
        // FrameHarness construction syncs the existing A widget; it does not replace NotesApp.
        let mut a = a.into_driver(f.author.clone());
        for _ in 0..3 {
            a.relayout();
            a.expect(
                &ui::headless::SemanticExpectation::named("notes.first_note")
                    .role(ui::a11y::Role::Button)
                    .name("New note"),
            )
            .unwrap();
            let snapshot = a.inspection();
            assert!(!has_selected_node(&snapshot.root));
            let panel = inspected_widget(&snapshot.root, "notes.first_note").unwrap();
            assert!(panel.children.is_empty());
            assert!(panel.semantics.as_ref().unwrap().value.is_none());
            assert!(inspected_widget(&snapshot.root, "note_title").is_none());
            assert!(inspected_widget(&snapshot.root, "block_editor").is_none());
            f.records
                .read(&checkout.id, |doc| {
                    assert!(notes::read::project(doc).unwrap().pages().is_empty());
                    assert_eq!((doc.base_heads(), doc.pending_ops()), deleted);
                })
                .unwrap();
            assert_eq!(revision(&f, &checkout), deleted_revision);
        }
        close_driver(&mut a);
        close_driver(&mut b);
    }
}

fn context_menu_select(driver: &mut ui::headless::FrameHarness, page: notes::PageId, label: &str) {
    fn menu_items(node: &ui::a11y::AccessNode, out: &mut Vec<(String, bool)>) {
        if node.state.hidden {
            return;
        }
        if node.role == ui::a11y::Role::MenuItem {
            out.push((
                node.name.clone().expect("menu item label"),
                node.state.focused,
            ));
        }
        for child in &node.children {
            menu_items(child, out);
        }
    }
    fn items(node: &ui::headless::InspectionNode, out: &mut Vec<(String, bool)>) {
        if let Some(semantics) = &node.semantics {
            menu_items(semantics, out);
        }
        for child in &node.children {
            items(child, out);
        }
    }
    let point = page_rect(driver, page).center();
    move_pointer(driver, point);
    mouse(
        driver,
        winit::event::MouseButton::Right,
        winit::event::ElementState::Pressed,
    );
    let mut visible = Vec::new();
    items(&driver.inspection().root, &mut visible);
    assert_eq!(
        visible.iter().filter(|(name, _)| name == label).count(),
        1,
        "expected exactly one visible {label} menu item: {visible:?}"
    );
    for _ in 0..=visible.len() {
        let mut current = Vec::new();
        items(&driver.inspection().root, &mut current);
        if current
            .iter()
            .any(|(name, focused)| name == label && *focused)
        {
            driver.key("Enter");
            return;
        }
        driver.key("ArrowDown");
    }
    panic!("keyboard navigation never focused {label}");
}

fn inspected_widget<'a>(
    node: &'a ui::headless::InspectionNode,
    id: &str,
) -> Option<&'a ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some(id) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| inspected_widget(child, id))
}

fn placeholder_key(driver: &ui::headless::FrameHarness) -> String {
    let snapshot = driver.inspection();
    let editor = inspected_widget(&snapshot.root, "block_editor").unwrap();
    let block_nodes = editor
        .semantics
        .as_ref()
        .unwrap()
        .children
        .iter()
        .filter(|node| {
            node.semantic_key
                .as_deref()
                .is_some_and(|key| key.starts_with("notes.block:"))
        })
        .collect::<Vec<_>>();
    assert_eq!(block_nodes.len(), 1);
    block_nodes[0].semantic_key.clone().unwrap()
}

fn has_selected_node(node: &ui::headless::InspectionNode) -> bool {
    fn selected(node: &ui::a11y::AccessNode) -> bool {
        node.state.selected || node.children.iter().any(selected)
    }
    node.semantics.as_ref().is_some_and(selected) || node.children.iter().any(has_selected_node)
}

fn assert_empty_retypes_once(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    page: notes::PageId,
    driver: &mut ui::headless::FrameHarness,
) {
    driver.relayout();
    let placeholder = placeholder_key(driver);
    let before = f
        .records
        .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    let revision_before = revision(f, checkout);
    for _ in 0..3 {
        driver.relayout();
        assert_eq!(
            placeholder_key(driver),
            placeholder,
            "empty placeholder must survive no-op sync"
        );
    }
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before
    );
    assert_eq!(revision(f, checkout), revision_before);
    driver
        .perform_named("block_editor", &ui::a11y::Action::Focus)
        .unwrap();
    driver.type_text("é");
    f.records
        .read(&checkout.id, |doc| {
            notes::read::validate(doc).unwrap();
            let blocks = read::page_block_snapshots(doc, page);
            assert_eq!(
                blocks,
                vec![(
                    placeholder.strip_prefix("notes.block:").unwrap().to_owned(),
                    "é".into()
                )]
            );
        })
        .unwrap();
    assert_eq!(
        revision(f, checkout),
        revision_before,
        "the typed word is one step, still open"
    );
    assert!(driver.state_mut().doc.base_heads().is_empty());
    assert_eq!(driver.state_mut().doc.pending_ops(), 0);
}

#[test]
fn bound_open_tag_uses_current_membership_without_prior_sync() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut a = driver(&f, 11, &checkout);
    let mut b = driver(&f, 12, &checkout);
    open_note(&mut a, 11, f.child, None);
    invoke_with(
        &mut b,
        12,
        NOTEBOOK,
        "field-edit",
        serde_json::json!({ "edit": notes::Notebook::paths()
            .pages
            .item(&notes::notebook::page_id(f.child))
            .tags
            .remove(&notes::notebook::page_id(f.tag)) }),
    );
    assign_tag_through(&mut b, 12, f.empty, f.tag);
    assign_tag_through(&mut b, 12, f.root, f.tag);
    // A still displays the old member, but OpenTag must choose the current first UUID.
    assert_eq!(
        inspected_widget(&a.inspection().root, "note_title")
            .unwrap()
            .semantics
            .as_ref()
            .unwrap()
            .value
            .as_deref(),
        Some("Launch")
    );
    assert_open_tag_view(&f, &checkout, &mut a, f.tag, "Projects");
    open_note(&mut a, 11, f.empty, None);
    assert_open_tag_view(&f, &checkout, &mut a, f.tag, "Empty");
    invoke_with(
        &mut b,
        12,
        NOTEBOOK,
        "new-tag",
        serde_json::json!({ "name": "Fresh" }),
    );
    let fresh = f
        .records
        .read(&checkout.id, |doc| {
            read::list_tags(doc)
                .into_iter()
                .find(|tag| tag.name == "Fresh")
                .unwrap()
                .id
        })
        .unwrap();
    assign_tag_through(&mut b, 12, f.child, fresh);
    assert_open_tag_view(&f, &checkout, &mut a, fresh, "Launch");
    close_driver(&mut a);
    close_driver(&mut b);
}

fn assert_open_tag_view(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    a: &mut ui::headless::FrameHarness,
    tag: uuid::Uuid,
    title: &str,
) {
    let before = f
        .records
        .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
        .unwrap();
    let history = revision(f, checkout);
    // Invoke directly before relayout; the public bound front door must resolve current data.
    a.invoke_bound_action(
        &ui::bound_dispatch::BoundActionAddress {
            instance_id: 11,
            address: notes::NotesEffect::OPEN_TAG.on(NOTEBOOK),
        },
        serde_json::json!({ "tag": tag }).as_object().unwrap(),
    )
    .unwrap();
    a.relayout();
    a.expect(&ui::headless::SemanticExpectation::named("note_title").value(title))
        .unwrap();
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| (doc.base_heads(), doc.pending_ops()))
            .unwrap(),
        before
    );
    assert_eq!(revision(f, checkout), history);
}

#[test]
fn page_tree_bound_action_preserves_rich_bodies_owned_undo_and_revocation() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let before = f.records.read(&checkout.id, content).unwrap();
    let bodies = f
        .records
        .read(&checkout.id, |doc| {
            [f.root, f.child, f.empty].map(|page| read::page_stored_blocks(doc, page))
        })
        .unwrap();
    let mut a = driver(&f, 11, &checkout);
    invoke(&mut a, 11, actions::page_resource_id(f.child), "outdent");
    assert_eq!(revision(&f, &checkout), 1);
    f.records
        .read(&checkout.id, |doc| {
            assert_eq!(
                read::list_pages(doc)
                    .iter()
                    .find(|page| page.id == f.child)
                    .unwrap()
                    .parent,
                None
            );
            assert_eq!(
                [f.root, f.child, f.empty].map(|page| read::page_stored_blocks(doc, page)),
                bodies
            );
        })
        .unwrap();
    f.records
        .with_history(&checkout.id, |doc, history| {
            history
                .undo_in(doc, &f.author, 11, None, None, None)
                .unwrap();
            assert_eq!(content(doc), before);
            assert!(history.next_undo_in(doc, &f.author, 11).is_none());
        })
        .unwrap();
    a.relayout();
    *f.authority.borrow_mut() =
        ui::permissions::PermissionContext::member(&f.author.0, ui::permissions::Role::Viewer)
            .at_unix_time(100);
    assert!(
        a.invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: 11,
                address: notes::Notebook::OUTDENT.on(actions::page_resource_id(f.child)),
            },
            &serde_json::Map::new()
        )
        .is_err()
    );
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| { content(doc) })
            .unwrap(),
        before
    );
    close_driver(&mut a);
}

#[test]
fn newly_created_note_opens_before_the_next_presentation_frame() {
    let f = NotebookFixture::rich();
    let checkout = notebook(&f);
    let mut app = driver(&f, 11, &checkout);
    let address = |address: tilo_actions::ActionAddress| ui::bound_dispatch::BoundActionAddress {
        instance_id: 11,
        address,
    };
    let arguments = serde_json::json!({"title": "Immediate canonical open"})
        .as_object()
        .unwrap()
        .clone();
    let create =
        app.invoke_bound_action(&address(notes::Notebook::NEW_NOTE.on(NOTEBOOK)), &arguments);
    assert!(create.is_ok(), "normal declared creation: {create:?}");
    let page = f
        .records
        .read(&checkout.id, |doc| {
            read::list_pages(doc)
                .into_iter()
                .find(|page| page.title == "Immediate canonical open")
                .map(|page| page.id)
        })
        .unwrap()
        .expect("creation already exists in canonical state");
    // No presentation frame is forced between the two admitted operations.
    // Open must validate against canonical state, including the newly committed note.
    let created_revision = revision(&f, &checkout);
    let rich = f
        .records
        .read(&checkout.id, |doc| read::page_stored_blocks(doc, f.child))
        .unwrap();
    let opened = app.invoke_bound_action(
        &address(notes::NotesEffect::OPEN_NOTE.on(NOTEBOOK)),
        serde_json::json!({ "page": page }).as_object().unwrap(),
    );
    let observed = opened.as_ref().ok().map(|_| {
        app.relayout();
        app.invoke_bound_action(
            &address(notes::NotesEffect::INSPECT_EDITOR.on(NOTEBOOK)),
            &serde_json::Map::new(),
        )
        .unwrap()
        .completed()
        .unwrap()
        .value
        .unwrap()
    });
    close_driver(&mut app);
    assert!(
        opened
            .as_ref()
            .ok()
            .cloned()
            .is_some_and(|started| started.completed().is_some()),
        "new canonical note opens with actual terminal completion: {opened:?}"
    );
    let observed = observed.expect("the completed Open can be inspected");
    for key in ["selected_page", "main_page", "editor_binding_page"] {
        assert_eq!(observed[key], page.to_string());
    }
    assert_eq!(observed["pending_page_events"], 0);
    assert_eq!(
        revision(&f, &checkout),
        created_revision,
        "Open is a local view operation"
    );
    assert_eq!(
        f.records
            .read(&checkout.id, |doc| read::page_stored_blocks(doc, f.child))
            .unwrap(),
        rich
    );
}

/// The document's content: its history's own bookkeeping (what is undone)
/// left out.
fn content(doc: &automerge::AutoCommit) -> serde_json::Value {
    let mut value = serde_json::to_value(automerge::AutoSerde::from(doc)).unwrap();
    if let Some(root) = value.as_object_mut() {
        root.retain(|key, _| !key.starts_with("tilo:undone:"));
    }
    value
}

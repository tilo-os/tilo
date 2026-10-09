use crate::component_support::*;
use notes::{markdown, read};

/// A panel showing `page` of `doc`, as a person opening it sees it.
fn panel(doc: &automerge::AutoCommit, page: notes::PageId, title: &str) -> notes::MainPanel {
    let mut panel = notes::MainPanel::new();
    panel.bind_canonical_page(doc, page, title).unwrap();
    panel
}

fn exported(doc: &automerge::AutoCommit, page: notes::PageId) -> String {
    notes::read::validate(doc).unwrap();
    let panel = panel(doc, page, "Code");
    markdown::serialize(&markdown::blocks_to_specs(panel.editor().blocks()))
}

#[allow(
    clippy::too_many_lines,
    reason = "end-to-end semantic test verifies nested code edits, persistence, and undo as one flow"
)]
#[test]
fn code_fences_keep_their_groups_through_import_reopen_and_nesting() {
    eprintln!("phase list_children_include_real_paragraphs_and_fences_after_import_and_reopen");
    {
        let source = "1. Parent A\n\n  Paragraph child\n\n  ```python\n  first()\n  second()\n  ```\n\n  - [x] Child task\n    1. Grandchild\n\n2. Parent B\n";
        let f = NotebookFixture::rich();
        f.write_page(f.child, source.as_bytes());
        let checkout = notebook(&f);
        let mut original = f.records.read(&checkout.id, Clone::clone).unwrap();
        let reopened = automerge::AutoCommit::load(&original.save()).unwrap();
        for doc in [&original, &reopened] {
            let panel = panel(doc, f.child, "Mixed content");
            let blocks = panel.editor().blocks();
            assert_eq!(
                blocks
                    .iter()
                    .map(ui::widget::Block::text)
                    .collect::<Vec<_>>(),
                [
                    "Parent A",
                    "Paragraph child",
                    "first()",
                    "second()",
                    "Child task",
                    "Grandchild",
                    "Parent B"
                ]
            );
            assert_eq!(blocks[1].kind, ui::widget::BlockKind::Paragraph);
            assert_eq!(
                blocks[2].kind,
                ui::widget::BlockKind::CodeBlock {
                    lang: Some("python".into())
                }
            );
            assert!(blocks[2].same_code_group(&blocks[3]));
            assert_eq!(
                blocks
                    .iter()
                    .map(ui::widget::Block::indent)
                    .collect::<Vec<_>>(),
                [0, 1, 1, 1, 1, 2, 0]
            );
            let actual = markdown::serialize(&markdown::blocks_to_specs(blocks));
            assert!(
                actual.contains("  ```python\n  first()\n  second()\n  ```"),
                "{actual}"
            );
            assert!(
                actual.ends_with("2. Parent B\n"),
                "numbering resumes after mixed children: {actual}"
            );
            assert_eq!(markdown::parse(&actual), markdown::parse(source));
        }
        let copy = crate::component_support::duplicate(&mut original, f.child);
        assert_eq!(exported(&original, copy), exported(&original, f.child));
    }

    eprintln!("phase nested_content_indents_semantically_and_exits_code_into_the_same_list_item");
    {
        let f = NotebookFixture::rich();
        f.write_page(f.child, b"1. Parent\n\n  Explanation\n\n  ```python\n  first()\n  second()\n  ```\n\n2. Next item\n");
        let checkout = notebook(&f);
        let rows = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap();
        let mut live = driver(&f, 93, &checkout);
        open_note(&mut live, 93, f.child, None);
        let before_revision = revision(&f, &checkout);
        live.perform_semantic_named(
            "block_editor",
            Some(&format!("notes.block-indent:{}:in", rows[3].0)),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
        assert_eq!(revision(&f, &checkout), before_revision + 1);
        let indented = f
            .records
            .read(&checkout.id, |doc| exported(doc, f.child))
            .unwrap();
        assert!(
            indented.contains("    ```python\n    first()\n    second()\n    ```"),
            "{indented}"
        );
        live.set_modifiers(&["ctrl".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        let restored = f
            .records
            .read(&checkout.id, |doc| exported(doc, f.child))
            .unwrap();
        assert!(
            restored.contains("  ```python\n  first()\n  second()\n  ```"),
            "{restored}"
        );
        let rect = semantic_rect(&live, &format!("notes.block:{}", rows[2].0));
        live.click_at(rect.x0 + 8.0, rect.center().y);
        live.set_modifiers(&["ctrl".into()]);
        live.key("]");
        live.set_modifiers(&[]);
        let indented = f
            .records
            .read(&checkout.id, |doc| exported(doc, f.child))
            .unwrap();
        assert!(
            indented.contains("    ```python\n    first()\n    second()\n    ```"),
            "{indented}"
        );
        live.set_modifiers(&["ctrl".into()]);
        live.key("[");
        live.set_modifiers(&[]);
        let rect = semantic_rect(&live, &format!("notes.block:{}", rows[1].0));
        live.click_at(rect.x0 + 8.0, rect.center().y);
        live.key("Tab");
        let indented = f
            .records
            .read(&checkout.id, |doc| exported(doc, f.child))
            .unwrap();
        assert!(indented.contains("    Explanation"), "{indented}");
        live.set_modifiers(&["shift".into()]);
        live.key("Tab");
        live.set_modifiers(&[]);
        let rect = semantic_rect(&live, &format!("notes.block:{}", rows[3].0));
        live.click_at(rect.x0 + 8.0, rect.center().y);
        live.set_modifiers(&["ctrl".into()]);
        live.key("Enter");
        live.set_modifiers(&[]);
        // An empty nested paragraph provides a keyboard route out of the item.
        live.key("Enter");
        let outside = f
            .records
            .read(&checkout.id, |doc| {
                read::page_blocks(doc, f.child)[4].nesting
            })
            .unwrap();
        assert_eq!(outside, 0);
        live.set_modifiers(&["ctrl".into()]);
        live.key("]");
        live.set_modifiers(&[]);
        live.type_text("Still inside the item");
        close_driver(&mut live);
        drop(live);
        f.records.try_flush().unwrap();
        let f = f.reopen();
        let mut reopened = driver(&f, 94, &checkout);
        let actual = f
            .records
            .read(&checkout.id, |doc| exported(doc, f.child))
            .unwrap();
        assert!(actual.contains("  Explanation"), "{actual}");
        assert!(actual.contains("  Still inside the item"), "{actual}");
        assert!(actual.ends_with("2. Next item\n"), "{actual}");
        close_driver(&mut reopened);
    }

    eprintln!(
        "phase adjacent_same_language_fences_remain_separate_after_canonical_import_and_reopen"
    );
    {
        let source = "```python\nfirst()\n```\n\n```python\nsecond()\n```\n";
        let expected = markdown::serialize(&markdown::parse(source));
        let f = NotebookFixture::rich();
        f.write_page(f.child, source.as_bytes());
        let checkout = notebook(&f);
        let mut original = f.records.read(&checkout.id, Clone::clone).unwrap();
        let reopened = automerge::AutoCommit::load(&original.save()).unwrap();
        for doc in [&original, &reopened] {
            notes::read::validate(doc).unwrap();
            let panel = panel(doc, f.child, "Separate fences");
            let actual = markdown::serialize(&markdown::blocks_to_specs(panel.editor().blocks()));
            assert_eq!(
                actual, expected,
                "two separately authored code blocks must retain their boundary"
            );
        }
    }
}

#[allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "clipboard backend and the full split/copy lifecycle are local to this regression test"
)]
#[test]
fn code_fence_edits_keep_identities_boundaries_and_copy_buttons() {
    eprintln!("phase deleting_first_fence_row_and_duplicating_keep_boundaries_and_new_group_ids");
    {
        let source = "```python\nfirst()\nkept()\n```\n\n```python\nsecond()\n```\n";
        let f = NotebookFixture::rich();
        f.write_page(f.child, source.as_bytes());
        let checkout = notebook(&f);
        let mut doc = f.records.read(&checkout.id, Clone::clone).unwrap();
        let first = read::page_block_snapshots(&doc, f.child)[0].0.clone();
        remove_block(&mut doc, f.child, &first);
        let expected = "```python\nkept()\n```\n\n```python\nsecond()\n```\n";
        let expected = markdown::serialize(&markdown::parse(expected));
        assert_eq!(exported(&doc, f.child), expected);
        let destination = crate::component_support::duplicate(&mut doc, f.child);
        let mut doc = automerge::AutoCommit::load(&doc.save()).unwrap();
        assert_eq!(exported(&doc, destination), expected);
        let groups = |page| {
            read::page_blocks(&doc, page)
                .into_iter()
                .map(|block| block.code_group.unwrap())
                .collect::<Vec<_>>()
        };
        let source_groups = groups(f.child);
        let copy_groups = groups(destination);
        assert_ne!(copy_groups[0], copy_groups[1]);
        assert!(
            copy_groups
                .iter()
                .all(|group| !source_groups.contains(group))
        );
        // Merely projecting the reopened note must never create durable state.
        let before = doc.get_heads();
        assert_eq!(exported(&doc, destination), expected);
        assert_eq!(doc.get_heads(), before);
    }

    eprintln!("phase multiline_paste_and_enter_keep_the_authored_fence_identity");
    {
        let mut editor = ui::widget::BlockEditor::new();
        editor.set_blocks(markdown::specs_to_blocks(&markdown::parse(
            "```python\na()\n```\n\n```python\nb()\n```\n",
        )));
        let first_group = editor.blocks()[0].code_group.clone();
        editor.split_at_cursor(0, 3);
        assert_eq!(editor.blocks()[1].code_group, first_group);
        assert!(!editor.blocks()[1].same_code_group(&editor.blocks()[2]));
        assert_eq!(markdown::blocks_to_specs(editor.blocks()).len(), 2);
        editor.select_document_range(
            ui::widget::DocumentPosition { block: 0, byte: 3 },
            ui::widget::DocumentPosition { block: 1, byte: 0 },
        );
        assert!(editor.replace_document_selection("pasted()\ncontinued()"));
        assert_eq!(editor.blocks()[1].code_group, first_group);
        assert!(!editor.blocks()[1].same_code_group(&editor.blocks()[2]));
        assert_eq!(editor.blocks()[0].text(), "a()pasted()");
        assert_eq!(editor.blocks()[1].text(), "continued()");
        assert_eq!(editor.blocks()[2].text(), "b()");
        assert_eq!(markdown::blocks_to_specs(editor.blocks()).len(), 2);
    }

    eprintln!("phase closing_a_fence_in_the_middle_keeps_both_copy_buttons_addressable");
    {
        let f = NotebookFixture::rich();
        f.write_page(
            f.child,
            b"```python\nfirst()\nsecond()\n\nthird()\nfourth()\n```\n",
        );
        let checkout = notebook(&f);
        let rows = f
            .records
            .read(&checkout.id, |doc| read::page_block_snapshots(doc, f.child))
            .unwrap();
        let mut live = driver(&f, 96, &checkout);
        open_note(&mut live, 96, f.child, None);
        let original = copy_targets(&live);
        assert_eq!(original.len(), 1);
        live.perform_semantic_named(
            "block_editor",
            Some(&format!("notes.block-text:{}", rows[2].0)),
            &ui::a11y::Action::SetSelection {
                anchor: 0,
                focus: 0,
            },
        )
        .unwrap();
        live.type_text("```");
        live.key("Enter");
        let targets = copy_targets(&live);
        assert_eq!(targets.len(), 2);
        assert_ne!(
            targets[0], targets[1],
            "each visible fence must have its own action target"
        );
        let before_copy = revision(&f, &checkout);
        // This test owns an in-memory clipboard, never the user's system clipboard.
        #[derive(Default)]
        struct Clipboard(std::sync::Mutex<String>);
        impl ui::clipboard::ClipboardBackend for Clipboard {
            fn set_text(&self, _: ui::clipboard::ClipboardTarget, text: &str) -> bool {
                *self.0.lock().unwrap() = text.into();
                true
            }
            fn text(&self, _: ui::clipboard::ClipboardTarget) -> Option<String> {
                Some(self.0.lock().unwrap().clone())
            }
            fn name(&self) -> &'static str {
                "notes-copy-test"
            }
        }
        let clipboard = std::sync::Arc::new(Clipboard::default());
        let services = ui::host::HostServices::null().with_clipboard(clipboard.clone());
        let _host = ui::host::enter(&services);
        for (target, expected) in targets
            .iter()
            .zip(["first()\nsecond()", "third()\nfourth()"])
        {
            live.perform_semantic_named("block_editor", Some(target), &ui::a11y::Action::Activate)
                .unwrap();
            assert_eq!(*clipboard.0.lock().unwrap(), expected);
        }
        assert_eq!(revision(&f, &checkout), before_copy);
        assert!(
            live.perform_semantic_named(
                "block_editor",
                Some(&original[0]),
                &ui::a11y::Action::Activate
            )
            .is_err(),
            "an old whole-fence target must not silently copy one fragment"
        );
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/notes-review/split-code-copy-dark.png");
        live.render_png(&output);
        live.set_modifiers(&["ctrl".into()]);
        live.key("z");
        live.set_modifiers(&[]);
        assert_eq!(copy_targets(&live), original);
        for target in &targets {
            assert!(
                live.perform_semantic_named(
                    "block_editor",
                    Some(target),
                    &ui::a11y::Action::Activate
                )
                .is_err(),
                "a stale fragment target must not expand to the restored full fence"
            );
        }
        live.set_modifiers(&["ctrl".into()]);
        live.key("y");
        live.set_modifiers(&[]);
        assert_eq!(copy_targets(&live), targets);
        close_driver(&mut live);
        drop(live);
        f.records.try_flush().unwrap();
        let f = f.reopen();
        let mut live = driver(&f, 96, &checkout);
        open_note(&mut live, 96, f.child, None);
        assert_eq!(copy_targets(&live), targets);
        for (target, expected) in targets
            .iter()
            .zip(["first()\nsecond()", "third()\nfourth()"])
        {
            live.perform_semantic_named("block_editor", Some(target), &ui::a11y::Action::Activate)
                .unwrap();
            assert_eq!(*clipboard.0.lock().unwrap(), expected);
        }
        for ((target, row), expected) in targets
            .iter()
            .zip([&rows[0].0, &rows[3].0])
            .zip(["first()\nsecond()", "third()\nfourth()"])
        {
            clipboard.0.lock().unwrap().clear();
            let button = semantic_rect(&live, target);
            live.click_at(button.center().x, button.center().y);
            assert_eq!(*clipboard.0.lock().unwrap(), expected);
            clipboard.0.lock().unwrap().clear();
            live.perform_semantic_named(
                "block_editor",
                Some(&format!("notes.block-text:{row}")),
                &ui::a11y::Action::Focus,
            )
            .unwrap();
            live.set_modifiers(&["ctrl".into(), "shift".into()]);
            live.key("c");
            live.set_modifiers(&[]);
            assert_eq!(*clipboard.0.lock().unwrap(), expected);
        }
        live.set_theme(ui::Theme::light().with_text_scale(2.0));
        live.resize((1000, 1000)).unwrap();
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/notes-review/split-code-copy-light-large.png");
        live.render_png(&output);
        close_driver(&mut live);
    }
}

fn copy_targets(live: &ui::headless::Driver) -> Vec<String> {
    fn collect(node: &ui::a11y::AccessNode, targets: &mut Vec<String>) {
        if let Some(target) = &node.action_target
            && target.starts_with("notes.code-copy:")
        {
            targets.push(target.clone());
        }
        for child in &node.children {
            collect(child, targets);
        }
    }
    fn widgets(node: &ui::headless::InspectionNode, targets: &mut Vec<String>) {
        if node.test_id.as_deref() == Some("block_editor") {
            collect(node.semantics.as_ref().unwrap(), targets);
        } else {
            for child in &node.children {
                widgets(child, targets);
            }
        }
    }
    let mut targets = Vec::new();
    widgets(&live.inspection().root, &mut targets);
    targets
}

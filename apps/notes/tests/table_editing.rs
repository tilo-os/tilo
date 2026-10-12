use ui::a11y::{AccessNode, Action};
use ui::headless::Framed;
use ui::theme::Theme;
use ui::widget::Edits;
use ui::widget::Widget;
use ui::widget::block_editor::{Block, BlockEditor};
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

struct Fixture {
    editor: Framed<BlockEditor>,
}

impl Fixture {
    fn new(source: &str) -> Self {
        let mut editor = BlockEditor::new().with_wrap_lines(true);
        editor.set_blocks(vec![
            Block::table(source, true),
            Block::table(source, true),
            Block::paragraph("After"),
        ]);
        let mut fixture = Self {
            editor: crate::component_support::framed_block_editor(
                editor,
                500.0,
                (0.0, 0.0),
                Theme::dark(),
            ),
        };
        fixture.editor.take_structure_dirty();
        fixture.editor.is_dirty();
        fixture
    }

    fn paint(&mut self) {
        Framed::harness_mut(&mut self.editor)
            .recompose()
            .expect("the table editor lays out");
    }

    fn target(&self, block: usize, row: usize, column: usize) -> String {
        format!(
            "notes.table-cell:{}:{row}:{column}",
            self.editor.blocks()[block].id
        )
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper owns each semantic action for immediate dispatch"
    )]
    fn action(&mut self, target: &str, action: Action) -> bool {
        let (editor, state) = Framed::split(&mut self.editor);
        editor.perform_semantic_action(
            target,
            &action,
            &mut Edits::lend(&mut state.doc),
            &state.root,
        )
    }

    fn value(&self, block: usize, row: usize, column: usize) -> String {
        fn find<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
            if node.action_target.as_deref() == Some(target) {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        let node = self.editor.accessibility().unwrap();
        find(&node, &self.target(block, row, column))
            .unwrap()
            .value
            .clone()
            .unwrap()
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper owns each synthetic event for immediate dispatch"
    )]
    fn event(
        &mut self,
        event: WindowEvent,
        modifiers: ModifiersState,
    ) -> Vec<ui::widget::BlockEditOp> {
        Framed::harness_mut(&mut self.editor)
            .dispatch_to_root_with(&event, &Modifiers::from(modifiers));
        let ops = self.editor.take_edit_ops();
        self.paint();
        ops
    }

    fn key(&mut self, key: Key, modifiers: ModifiersState) -> Vec<ui::widget::BlockEditOp> {
        let event = ui::synthetic_input::key_event(key, None, ElementState::Pressed);
        self.event(
            WindowEvent::KeyboardInput {
                device_id: winit::event::DeviceId::dummy(),
                event,
                is_synthetic: false,
            },
            modifiers,
        )
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn table_cell_editing_survives_echo_undo_and_reload() {
    eprintln!("phase typed_spaces_survive_each_model_echo_and_context_cut_stays_inside_the_cell");
    {
        let mut f = Fixture::new("| Header | Other |\n| --- | --- |\n| | untouched |");
        assert!(f.action(&f.target(0, 1, 0), Action::Focus));
        for text in ["two", " ", "words", " "] {
            assert!(
                !f.event(
                    WindowEvent::Ime(winit::event::Ime::Commit(text.into())),
                    ModifiersState::empty()
                )
                .is_empty()
            );
        }
        assert_eq!(f.value(0, 1, 0), "two words ");
        f.editor.set_focus(true); // body right-click restores editor focus
        assert!(f.action("notes.editor:select-all", Action::Activate));
        assert!(f.action("notes.editor:cut", Action::Activate));
        f.paint();
        assert_eq!(f.value(0, 1, 0), "");
        assert_eq!(f.value(0, 1, 1), "untouched");
        assert!(f.editor.blocks()[0].text().contains("| --- | --- |"));
    }

    eprintln!("phase escaped_cells_and_dash_values_survive_canonical_undo_and_reload");
    {
        use notes::read;
        let source = "| A | B |\n| --- | --- |\n| initial | other |";
        let mut f = Fixture::new(source);
        let target = f.target(0, 1, 0);
        assert!(!f.action(&target, Action::SetValue("initial".into())));
        assert!(f.editor.take_pending_semantic_ops().is_empty());
        assert!(f.action(&target, Action::SetValue(" a|b\nC\\D & <br> ".into())));
        let ops = f.editor.take_pending_semantic_ops();
        assert!(!ops.is_empty());
        f.paint();
        assert_eq!(f.value(0, 1, 0), " a|b\nC\\D & <br> ");

        let mut state = ui::State::new();
        notes::read::initialize(&mut state.doc);
        let page = uuid::Uuid::new_v4();
        crate::component_support::upsert_page(
            &mut state.doc,
            &notes::model::PageMeta {
                id: page,
                title: "Table".into(),
                parent: None,
            },
        );
        crate::component_support::apply_to(
            &mut state.doc,
            &notes::actions::write_body_action(
                page,
                &[ui::widget::BlockContent {
                    kind: ui::widget::BlockKind::Table { visual: true },
                    ..ui::widget::BlockContent::paragraph("", source)
                }],
            ),
        )
        .unwrap();
        state.commit_action(None, None);
        let id = read::page_block_snapshots(&state.doc, page)[0].0.clone();
        let edited = f.editor.blocks()[0].text().to_owned();
        crate::component_support::replace_block_text(&mut state.doc, page, &id, &edited);
        state.commit_action(None, None);
        let saved = read::page_block_snapshots(&state.doc, page)[0].1.clone();
        let reopened = automerge::AutoCommit::load(&state.doc.save()).unwrap();
        assert_eq!(read::page_block_snapshots(&reopened, page)[0].1, saved);
        let reopened_editor = Fixture::new(&saved);
        assert_eq!(reopened_editor.value(0, 1, 0), " a|b\nC\\D & <br> ");
        state.undo(None, None).unwrap();
        assert_eq!(read::page_block_snapshots(&state.doc, page)[0].1, source);
        state.redo(None, None).unwrap();
        assert_eq!(read::page_block_snapshots(&state.doc, page)[0].1, saved);

        let mut dash = Fixture::new("| Header |\n| --- |\n| value |");
        assert!(dash.action(&dash.target(0, 1, 0), Action::SetValue("---".into())));
        dash.paint();
        assert_eq!(dash.value(0, 1, 0), "---");
    }

    eprintln!("phase table_structure_commands_use_the_same_canonical_bridge");
    {
        let mut f = Fixture::new("| A | B |\n| :--- | ---: |\n| first | second |");
        assert!(f.action(&f.target(0, 1, 0), Action::Focus));
        assert!(f.action("notes.editor:table-add-row", Action::Activate));
        assert!(!f.editor.take_pending_semantic_ops().is_empty());
        f.paint();
        assert_eq!(f.value(0, 2, 0), "");
        assert_eq!(f.editor.focused_table_cell().unwrap().1, 2);
        assert!(f.action("notes.editor:table-add-column", Action::Activate));
        assert!(!f.editor.take_pending_semantic_ops().is_empty());
        f.paint();
        assert_eq!(f.value(0, 1, 2), "second");
        assert!(
            f.editor.blocks()[0]
                .text()
                .contains("| :--- | --- | ---: |")
        );
        assert!(f.action("notes.editor:table-delete-column", Action::Activate));
        assert!(f.action("notes.editor:table-delete-row", Action::Activate));
        f.paint();
        assert_eq!(f.value(0, 1, 0), "first");
        assert_eq!(f.value(0, 1, 1), "second");
        assert!(f.editor.blocks()[0].text().contains("| :--- | ---: |"));
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn table_focus_is_exclusive_and_survives_shape_changes() {
    eprintln!("phase focus_is_exclusive_and_navigation_has_no_content_operations");
    {
        let mut f = Fixture::new("| A | B |\n| --- | --- |\n| first | second |");
        assert!(f.action(&f.target(0, 1, 0), Action::Focus));
        assert!(f.action(&f.target(1, 1, 0), Action::Focus));
        for key in [NamedKey::ArrowRight, NamedKey::Tab, NamedKey::ArrowLeft] {
            assert!(f.key(Key::Named(key), ModifiersState::empty()).is_empty());
            f.editor.take_structure_dirty();
            assert!(!f.editor.is_dirty());
        }
        f.key(Key::Character("a".into()), ModifiersState::CONTROL);
        f.event(
            WindowEvent::Ime(winit::event::Ime::Commit("updated".into())),
            ModifiersState::empty(),
        );
        assert_eq!(f.value(0, 1, 1), "second");
        assert_eq!(f.value(1, 1, 1), "updated");
        assert!(!f.action(&f.target(1, 1, usize::MAX), Action::SetValue("bad".into())));
    }

    eprintln!("phase adding_a_row_to_another_table_transfers_editing_focus_exclusively");
    {
        let mut f = Fixture::new("| A | B |\n| --- | --- |\n| first | second |");
        assert!(f.action(&f.target(0, 1, 0), Action::Focus));
        let target_id = f.editor.blocks()[1].id.clone();
        assert!(f.action(
            &format!("notes.table-add-row:{target_id}"),
            Action::Activate
        ));
        assert!(!f.editor.take_pending_semantic_ops().is_empty());
        f.paint();
        assert_eq!(f.editor.focused_index(), Some(1));
        assert_eq!(f.editor.focused_table_cell(), Some((target_id, 1, 0)));
        assert!(
            f.editor
                .blocks()
                .iter()
                .all(|block| !block.line.is_focused())
        );
        let first_source = f.editor.blocks()[0].text().to_owned();
        let ops = f.event(
            WindowEvent::Ime(winit::event::Ime::Commit("only the second table".into())),
            ModifiersState::empty(),
        );
        assert!(!ops.is_empty());
        assert_eq!(f.editor.blocks()[0].text(), first_source);
        assert_eq!(f.value(0, 1, 0), "first");
        assert_eq!(f.value(1, 1, 0), "only the second table");
    }

    eprintln!("phase bound_table_keeps_cell_focus_after_canonical_shape_undo_and_sync");
    {
        use crate::framework_support::{body, surface, with_note};
        let (mut app, page) = with_note(
            "Bound table",
            "| A | B |\n| --- | --- |\n| first | second |",
        );
        let block_id = surface(&mut app, |notes| {
            notes.main_panel_mut().editor().blocks()[0].id.clone()
        });
        let cell =
            |row: usize, column: usize| format!("notes.table-cell:{block_id}:{row}:{column}");
        let value = |app: &mut ui::framework::testing::TestApp<notes::NotesShell>, row, column| {
            fn find<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
                if node.action_target.as_deref() == Some(target) {
                    return Some(node);
                }
                node.children.iter().find_map(|child| find(child, target))
            }
            let target = cell(row, column);
            surface(app, |notes| {
                let node = notes.main_panel_mut().editor().accessibility().unwrap();
                find(&node, &target).unwrap().value.clone().unwrap()
            })
        };
        let focused = |app: &mut ui::framework::testing::TestApp<notes::NotesShell>| {
            surface(app, |notes| {
                let editor = notes.main_panel_mut().editor();
                (
                    editor.focused_table_cell(),
                    editor.blocks()[0].line.is_focused(),
                )
            })
        };
        app.harness()
            .perform_semantic_named("block_editor", Some(&cell(1, 1)), &Action::Focus)
            .unwrap();
        app.harness()
            .perform_semantic_named(
                "block_editor",
                Some(&format!("notes.table-add-row:{block_id}")),
                &Action::Activate,
            )
            .unwrap();
        assert_eq!(focused(&mut app).0, Some((block_id.clone(), 2, 1)));

        // Undo removes the active row in the actual canonical document. Sync must
        // clamp the cell coordinate and must never focus the hidden Markdown input.
        assert!(app.undo());
        assert_eq!(focused(&mut app), (Some((block_id.clone(), 1, 1)), false));
        app.harness().commit_text("!");
        assert_eq!(value(&mut app, 1, 0), "first");
        assert_eq!(value(&mut app, 1, 1), "second!");
        let shown = surface(&mut app, |notes| {
            notes.main_panel_mut().editor().blocks()[0]
                .text()
                .to_owned()
        });
        assert_eq!(body(&app, page)[0], shown);

        // A peer appending a row must retain the existing cell's caret and
        // selection; typing immediately replaces the selected final character.
        app.harness().set_modifiers(&["shift".into()]);
        app.harness().key("ArrowLeft");
        app.harness().set_modifiers(&[]);
        let current = body(&app, page)[0].clone();
        app.merge_peer_edit(|doc| {
            crate::component_support::edit_body(
                doc,
                page,
                tilo_document::RichTextAction::SpliceText {
                    region: tilo_document::action::RegionKind::Body,
                    block: block_id.clone(),
                    at: current.chars().count(),
                    del: 0,
                    ins: "\n| remote | row |".into(),
                },
            );
        });
        app.harness().commit_text("?");
        assert_eq!(value(&mut app, 1, 1), "second?");
        assert_eq!(value(&mut app, 2, 0), "remote");
    }
}

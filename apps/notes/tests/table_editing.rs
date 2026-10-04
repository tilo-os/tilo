use std::collections::HashMap;
use taffy::prelude::*;
use ui::a11y::{AccessNode, Action};
use ui::headless::Framed;
use ui::presence::PresenceCtx;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::block_editor::{Block, BlockEditor};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
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
        editor.set_action_interception(true);
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
        editor.perform_semantic_action(target, &action, &mut state.doc, &state.root)
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
        self.editor.begin_action_intercept();
        Framed::harness_mut(&mut self.editor)
            .dispatch_to_root_with(&event, &Modifiers::from(modifiers));
        let ops = self.editor.take_intercepted_ops();
        self.paint();
        ops
    }

    fn key(&mut self, key: Key, modifiers: ModifiersState) -> Vec<ui::widget::BlockEditOp> {
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = key;
        event.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.text = None;
        self.event(
            WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event,
                is_synthetic: false,
            },
            modifiers,
        )
    }
}

/// A table page's body in the SDK editor, bound to its note.
struct Bound {
    panel: notes::MainPanel,
    tree: TaffyTree<()>,
    node: NodeId,
    renderer: ui::TextRenderer,
}

impl Bound {
    fn new(doc: &automerge::AutoCommit, page: notes::PageId) -> Self {
        let mut panel = notes::MainPanel::new();
        panel.bind_canonical_page(doc, page, "Bound table").unwrap();
        let mut bound = Self {
            panel,
            tree: TaffyTree::new(),
            node: NodeId::new(0),
            renderer: crate::component_support::renderer(),
        };
        bound.paint();
        bound
    }

    fn editor(&self) -> &BlockEditor {
        self.panel.editor()
    }

    fn paint(&mut self) {
        self.tree = TaffyTree::new();
        let mut nodes = HashMap::new();
        let surface = self.panel.body_editor_mut();
        self.node = surface.build_layout(&mut self.tree, &Theme::dark(), &mut nodes);
        let renderer = &mut self.renderer;
        self.tree
            .compute_layout_with_measure(
                self.node,
                Size {
                    width: AvailableSpace::Definite(500.0),
                    height: AvailableSpace::MaxContent,
                },
                |known, available, node, _, _| {
                    nodes.get(&node).map_or(Size::ZERO, |ptr| unsafe {
                        (**ptr).measure(known, available, renderer)
                    })
                },
            )
            .unwrap();
        let surface = self.panel.body_editor_mut();
        surface.apply_layout(&self.tree, self.node, Point::ZERO);
        surface.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut self.renderer,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    /// Bring the editor up to date with `state`, recording what the editor
    /// wrote as one undo step.
    fn sync(&mut self, state: &mut ui::State) {
        self.panel
            .body_editor_mut()
            .sync(&mut state.doc, &automerge::ROOT);
        if let Some(before) = ui::undo::take_committed_action(&mut self.panel) {
            state.record_committed_action(before, None, None);
        }
        self.paint();
    }

    fn target(&self, row: usize, column: usize) -> String {
        format!(
            "notes.table-cell:{}:{row}:{column}",
            self.editor().blocks()[0].id
        )
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper owns each semantic action for immediate dispatch"
    )]
    fn action(&mut self, state: &mut ui::State, target: &str, action: Action) -> bool {
        let done = self.panel.editor_mut().perform_semantic_action(
            target,
            &action,
            &mut state.doc,
            &automerge::ROOT,
        );
        self.sync(state);
        done
    }

    fn value(&mut self, row: usize, column: usize) -> String {
        fn find<'a>(node: &'a AccessNode, target: &str) -> Option<&'a AccessNode> {
            if node.action_target.as_deref() == Some(target) {
                return Some(node);
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        let target = self.target(row, column);
        let node = self.panel.body_editor_mut().accessibility().unwrap();
        find(&node, &target).unwrap().value.clone().unwrap()
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "test helper owns each synthetic event for immediate dispatch"
    )]
    fn event(&mut self, state: &mut ui::State, event: WindowEvent) {
        let node = self.node;
        self.panel.body_editor_mut().handle_event(
            &event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut state.doc,
                path: &automerge::ROOT,
                handled: &mut false,
                focus_target: &mut None,
                node_id: node,
                taffy: &self.tree,
            },
        );
        self.sync(state);
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
        use notes::doc::NotesDoc;
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
        notes::notebook::initialize_empty(&mut state.doc).unwrap();
        let page = uuid::Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut state.doc,
            &notes::model::PageMeta {
                id: page,
                title: "Table".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::fill_page_body(
            &mut state.doc,
            page,
            &[ui::widget::BlockContent {
                kind: ui::widget::BlockKind::Table { visual: true },
                ..ui::widget::BlockContent::paragraph("", source)
            }],
        )
        .unwrap();
        state.commit_action(None, None);
        let id = NotesDoc::page_block_snapshots(&state.doc, page)[0]
            .0
            .clone();
        let edited = f.editor.blocks()[0].text().to_owned();
        crate::component_support::replace_block_text(&mut state.doc, page, &id, &edited);
        state.commit_action(None, None);
        let saved = NotesDoc::page_block_snapshots(&state.doc, page)[0]
            .1
            .clone();
        let reopened = automerge::AutoCommit::load(&state.doc.save()).unwrap();
        assert_eq!(NotesDoc::page_block_snapshots(&reopened, page)[0].1, saved);
        let reopened_editor = Fixture::new(&saved);
        assert_eq!(reopened_editor.value(0, 1, 0), " a|b\nC\\D & <br> ");
        state.undo(None, None).unwrap();
        assert_eq!(
            NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
            source
        );
        state.redo(None, None).unwrap();
        assert_eq!(NotesDoc::page_block_snapshots(&state.doc, page)[0].1, saved);

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
        use notes::doc::NotesDoc;
        let source = "| A | B |\n| --- | --- |\n| first | second |";
        let mut state = ui::State::new();
        notes::notebook::initialize_empty(&mut state.doc).unwrap();
        let page = uuid::Uuid::new_v4();
        NotesDoc::upsert_page_meta(
            &mut state.doc,
            &notes::model::PageMeta {
                id: page,
                title: "Bound table".into(),
                parent: None,
                order: 0,
            },
        );
        NotesDoc::fill_page_body(
            &mut state.doc,
            page,
            &[ui::widget::BlockContent {
                kind: ui::widget::BlockKind::Table { visual: true },
                ..ui::widget::BlockContent::paragraph("", source)
            }],
        )
        .unwrap();
        state.commit_action(None, None);
        let mut b = Bound::new(&state.doc, page);
        let block_id = b.editor().blocks()[0].id.clone();
        assert!(b.action(&mut state, &b.target(1, 1), Action::Focus));
        assert!(b.action(&mut state, "notes.editor:table-add-row", Action::Activate));
        assert_eq!(
            b.editor().focused_table_cell(),
            Some((block_id.clone(), 2, 1))
        );

        // Undo removes the active row in the actual canonical document. Sync must
        // clamp the cell coordinate and must never focus the hidden Markdown input.
        state.undo(None, None).unwrap();
        b.sync(&mut state);
        assert_eq!(b.editor().focused_table_cell(), Some((block_id, 1, 1)));
        assert!(!b.editor().blocks()[0].line.is_focused());
        b.event(
            &mut state,
            WindowEvent::Ime(winit::event::Ime::Commit("!".into())),
        );
        assert_eq!(b.value(1, 0), "first");
        assert_eq!(b.value(1, 1), "second!");
        assert_eq!(
            NotesDoc::page_block_snapshots(&state.doc, page)[0].1,
            b.editor().blocks()[0].text()
        );

        // A peer appending a row must retain the existing cell's caret and
        // selection; typing immediately replaces the selected final character.
        let mut shift = unsafe { std::mem::zeroed::<KeyEvent>() };
        shift.logical_key = Key::Named(NamedKey::ArrowLeft);
        shift.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        shift.location = winit::keyboard::KeyLocation::Standard;
        shift.state = ElementState::Pressed;
        let node = b.node;
        b.panel.body_editor_mut().handle_event(
            &WindowEvent::KeyboardInput {
                device_id: unsafe { std::mem::zeroed() },
                event: shift,
                is_synthetic: false,
            },
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::from(ModifiersState::SHIFT),
                state: &mut state.doc,
                path: &automerge::ROOT,
                handled: &mut false,
                focus_target: &mut None,
                node_id: node,
                taffy: &b.tree,
            },
        );
        b.sync(&mut state);
        let current = b.editor().blocks()[0].text().to_owned();
        crate::component_support::edit_body(
            &mut state.doc,
            page,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: b.editor().blocks()[0].id.clone(),
                at: current.chars().count(),
                del: 0,
                ins: "\n| remote | row |".into(),
            },
        );
        b.sync(&mut state);
        b.event(
            &mut state,
            WindowEvent::Ime(winit::event::Ime::Commit("?".into())),
        );
        assert_eq!(b.value(1, 1), "second?");
        assert_eq!(b.value(2, 0), "remote");
    }
}

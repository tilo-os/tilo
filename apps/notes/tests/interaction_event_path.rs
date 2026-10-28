//! Interaction coverage that drives the hosted notebook surface on a live,
//! settled taffy layout — the same five-pass cycle `ui::App` runs — so
//! the tests exercise the *real* event path (focus gates, pointer
//! hit-tests, keyboard routing) rather than internal helper methods.
//!
//! Covers the reachability fixes:
//!   - `?` types into a focused note but opens help when it isn't focused,
//!   - Cmd-N creates a child regardless of which pane holds focus,
//!   - the sidebar "+" button creates a root page,
//!   - Delete / F2 / Arrow keys work once the tree is focused,
//!   - Alt-↑ / Alt-↓ reorder a page among its siblings.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::headless::Framed;
use ui::widget::Edits;

use crate::framework_support::{notes, with_note};
use notes::sidebar::OutEvent;
use notes::{NotesApp, NotesShell};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::widget::Widget;

fn layout_block_editor(
    editor: ui::widget::BlockEditor,
    width: f32,
) -> Framed<ui::widget::BlockEditor> {
    crate::component_support::framed_block_editor(editor, width, (50.0, 50.0), Theme::dark())
}

/// Lay an edited editor out again from scratch.
fn relayout_block_editor(editor: &mut Framed<ui::widget::BlockEditor>) {
    Framed::harness_mut(editor)
        .recompose()
        .expect("the block editor lays out");
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn rich_blocks_render_insets_copy_actions_and_contiguous_code() {
    eprintln!("phase code_copy_actions_follow_fences_through_reorder_and_first_row_deletion");
    {
        use ui::widget::BlockEditor;
        let mut editor = BlockEditor::new();
        editor.set_blocks(notes::markdown::specs_to_blocks(&notes::markdown::parse(
            "```python\nfirst()\nkept()\n```\n\n```python\nother()\n```\n",
        )));
        let mut editor = layout_block_editor(editor, 600.0);
        let targets = |editor: &BlockEditor| {
            editor
                .accessibility()
                .unwrap()
                .children
                .iter()
                .filter_map(|node| node.action_target.as_ref())
                .filter(|target| target.starts_with("notes.code-copy:"))
                .cloned()
                .collect::<Vec<_>>()
        };
        let before = targets(&editor);
        assert_eq!(before.len(), 2);
        assert_ne!(before[0], before[1]);
        let mut state = automerge::AutoCommit::new();
        let first = editor.blocks()[0].id.clone();
        assert!(editor.perform_semantic_action(
            &format!("notes.block-move:{first}:down"),
            &ui::a11y::Action::Activate,
            &mut Edits::lend(&mut state),
            &automerge::ROOT,
        ));
        relayout_block_editor(&mut editor);
        assert_eq!(editor.blocks()[0].text(), "other()");
        assert_eq!(targets(&editor), vec![before[1].clone(), before[0].clone()]);
        editor.blocks_mut().remove(1);
        relayout_block_editor(&mut editor);
        assert_eq!(editor.blocks()[1].text(), "kept()");
        assert_eq!(targets(&editor), vec![before[1].clone(), before[0].clone()]);
        editor.blocks_mut().remove(1);
        relayout_block_editor(&mut editor);
        assert_eq!(targets(&editor), vec![before[1].clone()]);
        assert!(
            !editor.perform_semantic_action(
                &before[0],
                &ui::a11y::Action::Activate,
                &mut Edits::lend(&mut state),
                &automerge::ROOT,
            ),
            "a removed fence must never copy another fence at its old position"
        );
    }

    eprintln!("phase nested_table_and_quote_use_the_same_outline_inset_as_prose");
    {
        let source = "- Parent\n\n  Explanation\n\n  > Quoted child\n\n  | Key | Value |\n  | --- | --- |\n  | Name | Nested |\n";
        let mut editor = ui::widget::BlockEditor::new().with_wrap_lines(true);
        editor.set_blocks(notes::markdown::specs_to_blocks(&notes::markdown::parse(
            source,
        )));
        let editor = layout_block_editor(editor, 500.0);
        let blocks = editor.blocks();
        assert_eq!(blocks.len(), 4);
        assert_eq!(
            blocks
                .iter()
                .map(ui::widget::Block::indent)
                .collect::<Vec<_>>(),
            [0, 1, 1, 1]
        );
        assert!(matches!(
            blocks[3].kind,
            ui::widget::BlockKind::Table { visual: true }
        ));
        assert!((blocks[3].line.get_rect().x0 - blocks[1].line.get_rect().x0).abs() < 0.01);
        assert!((blocks[2].line.get_rect().x0 - blocks[1].line.get_rect().x0 - 16.0).abs() < 0.01);
        let exported = notes::markdown::serialize(&notes::markdown::blocks_to_specs(blocks));
        assert_eq!(
            notes::markdown::parse(&exported),
            notes::markdown::parse(source)
        );
    }

    eprintln!("phase multiline_code_block_renders_as_one_contiguous_unit");
    {
        // Bug: code blocks rendered as separate lines with gaps. A multi-line
        // fenced block must render as ONE contiguous unit — consecutive code
        // lines butt together (no inter-line gap), each in the monospace face.
        let body = "Intro line.\n\n```rust\nlet a = 1;\nlet b = 2;\nlet c = 3;\n```\n\nAfter.\n";
        let (mut app, _) = with_note("T", body);

        app.harness().relayout_at((1200, 800)).unwrap();

        let editor = n(&mut app).main_panel_mut().editor();
        let blocks = editor.blocks();
        let code_idx: Vec<usize> = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.kind, ui::widget::BlockKind::CodeBlock { .. }))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(code_idx.len(), 3, "a 3-line fence yields 3 code cells");

        // Consecutive code lines are contiguous — no 8px inter-block gap.
        for w in code_idx.windows(2) {
            let a = blocks[w[0]].line.get_rect();
            let b = blocks[w[1]].line.get_rect();
            let gap = b.y0 - a.y1;
            assert!(
                gap.abs() < 1.0,
                "code lines must be contiguous (one visual block), gap was {gap}"
            );
        }
        for &i in &code_idx {
            assert!(
                blocks[i].line.inner().is_mono(),
                "code cells render in the monospace face"
            );
        }
        // The paragraph AFTER the code block keeps its inter-block gap, proving
        // the no-gap rule is code-specific, not global.
        let last = *code_idx.last().unwrap();
        if last + 1 < blocks.len() {
            let a = blocks[last].line.get_rect();
            let p = blocks[last + 1].line.get_rect();
            assert!(
                p.y0 - a.y1 > 4.0,
                "a normal block after the code run keeps its gap"
            );
        }
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn editor_typing_selection_and_block_promotion_use_the_event_path() {
    eprintln!("phase backspace_at_note_start_does_not_delete_or_switch_page");
    {
        // Regression: with a page selected from the tree, the tree and the
        // editor both hold focus. A Backspace at the very start of the note
        // used to fall through to the tree's page-delete binding, deleting the
        // note and jumping the user to another one. It must be a no-op now.
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        // A second root page so a stray delete would visibly "switch" pages.
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        // Select the first page from the tree — this is the dual-focus state
        // (tree focused for its shortcuts, editor focused for typing).
        click_row(&mut app, 0).unwrap();
        assert_eq!(n(&mut app).selected_for_test(), Some(welcome));
        // Put the caret at the very start of the first block.
        n(&mut app).main_panel_mut().editor_mut().blocks_mut()[0]
            .line
            .set_cursor_pos(0);

        let before = n(&mut app).workspace_pages_for_test().len();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &named_key(NamedKey::Backspace));

        assert_eq!(
            n(&mut app).workspace_pages_for_test().len(),
            before,
            "Backspace at the start of a note must NOT delete a page"
        );
        assert_eq!(
            n(&mut app).selected_for_test(),
            Some(welcome),
            "Backspace at the start of a note must NOT switch pages"
        );
    }

    eprintln!("phase double_click_in_editor_selects_a_word");
    {
        // Regression guard for "double-click gives a weird selection": a
        // double-click inside a note block must select exactly the word under
        // the cursor (via the ui TextInput's multi-click ladder), never a
        // stray partial or cross-block range.
        let (mut app, _) = with_note("T", "the quick brown fox\n");

        app.harness().relayout_at((1200, 800)).unwrap();

        // Click a point a few characters into the first block.
        let rect = n(&mut app).main_panel_mut().editor().blocks()[0]
            .line
            .get_rect();
        let x = rect.x0 + 60.0;
        let y = rect.y0 + rect.height() / 2.0;
        let no_mods = Modifiers::default();

        // Single click: caret only, no selection.
        dispatch(&mut app, no_mods, &cursor_moved(x, y));
        dispatch(&mut app, no_mods, &mouse_press());
        dispatch(&mut app, no_mods, &mouse_release());
        assert!(
            n(&mut app).main_panel_mut().editor().blocks()[0]
                .line
                .selection_range()
                .is_none(),
            "a single click places the caret without selecting a word"
        );

        // Double click at the same spot: exactly the word under the cursor.
        dispatch(&mut app, no_mods, &cursor_moved(x, y));
        dispatch(&mut app, no_mods, &mouse_press());
        dispatch(&mut app, no_mods, &mouse_release());

        let block = &n(&mut app).main_panel_mut().editor().blocks()[0];
        let text = block.text().to_string();
        let (s, e) = block
            .line
            .selection_range()
            .expect("double-click selects a word");
        assert!(e > s, "the selection is non-empty");
        let word = text
            .get(s..e)
            .expect("selection range lands on a char boundary");
        assert!(
            !word.trim().is_empty() && !word.contains(' '),
            "the selection is exactly one word (no spaces), got {word:?}"
        );
        assert!(
            text.split_whitespace().any(|w| w == word),
            "the selected span is a whole word from the line, got {word:?}"
        );
    }

    eprintln!("phase editing_the_in_page_title_renames_the_page_and_updates_the_sidebar");
    {
        // Clicking the prominent title opens an inline rename; committing it renames
        // the page in the model AND re-syncs the sidebar row — the in-note title and
        // the tree stay in step.
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;

        app.harness().relayout_at((1200, 800)).unwrap();
        assert_eq!(n(&mut app).sidebar_row_title_for_test(0), Some("Welcome"));

        // Click the title band → begin editing (whole title selected).
        let c = n(&mut app).main_title_center_for_test();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        assert!(
            n(&mut app).main_title_editing_for_test(),
            "clicking the title opens the inline rename"
        );

        // Type a new name — select-all means it replaces the old value — then Enter.
        for ch in ["R", "o", "a", "d", "m", "a", "p"] {
            dispatch(&mut app, no_mods, &char_key(ch));
        }
        dispatch(&mut app, no_mods, &named_key(NamedKey::Enter));

        assert!(
            !n(&mut app).main_title_editing_for_test(),
            "Enter commits and ends the edit"
        );
        let pages = n(&mut app).workspace_pages_for_test();
        let renamed = pages.iter().find(|p| p.id == welcome).unwrap();
        assert_eq!(
            renamed.title, "Roadmap",
            "committing the title renames the page"
        );
        assert_eq!(
            n(&mut app).main_panel_mut().title(),
            Some("Roadmap"),
            "the panel shows the new title"
        );
        assert_eq!(
            n(&mut app).sidebar_row_title_for_test(0),
            Some("Roadmap"),
            "the rename propagates into the sidebar row"
        );

        // Escape must cancel without renaming.
        let c = n(&mut app).main_title_center_for_test();
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        for ch in ["N", "o", "p", "e"] {
            dispatch(&mut app, no_mods, &char_key(ch));
        }
        dispatch(&mut app, no_mods, &named_key(NamedKey::Escape));
        assert!(
            !n(&mut app).main_title_editing_for_test(),
            "Escape ends the edit"
        );
        let pages = n(&mut app).workspace_pages_for_test();
        let after = pages.iter().find(|p| p.id == welcome).unwrap();
        assert_eq!(
            after.title, "Roadmap",
            "Escape cancels — the title is unchanged"
        );
    }

    eprintln!("phase hash_space_promotes_paragraph_to_heading_through_the_event_path");
    {
        // The markdown shortcut, proven on the real keystroke flow: typing `#`
        // then a space at the start of an empty paragraph turns it into an H1 and
        // strips the `# ` prefix. This is the sequence a user actually types —
        // the old code only fired when the block was empty *before* the keystroke,
        // so the space (typed into a non-empty "#") never promoted.
        let mut app = starter();
        // A fresh, empty page so the first block is an empty paragraph.
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());

        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &char_key("#"));
        // A lone `#` must NOT promote — the prefix isn't complete yet.
        assert!(
            matches!(
                n(&mut app).main_panel_mut().editor().blocks()[0].kind,
                ui::widget::BlockKind::Paragraph
            ),
            "a bare `#` stays a paragraph until the space completes the prefix"
        );
        dispatch(&mut app, no_mods, &char_key(" "));

        {
            let blocks = n(&mut app).main_panel_mut().editor().blocks();
            assert_eq!(blocks.len(), 1);
            assert!(
                matches!(blocks[0].kind, ui::widget::BlockKind::Heading(1)),
                "`# ` promotes the paragraph to an H1"
            );
            assert_eq!(
                blocks[0].text(),
                "",
                "the `# ` prefix is stripped on promotion"
            );
        }

        // `## ` and `### ` promote to H2 / H3 on their own fresh pages.
        for (hashes, level) in [("##", 2u8), ("###", 3u8)] {
            n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
            app.harness().relayout();
            app.harness().relayout();
            app.harness().relayout_at((1200, 800)).unwrap();
            for ch in hashes.chars() {
                dispatch(&mut app, no_mods, &char_key(&ch.to_string()));
            }
            dispatch(&mut app, no_mods, &char_key(" "));
            let blocks = n(&mut app).main_panel_mut().editor().blocks();
            assert!(
                matches!(blocks[0].kind, ui::widget::BlockKind::Heading(l) if l == level),
                "`{hashes} ` promotes to a level-{level} heading"
            );
            assert_eq!(blocks[0].text(), "");
        }
    }

    eprintln!("phase greater_than_space_promotes_paragraph_to_quote_through_the_event_path");
    {
        let mut app = starter();
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &char_key(">"));
        assert!(matches!(
            n(&mut app).main_panel_mut().editor().blocks()[0].kind,
            ui::widget::BlockKind::Paragraph
        ));
        dispatch(&mut app, no_mods, &char_key(" "));

        let blocks = n(&mut app).main_panel_mut().editor().blocks();
        assert!(matches!(blocks[0].kind, ui::widget::BlockKind::Quote));
        assert_eq!(blocks[0].text(), "");
    }
}

use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{ModifiersState, NamedKey};

use ui::synthetic_input::cursor_moved;

use ui::synthetic_input::mouse_press;

fn mouse_press_right() -> WindowEvent {
    ui::synthetic_input::mouse_input(
        ui::winit::event::ElementState::Pressed,
        ui::winit::event::MouseButton::Right,
    )
}

use ui::synthetic_input::mouse_release;

use ui::synthetic_input::char_key;

use ui::synthetic_input::named_key;

/// Notes with a Welcome note that uses every block kind, open.
fn starter() -> TestApp<NotesShell> {
    with_note(
        "Welcome",
        crate::welcome_page_renders_all_block_kinds::WELCOME_BODY,
    )
    .0
}

/// The notebook surface, between the events a test sends.
fn n(app: &mut TestApp<NotesShell>) -> &mut NotesApp {
    app.surface_mut::<NotesApp>()
}

/// Deliver `event`, then the frame that follows it, where the notebook
/// applies what the event asked for.
fn dispatch(app: &mut TestApp<NotesShell>, modifiers: Modifiers, event: &WindowEvent) -> bool {
    let handled = app.harness().dispatch_to_root_with(event, &modifiers);
    app.harness().relayout();
    handled
}

/// Click the centre of sidebar row `idx` to select it AND give the
/// sidebar keyboard focus (the gate the tree's key commands require).
fn click_row(app: &mut TestApp<NotesShell>, idx: usize) -> Result<(), String> {
    let no_mods = Modifiers::default();
    let c = n(app)
        .sidebar_row_center_for_test(idx)
        .ok_or_else(|| "row laid out".to_string())?;
    dispatch(app, no_mods, &cursor_moved(c.x, c.y));
    dispatch(app, no_mods, &mouse_press());
    Ok(())
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn first_note_empty_state_and_help_shortcut_respect_focus() {
    eprintln!("phase first_note_empty_state_has_a_working_primary_action");
    {
        let mut app = notes();
        assert!(n(&mut app).workspace_pages_for_test().is_empty());

        app.harness().relayout_at((1024, 720)).unwrap();
        // Paint once so the first-note action resolves its hit rect.
        app.harness().relayout();
        let c = n(&mut app)
            .main_panel_mut()
            .empty_action_center_for_test()
            .expect("empty state renders a Create note action");

        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        assert!(
            dispatch(&mut app, no_mods, &mouse_press(),),
            "the first-note action consumes its click"
        );

        assert_eq!(n(&mut app).workspace_pages_for_test().len(), 1);
        assert!(n(&mut app).selected_for_test().is_some());
    }

    eprintln!("phase focused_first_note_action_activates_with_enter");
    {
        let mut app = notes();
        app.harness().relayout_at((1024, 720)).unwrap();

        n(&mut app).main_panel_mut().set_focus(true);
        let no_mods = Modifiers::default();
        assert!(
            dispatch(&mut app, no_mods, &named_key(NamedKey::Enter),),
            "Enter activates the focused Create note button"
        );
        assert_eq!(n(&mut app).workspace_pages_for_test().len(), 1);
        assert!(n(&mut app).selected_for_test().is_some());
    }

    eprintln!("phase focused_first_note_action_activates_with_space");
    {
        let mut app = notes();
        app.harness().relayout_at((1024, 720)).unwrap();

        n(&mut app).main_panel_mut().set_focus(true);
        let no_mods = Modifiers::default();
        assert!(
            dispatch(&mut app, no_mods, &char_key(" "),),
            "Space activates the focused Create note button"
        );
        assert_eq!(n(&mut app).workspace_pages_for_test().len(), 1);
        assert!(n(&mut app).selected_for_test().is_some());
    }

    eprintln!("phase question_mark_types_into_focused_note_and_does_not_open_help");
    {
        // Regression guard: `?` must be a literal character in a note. The
        // editor holds focus after load, so the global help shortcut must
        // yield to typing.
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());

        let no_mods = Modifiers::default();
        let handled = dispatch(&mut app, no_mods, &char_key("?"));

        assert!(
            !n(&mut app).shortcuts_visible_for_test(),
            "`?` must NOT open the shortcuts overlay while a note is focused"
        );
        assert!(
            n(&mut app).editor_text_for_test().contains('?'),
            "`?` must be typed into the focused note; got {:?}",
            n(&mut app).editor_text_for_test()
        );
        assert!(handled, "the editor should consume the typed `?`");
    }

    eprintln!("phase question_mark_types_into_the_sidebar_rename_field_and_ctrl_slash_opens_help");
    {
        // A bare `?` is text wherever a field holds the keyboard; help is a
        // chord, Ctrl+/, and it works with the note focused.
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();

        let before = n(&mut app).editor_text_for_test();
        n(&mut app).blur_editor_for_test();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &char_key("?"));
        assert!(
            !n(&mut app).shortcuts_visible_for_test(),
            "`?` is never the help key"
        );

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(dispatch(&mut app, ctrl, &char_key("/")));
        assert!(
            n(&mut app).shortcuts_visible_for_test(),
            "Ctrl+/ opens the shortcuts overlay"
        );
        assert_eq!(
            n(&mut app).editor_text_for_test(),
            before,
            "opening help must not mutate the note body"
        );
        dispatch(&mut app, no_mods, &named_key(NamedKey::Escape));

        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());
        let before = n(&mut app).editor_text_for_test();
        assert!(dispatch(&mut app, ctrl, &char_key("/")));
        assert!(
            n(&mut app).shortcuts_visible_for_test(),
            "Ctrl+/ opens help while a note is being typed into"
        );
        assert_eq!(n(&mut app).editor_text_for_test(), before);
    }

    eprintln!("phase open_help_overlay_blocks_the_editor");
    {
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();

        n(&mut app).shortcuts_mut().set_visible(true);
        let before = n(&mut app).editor_text_for_test();
        let no_mods = Modifiers::default();
        let handled = dispatch(&mut app, no_mods, &char_key("x"));

        assert!(handled, "the overlay must consume keystrokes while visible");
        assert_eq!(
            n(&mut app).editor_text_for_test(),
            before,
            "no keystroke may reach the editor while the overlay is up"
        );
        assert!(n(&mut app).shortcuts_visible_for_test());

        dispatch(&mut app, no_mods, &named_key(NamedKey::Escape));
        assert!(
            !n(&mut app).shortcuts_visible_for_test(),
            "Esc must close the overlay"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn page_tree_keyboard_and_pointer_commands_are_gated_on_tree_focus() {
    eprintln!("phase ctrl_n_creates_a_top_level_note_even_while_editor_is_focused");
    {
        // Handled at the app level, Ctrl+N works whichever pane holds focus.
        // It makes a top-level note: taking the selection as the parent
        // nested every quick note one level under the last.
        let mut app = starter();
        let root = n(&mut app).workspace_pages_for_test()[0].id;
        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());
        assert_eq!(n(&mut app).selected_for_test(), Some(root));

        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(
            dispatch(&mut app, ctrl, &char_key("n")),
            "Ctrl+N must be consumed at the app level"
        );
        let first = n(&mut app).selected_for_test().expect("new note selected");
        dispatch(&mut app, Modifiers::default(), &named_key(NamedKey::Escape));
        assert!(dispatch(&mut app, ctrl, &char_key("n")));
        let second = n(&mut app).selected_for_test().expect("new note selected");
        let pages = n(&mut app).workspace_pages_for_test();
        assert_eq!(pages.len(), 3, "each Ctrl+N adds exactly one note");
        for id in [first, second] {
            assert_ne!(id, root);
            let meta = pages.iter().find(|p| p.id == id).unwrap();
            assert_eq!(meta.parent, None, "Ctrl+N makes a top-level note");
        }
    }

    eprintln!("phase ctrl_shift_n_creates_a_note_inside_the_selected_one");
    {
        let mut app = starter();
        let root = n(&mut app).workspace_pages_for_test()[0].id;
        app.harness().relayout_at((1200, 800)).unwrap();
        let ctrl_shift = Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert!(dispatch(&mut app, ctrl_shift, &char_key("N")));
        let pages = n(&mut app).workspace_pages_for_test();
        assert_eq!(pages.len(), 2);
        let new_id = n(&mut app).selected_for_test().expect("new note selected");
        let meta = pages.iter().find(|p| p.id == new_id).unwrap();
        assert_eq!(
            meta.parent,
            Some(root),
            "Ctrl+Shift+N nests under the selection"
        );
    }

    eprintln!("phase plus_button_creates_a_root_page");
    {
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();
        assert_eq!(n(&mut app).workspace_pages_for_test().len(), 1);

        let no_mods = Modifiers::default();
        let btn = n(&mut app).sidebar_new_page_btn_center_for_test();
        dispatch(&mut app, no_mods, &cursor_moved(btn.x, btn.y));
        let handled = dispatch(&mut app, no_mods, &mouse_press());
        let mut release = mouse_press();
        if let WindowEvent::MouseInput { state, .. } = &mut release {
            *state = ElementState::Released;
        }
        // Keep the direct root press diagnostic above, then admit its
        // release through the host that owns queued native control actions.
        app.harness().send(&release);
        app.settle();

        assert!(handled, "clicking the + button must be consumed");
        assert!(
            app.harness().control_failures().is_empty(),
            "{:?}",
            app.harness().control_failures()
        );
        let pages = n(&mut app).workspace_pages_for_test();
        if pages.len() != 2 {
            let resources = app.harness().resources();
            eprintln!(
                "new-note target {:?}; native controls {:?}; contract violations {:?}",
                app.harness().targets().get("new-note"),
                resources
                    .iter()
                    .filter(|resource| resource.kind == "tilo.control")
                    .map(|resource| (
                        &resource.id,
                        resource
                            .actions
                            .iter()
                            .map(|action| &action.id)
                            .collect::<Vec<_>>()
                    ))
                    .collect::<Vec<_>>(),
                app.harness().component_contract_violations()
            );
        }
        assert_eq!(pages.len(), 2, "the + button must add exactly one page");
        let new_id = n(&mut app).selected_for_test().unwrap();
        let new_meta = pages.iter().find(|p| p.id == new_id).unwrap();
        assert_eq!(
            new_meta.parent, None,
            "the + button must create a ROOT-level page"
        );
    }

    eprintln!("phase delete_key_removes_selected_page_after_focusing_tree");
    {
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        let victim = n(&mut app).selected_for_test().unwrap();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        // Row 1 is the newly-created second root page. Click focuses the
        // tree AND selects it; then Delete removes it through the gated
        // keyboard path.
        click_row(&mut app, 1).unwrap();
        assert_eq!(n(&mut app).selected_for_test(), Some(victim));

        let no_mods = Modifiers::default();
        let handled = dispatch(&mut app, no_mods, &named_key(NamedKey::Delete));

        assert!(handled, "Delete must be consumed by the focused tree");
        // Undo takes a deleted page back, so nobody is asked.
        assert!(app.asking().is_none(), "nothing asks first");
        let pages = n(&mut app).workspace_pages_for_test();
        assert_eq!(pages.len(), 1, "Delete must remove the page");
        assert_eq!(pages[0].id, welcome);
        assert_eq!(
            n(&mut app).selected_for_test(),
            Some(welcome),
            "selection falls back"
        );
    }

    eprintln!("phase delete_key_is_gated_on_tree_focus");
    {
        // Without focusing the tree (editor holds focus after load), Delete
        // must NOT delete a page — it belongs to the editor. This proves the
        // focus gate is real, not bypassed.
        let mut app = starter();
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();
        let before = n(&mut app).workspace_pages_for_test().len();

        app.harness().relayout_at((1200, 800)).unwrap();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &named_key(NamedKey::Delete));

        assert_eq!(
            n(&mut app).workspace_pages_for_test().len(),
            before,
            "Delete must not delete a page while the editor holds focus"
        );
    }

    eprintln!("phase f2_opens_inline_rename_after_focusing_tree");
    {
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();

        click_row(&mut app, 0).unwrap();
        assert!(!n(&mut app).sidebar_is_renaming_for_test());

        let no_mods = Modifiers::default();
        let handled = dispatch(&mut app, no_mods, &named_key(NamedKey::F2));
        assert!(handled, "F2 must be consumed by the focused tree");
        assert!(
            n(&mut app).sidebar_is_renaming_for_test(),
            "F2 must open the inline rename editor"
        );
    }

    eprintln!("phase arrow_keys_move_selection_after_focusing_tree");
    {
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        let second = n(&mut app).selected_for_test().unwrap();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        click_row(&mut app, 0).unwrap();
        assert_eq!(n(&mut app).selected_for_test(), Some(welcome));

        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &named_key(NamedKey::ArrowDown));
        assert_eq!(
            n(&mut app).selected_for_test(),
            Some(second),
            "ArrowDown moves selection to the next row"
        );
        dispatch(&mut app, no_mods, &named_key(NamedKey::ArrowUp));
        assert_eq!(
            n(&mut app).selected_for_test(),
            Some(welcome),
            "ArrowUp moves selection back up"
        );
    }

    eprintln!("phase alt_arrow_reorders_selected_page_among_siblings");
    {
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        let p1 = n(&mut app).selected_for_test().unwrap();
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        let p2 = n(&mut app).selected_for_test().unwrap();
        app.harness().relayout();

        // Sanity: creation order is [welcome, p1, p2].
        let order_ids = |app: &mut TestApp<NotesShell>| -> Vec<uuid::Uuid> {
            let mut ps = n(app).workspace_pages_for_test();
            ps.retain(|p| p.parent.is_none());
            ps.into_iter().map(|p| p.id).collect()
        };
        assert_eq!(order_ids(&mut app), vec![welcome, p1, p2]);

        app.harness().relayout_at((1200, 800)).unwrap();

        click_row(&mut app, 0).unwrap();
        assert_eq!(n(&mut app).selected_for_test(), Some(welcome));

        let alt = Modifiers::from(ModifiersState::ALT);
        let handled = dispatch(&mut app, alt, &named_key(NamedKey::ArrowDown));
        assert!(handled, "Alt-↓ must be consumed by the focused tree");
        assert_eq!(
            order_ids(&mut app),
            vec![p1, welcome, p2],
            "Alt-↓ must move `welcome` down one slot among its siblings"
        );

        dispatch(&mut app, alt, &named_key(NamedKey::ArrowUp));
        assert_eq!(
            order_ids(&mut app),
            vec![welcome, p1, p2],
            "Alt-↑ must move `welcome` back up"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn page_tree_rows_disclose_rename_indent_and_open_context_menus() {
    eprintln!("phase disclosure_triangle_click_toggles_child_visibility");
    {
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        // Give `welcome` a child so it becomes a collapsible branch.
        n(&mut app).apply_event_for_test(OutEvent::CreateChild {
            parent: Some(welcome),
        });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        // Creating a child reveals it (its branch expands), so both rows show.
        assert!(n(&mut app).sidebar_row_center_for_test(0).is_some());
        assert!(
            n(&mut app).sidebar_row_center_for_test(1).is_some(),
            "the newly-created child is revealed under its parent"
        );

        // Click the disclosure triangle (leftmost glyph of row 0) to COLLAPSE.
        let row0 = n(&mut app).sidebar_row_center_for_test(0).unwrap();
        let glyph_x = n(&mut app).sidebar_disclosure_x_for_test(0).unwrap();
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(glyph_x, row0.y));
        dispatch(&mut app, no_mods, &mouse_press());
        // Re-settle so the re-flattened rows lay out.
        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(
            n(&mut app).sidebar_row_center_for_test(1).is_none(),
            "collapsing must hide the child row through the event path"
        );

        // Click again → the child row reappears.
        let row0 = n(&mut app).sidebar_row_center_for_test(0).unwrap();
        let glyph_x = n(&mut app).sidebar_disclosure_x_for_test(0).unwrap();
        dispatch(&mut app, no_mods, &cursor_moved(glyph_x, row0.y));
        dispatch(&mut app, no_mods, &mouse_press());
        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(
            n(&mut app).sidebar_row_center_for_test(1).is_some(),
            "expanding must reveal the child row again"
        );
    }

    eprintln!("phase double_click_row_opens_inline_rename");
    {
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();

        let c = n(&mut app)
            .sidebar_row_center_for_test(0)
            .expect("row 0 laid out");
        let no_mods = Modifiers::default();
        // Two presses at the same spot in quick succession → double-click.
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        assert!(
            !n(&mut app).sidebar_is_renaming_for_test(),
            "single click must not rename"
        );
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        assert!(
            n(&mut app).sidebar_is_renaming_for_test(),
            "double-click on a row must open the inline rename editor"
        );
    }

    eprintln!("phase inline_rename_is_plain_and_commits_on_click_outside");
    {
        // Bug: the rename field was a jarring boxed input that never deselected.
        // It must be plain-chrome (styled like the row) and commit on focus-out.
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        // Second root page so we can click a DIFFERENT row to blur + commit.
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        // Double-click row 0 to open the in-place rename field.
        let c = n(&mut app)
            .sidebar_row_center_for_test(0)
            .expect("row 0 laid out");
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press());
        assert!(
            n(&mut app).sidebar_is_renaming_for_test(),
            "double-click opens the rename field"
        );
        assert_eq!(
            n(&mut app).sidebar_rename_is_plain_for_test(),
            Some(true),
            "the rename field is plain-chrome (styled like the row, not a boxed input)"
        );

        // Type an edit into the field, then click a DIFFERENT row → focus-out
        // must COMMIT (not discard) and end editing.
        dispatch(&mut app, no_mods, &char_key("!"));
        let c1 = n(&mut app)
            .sidebar_row_center_for_test(1)
            .expect("row 1 laid out");
        dispatch(&mut app, no_mods, &cursor_moved(c1.x, c1.y));
        dispatch(&mut app, no_mods, &mouse_press());

        assert!(
            !n(&mut app).sidebar_is_renaming_for_test(),
            "clicking outside the field must end editing (it deselects/blurs)"
        );
        let pages = n(&mut app).workspace_pages_for_test();
        let renamed = pages.iter().find(|p| p.id == welcome).unwrap();
        assert_eq!(
            renamed.title, "Welcome!",
            "clicking outside must COMMIT the edit, not discard it"
        );
    }

    eprintln!("phase right_click_row_opens_context_menu_and_new_child_action_works");
    {
        // Bug: right-click on a page row did nothing. It must open the context
        // menu, and its actions must actually run.
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        app.harness().relayout_at((1200, 800)).unwrap();

        let c = n(&mut app)
            .sidebar_row_center_for_test(0)
            .expect("row 0 laid out");
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(c.x, c.y));
        dispatch(&mut app, no_mods, &mouse_press_right());
        assert!(
            n(&mut app).sidebar_menu_open_for_test(),
            "right-click on a page row must open the context menu"
        );

        // Drive the menu through its real key path: ArrowDown highlights the
        // first action ("New child"), Enter fires it.
        dispatch(&mut app, no_mods, &named_key(NamedKey::ArrowDown));
        dispatch(&mut app, no_mods, &named_key(NamedKey::Enter));

        let pages = n(&mut app).workspace_pages_for_test();
        assert_eq!(pages.len(), 2, "the New child action must create a page");
        let children_count = pages.iter().filter(|p| p.parent == Some(welcome)).count();
        assert_eq!(
            children_count, 1,
            "New child must nest the new page under the right-clicked page"
        );
        assert!(
            !n(&mut app).sidebar_menu_open_for_test(),
            "the menu closes after an action is chosen"
        );
    }

    eprintln!("phase keyboard_context_menu_opens_on_the_selected_page");
    {
        let mut app = starter();
        app.harness().relayout_at((1200, 800)).unwrap();
        let row = n(&mut app)
            .sidebar_row_center_for_test(0)
            .expect("row laid out");
        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &cursor_moved(row.x, row.y));
        dispatch(&mut app, no_mods, &mouse_press());

        dispatch(&mut app, no_mods, &named_key(NamedKey::ContextMenu));
        assert!(n(&mut app).sidebar_menu_open_for_test());
        dispatch(&mut app, no_mods, &named_key(NamedKey::Escape));

        let shift = Modifiers::from(ModifiersState::SHIFT);
        dispatch(&mut app, shift, &named_key(NamedKey::F10));
        assert!(
            n(&mut app).sidebar_menu_open_for_test(),
            "Shift+F10 opens the same selected-page menu"
        );
    }

    eprintln!("phase nested_child_renders_indented_under_its_parent");
    {
        // Bug: the sidebar rendered a flat list despite the model's hierarchy.
        // A child of the selected page must show one indent level below it.
        let mut app = starter();
        let welcome = n(&mut app).workspace_pages_for_test()[0].id;
        // Creating a child auto-selects it, so revealing it proves the tree
        // expands its ancestors (not a collapsed flat list).
        n(&mut app).apply_event_for_test(OutEvent::CreateChild {
            parent: Some(welcome),
        });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();

        assert_eq!(
            n(&mut app).sidebar_row_depth_for_test(0),
            Some(0),
            "parent sits at depth 0"
        );
        assert_eq!(
            n(&mut app).sidebar_row_has_children_for_test(0),
            Some(true),
            "the parent shows it has children (disclosure triangle)"
        );
        assert_eq!(
            n(&mut app).sidebar_row_depth_for_test(1),
            Some(1),
            "the child renders one level indented under its parent — a real tree"
        );
    }

    eprintln!("phase sidebar_tab_and_shift_tab_edit_the_page_hierarchy");
    {
        let mut app = starter();
        let first = n(&mut app).workspace_pages_for_test()[0].id;
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        let second = n(&mut app)
            .selected_for_test()
            .expect("new root is selected");
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();
        click_row(&mut app, 1).unwrap();

        assert!(dispatch(
            &mut app,
            Modifiers::default(),
            &named_key(NamedKey::Tab),
        ));
        assert_eq!(
            n(&mut app)
                .workspace_pages_for_test()
                .iter()
                .find(|page| page.id == second)
                .and_then(|page| page.parent),
            Some(first),
            "Tab nests the selected root beneath its previous sibling"
        );

        // The sidebar re-flattens synchronously after the canonical action so the
        // moved page remains visible and the inverse command is immediately usable.
        assert_eq!(n(&mut app).sidebar_row_depth_for_test(1), Some(1));
        assert!(dispatch(
            &mut app,
            Modifiers::from(ModifiersState::SHIFT),
            &named_key(NamedKey::Tab),
        ));
        assert_eq!(
            n(&mut app)
                .workspace_pages_for_test()
                .iter()
                .find(|page| page.id == second)
                .and_then(|page| page.parent),
            None,
            "Shift-Tab lifts the selected page one level"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn slash_commands_apply_block_kinds_and_continue_lists() {
    eprintln!("phase slash_menu_creates_numbered_list_that_continues_on_enter");
    {
        // Drive the real path: `/number` + Enter converts the focused block to
        // a numbered list; pressing Enter continues the list; the rendered
        // ordinals increment and the saved markdown numbers sequentially.
        let mut app = starter();
        // A fresh, empty page so the slash command has a clean paragraph.
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());

        let no_mods = Modifiers::default();
        dispatch(&mut app, no_mods, &char_key("/"));
        assert!(
            n(&mut app).editor_slash_menu_visible_for_test(),
            "typing `/` opens the block menu"
        );
        for c in ["n", "u", "m", "b", "e", "r"] {
            dispatch(&mut app, no_mods, &char_key(c));
        }
        // Enter commits the highlighted row (the sole match) → Numbered.
        dispatch(&mut app, no_mods, &named_key(NamedKey::Enter));
        assert!(
            !n(&mut app).editor_slash_menu_visible_for_test(),
            "committing the menu closes it"
        );
        {
            let blocks = n(&mut app).main_panel_mut().editor().blocks();
            assert_eq!(blocks.len(), 1);
            assert!(
                matches!(
                    blocks[0].kind,
                    ui::widget::BlockKind::Numbered { indent: 0 }
                ),
                "the `/number` command converts the block to a numbered list"
            );
            assert_eq!(
                blocks[0].text(),
                "",
                "the `/number` filter text is stripped"
            );
        }

        // Type the first item, then Enter to continue the list twice more.
        for c in ["o", "n", "e"] {
            dispatch(&mut app, no_mods, &char_key(c));
        }
        app.harness().relayout_at((1200, 800)).unwrap();
        dispatch(&mut app, no_mods, &named_key(NamedKey::Enter));
        for c in ["t", "w", "o"] {
            dispatch(&mut app, no_mods, &char_key(c));
        }
        app.harness().relayout_at((1200, 800)).unwrap();
        dispatch(&mut app, no_mods, &named_key(NamedKey::Enter));
        for c in ["s", "i", "x"] {
            dispatch(&mut app, no_mods, &char_key(c));
        }

        let editor = n(&mut app).main_panel_mut().editor();
        let blocks = editor.blocks();
        assert_eq!(blocks.len(), 3, "each Enter continues the numbered list");
        for b in blocks {
            assert!(
                matches!(b.kind, ui::widget::BlockKind::Numbered { indent: 0 }),
                "Enter on a numbered item yields another numbered item"
            );
        }
        assert_eq!(
            ui::widget::block_editor::numbered_ordinals(blocks),
            vec![1, 2, 3],
            "the rendered ordinals increment down the list"
        );

        let md = n(&mut app).main_panel_mut().take_body_snapshot();
        assert_eq!(md, "1. one\n\n2. two\n\n3. six\n");
    }

    eprintln!("phase slash_command_click_applies_kind_and_strips_the_trigger");
    {
        // Selecting a slash command with the MOUSE must delete the `/…` trigger
        // and apply the block kind — the old menu had no mouse handler, so a click
        // just dismissed it and left the literal `/` behind.
        let mut app = starter();
        n(&mut app).apply_event_for_test(OutEvent::CreateChild { parent: None });
        app.harness().relayout();
        app.harness().relayout();

        app.harness().relayout_at((1200, 800)).unwrap();
        assert!(n(&mut app).main_panel_mut().editor_mut().is_focused());

        let no_mods = Modifiers::default();
        // Open the menu and filter to the single "Heading 1" row.
        dispatch(&mut app, no_mods, &char_key("/"));
        assert!(
            n(&mut app).editor_slash_menu_visible_for_test(),
            "`/` opens the block menu"
        );
        for c in ["h", "1"] {
            dispatch(&mut app, no_mods, &char_key(c));
        }
        assert_eq!(
            n(&mut app)
                .main_panel_mut()
                .editor()
                .slash_menu()
                .visible_len(),
            1,
            "`/h1` narrows to exactly the Heading 1 row"
        );

        let (cx, cy) = {
            let rect = n(&mut app)
                .main_panel_mut()
                .editor()
                .slash_menu()
                .row_rect(0)
                .expect("the sole filtered command has a painted row");
            (rect.center().x, rect.center().y)
        };
        dispatch(&mut app, no_mods, &cursor_moved(cx, cy));
        dispatch(&mut app, no_mods, &mouse_press());

        assert!(
            !n(&mut app).editor_slash_menu_visible_for_test(),
            "clicking a row closes the menu"
        );
        let blocks = n(&mut app).main_panel_mut().editor().blocks();
        assert_eq!(blocks.len(), 1);
        assert!(
            matches!(blocks[0].kind, ui::widget::BlockKind::Heading(1)),
            "the clicked command converts the block to a Heading 1"
        );
        assert_eq!(
            blocks[0].text(),
            "",
            "the `/` trigger (and filter) is stripped — no literal slash left behind"
        );
    }
}

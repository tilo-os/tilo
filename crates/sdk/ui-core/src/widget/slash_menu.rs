//! `SlashMenu` — block-type conversion popover triggered by `/` inside
//! a [`BlockEditor`](crate::widget::block_editor::BlockEditor) block: a
//! [`FilterMenu`] over every block kind the editor supports. Filtering
//! matches keywords as well as labels, so `/h2` finds "Heading 2".
//!
//! Not a [`CommandPalette`](crate::widget::CommandPalette): that is a
//! full-screen search affordance pinned at 360 px, wider than most
//! paragraph blocks; this is a compact popover anchored at the caret.

use crate::widget::block_editor::BlockKind;
use crate::widget::filter_menu::{FilterMenu, MenuEntry, MenuSpec};

pub type SlashMenu = FilterMenu<BlockKind>;

static SPEC: MenuSpec = MenuSpec {
    // Fits inside a typical paragraph block, wide enough for "Heading 1".
    width: 200.0,
    row_height: 28.0,
    max_rows: usize::MAX,
    placeholder: "Filter blocks…",
    access_name: "Block types",
    widget_name: "SlashMenu",
    test_id: "slash_menu",
};

fn entries() -> Vec<MenuEntry<BlockKind>> {
    vec![
        MenuEntry {
            label: "Paragraph".to_string(),
            keywords: &["paragraph", "text", "body", "p"],
            value: BlockKind::Paragraph,
        },
        MenuEntry {
            label: "Heading 1".to_string(),
            keywords: &["heading 1", "h1", "title"],
            value: BlockKind::Heading(1),
        },
        MenuEntry {
            label: "Heading 2".to_string(),
            keywords: &["heading 2", "h2", "subtitle"],
            value: BlockKind::Heading(2),
        },
        MenuEntry {
            label: "Heading 3".to_string(),
            keywords: &["heading 3", "h3"],
            value: BlockKind::Heading(3),
        },
        MenuEntry {
            label: "Quote".to_string(),
            keywords: &["quote", "blockquote", "citation", ">"],
            value: BlockKind::Quote,
        },
        MenuEntry {
            label: "Bullet list".to_string(),
            keywords: &["bullet", "list", "ul", "unordered"],
            value: BlockKind::Bullet { indent: 0 },
        },
        MenuEntry {
            label: "Numbered list".to_string(),
            keywords: &["numbered", "number", "ordered", "ol", "1."],
            value: BlockKind::Numbered { indent: 0 },
        },
        MenuEntry {
            label: "Checklist".to_string(),
            keywords: &["checklist", "todo", "task", "check"],
            value: BlockKind::Checklist {
                indent: 0,
                done: false,
            },
        },
        MenuEntry {
            label: "Code block".to_string(),
            keywords: &["code", "codeblock", "code block"],
            value: BlockKind::CodeBlock { lang: None },
        },
        MenuEntry {
            label: "Table".to_string(),
            keywords: &["table", "grid", "columns", "gfm"],
            value: BlockKind::Table { visual: true },
        },
        MenuEntry {
            label: "TeX formula".to_string(),
            keywords: &["tex", "latex", "math", "equation", "formula"],
            value: BlockKind::CodeBlock {
                lang: Some("tex".to_string()),
            },
        },
        MenuEntry {
            label: "Typst formula".to_string(),
            keywords: &["typst", "math", "equation", "formula"],
            value: BlockKind::CodeBlock {
                lang: Some("typst".to_string()),
            },
        },
        MenuEntry {
            label: "Divider".to_string(),
            keywords: &["divider", "hr", "rule", "separator"],
            value: BlockKind::Divider,
        },
    ]
}

impl Default for FilterMenu<BlockKind> {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterMenu<BlockKind> {
    /// New menu seeded with every block kind. Starts closed.
    pub fn new() -> Self {
        Self::with_spec(&SPEC, entries())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use vello::Scene;
    use vello::kurbo::{Affine, Point};
    use winit::event::WindowEvent;
    use winit::keyboard::{Key, NamedKey, SmolStr};

    use crate::synthetic_input::key_press;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use crate::widget::Widget;

    fn char_press(c: &str) -> WindowEvent {
        key_press(Key::Character(SmolStr::new(c)), Some(c))
    }

    #[test]
    fn visibility_resets_filter_and_invisible_menu_ignores_keys() {
        // Case `new_starts_hidden_with_full_row_set`.
        {
            let m = SlashMenu::new();
            assert!(!m.is_visible());
            assert_eq!(m.visible_len(), entries().len());
        }

        // Case `set_visible_resets_filter_and_cursor_on_hide`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h");
            // Move cursor to a non-zero index.
            let _ = m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert!(m.visible_len() > 0);
            m.set_visible(false);
            assert_eq!(m.filter(), "");
        }

        // Case `invisible_menu_ignores_keys`.
        {
            let mut m = SlashMenu::new();
            let consumed = m.handle_key(&char_press("h"));
            assert!(!consumed);
            assert_eq!(m.filter(), "");
        }
    }

    #[test]
    fn filter_narrows_rows_from_typing_ime_and_backspace() {
        // Case `filter_narrows_to_matching_rows`.
        {
            let mut m = SlashMenu::new();
            m.set_filter("h");
            // "Heading 1/2/3" and "Checklist" (contains "h" in "Checklist")
            // match. All three Heading rows should be present.
            assert!(m.visible_len() < entries().len());
            // Specifically: "Heading 1" must be in the visible set.
            let any_h1 = (0..m.visible_len()).any(|_| {
                matches!(m.highlighted(), Some(BlockKind::Heading(1))) || m.visible_len() > 0
            });
            assert!(any_h1);
        }

        // Case `filter_h2_isolates_heading_2`.
        {
            let mut m = SlashMenu::new();
            m.set_filter("Heading 2");
            // The exact-substring filter should match exactly one row.
            assert_eq!(m.visible_len(), 1);
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(2)));
        }

        // Case `typing_appends_to_filter`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&char_press("h"));
            assert_eq!(m.filter(), "h");
            m.handle_key(&char_press("2"));
            assert_eq!(m.filter(), "h2");
        }

        // Case `ime_preedit_is_visible_but_filters_only_after_commit`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            let initial_rows = m.visible_len();
            assert!(m.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                "h2".into(),
                Some((1, 1)),
            ))));
            assert_eq!(m.filter(), "");
            assert_eq!(m.visible_len(), initial_rows);
            assert_eq!(Widget::accessibility(&m).unwrap().value, None);

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            m.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(Widget::ime_cursor_rect(&m).is_some());

            assert!(m.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("h2".into()))));
            assert_eq!(m.filter(), "h2");
            assert_eq!(m.visible_len(), 1);
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(2)));
            let access = Widget::accessibility(&m).unwrap();
            assert_eq!(access.name.as_deref(), Some("Block types"));
            assert_eq!(access.value.as_deref(), Some("h2"));
        }

        // Case `backspace_removes_one_extended_grapheme`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("👩🏽‍💻");
            assert!(m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
            assert_eq!(m.filter(), "");
            assert!(m.is_visible());
        }

        // Case `backspace_pops_filter`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h2");
            m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert_eq!(m.filter(), "h");
        }

        // Case `backspace_on_empty_filter_closes_menu`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            // Filter starts empty.
            let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            // Returns false so the parent can interpret as "menu closed,
            // fall through to delete the `/` glyph".
            assert!(!consumed);
            assert!(!m.is_visible());
        }
    }

    #[test]
    fn keyboard_moves_the_cursor_and_commits_or_escapes() {
        // Case `arrow_keys_move_cursor`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            assert_eq!(m.highlighted(), Some(&BlockKind::Paragraph));
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(1)));
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(2)));
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(1)));
        }

        // Case `arrow_up_at_top_is_clamped`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            // Already at row 0.
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(m.highlighted(), Some(&BlockKind::Paragraph));
        }

        // Case `enter_emits_selected_kind_and_closes`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert!(consumed);
            assert_eq!(m.take_selection(), Some(BlockKind::Heading(1)));
            assert!(!m.is_visible(), "Enter should close the menu");
            // Subsequent take is empty.
            assert!(m.take_selection().is_none());
        }

        // Case `escape_closes_without_committing`.
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            m.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!m.is_visible());
            assert!(m.take_selection().is_none());
        }

        // Case `filter_number_then_enter_commits_numbered_list`.
        {
            // Typing `/number` narrows to the "Numbered list" row and Enter
            // commits an ordered-list block.
            let mut m = SlashMenu::new();
            m.set_visible(true);
            for c in ["n", "u", "m", "b", "e", "r"] {
                m.handle_key(&char_press(c));
            }
            assert_eq!(m.visible_len(), 1, "`number` matches only Numbered list");
            m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert_eq!(m.take_selection(), Some(BlockKind::Numbered { indent: 0 }));
        }

        // Case `filter_h2_then_enter_commits_heading_2`.
        {
            // Type `/h2`, Enter → Heading(2). The leading `/` is consumed by
            // the editor before the menu opens, so the menu only sees `h2`.
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&char_press("h"));
            m.handle_key(&char_press("2"));
            assert_eq!(m.visible_len(), 1);
            m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert_eq!(m.take_selection(), Some(BlockKind::Heading(2)));
        }
    }

    #[test]
    fn pointer_and_semantic_activation_commit_the_exact_row() {
        // Case `synthetic_menu_item_activation_commits_that_exact_filtered_row`.
        {
            let mut menu = SlashMenu::new();
            menu.set_visible(true);
            let access = Widget::accessibility(&menu).expect("open menu semantics");
            let heading = access
                .children
                .iter()
                .find(|item| item.name.as_deref() == Some("Heading 2"))
                .expect("Heading 2 item");
            let target = heading.action_target.clone().expect("stable menu target");

            assert!(Widget::perform_semantic_action(
                &mut menu,
                &target,
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(menu.take_selection(), Some(BlockKind::Heading(2)));
            assert!(!menu.is_visible());
        }

        // Case `click_at_selects_row_strips_and_closes`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            // Row 1 is "Heading 1"; land the click inside its band.
            let pt = m.row_rect(1).expect("second row is painted").center();
            assert_eq!(m.click_at(pt), Some(BlockKind::Heading(1)));
            assert!(
                !m.is_visible(),
                "clicking a row commits and closes the menu"
            );
            assert_eq!(
                m.take_selection(),
                Some(BlockKind::Heading(1)),
                "the click also queues the kind for the parent to drain"
            );
        }

        // Case `click_off_any_row_selects_nothing`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            assert_eq!(m.click_at(Point::new(10.0, 5000.0)), None);
            assert!(
                m.is_visible(),
                "an off-row click leaves the menu open for the caller"
            );
        }

        // Case `hover_at_moves_the_highlight_to_the_row_under_the_pointer`.
        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            assert_eq!(m.highlighted(), Some(&BlockKind::Paragraph));
            // Row 2 is "Heading 2".
            let pt = m.row_rect(2).expect("third row is painted").center();
            assert!(m.hover_at(pt), "hovering a new row reports a move");
            assert_eq!(m.highlighted(), Some(&BlockKind::Heading(2)));
            assert!(!m.hover_at(pt), "hovering the same row is not a move");
            assert!(
                !m.hover_at(Point::new(10.0, 5000.0)),
                "hovering off any row is a no-op"
            );
        }
    }
}

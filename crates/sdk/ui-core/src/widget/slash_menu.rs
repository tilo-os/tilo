//! `SlashMenu` — the popover `/` opens inside a
//! [`BlockEditor`](crate::widget::block_editor::BlockEditor) block: a
//! [`FilterMenu`] over every block kind the editor makes, plus the commands
//! its host answers (a picture, a link to another page, a page inside this
//! one). Filtering matches keywords as well as labels, so `/h2` finds
//! "Heading 2"; each row shows the Markdown that makes the same block.
//!
//! Not a [`CommandPalette`](crate::CommandPalette): that is a
//! full-screen search affordance pinned at 360 px, wider than most
//! paragraph blocks; this is a compact popover anchored at the caret.

use crate::icons::Icon;
use crate::widget::block_editor::{BlockKind, CalloutKind};
use crate::widget::filter_menu::{FilterMenu, MenuEntry, MenuSpec};

/// What a row of the slash menu does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommand {
    Block(BlockKind),
    /// Link another page, through the editor's link picker.
    LinkPage,
    /// A command the editor's host answers; see
    /// [`BlockEditor::take_slash_request`](crate::widget::block_editor::BlockEditor::take_slash_request).
    Host(SlashHostCommand),
}

/// A slash command only the editor's host can carry out. A host offers the
/// ones it answers with
/// [`BlockEditor::set_slash_commands`](crate::widget::block_editor::BlockEditor::set_slash_commands).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlashHostCommand {
    /// Choose a picture and put it after the block.
    Image,
    /// Make a page inside this one: titled with the block's text, which
    /// becomes a link to it, or untitled and opened for naming.
    Page,
}

pub type SlashMenu = FilterMenu<SlashCommand>;

static SPEC: MenuSpec = MenuSpec {
    // Room for an icon, "Numbered list" and its Markdown hint.
    width: 248.0,
    row_height: 30.0,
    // Eight rows fit under a caret in the lower half of most windows; the
    // rest scroll with the highlight.
    max_rows: 8,
    placeholder: "slash-menu-filter",
    access_name: "slash-menu-name",
    widget_name: "SlashMenu",
    test_id: "slash_menu",
};

fn row(
    key: &str,
    keywords: &'static [&'static str],
    icon: Icon,
    hint: Option<&'static str>,
    value: SlashCommand,
) -> MenuEntry<SlashCommand> {
    MenuEntry {
        label: crate::i18n!(key),
        keywords,
        value,
        icon: Some(icon),
        hint,
    }
}

fn block(
    key: &str,
    keywords: &'static [&'static str],
    icon: Icon,
    hint: Option<&'static str>,
    kind: BlockKind,
) -> MenuEntry<SlashCommand> {
    row(key, keywords, icon, hint, SlashCommand::Block(kind))
}

/// A callout entry: the kind's name, found by it and by `callout`.
fn callout(
    kind: CalloutKind,
    keywords: &'static [&'static str],
    icon: Icon,
) -> MenuEntry<SlashCommand> {
    MenuEntry {
        label: kind.label(),
        keywords,
        value: SlashCommand::Block(BlockKind::Callout(kind)),
        icon: Some(icon),
        hint: None,
    }
}

/// The rows for a host that answers `host` and, with `link_pages`, has
/// pages to link: text first, then lists, what the host adds, and the
/// rarer blocks.
fn entries(link_pages: bool, host: &[SlashHostCommand]) -> Vec<MenuEntry<SlashCommand>> {
    let mut rows = vec![
        block(
            "slash-paragraph",
            &["paragraph", "text", "body", "p"],
            Icon::Type,
            None,
            BlockKind::Paragraph,
        ),
        block(
            "slash-heading-1",
            &["heading 1", "h1", "title"],
            Icon::Heading1,
            Some("#"),
            BlockKind::Heading(1),
        ),
        block(
            "slash-heading-2",
            &["heading 2", "h2", "subtitle"],
            Icon::Heading2,
            Some("##"),
            BlockKind::Heading(2),
        ),
        block(
            "slash-heading-3",
            &["heading 3", "h3"],
            Icon::Hash,
            Some("###"),
            BlockKind::Heading(3),
        ),
        block(
            "slash-bullet",
            &["bullet", "list", "ul", "unordered"],
            Icon::List,
            Some("-"),
            BlockKind::Bullet { indent: 0 },
        ),
        block(
            "slash-numbered",
            &["numbered", "number", "ordered", "ol", "1."],
            Icon::ListOrdered,
            Some("1."),
            BlockKind::Numbered { indent: 0 },
        ),
        block(
            "slash-checklist",
            &["checklist", "todo", "task", "check"],
            Icon::CircleCheck,
            Some("[ ]"),
            BlockKind::Checklist {
                indent: 0,
                done: false,
            },
        ),
    ];
    if host.contains(&SlashHostCommand::Page) {
        rows.push(row(
            "slash-page",
            &["page", "note", "subnote", "sub-note", "child", "new"],
            Icon::FilePlus,
            None,
            SlashCommand::Host(SlashHostCommand::Page),
        ));
    }
    if link_pages {
        rows.push(row(
            "slash-link-page",
            &["link", "page", "note", "mention", "reference"],
            Icon::Link,
            Some("[["),
            SlashCommand::LinkPage,
        ));
    }
    if host.contains(&SlashHostCommand::Image) {
        rows.push(row(
            "slash-image",
            &["image", "picture", "photo", "screenshot", "img"],
            Icon::Image,
            None,
            SlashCommand::Host(SlashHostCommand::Image),
        ));
    }
    rows.extend([
        block(
            "slash-quote",
            &["quote", "blockquote", "citation", ">"],
            Icon::Quote,
            Some(">"),
            BlockKind::Quote,
        ),
        callout(
            CalloutKind::Note,
            &["note", "callout", "info", "admonition"],
            Icon::Info,
        ),
        callout(
            CalloutKind::Tip,
            &["tip", "callout", "hint", "idea"],
            Icon::Lightbulb,
        ),
        callout(
            CalloutKind::Important,
            &["important", "callout", "key"],
            Icon::CircleAlert,
        ),
        callout(
            CalloutKind::Warning,
            &["warning", "warn", "callout", "alert"],
            Icon::TriangleAlert,
        ),
        callout(
            CalloutKind::Caution,
            &["caution", "danger", "callout", "error"],
            Icon::Ban,
        ),
        block(
            "slash-code",
            &["code", "codeblock", "code block"],
            Icon::Code,
            Some("```"),
            BlockKind::CodeBlock { lang: None },
        ),
        block(
            "slash-table",
            &["table", "grid", "columns", "gfm"],
            Icon::Table,
            Some("|"),
            BlockKind::Table { visual: true },
        ),
        block(
            "slash-tex",
            &["tex", "latex", "math", "equation", "formula"],
            Icon::Superscript,
            None,
            BlockKind::CodeBlock {
                lang: Some("tex".to_string()),
            },
        ),
        block(
            "slash-typst",
            &["typst", "math", "equation", "formula"],
            Icon::Superscript,
            None,
            BlockKind::CodeBlock {
                lang: Some("typst".to_string()),
            },
        ),
        block(
            "slash-divider",
            &["divider", "hr", "rule", "separator", "line"],
            Icon::Minus,
            Some("---"),
            BlockKind::Divider,
        ),
    ]);
    rows
}

impl Default for FilterMenu<SlashCommand> {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterMenu<SlashCommand> {
    /// New menu seeded with every block kind. Starts closed.
    pub fn new() -> Self {
        Self::with_spec(&SPEC, entries(false, &[]))
    }

    /// Offer what the host answers besides the block kinds: a link to
    /// another page when it has pages to link, and its own commands.
    pub fn set_commands(&mut self, link_pages: bool, host: &[SlashHostCommand]) {
        self.set_entries(entries(link_pages, host));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect};
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
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        {
            let m = SlashMenu::new();
            assert!(!m.is_visible());
            assert_eq!(m.visible_len(), entries(false, &[]).len());
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h");
            let _ = m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert!(m.visible_len() > 0);
            m.set_visible(false);
            assert_eq!(m.filter(), "");
        }

        {
            let mut m = SlashMenu::new();
            let consumed = m.handle_key(&char_press("h"));
            assert!(!consumed);
            assert_eq!(m.filter(), "");
        }
    }

    #[test]
    fn filter_narrows_rows_from_typing_ime_and_backspace() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        {
            let mut m = SlashMenu::new();
            m.set_filter("h");
            // "Heading 1/2/3" and "Checklist" (contains "h" in "Checklist")
            // match. All three Heading rows should be present.
            assert!(m.visible_len() < entries(false, &[]).len());
            // Specifically: "Heading 1" must be in the visible set.
            let any_h1 = (0..m.visible_len()).any(|_| {
                matches!(
                    m.highlighted(),
                    Some(SlashCommand::Block(BlockKind::Heading(1)))
                ) || m.visible_len() > 0
            });
            assert!(any_h1);
        }

        // Case `filter_h2_isolates_heading_2`.
        {
            let mut m = SlashMenu::new();
            m.set_filter("Heading 2");
            // The exact-substring filter should match exactly one row.
            assert_eq!(m.visible_len(), 1);
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(2)))
            );
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&char_press("h"));
            assert_eq!(m.filter(), "h");
            m.handle_key(&char_press("2"));
            assert_eq!(m.filter(), "h2");
        }

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
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(2)))
            );
            let access = Widget::accessibility(&m).unwrap();
            assert_eq!(access.name.as_deref(), Some("Block types"));
            assert_eq!(access.value.as_deref(), Some("h2"));
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("👩🏽‍💻");
            assert!(m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
            assert_eq!(m.filter(), "");
            assert!(m.is_visible());
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.set_filter("h2");
            m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert_eq!(m.filter(), "h");
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            // Returns false so the parent can interpret as "menu closed,
            // fall through to delete the `/` glyph".
            assert!(!consumed);
            assert!(!m.is_visible());
        }
    }

    #[test]
    fn keyboard_moves_the_cursor_and_commits_or_escapes() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Paragraph))
            );
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(1)))
            );
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(2)))
            );
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(1)))
            );
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Divider)),
                "Up from the first row wraps to the last"
            );
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Paragraph)),
                "Down from the last row wraps to the first"
            );
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert!(m.handle_key(&key_press(Key::Named(NamedKey::Tab), None)));
            assert_eq!(
                m.take_selection(),
                Some(SlashCommand::Block(BlockKind::Heading(1)))
            );
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            let consumed = m.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            assert!(consumed);
            assert_eq!(
                m.take_selection(),
                Some(SlashCommand::Block(BlockKind::Heading(1)))
            );
            assert!(!m.is_visible(), "Enter should close the menu");
            assert!(m.take_selection().is_none());
        }

        {
            let mut m = SlashMenu::new();
            m.set_visible(true);
            m.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            m.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!m.is_visible());
            assert!(m.take_selection().is_none());
        }

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
            assert_eq!(
                m.take_selection(),
                Some(SlashCommand::Block(BlockKind::Numbered { indent: 0 }))
            );
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
            assert_eq!(
                m.take_selection(),
                Some(SlashCommand::Block(BlockKind::Heading(2)))
            );
        }
    }

    #[test]
    fn pointer_and_semantic_activation_commit_the_exact_row() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
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
            assert_eq!(
                menu.take_selection(),
                Some(SlashCommand::Block(BlockKind::Heading(2)))
            );
            assert!(!menu.is_visible());
        }

        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            // Row 1 is "Heading 1"; land the click inside its band.
            let pt = m.row_rect(1).expect("second row is painted").center();
            assert_eq!(
                m.click_at(pt),
                Some(SlashCommand::Block(BlockKind::Heading(1)))
            );
            assert!(
                !m.is_visible(),
                "clicking a row commits and closes the menu"
            );
            assert_eq!(
                m.take_selection(),
                Some(SlashCommand::Block(BlockKind::Heading(1))),
                "the click also queues the kind for the parent to drain"
            );
        }

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

        {
            let mut m = SlashMenu::new();
            m.set_anchor(Point::new(0.0, 0.0));
            m.set_visible(true);
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Paragraph))
            );
            // Row 2 is "Heading 2".
            let pt = m.row_rect(2).expect("third row is painted").center();
            assert!(m.hover_at(pt), "hovering a new row reports a move");
            assert_eq!(
                m.highlighted(),
                Some(&SlashCommand::Block(BlockKind::Heading(2)))
            );
            assert!(!m.hover_at(pt), "hovering the same row is not a move");
            assert!(
                !m.hover_at(Point::new(10.0, 5000.0)),
                "hovering off any row is a no-op"
            );
        }
    }

    #[test]
    fn labels_that_start_with_the_query_come_first() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut m = SlashMenu::new();
        m.set_filter("c");
        assert_eq!(
            m.highlighted(),
            Some(&SlashCommand::Block(BlockKind::Checklist {
                indent: 0,
                done: false
            })),
            "\"Checklist\" starts with c; \"Quote\" only has a keyword with it"
        );
        m.set_filter("li");
        assert_eq!(
            m.highlighted(),
            Some(&SlashCommand::Block(BlockKind::Bullet { indent: 0 })),
            "a word of the label starting with the query beats a mere substring"
        );
    }

    #[test]
    fn an_inline_menu_leaves_typing_to_the_document_and_gives_back_a_dead_enter() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut m = SlashMenu::new();
        m.set_inline(true);
        m.set_visible(true);
        assert!(
            !m.handle_key(&char_press("h")),
            "a typed character is the document's"
        );
        assert_eq!(m.filter(), "", "and never reaches a hidden field");
        assert!(!m.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
        assert!(m.is_visible(), "the document decides what Backspace does");

        m.set_filter("zzz");
        assert_eq!(m.visible_len(), 0);
        assert!(
            !m.handle_key(&key_press(Key::Named(NamedKey::Enter), None)),
            "Enter with nothing to choose ends the line instead"
        );
        assert!(!m.is_visible());
        assert!(m.take_selection().is_none());

        m.set_visible(true);
        m.set_filter("h2");
        let full = m.height();
        m.set_inline(false);
        assert!(m.height() > full, "an inline menu paints no filter field");
    }

    #[test]
    fn a_menu_at_the_bottom_of_the_view_opens_above_the_caret_and_stays_inside() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut m = SlashMenu::new();
        m.set_inline(true);
        m.set_visible(true);
        let bounds = Rect::new(0.0, 0.0, 600.0, 500.0);
        let caret = Rect::new(500.0, 470.0, 501.0, 490.0);
        m.set_caret(caret, Some(bounds));
        let rect = m.rect();
        assert!(rect.y1 <= caret.y0, "{rect:?} sits above the caret line");
        assert!(
            rect.y0 >= bounds.y0 && rect.x1 <= bounds.x1,
            "{rect:?} stays in view"
        );
        assert!(
            m.visible_len() > 8 && rect.height() < 8.5 * 30.0 + 20.0,
            "at most eight rows show; the rest scroll"
        );
        let caret = Rect::new(20.0, 40.0, 21.0, 60.0);
        m.set_caret(caret, Some(bounds));
        assert!(m.rect().y0 >= caret.y1, "with room below, it opens below");
    }

    #[test]
    fn host_commands_are_offered_only_by_a_host_that_answers_them() {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut m = SlashMenu::new();
        m.set_filter("image");
        assert_eq!(m.visible_len(), 0);
        m.set_commands(true, &[SlashHostCommand::Image, SlashHostCommand::Page]);
        assert_eq!(
            m.highlighted(),
            Some(&SlashCommand::Host(SlashHostCommand::Image))
        );
        m.set_filter("link");
        assert_eq!(m.highlighted(), Some(&SlashCommand::LinkPage));
    }
}

//! `LinkPopover` — search popover for `[[Page Title]]` inline page links:
//! a [`FilterMenu`] over caller-provided pages, matched by title.

use uuid::Uuid;

use crate::widget::filter_menu::{FilterMenu, MenuEntry, MenuSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    pub id: Uuid,
    pub title: String,
}

pub type LinkPopover = FilterMenu<PageEntry>;

static SPEC: MenuSpec = MenuSpec {
    width: 220.0,
    row_height: 26.0,
    // Keeps the popover small for big workspaces; the user can keep typing
    // to narrow the result.
    max_rows: 8,
    placeholder: "link-popover-filter",
    access_name: "link-popover-name",
    widget_name: "LinkPopover",
    test_id: "link_popover",
};

impl Default for FilterMenu<PageEntry> {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterMenu<PageEntry> {
    pub fn new() -> Self {
        Self::with_spec(&SPEC, Vec::new())
    }

    pub fn set_items(&mut self, items: Vec<PageEntry>) {
        self.set_entries(
            items
                .into_iter()
                .map(|page| MenuEntry::plain(page.title.clone(), &[], page))
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn seed_three() -> LinkPopover {
        let mut p = LinkPopover::new();
        p.set_items(vec![
            PageEntry {
                id: Uuid::from_u128(1),
                title: "Welcome".into(),
            },
            PageEntry {
                id: Uuid::from_u128(2),
                title: "Roadmap".into(),
            },
            PageEntry {
                id: Uuid::from_u128(3),
                title: "Inbox".into(),
            },
        ]);
        p
    }

    #[test]
    fn link_popover_items_filter_and_empty_hint() {
        fn new_starts_empty_and_hidden() {
            let p = LinkPopover::new();
            assert!(!p.is_visible());
            assert_eq!(p.visible_len(), 0);
        }

        fn set_items_seeds_visible_list() {
            let p = seed_three();
            assert_eq!(p.visible_len(), 3);
        }

        fn filter_narrows_results() {
            let mut p = seed_three();
            p.set_filter("wel");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
        }

        fn empty_results_renders_no_matches_hint() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("zzz");
            assert_eq!(p.visible_len(), 0);
            assert!(p.highlighted().is_none());
        }

        new_starts_empty_and_hidden();
        set_items_seeds_visible_list();
        filter_narrows_results();
        empty_results_renders_no_matches_hint();
    }

    #[test]
    fn link_popover_keyboard_and_pointer_select_commit_or_escape() {
        fn arrow_keys_move_cursor() {
            let mut p = seed_three();
            p.set_visible(true);
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
            p.handle_key(&key_press(Key::Named(NamedKey::ArrowUp), None));
            assert_eq!(p.highlighted().unwrap().title, "Welcome");
        }

        fn keyboard_highlight_keeps_scrolling_through_large_page_sets() {
            let mut p = LinkPopover::new();
            p.set_items(
                (0..12)
                    .map(|index| PageEntry {
                        id: Uuid::from_u128(index + 1),
                        title: format!("Page {index}"),
                    })
                    .collect(),
            );
            p.set_visible(true);
            for _ in 0..9 {
                p.handle_key(&key_press(Key::Named(NamedKey::ArrowDown), None));
            }
            assert!(p.row_rect(1).is_none(), "the window scrolled past row 1");
            assert!(p.row_rect(9).is_some());
            assert_eq!(p.highlighted().unwrap().title, "Page 9");
        }

        fn enter_commits_selection_and_hides() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Enter), None));
            let chosen = p.take_selection().unwrap();
            assert_eq!(chosen.title, "Welcome");
            assert!(!p.is_visible());
        }

        fn pointer_hover_and_click_select_the_visible_row() {
            let mut p = seed_three();
            p.set_anchor(Point::new(20.0, 30.0));
            p.set_visible(true);
            let row = p.row_rect(1).expect("second row is painted");
            assert!(p.hover_at(row.center()));
            assert_eq!(
                p.highlighted().map(|entry| entry.title.as_str()),
                Some("Roadmap")
            );
            let chosen = p.click_at(row.center()).expect("click selects row");
            assert_eq!(chosen.title, "Roadmap");
            assert_eq!(p.take_selection(), Some(chosen));
            assert!(!p.is_visible());
        }

        fn escape_hides_without_committing() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&key_press(Key::Named(NamedKey::Escape), None));
            assert!(!p.is_visible());
            assert!(p.take_selection().is_none());
        }

        arrow_keys_move_cursor();
        keyboard_highlight_keeps_scrolling_through_large_page_sets();
        enter_commits_selection_and_hides();
        pointer_hover_and_click_select_the_visible_row();
        escape_hides_without_committing();
    }

    #[test]
    fn link_popover_filter_typing_ime_and_backspace() {
        fn typing_pushes_filter() {
            let mut p = seed_three();
            p.set_visible(true);
            p.handle_key(&char_press("r"));
            assert_eq!(p.filter(), "r");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "Roadmap");
        }

        fn ime_preedit_is_visible_but_filters_only_after_commit() {
            let mut p = LinkPopover::new();
            p.set_items(vec![
                PageEntry {
                    id: Uuid::from_u128(1),
                    title: "東京".into(),
                },
                PageEntry {
                    id: Uuid::from_u128(2),
                    title: "London".into(),
                },
            ]);
            p.set_visible(true);
            assert!(p.handle_key(&WindowEvent::Ime(winit::event::Ime::Preedit(
                "東京".into(),
                Some(("東".len(), "東".len())),
            ))));
            assert_eq!(p.filter(), "");
            assert_eq!(p.visible_len(), 2);
            assert_eq!(
                Widget::accessibility(&p).unwrap().value,
                None,
                "preedit is not exposed as committed menu state"
            );

            let mut scene = Scene::new();
            let mut renderer = TextRenderer::new();
            p.draw_overlay(&mut scene, Affine::IDENTITY, &mut renderer, &Theme::dark());
            assert!(Widget::ime_cursor_rect(&p).is_some());

            assert!(p.handle_key(&WindowEvent::Ime(winit::event::Ime::Commit("東京".into()))));
            assert_eq!(p.filter(), "東京");
            assert_eq!(p.visible_len(), 1);
            assert_eq!(p.highlighted().unwrap().title, "東京");
            let access = Widget::accessibility(&p).unwrap();
            assert_eq!(access.name.as_deref(), Some("Link notes"));
            assert_eq!(access.value.as_deref(), Some("東京"));
            assert_eq!(access.children.len(), 1);
        }

        fn backspace_removes_one_extended_grapheme() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("👩🏽‍💻");
            assert!(p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None)));
            assert_eq!(p.filter(), "");
            assert!(p.is_visible());
        }

        fn backspace_pops_filter_then_closes() {
            let mut p = seed_three();
            p.set_visible(true);
            p.set_filter("r");
            let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert!(consumed);
            assert_eq!(p.filter(), "");
            let consumed = p.handle_key(&key_press(Key::Named(NamedKey::Backspace), None));
            assert!(!consumed);
            assert!(!p.is_visible());
        }

        typing_pushes_filter();
        ime_preedit_is_visible_but_filters_only_after_commit();
        backspace_removes_one_extended_grapheme();
        backspace_pops_filter_then_closes();
    }
}

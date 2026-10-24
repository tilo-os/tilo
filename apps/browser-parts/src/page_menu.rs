//! Offscreen Chromium context menus displayed by browser chrome.
use std::cell::Cell;
use std::rc::Rc;

use ui::webview::{ContextMenuItem, SharedEngine, WebViewCommand};
use ui::widget::{ContextMenu, MenuItem};
use vello::kurbo::Rect;

/// Chromium's command for "Copy link address", which the Browser does
/// itself.
const COPY_LINK: u32 = 26502;

pub struct PageMenu {
    pub menu: ContextMenu,
    id: u32,
    link: String,
    items: Vec<ContextMenuItem>,
    /// The command of the row chosen in the menu or one of its submenus,
    /// set by the row's action.
    chosen: Rc<Cell<Option<u32>>>,
}
impl Default for PageMenu {
    fn default() -> Self {
        Self {
            menu: ContextMenu::new(),
            id: 0,
            link: String::new(),
            items: Vec::new(),
            chosen: Rc::new(Cell::new(None)),
        }
    }
}

/// A Chromium menu label as shown: `&` marks the key that chooses the row
/// and `&&` is an ampersand.
fn shown(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
        } else if chars.peek() == Some(&'&') {
            out.push('&');
            chars.next();
        }
    }
    out
}

impl PageMenu {
    pub fn open(
        &mut self,
        id: u32,
        x: f64,
        y: f64,
        link: String,
        items: Vec<ContextMenuItem>,
        surface: Rect,
        viewport: Rect,
    ) {
        self.id = id;
        self.link = link;
        self.chosen.set(None);
        self.menu = ContextMenu::with_items(self.rows(&items));
        self.items = items;
        let (px, py) = (surface.x0 + x, surface.y0 + y);
        self.menu
            .open_anchored(Rect::new(px, py, px + 1.0, py + 1.0), viewport);
    }

    /// The menu's rows: check marks on checkbox and radio items, a nested
    /// menu for each submenu, and greyed rows that cannot be chosen now.
    fn rows(&self, items: &[ContextMenuItem]) -> Vec<MenuItem> {
        items
            .iter()
            .map(|item| match item {
                ContextMenuItem::Separator => MenuItem::separator(),
                ContextMenuItem::Submenu {
                    label,
                    enabled,
                    items,
                } => MenuItem::submenu(shown(label), self.rows(items)).with_enabled(*enabled),
                ContextMenuItem::Command {
                    id,
                    label,
                    checked,
                    enabled,
                } => {
                    let chosen = self.chosen.clone();
                    let command = *id;
                    let row = MenuItem::action(shown(label), move || chosen.set(Some(command)))
                        .with_enabled(*enabled);
                    match checked {
                        Some(checked) => row.with_checked(*checked),
                        None => row,
                    }
                }
            })
            .collect()
    }

    pub fn dismiss(&mut self, id: u32) {
        if id == self.id {
            self.menu.close();
        }
    }
    pub fn cancel(&mut self, engine: &SharedEngine) {
        self.menu.close();
        engine.send(WebViewCommand::ContextMenuAction {
            id: self.id,
            command: 0,
        });
    }

    /// After the open menu took an event: send the row chosen, at any
    /// depth, or say the menu closed without a choice.
    pub fn settle(&mut self, engine: &SharedEngine) {
        // The rows report a choice themselves; the indices say nothing more.
        let _ = self.menu.take_selection();
        let _ = self.menu.take_submenu_selection();
        if let Some(command) = self.chosen.take() {
            self.choose(command, engine);
        } else if !self.menu.is_open() {
            self.cancel(engine);
        }
    }

    fn choose(&mut self, command: u32, engine: &SharedEngine) {
        if command == COPY_LINK {
            ui::text_selection::clipboard_copy(&self.link);
            self.cancel(engine);
        } else {
            engine.send(WebViewCommand::ContextMenuAction {
                id: self.id,
                command,
            });
            self.menu.close();
        }
    }

    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        self.menu.is_open().then(|| {
            AccessNode::new(Role::Menu)
                .with_name("Page context menu")
                .with_children(self.access_rows(&self.items))
        })
    }

    fn access_rows(&self, items: &[ContextMenuItem]) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        items
            .iter()
            .filter_map(|item| match item {
                ContextMenuItem::Separator => None,
                ContextMenuItem::Submenu {
                    label,
                    enabled,
                    items,
                } => Some(
                    AccessNode::new(Role::MenuItem)
                        .with_name(shown(label))
                        .with_state(AccessState::new().disabled(!enabled))
                        .with_children(if *enabled {
                            self.access_rows(items)
                        } else {
                            Vec::new()
                        }),
                ),
                ContextMenuItem::Command {
                    id,
                    label,
                    checked,
                    enabled,
                } => {
                    let mut state = AccessState::new().focusable(*enabled).disabled(!enabled);
                    if let Some(checked) = checked {
                        state = state.checked(*checked);
                    }
                    let node = AccessNode::new(Role::MenuItem)
                        .with_name(shown(label))
                        .with_state(state);
                    Some(if *enabled {
                        node.with_action_target(format!("browser.context:{}:{id}", self.id))
                    } else {
                        node
                    })
                }
            })
            .collect()
    }

    /// Choose the row `target` names (`browser.context:<menu>:<command>`)
    /// in the open menu, as assistive technology does.
    pub fn activate(&mut self, target: &str, engine: &SharedEngine) -> bool {
        let Some((id, command)) = target
            .strip_prefix("browser.context:")
            .and_then(|v| v.split_once(':'))
        else {
            return false;
        };
        let Ok(command) = command.parse::<u32>() else {
            return false;
        };
        if self.menu.is_open()
            && id.parse::<u32>().ok() == Some(self.id)
            && enabled_command(&self.items, command)
        {
            self.choose(command, engine);
            true
        } else {
            false
        }
    }
}

/// Whether `command` is a row that can be chosen now, at any depth.
fn enabled_command(items: &[ContextMenuItem], command: u32) -> bool {
    items.iter().any(|item| match item {
        ContextMenuItem::Command { id, enabled, .. } => *enabled && *id == command,
        ContextMenuItem::Submenu { enabled, items, .. } => {
            *enabled && enabled_command(items, command)
        }
        ContextMenuItem::Separator => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(id: u32, label: &str) -> ContextMenuItem {
        ContextMenuItem::Command {
            id,
            label: label.into(),
            checked: None,
            enabled: true,
        }
    }

    fn opened(items: Vec<ContextMenuItem>) -> PageMenu {
        let mut menu = PageMenu::default();
        menu.open(
            7,
            10.0,
            10.0,
            "https://target.example".into(),
            items,
            Rect::new(0.0, 80.0, 800.0, 600.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        menu
    }

    #[test]
    fn menu_actions_keep_native_target_and_reject_stale_accessibility_requests() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut menu = opened(vec![command(26501, "Open link in new tab")]);
        assert!(!menu.activate("browser.context:6:26501", &engine));
        assert!(!menu.activate("browser.context:7:26502", &engine));
        assert!(menu.activate("browser.context:7:26501", &engine));
        assert_eq!(
            capture.commands().last(),
            Some(&WebViewCommand::ContextMenuAction {
                id: 7,
                command: 26501
            })
        );
        assert!(!menu.menu.is_open());
    }

    #[test]
    fn checkbox_radio_and_submenu_rows_show_as_the_page_offers_them() {
        let menu = opened(vec![
            command(26501, "Save &As"),
            ContextMenuItem::Separator,
            ContextMenuItem::Command {
                id: 27000,
                label: "Remember the page".into(),
                checked: Some(true),
                enabled: true,
            },
            ContextMenuItem::Command {
                id: 27001,
                label: "Light".into(),
                checked: Some(false),
                enabled: false,
            },
            ContextMenuItem::Submenu {
                label: "Copy && share".into(),
                enabled: true,
                items: vec![command(27003, "As text")],
            },
        ]);
        let rows = menu.menu.items();
        assert_eq!(
            rows[0].label(),
            Some("Save As"),
            "a key marker is not shown"
        );
        assert_eq!(rows[0].checked(), None);
        assert!(matches!(rows[1], MenuItem::Separator));
        assert_eq!(rows[2].checked(), Some(true));
        assert_eq!(rows[3].checked(), Some(false));
        assert!(!rows[3].is_enabled(), "a disabled item is greyed");
        assert_eq!(rows[4].label(), Some("Copy & share"));
        let nested = rows[4].submenu_items().expect("a submenu");
        assert_eq!(nested[0].label(), Some("As text"));

        let tree = menu.accessibility().expect("open");
        let names: Vec<_> = tree.children.iter().map(|node| node.name.clone()).collect();
        assert_eq!(names.len(), 4, "separators are not announced: {names:?}");
        assert_eq!(tree.children[1].state.checked, Some(true));
        assert!(tree.children[2].state.disabled);
        assert_eq!(
            tree.children[3].children[0].action_target.as_deref(),
            Some("browser.context:7:27003")
        );
    }

    #[test]
    fn a_row_chosen_in_a_submenu_reaches_the_page_and_a_disabled_one_never_does() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut menu = opened(vec![
            ContextMenuItem::Submenu {
                label: "Translate".into(),
                enabled: true,
                items: vec![
                    command(27001, "To German"),
                    ContextMenuItem::Submenu {
                        label: "More".into(),
                        enabled: true,
                        items: vec![command(27003, "To Danish")],
                    },
                ],
            },
            ContextMenuItem::Command {
                id: 27004,
                label: "Off here".into(),
                checked: None,
                enabled: false,
            },
        ]);
        assert!(!menu.activate("browser.context:7:27004", &engine));
        // As a person picks in a nested menu: the row's action runs and the
        // whole menu closes.
        let MenuItem::Action { submenu, .. } = &menu.menu.items()[0] else {
            panic!("a submenu row");
        };
        let MenuItem::Action {
            submenu: deeper, ..
        } = &submenu.as_ref().unwrap()[1]
        else {
            panic!("a nested submenu row");
        };
        let MenuItem::Action { action, .. } = &deeper.as_ref().unwrap()[0] else {
            panic!("a command row");
        };
        action();
        menu.menu.close();
        menu.settle(&engine);
        assert_eq!(
            capture.commands(),
            vec![WebViewCommand::ContextMenuAction {
                id: 7,
                command: 27003
            }]
        );
        // Closed with nothing chosen: the page hears so.
        let mut menu = opened(vec![command(26501, "Open link in new tab")]);
        menu.menu.close();
        menu.settle(&engine);
        assert_eq!(
            capture.commands().last(),
            Some(&WebViewCommand::ContextMenuAction { id: 7, command: 0 })
        );
    }
}

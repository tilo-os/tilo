//! Offscreen Chromium context menus displayed by browser chrome.
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{ContextMenu, MenuItem};
use vello::kurbo::Rect;

pub(crate) struct PageMenu {
    pub menu: ContextMenu,
    id: u32,
    link: String,
    items: Vec<(u32, String)>,
}
impl Default for PageMenu {
    fn default() -> Self {
        Self {
            menu: ContextMenu::new(),
            id: 0,
            link: String::new(),
            items: Vec::new(),
        }
    }
}
impl PageMenu {
    pub fn open(
        &mut self,
        id: u32,
        x: f64,
        y: f64,
        link: String,
        items: Vec<(u32, String)>,
        surface: Rect,
        viewport: Rect,
    ) {
        self.id = id;
        self.link = link;
        self.items = items;
        self.menu = ContextMenu::new();
        for (_, label) in &self.items {
            self.menu
                .push(MenuItem::action(label.replace('&', ""), || {}));
        }
        let (px, py) = (surface.x0 + x, surface.y0 + y);
        self.menu
            .open_anchored(Rect::new(px, py, px + 1.0, py + 1.0), viewport);
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
    pub fn choose(&mut self, index: usize, engine: &SharedEngine) {
        if let Some((command, _)) = self.items.get(index) {
            if *command == 26502 {
                ui::text_selection::clipboard_copy(&self.link);
                self.cancel(engine);
            } else {
                engine.send(WebViewCommand::ContextMenuAction {
                    id: self.id,
                    command: *command,
                });
                self.menu.close();
            }
        }
    }
    pub fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        self.menu.is_open().then(|| {
            AccessNode::new(Role::Menu)
                .with_name("Page context menu")
                .with_children(
                    self.items
                        .iter()
                        .enumerate()
                        .map(|(index, (_, label))| {
                            AccessNode::new(Role::MenuItem)
                                .with_name(label.replace('&', ""))
                                .with_action_target(format!("browser.context:{}:{index}", self.id))
                                .with_state(AccessState::new().focusable(true))
                        })
                        .collect(),
                )
        })
    }
    pub fn activate(&mut self, target: &str, engine: &SharedEngine) -> bool {
        let Some((id, index)) = target
            .strip_prefix("browser.context:")
            .and_then(|v| v.split_once(':'))
        else {
            return false;
        };
        let Ok(index) = index.parse::<usize>() else {
            return false;
        };
        if self.menu.is_open()
            && id.parse::<u32>().ok() == Some(self.id)
            && index < self.items.len()
        {
            self.choose(index, engine);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_actions_keep_native_target_and_reject_stale_accessibility_requests() {
        let capture = ui::webview::test_engine::TestEngine::new();
        let engine: SharedEngine = capture.clone();
        let mut menu = PageMenu::default();
        menu.open(
            7,
            10.0,
            10.0,
            "https://target.example".into(),
            vec![(26501, "Open link in new tab".into())],
            Rect::new(0.0, 80.0, 800.0, 600.0),
            Rect::new(0.0, 0.0, 800.0, 600.0),
        );
        assert!(!menu.activate("browser.context:6:0", &engine));
        assert!(menu.activate("browser.context:7:0", &engine));
        assert_eq!(
            capture.commands().last(),
            Some(&WebViewCommand::ContextMenuAction {
                id: 7,
                command: 26501
            })
        );
        assert!(!menu.menu.is_open());
    }
}

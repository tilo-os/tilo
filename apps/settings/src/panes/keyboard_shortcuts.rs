//! Editable compositor shortcuts. The Settings CRDT is only the form model;
//! accepted bindings are validated and atomically persisted through the
//! shared typed contract the compositor consumes.

use crate::widgets::{caption, card, rows, setting_row_described};
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

const SEARCH: &str = "shortcuts.search";
const OVERVIEW: &str = "shortcuts.overview";
const LOCK: &str = "shortcuts.lock";
const QUICK_SETTINGS: &str = "shortcuts.quick_settings";

fn entries() -> [(ShortcutAction, &'static str, &'static str, &'static str); 4] {
    [
        (
            ShortcutAction::Search,
            SEARCH,
            "Open search",
            "Search apps, documents, settings and actions",
        ),
        (
            ShortcutAction::WorkspaceOverview,
            OVERVIEW,
            "Workspace overview",
            "Show the infinite workspace map",
        ),
        (
            ShortcutAction::Lock,
            LOCK,
            "Lock the screen",
            "Always handled before applications",
        ),
        (
            ShortcutAction::QuickSettings,
            QUICK_SETTINGS,
            "Quick Settings",
            "Open the shell control surface",
        ),
    ]
}

pub struct ShortcutPane {
    config: ShortcutConfig,
    last_text: HashMap<ShortcutAction, String>,
    needs_seed: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for ShortcutPane {
    fn default() -> Self {
        let config = ShortcutConfig::load();
        let last_text = config
            .bindings
            .iter()
            .map(|(a, c)| (*a, c.display()))
            .collect();
        Self {
            config,
            last_text,
            needs_seed: true,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }
}

impl ShortcutPane {
    fn composition(&self) -> Box<dyn Widget> {
        let mut system = Vec::new();
        for (action, key, label, description) in entries() {
            let placeholder = self
                .config
                .bindings
                .get(&action)
                .map(KeyChord::display)
                .unwrap_or_default();
            system.push(setting_row_described(
                label,
                description,
                Box::new(
                    TextInput::new(Vec::new())
                        .with_id(key)
                        .with_on_change_path(key)
                        .with_placeholder(&placeholder)
                        .with_font_size(13.0),
                ),
            ));
        }
        crate::widgets::cards(vec![
            card("System", rows(system)),
            card(
                "Safety and conflicts",
                rows(vec![caption(
                    "Type a chord such as Super+K. Printable keys require a modifier. Reserved emergency and virtual-terminal chords cannot be changed; duplicate chords are rejected without replacing the working binding.",
                )]),
            ),
            card(
                "Application shortcuts",
                rows(vec![caption(
                    "Applications own commands inside their windows. System bindings win only for the actions above, so apps remain free to expose their own shortcut editor.",
                )]),
            ),
        ])
    }

    fn read_text(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
        match state.get(root, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
            _ => None,
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(ShortcutPane::default())
}

impl Widget for ShortcutPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            for (action, key, _, _) in entries() {
                let value = self
                    .config
                    .bindings
                    .get(&action)
                    .map(KeyChord::display)
                    .unwrap_or_default();
                let _ = state.put(root, key, value.clone());
                self.last_text.insert(action, value);
            }
            self.needs_seed = false;
        }
        for (action, key, _, _) in entries() {
            let Some(value) = Self::read_text(state, root, key) else {
                continue;
            };
            if self.last_text.get(&action) == Some(&value) {
                continue;
            }
            let valid = KeyChord::parse(&value).and_then(|chord| {
                self.config
                    .set(action, chord)
                    .map_err(|_| "shortcut conflicts with another system action")
            });
            if valid.is_ok() && self.config.save().is_ok() {
                self.last_text.insert(action, value);
            } else if let Some(previous) = self.last_text.get(&action) {
                let _ = state.put(root, key, previous.clone());
            }
        }
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut c = self.composition();
        let child = c.build_layout(taffy, theme, node_map);
        self.composed = Some(c);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x + l.size.width),
            origin.y + f64::from(l.location.y + l.size.height),
        );
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text, theme, presence);
        }
    }
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus: &mut Option<NodeId>,
        node: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let was_handled = *handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event, scale, modifiers, state, path, handled, focus, cn, taffy,
            );
        }
        self.commit_context_pending |= !was_handled && *handled;
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }
    fn take_commit_context(&mut self) -> Option<ui::qos_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "keyboard-shortcuts",
            "Keyboard shortcuts",
            "Change keyboard shortcuts",
        )
    }
    fn name(&self) -> &'static str {
        "ShortcutPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seeds_real_default_bindings() {
        let mut pane = ShortcutPane::default();
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            ShortcutPane::read_text(&doc, &automerge::ROOT, SEARCH).as_deref(),
            Some("Super+k")
        );
    }
    #[test]
    fn conflicting_edit_is_rolled_back() {
        let _guard = crate::prefs::test_env::ConfigGuard::new("shortcut_conflict");
        let mut pane = ShortcutPane::default();
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        let _ = doc.put(&automerge::ROOT, LOCK, "Super+k");
        pane.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            ShortcutPane::read_text(&doc, &automerge::ROOT, LOCK).as_deref(),
            Some("Super+l")
        );
    }
}

//! Editable compositor shortcuts. The Settings CRDT is only the form model;
//! accepted bindings are validated and atomically persisted through the
//! shared typed contract the compositor consumes.

use crate::widgets::{card, rows, setting_row};
use automerge::{AutoCommit, ObjId, ReadDoc, Value, transaction::Transactable};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use ui::theme::Theme;
use ui::widget::{ShortcutRecorder, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

const SEARCH: &str = "shortcuts.search";
const OVERVIEW: &str = "shortcuts.overview";
const LOCK: &str = "shortcuts.lock";
const QUICK_SETTINGS: &str = "shortcuts.quick_settings";
const VIM_MODE: &str = "shortcuts.vim_mode";

fn entries() -> Vec<(ShortcutAction, &'static str, &'static str, &'static str)> {
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
        (
            ShortcutAction::ShortcutHelp,
            "shortcuts.help",
            "Shortcut guide",
            "Show the always-available desktop controls overlay",
        ),
        (
            ShortcutAction::FocusPreviousWindow,
            "shortcuts.focus_previous_window",
            "Focus previous window",
            "Move the viewport left along the current strip",
        ),
        (
            ShortcutAction::FocusNextWindow,
            "shortcuts.focus_next_window",
            "Focus next window",
            "Move the viewport right along the current strip",
        ),
        (
            ShortcutAction::PreviousTab,
            "shortcuts.previous_tab",
            "Previous tab",
            "Cycle tabs in the focused window group",
        ),
        (
            ShortcutAction::NextTab,
            "shortcuts.next_tab",
            "Next tab",
            "Cycle tabs in the focused window group",
        ),
        (
            ShortcutAction::PreviousStrip,
            "shortcuts.previous_strip",
            "Previous strip",
            "Move the view to the strip above",
        ),
        (
            ShortcutAction::NextStrip,
            "shortcuts.next_strip",
            "Next strip",
            "Move the view to the strip below",
        ),
        (
            ShortcutAction::PreviousWorkspace,
            "shortcuts.previous_workspace",
            "Previous workspace",
            "Move the view to the workspace above",
        ),
        (
            ShortcutAction::NextWorkspace,
            "shortcuts.next_workspace",
            "Next workspace",
            "Move the view to the workspace below",
        ),
        (
            ShortcutAction::MoveGroupLeft,
            "shortcuts.move_group_left",
            "Move group left",
            "Reorder the focused window group",
        ),
        (
            ShortcutAction::MoveGroupRight,
            "shortcuts.move_group_right",
            "Move group right",
            "Reorder the focused window group",
        ),
        (
            ShortcutAction::ResizeNarrower,
            "shortcuts.resize_narrower",
            "Make window narrower",
            "Reduce the focused group's width",
        ),
        (
            ShortcutAction::ResizeWider,
            "shortcuts.resize_wider",
            "Make window wider",
            "Increase the focused group's width",
        ),
    ]
    .to_vec()
}

fn is_desktop_navigation(action: ShortcutAction) -> bool {
    !matches!(
        action,
        ShortcutAction::Search
            | ShortcutAction::WorkspaceOverview
            | ShortcutAction::Lock
            | ShortcutAction::QuickSettings
            | ShortcutAction::ShortcutHelp
    )
}

pub struct ShortcutPane {
    config: ShortcutConfig,
    desktop_controls: settings_store::hardware::DesktopControlsConfig,
    last_vim_mode: bool,
    last_text: HashMap<ShortcutAction, String>,
    needs_seed: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for ShortcutPane {
    fn default() -> Self {
        let config = ShortcutConfig::load();
        let desktop_controls = settings_store::hardware::DesktopControlsConfig::load();
        let last_text = config
            .bindings
            .iter()
            .map(|(a, c)| (*a, c.display()))
            .collect();
        Self {
            config,
            last_vim_mode: desktop_controls.vim_mode,
            desktop_controls,
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
        let mut system: Vec<Box<dyn Widget>> = Vec::new();
        let mut desktop: Vec<Box<dyn Widget>> = vec![setting_row(
            "Vim navigation (H J K L)",
            Box::new(
                Toggle::new("settings.shortcuts.vim_mode", VIM_MODE).with_id("shortcuts.vim_mode"),
            ),
        )];
        let defaults = ShortcutConfig::default();
        for (action, key, label, _) in entries() {
            let value = self
                .config
                .bindings
                .get(&action)
                .map(KeyChord::display)
                .unwrap_or_default();
            let default = defaults
                .bindings
                .get(&action)
                .map(KeyChord::display)
                .unwrap_or_default();
            let row = setting_row(
                label,
                Box::new(ShortcutRecorder::new(key, key, &value, &default)),
            );
            if is_desktop_navigation(action) {
                desktop.push(row);
            } else {
                system.push(row);
            }
        }
        crate::widgets::cards(vec![
            card("System", rows(system)),
            card("Desktop navigation", rows(desktop)),
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
            let _ = state.put(root, VIM_MODE, self.desktop_controls.vim_mode);
            self.needs_seed = false;
        }
        if let Ok(Some((Value::Scalar(value), _))) = state.get(root, VIM_MODE)
            && let automerge::ScalarValue::Boolean(enabled) = value.as_ref()
            && *enabled != self.last_vim_mode
        {
            let previous = self.last_vim_mode;
            self.desktop_controls.vim_mode = *enabled;
            if self.desktop_controls.save().is_ok() {
                self.last_vim_mode = *enabled;
            } else {
                self.desktop_controls.vim_mode = previous;
                let _ = state.put(root, VIM_MODE, previous);
            }
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
                let _ = state.put(root, format!("{key}.error"), false);
            } else if let Some(previous) = self.last_text.get(&action) {
                let _ = state.put(root, key, previous.clone());
                let _ = state.put(root, format!("{key}.error"), true);
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

    #[test]
    fn vim_mode_toggle_persists_to_the_shell_contract() {
        let _guard = crate::prefs::test_env::ConfigGuard::new("shortcut_vim_mode");
        let mut pane = ShortcutPane::default();
        let mut doc = AutoCommit::new();
        pane.sync(&mut doc, &automerge::ROOT);
        let _ = doc.put(&automerge::ROOT, VIM_MODE, true);
        pane.sync(&mut doc, &automerge::ROOT);
        assert!(settings_store::hardware::DesktopControlsConfig::load().vim_mode);
    }
}

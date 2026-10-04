//! Editable compositor shortcuts. The rows are the desktop's keyboard
//! shortcut catalogue ([`settings_store::keyboard_shortcuts`]), the same one
//! the shell publishes on the action door and the shortcut guide shows. The
//! Settings CRDT is only the form model and never a replayable setting;
//! accepted bindings are validated and atomically persisted through the
//! shared typed contract the compositor consumes.

use crate::actions::{SETTINGS_ACTION_TYPE, SettingsAction, set_bool_action};
use crate::i18n::t;
use crate::widgets::{card, choice_row, rows, setting_row};
use automerge::{AutoCommit, transaction::Transactable};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use settings_store::keyboard_shortcuts::{ShortcutCategory, ShortcutEntry, catalogue};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use ui::doc::{read_bool, read_str_or_text};
use ui::input::{KeyLegend, KeyboardBinding, KeyboardModifier};
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{InputHint, Row, ShortcutRecorder, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

const VIM_MODE: &str = "shortcuts.vim_mode";

fn draft_key(key: &str) -> String {
    format!("{key}.edit")
}

/// The form-model key of a rebindable action (`shortcuts.quick_settings`).
fn durable_key(action: ShortcutAction) -> String {
    format!("shortcuts.{}", action.id().replace('-', "_"))
}

/// Every rebindable action, in catalogue order.
fn rebindable() -> impl Iterator<Item = ShortcutAction> {
    ShortcutAction::ALL
        .into_iter()
        .filter(|action| action.rebindable())
}

/// Every shortcut the page lists, with its name, in page order — what a pen
/// or pad button can be set to run.
pub(crate) fn shortcut_actions() -> Vec<(ShortcutAction, String)> {
    rebindable()
        .map(|action| (action, t(&action.label_key())))
        .collect()
}

/// The legends printed on the keys of the layout the keyboard is typing
/// with, which the page names keys and modifiers in.
fn active_keycaps() -> ui::Keycaps {
    let config = settings_store::hardware::RichInputConfig::from_section(
        &settings_store::load_section("input"),
    );
    config
        .sources()
        .into_iter()
        .nth(config.active_layout_index())
        .map_or(ui::Keycaps::US, |source| {
            ui::Keycaps::from_xkb(&source.layout, source.variant.as_deref().unwrap_or(""))
        })
}

/// The keycaps a stored chord is drawn with.
fn binding_of(chord: &KeyChord) -> KeyboardBinding {
    let modifiers = chord.modifiers.iter().filter_map(|modifier| {
        match modifier.to_ascii_lowercase().as_str() {
            "super" | "meta" => Some(KeyboardModifier::System),
            "ctrl" | "control" => Some(KeyboardModifier::Control),
            "alt" | "option" => Some(KeyboardModifier::Alt),
            "shift" => Some(KeyboardModifier::Shift),
            _ => None,
        }
    });
    KeyboardBinding::new(modifiers, KeyLegend::from_name(&chord.key))
}

/// The chords of a shortcut nobody can rebind, as keycaps in the same
/// column the editable ones use.
fn chords_keycaps(chords: &[KeyChord]) -> Box<dyn Widget> {
    if chords.is_empty() {
        return Box::new(Text::new(&t("settings-shortcut-unassigned")).dimmed());
    }
    let mut row = Row::new().no_wrap();
    for (index, chord) in chords.iter().enumerate() {
        if index > 0 {
            row = row.push(Box::new(Text::new(&t("settings-shortcut-or")).dimmed()));
        }
        row = row.push(Box::new(InputHint::keyboard(binding_of(chord))));
    }
    Box::new(row)
}

fn entry_row(entry: &ShortcutEntry) -> Box<dyn Widget> {
    let label = t(&entry.label_key);
    match entry.action.filter(|_| entry.rebindable) {
        Some(action) => {
            let key = durable_key(action);
            choice_row(
                &label,
                Box::new(ShortcutRecorder::new(
                    &key,
                    &draft_key(&key),
                    &entry
                        .chords
                        .first()
                        .map(KeyChord::display)
                        .unwrap_or_default(),
                    &entry
                        .defaults
                        .first()
                        .map(KeyChord::display)
                        .unwrap_or_default(),
                )),
            )
        }
        None => choice_row(&label, chords_keycaps(&entry.chords)),
    }
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
    actions: crate::resource::ActionSink,
}

impl Default for ShortcutPane {
    fn default() -> Self {
        ui::keycaps::set_active(active_keycaps());
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
            actions: crate::resource::ActionSink::default(),
        }
    }
}

impl ShortcutPane {
    fn composition(&self) -> Box<dyn Widget> {
        // The catalogue lists a few shortcuts twice, once as a rebindable
        // action and once as the desktop's fixed chord under the same name.
        // One row per name, the first (the editable one) wins.
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<_> = catalogue(&self.config)
            .into_iter()
            .filter(|entry| seen.insert(t(&entry.label_key)))
            .collect();
        let mut cards: Vec<Box<dyn Widget>> = ShortcutCategory::ALL
            .into_iter()
            .map(|category| {
                let mut body: Vec<Box<dyn Widget>> = Vec::new();
                if category == ShortcutCategory::Navigation {
                    body.push(setting_row(
                        &t("settings-shortcuts-vim"),
                        Box::new(
                            Toggle::new("settings.shortcuts.vim_mode", &draft_key(VIM_MODE))
                                .with_id("shortcuts.vim_mode"),
                        ),
                    ));
                }
                body.extend(
                    entries
                        .iter()
                        .filter(|entry| entry.category == category)
                        .map(entry_row),
                );
                card(&t(&category.label_key()), rows(body))
            })
            .collect();
        if !self.config.actions.is_empty() {
            let actions: Vec<Box<dyn Widget>> = self
                .config
                .actions
                .iter()
                .map(|bound| {
                    choice_row(
                        &bound.label,
                        chords_keycaps(std::slice::from_ref(&bound.chord)),
                    )
                })
                .collect();
            cards.push(card(&t("settings-shortcuts-actions"), rows(actions)));
        }
        crate::widgets::cards(cards)
    }

    fn dispatch(&mut self, state: &mut AutoCommit, action: &SettingsAction) {
        self.actions.dispatch_with_context(
            state,
            SETTINGS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.settings", "Settings", action),
        );
    }

    pub(crate) fn bind_dispatcher(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Settings>,
    ) {
        self.actions.attach(dispatcher);
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(ShortcutPane::default())
}

impl Widget for ShortcutPane {
    ui::provides!(dyn ui::undo::CommittedActions, dyn ui::undo::UndoPoints);

    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
        if self.needs_seed {
            for action in rebindable() {
                let key = durable_key(action);
                let value = self
                    .config
                    .bindings
                    .get(&action)
                    .map(KeyChord::display)
                    .unwrap_or_default();
                let _ = state.put(root, &key, value.clone());
                let _ = state.put(root, draft_key(&key), value.clone());
                self.last_text.insert(action, value);
            }
            let _ = state.put(root, VIM_MODE, self.desktop_controls.vim_mode);
            let _ = state.put(root, draft_key(VIM_MODE), self.desktop_controls.vim_mode);
            self.needs_seed = false;
        }
        let vim = read_bool(state, root, VIM_MODE).unwrap_or(self.last_vim_mode);
        let vim_draft = read_bool(state, root, draft_key(VIM_MODE)).unwrap_or(vim);
        if vim != self.last_vim_mode {
            let _ = state.put(root, draft_key(VIM_MODE), vim);
            let previous = self.last_vim_mode;
            self.desktop_controls.vim_mode = vim;
            if self.desktop_controls.save().is_ok() {
                self.last_vim_mode = vim;
            } else {
                self.desktop_controls.vim_mode = previous;
                self.dispatch(state, &set_bool_action(VIM_MODE, previous));
            }
        } else if vim_draft != vim {
            self.desktop_controls.vim_mode = vim_draft;
            if self.desktop_controls.save().is_ok() {
                self.last_vim_mode = vim_draft;
                self.dispatch(state, &set_bool_action(VIM_MODE, vim_draft));
            } else {
                self.desktop_controls.vim_mode = vim;
                let _ = state.put(root, draft_key(VIM_MODE), vim);
            }
        }
        for action in rebindable() {
            let key = durable_key(action);
            let key = key.as_str();
            let Some(value) = read_str_or_text(state, root, key) else {
                continue;
            };
            if self.last_text.get(&action) == Some(&value) {
                let draft =
                    read_str_or_text(state, root, draft_key(key)).unwrap_or_else(|| value.clone());
                if draft == value {
                    continue;
                }
                let valid = KeyChord::parse(&draft).and_then(|chord| {
                    self.config
                        .set(action, chord)
                        .map_err(|_| "shortcut conflicts with another system action")
                });
                if valid.is_ok() && self.config.save().is_ok() {
                    self.last_text.insert(action, draft.clone());
                    let _ = state.put(root, key, draft);
                    let _ = state.put(root, format!("{key}.error"), false);
                } else {
                    let _ = state.put(root, draft_key(key), value);
                    let _ = state.put(root, format!("{key}.error"), true);
                }
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
            } else if let Some(previous) = self.last_text.get(&action).cloned() {
                let _ = state.put(root, key, previous.clone());
                let _ = state.put(root, draft_key(key), previous.clone());
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
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let was_handled = *ctx.handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
        self.commit_context_pending |= !was_handled && *ctx.handled;
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
    fn name(&self) -> &'static str {
        "ShortcutPane"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl ui::undo::UndoPoints for ShortcutPane {
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| ui::undo::needs_commit(content.as_ref()));
        crate::panes::preference_commit_context(
            pending,
            "keyboard-shortcuts",
            "Keyboard shortcuts",
            "Change keyboard shortcuts",
        )
    }
}

impl ui::undo::CommittedActions for ShortcutPane {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH: &str = "shortcuts.search";
    const QUICK_SETTINGS: &str = "shortcuts.quick_settings";

    /// Default bindings, catalogue rows, conflict rollback and the vim-mode shell contract.
    #[test]
    fn shortcut_bindings_seed_explain_and_persist_safely() {
        // Case `seeds_real_default_bindings`.
        {
            let mut pane = ShortcutPane::default();
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                read_str_or_text(&doc, &automerge::ROOT, SEARCH).as_deref(),
                Some("Super+k")
            );
        }

        // Case `rows_are_the_shortcut_catalogue`.
        {
            let listed: Vec<String> = rebindable().map(durable_key).collect();
            assert!(listed.contains(&QUICK_SETTINGS.to_string()));
            assert!(
                !listed.contains(&"shortcuts.lock".to_string()),
                "the lock chord is shown, not offered for editing"
            );
            for entry in catalogue(&ShortcutConfig::default()) {
                assert_ne!(
                    t(&entry.label_key),
                    entry.label_key,
                    "{} has no name in the Settings catalogue",
                    entry.id
                );
            }
            for category in ShortcutCategory::ALL {
                assert_ne!(t(&category.label_key()), category.label_key());
            }
        }

        // Case `conflicting_edit_is_rolled_back`.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = ShortcutPane::default();
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            let _ = doc.put(&automerge::ROOT, QUICK_SETTINGS, "Super+k");
            pane.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                read_str_or_text(&doc, &automerge::ROOT, QUICK_SETTINGS).as_deref(),
                Some("Super+q")
            );
        }

        // Case `vim_mode_toggle_persists_to_the_shell_contract`.
        {
            let _guard = crate::prefs::test_env::ConfigGuard::new();
            let mut pane = ShortcutPane::default();
            let mut doc = AutoCommit::new();
            pane.sync(&mut doc, &automerge::ROOT);
            let _ = doc.put(&automerge::ROOT, VIM_MODE, true);
            pane.sync(&mut doc, &automerge::ROOT);
            assert!(settings_store::hardware::DesktopControlsConfig::load().vim_mode);
        }
    }
}

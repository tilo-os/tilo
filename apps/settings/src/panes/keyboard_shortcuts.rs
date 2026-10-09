//! Editable compositor shortcuts. The rows are the desktop's keyboard
//! shortcut catalogue ([`settings_store::keyboard_shortcuts`]), the same one
//! the shell publishes on the action door and the shortcut guide shows. A
//! chord is bound with a [`ShortcutEffect`], which validates it against the
//! catalogue and stores it in the person's `shortcuts` section the
//! compositor reads; a chord it refuses leaves the binding as it was and
//! says why.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig};
use settings_store::keyboard_shortcuts::{ShortcutCategory, ShortcutEntry, catalogue};
use taffy::{NodeId, TaffyTree};
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::theme::Theme;
use ui::tilo_resource;
use ui::widget::{AppModel, ModelPath, ShortcutRecorder, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::t;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{caption, card, rows, setting_row};

/// The shell's desktop navigation preferences, as the person's settings
/// hold them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopControls {
    #[serde(default)]
    pub vim_mode: bool,
}

impl ui::storage::SettingsSchema for DesktopControls {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("desktop_controls");
}

/// The section holding the person's shortcut bindings.
const SHORTCUTS_SECTION: &str = "shortcuts";

/// The ids of the rebindable shortcuts, as an effect takes them.
fn rebindable_ids() -> impl Iterator<Item = &'static str> {
    rebindable().map(ShortcutAction::id)
}

/// The keys that move around the desktop.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum ShortcutEffect {
    /// Bind a desktop shortcut to a chord.
    #[effect(idempotent)]
    BindShortcut {
        #[param("The shortcut", choices = rebindable_ids())]
        shortcut: String,
        #[param("The chord, such as `Super+K` or `Ctrl+Alt+T`")]
        chord: String,
    },
    /// Add Vim-style keys for moving around the desktop, or stop.
    #[effect(idempotent, label = "Set Vim-style desktop navigation")]
    SetVimMode { on: bool },
}

/// Answer `effect`: a chord the catalogue refuses changes nothing and the
/// caller is told why.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: ShortcutEffect,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let host = app.host();
    match effect {
        ShortcutEffect::BindShortcut { shortcut, chord } => {
            let action = ShortcutAction::from_id(&shortcut)
                .ok_or_else(|| format!("`{shortcut}` is not a shortcut that can be changed"))?;
            let chord = KeyChord::parse(&chord).map_err(|error| format!("`{chord}`: {error}"))?;
            let document = host.storage().settings_document();
            let mut config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
            config
                .set(action, chord)
                .map_err(|error| error.to_string())?;
            let section = config.section().map_err(|error| error.to_string())?;
            document
                .update_section(SHORTCUTS_SECTION, section)
                .map_err(|error| format!("the shortcut could not be saved: {error}"))?;
            Ok(Effected::done())
        }
        ShortcutEffect::SetVimMode { on } => {
            host.update(|controls: &mut DesktopControls| controls.vim_mode = on)
        }
    }
}

/// Every rebindable action, in catalogue order.
fn rebindable() -> impl Iterator<Item = ShortcutAction> {
    ShortcutAction::ALL.into_iter()
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
fn active_keycaps(document: &ui::storage::SettingsDocument) -> ui::Keycaps {
    let config =
        settings_store::hardware::RichInputConfig::from_section(&document.section("input"));
    config
        .sources()
        .into_iter()
        .nth(config.active_layout_index())
        .map_or(ui::Keycaps::US, |source| {
            ui::Keycaps::from_xkb(&source.layout, source.variant.as_deref().unwrap_or(""))
        })
}

/// `chord` as the active layout's keycaps spell it (`Strg+Umschalt+T` on a
/// German layout). The stored chord stays canonical.
fn chord_label(chord: &KeyChord) -> String {
    ui::keycaps::active().chord(chord.modifiers.iter().map(String::as_str), &chord.key)
}

fn chords_text(chords: &[KeyChord]) -> String {
    if chords.is_empty() {
        return t("settings-shortcut-unassigned");
    }
    chords
        .iter()
        .map(chord_label)
        .collect::<Vec<_>>()
        .join("  /  ")
}

pub struct ShortcutPane {
    cx: PaneCx,
    config: ShortcutConfig,
    /// When the shortcuts section last changed, to read it again.
    seen: ui::storage::SettingsRevision,
    controls: Followed<DesktopControls>,
    vim_mode: LocalState<bool>,
    /// The chord each rebindable shortcut's recorder shows.
    chords: HashMap<ShortcutAction, LocalState<String>>,
    /// The last chord refused, and why.
    refused: LocalState<Option<ui::framework::Refused>>,
    refusal_shown: Option<String>,
    structure_dirty: bool,
    composed: ui::widget::Composed,
}

impl ShortcutPane {
    /// The pane over `cx`'s shortcut bindings.
    pub fn new(cx: PaneCx) -> Self {
        let document = cx.storage().settings_document().clone();
        ui::keycaps::set_active(active_keycaps(&document));
        let config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
        let controls = cx.follow::<DesktopControls>();
        let chords = rebindable()
            .map(|action| (action, LocalState::new(String::new())))
            .collect();
        let pane = Self {
            vim_mode: LocalState::new(controls.get().vim_mode),
            refused: cx.refused(),
            cx,
            config,
            seen: document.revision(),
            controls,
            chords,
            refusal_shown: None,
            structure_dirty: false,
            composed: ui::widget::Composed::default(),
        };
        pane.show();
        pane
    }

    fn show(&self) {
        for (action, state) in &self.chords {
            state.set(
                self.config
                    .bindings
                    .get(action)
                    .map(KeyChord::display)
                    .unwrap_or_default(),
            );
        }
    }

    fn entry_row(&self, entry: &ShortcutEntry) -> Box<dyn Widget> {
        let label = t(&entry.label_key);
        let recorder = entry
            .action
            .filter(|_| entry.rebindable)
            .and_then(|action| Some((action, self.chords.get(&action)?)));
        match recorder {
            Some((action, chord)) => {
                let id = action.id();
                let recorder = ShortcutRecorder::new(
                    &format!("shortcuts.{}", id.replace('-', "_")),
                    chord,
                    &entry
                        .defaults
                        .first()
                        .map(KeyChord::display)
                        .unwrap_or_default(),
                );
                let recorder = match self.cx.binding(
                    effect(ShortcutEffect::BIND_SHORTCUT)
                        .map(move |chord: String| (id.to_owned(), chord)),
                ) {
                    Some(binding) => recorder.on_action(binding),
                    None => recorder,
                };
                setting_row(&label, Box::new(recorder))
            }
            None => crate::widgets::value_row(&label, &chords_text(&entry.chords)),
        }
    }

    fn composition(&self) -> Box<dyn Widget> {
        let entries = catalogue(&self.config);
        let mut cards: Vec<Box<dyn Widget>> = ShortcutCategory::ALL
            .into_iter()
            .map(|category| {
                let mut body: Vec<Box<dyn Widget>> = Vec::new();
                if category == ShortcutCategory::Navigation {
                    body.push(setting_row(
                        &t("settings-shortcuts-vim"),
                        Box::new(self.cx.bind(
                            Toggle::from_state(&self.vim_mode).with_id("shortcuts.vim_mode"),
                            effect(ShortcutEffect::SET_VIM_MODE),
                        )),
                    ));
                    if let Some(reason) = &self.refusal_shown {
                        body.push(caption(reason));
                    }
                }
                body.extend(
                    entries
                        .iter()
                        .filter(|entry| entry.category == category)
                        .map(|entry| self.entry_row(entry)),
                );
                card(&t(&category.label_key()), rows(body))
            })
            .collect();
        if !self.config.actions.is_empty() {
            let mut actions: Vec<Box<dyn Widget>> = self
                .config
                .actions
                .iter()
                .map(|bound| crate::widgets::value_row(&bound.label, &chord_label(&bound.chord)))
                .collect();
            actions.push(caption(&t("settings-shortcuts-actions-caption")));
            cards.push(card(&t("settings-shortcuts-actions"), rows(actions)));
        }
        crate::widgets::cards(cards)
    }
}

impl Widget for ShortcutPane {
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let document = self.cx.storage().settings_document();
        let revision = document.revision();
        if revision != self.seen {
            self.seen = revision;
            let config = ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION));
            if config != self.config {
                self.config = config;
                self.structure_dirty = true;
            }
        }
        // A refused chord is not kept: the recorder shows the stored one
        // again, and the pane says why it was refused.
        let refusal = self
            .refused
            .get()
            .filter(|refused| refused.action == ShortcutEffect::BIND_SHORTCUT.id)
            .map(|refused| refused.reason);
        if refusal != self.refusal_shown {
            self.refusal_shown = refusal;
            self.structure_dirty = true;
        }
        if self.structure_dirty {
            self.show();
        }
        if self.controls.refresh() {
            self.vim_mode.set(self.controls.get().vim_mode);
        }
        self.composed.sync(state, root);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut c = self.composition();
        let child = c.build_layout(taffy, theme, node_map);
        self.composed.replace(c);
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
        self.composed.apply_layout(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = self.composed.element_mut() {
            c.draw(scene, transform, text, theme, presence);
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.composed.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }
    fn name(&self) -> &'static str {
        "ShortcutPane"
    }
    fn get_rect(&self) -> Rect {
        self.composed.rect()
    }
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.composed.take_structure_dirty();
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    fn bind(app: &mut TestApp<SettingsShell>, shortcut: &str, chord: &str) -> bool {
        app.effect(&SettingsEffect::from(ShortcutEffect::BindShortcut {
            shortcut: shortcut.into(),
            chord: chord.into(),
        }))
        .is_ok()
    }

    /// The catalogue names every shortcut, and only the editable ones are
    /// offered for rebinding.
    #[test]
    fn rows_are_the_shortcut_catalogue() {
        let listed: Vec<&str> = rebindable_ids().collect();
        assert!(listed.contains(&"quick-settings"));
        assert!(
            listed.contains(&"lock"),
            "the system catalogue offers the lock chord for editing"
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

    /// A chord lands in the `shortcuts` section the compositor reads; a
    /// chord another shortcut holds is refused with the reason and changes
    /// nothing; Vim mode is the shell's desktop-controls preference.
    #[test]
    fn bindings_are_validated_and_stored() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let document = app.settings::<DesktopControls>().document().clone();
        let stored = |document: &ui::storage::SettingsDocument| {
            ShortcutConfig::from_section(document.section(SHORTCUTS_SECTION))
        };
        assert!(bind(&mut app, "quick-settings", "Super+Shift+Q"));
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::QuickSettings].display(),
            "Super+Shift+q"
        );
        assert!(
            !bind(&mut app, "quick-settings", "Super+Space"),
            "Super+Space opens search"
        );
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::QuickSettings].display(),
            "Super+Shift+q"
        );
        assert!(bind(&mut app, "lock", "Super+Shift+L"));
        assert_eq!(
            stored(&document).bindings[&ShortcutAction::Lock].display(),
            "Super+Shift+l"
        );

        app.effect(&SettingsEffect::from(ShortcutEffect::SetVimMode {
            on: true,
        }))
        .unwrap();
        assert!(app.settings::<DesktopControls>().get().vim_mode);
    }

    /// The pane shows the stored chords and follows a change made elsewhere.
    #[test]
    fn the_pane_follows_the_bindings() {
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = ShortcutPane::new(cx.clone());
        assert_eq!(
            pane.chords[&ShortcutAction::Search].get(),
            "Super+Space",
            "the default binding"
        );
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::Search, KeyChord::parse("Super+J").unwrap())
            .unwrap();
        cx.storage()
            .settings_document()
            .update_section(SHORTCUTS_SECTION, config.section().unwrap())
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        assert_eq!(pane.chords[&ShortcutAction::Search].get(), "Super+j");
    }
}

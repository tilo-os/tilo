//! The shortcut guide ([`tilo_sheets::shortcut_guide`]) as the shell hosts
//! it: the shell tells the app the shortcut table as it stands, takes the
//! next chord for the key the person chose to change, and checks and keeps
//! every change in the person's settings.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

use settings_store::hardware::{ShortcutAction, ShortcutConfig};
use tilo_sheets::shortcut_guide::{Faces, Guide, GuideEffect, GuideSeat, Modality, Pair, Shortcut};
use ui::input::{KeyLegend, KeyboardBinding, KeyboardModifier};

use super::sheet_apps::{SheetHost, mount_sheet};

impl Default for SheetHost<GuideSeat> {
    fn default() -> Self {
        Self::new(
            LayerId::ShortcutHelp,
            LayerId::ShortcutHelp.surface_kind(),
            tilo_sheets::shortcut_guide::definition,
        )
    }
}

/// `keys` (`["Super", "Shift", "T"]`) as one keyboard binding.
pub(super) fn binding_of(keys: &[String]) -> KeyboardBinding {
    let modifier = |key: &str| match key {
        "Super" => Some(KeyboardModifier::System),
        "Ctrl" => Some(KeyboardModifier::Control),
        "Alt" => Some(KeyboardModifier::Alt),
        "Shift" => Some(KeyboardModifier::Shift),
        _ => None,
    };
    let key = keys
        .iter()
        .rev()
        .find(|key| modifier(key).is_none())
        .map_or_else(
            || KeyLegend::Character("—".into()),
            |key| KeyLegend::from_name(key),
        );
    KeyboardBinding::new(keys.iter().filter_map(|key| modifier(key)), key)
}

/// The keys `action` has in `shortcuts`.
fn shortcut(shortcuts: &ShortcutConfig, action: ShortcutAction) -> Shortcut {
    let keys = shortcuts.bindings.get(&action).map_or_else(
        || binding_of(&[]),
        |chord| {
            let mut keys: Vec<String> = chord.modifiers.iter().cloned().collect();
            keys.push(chord.key.clone());
            binding_of(&keys)
        },
    );
    Shortcut {
        action: action.id().to_owned(),
        label: shortcut_action_label(action),
        keys,
    }
}

impl ShellOverlayHost {
    /// What the guide shows now: the input in use and the shortcut table as
    /// it stands.
    pub(super) fn shortcut_guide(&self) -> Guide {
        use crate::compositor::controller_nav::FaceRole;

        let shortcuts = self
            .nested_shortcuts
            .as_ref()
            .unwrap_or(&self.shortcut_config);
        let defaults = ShortcutConfig::default();
        let pairs = SHORTCUT_GUIDE_ROWS
            .iter()
            .map(|(label, previous, next)| Pair {
                label: t(label),
                previous: shortcut(shortcuts, *previous),
                next: shortcut(shortcuts, *next),
                changed: [previous, next]
                    .into_iter()
                    .any(|action| shortcuts.bindings.get(action) != defaults.bindings.get(action)),
            })
            .collect();
        let essentials = shortcut_reference_groups(shortcuts)
            .into_iter()
            .map(|rows| {
                rows.into_iter()
                    .map(|(label, chord)| {
                        let alternatives = shortcut_chord_keycaps(&chord)
                            .iter()
                            .map(|keys| binding_of(keys))
                            .collect();
                        (label, alternatives)
                    })
                    .collect()
            })
            .collect();
        let mut notes = vec![
            shortcuts::shortcut_strip_jump_note(shortcuts),
            shortcut_app_tab_note(),
        ];
        notes.extend(shortcut_action_summary(shortcuts));
        if self.desktop_controls.config().vim_mode {
            notes.push(t("guide-vim-mode"));
        }
        let family = self.interaction.controller;
        let face = |role| {
            crate::compositor::controller_nav::face_button(
                family.unwrap_or(ui::ControllerFamily::Generic),
                role,
            )
        };
        Guide {
            modality: match self.shortcut_help_modality() {
                ui::InputModality::Controller => Modality::Controller,
                ui::InputModality::Touch => Modality::Touch,
                ui::InputModality::Keyboard | ui::InputModality::Pointer => Modality::Keys,
            },
            controller: family,
            faces: Faces {
                confirm: face(FaceRole::Confirm),
                cancel: face(FaceRole::Cancel),
                third: face(FaceRole::Third),
                fourth: face(FaceRole::Fourth),
            },
            pairs,
            essentials,
            notes,
            listening: self.shortcut_rebind.map(|action| action.id().to_owned()),
            message: self.shortcut_rebind_message.clone(),
        }
    }

    /// Take the guide down: it is asked for no longer, and nothing waits
    /// for a chord.
    pub(super) fn close_shortcut_guide(&mut self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.shortcut_help = false;
            state.bump();
        }
        self.shortcut_rebind = None;
        self.shortcut_rebind_message = None;
    }

    /// Follow the guide's request to be up, carry out what it asked, and
    /// keep what it shows current; whether anything changed on screen.
    pub(super) fn pump_shortcut_guide(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        let shown = self.layers.shortcut_guide.update(doc, path);
        let asked = self.layers.shortcut_guide.seat().take_asked();
        let acted = !asked.is_empty();
        for request in asked {
            match request {
                GuideEffect::Listen { action } => {
                    if let Some(action) = ShortcutAction::from_id(&action) {
                        self.shortcut_rebind = Some(action);
                        self.shortcut_rebind_message = None;
                    }
                }
                GuideEffect::Reset { first, second } => {
                    if let (Some(first), Some(second)) = (
                        ShortcutAction::from_id(&first),
                        ShortcutAction::from_id(&second),
                    ) {
                        self.reset_shortcut_pair(first, second);
                    }
                }
                GuideEffect::Done => self.close_shortcut_guide(),
            }
        }
        self.follow_shortcut_guide();
        shown | acted
    }

    /// Put the guide up or take it down as it is asked for, and tell it
    /// what to show.
    pub(super) fn follow_shortcut_guide(&mut self) {
        let asked = self.overlay_flags().shortcut_help;
        let guide = &self.layers.shortcut_guide;
        if asked && !guide.requested() {
            let room = self.sheet_room();
            let apps = self.sheet_apps();
            self.layers
                .shortcut_guide
                .mount(&room, |definition, room, instance, seat| {
                    mount_sheet(apps.as_ref(), definition, room, instance, seat)
                });
            self.layers.shortcut_guide.open();
        } else if !asked && guide.is_drawn() {
            self.layers.shortcut_guide.close_now();
        }
        if asked {
            let guide = self.shortcut_guide();
            self.layers.shortcut_guide.seat().show(guide);
        }
    }

    /// While the guide is up every event is its own: a chord it waits for
    /// is the shell's to check, and Escape stops waiting.
    pub(super) fn route_shortcut_guide(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.overlay_flags().shortcut_help {
            return false;
        }
        if let (
            Some(action),
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            },
        ) = (self.shortcut_rebind, event)
        {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.shortcut_rebind = None;
                self.shortcut_rebind_message = None;
            } else {
                self.capture_shortcut_chord(action, logical_key, *ctx.modifiers);
            }
            self.follow_shortcut_guide();
            self.bump_chrome();
            *ctx.handled = true;
            return true;
        }
        // F1, and the help shortcut, close it again as they opened it.
        if let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    repeat,
                    ..
                },
            ..
        } = event
            && (matches!(logical_key, Key::Named(NamedKey::F1))
                || self.configured_shortcut_action(logical_key, *ctx.modifiers)
                    == Some(ShortcutAction::ShortcutHelp))
        {
            if !*repeat {
                self.close_shortcut_guide();
                self.follow_shortcut_guide();
            }
            *ctx.handled = true;
            return true;
        }
        if self.layers.shortcut_guide.is_drawn() {
            self.layers.shortcut_guide.route(event, ctx);
        }
        *ctx.handled = true;
        true
    }
}

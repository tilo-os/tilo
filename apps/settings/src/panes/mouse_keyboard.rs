#![allow(
    clippy::unreachable,
    reason = "static control builders accept only their matching closed InputOperation variant"
)]

//! Mouse & Keyboard pane — pointer behaviour and keyboard layout.
//!
//! Every control drives real, durable state:
//! - **Pointer speed / per-device natural scrolling / Tap to click / Key repeat** are
//!   persisted to the shared preferences store
//!   ([`crate::prefs::SystemPrefs`]); the compositor reads them when it
//!   configures libinput for the session.
//! - **Keyboard input sources** are per-user durable intent. Settings never
//!   invokes `localectl`; the compositor owns XKB and hot-reloads this file.
//
// CRDT keys this pane reads / writes:
//   `input.touchpad_pointer_speed` — f64 in `[0.0, 1.0]`
//   `input.mouse_pointer_speed`    — f64 in `[0.0, 1.0]`
//   `input.touchpad_natural_scroll` — bool
//   `input.mouse_natural_scroll`    — bool
//   `input.tap_to_click`     — bool
//   `input.key_repeat`       — f64 in `[0.0, 1.0]`
//   `input.keyboard_layout`  — active source display label

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::doc::{read_bool, read_str_or_text};
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Row, Slider, Spacer, Text, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use crate::input_sources::{CatalogEntry, installed_catalog};
use crate::prefs::{InputPrefs, SystemPrefs};
use crate::widgets::{caption, card, choice_row, rows, setting_row, setting_row_described};

fn bool_action(id: &str) -> tilo_actions::ActionBinding<bool> {
    match crate::actions::input_bool_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn number_action(id: &str) -> tilo_actions::ActionBinding<f64> {
    match crate::actions::input_number_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn text_action(id: &str) -> tilo_actions::ActionBinding<String> {
    match crate::actions::input_text_binding(id) {
        Some(binding) => binding,
        None => unreachable!("Mouse & Keyboard action must be published"),
    }
}

fn source_action_binding(source: &str, action: &str) -> tilo_actions::ActionBinding<()> {
    match crate::actions::keyboard_source_binding(source, action) {
        Some(binding) => binding,
        None => unreachable!("keyboard source action must be published"),
    }
}

pub const POINTER_SPEED_KEY: &str = "input.pointer_speed";
pub const TOUCHPAD_POINTER_SPEED_KEY: &str = "input.touchpad_pointer_speed";
pub const MOUSE_POINTER_SPEED_KEY: &str = "input.mouse_pointer_speed";
pub const NATURAL_SCROLL_KEY: &str = "input.natural_scroll";
pub const TOUCHPAD_NATURAL_SCROLL_KEY: &str = "input.touchpad_natural_scroll";
pub const MOUSE_NATURAL_SCROLL_KEY: &str = "input.mouse_natural_scroll";
pub const TAP_CLICK_KEY: &str = "input.tap_to_click";
pub const KEY_REPEAT_KEY: &str = "input.key_repeat";
pub const LAYOUT_KEY: &str = "input.keyboard_layout";
pub const TAP_DRAG_KEY: &str = "input.tap_drag";
pub const DWT_KEY: &str = "input.disable_while_typing";
pub const MIDDLE_EMULATION_KEY: &str = "input.middle_emulation";
pub const LEFT_HANDED_KEY: &str = "input.left_handed";
pub const ACCEL_PROFILE_KEY: &str = "input.accel_profile";
pub const GESTURES_KEY: &str = "input.touchpad_gestures";
pub const COMPOSE_KEY: &str = "input.compose_key";
pub const ADD_SOURCE_PICKER_KEY: &str = "input.source.add_picker";
pub const ADD_SOURCE_KEY: &str = "input.source.add";
pub const CHECK_SPELLING_KEY: &str = "text.check_spelling";
pub const AUTO_CORRECT_KEY: &str = "text.auto_correct";

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independently-toggled preference paired with its \
              initial value for dirty-tracking; no combination is invalid"
)]
#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
pub struct MouseKeyboardPane {
    initial: InputPrefs,
    last: InputPrefs,
    initial_check_spelling: bool,
    check_spelling: bool,
    initial_auto_correct: bool,
    auto_correct: bool,
    catalog: Vec<CatalogEntry>,
    pending_add_id: Option<String>,
    operation_status: ui::LocalState<crate::actions::LocalOperationStatus>,
    needs_seed: bool,
    structure_dirty: bool,
    commit_context_pending: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for MouseKeyboardPane {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
impl MouseKeyboardPane {
    #[must_use]
    pub fn with_operation_status(
        mut self,
        status: &ui::LocalState<crate::actions::LocalOperationStatus>,
    ) -> Self {
        self.operation_status = status.clone();
        self
    }

    fn operation_succeeded(
        &self,
        operation: &crate::actions::KeyboardSourceOperation,
        message: impl Into<String>,
    ) {
        let token = crate::actions::local_operation_token_for(
            crate::actions::KEYBOARD_SOURCE_OPERATION_TYPE,
            operation,
        );
        if matches!(
            self.operation_status.get(),
            crate::actions::LocalOperationStatus::Running { token: active } if active == token
        ) {
            self.operation_status
                .set(crate::actions::LocalOperationStatus::Succeeded {
                    token,
                    message: message.into(),
                });
        }
    }

    pub fn new() -> Self {
        let mut prefs = SystemPrefs::load().input;
        prefs.normalise_keyboard_sources();
        let text = settings_store::load_section(ui::text_assistance::SETTINGS_SECTION);
        let check_spelling = text
            .get(ui::text_assistance::CHECK_SPELLING_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let auto_correct = text
            .get(ui::text_assistance::AUTO_CORRECT_KEY)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        Self {
            initial: prefs.clone(),
            last: prefs,
            initial_check_spelling: check_spelling,
            check_spelling,
            initial_auto_correct: auto_correct,
            auto_correct,
            catalog: installed_catalog(),
            pending_add_id: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test constructor with a deterministic installed-source catalogue.
    #[cfg(test)]
    fn with_catalog(mut initial: InputPrefs, catalog: Vec<CatalogEntry>) -> Self {
        initial.normalise_keyboard_sources();
        Self {
            initial: initial.clone(),
            last: initial,
            initial_check_spelling: true,
            check_spelling: true,
            initial_auto_correct: true,
            auto_correct: true,
            catalog,
            pending_add_id: None,
            operation_status: ui::LocalState::new(crate::actions::LocalOperationStatus::Idle),
            needs_seed: true,
            structure_dirty: false,
            commit_context_pending: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    fn source_label(&self, id: &str) -> String {
        self.catalog
            .iter()
            .find(|entry| entry.id == id)
            .map_or_else(|| id.to_string(), |entry| entry.label.clone())
    }

    fn source_id(&self, label: &str) -> Option<String> {
        self.catalog
            .iter()
            .find(|entry| entry.label == label)
            .map(|entry| entry.id.clone())
            .or_else(|| {
                self.last
                    .keyboard_layouts
                    .iter()
                    .find(|id| id.as_str() == label)
                    .cloned()
            })
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        crate::widgets::cards(vec![
            self.keyboard_card(),
            self.spell_checker_card(),
            self.input_sources_card(),
            self.pointer_card(),
            self.pointer_options_card(),
            Self::pen_card(),
        ])
    }

    fn pointer_card(&self) -> Box<dyn Widget> {
        card(
            "Pointer",
            rows(vec![
                setting_row(
                    "Touchpad speed",
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            self.last
                                .touchpad_pointer_speed
                                .unwrap_or(self.last.pointer_speed),
                        ))
                        .on_action(number_action("touchpad-speed"))
                        .with_id("input.touchpad_pointer_speed"),
                    ),
                ),
                setting_row(
                    "Mouse speed",
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(
                            self.last
                                .mouse_pointer_speed
                                .unwrap_or(self.last.pointer_speed),
                        ))
                        .on_action(number_action("mouse-speed"))
                        .with_id("input.mouse_pointer_speed"),
                    ),
                ),
                setting_row_described(
                    "Touchpad natural scrolling",
                    "Content tracks finger movement",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(
                            self.last
                                .touchpad_natural_scroll
                                .unwrap_or(self.last.natural_scroll),
                        ))
                        .on_action(bool_action("touchpad-natural-scroll"))
                        .with_id("input.touchpad_natural_scroll"),
                    ),
                ),
                setting_row_described(
                    "Mouse natural scrolling",
                    "Content moves in the wheel direction",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(
                            self.last
                                .mouse_natural_scroll
                                .unwrap_or(self.last.natural_scroll),
                        ))
                        .on_action(bool_action("mouse-natural-scroll"))
                        .with_id("input.mouse_natural_scroll"),
                    ),
                ),
                setting_row_described(
                    "Tap to click",
                    "Tap the touchpad to register a click",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.tap_to_click))
                            .on_action(bool_action("tap-to-click"))
                            .with_id("input.tap_click"),
                    ),
                ),
                setting_row_described(
                    "Tap-and-drag",
                    "Hold after a tap, then move to drag",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.tap_drag))
                            .on_action(bool_action("tap-drag"))
                            .with_id("input.tap_drag"),
                    ),
                ),
                setting_row_described(
                    "Disable while typing",
                    "Prevents accidental palm input",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.disable_while_typing))
                            .on_action(bool_action("disable-while-typing"))
                            .with_id("input.disable_while_typing"),
                    ),
                ),
                setting_row_described(
                    "Touchpad gestures",
                    "Three-finger navigation; app gestures still receive other gestures",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.touchpad_gestures))
                            .on_action(bool_action("touchpad-gestures"))
                            .with_id("input.touchpad_gestures"),
                    ),
                ),
            ]),
        )
    }

    /// Less-common pointer choices still get a specific destination instead
    /// of an opaque "Advanced" bucket: people and assistive agents can tell
    /// what will be changed before opening the card.
    fn pointer_options_card(&self) -> Box<dyn Widget> {
        card(
            "Buttons & acceleration",
            rows(vec![
                setting_row_described(
                    "Left-handed",
                    "Swap primary and secondary buttons",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.left_handed))
                            .on_action(bool_action("left-handed"))
                            .with_id("input.left_handed"),
                    ),
                ),
                setting_row_described(
                    "Middle-button emulation",
                    "Press left and right together for middle-click",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.last.middle_emulation))
                            .on_action(bool_action("middle-emulation"))
                            .with_id("input.middle_emulation"),
                    ),
                ),
                choice_row(
                    "Acceleration",
                    Box::new(
                        Dropdown::from_state(
                            vec!["Adaptive".into(), "Flat".into()],
                            &ui::LocalState::new(if self.last.accel_profile == "flat" {
                                "Flat".into()
                            } else {
                                "Adaptive".into()
                            }),
                        )
                        .on_action(text_action("acceleration-profile"))
                        .with_id("input.accel_profile")
                        .with_placeholder(
                            if self.initial.accel_profile == "flat" {
                                "Flat"
                            } else {
                                "Adaptive"
                            },
                        ),
                    ),
                ),
            ]),
        )
    }

    fn keyboard_card(&self) -> Box<dyn Widget> {
        let active_sources: Vec<String> = self
            .last
            .keyboard_layouts
            .iter()
            .map(|id| self.source_label(id))
            .collect();
        let repeat_value = self.key_repeat_slider_value();
        let repeat_description = self.key_repeat_description();
        card(
            "Keyboard",
            rows(vec![
                choice_row(
                    "Active input source",
                    Box::new(
                        Dropdown::from_state(
                            active_sources,
                            &ui::LocalState::new(self.source_label(&self.last.keyboard_layout)),
                        )
                        .on_action(text_action("active-source"))
                        .with_id("input.layout")
                        .with_placeholder(&self.source_label(&self.last.keyboard_layout)),
                    ),
                ),
                setting_row_described(
                    "Key repeat speed",
                    &repeat_description,
                    Box::new(
                        Slider::from_state(&ui::LocalState::new(repeat_value))
                            .on_action(number_action("key-repeat"))
                            .with_id("input.key_repeat"),
                    ),
                ),
                caption(
                    "Changes apply to this user session through the compositor. With multiple sources, Alt+Shift switches temporarily; Super+Space remains global search.",
                ),
                choice_row(
                    "Compose key",
                    Box::new(
                        Dropdown::from_state(
                            vec![
                                "Disabled".into(),
                                "Right Alt".into(),
                                "Menu".into(),
                                "Caps Lock".into(),
                                "Right Super".into(),
                                "Left Super".into(),
                            ],
                            &ui::LocalState::new(match self.last.compose_key.as_deref() {
                                Some("ralt") => "Right Alt".into(),
                                Some("menu") => "Menu".into(),
                                Some("caps") => "Caps Lock".into(),
                                Some("rwin") => "Right Super".into(),
                                Some("lwin") => "Left Super".into(),
                                _ => "Disabled".into(),
                            }),
                        )
                        .on_action(text_action("compose-key"))
                        .with_id("input.compose_key")
                        .with_placeholder(
                            match self.initial.compose_key.as_deref() {
                                Some("ralt") => "Right Alt",
                                Some("menu") => "Menu",
                                Some("caps") => "Caps Lock",
                                Some("rwin") => "Right Super",
                                Some("lwin") => "Left Super",
                                _ => "Disabled",
                            },
                        ),
                    ),
                ),
            ]),
        )
    }

    /// The normal slider owns one linked speed/delay curve. Imported desktop
    /// settings retain their exact physical pair until changed, so render the
    /// imported rate rather than an unrelated legacy slider fraction.
    fn key_repeat_slider_value(&self) -> f64 {
        let Some((_, rate)) = self
            .last
            .key_repeat_delay_ms
            .zip(self.last.key_repeat_rate_hz)
        else {
            return self.last.key_repeat;
        };
        let min = settings_store::hardware::KEY_REPEAT_SLIDER_MIN_HZ;
        let max = settings_store::hardware::KEY_REPEAT_SLIDER_MAX_HZ;
        (f64::from(rate.saturating_sub(min)) / f64::from(max - min)).clamp(0.0, 1.0)
    }

    fn key_repeat_description(&self) -> String {
        match (self.last.key_repeat_delay_ms, self.last.key_repeat_rate_hz) {
            (Some(_), Some(0)) => "Repeat is off — move the slider to turn it on".into(),
            (Some(delay), Some(rate)) => {
                format!("{rate} keys per second · starts after {delay} ms")
            }
            _ => "Faster repeat starts sooner".into(),
        }
    }

    fn spell_checker_card(&self) -> Box<dyn Widget> {
        card(
            "Spell checker",
            rows(vec![
                setting_row_described(
                    "Check spelling while typing",
                    "Underline possible misspellings in natural-language fields",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.check_spelling))
                            .on_action(bool_action("spell-checking"))
                            .with_id("text.check_spelling"),
                    ),
                ),
                setting_row_described(
                    "Auto-correct obvious misspellings",
                    "Apply only high-confidence corrections after Space, punctuation, or Enter",
                    Box::new(
                        Toggle::from_state(&ui::LocalState::new(self.auto_correct))
                            .on_action(bool_action("auto-correct"))
                            .with_id("text.auto_correct"),
                    ),
                ),
                caption(
                    "Press Backspace immediately to restore the original word. Passwords are always excluded; other structured fields require an app to opt in.",
                ),
            ]),
        )
    }

    fn input_sources_card(&self) -> Box<dyn Widget> {
        let mut source_rows = Vec::new();
        for (index, id) in self.last.keyboard_layouts.iter().enumerate() {
            let mut row = Row::new()
                .push(Box::new(Text::new(&self.source_label(id)).with_size(13.0)))
                .push(Box::new(Spacer::new()));
            if id == &self.last.keyboard_layout {
                row = row.push(Box::new(Text::new("Active").with_size(12.0)));
            }
            let up = format!("input.source.{index}.up");
            let down = format!("input.source.{index}.down");
            let remove = format!("input.source.{index}.remove");
            let mut up_button = Button::new(&up, "↑", Some(&up))
                .with_size(36.0, 34.0)
                .with_accessible_name("Move input source up")
                .with_enabled(index > 0);
            if index > 0 {
                up_button = up_button.on_action(source_action_binding(id, "move-up"));
            }
            let mut down_button = Button::new(&down, "↓", Some(&down))
                .with_size(36.0, 34.0)
                .with_accessible_name("Move input source down")
                .with_enabled(index + 1 < self.last.keyboard_layouts.len());
            if index + 1 < self.last.keyboard_layouts.len() {
                down_button = down_button.on_action(source_action_binding(id, "move-down"));
            }
            let mut remove_button = Button::new(&remove, "Remove", Some(&remove))
                .with_size(84.0, 34.0)
                .with_enabled(self.last.keyboard_layouts.len() > 1);
            if self.last.keyboard_layouts.len() > 1 {
                remove_button = remove_button.on_action(source_action_binding(id, "remove"));
            }
            row = row
                .push(Box::new(up_button))
                .push(Box::new(down_button))
                .push(Box::new(remove_button));
            source_rows.push(Box::new(row) as Box<dyn Widget>);
        }
        let available: Vec<String> = self
            .catalog
            .iter()
            .filter(|entry| !self.last.keyboard_layouts.contains(&entry.id))
            .map(|entry| entry.label.clone())
            .collect();
        if self.last.keyboard_layouts.len() < settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
            && !available.is_empty()
        {
            source_rows.push(choice_row(
                "Add a language or layout",
                Box::new(
                    Dropdown::from_state(
                        available.clone(),
                        &ui::LocalState::new(available.first().cloned().unwrap_or_default()),
                    )
                    .on_action(text_action("add-source"))
                    .with_id("input.source.add_picker")
                    .with_placeholder("Choose a source"),
                ),
            ));
        } else {
            source_rows.push(caption(
                "Up to four input sources can be active at once, matching the interoperable XKB group limit.",
            ));
        }
        card("Input sources", rows(source_rows))
    }

    fn pen_card() -> Box<dyn Widget> {
        card(
            "Pen & tablet",
            rows(vec![caption(
                "Absolute pen and tablet mapping follows the selected display connector. Controls appear only when a tablet is attached; pressure curves and button mappings are stored per device.",
            )]),
        )
    }

    /// Load prefs fresh, apply `f` to the input section, persist.
    fn persist_input(f: impl FnOnce(&mut InputPrefs)) {
        let mut p = SystemPrefs::load();
        f(&mut p.input);
        p.input.normalise_keyboard_sources();
        let section = serde_json::to_value(&p.input)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        if let Err(e) = settings_store::update_section("input", section) {
            warn!(target: "tilo::settings", op = "input_prefs_save", error = %e, "failed to persist input prefs");
        }
    }

    fn persist_text_preference(key: &str, value: bool) {
        let mut section = serde_json::Map::new();
        section.insert(key.to_string(), serde_json::Value::Bool(value));
        if let Err(e) =
            settings_store::update_section(ui::text_assistance::SETTINGS_SECTION, section)
        {
            warn!(target: "tilo::settings", op = "text_assistance_save", error = %e, "failed to persist text-assistance prefs");
        }
    }

    fn persist_keyboard_sources(&self) {
        let active = self.last.keyboard_layout.clone();
        let sources = self.last.keyboard_layouts.clone();
        Self::persist_input(|input| {
            input.keyboard_layout = active;
            input.keyboard_layouts = sources;
        });
    }

    fn source_action(index: usize, action: &str) -> String {
        format!("input.source.{index}.{action}")
    }

    fn consume_action(state: &mut AutoCommit, root: &ObjId, key: &str) -> bool {
        if read_bool(state, root, key) != Some(true) {
            return false;
        }
        let _ = state.put(root, key, automerge::ScalarValue::Boolean(false));
        true
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(MouseKeyboardPane::new())
}

#[allow(
    dead_code,
    reason = "legacy latch helpers remain documented while local effects own input state"
)]
impl MouseKeyboardPane {
    fn seed_if_needed(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            // Hardware preferences are deliberately never seeded into the
            // collaborative document. Their durable local source is the
            // input section of settings.json and all mutations arrive as
            // typed device-local effects.
            let _ = state.put(root, CHECK_SPELLING_KEY, self.initial_check_spelling);
            let _ = state.put(root, AUTO_CORRECT_KEY, self.initial_auto_correct);
            self.needs_seed = false;
        }
        /* Legacy CRDT seeding removed: input hardware preferences are local effects.
            let _ = state.put(
                root,
                TOUCHPAD_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(
                    self.initial
                        .touchpad_pointer_speed
                        .unwrap_or(self.initial.pointer_speed),
                ),
            );
            let _ = state.put(
                root,
                MOUSE_POINTER_SPEED_KEY,
                automerge::ScalarValue::F64(
                    self.initial
                        .mouse_pointer_speed
                        .unwrap_or(self.initial.pointer_speed),
                ),
            );
            let _ = state.put(root, TAP_DRAG_KEY, self.initial.tap_drag);
            let _ = state.put(root, DWT_KEY, self.initial.disable_while_typing);
            let _ = state.put(root, MIDDLE_EMULATION_KEY, self.initial.middle_emulation);
            let _ = state.put(root, LEFT_HANDED_KEY, self.initial.left_handed);
            let _ = state.put(root, GESTURES_KEY, self.initial.touchpad_gestures);
            let _ = state.put(
                root,
                ACCEL_PROFILE_KEY,
                if self.initial.accel_profile == "flat" {
                    "Flat"
                } else {
                    "Adaptive"
                },
            );
            let compose = match self.initial.compose_key.as_deref() {
                Some("ralt") => "Right Alt",
                Some("menu") => "Menu",
                Some("caps") => "Caps Lock",
                _ => "Disabled",
            };
            let _ = state.put(root, COMPOSE_KEY, compose);
            let _ = state.put(
                root,
                TOUCHPAD_NATURAL_SCROLL_KEY,
                automerge::ScalarValue::Boolean(
                    self.initial
                        .touchpad_natural_scroll
                        .unwrap_or(self.initial.natural_scroll),
                ),
            );
            let _ = state.put(
                root,
                MOUSE_NATURAL_SCROLL_KEY,
                automerge::ScalarValue::Boolean(
                    self.initial
                        .mouse_natural_scroll
                        .unwrap_or(self.initial.natural_scroll),
                ),
            );
            let _ = state.put(
                root,
                TAP_CLICK_KEY,
                automerge::ScalarValue::Boolean(self.initial.tap_to_click),
            );
            let _ = state.put(
                root,
                KEY_REPEAT_KEY,
                automerge::ScalarValue::F64(self.initial.key_repeat),
            );
            let _ = state.put(
                root,
                LAYOUT_KEY,
                self.source_label(&self.initial.keyboard_layout),
            );
            let first_available = self
                .catalog
                .iter()
                .find(|entry| !self.initial.keyboard_layouts.contains(&entry.id))
                .cloned();
            if let Some(entry) = first_available {
                let _ = state.put(root, ADD_SOURCE_PICKER_KEY, entry.label);
                self.pending_add_id = Some(entry.id);
            }
            let _ = state.put(root, ADD_SOURCE_KEY, false);
            let _ = state.put(root, CHECK_SPELLING_KEY, self.initial_check_spelling);
            let _ = state.put(root, AUTO_CORRECT_KEY, self.initial_auto_correct);
            self.last = self.initial.clone();
            self.needs_seed = false;
        */
    }

    fn sync_simple_fields(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(value) = read_bool(state, root, CHECK_SPELLING_KEY)
            && value != self.check_spelling
        {
            self.check_spelling = value;
            Self::persist_text_preference(ui::text_assistance::CHECK_SPELLING_KEY, value);
        }
        if let Some(value) = read_bool(state, root, AUTO_CORRECT_KEY)
            && value != self.auto_correct
        {
            self.auto_correct = value;
            Self::persist_text_preference(ui::text_assistance::AUTO_CORRECT_KEY, value);
        }
        /* Legacy CRDT readers removed: a remote document never applies input hardware state.
            if let Some(v) = read_f64(state, root, TOUCHPAD_POINTER_SPEED_KEY)
                && self
                    .last
                    .touchpad_pointer_speed
                    .is_none_or(|last| (v - last).abs() > 0.001)
            {
                self.last.touchpad_pointer_speed = Some(v);
                Self::persist_input(|i| i.touchpad_pointer_speed = Some(v));
            }
            if let Some(v) = read_f64(state, root, MOUSE_POINTER_SPEED_KEY)
                && self
                    .last
                    .mouse_pointer_speed
                    .is_none_or(|last| (v - last).abs() > 0.001)
            {
                self.last.mouse_pointer_speed = Some(v);
                Self::persist_input(|i| i.mouse_pointer_speed = Some(v));
            }
            if let Some(v) = read_f64(state, root, KEY_REPEAT_KEY)
                && (v - self.last.key_repeat).abs() > 0.001
            {
                self.last.key_repeat = v;
                Self::persist_input(|i| {
                    i.key_repeat = v;
                    i.key_repeat_delay_ms = None;
                    i.key_repeat_rate_hz = None;
                });
            }
            if let Some(b) = read_bool(state, root, TOUCHPAD_NATURAL_SCROLL_KEY)
                && Some(b) != self.last.touchpad_natural_scroll
            {
                self.last.touchpad_natural_scroll = Some(b);
                Self::persist_input(|i| i.touchpad_natural_scroll = Some(b));
            }
            if let Some(b) = read_bool(state, root, MOUSE_NATURAL_SCROLL_KEY)
                && Some(b) != self.last.mouse_natural_scroll
            {
                self.last.mouse_natural_scroll = Some(b);
                Self::persist_input(|i| i.mouse_natural_scroll = Some(b));
            }
            if let Some(b) = read_bool(state, root, TAP_CLICK_KEY)
                && b != self.last.tap_to_click
            {
                self.last.tap_to_click = b;
                Self::persist_input(|i| i.tap_to_click = b);
            }
            for (key, current, apply) in [
                (TAP_DRAG_KEY, self.last.tap_drag, 0u8),
                (DWT_KEY, self.last.disable_while_typing, 1),
                (MIDDLE_EMULATION_KEY, self.last.middle_emulation, 2),
                (LEFT_HANDED_KEY, self.last.left_handed, 3),
                (GESTURES_KEY, self.last.touchpad_gestures, 4),
            ] {
                if let Some(value) = read_bool(state, root, key)
                    && value != current
                {
                    match apply {
                        0 => {
                            self.last.tap_drag = value;
                            Self::persist_input(|i| i.tap_drag = value);
                        }
                        1 => {
                            self.last.disable_while_typing = value;
                            Self::persist_input(|i| i.disable_while_typing = value);
                        }
                        2 => {
                            self.last.middle_emulation = value;
                            Self::persist_input(|i| i.middle_emulation = value);
                        }
                        3 => {
                            self.last.left_handed = value;
                            Self::persist_input(|i| i.left_handed = value);
                        }
                        _ => {
                            self.last.touchpad_gestures = value;
                            Self::persist_input(|i| i.touchpad_gestures = value);
                        }
                    }
                }
            }
        */
    }

    fn sync_choice_fields(&mut self, state: &AutoCommit, root: &ObjId) {
        if let Some(label) = read_str_or_text(state, root, ACCEL_PROFILE_KEY) {
            let profile = if label == "Flat" { "flat" } else { "adaptive" };
            if profile != self.last.accel_profile {
                self.last.accel_profile = profile.into();
                Self::persist_input(|i| i.accel_profile = profile.into());
            }
        }
        if let Some(label) = read_str_or_text(state, root, COMPOSE_KEY) {
            let compose = match label.as_str() {
                "Right Alt" => Some("ralt"),
                "Menu" => Some("menu"),
                "Caps Lock" => Some("caps"),
                "Right Super" => Some("rwin"),
                "Left Super" => Some("lwin"),
                _ => None,
            };
            if compose != self.last.compose_key.as_deref() {
                self.last.compose_key = compose.map(str::to_string);
                let owned = self.last.compose_key.clone();
                Self::persist_input(|i| i.compose_key = owned);
            }
        }
        // Active source is durable preference only. The session compositor's
        // file watch compiles and applies it without a privileged helper.
        if let Some(label) = read_str_or_text(state, root, LAYOUT_KEY)
            && let Some(id) = self.source_id(&label)
            && self.last.keyboard_layouts.contains(&id)
            && id != self.last.keyboard_layout
        {
            self.last.keyboard_layout = id;
            self.persist_keyboard_sources();
            self.structure_dirty = true;
        }
    }

    fn sync_add_source(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if let Some(label) = read_str_or_text(state, root, ADD_SOURCE_PICKER_KEY) {
            self.pending_add_id = self.source_id(&label);
        }
        if Self::consume_action(state, root, ADD_SOURCE_KEY)
            && self.last.keyboard_layouts.len()
                < settings_store::hardware::MAX_KEYBOARD_INPUT_SOURCES
            && let Some(id) = self.pending_add_id.clone()
            && !self.last.keyboard_layouts.contains(&id)
        {
            self.last.keyboard_layouts.push(id.clone());
            self.last.keyboard_layout = id;
            let _ = state.put(
                root,
                LAYOUT_KEY,
                self.source_label(&self.last.keyboard_layout),
            );
            self.persist_keyboard_sources();
            self.operation_succeeded(
                &crate::actions::KeyboardSourceOperation::Add {
                    label: self.source_label(&self.last.keyboard_layout),
                },
                "keyboard input source added",
            );
            self.structure_dirty = true;
        }
    }

    fn sync_source_order(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let source_count = self.last.keyboard_layouts.len();
        for index in 0..source_count {
            let up = Self::source_action(index, "up");
            let down = Self::source_action(index, "down");
            let remove = Self::source_action(index, "remove");
            if index > 0 && Self::consume_action(state, root, &up) {
                self.last.keyboard_layouts.swap(index - 1, index);
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::MoveUp { index },
                    "keyboard input source moved up",
                );
                self.structure_dirty = true;
                break;
            }
            if index + 1 < source_count && Self::consume_action(state, root, &down) {
                self.last.keyboard_layouts.swap(index, index + 1);
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::MoveDown { index },
                    "keyboard input source moved down",
                );
                self.structure_dirty = true;
                break;
            }
            if source_count > 1 && Self::consume_action(state, root, &remove) {
                let removed = self.last.keyboard_layouts.remove(index);
                if removed == self.last.keyboard_layout {
                    let fallback = index.min(self.last.keyboard_layouts.len().saturating_sub(1));
                    if let Some(new_active) = self.last.keyboard_layouts.get(fallback) {
                        self.last.keyboard_layout = new_active.clone();
                        let _ = state.put(
                            root,
                            LAYOUT_KEY,
                            self.source_label(&self.last.keyboard_layout),
                        );
                    }
                }
                self.persist_keyboard_sources();
                self.operation_succeeded(
                    &crate::actions::KeyboardSourceOperation::Remove { index },
                    "keyboard input source removed",
                );
                self.structure_dirty = true;
                break;
            }
        }
    }
}

impl Widget for MouseKeyboardPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.seed_if_needed(state, root);
        self.sync_simple_fields(state, root);
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
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        // `child` was just created in this same tree, so this call is not
        // reachable to fail; if it (impossibly) did, wrap it in itself
        // rather than panic this panic=abort process.
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let was_handled = *handled;
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
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

    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        // The add/reorder/remove source edge keys are transient internally,
        // but they exist only to apply a durable keyboard-source preference.
        let pending = std::mem::take(&mut self.commit_context_pending)
            || self
                .composed
                .as_ref()
                .is_some_and(|content| content.needs_commit());
        crate::panes::preference_commit_context(
            pending,
            "input",
            "Mouse & keyboard",
            "Change input preferences",
        )
    }

    fn name(&self) -> &'static str {
        "MouseKeyboardPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty());
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<CatalogEntry> {
        vec![
            CatalogEntry {
                id: "us".into(),
                label: "English (US)".into(),
            },
            CatalogEntry {
                id: "de".into(),
                label: "German".into(),
            },
            CatalogEntry {
                id: "fr".into(),
                label: "French".into(),
            },
        ]
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    #[test]
    fn imported_repeat_settings_are_shown_as_the_live_rate_or_disabled() {
        let disabled = MouseKeyboardPane::with_catalog(
            InputPrefs {
                key_repeat: 0.5,
                key_repeat_delay_ms: Some(500),
                key_repeat_rate_hz: Some(0),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(disabled.key_repeat_slider_value(), 0.0);
        assert_eq!(
            disabled.key_repeat_description(),
            "Repeat is off — move the slider to turn it on"
        );

        let exact = MouseKeyboardPane::with_catalog(
            InputPrefs {
                key_repeat: 0.5,
                key_repeat_delay_ms: Some(275),
                key_repeat_rate_hz: Some(37),
                ..Default::default()
            },
            catalog(),
        );
        assert!((exact.key_repeat_slider_value() - 0.675).abs() < f64::EPSILON);
        assert_eq!(
            exact.key_repeat_description(),
            "37 keys per second · starts after 275 ms"
        );

        let partial = MouseKeyboardPane::with_catalog(
            InputPrefs {
                key_repeat: 0.25,
                key_repeat_rate_hz: Some(50),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(partial.key_repeat_slider_value(), 0.25);
        assert_eq!(
            partial.key_repeat_description(),
            "Faster repeat starts sooner"
        );

        let clamped = MouseKeyboardPane::with_catalog(
            InputPrefs {
                key_repeat_delay_ms: Some(200),
                key_repeat_rate_hz: Some(1_000),
                ..Default::default()
            },
            catalog(),
        );
        assert_eq!(clamped.key_repeat_slider_value(), 1.0);
    }

    #[test]
    fn document_active_source_selection_is_ignored() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let initial = InputPrefs {
            keyboard_layouts: vec!["us".into(), "de".into()],
            ..Default::default()
        };
        let mut pane = MouseKeyboardPane::with_catalog(initial, catalog());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, LAYOUT_KEY, "German");
        pane.sync(&mut doc, &root);
        assert_eq!(pane.last.keyboard_layout, "us");
        assert_eq!(SystemPrefs::load().input.keyboard_layout, "us");
        assert!(!pane.take_structure_dirty());
    }

    #[test]
    fn document_source_latches_are_ignored() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = MouseKeyboardPane::with_catalog(InputPrefs::default(), catalog());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);

        let _ = doc.put(&root, ADD_SOURCE_PICKER_KEY, "German");
        let _ = doc.put(&root, ADD_SOURCE_KEY, true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.last.keyboard_layouts, ["us"]);
        assert_eq!(pane.last.keyboard_layout, "us");

        let _ = doc.put(&root, MouseKeyboardPane::source_action(1, "up"), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.last.keyboard_layouts, ["us"]);
        assert_eq!(pane.last.keyboard_layout, "us");

        let _ = doc.put(&root, MouseKeyboardPane::source_action(0, "remove"), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.last.keyboard_layouts, ["us"]);
        assert_eq!(pane.last.keyboard_layout, "us");
    }

    /// Device class speed sliders only travel through local effect actions.
    #[test]
    fn document_pointer_speed_changes_are_ignored() {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let mut pane = MouseKeyboardPane::with_catalog(InputPrefs::default(), catalog());
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            TOUCHPAD_POINTER_SPEED_KEY,
            automerge::ScalarValue::F64(0.9),
        );
        let _ = doc.put(
            &root,
            MOUSE_POINTER_SPEED_KEY,
            automerge::ScalarValue::F64(0.2),
        );
        pane.sync(&mut doc, &root);
        assert_eq!(pane.last.touchpad_pointer_speed, None);
        assert_eq!(pane.last.mouse_pointer_speed, None);
    }
}

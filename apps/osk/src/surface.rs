//! The keyboard as the window of [`Osk`]'s view.
//!
//! [`OskSurface`] presents the person's keyboard preferences — their
//! settings — to the [`OskWidget`], stores the changes the keyboard makes
//! itself (a layout key, an emoji pick) and the ones a palette, agent or
//! script asks for through an [`OskEffect`], which the view answers on it.
//! The view publishes the preferences as live rows beside the resource.
//! What typing teaches the keyboard is kept in the keyboard's [`Learned`]
//! record, synced with the workspace it is open in. In the keyboard's own
//! process ([`crate::service::serve`]) a live window also connects to the
//! session: it follows the focused field over the input method and types
//! into it.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, Effected, PersonRecord, Publishes, Setting, ViewCx};
use ui::widget::{AppModel, Decorator, Element, ModelPath, Widget};

use crate::actions::{OskEffect, SetPreference};
use crate::learned::{Learned, learned};
use crate::resource::{
    HIT_OVERLAY_FLAG, INPUT_METHOD_FLAG, NO_INPUT_METHOD_FLAG, Osk, OskPreferences, PURPOSE_OPTION,
};
use crate::service::{self, InputMethodChoice};
use crate::session::{self, Said};
use crate::{InputPurpose, OskWidget};

pub(crate) const SCENARIOS: &[&str] = &[
    "terminal",
    "terminal-ctrl",
    "terminal-locked",
    "number",
    "phone",
    "email",
    "url",
    "pin",
    "french",
    "compact",
    "large",
    "emoji",
    "composing",
    "parked",
];

pub struct OskSurface {
    keyboard: OskWidget,
    preferences: Option<(Setting<OskPreferences>, Option<OskPreferences>)>,
    learned: Option<(PersonRecord<Learned>, u64)>,
    vocabulary: Option<ui::personal_dictionary::Snapshot>,
    vocabulary_identity: (Option<[u8; 32]>, u64),
    focus: Option<service::FocusChanges>,
    /// What the session says the keyboard does, while it runs as an app of
    /// one: whether it belongs on screen, which the input method alone cannot
    /// say.
    said: Option<Said>,
    dismissed: bool,
    context: Option<service::ContextChanges>,
    /// The scenario to stage on the next sync.
    staged: Rc<RefCell<Option<Scenario>>>,
}

impl Default for OskSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl OskSurface {
    /// Tap the toolbar item `id`, as a scenario's finger would.
    fn tap_tool(&mut self, id: &str) {
        if let Some(index) = self
            .keyboard
            .toolbar_items()
            .iter()
            .position(|item| item.id == id)
        {
            self.keyboard.activate_tool(index);
        }
    }

    pub fn new() -> Self {
        Self::with_keyboard(OskWidget::new())
    }

    pub fn with_keyboard(keyboard: OskWidget) -> Self {
        Self {
            keyboard: keyboard.with_surface_height(),
            preferences: None,
            learned: None,
            vocabulary: None,
            vocabulary_identity: (None, 0),
            focus: None,
            said: None,
            dismissed: false,
            context: None,
            staged: Rc::default(),
        }
    }

    pub fn keyboard(&self) -> &OskWidget {
        &self.keyboard
    }

    fn store(&mut self, edits: &[SetPreference]) -> Result<(), String> {
        if edits.is_empty() {
            return Ok(());
        }
        let Some((setting, _)) = &self.preferences else {
            return Err("the keyboard keeps no preferences here".into());
        };
        let mut preferences = setting.get();
        for edit in edits {
            edit.apply(&mut preferences)?;
        }
        setting
            .set(preferences)
            .map_err(|error| format!("the keyboard preferences could not be stored: {error}"))?;
        self.present_preferences();
        Ok(())
    }

    fn present_preferences(&mut self) {
        let Some((setting, presented)) = &mut self.preferences else {
            return;
        };
        let current = setting.get();
        if presented.as_ref() == Some(&current) {
            return;
        }
        *presented = Some(current.clone());
        self.keyboard.present_preferences(&current);
        if !current.personalization
            && let Some((learned, _)) = &self.learned
            && !learned.with(Learned::is_empty)
            && let Err(error) = learned.invoke(&Learned::FORGET.invoke())
        {
            tracing::warn!(%error, "osk: what the keyboard learned could not be forgotten");
        }
    }

    fn submit_edits(&mut self) {
        let edits = self.keyboard.take_preference_edits();
        if let Err(error) = self.store(&edits) {
            tracing::warn!(%error, "osk: a preference change was not kept");
        }
        let forget = self.keyboard.take_forget_learned();
        let lesson = self.keyboard.take_lesson();
        let Some((learned, _)) = &self.learned else {
            return;
        };
        let saved = if forget {
            learned.invoke(&Learned::FORGET.invoke())
        } else {
            lesson.map_or(Ok(()), |lesson| {
                learned.invoke(&Learned::LEARN.with(learned::Learn { lesson }))
            })
        };
        if let Err(error) = saved {
            tracing::warn!(%error, "osk: what the keyboard learned could not be saved");
        }
    }

    fn present_learned(&mut self) {
        let Some((learned, presented)) = &mut self.learned else {
            return;
        };
        let revision = learned.revision();
        if revision != *presented {
            *presented = revision;
            self.keyboard.present_learned(learned.get());
        }
    }

    fn connect(&mut self, cx: &ViewCx<Osk>) {
        let choice = if cx.launch_flag(NO_INPUT_METHOD_FLAG) {
            InputMethodChoice::Off
        } else if cx.launch_flag(INPUT_METHOD_FLAG) {
            InputMethodChoice::Require
        } else {
            InputMethodChoice::Auto
        };
        self.said = session::follow(cx.waker());
        match service::connect(choice, cx.waker()) {
            Ok(connection) => {
                if connection.focus.is_some() {
                    self.keyboard.set_visible(false);
                }
                self.keyboard.set_output(connection.output);
                self.keyboard.set_context_handler(connection.invoke_context);
                self.focus = connection.focus;
                self.context = connection.context;
            }
            Err(error) => required_input_method_missing(&error),
        }
    }
}

/// A keyboard launched to serve only through the input method cannot serve
/// without it: it stops before anything was typed or changed.
#[allow(
    clippy::exit,
    reason = "an explicitly required input method is missing; the launch fails"
)]
fn required_input_method_missing(error: &str) -> ! {
    tracing::error!("osk: {error}");
    std::process::exit(1)
}

impl Decorator for OskSurface {
    type Inner = OskWidget;

    fn inner(&self) -> &OskWidget {
        &self.keyboard
    }

    fn inner_mut(&mut self) -> &mut OskWidget {
        &mut self.keyboard
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let staged = self.staged.borrow_mut().take();
        if let Some(Scenario(scenario)) = staged {
            self.stage(scenario.as_deref());
        }
        if let Some(focus) = &mut self.focus {
            for transition in focus.take() {
                self.keyboard.follow_input_method(transition);
            }
        }
        if self.keyboard.take_dismissed()
            && let Some(said) = &self.said
        {
            said.set_visible(false);
            self.dismissed = true;
        }
        // The session's own word on whether the keyboard belongs on screen
        // comes after the input method's: an always-on keyboard is up with no
        // field focused at all, and one the person is not typing on the glass
        // stays down even as a field takes the focus.
        if let Some(visible) = self.said.as_ref().and_then(Said::visible) {
            if visible || !self.dismissed {
                self.keyboard.set_visible(visible);
            }
            self.dismissed = self.dismissed && visible;
        }
        if let Some(context) = &mut self.context {
            for transition in context.take() {
                match transition {
                    service::ContextTransition::Context {
                        generation,
                        actions,
                    } => {
                        let items = actions
                            .into_iter()
                            .map(|item| {
                                crate::toolbar::OskToolbarItem::new(
                                    &format!("context:{}:{}", generation, item.slot),
                                    None,
                                    &item.label,
                                    crate::toolbar::OskToolbarAction::Contextual {
                                        generation,
                                        slot: item.slot,
                                    },
                                )
                            })
                            .collect();
                        self.keyboard.set_context_toolbar(generation, items);
                    }
                    service::ContextTransition::Result {
                        generation,
                        slot,
                        succeeded,
                        message,
                    } => {
                        self.keyboard
                            .context_result(generation, slot, succeeded, message);
                    }
                }
            }
        }
        self.keyboard.sync(state, path);
        self.submit_edits();
        self.present_preferences();
        self.present_learned();
        let identity = (
            ui::local_identity::bound().map(|identity| identity.group_public_key),
            ui::local_identity::generation(),
        );
        if identity != self.vocabulary_identity {
            self.vocabulary_identity = identity;
            self.keyboard
                .set_personal_vocabulary(Some(ui::personal_dictionary::reader()));
        }
        let vocabulary = ui::personal_dictionary::snapshot().ok();
        if vocabulary != self.vocabulary {
            self.vocabulary = vocabulary;
            self.keyboard.refresh_personal_vocabulary();
        }
    }
}

struct Scenario(Option<String>);

pub(crate) fn window(cx: &ViewCx<Osk>) -> Element {
    let mut window = OskSurface::new();
    window.attach(cx);
    let staged = Rc::clone(&window.staged);
    cx.on_stage(move |scenario| *staged.borrow_mut() = Some(Scenario(scenario.map(str::to_owned))));
    cx.window_effects(|window: &mut OskSurface, effect: OskEffect, _cx| window.answer(&effect));
    let preferences = cx.settings::<OskPreferences>();
    cx.publishing(
        Publishes::new().live_rows(move || crate::actions::preference_rows(&preferences.get())),
        Box::new(window),
    )
}

impl OskSurface {
    fn attach(&mut self, cx: &ViewCx<Osk>) {
        if cx.mode().is_live() {
            self.keyboard
                .set_personal_vocabulary(Some(ui::personal_dictionary::reader()));
        }
        if let Some(purpose) = cx.launch_option(PURPOSE_OPTION).and_then(purpose_from_str) {
            self.keyboard.set_purpose(purpose);
        }
        self.keyboard
            .set_hit_overlay(cx.launch_flag(HIT_OVERLAY_FLAG));
        self.preferences = Some((cx.settings::<OskPreferences>(), None));
        if cx.mode().is_live()
            && let Some(learned) = cx.person::<Learned>()
        {
            self.keyboard.present_learned(learned.get());
            self.learned = Some((learned.clone(), learned.revision()));
        }
        self.present_preferences();
        if cx.mode().is_live() && service::serving() {
            self.connect(cx);
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        let staged = match scenario {
            Some("french") => Some(SetPreference::Layout("azerty".into())),
            Some("compact") => Some(SetPreference::Size("compact".into())),
            Some("large") => Some(SetPreference::Size("large".into())),
            Some("emoji") => Some(SetPreference::EmojiRecents(
                [
                    "thumbs up",
                    "face with tears of joy",
                    "red heart",
                    "party popper",
                ]
                .into_iter()
                .filter_map(|name| {
                    emoji::Catalogue::english()
                        .emoji()
                        .iter()
                        .find(|entry| entry.name() == name)
                })
                .map(|entry| entry.text().to_owned())
                .collect(),
            )),
            _ => None,
        };
        if let Some(staged) = staged
            && let Err(error) = self.store(&[staged])
        {
            tracing::warn!(%error, "osk: a scenario's preference was not staged");
        }
        match scenario {
            Some("emoji") => self.keyboard.open_emoji_panel(),
            Some("parked") => self.keyboard.set_parked(true),
            Some("terminal-ctrl") => {
                self.keyboard.set_purpose(InputPurpose::Terminal);
                self.tap_tool("ctrl");
            }
            Some("terminal-locked") => {
                self.keyboard.set_purpose(InputPurpose::Terminal);
                self.tap_tool("ctrl");
                self.tap_tool("ctrl");
                self.tap_tool("alt");
            }
            Some("composing") => {
                for letter in ["h", "e", "l"] {
                    self.keyboard.activate_label(letter);
                }
            }
            Some(name) => {
                if let Some(purpose) = purpose_from_str(name) {
                    self.keyboard.set_purpose(purpose);
                }
            }
            None => {}
        }
    }

    fn answer(&mut self, effect: &OskEffect) -> EffectAnswer {
        if let Some(change) = effect.preference() {
            self.store(&[change])?;
        } else {
            self.keyboard.clear_personalization();
            self.submit_edits();
        }
        Ok(Effected::done())
    }
}

/// The field kind a `--purpose` value names, in the short spellings a person
/// types as well as the canonical names. An unknown kind is `None`, so a typo
/// leaves the keyboard on plain text rather than failing its launch.
pub(crate) fn purpose_from_str(name: &str) -> Option<InputPurpose> {
    Some(match name.to_ascii_lowercase().as_str() {
        "text" => InputPurpose::Text,
        "terminal" | "code" => InputPurpose::Terminal,
        "number" | "numeric" => InputPurpose::Number,
        "decimal" => InputPurpose::Decimal,
        "phone" | "tel" | "telephone" => InputPurpose::Telephone,
        "url" => InputPurpose::Url,
        "email" => InputPurpose::Email,
        "pin" => InputPurpose::Pin,
        "password" | "pass" => InputPurpose::Password,
        "date" => InputPurpose::Date,
        "time" => InputPurpose::Time,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_scenario_presents_the_real_code_keyboard_purpose() {
        let mut surface = OskSurface::new();
        surface.stage(Some("terminal"));
        assert_eq!(surface.keyboard().purpose(), InputPurpose::Terminal);
        assert_eq!(purpose_from_str("code"), Some(InputPurpose::Terminal));
        assert_eq!(purpose_from_str("wrong-kind"), None);
    }
}

//! The keyboard as the surface of its [`Osk`] preferences.
//!
//! [`OskSurface`] presents the preferences to the [`OskWidget`] and submits
//! the changes the keyboard makes itself (a layout key, an emoji pick)
//! through the dispatcher, like every other edit. In the keyboard's own
//! process ([`crate::service::serve`]) a live surface also connects to the
//! session: it follows the focused field over the input method and types
//! into it.

use std::rc::Rc;

use ui::framework::{Dispatcher, EffectAnswer, EffectCx, Effected, ResourceSurface, SurfaceCx};
use ui::widget::{AppModel, Decorator, ModelPath, Widget};

use crate::actions::OskEffect;
use crate::resource::{
    HIT_OVERLAY_FLAG, INPUT_METHOD_FLAG, NO_INPUT_METHOD_FLAG, Osk, OskPreferences, PURPOSE_OPTION,
};
use crate::service::{self, InputMethodChoice};
use crate::{InputPurpose, OskWidget};

/// The screenshot scenarios: a field of each kind the keyboard serves, the
/// French layout, the emoji panel and a word being composed.
pub(crate) const SCENARIOS: &[&str] = &[
    "number",
    "phone",
    "email",
    "url",
    "pin",
    "french",
    "emoji",
    "composing",
];

/// The on-screen keyboard over its preferences.
pub struct OskSurface {
    keyboard: OskWidget,
    dispatch: Option<Dispatcher<Osk>>,
    /// Focus changes the input method reports, while the keyboard serves the
    /// session through one.
    focus: Option<service::FocusChanges>,
}

impl Default for OskSurface {
    fn default() -> Self {
        Self::new()
    }
}

impl OskSurface {
    pub fn new() -> Self {
        Self::with_keyboard(OskWidget::new())
    }

    /// The surface over `keyboard`.
    pub fn with_keyboard(keyboard: OskWidget) -> Self {
        Self {
            keyboard,
            dispatch: None,
            focus: None,
        }
    }

    /// The keyboard it shows.
    pub fn keyboard(&self) -> &OskWidget {
        &self.keyboard
    }

    /// Submit the preference changes the keyboard made itself.
    fn submit_edits(&mut self) {
        let edits = self.keyboard.take_preference_edits();
        if let Some(dispatch) = &self.dispatch {
            for edit in &edits {
                dispatch.apply(edit);
            }
        }
    }

    /// Serve the session: take its input method, unless the launch said
    /// otherwise, and type into the focused field.
    fn connect(&mut self, cx: &SurfaceCx<Osk>) {
        let choice = if cx.launch_flag(NO_INPUT_METHOD_FLAG) {
            InputMethodChoice::Off
        } else if cx.launch_flag(INPUT_METHOD_FLAG) {
            InputMethodChoice::Require
        } else {
            InputMethodChoice::Auto
        };
        match service::connect(choice, cx.waker()) {
            Ok(connection) => {
                // Driven by the input method, the keyboard waits hidden for
                // a focused field.
                if connection.focus.is_some() {
                    self.keyboard.set_visible(false);
                }
                self.keyboard.set_output(connection.output);
                self.focus = connection.focus;
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

    /// The keyboard's node is the surface's: the window finds the surface
    /// where the keyboard is laid out, so a control operated there (an
    /// assistive technology's key press) reaches the keyboard through it.
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        theme: &ui::theme::Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = self.keyboard.build_layout(taffy, theme, node_map);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some(focus) = &mut self.focus {
            for transition in focus.take() {
                self.keyboard.follow_input_method(transition);
            }
        }
        self.keyboard.sync(state, path);
        self.submit_edits();
    }
}

impl ResourceSurface<Osk, OskEffect> for OskSurface {
    fn present(&mut self, state: &Rc<OskPreferences>) {
        self.keyboard.present_preferences(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Osk>) {
        if let Some(purpose) = cx.launch_option(PURPOSE_OPTION).and_then(purpose_from_str) {
            self.keyboard.set_purpose(purpose);
        }
        self.keyboard
            .set_hit_overlay(cx.launch_flag(HIT_OVERLAY_FLAG));
        self.dispatch = Some(cx.dispatcher());
        if cx.mode().is_live() && service::serving() {
            self.connect(&cx);
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("emoji") => self.keyboard.open_emoji_panel(),
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

    fn effect(&mut self, effect: OskEffect, _cx: &mut EffectCx<Osk, Self>) -> EffectAnswer {
        match effect {
            OskEffect::ClearPersonal => {
                self.keyboard.clear_personalization();
                self.submit_edits();
            }
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

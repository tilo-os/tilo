//! The on-screen keyboard, as the session keeps it: a part of the desktop whose view is a
//! tilolang package (`apps/tl/osk`, or one the person bound in its place), run while the person
//! may want it, docked at the bottom of the display they type on, and shown while a text field
//! waits for typing on the glass.
//!
//! The session keeps the state and publishes it as [`KEYBOARD`]; the view reads it as
//! `session.keyboard`. Whether it belongs on screen is [`crate::a11y_settings::keyboard_visible`],
//! from the person's own choice and what the compositor sees of their typing; whether it runs at
//! all is [`crate::a11y_settings::keyboard_runs`].
//!
//! The reading also says what the field in front asks of typing (its purpose, its hints and
//! whether it is secret) and carries the person's keyboard settings (`settings/apps/so.tilo.osk`:
//! the layouts, the key size, suggestions, corrections and what typing taught). The resource's
//! actions type into the field: committed text, the word being composed, and named keys. The
//! session carries them out on its pass, where the compositor is, through the compositor's input
//! method, so the keyboard holds no connection to the compositor of its own and no copy of the
//! field. What typing teaches it keeps in the person's settings through the same resource. An app
//! asks them only while the session runs it as the keyboard; the person and an agent acting for
//! them ask as they would press a key.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::{ActionOutput, ApiError, ResourceSpec};
use tilo_service::{Service, Started, decode, reading};

use settings_sections::osk::OskSourceSettings;

/// The resource the keyboard's app reads.
pub const KEYBOARD: &str = "keyboard";

/// The resource's type, as a package reads it (`session.keyboard`).
pub const KIND: &str = "keyboard";

/// The part the keyboard's view stands in, as the person binds a view to it.
pub const PART: &str = "keyboard";

/// The view the session draws the keyboard with until the person binds another: the on-screen
/// keyboard's package, which every session ships.
const SHIPPED: (&str, &str) = (
    tilolang_tilo::keyboard::KEYBOARD_APP,
    tilolang_tilo::keyboard::KEYBOARD_VIEW,
);

/// The person's keyboard settings, which the reading carries and what typing teaches goes to.
pub const SETTINGS: &str = "settings/apps/so.tilo.osk";

/// The person's own words, which the spelling check, the word processor and the keyboard share.
const PERSONAL_WORDS: &str = "settings/personal-dictionary";

/// The language the keyboard reads words in: its own choice, or the letter layout's (AZERTY types
/// French, QWERTY English).
fn typing_language(settings: &OskSourceSettings) -> &str {
    match settings.typing_language.as_str() {
        "layout" | "" => {
            if settings.layout == "azerty" {
                "fr"
            } else {
                "en"
            }
        }
        chosen => chosen,
    }
}

/// The on-screen keyboard as the session publishes it: whether it shows and where it stands.
#[derive(tilo_resource::Row)]
#[row(kind = "keyboard", label = "On-screen keyboard")]
pub(crate) struct OnScreenKeyboard;

/// The margin between the keyboard and the bottom and sides of the display,
/// in logical pixels.
const MARGIN: i32 = 20;
/// How wide the keyboard grows at most before it stops widening with the display.
const WIDEST: u32 = 960;
/// How tall the keyboard is at least, on a short display, and how tall it grows at most: it takes
/// two fifths of the display's height between them.
const LOWEST: u32 = 220;
const TALLEST: u32 = 380;

/// Where the keyboard stands on a display of `width` by `height` logical pixels: the size its
/// window takes, and the margin from the edges it is pinned to.
/// At the bottom, centred, no wider than [`WIDEST`], between [`LOWEST`] and [`TALLEST`] tall with
/// [`MARGIN`] to the display's edge: wide enough to type on with two thumbs on a phone, and narrow
/// enough to leave the window it types into in view on a monitor.
#[must_use]
pub fn band(width: i32, height: i32) -> ((u32, u32), (i32, i32, i32, i32)) {
    let (width, height) = (i64::from(width), i64::from(height));
    let margin = i64::from(MARGIN).min(width.saturating_div(20));
    let widest = width
        .saturating_sub(margin.saturating_mul(2))
        .max(1)
        .min(i64::from(WIDEST));
    let tallest = height
        .saturating_mul(2)
        .saturating_div(5)
        .clamp(i64::from(LOWEST), i64::from(TALLEST));
    let shortest = height.saturating_sub(margin.saturating_mul(2)).max(1);
    let band = (
        u32::try_from(widest).unwrap_or(1),
        u32::try_from(tallest.min(shortest)).unwrap_or(1),
    );
    // Only the bottom edge is pinned, so only its margin places the keyboard: it stands that far
    // above the display's edge.
    (band, (0, 0, i32::try_from(margin).unwrap_or(0), 0))
}

/// What the on-screen keyboard does now. The session's loop says whether it belongs on screen and
/// where it stands; the service hands that to the app.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shown {
    /// The keyboard belongs on screen now.
    pub visible: bool,
    /// The display it stands on, while it runs.
    pub display: Option<String>,
    /// Which field the person types into: it changes when they move to another one.
    pub field: u64,
    /// What the field asks for: the layout its purpose picks (`text`, `number`, `email`,
    /// `password`, …).
    pub purpose: String,
    /// The hints the field gave (`completion`, `spellcheck`, `auto-capitalization`, `lowercase`,
    /// `uppercase`, `titlecase`, `hidden-text`, `sensitive-data`, `latin`, `multiline`).
    pub hints: u32,
    /// The field marks what is typed there as a secret: nothing typed is shown as composing
    /// text, suggested from, corrected or learned.
    pub secret: bool,
    /// The person's keyboard settings: the layouts, the key size, suggestions and corrections,
    /// and what typing taught.
    #[serde(flatten)]
    pub settings: OskSourceSettings,
    /// The person's own words in the language the keyboard reads words in: offered first, and
    /// never corrected.
    pub personal: Vec<String>,
}

/// Where the on-screen keyboard's app stands, as the session's loop keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Run {
    /// It does not run: nobody may want it now.
    #[default]
    Down,
    /// It does not run, and is not asked again this session: its program is not installed, and does
    /// not appear by waiting.
    Failed,
    /// It runs on `display` , `size` logical pixels wide and tall.
    Running { display: String, size: (u32, u32) },
}

/// What the keyboard's resource asks of the session: the person's own word on whether it shows,
/// and text typed into the field in front.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Show the keyboard, or put it away, until the field it was asked over is left.
    SetVisible(bool),
    /// Type `text` into the field the person types into, as a key of the keyboard commits it.
    TypeText(String),
    /// Show `text` in the field as the word being composed, not yet committed.
    Preedit(String),
    /// Press the key `name` (a letter, a mark, or `escape`, `tab`, `left`, `right`, `up`,
    /// `down`, `home`, `end`, `page-up`, `page-down`, `delete`, `backspace`, `return`, `space`)
    /// with Ctrl and Alt held when asked.
    PressKey {
        name: String,
        ctrl: bool,
        alt: bool,
        times: u32,
    },
    /// Type what the clipboard holds into the field the person types into, as it is.
    Paste,
    /// Ask the person's keyboard settings for `action` with `arguments`: what typing taught, or
    /// the layout the keyboard's own switch key moved to.
    Settings(&'static str, serde_json::Map<String, Value>),
}

/// The person's own word on the keyboard, and what the field was asking for when they said it: it
/// stands until that field is left or one is reached, where the automatic rule takes over again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Asked {
    visible: bool,
    wanted_typing: bool,
}

/// The keyboard's state, as the session's loop keeps it.
#[derive(Clone, Default)]
pub struct Keyboard {
    shown: Rc<RefCell<Shown>>,
    /// What the resource asked for, waiting for the loop that has the compositor.
    asks: Rc<RefCell<Vec<Ask>>>,
    asked: Rc<Cell<Option<Asked>>>,
    /// The app the session runs as the keyboard: the one app whose asks to type are carried out.
    typist: Rc<RefCell<Option<String>>>,
}

impl Keyboard {
    /// Say what the keyboard does now, when it is not what it did. Whether it changed.
    pub fn show(&self, shown: Shown) -> bool {
        if *self.shown.borrow() == shown {
            return false;
        }
        *self.shown.borrow_mut() = shown;
        true
    }

    /// What the keyboard does now.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shown.borrow().clone()
    }

    /// Ask the session for `ask`, on its next pass.
    fn ask(&self, ask: Ask) {
        self.asks.borrow_mut().push(ask);
    }

    /// The session runs `app` as the keyboard now, or none.
    pub(crate) fn typed_by(&self, app: Option<&str>) {
        *self.typist.borrow_mut() = app.map(str::to_owned);
    }

    /// Whether `app` may ask the keyboard's actions: the app the session runs as the keyboard.
    fn types(&self, app: &str) -> bool {
        self.typist.borrow().as_deref() == Some(app)
    }

    /// What was asked since the last pass.
    pub(crate) fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }

    /// The person's own word on whether the keyboard shows, while it stands: `None` while the
    /// automatic rule decides.
    #[must_use]
    pub fn asked(&self) -> Option<bool> {
        self.asked.get().map(|asked| asked.visible)
    }

    /// The person said whether the keyboard shows while the field asks `wanted_typing`.
    pub(crate) fn say(&self, visible: bool, wanted_typing: bool) {
        self.asked.set(Some(Asked {
            visible,
            wanted_typing,
        }));
    }

    /// The field the person's word was about is gone: the automatic rule decides again.
    pub(crate) fn forget_ask(&self) {
        self.asked.set(None);
    }

    /// The person's word was about a field that wants typing, or one that does not: a pass that
    /// finds the other lets it go, so the field they moved to decides as it always does.
    pub(crate) fn forget_ask_when_the_field_moved(&self, wanted_typing: bool) {
        if self
            .asked
            .get()
            .is_some_and(|asked| asked.wanted_typing != wanted_typing)
        {
            self.asked.set(None);
        }
    }
}

/// The keyboard's state, as the session's services publish it.
pub struct KeyboardService {
    state: Keyboard,
    /// What was last published, so a change of it is one revision.
    shown: Option<Shown>,
}

impl KeyboardService {
    /// The service publishing `state` .
    #[must_use]
    pub fn new(state: Keyboard) -> Self {
        let shown = Some(state.shown());
        Self { state, shown }
    }
}

impl Service for KeyboardService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![keyboard_resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == KEYBOARD).then(|| reading(&*self.state.shown.borrow()))
    }

    /// The keyboard shows itself by the person's own word on it and by what the field in front asks,
    /// and types through the compositor's input method: both are the session's to carry out, so what
    /// a press of its Dismiss and an agent's step ask for is one resource's actions.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<Started, ApiError> {
        match decode::<KeyboardAction>(&self.resources(), resource, action, arguments)? {
            KeyboardAction::SetVisible { visible } => self.state.ask(Ask::SetVisible(visible)),
            KeyboardAction::TypeText { text } => self.state.ask(Ask::TypeText(text)),
            KeyboardAction::Preedit { text } => self.state.ask(Ask::Preedit(text)),
            KeyboardAction::PressKey {
                name,
                ctrl,
                alt,
                times,
            } => {
                self.state.ask(Ask::PressKey {
                    name,
                    ctrl: ctrl.unwrap_or(false),
                    alt: alt.unwrap_or(false),
                    times: times
                        .map_or(1, |times| u32::try_from(times.clamp(0, 1024)).unwrap_or(0)),
                });
            }
            KeyboardAction::Paste => self.state.ask(Ask::Paste),
            KeyboardAction::Learn { before, word } => self.state.ask(Ask::Settings(
                "learn",
                arguments_of([("before", before.into()), ("word", word.into())]),
            )),
            KeyboardAction::Teach {
                letter,
                dx,
                dy,
                samples,
            } => self.state.ask(Ask::Settings(
                "teach",
                arguments_of([
                    ("letter", letter.into()),
                    ("dx", dx.into()),
                    ("dy", dy.into()),
                    ("samples", samples.into()),
                ]),
            )),
            KeyboardAction::Keep { word } => {
                self.state
                    .ask(Ask::Settings("keep", arguments_of([("word", word.into())])));
            }
            KeyboardAction::SetLayouts { layouts, active } => self.state.ask(Ask::Settings(
                "set-layouts",
                arguments_of([("layouts", layouts.into()), ("active", active.into())]),
            )),
        }
        Ok(Started::Done(ActionOutput::default()))
    }

    /// An app asks the keyboard's actions only while the session runs it as the keyboard: the
    /// field in front belongs to another app, which no other app types into or reads the purpose
    /// of. The person and an agent acting for them ask through [`Self::invoke`], as they press a
    /// key.
    fn invoke_for(
        &mut self,
        app: &str,
        _workspace: &str,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if !self.state.types(app) {
            return Err(ApiError::Unavailable {
                code: "permission_denied".to_owned(),
                reason: format!(
                    "{app} is not the on-screen keyboard the session runs: only that app types \
                     into the field in front"
                ),
            });
        }
        self.invoke(resource, action, arguments)
    }

    fn poll(&mut self) -> bool {
        let shown = self.state.shown();
        if self.shown.as_ref() == Some(&shown) {
            return false;
        }
        self.shown = Some(shown);
        true
    }
}

/// What the on-screen keyboard offers: whether it shows, and typing text into the field in front.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.session.keyboard")]
pub(crate) enum KeyboardAction {
    /// Show the keyboard, or put it away, until the field it was asked over is left
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetVisible {
        /// Whether the keyboard belongs on screen
        #[param("Whether the keyboard belongs on screen")]
        visible: bool,
    },
    /// Type text into the field the person types into
    #[effect(kind = "outside")]
    #[effect(own_device)]
    TypeText {
        /// The text to type
        #[param("The text to type")]
        text: String,
    },
    /// Show the word being composed in the field, not yet committed
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Preedit {
        /// The composing text
        #[param("The composing text")]
        text: String,
    },
    /// Press a key of the keyboard, with Ctrl or Alt held when asked
    #[effect(kind = "outside")]
    #[effect(own_device)]
    PressKey {
        /// The key to press
        #[param("The key to press")]
        name: String,
        /// Hold Ctrl
        #[param("Hold Ctrl")]
        ctrl: Option<bool>,
        /// Hold Alt
        #[param("Hold Alt")]
        alt: Option<bool>,
        /// How many times to press it
        #[param("How many times to press it")]
        times: Option<i64>,
    },
    /// Type what the clipboard holds into the field the person types into
    #[effect(kind = "outside")]
    #[effect(own_device)]
    Paste,
    /// Learn a word after another
    #[effect(kind = "outside")]
    Learn {
        /// The word before, or empty at the start of a phrase
        #[param("The word before, or empty at the start of a phrase")]
        before: String,
        /// The word that followed it
        #[param("The word that followed it")]
        word: String,
    },
    /// Learn where taps land on a letter
    #[effect(kind = "outside")]
    Teach {
        /// The letter tapped
        #[param("The letter tapped")]
        letter: String,
        /// How far across from its middle the taps land, in key widths
        #[param("How far across from its middle the taps land, in key widths")]
        dx: f64,
        /// How far down from its middle the taps land, in key heights
        #[param("How far down from its middle the taps land, in key heights")]
        dy: f64,
        /// How many taps taught it
        #[param("How many taps taught it")]
        samples: i64,
    },
    /// Keep a word as typed
    #[effect(kind = "outside")]
    Keep {
        /// The word as typed
        #[param("The word as typed")]
        word: String,
    },
    /// Set the letter layouts
    #[effect(kind = "outside")]
    #[effect(idempotent)]
    SetLayouts {
        /// The layouts in switching order
        #[param("The layouts in switching order")]
        layouts: Vec<String>,
        /// The one typed with
        #[param("The one typed with")]
        active: String,
    },
}

/// Arguments by name, as an action takes them.
fn arguments_of<const N: usize>(pairs: [(&str, Value); N]) -> serde_json::Map<String, Value> {
    pairs
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
}

/// The on-screen keyboard as the session publishes it.
pub(crate) fn keyboard_resource() -> ResourceSpec {
    tilo_service::offer::<KeyboardAction>(
        tilo_resource::LiveRow::typed::<OnScreenKeyboard>(
            KEYBOARD,
            crate::i18n::t("session-resource-keyboard"),
        )
        .into_resource(),
    )
}

impl crate::Session {
    /// Whether the on-screen keyboard's view runs now: it does while the person may want it.
    #[must_use]
    pub fn keyboard_runs(&self) -> bool {
        self.part_views.running(PART)
    }

    /// The band the keyboard stands in, `(x, y, w, h)` on the desktop, while it runs.
    #[must_use]
    pub fn keyboard_band(&self) -> Option<(i32, i32, u32, u32)> {
        self.part_views.docked(PART)
    }

    /// Follow the on-screen keyboard: run its view while the person may want it, docked on the
    /// display they are working on, and publish whether it shows, what the field asks and the
    /// person's keyboard settings.
    /// What it does about showing itself is decided each pass, from the person's choice and what
    /// the compositor sees of their typing.
    pub(crate) fn follow_keyboard(&mut self) {
        let keyboard = self.shared.keyboard.clone();
        self.serve_keyboard_asks();
        let entry = self.compositor.text_entry();
        let preference = self.keyboard_preference;
        // The person's own word on the keyboard stands over the field it was said over: once that
        // field is left, or one is reached, the automatic rule decides again.
        keyboard.forget_ask_when_the_field_moved(entry.wants_typing);
        // Whether it shows: the person's word while it stands, and the automatic rule otherwise.
        let shows = keyboard
            .asked()
            .unwrap_or_else(|| crate::a11y_settings::keyboard_visible(preference, entry));
        // The lock screen draws its own keyboard over a locked session, and takes every key:
        // nothing of the session's stands over it.
        let unlocked = !self.compositor.locked();
        // An engine of the active source owns the seat's input method: its
        // keys compose characters, which a keyboard's committed text cannot,
        // and one field broken between two input methods belongs to neither.
        let composing = crate::input_method::composes(&self.sources.active);
        let runs = unlocked && !composing && crate::a11y_settings::keyboard_runs(preference, entry);
        if !runs {
            self.stop_keyboard();
            // Wanted again later, the keyboard is asked afresh.
            self.keyboard_run = Run::Down;
            keyboard.forget_ask();
            keyboard.show(Shown::default());
            return;
        }
        if self.keyboard_run == Run::Failed {
            keyboard.show(Shown::default());
            return;
        }
        let Some((screen, (x, y, width, height))) = self.keyboard_display() else {
            // No display to stand on, and nothing to say about one.
            self.stop_keyboard();
            keyboard.show(Shown::default());
            return;
        };
        let field = self.compositor.focused_field();
        // The settings sections publish these readings from the very types read here: one that
        // does not read back is a bug in this build, never a person's doing.
        let settings: OskSourceSettings =
            self.read(SETTINGS)
                .map_or_else(Default::default, |reading| {
                    serde_json::from_value(reading)
                        .expect("the keyboard's settings read as they are published")
                });
        let personal = self
            .read(PERSONAL_WORDS)
            .map(|reading| {
                serde_json::from_value::<
                        settings_sections::personal_dictionary::PersonalDictionary,
                    >(reading)
                    .expect("the personal dictionary reads as it is published")
            })
            .map(|dictionary| {
                dictionary
                    .words
                    .hints(typing_language(&settings))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let shown = Shown {
            visible: shows,
            display: Some(screen.clone()),
            field: field.field,
            purpose: field.purpose.to_owned(),
            hints: field.hints,
            secret: field.secret,
            settings,
            personal,
        };
        let moved = keyboard.show(shown.clone());
        if !self.part_views.running(PART) && !self.start_keyboard(&shown) {
            self.keyboard_run = Run::Failed;
            keyboard.typed_by(None);
            keyboard.show(Shown::default());
            return;
        }
        if moved {
            self.part_views.publish(PART, &reading(&shown));
        }
        // At the bottom of the display, centred: the view draws its keys in that band while the
        // reading says it shows, and nothing otherwise, so a hidden keyboard takes no touch from
        // the window above it.
        let (size, margin) = band(width, height);
        let (band_width, band_height) = (
            i32::try_from(size.0).unwrap_or(i32::MAX),
            i32::try_from(size.1).unwrap_or(i32::MAX),
        );
        let left = x.saturating_add(width.saturating_sub(band_width).saturating_div(2));
        let top = y
            .saturating_add(height)
            .saturating_sub(margin.2)
            .saturating_sub(band_height);
        self.part_views
            .dock(PART, Some((left, top, size.0, size.1)));
        let started = Run::Running {
            display: screen,
            size,
        };
        if self.keyboard_run != started {
            tracing::debug!(?started, "the on-screen keyboard stands");
            self.keyboard_run = started;
        }
        self.serve_keyboard_view();
    }

    /// Run the view bound to the keyboard's part over `shown` : the on-screen keyboard's package,
    /// or the one the person bound in its place. Whether one runs.
    fn start_keyboard(&mut self, shown: &Shown) -> bool {
        use crate::parts::{Mount, View};
        let bindings = self.read(crate::parts::BINDINGS);
        match self.parts.follow(PART, SHIPPED, bindings.as_ref()) {
            Mount::Start {
                view: View::Package { dir, app, view },
                restart,
            } => {
                if restart {
                    self.stop_part_view(PART);
                }
                self.start_package_part(PART, dir, app, view, Some(&reading(shown)));
            }
            Mount::Start {
                view: View::Program(program),
                ..
            } => {
                tracing::warn!(
                    program = %program.display(),
                    "a keyboard is a view of the session's keyboard, never a program of its own"
                );
            }
            Mount::Nothing | Mount::Stop => {}
        }
        let Some(host) = self.part_views.host_of(PART) else {
            tracing::warn!("no on-screen keyboard is installed: no keyboard is shown");
            return false;
        };
        if !self.part_views.running(PART) {
            self.parts.forget(PART);
            return false;
        }
        if !self.keyboard_published {
            // Its keys are controls the session's API lists and presses, and assistive technology
            // reads, as every surface the session draws.
            self.keyboard_published = true;
            self.shared.published.add(host);
        }
        let app = self
            .parts
            .running(PART)
            .map_or(SHIPPED.0, |binding| binding.app.as_str())
            .to_owned();
        self.shared.keyboard.typed_by(Some(&app));
        true
    }

    /// Run what the keyboard's view asked through the session's API, as any caller's asks.
    fn serve_keyboard_view(&mut self) {
        if !self.part_views.has_asks(PART) {
            return;
        }
        let published = &self.shared.published;
        self.part_views.serve(PART, |ask| {
            let arguments: serde_json::Map<String, Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            published
                .invoke(
                    &tilo_actions::ActionAddress::new(&ask.resource, &ask.action),
                    &arguments,
                    &mut tilo_session_core::surfaces::Look::none(),
                )
                .map(drop)
                .map_err(|error| error.to_string())
        });
        // What it typed is carried out on the next pass, where the compositor is.
        self.serve_keyboard_asks();
    }

    /// Carry out what the keyboard's resource asked for: the word being composed, text typed
    /// into the field in front, an editing key, and the person's own word on whether it shows.
    fn serve_keyboard_asks(&mut self) {
        let keyboard = self.shared.keyboard.clone();
        let wanted_typing = self.compositor.text_entry().wants_typing;
        for ask in keyboard.take_asks() {
            match ask {
                Ask::SetVisible(visible) => keyboard.say(visible, wanted_typing),
                Ask::TypeText(text) => {
                    if self.compositor.insert_active_text(&text).is_none() {
                        tracing::debug!("no field took the typed text");
                    }
                }
                Ask::Preedit(text) => {
                    if self.compositor.preedit_active_text(&text).is_none() {
                        tracing::debug!("no field took the composing text");
                    }
                }
                Ask::PressKey {
                    name,
                    ctrl,
                    alt,
                    times,
                } => {
                    for _ in 0..times {
                        self.press_key(&name, ctrl, alt);
                    }
                }
                // What the clipboard holds goes in as typed text: literal, so a keyboard never
                // learns from it. A secret the person copied is not kept, so nothing is pasted.
                Ask::Paste => match self.shared.clipboard.text() {
                    Some(text) if !text.is_empty() => {
                        if self.compositor.insert_active_text(&text).is_none() {
                            tracing::debug!("no field took the pasted text");
                        }
                    }
                    _ => tracing::debug!("the clipboard holds no text to paste"),
                },
                // What typing taught, kept in the person's settings through their own actions.
                Ask::Settings(action, arguments) => {
                    if let Err(error) = self.invoke(SETTINGS, action, &arguments) {
                        tracing::warn!(%error, action, "the keyboard's settings did not take it");
                    }
                }
            }
        }
    }

    /// Press the key `name` in the field the person types into, with Ctrl and Alt held when
    /// asked: the key reaches the field as the person's own keyboard's would, so Return submits a
    /// single line and Backspace deletes there.
    fn press_key(&mut self, name: &str, ctrl: bool, alt: bool) {
        let Some((keysym, shift)) = key_of(name) else {
            tracing::debug!(
                key = name,
                "the on-screen keyboard pressed no key of that name"
            );
            return;
        };
        let mut found = self.compositor.keycode_for_keysym(keysym);
        if found.is_none() && shift {
            found = self.compositor.keycode_for_keysym(lowercase_of(keysym));
        }
        let Some(keycode) = found else {
            tracing::debug!(key = name, "the on-screen keyboard found no keycode for it");
            return;
        };
        let mut held: Vec<u32> = [
            ctrl.then_some(KEY_LEFT_CTRL),
            alt.then_some(KEY_LEFT_ALT),
            shift.then_some(KEY_LEFT_SHIFT),
        ]
        .into_iter()
        .flatten()
        .filter_map(|keysym| self.compositor.keycode_for_keysym(keysym))
        .collect();
        held.sort_unstable();
        held.dedup();
        for modifier in &held {
            self.send_key(*modifier, true);
        }
        self.send_key(keycode, true);
        self.send_key(keycode, false);
        for modifier in held.iter().rev() {
            self.send_key(*modifier, false);
        }
    }

    fn send_key(&mut self, keycode: u32, pressed: bool) {
        // The compositor refuses a key only while no field takes typing: the key had nowhere to go.
        if let Err(error) = self
            .compositor
            .input(tilo_compositor::InputEvent::Key { keycode, pressed })
        {
            tracing::warn!(%error, "a key the keyboard pressed reached no field");
        }
    }

    /// Stop the on-screen keyboard's view, if it runs.
    pub(crate) fn stop_keyboard(&mut self) {
        if self.part_views.running(PART) {
            self.stop_part_view(PART);
            self.parts.forget(PART);
        }
        self.part_views.dock(PART, None);
        if matches!(self.keyboard_run, Run::Running { .. }) {
            self.keyboard_run = Run::Down;
        }
        self.shared.keyboard.typed_by(None);
    }

    /// The display the person is working on, with its rectangle: the one holding the focused
    /// window, and the first display there is otherwise.
    fn keyboard_display(&self) -> Option<(String, (i32, i32, i32, i32))> {
        let displays: Vec<(String, (i32, i32, i32, i32))> = self
            .compositor
            .outputs()
            .into_iter()
            .filter(|output| output.enabled)
            .filter_map(|output| Some((output.name, output.rect?)))
            .collect();
        let focused = self
            .compositor
            .focused_window()
            .and_then(|focused| {
                self.compositor
                    .windows()
                    .into_iter()
                    .find(|(window, _)| window.id == focused.id)
                    .map(|(_, rect)| rect)
            })
            .map(|(x, y, w, h)| {
                (
                    i64::from(x).saturating_add(i64::from(w).saturating_div(2)),
                    i64::from(y).saturating_add(i64::from(h).saturating_div(2)),
                )
            });
        let holding = focused.and_then(|(cx, cy)| {
            displays
                .iter()
                .find(|(_, (x, y, w, h))| {
                    let (x, y) = (i64::from(*x), i64::from(*y));
                    let (w, h) = (i64::from(*w), i64::from(*h));
                    cx >= x && cx < x.saturating_add(w) && cy >= y && cy < y.saturating_add(h)
                })
                .cloned()
        });
        holding.or_else(|| displays.first().cloned())
    }
}

/// The keys the keyboard may press by name: the xkb keysym each stands for, and whether it needs
/// Shift held to land as the person's own layout would type it.
fn key_of(name: &str) -> Option<(u32, bool)> {
    Some(match name {
        "escape" => (0xff1b, false),
        "tab" => (0xff09, false),
        "backspace" => (0xff08, false),
        "return" | "enter" => (0xff0d, false),
        "space" => (0x0020, false),
        "left" => (0xff51, false),
        "right" => (0xff53, false),
        "up" => (0xff52, false),
        "down" => (0xff54, false),
        "home" => (0xff50, false),
        "end" => (0xff57, false),
        "page-up" => (0xff55, false),
        "page-down" => (0xff56, false),
        "delete" => (0xffff, false),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => (u32::from(ch), ch.is_ascii_uppercase()),
                _ => return None,
            }
        }
    })
}

/// The keysym of `keysym`'s lower-case letter, for a letter the layout reaches with Shift: a
/// layout that types `A` has the key code of `a`, and the Shift held over it types the capital.
fn lowercase_of(keysym: u32) -> u32 {
    char::from_u32(keysym)
        .map(|ch| ch.to_lowercase().next().map_or(keysym, u32::from))
        .unwrap_or(keysym)
}

const KEY_LEFT_SHIFT: u32 = 0xffe1;
const KEY_LEFT_CTRL: u32 = 0xffe3;
const KEY_LEFT_ALT: u32 = 0xffe9;

#[cfg(test)]
mod tests {
    use super::*;

    /// The arguments of `arguments` as the API hands them over.
    fn arguments(arguments: serde_json::Value) -> serde_json::Map<String, Value> {
        match arguments {
            Value::Object(map) => map,
            other => panic!("{other} is not an object"),
        }
    }

    tilo_test_env::scenario! {
        fn the_keyboards_actions_and_the_persons_word_are_kept_for_the_session;

        phase the_keyboard_s_actions_are_asked_of_the_session;
        // What the resource's actions ask is carried out on the session's pass, where the compositor
        // is: the resource only says what was asked.
        {
            let keyboard = Keyboard::default();
            let mut service = KeyboardService::new(keyboard.clone());
            service
                .invoke(
                    KEYBOARD,
                    "set-visible",
                    &arguments(serde_json::json!({ "visible": false })),
                )
                .expect("the keyboard shows nothing");
            service
                .invoke(
                    KEYBOARD,
                    "type-text",
                    &arguments(serde_json::json!({ "text": "hi" })),
                )
                .expect("the keyboard types");
            service
                .invoke(
                    KEYBOARD,
                    "preedit",
                    &arguments(serde_json::json!({ "text": "hel" })),
                )
                .expect("the keyboard composes");
            service
                .invoke(
                    KEYBOARD,
                    "press-key",
                    &arguments(serde_json::json!({ "name": "left", "ctrl": true })),
                )
                .expect("the keyboard presses a key");
            assert_eq!(
                keyboard.take_asks(),
                [
                    Ask::SetVisible(false),
                    Ask::TypeText("hi".into()),
                    Ask::Preedit("hel".into()),
                    Ask::PressKey {
                        name: "left".into(),
                        ctrl: true,
                        alt: false,
                        times: 1,
                    },
                ],
                "all of them wait for the loop that has the compositor"
            );
        }

        phase the_persons_word_on_the_keyboard_stands_over_that_field;
        // The person's word stands while the field it was said over is there, and the automatic rule
        // decides again once they leave it or reach one.
        {
            let keyboard = Keyboard::default();
            assert_eq!(keyboard.asked(), None, "the rule decides until they say");
            keyboard.say(false, true);
            assert_eq!(keyboard.asked(), Some(false));
            keyboard.forget_ask_when_the_field_moved(true);
            assert_eq!(
                keyboard.asked(),
                Some(false),
                "the same field keeps their word"
            );
            keyboard.forget_ask_when_the_field_moved(false);
            assert_eq!(
                keyboard.asked(),
                None,
                "the field they were in is left: the rule decides again"
            );
        }

        phase the_reading_carries_what_the_field_asks_of_typing;
        // What the field asks reaches a keyboard as the resource's reading: the layout its purpose
        // picks, the hints it gave, and whether what is typed there is secret, so an app draws the
        // right keys and neither suggests from nor learns a password.
        {
            let keyboard = Keyboard::default();
            let mut service = KeyboardService::new(keyboard.clone());
            keyboard.show(Shown {
                visible: true,
                display: Some("DP-1".into()),
                field: 7,
                purpose: "password".into(),
                hints: 8,
                secret: true,
                ..Shown::default()
            });
            assert!(service.poll(), "the reading moved");
            let read = service
                .read(KEYBOARD)
                .expect("the session publishes its keyboard");
            assert_eq!(read["visible"], Value::Bool(true));
            assert_eq!(read["display"], Value::String("DP-1".into()));
            assert_eq!(read["field"], Value::from(7));
            assert_eq!(read["purpose"], Value::String("password".into()));
            assert_eq!(read["hints"], Value::from(8));
            assert_eq!(read["secret"], Value::Bool(true));
        }

        phase only_the_app_the_session_runs_as_the_keyboard_types;
        // The field in front is another app's: an app types into it only while the session runs it
        // as the keyboard, and the person, or an agent acting for them, types as they would.
        {
            let keyboard = Keyboard::default();
            let mut service = KeyboardService::new(keyboard.clone());
            let typing = arguments(serde_json::json!({ "text": "secret" }));
            assert!(
                service
                    .invoke_for("org.example.Spy", "ws", KEYBOARD, "type-text", &typing)
                    .is_err(),
                "an app that is not the keyboard types nothing"
            );
            keyboard.typed_by(Some("so.tilo.osk"));
            assert!(
                service
                    .invoke_for("org.example.Spy", "ws", KEYBOARD, "type-text", &typing)
                    .is_err(),
                "nor while another app is the keyboard"
            );
            service
                .invoke_for("so.tilo.osk", "ws", KEYBOARD, "type-text", &typing)
                .expect("the keyboard types");
            service
                .invoke(KEYBOARD, "press-key", &arguments(serde_json::json!({ "name": "tab" })))
                .expect("the person presses a key");
            keyboard.typed_by(None);
            assert!(
                service
                    .invoke_for("so.tilo.osk", "ws", KEYBOARD, "type-text", &typing)
                    .is_err(),
                "a keyboard the session stopped types nothing"
            );
            assert_eq!(
                keyboard.take_asks(),
                [
                    Ask::TypeText("secret".into()),
                    Ask::PressKey {
                        name: "tab".into(),
                        ctrl: false,
                        alt: false,
                        times: 1,
                    },
                ],
            );
        }

        phase another_resource_s_action_is_refused;
        // The keyboard reads what it shows, and only that: an action of another resource is refused.
        {
            let mut service = KeyboardService::new(Keyboard::default());
            assert!(
                service
                    .invoke("session:input", "type-text", &serde_json::Map::new())
                    .is_err(),
                "the input resource has no keyboard action"
            );
        }
    }
}

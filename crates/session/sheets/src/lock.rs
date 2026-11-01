//! The lock screen: the session's own, shown on every display while the
//! session is locked, over nothing (the compositor draws no window or other
//! surface then, and each display shows the screen scaled to its own size).
//!
//! It is one quiet screen: the time and its date in poster type, whose
//! session this is, and the one password field that unlocks it. Typing the
//! password and unlocking are the app's effects, so a person, an assistive
//! tool and a headless run reach the same door. The password never leaves the
//! seat's zeroizing buffer except to the session, which checks it and either
//! lifts the lock or says it was wrong.
//!
//! The screen runs as its own process: the session publishes what it shows as
//! a resource of its own ([`Shown`]), the app reads it and asks through the
//! same actions as any other app, and what the screen asks of the session
//! arrives as its effects.
//!
//! Everything a device says about itself — the network, the battery, the
//! keyboard layout — arrives as plain data in [`Shown`], so the screen reads
//! the same in a screenshot as on a laptop. What it can do beyond unlocking
//! (switch layout, suspend, restart, shut down) it asks the session for as
//! the same published effects anything else uses, and offers only what the
//! session says it allows ([`Status`]).
//!
//! It moves the way the rest of the toolkit does: the ground is the shared
//! authentication artwork ([`ui::auth_backdrop`]), the screen arrives on
//! [`Appear`], a refusal shakes it on [`Attention`], and a while without
//! input fades it back to that ground until someone touches the keyboard or
//! the pointer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ui::LocalStateWriter;
use ui::advanced::WindowEvent;
use ui::caps_lock::CapsLockEstimate;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::motion::{
    Appear, Attention, Direction, MotionSpec, MotionValue, Pace, Presence, Reduced, Shortcut,
    frame_now, want_frames_until,
};
use ui::passcode_keyboard::{PasscodeInput, PasscodeKey, PasscodeKeyboard};
use ui::prelude::*;
use ui::text_render::{NumericStyle, OpenTypeTag, TextWeight};
use ui::tokens::{EffectiveMotion, MotionSource, Rhythm, Spacing};
use ui::widget::{
    AccessoryAction, Avatar, AvatarSize, ControlSize, FloatingPanel, IconSize, IconWidget, Manual,
    MeasureConstraint, Spinner, Tone,
};
use zeroize::{Zeroize, Zeroizing};

use crate::Seat as _;
use crate::presentation::Presentation;
use crate::t;

/// The app the session mounts the lock screen as.
pub const APP_ID: &str = "so.tilo.lock";

/// The password field's id, which the stage finds it by.
const PROMPT_ID: &str = "lock-password";

/// The clock's size in logical pixels: content-sized like the greeter's, so
/// it reads as one line at any window size rather than following the detail
/// text scale.
const CLOCK_SIZE: f32 = 88.0;

/// The clock's weight: the light end of the variable face, as the greeter
/// sets its own.
const CLOCK_WEIGHT: f32 = 280.0;

/// The card's width: one large field with its inline arrow, and a name
/// beside an avatar, under the clock.
const CARD_WIDTH: f32 = 400.0;

/// How long the screen waits for input before it goes quiet.
const IDLE: Duration = Duration::from_secs(20);

/// What the screen's own motion follows. Whatever put the lock up — a key, a
/// timer, an agent — the person in front of it watches it come and go, so it
/// moves as a surface they opened does instead of landing like a shortcut's
/// result. Input into it is still the person's own: typing and unlocking
/// never wait on a motion.
const WATCHED: MotionSource = MotionSource::Pointer;

/// The screen going quiet and coming back: a slow fade either way, a plain
/// cross-fade under reduced motion, and no motion at all for a shortcut.
const BLANK: MotionSpec = MotionSpec {
    enter: Pace::Base,
    exit: Pace::Slow,
    reduced: Reduced::Fades,
    shortcut: Shortcut::Lands,
};

/// The lock screen's resource: what it shows and what it asks. The session
/// publishes it as its own, and the screen's process reaches it as it reaches
/// any other app's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct LockScreen {}

#[resource(kind = "lock.screen", label = "Lock screen")]
impl LockScreen {}

/// What can be done on the lock screen.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Effect)]
#[effect(operation = "so.tilo.lock.screen")]
pub enum LockEffect {
    /// Type the password
    #[effect(own_device)]
    TypePassword {
        #[param(secret, "The password field's whole value")]
        password: String,
    },
    /// Unlock with the password typed
    #[effect(own_device)]
    Unlock,
    /// Switch to the next keyboard layout
    #[effect(own_device, keywords = "keyboard, layout, language, input")]
    NextLayout,
    /// Ask for the on-screen keyboard
    #[effect(own_device, keywords = "keyboard, on-screen, accessibility, touch")]
    ShowKeyboard,
    /// Play or pause what is playing
    #[effect(own_device, keywords = "music, media, pause, resume")]
    PlayPause,
    /// Play the next song
    #[effect(own_device, keywords = "music, media, skip")]
    NextTrack,
    /// Play the previous song
    #[effect(own_device, keywords = "music, media, skip")]
    PreviousTrack,
    /// Suspend the computer
    #[effect(own_device, keywords = "sleep, standby")]
    Suspend,
    /// Restart the computer
    #[effect(own_device, destructive, keywords = "reboot, reset")]
    Restart,
    /// Shut the computer down
    #[effect(own_device, destructive, keywords = "shutdown, power off")]
    ShutDown,
    /// The screen has finished leaving: the lock may lift
    #[effect(own_device)]
    Dissolved,
}

/// What plays, as the screen says it: the song and what the player in front
/// can be asked to do about it. A locked screen shows no more than that —
/// no queue, no scrubber, no choice of player.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent capability the player reports"
)]
pub struct Media {
    /// The song's name.
    pub title: String,
    /// Who it is by.
    pub subtitle: String,
    /// Whether it plays now, which is what the middle control offers to
    /// change.
    pub playing: bool,
    /// Whether the player can be paused and resumed.
    pub can_toggle: bool,
    /// Whether the player has a song before this one.
    pub can_previous: bool,
    /// Whether the player has a song after this one.
    pub can_next: bool,
}

/// The battery, as the screen says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Battery {
    /// How full it is, when the device says.
    pub percent: Option<u8>,
    /// Whether it is on the charger.
    pub charging: bool,
}

/// What the device says about itself, shown under the card.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Status {
    /// The network the person is on, when there is one.
    pub network: Option<String>,
    /// The battery, when the machine has one.
    pub battery: Option<Battery>,
    /// The keyboard layout in use, when the session names it.
    pub layout: Option<String>,
    /// The layout a press on the switch goes to, when there is one.
    pub next_layout: Option<String>,
    /// How many layouts there are: with more than one, the screen offers to
    /// switch.
    pub layouts: usize,
    /// Whether the session has an on-screen keyboard to ask for.
    pub keyboard: bool,
    /// Whether the session lets this screen suspend, restart or shut down.
    pub power: bool,
}

/// What the lock screen shows: plain data the session keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent fact each about what the screen shows and what the session lets it do"
)]
pub struct Shown {
    /// The time, as the person reads clocks.
    pub clock: String,
    /// The date under it.
    pub date: String,
    /// Whose session this is.
    pub person: String,
    /// Why the last password did not unlock.
    pub error: Option<String>,
    /// The password is being checked.
    pub checking: bool,
    /// Whether the keyboard's Caps Lock is on, as the typed letters say: the
    /// screen's own, which it fills in as it draws.
    pub caps_lock: bool,
    /// What the device says about itself.
    pub status: Status,
    /// What plays, while a player says.
    pub media: Option<Media>,
    /// The keyboard drawn on the screen is up, for unlocking by touch: the
    /// screen's own, which it fills in as it draws.
    pub keyboard: bool,
    /// The password was accepted: the screen is dissolving.
    pub unlocking: bool,
}

impl Shown {
    /// What the session published, as the screen draws it.
    ///
    /// # Errors
    /// The reading is not a lock's state: nothing of it is drawn.
    pub fn from_reading(reading: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(reading)
    }

    /// The line under the field: the answer to the last attempt, the check
    /// in flight, the keyboard's Caps Lock, or the quiet waiting hint.
    fn notice(&self) -> (String, Tone) {
        if let Some(error) = &self.error {
            (error.clone(), Tone::Danger)
        } else if self.checking {
            (t("lock-checking"), Tone::Neutral)
        } else if self.caps_lock {
            (t("lock-caps-lock"), Tone::Warning)
        } else {
            (t("lock-enter-hint"), Tone::Neutral)
        }
    }
}

/// What the screen's process is lent: what it shows, the password field's
/// buffer, the session's word on the lock, and how what it asks reaches the
/// session.
///
/// The screen is an app of its own, so nothing here crosses the two: each
/// reading the session publishes is written into the local the view draws, by
/// the worker that watches it ([`Self::reaching_session`]), and each effect
/// the screen binds goes out through the channel the app opened with
/// [`Self::ask_with`].
#[derive(Clone, Default)]
pub struct LockSeat {
    presentation: Presentation,
    shown: Rc<RefCell<Shown>>,
    local: Rc<RefCell<Option<LocalState<Shown>>>>,
    secret: Arc<Mutex<String>>,
    /// Where the screen's effects go: the session's resource, which takes
    /// them as the app's actions.
    outgoing: Rc<RefCell<Option<Sender<LockEffect>>>>,
    /// Where the app reaching the session is opened, handed the local its
    /// view draws.
    reaching: Rc<RefCell<Option<Reach>>>,
    /// The app was handed its local: reaching the session is done once.
    reached: Rc<Cell<bool>>,
    /// The screen was told it may go: it says so once.
    told_gone: Rc<Cell<bool>>,
}

/// How the app lending the seat reaches the session.
type Reach = Rc<dyn Fn(LocalStateWriter<Shown>)>;

impl std::fmt::Debug for LockSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockSeat")
            .field("presentation", &self.presentation)
            .field("shown", &self.shown.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for LockSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl LockSeat {
    /// Show `shown`, when it is not what shows already.
    pub fn show(&self, shown: Shown) {
        if self.shown() == shown {
            return;
        }
        if let Some(local) = self.local.borrow().as_ref() {
            local.update(|now| *now = shown.clone());
        }
        *self.shown.borrow_mut() = shown;
        self.presentation.wake();
    }

    /// What the lock screen shows now: the view's local once it draws one,
    /// which is where each reading of the session's lands.
    pub fn shown(&self) -> Shown {
        if let Some(local) = self.local.borrow().as_ref() {
            return local.get();
        }
        self.shown.borrow().clone()
    }

    /// The screen's effects go `outgoing` from here on: the app opened the
    /// session's resource and hands what the screen asks to it.
    pub fn ask_with(&self, outgoing: Sender<LockEffect>) {
        *self.outgoing.borrow_mut() = Some(outgoing);
    }

    /// The app lending this seat reaches the session as `reach` says, handed
    /// the local its view draws: each reading the session publishes is
    /// written there.
    pub fn reaching_session(&self, reach: impl Fn(LocalStateWriter<Shown>) + 'static) {
        *self.reaching.borrow_mut() = Some(Rc::new(reach));
    }

    /// Hand the app lending the seat its local, once, so what the session
    /// publishes reaches the screen.
    pub fn reach(&self, showing: LocalStateWriter<Shown>) {
        if self.reached.replace(true) {
            return;
        }
        let reaching = self.reaching.borrow().clone();
        if let Some(reaching) = reaching {
            reaching(showing);
        }
    }

    /// Hand what the field holds to the session, which checks it and says
    /// what came of it; an empty field gives nothing.
    fn submit(&self) -> Result<(), String> {
        let password = self
            .secret
            .lock()
            .map(|held| Zeroizing::new(held.clone()))
            .map_err(|_| no_password())?;
        if password.is_empty() {
            return Err(no_password());
        }
        self.wipe();
        self.ask(LockEffect::TypePassword {
            password: password.to_string(),
        });
        self.ask(LockEffect::Unlock);
        // What was typed is in the session's hands now; the keyboard has
        // done its work.
        self.show(Shown {
            error: None,
            checking: true,
            keyboard: false,
            ..self.shown()
        });
        Ok(())
    }

    /// The screen finished leaving: the session may lift the lock.
    fn gone(&self) {
        if self.told_gone.replace(true) {
            return;
        }
        self.ask(LockEffect::Dissolved);
    }

    /// What the password field holds, for the view to build the field with
    /// after a rebuild.
    fn secret_text(&self) -> String {
        self.secret
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }

    fn set_caps(&self, on: bool) {
        if self.shown.borrow().caps_lock == on {
            return;
        }
        self.show(Shown {
            caps_lock: on,
            ..self.shown()
        });
    }

    fn type_password(&self, password: &str) {
        if let Ok(mut held) = self.secret.lock() {
            held.zeroize();
            held.push_str(password);
        }
    }

    /// Hand `effect` to the session: the app opened its resource, and what
    /// the screen asked is an action on it like any other.
    fn ask(&self, effect: LockEffect) {
        if let Some(outgoing) = self.outgoing.borrow().as_ref() {
            let _ = outgoing.send(effect);
        }
    }

    fn wipe(&self) {
        if let Ok(mut held) = self.secret.lock() {
            held.zeroize();
        }
    }
}

/// What the lock screen says when a password did not unlock.
#[must_use]
pub fn wrong_password() -> String {
    t("lock-wrong-password")
}

/// What it says when the password could not be checked.
#[must_use]
pub fn unavailable() -> String {
    t("lock-unavailable")
}

/// What it says when there is no password to unlock with.
#[must_use]
pub fn no_password() -> String {
    t("lock-no-password")
}

/// The lock screen as an app.
pub struct LockApp;

impl ResourceApp for LockApp {
    type Resource = LockScreen;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Lock screen";
    const ICON: Option<&'static str> = Some("lock");
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<LockScreen>) -> Element {
        let seat = cx.take_lent::<LockSeat>().unwrap_or_default();
        let initial = cx
            .fixture::<Shown>()
            .map_or_else(|| seat.shown(), |shown| (*shown).clone());
        let shown = cx.local(initial);
        // What the session publishes is what the screen draws: the worker
        // that watches it writes each reading into the local the view draws.
        *seat.local.borrow_mut() = Some(shown.clone());
        seat.reach(shown.writer());
        seat.presentation().wake_with(cx.waker());
        let acting = seat.clone();
        cx.effects(move |effect: LockEffect, _cx| {
            match effect {
                LockEffect::TypePassword { password } => {
                    let password = Zeroizing::new(password);
                    acting.type_password(&password);
                }
                LockEffect::Unlock => acting.submit()?,
                // The keyboard is the screen's own: opening it is nothing the
                // session has to do.
                LockEffect::ShowKeyboard => acting.show(Shown {
                    keyboard: true,
                    ..acting.shown()
                }),
                // The screen says it has left itself, once, as the
                // dissolve ends: see `Stage::advance`.
                LockEffect::Dissolved => {}
                asked => acting.ask(asked),
            }
            Ok(Effected::done())
        });
        let dispatch = cx.dispatcher();
        let hosting = seat.clone();
        let content = cx.switch(&shown, move |cx, shown| screen(cx, shown, &seat));
        Box::new(Stage {
            content,
            dispatch,
            seat: hosting,
            presence: Presence::closed(),
            shake: Attention::default(),
            blank: MotionValue::new(0.0),
            caps: CapsLockEstimate::new(),
            keyboard: Cell::new(PasscodeKeyboard::default()),
            keyboard_place: Cell::new((Rect::ZERO, 0.0, 0.0)),
            pointer: None,
            last_input: frame_now(),
            idle: Idle::Awake,
            seen_error: None,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "plain", "refused", "checking", "caps", "quiet", "playing", "minimal",
        ]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<LockScreen>) {
        let shown = Shown {
            clock: "10:42".to_owned(),
            date: ui::format::day_heading(ui::clock::today(), false),
            person: "Ada Lovelace".to_owned(),
            error: None,
            checking: false,
            caps_lock: false,
            status: Status {
                network: Some("Home".to_owned()),
                battery: Some(Battery {
                    percent: Some(82),
                    charging: false,
                }),
                layout: Some("US".to_owned()),
                next_layout: Some("DE".to_owned()),
                layouts: 2,
                keyboard: false,
                power: true,
            },
            media: None,
            keyboard: false,
            unlocking: false,
        };
        let shown = match fixture.scenario() {
            Some("refused") => Shown {
                error: Some(t("lock-wrong-password")),
                ..shown
            },
            Some("checking") => Shown {
                checking: true,
                ..shown
            },
            Some("caps") => Shown {
                caps_lock: true,
                ..shown
            },
            Some("playing") => Shown {
                media: Some(Media {
                    title: "Clair de lune".to_owned(),
                    subtitle: "Claude Debussy".to_owned(),
                    playing: true,
                    can_toggle: true,
                    can_previous: true,
                    can_next: true,
                }),
                ..shown
            },
            Some("minimal") => Shown {
                status: Status::default(),
                ..shown
            },
            _ => shown,
        };
        fixture.provide(shown);
    }
}

/// The clock, the card and what the device says, as one centred block.
fn screen(cx: &ViewCx<LockScreen>, shown: &Shown, seat: &LockSeat) -> Element {
    let time = Text::new(&shown.clock)
        .with_id("lock-clock")
        .with_content_size(CLOCK_SIZE)
        .with_font_variation(OpenTypeTag::WEIGHT, CLOCK_WEIGHT)
        .with_numeric_style(NumericStyle::TabularLining)
        .with_line_height(1.0)
        .with_spoken(&shown.clock);
    let date = Text::new(&shown.date)
        .with_id("lock-date")
        .with_role(TextRole::Subheading)
        .with_weight(TextWeight::Medium)
        .dimmed();
    let clock = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Tight)
        .push(centred(Box::new(time)))
        .push(centred(Box::new(date)));

    // The panel's own inset is the menu's: a card whose rows and field fill it
    // needs room of its own so nothing sits against the edge.
    let card = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_vertical_padding(Spacing::BASE.s4)
        .with_horizontal_padding(Spacing::BASE.s4, Spacing::BASE.s4)
        .with_rhythm(Rhythm::Section)
        .push(person(shown))
        .push(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(Rhythm::Tight)
                .push(Box::new(field(shown, seat)))
                .push(notice(shown)),
        ));
    let card = FloatingPanel::new(Box::new(card)).width(CARD_WIDTH);

    let mut blocks = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Group)
        .push(Box::new(clock))
        .push(centred(Box::new(card)));
    if let Some(transport) = transport(cx, shown) {
        blocks = blocks.push(centred(transport));
    }
    let blocks = blocks.push(status_row(cx, shown));

    Box::new(
        Manual::new(|layout| {
            let available = layout.available();
            let area = Rect::new(
                0.0,
                0.0,
                f64::from(available.width),
                f64::from(available.height),
            );
            let edge = Spacing::BASE.s6;
            let widest = (available.width - edge * 2.0).max(edge);
            let measured = layout.measure(
                0,
                ui::taffy::Size {
                    width: MeasureConstraint::AtMost(widest),
                    height: MeasureConstraint::Intrinsic,
                },
            )?;
            let width = f64::from(measured.size.width).min(f64::from(widest));
            let height = f64::from(measured.size.height).min(area.height());
            // A little above the middle: the block reads as one thing, and
            // what sits under the hand is the field rather than the clock.
            let centre = Point::new(area.center().x, area.height() * 0.46);
            layout.place(0, Rect::from_center_size(centre, (width, height)))?;
            Ok(available)
        })
        .child(Box::new(blocks)),
    )
}

/// What plays, and the controls the player in front allows: nothing the
/// song is behind, so a locked screen leaks no more of the session than its
/// name.
fn transport(cx: &ViewCx<LockScreen>, shown: &Shown) -> Option<Element> {
    let media = shown.media.as_ref()?;
    let song = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_intrinsic_width()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(
            Text::new(&media.title)
                .with_id("lock-media-title")
                .with_role(TextRole::Body)
                .strong(),
        ))
        .push(Box::new(
            Text::new(&media.subtitle)
                .with_id("lock-media-subtitle")
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    let actions = cx.actions();
    let mut controls = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Tight)
        .with_align_items(ui::taffy::AlignItems::Center);
    let toggle = if media.playing {
        (Icon::Pause, "lock-pause")
    } else {
        (Icon::Play, "lock-play")
    };
    for (offered, effect, icon, id, name) in [
        (
            media.can_previous,
            LockEffect::PreviousTrack,
            Icon::SkipBack,
            "lock-previous",
            "lock-previous",
        ),
        (
            media.can_toggle,
            LockEffect::PlayPause,
            toggle.0,
            toggle.1,
            toggle.1,
        ),
        (
            media.can_next,
            LockEffect::NextTrack,
            Icon::SkipForward,
            "lock-next",
            "lock-next",
        ),
    ] {
        let Some(binding) = actions.effect(&effect).filter(|_| offered) else {
            continue;
        };
        controls = controls.push(Box::new(
            Button::icon_only(id, icon, &t(name))
                .control_size(ControlSize::Small)
                .on_action(binding),
        ));
    }
    Some(Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(song))
            .push(Box::new(controls)),
    ))
}

/// The avatar and whose session this is.
fn person(shown: &Shown) -> Element {
    let avatar = Avatar::person(shown.person.clone(), shown.person.clone()).size(AvatarSize::Large);
    let name = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_intrinsic_width()
        .with_rhythm(Rhythm::Tight)
        .push(Box::new(
            Text::new(&shown.person).with_role(TextRole::Body).strong(),
        ))
        .push(Box::new(
            Text::new(&t("lock-welcome"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Related)
            .with_align_items(ui::taffy::AlignItems::Center)
            .with_justify(ui::taffy::JustifyContent::Center)
            .push(Box::new(avatar))
            .push(Box::new(name)),
    )
}

/// The one password field, with the arrow that submits it.
fn field(shown: &Shown, seat: &LockSeat) -> TextInput {
    // What is typed is the screen's at every keystroke, not at the next
    // frame: a reading of the session's may draw the screen again in
    // between, and the field it draws says what the person has typed so far.
    let typed = seat.clone();
    let mut field = TextInput::local()
        .with_id(PROMPT_ID)
        .with_secret(true)
        .with_placeholder("lock-password")
        .with_accessible_name(t("lock-password"))
        .control_size(ControlSize::Large)
        .with_enter_submits(true)
        .with_trailing_accessory(Icon::ArrowRight, &t("lock-unlock"), AccessoryAction::Submit)
        .with_external_buffer(Arc::clone(&seat.secret))
        .with_initial_value(&seat.secret_text())
        .on_change(move |text| typed.type_password(text))
        .with_read_only(shown.checking);
    // The field is where the keyboard goes: the screen has one thing to do.
    Widget::set_focus(&mut field, true);
    field
}

/// What the screen says under the field: the answer, the check in flight,
/// the keyboard's Caps Lock, or nothing to worry about yet.
fn notice(shown: &Shown) -> Element {
    let (line, tone) = shown.notice();
    let mut row = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Related)
        .with_align_items(ui::taffy::AlignItems::Center)
        .with_justify(ui::taffy::JustifyContent::Center);
    if shown.checking {
        row = row.push(Box::new(Spinner::for_text(TextRole::Caption)));
    }
    row = row.push(Box::new(
        Text::new(&line)
            .with_id("lock-notice")
            .with_role(TextRole::Caption)
            .tone(tone)
            .with_live(ui::a11y::Politeness::Polite),
    ));
    Box::new(row)
}

/// What the device says, and what the session lets this screen do about it.
fn status_row(cx: &ViewCx<LockScreen>, shown: &Shown) -> Element {
    let actions = cx.actions();
    let mut row = Row::new()
        .no_wrap()
        .with_rhythm(Rhythm::Sibling)
        .with_align_items(ui::taffy::AlignItems::Center)
        .with_justify(ui::taffy::JustifyContent::Center);
    if let Some(network) = &shown.status.network {
        row = row.push(status_item(Icon::Wifi, network));
    }
    if let Some(battery) = &shown.status.battery {
        let glyph = if battery.charging {
            Icon::BatteryCharging
        } else {
            Icon::Battery
        };
        let label = battery.percent.map_or_else(
            || t("lock-battery"),
            |percent| ui::format::percent(f64::from(percent) / 100.0),
        );
        row = row.push(status_item(glyph, &label));
    }
    if shown.status.layouts > 1 {
        let layout = shown.status.layout.clone().unwrap_or_default();
        if let Some(binding) = actions.effect(&LockEffect::NextLayout) {
            let switch = shown.status.next_layout.as_ref().map(|next| {
                crate::t_args(
                    "lock-switch-layout",
                    &[("layout", ui::i18n::ArgValue::Text(next.clone()))],
                )
            });
            let mut button = Button::new("lock-layout", &layout)
                .control_size(ControlSize::Small)
                .on_action(binding);
            if let Some(switch) = switch {
                // The layout is on the button; where a press leads is what a
                // screen reader hears and what the tooltip says.
                button = button.with_accessible_name(&switch).tooltip(switch);
            }
            row = row.push(Box::new(button));
        }
    }
    if shown.status.keyboard
        && let Some(binding) = actions.effect(&LockEffect::ShowKeyboard)
    {
        row = row.push(Box::new(
            Button::icon_only(
                "lock-keyboard",
                Icon::Keyboard,
                &t("lock-on-screen-keyboard"),
            )
            .control_size(ControlSize::Small)
            .on_action(binding),
        ));
    }
    if shown.status.power {
        for (effect, icon, name) in [
            (LockEffect::Suspend, Icon::Moon, "lock-suspend"),
            (LockEffect::Restart, Icon::RotateCcw, "lock-restart"),
            (LockEffect::ShutDown, Icon::Power, "lock-shut-down"),
        ] {
            let Some(binding) = actions.effect(&effect) else {
                continue;
            };
            row = row.push(Box::new(
                Button::icon_only(power_id(&effect), icon, &t(name))
                    .control_size(ControlSize::Small)
                    .on_action(binding),
            ));
        }
    }
    Box::new(row)
}

/// One quiet item of the status row: a glyph and what it stands for.
fn status_item(icon: Icon, label: &str) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_rhythm(Rhythm::Tight)
            .with_align_items(ui::taffy::AlignItems::Center)
            .push(Box::new(IconWidget::new(icon).sized(IconSize::Small).dim()))
            .push(Box::new(
                Text::new(label).with_role(TextRole::Caption).dimmed(),
            )),
    )
}

/// A child centred across the width it is given.
fn centred(child: Element) -> Element {
    Box::new(
        Row::new()
            .no_wrap()
            .with_justify(ui::taffy::JustifyContent::Center)
            .push(child),
    )
}

/// The stable id a power action's button is found by.
fn power_id(effect: &LockEffect) -> &'static str {
    match effect {
        LockEffect::Suspend => "lock-suspend",
        LockEffect::Restart => "lock-restart",
        LockEffect::ShutDown => "lock-shut-down",
        _ => "lock-power",
    }
}

/// The screen itself: the ground, the arrival, the refusal's shake, the quiet
/// after a while, and a keyboard that always goes where it should.
struct Stage {
    content: Element,
    dispatch: Dispatcher<LockScreen>,
    seat: LockSeat,
    presence: Presence,
    shake: Attention,
    /// `0` with the screen awake, `1` when it has gone quiet.
    blank: MotionValue<f64>,
    /// The running guess at the keyboard's Caps Lock, from the typed letters.
    caps: CapsLockEstimate,
    /// The screen's own keyboard, for unlocking by touch: the layer and
    /// shift its keys pressed it to, which are no change to what the screen
    /// shows.
    keyboard: Cell<PasscodeKeyboard>,
    /// Where the last paint put that keyboard, so a finger presses exactly
    /// the keys it saw.
    keyboard_place: Cell<(Rect, f64, f64)>,
    /// Where the pointer is, in window coordinates.
    pointer: Option<Point>,
    /// When the person last touched the keyboard or the pointer.
    last_input: ui::time::Instant,
    /// Where the screen stands in going quiet and coming back.
    idle: Idle,
    /// The refusal the skin has already shaken for.
    seen_error: Option<String>,
}

/// Where the screen stands in going quiet and coming back. The motion is
/// started from the paint, which is where the theme's pacing is known, so
/// input and deadlines only say what the next paint is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Idle {
    /// Awake, and counting down to going quiet.
    Awake,
    /// The deadline passed: the next paint starts the fade.
    Fading,
    /// Gone quiet: the next input brings the screen back.
    Quiet,
    /// Input arrived while quiet: the next paint fades the screen in.
    Waking,
}

impl Stage {
    /// Start the motions a change of what is shown asks for, and say when the
    /// screen has finished dissolving.
    fn advance(&mut self, now: ui::time::Instant, motion: EffectiveMotion) {
        let shown = self.seat.shown();
        // A screen drawn is a screen up: asking for its arrival every paint
        // is a no-op while it is, and is what brings it back with its motion
        // the next time the session locks.
        let open = !shown.unlocking;
        self.presence.show(open, Appear::SPEC, motion, WATCHED, now);
        self.ask_until();
        if shown.error != self.seen_error {
            if shown.error.is_some() {
                self.shake.refuse(motion, now);
                self.ask_until();
            }
            self.seen_error.clone_from(&shown.error);
        }
        if self.idle == Idle::Waking {
            self.idle = Idle::Awake;
            self.blank
                .set(0.0, BLANK.timing(Direction::Exit, motion, WATCHED), now);
            self.ask_until();
        }
        if self.idle == Idle::Fading {
            self.idle = Idle::Quiet;
            self.blank
                .set(1.0, BLANK.timing(Direction::Enter, motion, WATCHED), now);
            self.ask_until();
        }
        if shown.unlocking && !self.presence.is_moving(now) {
            self.seat.gone();
        }
    }

    /// Ask the frame pump for frames until every motion in flight rests.
    fn ask_until(&self) {
        let until = [self.presence.settles_at(), self.shake.settles_at()]
            .into_iter()
            .flatten()
            .max();
        if let Some(until) = until {
            want_frames_until(until);
        }
    }

    /// When the screen goes quiet unless the person touches it first.
    fn idle_deadline(&self) -> Option<ui::time::Instant> {
        (self.idle == Idle::Awake && !self.seat.shown().unlocking)
            .then(|| self.last_input.checked_add(IDLE))
            .flatten()
    }

    /// Bring the screen's keyboard up, on the layer it was left on.
    fn show_keyboard(&self) {
        self.seat.show(Shown {
            keyboard: true,
            ..self.seat.shown()
        });
    }

    /// A press at `point` on the keyboard the last paint placed: the key
    /// there is pressed, as a finger would.
    fn press_key(&mut self, point: Point) -> bool {
        let (rect, key_height, gap) = self.keyboard_place.get();
        let Some(pressed) = self.keyboard.get().key_at(rect, key_height, gap, point) else {
            return false;
        };
        self.press(pressed);
        true
    }

    /// Press one key of the screen's keyboard: a character, a layer or shift
    /// changes the keyboard, and everything else is typed into the field.
    fn press(&mut self, pressed: PasscodeKey) {
        let mut keyboard = self.keyboard.get();
        let typed = keyboard.press(pressed);
        self.keyboard.set(keyboard);
        if let Some(typed) = typed {
            self.type_input(&typed);
        }
    }

    /// Type what a key of the screen's keyboard asks into the one field, or
    /// unlock with what is there.
    fn type_input(&mut self, input: &PasscodeInput) {
        let checking = self.seat.shown().checking;
        let Some(field) = find_field(self.content.as_mut()) else {
            return;
        };
        match input {
            PasscodeInput::Text(text) => field.insert_at_cursor(text),
            PasscodeInput::Backspace => {
                let mut value = Zeroizing::new(field.value().to_owned());
                value.pop();
                field.set_text(&value);
            }
            PasscodeInput::Enter if !checking => self.dispatch.effect(&LockEffect::Unlock),
            PasscodeInput::Enter => {}
        }
    }
}

/// How tall a key of the screen's keyboard is and how far apart they stand,
/// as the sheet's other keys do.
fn key_metrics(theme: &ui::theme::Theme) -> (f64, f64) {
    (
        f64::from(theme.tokens.metrics.panel_row_height),
        f64::from(theme.tokens.spacing.s2),
    )
}

/// Where the keyboard stands on a screen of `bounds`: along the bottom edge,
/// no wider than a wide sheet so its keys stay a thumb's reach apart.
fn keyboard_rect(theme: &ui::theme::Theme, bounds: Rect) -> Rect {
    let (key_height, gap) = key_metrics(theme);
    let height = PasscodeKeyboard::height(key_height, gap);
    let margin = f64::from(Spacing::BASE.s4);
    let widest = (bounds.width() - margin * 2.0)
        .max(1.0)
        .min(f64::from(theme.tokens.metrics.panel_width_sheet) * 1.25);
    Rect::from_center_size(
        Point::new(bounds.center().x, bounds.y1 - margin - height / 2.0),
        (widest, height),
    )
}

/// The password field anywhere under `widget`, so its input can be reached
/// without the stage holding a copy of it.
fn find_field(widget: &mut dyn Widget) -> Option<&mut ui::widget::TextInput> {
    if widget.test_id() == Some(PROMPT_ID) {
        return widget
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<ui::widget::TextInput>());
    }
    widget.children_mut().into_iter().find_map(find_field)
}

impl ui::widget::Decorator for Stage {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::event::ElementState;
        use ui::winit::keyboard::{Key, NamedKey};

        // Where the pointer is, for a press on the screen's own keyboard.
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Some(Point::new(position.x, position.y));
            }
            WindowEvent::CursorLeft { .. } => self.pointer = None,
            _ => {}
        }
        let now = frame_now();
        // A typed letter is what says whether Caps Lock is on; nothing else
        // knows.
        let typed = match event {
            WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed => {
                if let Some(text) = key.text.as_deref() {
                    self.caps
                        .observe_text(text, ctx.modifiers.state().shift_key());
                    self.seat.set_caps(self.caps.on());
                }
                key.text.clone()
            }
            _ => None,
        };
        let press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: ui::winit::event::MouseButton::Left,
                ..
            }
        );
        let touched = typed.is_some()
            || matches!(
                event,
                WindowEvent::KeyboardInput { event: key, .. } if key.state == ElementState::Pressed
            )
            || press;
        if touched {
            self.last_input = now;
        }
        if self.idle == Idle::Quiet && touched {
            self.idle = Idle::Waking;
            // A press somewhere wakes the screen and goes no further; a key
            // that types something also lands in the field, so a password
            // typed into a dark screen is never lost to waking it.
            if typed.is_none() {
                *ctx.handled = true;
                return;
            }
        }
        // The screen's keyboard, where the last paint put it, is above
        // everything else: a press on a key types that key.
        if self.seat.shown().keyboard
            && press
            && let Some(point) = self.pointer
            && self.press_key(point)
        {
            *ctx.handled = true;
            return;
        }
        let escape = matches!(
            event,
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && key.logical_key == Key::Named(NamedKey::Escape)
        );
        if escape {
            if let Some(field) = find_field(self.content.as_mut()) {
                field.set_text("");
                field.set_cursor_pos(0);
                Widget::set_focus(field, true);
            }
            *ctx.handled = true;
            return;
        }
        self.content.handle_event(event, ctx);
        let checking = self.seat.shown().checking;
        if let Some(field) = find_field(self.content.as_mut()) {
            // Anything typed elsewhere belongs in the one field this screen
            // has: focus follows the keyboard.
            if !*ctx.handled
                && !checking
                && let Some(text) = typed.as_deref()
            {
                if !Widget::is_focused(field) {
                    Widget::set_focus(field, true);
                }
                field.insert_at_cursor(text);
                *ctx.handled = true;
            }
            let submit = field.take_submit();
            if submit && !checking {
                self.dispatch.effect(&LockEffect::Unlock);
            }
        }
        // A press on the screen's own ground brings the keyboard up: a finger
        // is unlocking, and there is no keyboard under it to type on. A press
        // the screen's controls took is theirs.
        if press && !*ctx.handled && !self.seat.shown().keyboard {
            self.show_keyboard();
            *ctx.handled = true;
            return;
        }
        // A letter typed on a real keyboard puts the screen's own away: the
        // one under the hands is what a person is using.
        if typed.is_some() && self.seat.shown().keyboard {
            self.seat.show(Shown {
                keyboard: false,
                ..self.seat.shown()
            });
        }
    }

    fn traps_focus(&self) -> bool {
        true
    }

    fn draw(
        &mut self,
        scene: &mut ui::advanced::Scene,
        transform: ui::advanced::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let now = frame_now();
        let motion = theme.motion();
        self.advance(now, motion);
        let bounds = self.content.get_rect();
        ui::auth_backdrop::draw(
            scene,
            theme,
            transform,
            bounds,
            ui::auth_backdrop::STILL_SECONDS,
        );
        let quiet = self.blank.get(now).clamp(0.0, 1.0);
        let pose = Appear::pose(self.presence.progress(now), bounds.center(), motion)
            .fade(ui::num::f32_of_f64(1.0 - quiet));
        let shake = self.shake.offset(now);
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine * Affine is kurbo's float matrix product: it cannot overflow or panic"
        )]
        let placement = transform * ui::advanced::Affine::translate((shake.x, shake.y));
        let content = &mut self.content;
        // The screen's own keyboard goes up with the screen it belongs to:
        // same arrival, same shake, same fade when it leaves.
        let showed = self.seat.shown().keyboard;
        let (key_height, gap) = key_metrics(theme);
        let place = keyboard_rect(theme, bounds);
        self.keyboard_place.set((place, key_height, gap));
        let keyboard = self.keyboard.get();
        pose.paint(scene, placement, bounds, |scene, xform| {
            content.draw(scene, xform, text_renderer, theme, presence);
            if showed {
                keyboard.paint(scene, xform, text_renderer, theme, (place, key_height, gap));
            }
        });
    }

    fn needs_animation_frame(&self) -> bool {
        let now = frame_now();
        self.presence.is_moving(now)
            || self.shake.is_moving(now)
            || self.blank.is_moving(now)
            || self.content.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.idle_deadline()
    }

    fn serve_deadline(&mut self, _now: ui::time::Instant) -> bool {
        self.idle = Idle::Fading;
        true
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        regions.push(self.content.get_rect());
    }

    /// The whole surface is one dialog. Its field and its controls are
    /// widgets of their own, so they are exported beneath it; the keyboard
    /// drawn on top never is — a screen reader types into the field and
    /// presses the same controls a finger does, without seeing the screen.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Dialog)
                .with_name(t("lock-title"))
                .with_description(t("lock-description"))
                .with_state(AccessState::new().modal(true)),
        )
    }
}

/// The app's definition, which the session mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(LockApp)
}

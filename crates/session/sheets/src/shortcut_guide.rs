//! The shortcut guide: how to move around the desktop with what is in the
//! person's hands, and where they change the keys.
//!
//! The guide follows the input the person is using: the keyboard's
//! navigation pairs and essentials, the controller's buttons, the
//! touchscreen's gestures. On the keyboard each navigation key can be
//! changed: pressing it takes the next chord as its new keys, and a changed
//! pair can go back to its defaults. [`State`] is that state machine, which
//! every host keeps and draws with [`sheet`]; [`keyboard_guide`] shapes the
//! table into what the guide shows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig, ShortcutConfigError};
use settings_store::keyboard_shortcuts::{ShortcutEntry, catalogue};
use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::input::{
    ControllerBinding, ControllerButton, InputBinding, KeyLegend, KeyboardBinding,
    KeyboardModifier, TouchGesture,
};
use ui::prelude::*;
use ui::widget::{Column, Container, Element, InputHint, ModalDialog, Row, Scrollable, Text};

use crate::keyboard_shortcuts::chord_text;
use crate::presentation::{Presentation, Seat, presented};
use crate::t;

/// The app the shell mounts the guide as.
pub const APP_ID: &str = "so.tilo.shortcut-guide";

/// The guide's resource: the door its actions are published on. It holds
/// nothing; the shortcut table is the person's settings.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ShortcutGuide {}

#[resource(kind = "shortcut-guide.guide", label = "Shortcut guide")]
impl ShortcutGuide {}

/// What the guide asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.shortcut-guide.edit")]
pub enum GuideEffect {
    /// Change a shortcut: the next chord pressed becomes its keys
    #[effect(own_device)]
    Listen {
        #[param("The shortcut to change, by its id")]
        action: String,
    },
    /// Put a pair of shortcuts back to their default keys
    #[effect(own_device)]
    Reset {
        #[param("The first shortcut of the pair, by its id")]
        first: String,
        #[param("The second shortcut of the pair, by its id")]
        second: String,
    },
    /// Close the shortcut guide
    #[effect(own_device, kind = "read")]
    Done,
}

/// The input the guide explains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Modality {
    /// The keyboard and the pointer.
    #[default]
    Keys,
    Controller,
    Touch,
}

/// One shortcut's current keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Its id, which [`GuideEffect::Listen`] names.
    pub action: String,
    /// What it does, read by assistive technology.
    pub label: String,
    pub keys: KeyboardBinding,
}

/// A navigation row: what it moves, and the shortcuts going each way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pair {
    pub label: String,
    pub previous: Shortcut,
    pub next: Shortcut,
    /// Either differs from its default, so the pair can be reset.
    pub changed: bool,
}

/// Which face button does what on the controller in the person's hands:
/// the families put Confirm and Back in different places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Faces {
    pub confirm: ControllerButton,
    pub cancel: ControllerButton,
    pub third: ControllerButton,
    pub fourth: ControllerButton,
}

impl Default for Faces {
    fn default() -> Self {
        Self {
            confirm: ControllerButton::South,
            cancel: ControllerButton::East,
            third: ControllerButton::West,
            fourth: ControllerButton::North,
        }
    }
}

/// Who answers what the person pressed on the guide.
pub type Ask = Rc<dyn Fn(GuideEffect)>;

/// What the guide shows: plain data the shell keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Guide {
    pub modality: Modality,
    /// The controller in the person's hands, when it is known, and its
    /// face buttons.
    pub controller: Option<ui::ControllerFamily>,
    pub faces: Faces,
    /// The navigation pairs, which can be changed.
    pub pairs: Vec<Pair>,
    /// The essentials, in their three groups (windows, search, capture):
    /// what each does and the keys that do it, any of them.
    pub essentials: Vec<Vec<(String, Vec<KeyboardBinding>)>>,
    /// What else to know about the keys.
    pub notes: Vec<String>,
    /// The shortcut whose new keys the next chord sets.
    pub listening: Option<String>,
    /// Why the last change was refused, or could not be kept.
    pub message: Option<String>,
}

/// What the shell lends the guide: whether it is up, what it shows, and
/// where its requests go.
#[derive(Clone, Default)]
pub struct GuideSeat {
    presentation: Presentation,
    guide: Rc<RefCell<Guide>>,
    shown: Rc<RefCell<Option<LocalState<Guide>>>>,
    asked: Rc<RefCell<Vec<GuideEffect>>>,
}

impl std::fmt::Debug for GuideSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuideSeat")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl crate::Seat for GuideSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl GuideSeat {
    /// Show `guide`, or the same guide as it stands now.
    pub fn show(&self, guide: Guide) {
        if *self.guide.borrow() == guide {
            return;
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.set(guide.clone());
        }
        *self.guide.borrow_mut() = guide;
    }

    /// What the guide shows now.
    pub fn guide(&self) -> Guide {
        self.guide.borrow().clone()
    }

    /// The requests made since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<GuideEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// What the guide waits for and what it was given while waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caught {
    /// The person left without choosing keys.
    Cancelled,
    /// The keys a shortcut takes.
    Chord(KeyChord),
}

/// What the guide changes of the desktop beyond its own table: the apps'
/// global shortcuts, which step aside for the table the desktop answers.
pub trait AppShortcuts {
    /// Answer `table` from now on.
    fn set_shortcut_table(&self, table: ShortcutConfig);
}

/// What the guide explains, what it waits for, and what it last showed, as
/// every host keeps it: the table, the shortcut whose chord is being taken,
/// why a change was refused, and the chords pressed.
#[derive(Default)]
pub struct State {
    table: Rc<RefCell<ShortcutConfig>>,
    apps: Option<Rc<dyn AppShortcuts>>,
    controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    up: Cell<bool>,
    listening: RefCell<Option<String>>,
    caught: RefCell<Vec<Caught>>,
    message: RefCell<Option<String>>,
    /// What the guide was last shaped from, and that guide.
    built: RefCell<Option<(GuideKey, Guide)>>,
}

/// The state a [`State`] shapes its guide from: a guide is built again only
/// when one of these changed.
type GuideKey = (
    ShortcutConfig,
    Option<String>,
    Option<String>,
    Option<ui::ControllerFamily>,
);

impl State {
    /// The guide's state over `table`, the table the desktop answers keys
    /// with; `apps` is told when a change reaches the apps' shortcuts, and
    /// `controller` is the controller the person last used, if any.
    #[must_use]
    pub fn new(
        table: Rc<RefCell<ShortcutConfig>>,
        apps: Option<Rc<dyn AppShortcuts>>,
        controller: Rc<Cell<Option<ui::ControllerFamily>>>,
    ) -> Self {
        Self {
            table,
            apps,
            controller,
            ..Self::default()
        }
    }

    /// Whether the guide is up.
    #[must_use]
    pub fn up(&self) -> bool {
        self.up.get()
    }

    /// The guide is up, or has gone: a guide that is gone waits for no chord
    /// and has nothing left to answer.
    pub fn set_up(&self, up: bool) {
        self.up.set(up);
        if !up {
            self.listening.replace(None);
            self.caught.borrow_mut().clear();
            self.message.replace(None);
        }
    }

    /// The person asked for the guide, or asked for it to go: whether it is up
    /// now.
    pub fn toggle(&self) -> bool {
        let up = !self.up.get();
        self.set_up(up);
        up
    }

    /// The shortcut whose new chord the next press is, while the guide waits
    /// for one.
    #[must_use]
    pub fn listening(&self) -> Option<String> {
        self.listening.borrow().clone()
    }

    /// Wait for a new chord for `action`.
    pub fn listen_for(&self, action: &str) {
        self.message.replace(None);
        self.listening.replace(Some(action.to_owned()));
    }

    /// Stop waiting for a chord: the guide stays up and takes its keys for
    /// nothing but itself.
    pub fn stop_listening(&self) {
        self.listening.replace(None);
    }

    /// Keep `caught` for the guide.
    pub fn catch(&self, caught: Caught) {
        self.caught.borrow_mut().push(caught);
    }

    /// What was caught since the guide last looked, oldest first.
    pub fn take_caught(&self) -> Vec<Caught> {
        std::mem::take(&mut *self.caught.borrow_mut())
    }

    /// Carry out what the person chose on the guide.
    pub fn effect(&self, effect: GuideEffect) {
        match effect {
            GuideEffect::Done => self.set_up(false),
            GuideEffect::Listen { action } => self.listen_for(&action),
            GuideEffect::Reset { first, second } => self.reset_to_defaults(&first, &second),
        }
    }

    /// Answer what was caught since the last pass: a chord the guide waited
    /// for becomes its shortcut's keys, and leaving the wait says nothing.
    pub fn follow(&self) {
        for caught in self.take_caught() {
            match caught {
                Caught::Cancelled => {
                    self.message.replace(None);
                }
                Caught::Chord(chord) => {
                    if let Some(action) = self.listening() {
                        self.bind(&action, &chord);
                    }
                }
            }
        }
    }

    /// The table the desktop answers keys with.
    #[must_use]
    pub fn table(&self) -> Rc<RefCell<ShortcutConfig>> {
        Rc::clone(&self.table)
    }

    /// Answer `table` from now on: the table the guide explains, the keys and
    /// the apps' shortcuts all move to it at once.
    pub fn set_table(&self, table: ShortcutConfig) {
        *self.table.borrow_mut() = table.clone();
        if let Some(apps) = &self.apps {
            apps.set_shortcut_table(table);
        }
    }

    /// The guide as it stands now, shaped again only when the table, the
    /// shortcut it waits for, why a change was refused or the controller in
    /// hand changed.
    #[must_use]
    pub fn shaped(&self) -> Guide {
        let shown: GuideKey = (
            self.table.borrow().clone(),
            self.listening(),
            self.message.borrow().clone(),
            self.controller.get(),
        );
        if let Some((built, guide)) = self.built.borrow().as_ref()
            && built == &shown
        {
            return guide.clone();
        }
        let mut guide = keyboard_guide(&shown.0);
        guide.listening.clone_from(&shown.1);
        guide.message.clone_from(&shown.2);
        // A controller in hand is explained by its own buttons, drawn with the
        // legends that controller prints.
        if let Some(family) = shown.3 {
            guide.modality = Modality::Controller;
            guide.controller = Some(family);
            guide.faces = faces(family);
        }
        *self.built.borrow_mut() = Some((shown, guide.clone()));
        guide
    }

    /// Give the shortcut `action` its keys: `chord`, once the table accepts it
    /// and the person's settings keep it.
    fn bind(&self, action: &str, chord: &KeyChord) {
        let Some(action) = ShortcutAction::from_id(action) else {
            return;
        };
        // A key that types a character needs a modifier: without one the
        // shortcut would run instead of the letter everywhere.
        if types_a_character(chord) {
            self.refuse(crate::word("guide-rebind-needs-modifier"));
            return;
        }
        let mut candidate = self.table.borrow().clone();
        if let Err(error) = candidate.set(action, chord.clone()) {
            self.refuse(match error {
                ShortcutConfigError::Conflict(other) => {
                    crate::words("guide-rebind-in-use", &[("action", &shortcut_name(other))])
                }
                ShortcutConfigError::ReservedSystemKey => crate::word("guide-rebind-reserved"),
                ShortcutConfigError::Fixed(id) => crate::words(
                    "guide-rebind-in-use",
                    &[("action", &fixed_shortcut_name(id))],
                ),
                ShortcutConfigError::InvalidChord => crate::word("guide-rebind-unsupported"),
                ShortcutConfigError::TakenByAction => crate::word("guide-rebind-taken-by-action"),
            });
            return;
        }
        self.accept(candidate, "guide-rebind-save-failed");
    }

    /// Put the pair `first` and `second` back on their default chords: both at
    /// once, so a default the pair shares among itself is free to take.
    fn reset_to_defaults(&self, first: &str, second: &str) {
        let (Some(first), Some(second)) = (
            ShortcutAction::from_id(first),
            ShortcutAction::from_id(second),
        ) else {
            return;
        };
        let defaults = ShortcutConfig::default();
        let mut candidate = self.table.borrow().clone();
        candidate.bindings.remove(&first);
        candidate.bindings.remove(&second);
        let restore = [first, second].into_iter().try_for_each(|action| {
            defaults
                .bindings
                .get(&action)
                .cloned()
                .ok_or(ShortcutConfigError::InvalidChord)
                .and_then(|chord| candidate.set(action, chord))
        });
        if let Err(error) = restore {
            self.refuse(match error {
                ShortcutConfigError::Conflict(other) => {
                    crate::words("guide-reset-in-use", &[("action", &shortcut_name(other))])
                }
                ShortcutConfigError::TakenByAction => crate::word("guide-reset-taken-by-action"),
                ShortcutConfigError::ReservedSystemKey
                | ShortcutConfigError::InvalidChord
                | ShortcutConfigError::Fixed(_) => crate::word("guide-reset-invalid"),
            });
            return;
        }
        self.accept(candidate, "guide-reset-save-failed");
    }

    /// Keep `candidate` as the table the desktop answers: saved first, so a
    /// table the person's settings cannot keep never runs, then shared with
    /// the keys, the apps' shortcuts and the guide, and the waiting ends.
    fn accept(&self, candidate: ShortcutConfig, on_save_failure: &str) {
        if candidate.save().is_err() {
            self.refuse(crate::word(on_save_failure));
            return;
        }
        self.set_table(candidate);
        self.listening.replace(None);
        self.message.replace(None);
    }

    /// Say why the last press changed nothing; the guide keeps waiting for the
    /// keys of the shortcut it asks about.
    fn refuse(&self, message: String) {
        self.message.replace(Some(message));
    }
}

/// Which face button carries each role on `family`, as the guide draws them.
#[must_use]
pub fn faces(family: ui::ControllerFamily) -> Faces {
    let [confirm, cancel, third, fourth] = family.faces();
    Faces {
        confirm,
        cancel,
        third,
        fourth,
    }
}

/// Whether `chord` is a key that types: a printable key with no modifier held
/// would run the shortcut instead of typing.
fn types_a_character(chord: &KeyChord) -> bool {
    chord.modifiers.is_empty() && (chord.key == "space" || chord.key.chars().count() == 1)
}

/// What the person calls the shortcut `action`, as the guide's own words say
/// it.
fn shortcut_name(action: ShortcutAction) -> String {
    crate::word(&action.label_key())
}

/// What the person calls the fixed shortcut `id`.
fn fixed_shortcut_name(id: &str) -> String {
    settings_store::keyboard_shortcuts::fixed_shortcut(id)
        .map_or_else(|| id.to_owned(), |fixed| crate::word(fixed.label_key))
}

/// The guide as an app.
pub struct ShortcutGuideApp;

impl ResourceApp for ShortcutGuideApp {
    type Resource = ShortcutGuide;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Shortcut guide";
    const ICON: Option<&'static str> = Some("keyboard");
    // Over the desktop: what is not the guide shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<ShortcutGuide>) -> Element {
        let seat = cx.take_lent::<GuideSeat>();
        let initial = match &seat {
            Some(seat) => seat.guide(),
            None => cx
                .fixture::<Guide>()
                .map(|guide| (*guide).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
        }
        let asking = seat.clone();
        let presentation = seat.as_ref().map(|seat| seat.presentation().clone());
        cx.effects(move |effect: GuideEffect, _cx| match &asking {
            Some(seat) => {
                if effect == GuideEffect::Done {
                    // Its own close: the card leaves the screen, and the shell tells it gone
                    // once nothing of it is drawn.
                    seat.presentation.close();
                }
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no desktop to change here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let ask: Ask = Rc::new(move |effect| dispatch.effect(&effect));
        cx.switch(&shown, move |_cx, guide| {
            sheet(guide, &ask, presentation.as_ref())
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["keyboard", "listening", "controller", "touch"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<ShortcutGuide>) {
        let guide = match fixture.scenario() {
            Some("controller") => Guide {
                modality: Modality::Controller,
                ..Guide::default()
            },
            Some("touch") => Guide {
                modality: Modality::Touch,
                ..Guide::default()
            },
            Some("listening") => Guide {
                listening: Some("focus-next-window".to_owned()),
                ..sample_keyboard_guide()
            },
            _ => sample_keyboard_guide(),
        };
        fixture.provide(guide);
    }
}

/// A keyboard guide as a fresh desktop has it, for captures.
fn sample_keyboard_guide() -> Guide {
    let keys =
        |key: &str| KeyboardBinding::new([KeyboardModifier::System], KeyLegend::from_name(key));
    let shortcut = |action: &str, label: &str, key: &str| Shortcut {
        action: action.to_owned(),
        label: label.to_owned(),
        keys: keys(key),
    };
    Guide {
        modality: Modality::Keys,
        controller: None,
        faces: Faces::default(),
        pairs: vec![
            Pair {
                label: "Windows".to_owned(),
                previous: shortcut("focus-previous-window", "Previous window", "Left"),
                next: shortcut("focus-next-window", "Next window", "Right"),
                changed: false,
            },
            Pair {
                label: "Strips".to_owned(),
                previous: shortcut("previous-strip", "Strip above", "Up"),
                next: shortcut("next-strip", "Strip below", "Down"),
                changed: true,
            },
        ],
        essentials: vec![
            vec![("Close window".to_owned(), vec![keys("W")])],
            vec![("Search".to_owned(), vec![keys("Space")])],
            vec![("Screenshot".to_owned(), vec![keys("S")])],
        ],
        notes: vec!["Super+1 to Super+9 jump to a strip".to_owned()],
        listening: None,
        message: None,
    }
}

/// The guide: a sheet over the desktop, Done or Escape to leave it. `ask`
/// answers what the person pressed, and `presentation` places the sheet where
/// the host draws it.
#[must_use]
pub fn sheet(guide: &Guide, ask: &Ask, presentation: Option<&Presentation>) -> Element {
    let done = Rc::clone(ask);
    let dismissed = Rc::clone(ask);
    Box::new(presented(
        presentation,
        ModalDialog::new(
            t("guide-title"),
            Scrollable::new_vertical().with_child(body(guide, ask)),
        )
        .with_test_id("shortcut-guide")
        .with_max_width(960.0)
        .primary_action(t("guide-done"), move || done(GuideEffect::Done))
        .on_dismiss(move || dismissed(GuideEffect::Done)),
    ))
}

fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption))
}

fn heading(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Subheading))
}

fn hint(binding: InputBinding) -> Element {
    Box::new(InputHint::new(binding))
}

fn body(guide: &Guide, ask: &Ask) -> Element {
    let gap = ui::tokens::Spacing::BASE.s3;
    let mut column = Column::new()
        .with_gap(gap)
        .push(caption(&t("guide-reopen-hint")));
    if guide.modality == Modality::Keys {
        // While a key listens, what to do next is one line and how to leave
        // it is a key hint beside it, never a glyph-joined sentence.
        let feedback = match (&guide.listening, &guide.message) {
            (Some(action), message) => {
                let label = guide
                    .pairs
                    .iter()
                    .flat_map(|pair| [&pair.previous, &pair.next])
                    .find(|shortcut| &shortcut.action == action)
                    .map_or_else(|| action.clone(), |shortcut| shortcut.label.clone());
                let text = message.clone().unwrap_or_else(|| {
                    crate::t_args(
                        "guide-press-new-shortcut",
                        &[("action", ui::i18n::ArgValue::Text(label))],
                    )
                });
                Some((text, true))
            }
            (None, message) => message.clone().map(|text| (text, false)),
        };
        if let Some((text, cancellable)) = feedback {
            let line: Element = Box::new(
                Text::new(&text)
                    .with_role(TextRole::Caption)
                    .with_id("guide-feedback"),
            );
            column = column.push(if cancellable {
                Box::new(
                    Row::new()
                        .with_gap(ui::tokens::Spacing::BASE.s4)
                        .push(line)
                        .push(Box::new(
                            Row::new()
                                .with_gap(ui::tokens::Spacing::BASE.s2)
                                .push(hint(InputBinding::Keyboard(KeyboardBinding {
                                    modifiers: Vec::new(),
                                    key: KeyLegend::Escape,
                                })))
                                .push(caption(&t("guide-listening-cancel"))),
                        )),
                )
            } else {
                line
            });
        }
    }
    let column = match guide.modality {
        Modality::Keys => keys(column, guide, ask),
        Modality::Controller => controller(
            column,
            guide.controller.unwrap_or(ui::ControllerFamily::Generic),
            guide.faces,
        ),
        Modality::Touch => touch(column),
    };
    Box::new(column)
}

/// The keyboard's navigation pairs, which can be changed, its essentials
/// and its notes.
fn keys(mut column: Column, guide: &Guide, ask: &Ask) -> Column {
    let gap = ui::tokens::Spacing::BASE.s2;
    column = column.push(heading(&t("guide-navigation-heading")));
    for pair in &guide.pairs {
        let key = |shortcut: &Shortcut| -> Element {
            let listen = Rc::clone(ask);
            let effect = GuideEffect::Listen {
                action: shortcut.action.clone(),
            };
            Box::new(
                Container::new()
                    .hug()
                    .interactive(format!("guide-key-{}", shortcut.action), move || {
                        listen(effect.clone());
                    })
                    .with_child(hint(InputBinding::Keyboard(shortcut.keys.clone()))),
            )
        };
        let mut row = Row::new()
            .with_gap(gap)
            .push(Box::new(
                Container::new()
                    .with_width(220.0)
                    .with_child(caption(&pair.label)),
            ))
            .push(key(&pair.previous))
            .push(caption("/"))
            .push(key(&pair.next));
        if pair.changed {
            let reset = Rc::clone(ask);
            let effect = GuideEffect::Reset {
                first: pair.previous.action.clone(),
                second: pair.next.action.clone(),
            };
            row = row.push(Box::new(
                ui::widget::Button::new(
                    &format!("guide-reset-{}", pair.previous.action),
                    &t("guide-reset"),
                )
                .on_own_device_effect(move || reset(effect.clone())),
            ));
        }
        column = column.push(Box::new(row));
    }
    column = column.push(heading(&t("guide-essentials-heading")));
    let titles = [
        "guide-group-windows",
        "guide-group-search",
        "guide-group-capture",
        "guide-published-heading",
    ];
    let mut groups = Row::new().with_gap(ui::tokens::Spacing::BASE.s6);
    for (index, rows) in guide.essentials.iter().enumerate() {
        let mut group = Column::new().with_gap(gap);
        if let Some(title) = titles.get(index) {
            group = group.push(heading(&t(title)));
        }
        for (label, alternatives) in rows {
            let mut keys = Row::new().with_gap(gap);
            if alternatives.is_empty() {
                keys = keys.push(caption(&t("guide-unassigned")));
            }
            for (alternative, binding) in alternatives.iter().enumerate() {
                if alternative > 0 {
                    keys = keys.push(caption("/"));
                }
                keys = keys.push(hint(InputBinding::Keyboard(binding.clone())));
            }
            group = group.push(caption(label)).push(Box::new(keys));
        }
        groups = groups.push(Box::new(group));
    }
    column = column
        .push(Box::new(groups))
        .push(caption(&t("guide-pointer")));
    for note in &guide.notes {
        column = column.push(caption(note));
    }
    column
}

/// The controller's buttons: the guide follows whichever controller is in
/// the person's hands, through the bindings' own legends.
fn controller(column: Column, family: ui::ControllerFamily, faces: Faces) -> Column {
    use ControllerButton as B;
    /// One row's buttons, and the second binding when it is a pair.
    type Legends = (Vec<B>, Option<Vec<B>>);
    let gap = ui::tokens::Spacing::BASE.s2;
    let one = |button: B| (vec![button], None);
    let both = |first: B, second: B| (vec![first], Some(vec![second]));
    let chord = |first: B, second: B| (vec![B::Guide, first], Some(vec![B::Guide, second]));
    let rows: [(&str, Legends); 13] = [
        ("guide-row-move-focus", both(B::DpadLeft, B::DpadRight)),
        ("guide-row-select", one(faces.confirm)),
        ("guide-row-back", one(faces.cancel)),
        ("guide-row-app-actions", one(faces.third)),
        ("guide-row-search", one(faces.fourth)),
        ("guide-row-tabs", both(B::LeftBumper, B::RightBumper)),
        (
            "guide-row-workspaces",
            both(B::LeftTrigger, B::RightTrigger),
        ),
        ("guide-overview", one(B::Select)),
        ("guide-row-this-guide", one(B::Start)),
        ("guide-row-windows", chord(B::DpadLeft, B::DpadRight)),
        ("guide-row-strips", chord(B::DpadUp, B::DpadDown)),
        (
            "guide-row-move-window",
            chord(B::LeftBumper, B::RightBumper),
        ),
        ("guide-row-quick-settings", one(B::Guide)),
    ];
    let mut column = column.push(caption(&crate::t_args(
        "guide-controller-intro",
        &[(
            "guide",
            ui::i18n::ArgValue::Text(B::Guide.label(family).to_owned()),
        )],
    )));
    for (label, (first, second)) in rows {
        let mut row = Row::new()
            .with_gap(gap)
            .push(Box::new(
                Container::new()
                    .with_width(220.0)
                    .with_child(caption(&t(label))),
            ))
            .push(hint(InputBinding::Controller(ControllerBinding::new(
                first,
            ))));
        if let Some(second) = second {
            row = row.push(caption("/")).push(hint(InputBinding::Controller(
                ControllerBinding::new(second),
            )));
        }
        column = column.push(Box::new(row));
    }
    column
}

/// The touchscreen's gestures.
fn touch(column: Column) -> Column {
    let gap = ui::tokens::Spacing::BASE.s2;
    let gestures = [
        (
            "guide-touch-focus-next-window",
            TouchGesture::SwipeLeft { fingers: 3 },
        ),
        (
            "guide-touch-focus-previous-window",
            TouchGesture::SwipeRight { fingers: 3 },
        ),
        (
            "guide-touch-strip-above",
            TouchGesture::SwipeUp { fingers: 3 },
        ),
        (
            "guide-touch-strip-below",
            TouchGesture::SwipeDown { fingers: 3 },
        ),
        (
            "guide-touch-previous-workspace",
            TouchGesture::SwipeUp { fingers: 4 },
        ),
        (
            "guide-touch-next-workspace",
            TouchGesture::SwipeDown { fingers: 4 },
        ),
        ("guide-touch-open-overview", TouchGesture::PinchIn),
        ("guide-touch-close-overview", TouchGesture::PinchOut),
    ];
    let mut column = column.push(caption(&t("guide-touch-intro")));
    for (label, gesture) in gestures {
        column = column.push(Box::new(
            Row::new()
                .with_gap(gap)
                .push(Box::new(
                    Container::new()
                        .with_width(220.0)
                        .with_child(caption(&t(label))),
                ))
                .push(hint(InputBinding::Touch(gesture))),
        ));
    }
    column
}

/// The keyboard guide for the shortcut table `table`: the navigation pairs
/// the person can change, the essentials with the chords answering now, and
/// what else to know about the keys.
///
/// Every row is a keyboard-shortcuts catalogue entry, so the guide, Settings
/// and the action door read the same names and the same live chords, and the
/// guide never promises a default the person has rebound.
#[must_use]
pub fn keyboard_guide(table: &ShortcutConfig) -> Guide {
    let defaults = ShortcutConfig::default();
    let entries = catalogue(table);
    Guide {
        modality: Modality::Keys,
        controller: None,
        faces: Faces::default(),
        pairs: NAVIGATION_ROWS
            .iter()
            .map(|(label, previous, next)| Pair {
                label: t(label),
                previous: shortcut(table, *previous),
                next: shortcut(table, *next),
                changed: [previous, next]
                    .into_iter()
                    .any(|action| table.bindings.get(action) != defaults.bindings.get(action)),
            })
            .collect(),
        essentials: essentials(table, &entries),
        notes: notes(table),
        listening: None,
        message: None,
    }
}

/// The navigation rows: what a row moves, and the pair going each way.
const NAVIGATION_ROWS: [(&str, ShortcutAction, ShortcutAction); 9] = {
    use ShortcutAction as A;
    [
        (
            "guide-row-windows",
            A::FocusPreviousWindow,
            A::FocusNextWindow,
        ),
        ("guide-row-tabs", A::PreviousTab, A::NextTab),
        ("guide-row-move-tab", A::AttachTabLeft, A::AttachTabRight),
        ("guide-row-maximize", A::MaximizeWindow, A::RestoreWindow),
        ("guide-row-strips", A::PreviousStrip, A::NextStrip),
        (
            "guide-row-workspaces",
            A::PreviousWorkspace,
            A::NextWorkspace,
        ),
        ("guide-row-move-group", A::MoveGroupLeft, A::MoveGroupRight),
        ("guide-row-resize", A::ResizeNarrower, A::ResizeWider),
        (
            "guide-row-move-group-strip",
            A::MoveGroupToPreviousStrip,
            A::MoveGroupToNextStrip,
        ),
    ]
};

/// The essentials in their three titled groups (windows and workspaces,
/// search and system, capture and display): the keyboard-shortcuts catalogue
/// entries of each, in reading order.
const ESSENTIALS: [&[&str]; 3] = [
    &[
        "fixed-switch-windows",
        "fixed-close-tab",
        // Moving between workspaces is already an editable pair above, so
        // this group names how to undo a close instead.
        ShortcutAction::ReopenClosedTab.id(),
        "fixed-new-window",
        "fixed-duplicate-tab",
    ],
    &[
        ShortcutAction::Search.id(),
        // A lone Super tap opens the Super surface, whose field is this
        // search and whose keys are the same; the keyboard map lists every
        // alias, the guide keeps one row per action.
        ShortcutAction::EmojiPicker.id(),
        ShortcutAction::WorkspaceOverview.id(),
        ShortcutAction::QuickSettings.id(),
        "fixed-strip-switcher",
        ShortcutAction::Lock.id(),
    ],
    &[
        "fixed-screenshot",
        "fixed-display-layout",
        "fixed-capture-screenshot",
        "fixed-open-terminal",
        "fixed-picture-in-picture",
        "fixed-save-recent-replay",
    ],
];

/// `action` as the guide shows it under `table`, unassigned when it holds no
/// chord.
fn shortcut(table: &ShortcutConfig, action: ShortcutAction) -> Shortcut {
    Shortcut {
        action: action.id().to_owned(),
        label: t(&action.label_key()),
        keys: table.bindings.get(&action).map_or_else(unassigned, keycaps),
    }
}

/// The keycap of a shortcut nobody holds.
fn unassigned() -> KeyboardBinding {
    KeyboardBinding::new([], KeyLegend::Character("—".to_owned()))
}

/// `chord` as the keycaps the guide draws: the keys the active layout
/// presses for it.
fn keycaps(chord: &KeyChord) -> KeyboardBinding {
    let (modifiers, key) =
        ui::keycaps::as_typed(chord.modifiers.iter().map(String::as_str), &chord.key);
    KeyboardBinding::from_keys(modifiers.iter().map(String::as_str).chain([key.as_str()]))
}

/// What else the guide tells the person about the keys, in reading order.
fn notes(table: &ShortcutConfig) -> Vec<String> {
    vec![strip_jump_note(table), app_tab_note()]
}

/// Going straight to a strip with the live chords of the first and the last
/// one: "Super+1 … Super+9" by default.
fn strip_jump_note(table: &ShortcutConfig) -> String {
    let chord = |action: ShortcutAction| {
        table
            .bindings
            .get(&action)
            .map_or_else(|| t("guide-unassigned-inline"), chord_text)
    };
    crate::t_args(
        "guide-strip-jump",
        &[
            ("first", chord(ShortcutAction::FocusStrip1).into()),
            ("last", chord(ShortcutAction::FocusStrip9).into()),
        ],
    )
}

/// The app tab chords: they reach the focused app first and change tabs only
/// where it leaves them unhandled, while the desktop's own tab shortcuts
/// always work. Written in the active layout's keycaps, so a German keyboard
/// reads `Strg`.
fn app_tab_note() -> String {
    let chord = |text: &str| {
        KeyChord::parse(text).map_or_else(|_| text.to_owned(), |chord| chord_text(&chord))
    };
    crate::t_args(
        "guide-app-tab-chords",
        &[
            ("next", chord("Ctrl+Tab").into()),
            ("previous", chord("Ctrl+Shift+Tab").into()),
            ("reopen", chord("Ctrl+Shift+T").into()),
        ],
    )
}

/// The essential groups: the platform's own, then what the apps published,
/// each action a row of its name and the chord that answers it.
fn essentials(
    table: &ShortcutConfig,
    entries: &[ShortcutEntry],
) -> Vec<Vec<(String, Vec<KeyboardBinding>)>> {
    let mut groups: Vec<Vec<(String, Vec<KeyboardBinding>)>> = ESSENTIALS
        .iter()
        .map(|group| {
            group
                .iter()
                .filter_map(|id| entries.iter().find(|entry| entry.id == *id))
                .map(|entry| {
                    (
                        t(&entry.label_key),
                        entry.chords.iter().map(keycaps).collect(),
                    )
                })
                .collect()
        })
        .collect();
    // A published action is no catalogue entry, so the guide would not
    // mention it otherwise.
    if !table.actions.is_empty() {
        groups.push(
            table
                .actions
                .iter()
                .map(|bound| (bound.label.clone(), vec![keycaps(&bound.chord)]))
                .collect(),
        );
    }
    groups
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ShortcutGuideApp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The essentials row called `label`.
    fn essential(guide: &Guide, label: &str) -> Vec<KeyboardBinding> {
        guide
            .essentials
            .iter()
            .flatten()
            .find(|(name, _)| name == label)
            .map(|(_, keys)| keys.clone())
            .expect("the guide names this essential")
    }

    #[test]
    fn the_guide_shows_the_chord_the_table_holds_now() {
        let table = ShortcutConfig::default();
        let search = t("shortcut-name-search");
        assert_eq!(
            essential(&keyboard_guide(&table), &search),
            vec![KeyboardBinding::new(
                [KeyboardModifier::System],
                KeyLegend::from_name("space")
            )]
        );

        let mut rebound = table;
        rebound
            .set(
                ShortcutAction::Search,
                KeyChord::parse("Ctrl+Alt+J").expect("a free chord"),
            )
            .expect("no other shortcut holds it");
        let guide = keyboard_guide(&rebound);
        assert_eq!(
            essential(&guide, &search),
            vec![KeyboardBinding::new(
                [KeyboardModifier::Control, KeyboardModifier::Alt],
                KeyLegend::from_name("j")
            )]
        );
        // What can be changed is the navigation pairs, and only a changed one
        // offers to go back.
        assert_eq!(guide.pairs.len(), NAVIGATION_ROWS.len());
        assert!(guide.pairs.iter().all(|pair| !pair.changed));
        assert_eq!(guide.modality, Modality::Keys);
        assert_eq!(guide.listening, None);
        assert_eq!(guide.message, None);
    }

    #[test]
    fn a_pair_whose_shortcut_holds_no_chord_reads_as_unassigned() {
        let mut table = ShortcutConfig::default();
        table.bindings.remove(&ShortcutAction::PreviousTab);
        let guide = keyboard_guide(&table);
        let pair = guide
            .pairs
            .iter()
            .find(|pair| pair.previous.action == "previous-tab")
            .expect("tabs are a navigation pair");
        assert_eq!(pair.previous.keys, unassigned());
        assert!(pair.changed, "no chord is not the default chord");
        assert_eq!(
            pair.next.keys,
            keycaps(&table.bindings[&ShortcutAction::NextTab])
        );
    }

    /// A published action is a row of its own: its name, and the chord beside
    /// it, laid out as key hints rather than joined into one string.
    #[test]
    fn a_published_action_is_a_row_of_its_name_and_its_chord() {
        let mut table = ShortcutConfig::default();
        assert_eq!(keyboard_guide(&table).essentials.len(), ESSENTIALS.len());
        table.actions.push(settings_store::hardware::ActionChord {
            chord: KeyChord::parse("Ctrl+Alt+D").expect("a free chord"),
            target: settings_store::hardware::ActionChordTarget {
                provider: "actions".to_owned(),
                resource: "deploy".to_owned(),
                action: "run".to_owned(),
            },
            label: "Deploy".to_owned(),
        });
        let guide = keyboard_guide(&table);
        let published = guide.essentials.last().expect("the published group");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, "Deploy");
        assert_eq!(published[0].1.len(), 1);
        assert_eq!(
            published[0].1[0],
            keycaps(&KeyChord::parse("Ctrl+Alt+D").unwrap())
        );
        assert!(
            guide.notes.iter().all(|note| !note.contains("Deploy")),
            "no note joins the published chord onto other copy"
        );
    }
}

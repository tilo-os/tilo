//! The shortcut guide: how to move around the desktop with what is in the
//! person's hands, and where they change the keys.
//!
//! The guide follows the input the person is using: the keyboard's
//! navigation pairs and essentials, the controller's buttons, the
//! touchscreen's gestures. On the keyboard each navigation key can be
//! changed: pressing it asks the shell to take the next chord as its new
//! keys, and a changed pair can go back to its defaults. The shell owns the
//! shortcut table and checks every change ([`GuideSeat`]).

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::input::{
    ControllerBinding, ControllerButton, InputBinding, KeyLegend, KeyboardBinding, TouchGesture,
};
use ui::prelude::*;
use ui::widget::{Column, Container, Element, InputHint, ModalDialog, Row, Scrollable, Text};

use crate::presentation::Presentation;
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
        let asking = seat;
        cx.effects(move |effect: GuideEffect, _cx| match &asking {
            Some(seat) => {
                if effect == GuideEffect::Done {
                    seat.presentation.gone();
                }
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no desktop to change here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        cx.switch(&shown, move |_cx, guide| sheet(guide, &dispatch))
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
    use ui::input::KeyboardModifier;
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

/// The guide: a sheet over the desktop, Done or Escape to leave it.
fn sheet(guide: &Guide, dispatch: &Dispatcher<ShortcutGuide>) -> Element {
    let done = dispatch.clone();
    let dismissed = dispatch.clone();
    Box::new(
        ModalDialog::new(
            t("guide-title"),
            Scrollable::new_vertical().with_child(body(guide, dispatch)),
        )
        .with_test_id("shortcut-guide")
        .with_max_width(960.0)
        .open(true)
        .primary_action(t("guide-done"), move || done.effect(&GuideEffect::Done))
        .on_dismiss(move || dismissed.effect(&GuideEffect::Done)),
    )
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

fn body(guide: &Guide, dispatch: &Dispatcher<ShortcutGuide>) -> Element {
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
        Modality::Keys => keys(column, guide, dispatch),
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
fn keys(mut column: Column, guide: &Guide, dispatch: &Dispatcher<ShortcutGuide>) -> Column {
    let gap = ui::tokens::Spacing::BASE.s2;
    column = column.push(heading(&t("guide-navigation-heading")));
    for pair in &guide.pairs {
        let key = |shortcut: &Shortcut| -> Element {
            let listen = dispatch.clone();
            let effect = GuideEffect::Listen {
                action: shortcut.action.clone(),
            };
            Box::new(
                Container::new()
                    .hug()
                    .interactive(format!("guide-key-{}", shortcut.action), move || {
                        listen.effect(&effect);
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
            let reset = dispatch.clone();
            let effect = GuideEffect::Reset {
                first: pair.previous.action.clone(),
                second: pair.next.action.clone(),
            };
            row = row.push(Box::new(
                ui::widget::Button::new(
                    &format!("guide-reset-{}", pair.previous.action),
                    &t("guide-reset"),
                )
                .on_click(move || reset.effect(&effect)),
            ));
        }
        column = column.push(Box::new(row));
    }
    column = column.push(heading(&t("guide-essentials-heading")));
    let titles = [
        "guide-group-windows",
        "guide-group-search",
        "guide-group-capture",
    ];
    let mut groups = Row::new().with_gap(ui::tokens::Spacing::BASE.s6);
    for (index, rows) in guide.essentials.iter().enumerate() {
        let mut group = Column::new().with_gap(gap);
        if let Some(title) = titles.get(index) {
            group = group.push(heading(&t(title)));
        }
        for (label, alternatives) in rows {
            let mut keys = Row::new().with_gap(gap);
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

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ShortcutGuideApp)
}

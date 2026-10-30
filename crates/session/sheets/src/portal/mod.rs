//! The questions the shell asks on an app's or a service's behalf: a
//! portal review, a choice of app, whether one app may use another's
//! action (with the draft it would run), which USB devices an app may use,
//! global shortcuts, a launcher, a window crossing into another workspace,
//! a drive, a running shortcut's step, administrator authentication and the
//! secrets a network needs.
//!
//! The shell holds every request and its one reply, decides what may be
//! answered and how, and tells the sheet what to show ([`Dialog`], plain
//! data through [`PortalSeat`]). The sheet presents it and asks: every
//! choice comes back as a [`PortalEffect`], and answering needs the person
//! (agents may decline, but never allow). Anything typed stays out of what
//! the shell is asked: typing (by key or by the `type` actions, whose text
//! is a secret parameter) goes to the shell through the seat, and a secret
//! prompt's fields are kept in buffers the shell owns and wipes.
//!
//! Keys: Up and Down (and Tab) move between the choices, Enter answers (or
//! turns to the next page of a review), Left and Right page a review,
//! Space ticks the row in focus, Escape declines. A shortcut row takes the
//! keys pressed with a modifier as its new binding, Backspace clears it.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{Element, SecretAnswer};

use crate::presentation::Presentation;
use crate::t;

mod fixtures;
mod keys;
mod view;

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.portal-sheet";

/// The sheet's resource: the door its answers are published on. It holds
/// nothing; the requests are the shell's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct PortalSheet {}

#[resource(kind = "portal-sheet.question", label = "System question")]
impl PortalSheet {}

/// What the sheet asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.portal-sheet.answer")]
pub enum PortalEffect {
    /// Highlight one of the sheet's choices
    #[effect(own_device, kind = "read")]
    Select {
        #[param("The choice, counted from zero")]
        index: u32,
    },
    /// Tick or untick one of the sheet's rows: a device, or a yes-or-no field of a draft
    #[effect(own_device)]
    Toggle {
        #[param("The row, counted from zero")]
        index: u32,
    },
    /// Tick or untick Remember
    #[effect(own_device)]
    Remember,
    /// Choose the icon a launcher shows
    #[effect(own_device)]
    Icon {
        #[param("The icon, counted from zero")]
        index: u32,
    },
    /// Bind the highlighted shortcut to keys
    #[effect(own_device)]
    Bind {
        #[param("The keys as pressed, modifiers first, for example CTRL+SHIFT+KEYK")]
        keys: String,
    },
    /// Clear the highlighted shortcut
    #[effect(own_device)]
    Unbind,
    /// Turn to the next page of the review
    #[effect(own_device, kind = "read")]
    Next,
    /// Turn back a page of the review
    #[effect(own_device, kind = "read")]
    Back,
    /// Answer the question with what the sheet shows
    #[effect(own_device, attended)]
    Confirm,
    /// Answer the question with one of its answers
    #[effect(own_device, attended)]
    Answer {
        #[param("The answer, counted from zero")]
        index: u32,
    },
    /// Type into one of the sheet's fields: a launcher's name, a step's
    /// answer or a draft's text
    #[effect(own_device)]
    Type {
        #[param("name, answer, or draft-N for the draft's field N")]
        field: String,
        #[param(secret, "The field's whole text")]
        text: String,
    },
    /// Type into one of the secret prompt's fields
    #[effect(own_device)]
    TypeSecret {
        #[param("The field, counted from zero")]
        field: u32,
        #[param(secret, "The field's whole value")]
        value: String,
    },
    /// Answer with what was typed into the secret prompt
    #[effect(own_device, attended)]
    Submit,
    /// Decline the question
    #[effect(own_device, idempotent)]
    Cancel,
}

/// What the sheet shows: plain data the shell keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dialog {
    /// The request; a new one starts the sheet afresh.
    pub handle: String,
    pub title: String,
    /// Who asks, or what it is about (the app, the drive, the action).
    pub heading: String,
    /// What the person reviews: this page of it, when it is paged.
    pub detail: String,
    /// A code to read out or compare, shown large above the detail: a
    /// pairing's passkey or PIN.
    pub code: Option<String>,
    pub body: Body,
    pub confirm_label: String,
    pub cancel_label: String,
    /// The highlighted choice.
    pub selected: usize,
    /// The page of a paged review, and how many there are.
    pub page: Option<(usize, usize)>,
    /// Why the last answer was refused.
    pub error: Option<String>,
}

impl Dialog {
    /// More of the review follows this page: Enter turns to it.
    pub fn reviewing_more(&self) -> bool {
        self.page
            .is_some_and(|(page, pages)| page.saturating_add(1) < pages)
    }

    /// How many choices Up and Down move between.
    fn choices(&self) -> usize {
        match &self.body {
            Body::Review | Body::Secret(_) => 0,
            Body::Choices(choices)
            | Body::Entry { choices, .. }
            | Body::Draft { choices, .. }
            | Body::Answers { choices, .. } => choices.len(),
            Body::Devices(devices) => devices.len(),
            Body::Shortcuts(rows) => rows.len(),
            Body::Launcher { icons, .. } => icons.len(),
        }
    }
}

/// What the sheet asks with, under its detail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Body {
    /// Nothing to choose: allow or not.
    #[default]
    Review,
    /// One of these.
    Choices(Vec<String>),
    /// Each choice is an answer of its own (the ways to open a drive), the
    /// suggested one first in line; with a Remember row when offered.
    Answers {
        choices: Vec<String>,
        suggested: usize,
        remember: Option<Remember>,
    },
    /// Devices, each ticked or not.
    Devices(Vec<Device>),
    /// Global shortcuts, each with its binding.
    Shortcuts(Vec<Shortcut>),
    /// A launcher's name and icon, where the person may change them.
    Launcher {
        /// The name, when it may be changed; it is typed into the sheet.
        name: Option<String>,
        icons: Vec<String>,
        /// Whether the icon may be changed.
        icon_editable: bool,
    },
    /// One typed answer, with choices above it when the step offers some.
    Entry {
        masked: bool,
        required: bool,
        choices: Vec<String>,
    },
    /// The draft of what would run, row by row on this page, and the
    /// allowing answers under it.
    Draft {
        rows: Vec<DraftRow>,
        choices: Vec<String>,
    },
    /// A password, a network's secrets or a SIM's PIN, asked with the one
    /// prompt every part of Tilo uses.
    Secret(Prompt),
}

/// The Remember row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Remember {
    pub label: String,
    pub on: bool,
}

/// A device the app asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    pub label: String,
    pub writable: bool,
    pub selected: bool,
}

/// A global shortcut the app asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shortcut {
    pub name: String,
    /// The keys it is bound to, as the person reads them.
    pub keys: Option<String>,
    /// Why the last keys pressed were not taken.
    pub status: Option<String>,
}

/// One field of a draft, on the page shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftRow {
    /// Its place in the draft.
    pub index: usize,
    pub label: String,
    pub value: DraftValue,
    /// The person may change it.
    pub editable: bool,
}

/// How a draft row shows its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftValue {
    Text {
        text: String,
        multiline: bool,
        number: bool,
        /// Shown hidden: the value is not the asker's to read.
        withheld: bool,
    },
    Flag(bool),
}

impl Default for DraftValue {
    fn default() -> Self {
        Self::Flag(false)
    }
}

/// A secret prompt as the shell asks it. Its values are never here: what
/// is typed stays in the buffers the seat lends ([`PortalSeat::secrets`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub icon: Option<ui::icons::Icon>,
    /// Who and what for, under the title.
    pub resource: String,
    pub fields: Vec<PromptField>,
    /// Who of several answers (polkit's administrators).
    pub choices: Option<(String, Vec<String>)>,
    /// A check row under the fields, and whether it starts ticked.
    pub option: Option<(String, bool)>,
    pub primary_label: String,
    /// Why the last answer was refused, and the field it was about.
    pub error: Option<(usize, String)>,
    /// What the shell is doing with the answer; the prompt waits.
    pub busy: Option<String>,
}

/// Which prompt the fields make.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptKind {
    /// The fields as named.
    #[default]
    Fields,
    /// A SIM's PIN.
    SimPin { attempts_left: Option<u32> },
    /// A SIM's PUK and a new PIN.
    SimPuk { attempts_left: Option<u32> },
}

/// A field of a secret prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptField {
    pub label: String,
    pub masked: bool,
    pub required: bool,
    /// What an unmasked field starts with.
    pub initial: String,
}

/// A typed field of the sheet, whose text reaches the shell through the
/// seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    /// A launcher's name.
    Name,
    /// A step's typed answer.
    Answer,
    /// A draft's text field, by its place in the draft.
    Draft(usize),
}

/// Where the keyboard is in the sheet, besides the highlighted choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Focus {
    /// On the choices (or the buttons, when there are none).
    #[default]
    Choices,
    /// In a typed field.
    Field(Field),
    /// On a draft's yes-or-no row.
    Flag(usize),
    /// On the launcher's icons.
    Icons,
}

/// What the view shows: the dialog and where the keyboard is.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    dialog: Option<Dialog>,
    focus: Focus,
    /// Counts what was typed through the door, so the fields show it.
    typed: u64,
}

impl Shown {
    /// Where the keyboard starts on `dialog`: in its field, if it has one.
    fn first_focus(dialog: &Dialog) -> Focus {
        match &dialog.body {
            Body::Entry { .. } => Focus::Field(Field::Answer),
            Body::Launcher { name: Some(_), .. } => Focus::Field(Field::Name),
            Body::Launcher {
                icon_editable: true,
                ..
            } => Focus::Icons,
            Body::Draft { rows, .. } => rows
                .iter()
                .find(|row| row.editable)
                .or_else(|| rows.first())
                .map_or(Focus::Choices, draft_focus),
            _ => Focus::Choices,
        }
    }
}

/// Where the keyboard is on a draft row.
fn draft_focus(row: &DraftRow) -> Focus {
    match row.value {
        DraftValue::Flag(_) => Focus::Flag(row.index),
        DraftValue::Text { .. } => Focus::Field(Field::Draft(row.index)),
    }
}

/// What the shell lends the sheet: whether it is up, what it shows, what
/// was asked in it and what was typed into it.
#[derive(Clone, Default)]
pub struct PortalSeat {
    presentation: Presentation,
    dialog: Rc<RefCell<Option<Dialog>>>,
    shown: Rc<RefCell<Option<LocalState<Shown>>>>,
    asked: Rc<RefCell<Vec<PortalEffect>>>,
    texts: Rc<RefCell<BTreeMap<Field, zeroize::Zeroizing<String>>>>,
    secrets: Rc<RefCell<Vec<Arc<Mutex<String>>>>>,
    answer: Rc<RefCell<Option<SecretAnswer>>>,
    option: Rc<Cell<Option<bool>>>,
}

impl std::fmt::Debug for PortalSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // What was typed is never written out.
        f.debug_struct("PortalSeat")
            .field("presentation", &self.presentation)
            .field("dialog", &self.dialog.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for PortalSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl PortalSeat {
    /// Show `dialog`, or nothing; another request starts the sheet afresh
    /// and wipes what was typed into the last one.
    pub fn show(&self, dialog: Option<Dialog>) {
        if *self.dialog.borrow() == dialog {
            return;
        }
        let fresh =
            self.dialog.borrow().as_ref().map(|d| &d.handle) != dialog.as_ref().map(|d| &d.handle);
        if fresh {
            self.forget();
            if let Some(Dialog {
                body: Body::Secret(prompt),
                ..
            }) = &dialog
            {
                *self.secrets.borrow_mut() = (0..secret_fields(prompt))
                    .map(|_| Arc::new(Mutex::new(String::new())))
                    .collect();
            }
        }
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| {
                if fresh {
                    shown.focus = dialog.as_ref().map_or(Focus::Choices, Shown::first_focus);
                }
                shown.dialog.clone_from(&dialog);
            });
        }
        *self.dialog.borrow_mut() = dialog;
        self.presentation.wake();
    }

    /// Whether `other` is this same seat, lent again.
    pub fn is(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.dialog, &other.dialog)
    }

    /// What the sheet shows now.
    pub fn dialog(&self) -> Option<Dialog> {
        self.dialog.borrow().clone()
    }

    /// What was asked since the shell last looked, oldest first.
    pub fn take_asked(&self) -> Vec<PortalEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// What is typed into `field` now; empty when nothing is.
    pub fn text(&self, field: Field) -> zeroize::Zeroizing<String> {
        self.texts.borrow().get(&field).cloned().unwrap_or_default()
    }

    /// What is typed into `field`, once the sheet has shown it.
    pub fn typed(&self, field: Field) -> Option<zeroize::Zeroizing<String>> {
        self.texts.borrow().get(&field).cloned()
    }

    /// Put `text` into `field`, as typing does: for a host test, or an
    /// on-screen keyboard of the shell's own.
    pub fn set_text(&self, field: Field, text: &str) {
        self.texts
            .borrow_mut()
            .insert(field, zeroize::Zeroizing::new(text.to_owned()));
        self.presentation.wake();
    }

    /// The secret prompt's answer, once the person gave it: taken once,
    /// and the prompt's buffers wiped.
    pub fn take_answer(&self) -> Option<SecretAnswer> {
        let answer = self.answer.borrow_mut().take();
        if answer.is_some() {
            self.wipe_secrets();
        }
        answer
    }

    /// Hand the shell `answer` as the secret prompt does when the person
    /// answers it: for a host test, or a prompt of the shell's own.
    pub fn give_answer(&self, answer: SecretAnswer) {
        *self.answer.borrow_mut() = Some(answer);
    }

    /// Put a refused answer's values back into the prompt's fields, for
    /// the person to complete rather than retype.
    pub fn give_back(&self, answer: &SecretAnswer) {
        for (buffer, value) in self.secrets.borrow().iter().zip(&answer.values) {
            if let Ok(mut held) = buffer.lock() {
                zeroize::Zeroize::zeroize(&mut *held);
                held.push_str(value);
            }
        }
        self.presentation.wake();
    }

    /// Whether the secret prompt's check row is ticked now, once the
    /// person changed it.
    pub fn option(&self) -> Option<bool> {
        self.option.get()
    }

    /// The buffers the secret prompt keeps its fields in, one per field.
    pub fn secrets(&self) -> Vec<Arc<Mutex<String>>> {
        self.secrets.borrow().clone()
    }

    /// Forget everything typed into the sheet.
    pub fn forget(&self) {
        self.texts.borrow_mut().clear();
        self.answer.borrow_mut().take();
        self.option.set(None);
        self.wipe_secrets();
    }

    fn wipe_secrets(&self) {
        for buffer in self.secrets.borrow().iter() {
            if let Ok(mut value) = buffer.lock() {
                zeroize::Zeroize::zeroize(&mut *value);
            }
        }
    }

    /// Ask `effect` as a press in the sheet would: for a host test.
    pub fn press(&self, effect: PortalEffect) {
        self.asked.borrow_mut().push(effect);
    }

    /// Take `effect` from the sheet's door. Typing stays in the seat and
    /// is never asked; a secret prompt answered through the door is
    /// answered with what its buffers hold, checked as the prompt checks
    /// it.
    fn take(&self, effect: PortalEffect) -> Result<(), String> {
        let dialog = self.dialog();
        match effect {
            PortalEffect::Type { field, text } => {
                let field = dialog
                    .as_ref()
                    .and_then(|dialog| typed_field(dialog, &field))
                    .ok_or_else(|| t("portal-no-such-field"))?;
                self.texts
                    .borrow_mut()
                    .insert(field, zeroize::Zeroizing::new(text));
                self.retype();
                return Ok(());
            }
            PortalEffect::TypeSecret { field, value } => {
                let value = zeroize::Zeroizing::new(value);
                let buffer = matches!(
                    dialog,
                    Some(Dialog {
                        body: Body::Secret(_),
                        ..
                    })
                )
                .then(|| {
                    let at = usize::try_from(field).ok()?;
                    self.secrets.borrow().get(at).cloned()
                })
                .flatten()
                .ok_or_else(|| t("portal-no-such-field"))?;
                if let Ok(mut held) = buffer.lock() {
                    zeroize::Zeroize::zeroize(&mut *held);
                    held.push_str(&value);
                }
                self.retype();
                return Ok(());
            }
            PortalEffect::Submit => {
                if let Some(
                    shown @ Dialog {
                        body: Body::Secret(prompt),
                        ..
                    },
                ) = &dialog
                    && self.answer.borrow().is_none()
                {
                    let mut built = view::secret_prompt(shown, prompt, self);
                    built.submit();
                    match built.take_outcome() {
                        Some(ui::widget::PromptOutcome::Submit(answer)) => {
                            *self.answer.borrow_mut() = Some(answer);
                        }
                        _ => {
                            return Err(built
                                .error()
                                .map_or_else(|| t("portal-secret-incomplete"), str::to_owned));
                        }
                    }
                }
            }
            PortalEffect::Remember => {
                if let Some(Dialog {
                    body:
                        Body::Secret(Prompt {
                            option: Some((_, on)),
                            ..
                        }),
                    ..
                }) = &dialog
                {
                    self.option.set(Some(!self.option.get().unwrap_or(*on)));
                    self.retype();
                }
            }
            _ => {}
        }
        self.asked.borrow_mut().push(effect);
        Ok(())
    }

    /// Show again what was typed through the door.
    fn retype(&self) {
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| shown.typed = shown.typed.wrapping_add(1));
        }
        self.presentation.wake();
    }
}

/// How many secrets the sheet asks for `prompt`: one buffer is lent per
/// field, so this is what the view builds rather than what the question
/// listed. A SIM locked by wrong PINs asks for its PUK and the new PIN
/// typed twice, however the question words the two it reads back.
fn secret_fields(prompt: &Prompt) -> usize {
    match prompt.kind {
        PromptKind::SimPuk { .. } => 3,
        PromptKind::Fields | PromptKind::SimPin { .. } => prompt.fields.len(),
    }
}

/// The typed field `name` names on `dialog`, when it has it and the person
/// may change it.
fn typed_field(dialog: &Dialog, name: &str) -> Option<Field> {
    match (&dialog.body, name) {
        (Body::Launcher { name: Some(_), .. }, "name") => Some(Field::Name),
        (Body::Entry { .. }, "answer") => Some(Field::Answer),
        (Body::Draft { rows, .. }, name) => {
            let at: usize = name.strip_prefix("draft-")?.parse().ok()?;
            rows.iter()
                .find(|row| {
                    row.index == at && row.editable && matches!(row.value, DraftValue::Text { .. })
                })
                .map(|row| Field::Draft(row.index))
        }
        _ => None,
    }
}

/// The sheet as an app.
pub struct PortalApp;

impl ResourceApp for PortalApp {
    type Resource = PortalSheet;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "System question";
    const ICON: Option<&'static str> = Some("shield");
    // Over the desktop: the sheet's scrim shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: a question and its answer are the shell's.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<PortalSheet>) -> Element {
        let seat = cx.take_lent::<PortalSeat>();
        let seat = seat.unwrap_or_else(|| {
            // Headless, a scenario stands in for the shell.
            let seat = PortalSeat::default();
            if let Some(dialog) = cx.fixture::<Dialog>() {
                seat.show(Some((*dialog).clone()));
            }
            seat
        });
        let dialog = seat.dialog();
        let focus = dialog.as_ref().map_or(Focus::Choices, Shown::first_focus);
        let shown = cx.local(Shown {
            dialog,
            focus,
            typed: 0,
        });
        *seat.shown.borrow_mut() = Some(shown.clone());
        seat.presentation.wake_with(cx.waker());
        let asking = seat.clone();
        cx.effects(move |effect: PortalEffect, _cx| {
            asking.take(effect)?;
            Ok(Effected::done())
        });
        let parts = view::Parts {
            dispatch: cx.dispatcher(),
            shown: shown.clone(),
            seat,
        };
        let building = parts.clone();
        let content = cx.switch(&shown, move |_cx, shown| view::sheet(shown, &building));
        Box::new(keys::Keys { content, parts })
    }

    fn scenarios() -> &'static [&'static str] {
        fixtures::SCENARIOS
    }

    fn fixture(fixture: &mut ui::framework::Fixture<PortalSheet>) {
        let dialog = fixtures::dialog(fixture.scenario());
        fixture.provide(dialog);
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(PortalApp)
}

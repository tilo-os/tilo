//! The one password prompt: every secret Tilo asks for — a Wi-Fi
//! passphrase, a polkit password, a VPN login, an unlock, a SIM PIN — is
//! asked by this component, drawn the same by the shell and by apps.
//!
//! It shows an icon and a title naming who asks and why ("Connect to
//! TruffleBoys2026"), the resource line, the field or fields the kind asks
//! for (each a secret [`TextInput`] with its own show/hide button), one
//! notice line under them (an inline error after a wrong secret, Caps Lock,
//! the PIN length or the attempts a SIM has left), a numeric keypad for the
//! PIN kinds while touch is the input in use, and Cancel plus a primary
//! button labelled by the action (Connect, Unlock, Allow).
//!
//! Three presentations of the same body:
//!
//! - [`Presentation::Sheet`]: the modal sheet a painted host (the shell)
//!   draws itself, through the one modal chrome ([`super::modal_dialog`]):
//!   floating on a roomy display, a full-window page on a phone. Enter
//!   answers, Escape cancels, and the sheet keeps the field clear of an
//!   on-screen keyboard ([`SecretPrompt::set_keyboard_inset`]).
//! - [`Presentation::Inline`]: inside another surface (Quick Settings):
//!   the resource line, fields, notice and its own Cancel and primary
//!   buttons, no title or sheet.
//! - [`Presentation::DialogBody`]: the body of a retained
//!   [`ModalDialog`](super::ModalDialog), which draws the title and the
//!   actions; [`SecretPrompt::into_dialog`] wires them.
//!
//! The secret lives only in the fields' zeroizing buffers and in the
//! [`SecretAnswer`] handed out on submit, which zeroes itself on drop. It is
//! never in the accessibility tree (fields report `protected`), a log (the
//! answer's `Debug` is redacted), a document or replicated state (fields are
//! local, with no binding), or a screenshot (a painted secret marks the
//! frame through [`crate::secret_paint`], so hosts withhold it).
//!
//! ```ignore
//! // A painted host:
//! let mut prompt = SecretPrompt::new(SecretKind::Secret)
//!     .with_icon(Icon::Wifi)
//!     .with_title("Connect to TruffleBoys2026")
//!     .with_resource("WPA2 Personal")
//!     .with_primary_label("Connect");
//! prompt.set_focus(true);
//! prompt.place(window, text, theme);
//! prompt.paint(scene, transform, text, theme);
//! prompt.handle_event(event, ctx);
//! match prompt.take_outcome() {
//!     Some(PromptOutcome::Submit(answer)) => connect(answer.secret),
//!     Some(PromptOutcome::Cancel) => close(),
//!     None => {}
//! }
//! // …and when the network refuses it:
//! prompt.reject("Wrong password");
//! ```

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, MouseButton, TouchPhase, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::Zeroizing;

use crate::a11y::{AccessNode, AccessState, Politeness, Role};
use crate::caps_lock::CapsLockEstimate;
use crate::icons::Icon;
use crate::interaction::{InputModality, InteractionEnvironment, PointerPrecision};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::control_size::ControlSize;
use crate::widget::modal_dialog::{self, ModalAction, ModalLayout, ModalSpec};
use crate::widget::{
    Button, ButtonFace, ButtonRole, EventCtx, ModalDialog, ModelPath, TaffyTreeExt, TextInput,
    TextInputKind, Widget,
};

/// The most lines the line under the fields wraps to.
const NOTICE_MAX_LINES: usize = 3;

/// The digits a SIM's PUK has.
pub const PUK_DIGITS: usize = 8;

/// The lengths a SIM PIN may have.
pub const SIM_PIN_DIGITS: (usize, usize) = (4, 8);

/// What a prompt asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    /// One secret: a password or a passphrase.
    Secret,
    /// A user name and a secret: enterprise Wi-Fi, a VPN login, polkit
    /// for one of several administrators.
    UsernameAndSecret,
    /// Digits only, between `min` and `max` of them.
    Pin { min: usize, max: usize },
    /// A SIM's PIN, with the attempts the SIM has left when the modem says.
    SimPin { attempts_left: Option<u32> },
    /// A SIM locked after too many wrong PINs: its PUK and a new PIN, typed
    /// twice, with the PUK attempts left when the modem says.
    SimPuk { attempts_left: Option<u32> },
    /// The fields the asker names ([`SecretPrompt::with_fields`]): a VPN's
    /// user name, password and one-time code, a key's passphrase.
    Fields,
}

impl SecretKind {
    fn is_pin(self) -> bool {
        matches!(
            self,
            Self::Pin { .. } | Self::SimPin { .. } | Self::SimPuk { .. }
        )
    }

    fn attempts_left(self) -> Option<u32> {
        match self {
            Self::SimPin { attempts_left } | Self::SimPuk { attempts_left } => attempts_left,
            _ => None,
        }
    }

    /// The fields this kind asks for, in order.
    fn roles(self) -> &'static [FieldRole] {
        match self {
            Self::Secret => &[FieldRole::Secret],
            Self::UsernameAndSecret => &[FieldRole::Username, FieldRole::Secret],
            Self::Pin { .. } | Self::SimPin { .. } => &[FieldRole::Pin],
            Self::SimPuk { .. } => &[FieldRole::Puk, FieldRole::NewPin, FieldRole::ConfirmPin],
            Self::Fields => &[],
        }
    }

    /// How many digits the PIN (or the new PIN) may have.
    fn pin_range(self) -> (usize, usize) {
        match self {
            Self::Pin { min, max } => (min.min(max), max.max(min)),
            _ => SIM_PIN_DIGITS,
        }
    }
}

/// What one field of a prompt holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldRole {
    Username,
    /// Plain text the asker names: a domain, an identity.
    Text,
    Secret,
    Pin,
    Puk,
    NewPin,
    ConfirmPin,
}

impl FieldRole {
    fn default_label(self) -> String {
        crate::i18n!(match self {
            Self::Username | Self::Text => "secret-prompt-username",
            Self::Secret => "secret-prompt-password",
            Self::Pin => "secret-prompt-pin",
            Self::Puk => "secret-prompt-puk",
            Self::NewPin => "secret-prompt-new-pin",
            Self::ConfirmPin => "secret-prompt-confirm-pin",
        })
    }

    fn id(self) -> &'static str {
        match self {
            Self::Username => "username",
            Self::Text => "text",
            Self::Secret => "secret",
            Self::Pin => "pin",
            Self::Puk => "puk",
            Self::NewPin => "new-pin",
            Self::ConfirmPin => "confirm-pin",
        }
    }

    fn is_digits(self) -> bool {
        matches!(
            self,
            Self::Pin | Self::Puk | Self::NewPin | Self::ConfirmPin
        )
    }
}

/// How the prompt is framed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Presentation {
    /// The modal sheet a painted host draws: floating on a roomy display, a
    /// full-window page on a phone.
    #[default]
    Sheet,
    /// Inside another surface: no title or sheet, its own buttons.
    Inline,
    /// The body of a [`ModalDialog`] that draws the title and the actions.
    DialogBody,
}

/// What the person typed, handed out once on submit. Every secret is in a
/// buffer that zeroes itself when dropped; `Debug` shows none of them.
pub struct SecretAnswer {
    /// The user name, for [`SecretKind::UsernameAndSecret`].
    pub username: Option<String>,
    /// The password, the PIN or, for [`SecretKind::SimPuk`], the PUK.
    pub secret: Zeroizing<String>,
    /// The new PIN chosen with a PUK.
    pub new_pin: Option<Zeroizing<String>>,
    /// Every field's value, in the order the fields stand.
    pub values: Vec<Zeroizing<String>>,
    /// The choice picked ([`SecretPrompt::with_choices`]).
    pub choice: Option<usize>,
    /// Whether the option is ticked ([`SecretPrompt::with_option`]).
    pub option: Option<bool>,
}

/// One field a [`SecretKind::Fields`] prompt asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub label: String,
    /// Masked, with its own show/hide button.
    pub secret: bool,
    /// What it starts with; a secret field never starts with anything.
    pub initial: String,
    /// The prompt cannot be answered while it is empty.
    pub required: bool,
}

impl FieldSpec {
    pub fn secret(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secret: true,
            initial: String::new(),
            required: true,
        }
    }

    pub fn text(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            secret: false,
            initial: String::new(),
            required: true,
        }
    }

    #[must_use]
    pub fn with_initial(mut self, initial: impl Into<String>) -> Self {
        if !self.secret {
            self.initial = initial.into();
        }
        self
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }
}

impl std::fmt::Debug for SecretAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretAnswer")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .field("new_pin", &self.new_pin.as_ref().map(|_| "<redacted>"))
            .field("values", &self.values.len())
            .field("choice", &self.choice)
            .field("option", &self.option)
            .finish()
    }
}

/// How the person answered.
#[derive(Debug)]
pub enum PromptOutcome {
    Submit(SecretAnswer),
    Cancel,
}

/// What the person asked for, before the answer is read out of the fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Submit,
    Cancel,
}

/// A key of the numeric keypad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeypadKey {
    Digit(u8),
    Delete,
    Enter,
}

impl KeypadKey {
    /// Phone order: 1 2 3 / 4 5 6 / 7 8 9 / delete 0 enter.
    const ALL: [Self; 12] = [
        Self::Digit(1),
        Self::Digit(2),
        Self::Digit(3),
        Self::Digit(4),
        Self::Digit(5),
        Self::Digit(6),
        Self::Digit(7),
        Self::Digit(8),
        Self::Digit(9),
        Self::Delete,
        Self::Digit(0),
        Self::Enter,
    ];

    fn key(self) -> u64 {
        match self {
            Self::Digit(digit) => KEYPAD_KEY.saturating_add(u64::from(digit)),
            Self::Delete => KEYPAD_KEY.saturating_add(10),
            Self::Enter => KEYPAD_KEY.saturating_add(11),
        }
    }

    fn name(self) -> String {
        match self {
            Self::Digit(digit) => digit.to_string(),
            Self::Delete => crate::i18n!("secret-prompt-keypad-delete"),
            Self::Enter => crate::i18n!("secret-prompt-keypad-enter"),
        }
    }
}

/// Where keyboard focus is inside the prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Choice,
    Field(usize),
    Option,
    Cancel,
    Primary,
}

/// What a point or a finger landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Cancel,
    Primary,
    Key(KeypadKey),
    Choice(usize),
    Option,
}

// Accessibility keys of the prompt's parts. Fields are keyed by index from
// `FIELD_KEY`; a field's show/hide button is its only child.
const CANCEL_KEY: u64 = 1;
const PRIMARY_KEY: u64 = 2;
const NOTICE_KEY: u64 = 3;
const OPTION_KEY: u64 = 4;
const CHOICE_KEY: u64 = 300;
const FIELD_KEY: u64 = 100;
const KEYPAD_KEY: u64 = 200;
const REVEAL_KEY: u64 = 1;

struct PromptField {
    role: FieldRole,
    label: String,
    required: bool,
    input: TextInput,
}

/// Where every part of the prompt landed in its last layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PromptLayout {
    /// The sheet's chrome, for [`Presentation::Sheet`].
    pub modal: Option<ModalLayout>,
    /// The body: everything between the title and the actions.
    pub body: Rect,
    pub resource: Option<Rect>,
    /// The choices' label and one button per choice.
    pub choice_label: Option<Rect>,
    pub choices: Vec<Rect>,
    /// One label rect per field.
    pub labels: Vec<Rect>,
    /// One rect per field.
    pub fields: Vec<Rect>,
    /// The notice line under the fields; always reserved, so an error or a
    /// Caps Lock notice never moves anything.
    pub notice: Rect,
    /// The option's check row.
    pub option: Option<Rect>,
    /// The numeric keypad's keys, while it shows.
    pub keypad: Vec<(KeypadKey, Rect)>,
    /// Cancel and the primary button (in a sheet, the chrome's actions).
    pub cancel: Rect,
    pub primary: Rect,
}

/// The password prompt. See the module documentation.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent focus, touch and echo flags, not a state machine"
)]
pub struct SecretPrompt {
    kind: SecretKind,
    presentation: Presentation,
    icon: Option<Icon>,
    title: String,
    resource: String,
    primary_label: String,
    cancel_label: String,
    fields: Vec<PromptField>,
    /// Who of several the secret is for (polkit's administrators), with
    /// what the row is called and the one picked.
    choices: Vec<String>,
    choice_label: String,
    choice: usize,
    /// A check row under the fields ("Remember on this device").
    option: Option<(String, bool)>,
    focus: Stop,
    has_focus: bool,
    /// Why the last answer was refused, and the field it was about.
    error: Option<(usize, String)>,
    /// Informational text from the asker (PAM's "Touch the key").
    notice: Option<String>,
    /// What the asker is doing with the answer ("Connecting…"); the prompt
    /// is inert meanwhile.
    busy: Option<String>,
    caps: CapsLockEstimate,
    environment: InteractionEnvironment,
    /// Touch is the input in use: the keypad shows for the PIN kinds.
    touch_mode: bool,
    /// The part of the window an on-screen keyboard covers, from the bottom.
    keyboard_inset: f64,
    max_width: f64,
    id: String,
    window: Rect,
    rect: Rect,
    resource_h: f64,
    /// The line under the fields as last measured, and how many lines it
    /// wraps to: a long refusal is read whole, never cut off.
    measured_notice: Option<String>,
    notice_lines: usize,
    /// Whether the keypad showed when the body was last measured.
    measured_keypad: bool,
    layout: PromptLayout,
    cursor: Option<Point>,
    pressed: Option<Target>,
    touch: Option<(u64, Target)>,
    decision: Rc<Cell<Option<Decision>>>,
    theme: Theme,
    /// Where the secret field's value is kept outside the prompt, for a
    /// host that builds its tree afresh ([`Self::with_secret_buffer`]).
    secret_buffer: Option<Arc<Mutex<String>>>,
    /// The host's default action, run by Enter in a dialog body's last
    /// field ([`Self::on_submit`]).
    on_submit: Option<Rc<dyn Fn()>>,
    /// What the prompt showed around its fields when it was last painted:
    /// while that holds, only what a field repaints is damage.
    painted: Option<Chrome>,
    /// How the line under the fields arrives when the answer changes.
    answer: answer::Answer,
}

/// Everything the prompt paints around its fields' text.
#[derive(Clone, Debug, PartialEq)]
struct Chrome {
    rect: Rect,
    window: Rect,
    notice: Option<(String, NoticeTone)>,
    focus: Stop,
    has_focus: bool,
    hovered: Option<Target>,
    pressed: Option<Target>,
    choice: usize,
    option: Option<bool>,
    keypad: bool,
    can_submit: bool,
}

impl SecretPrompt {
    /// A prompt for `kind`; name it with the `with_*` builders.
    pub fn new(kind: SecretKind) -> Self {
        let mut prompt = Self {
            kind,
            presentation: Presentation::default(),
            icon: Some(Icon::Lock),
            title: String::new(),
            resource: String::new(),
            primary_label: crate::i18n!("secret-prompt-continue"),
            cancel_label: crate::i18n!("secret-prompt-cancel"),
            fields: Vec::new(),
            choices: Vec::new(),
            choice_label: String::new(),
            choice: 0,
            option: None,
            focus: Stop::Field(0),
            has_focus: false,
            error: None,
            notice: None,
            busy: None,
            caps: CapsLockEstimate::new(),
            environment: InteractionEnvironment::default(),
            touch_mode: false,
            keyboard_inset: 0.0,
            max_width: 440.0,
            id: "secret-prompt".to_string(),
            window: Rect::ZERO,
            rect: Rect::ZERO,
            resource_h: 0.0,
            measured_notice: None,
            notice_lines: 1,
            measured_keypad: false,
            layout: PromptLayout::default(),
            cursor: None,
            pressed: None,
            touch: None,
            decision: Rc::new(Cell::new(None)),
            theme: Theme::dark(),
            secret_buffer: None,
            on_submit: None,
            painted: None,
            answer: answer::Answer::default(),
        };
        prompt.fields = kind
            .roles()
            .iter()
            .enumerate()
            .map(|(index, role)| prompt.new_field(index, *role, false))
            .collect();
        prompt
    }

    /// A prompt for the fields the asker names, in order; focus starts in
    /// the first required one still empty.
    pub fn with_fields(specs: Vec<FieldSpec>) -> Self {
        let mut prompt = Self::new(SecretKind::Fields);
        prompt.fields = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| {
                let role = if spec.secret {
                    FieldRole::Secret
                } else {
                    FieldRole::Text
                };
                let mut field = prompt.new_field(index, role, false);
                field.input = field.input.with_accessible_name(spec.label.clone());
                if !spec.secret && !spec.initial.is_empty() {
                    field.input = field.input.with_initial_value(&spec.initial);
                }
                field.label = spec.label;
                field.required = spec.required;
                field
            })
            .collect();
        prompt.focus = Stop::Field(prompt.first_empty_field());
        prompt
    }

    fn field_id(&self, index: usize, role: FieldRole) -> String {
        if self.kind == SecretKind::Fields {
            format!("{}-{index}", self.id)
        } else {
            format!("{}-{}", self.id, role.id())
        }
    }

    fn new_field(&self, index: usize, role: FieldRole, echo: bool) -> PromptField {
        let label = role.default_label();
        let mut input = TextInput::local()
            .with_id(&self.field_id(index, role))
            .with_accessible_name(label.clone());
        match role {
            FieldRole::Username | FieldRole::Text => {
                input = input.with_kind(TextInputKind::Username);
            }
            FieldRole::Secret if echo => input = input.with_kind(TextInputKind::Username),
            FieldRole::Secret => input = input.with_secret(true),
            FieldRole::Pin | FieldRole::Puk | FieldRole::NewPin | FieldRole::ConfirmPin => {
                // Secret first (it sets the password kind), then the PIN
                // kind, so the input method offers its digit pad.
                input = input
                    .with_secret(true)
                    .with_kind(TextInputKind::Pin)
                    .with_char_filter(|c| c.is_ascii_digit());
            }
        }
        PromptField {
            role,
            label,
            required: true,
            input,
        }
    }

    /// Who asks and for what: "Connect to TruffleBoys2026".
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// The glyph before the title; a lock unless set.
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The resource the secret is for, under the title: the network's
    /// security, the action polkit authorises, the VPN's server.
    #[must_use]
    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        self.resource = resource.into();
        self
    }

    /// The primary button's label: the action (Connect, Unlock, Allow).
    #[must_use]
    pub fn with_primary_label(mut self, label: impl Into<String>) -> Self {
        self.primary_label = label.into();
        self
    }

    #[must_use]
    pub fn with_cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = label.into();
        self
    }

    /// The label of the field holding `role`, in the asker's words (PAM's
    /// "Password:", NetworkManager's "Private key password").
    #[must_use]
    pub fn with_label(mut self, role: FieldRole, label: impl Into<String>) -> Self {
        let label = label.into();
        if let Some(field) = self.fields.iter_mut().find(|field| field.role == role) {
            field.input = std::mem::replace(&mut field.input, TextInput::local())
                .with_accessible_name(label.clone());
            field.label = label;
        }
        self
    }

    /// The user name the prompt starts with; focus starts in the secret then.
    #[must_use]
    pub fn with_username(mut self, username: &str) -> Self {
        if let Some(index) = self.index_of(FieldRole::Username) {
            if let Some(field) = self.fields.get_mut(index) {
                field.input = std::mem::replace(&mut field.input, TextInput::local())
                    .with_initial_value(username);
            }
            self.focus = Stop::Field(self.first_empty_field());
        }
        self
    }

    /// The secret is shown as typed: the asker said it is no secret (PAM's
    /// echo-on prompt for a one-time code).
    #[must_use]
    pub fn with_echo(mut self, echo: bool) -> Self {
        if let Some(index) = self.index_of(FieldRole::Secret) {
            let label = self.fields.get(index).map(|field| field.label.clone());
            let mut field = self.new_field(index, FieldRole::Secret, echo);
            if let Some(label) = label {
                field.input = field.input.with_accessible_name(label.clone());
                field.label = label;
            }
            if let Some(slot) = self.fields.get_mut(index) {
                *slot = field;
            }
        }
        self
    }

    #[must_use]
    pub fn with_presentation(mut self, presentation: Presentation) -> Self {
        self.presentation = presentation;
        self
    }

    /// Widest the sheet is drawn, in design pixels.
    #[must_use]
    pub fn with_max_width(mut self, max_width: f64) -> Self {
        self.max_width = max_width;
        self
    }

    /// The stable address of the prompt and its fields (`<id>-secret`, …)
    /// for inspection and tests.
    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        id.clone_into(&mut self.id);
        let ids: Vec<String> = self
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| self.field_id(index, field.role))
            .collect();
        for (field, id) in self.fields.iter_mut().zip(ids) {
            field.input = std::mem::replace(&mut field.input, TextInput::local()).with_id(&id);
        }
        self
    }

    /// Who of several the secret is for, as a row of buttons above the
    /// fields called `label`; `selected` starts picked. One choice or none
    /// shows no row.
    #[must_use]
    pub fn with_choices(
        mut self,
        label: impl Into<String>,
        choices: Vec<String>,
        selected: usize,
    ) -> Self {
        self.choice_label = label.into();
        self.choice = selected.min(choices.len().saturating_sub(1));
        self.choices = choices;
        self
    }

    /// Keep the secret (the password, the PIN or the PUK) in `buffer`, and
    /// start from what it holds: for a host that builds its widget tree
    /// afresh, so what was typed outlives the prompt that took it. The host
    /// owns the buffer and zeroes it when the secret is used or dropped.
    #[must_use]
    pub fn with_secret_buffer(mut self, buffer: Arc<Mutex<String>>) -> Self {
        let index = self.secret_field_index();
        if let Some(field) = self.fields.get_mut(index) {
            let value =
                Zeroizing::new(buffer.lock().map(|value| value.clone()).unwrap_or_default());
            field.input.set_text(&value);
            // Seeding is not an edit: nothing to clear or publish.
            drop(field.input.take_dirty().map(Zeroizing::new));
        }
        self.secret_buffer = Some(buffer);
        self
    }

    /// Show `error` under the field it is about from the start, as
    /// [`Self::reject`] does but leaving the caret where it is: for a host
    /// that builds its tree afresh and keeps the last refusal itself.
    #[must_use]
    pub fn with_error(mut self, error: Option<String>) -> Self {
        if let Some(error) = error.filter(|error| !error.trim().is_empty()) {
            let index = self.secret_field_index();
            for (i, field) in self.fields.iter_mut().enumerate() {
                Widget::set_invalid(&mut field.input, i == index);
            }
            self.error = Some((index, error));
        }
        self
    }

    /// What the host already knows of Caps Lock, for a prompt built afresh
    /// while the person types: the notice stays up across the rebuild.
    #[must_use]
    pub fn with_caps_lock(mut self, caps: CapsLockEstimate) -> Self {
        self.caps = caps;
        self
    }

    /// What the asker is doing with the answer, from the start
    /// ([`Self::set_busy`]).
    #[must_use]
    pub fn with_busy(mut self, busy: Option<String>) -> Self {
        self.set_busy(busy);
        self
    }

    /// Informational text from the asker, from the start
    /// ([`Self::set_notice`]).
    #[must_use]
    pub fn with_notice(mut self, notice: Option<String>) -> Self {
        self.set_notice(notice);
        self
    }

    /// Run `submit` when Enter is pressed in a dialog body's last field: for
    /// a host with no dialog around the body whose default action would
    /// take that Enter.
    #[must_use]
    pub fn on_submit(mut self, submit: impl Fn() + 'static) -> Self {
        self.on_submit = Some(Rc::new(submit));
        self
    }

    /// A check row under the fields, ticked or not to start with.
    #[must_use]
    pub fn with_option(mut self, label: impl Into<String>, checked: bool) -> Self {
        self.option = Some((label.into(), checked));
        self
    }

    fn shows_choices(&self) -> bool {
        self.choices.len() > 1
    }

    /// Pick the choice at `index`, as the arrow keys on the choice row do.
    pub fn set_choice(&mut self, index: usize) {
        if index < self.choices.len() {
            self.choice = index;
        }
    }

    /// The choice picked.
    pub fn choice(&self) -> usize {
        self.choice
    }

    /// Whether the option is ticked.
    pub fn option_checked(&self) -> Option<bool> {
        self.option.as_ref().map(|(_, checked)| *checked)
    }

    /// This prompt as the body of a [`ModalDialog`] titled by it, its
    /// actions Cancel and the primary button; Enter answers and Escape
    /// cancels as in every dialog. Read the answer with
    /// `dialog.content_mut().take_outcome()`.
    pub fn into_dialog(mut self) -> ModalDialog<Self> {
        self.presentation = Presentation::DialogBody;
        let submit = Rc::clone(&self.decision);
        let cancel = Rc::clone(&self.decision);
        let title = self.title.clone();
        let primary = self.primary_label.clone();
        let cancel_label = self.cancel_label.clone();
        let icon = self.icon;
        let mut dialog = ModalDialog::new(title, self)
            .open(true)
            .primary_action(primary, move || submit.set(Some(Decision::Submit)))
            .secondary_action(cancel_label, move || cancel.set(Some(Decision::Cancel)))
            .primary_enabled_if(Self::can_submit);
        if let Some(icon) = icon {
            dialog = dialog.with_icon(icon);
        }
        dialog
    }

    pub fn kind(&self) -> SecretKind {
        self.kind
    }

    pub fn presentation(&self) -> Presentation {
        self.presentation
    }

    pub fn set_presentation(&mut self, presentation: Presentation) {
        self.presentation = presentation;
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn resource(&self) -> &str {
        &self.resource
    }

    /// The SIM's attempts left changed (after a wrong PIN).
    pub fn set_attempts_left(&mut self, attempts: Option<u32>) {
        match &mut self.kind {
            SecretKind::SimPin { attempts_left } | SecretKind::SimPuk { attempts_left } => {
                *attempts_left = attempts;
            }
            _ => {}
        }
    }

    /// The field holding `role`.
    pub fn field(&self, role: FieldRole) -> Option<&TextInput> {
        self.fields
            .iter()
            .find(|field| field.role == role)
            .map(|field| &field.input)
    }

    /// The field holding `role`, to seed it in tests and captures.
    pub fn field_mut(&mut self, role: FieldRole) -> Option<&mut TextInput> {
        self.fields
            .iter_mut()
            .find(|field| field.role == role)
            .map(|field| &mut field.input)
    }

    /// Every field, in order, with what it holds.
    pub fn fields(&self) -> impl Iterator<Item = (FieldRole, &TextInput)> {
        self.fields.iter().map(|field| (field.role, &field.input))
    }

    /// The role of the field keyboard focus is in, if it is in one.
    pub fn focused_field(&self) -> Option<FieldRole> {
        match self.focus {
            Stop::Field(index) => self.fields.get(index).map(|field| field.role),
            Stop::Choice | Stop::Option | Stop::Cancel | Stop::Primary => None,
        }
    }

    /// The error shown, if an answer was refused.
    pub fn error(&self) -> Option<&str> {
        self.error.as_ref().map(|(_, message)| message.as_str())
    }

    /// What the asker is doing with the answer, while it does.
    pub fn busy(&self) -> Option<&str> {
        self.busy.as_deref()
    }

    /// Whether the numeric keypad shows.
    pub fn shows_keypad(&self) -> bool {
        self.kind.is_pin() && self.touch_mode && self.presentation != Presentation::Inline
    }

    /// Whether Caps Lock seems on while a secret is being typed.
    pub fn caps_lock_on(&self) -> bool {
        self.caps.on()
            && self.focused_field().is_some_and(|role| {
                self.field(role)
                    .is_some_and(|input| input.is_secret() && !role.is_digits())
            })
    }

    /// The line under the fields: the error, else the asker's activity, else
    /// Caps Lock, else the asker's notice, else the PIN length or attempts.
    /// With whether it is an error.
    pub fn notice_line(&self) -> Option<(String, NoticeTone)> {
        if let Some((_, error)) = &self.error {
            return Some((error.clone(), NoticeTone::Error));
        }
        if let Some(busy) = &self.busy {
            // What the asker says while it checks (touch the reader) is
            // what the person needs now; otherwise what is being done.
            return Some((
                self.notice.as_ref().unwrap_or(busy).clone(),
                NoticeTone::Busy,
            ));
        }
        if self.caps_lock_on() {
            return Some((crate::i18n!("secret-prompt-caps-lock"), NoticeTone::Warning));
        }
        if let Some(notice) = &self.notice {
            return Some((notice.clone(), NoticeTone::Quiet));
        }
        if let Some(count) = self.kind.attempts_left() {
            let tone = if count <= 1 {
                NoticeTone::Warning
            } else {
                NoticeTone::Quiet
            };
            return Some((
                crate::i18n!("secret-prompt-attempts-left", "count" => count),
                tone,
            ));
        }
        if self.kind.is_pin() {
            let (min, max) = self.kind.pin_range();
            let text = if min == max {
                crate::i18n!("secret-prompt-pin-length-exact", "count" => min)
            } else {
                crate::i18n!("secret-prompt-pin-length", "min" => min, "max" => max)
            };
            return Some((text, NoticeTone::Quiet));
        }
        None
    }

    /// Informational text from the asker, shown on the notice line when
    /// nothing more urgent is.
    pub fn set_notice(&mut self, notice: Option<String>) {
        self.notice = notice.filter(|notice| !notice.trim().is_empty());
    }

    /// The asker is using the answer ("Connecting…"): the prompt says so and
    /// takes no input but Escape until [`Self::reject`] or `set_busy(None)`.
    pub fn set_busy(&mut self, busy: Option<String>) {
        let was = self.busy.is_some();
        self.busy = busy;
        let now = self.busy.is_some();
        if now && !was {
            // What the asker said was about the answer before it was
            // given; what it says while checking is set after this.
            self.notice = None;
        }
        // The fields keep their face while the answer is checked: the
        // prompt takes no input then, and rings them in the informational
        // role, so they read as checking rather than unavailable.
        if was && !now {
            self.sync_focus();
        }
    }

    /// The answer was wrong: say why under the field, keep focus in it and
    /// select what it holds, so the next keystroke replaces it.
    pub fn reject(&mut self, message: impl Into<String>) {
        let index = self.secret_field_index();
        self.reject_field(index, message.into(), true);
    }

    /// The value of the field at `index` (in the order the fields stand)
    /// was wrong; as [`Self::reject`] for that field.
    pub fn reject_at(&mut self, index: usize, message: impl Into<String>) {
        let index = index.min(self.fields.len().saturating_sub(1));
        self.reject_field(index, message.into(), true);
    }

    /// The value of the field at `index` cannot be used as it stands (too
    /// short, missing): say why and put the caret back at its end, so the
    /// person completes it rather than retyping it.
    pub fn refuse_at(&mut self, index: usize, message: impl Into<String>) {
        let index = index.min(self.fields.len().saturating_sub(1));
        self.reject_field(index, message.into(), false);
    }

    fn reject_field(&mut self, index: usize, message: String, select: bool) {
        self.set_busy(None);
        self.decision.set(None);
        for (i, field) in self.fields.iter_mut().enumerate() {
            Widget::set_invalid(&mut field.input, i == index);
        }
        self.error = Some((index, message));
        self.focus = Stop::Field(index);
        self.sync_focus();
        if select && let Some(field) = self.fields.get_mut(index) {
            field.input.select_all();
        }
    }

    /// Take the error down, as typing does: the asker edited the field
    /// itself.
    pub fn clear_error(&mut self) {
        if self.error.take().is_some() {
            for field in &mut self.fields {
                Widget::set_invalid(&mut field.input, false);
            }
        }
    }

    /// The field a wrong answer is about: the secret, the PIN or the PUK.
    fn secret_field_index(&self) -> usize {
        self.fields
            .iter()
            .position(|field| !matches!(field.role, FieldRole::Username | FieldRole::Text))
            .unwrap_or(0)
    }

    fn index_of(&self, role: FieldRole) -> Option<usize> {
        self.fields.iter().position(|field| field.role == role)
    }

    fn first_empty_field(&self) -> usize {
        self.fields
            .iter()
            .position(|field| field.required && field.input.value().is_empty())
            .or_else(|| {
                self.fields
                    .iter()
                    .position(|field| field.input.value().is_empty())
            })
            .unwrap_or_else(|| self.fields.len().saturating_sub(1))
    }

    /// Whether every field holds something and nothing is in flight: the
    /// primary button is enabled.
    pub fn can_submit(&self) -> bool {
        self.busy.is_none()
            && self
                .fields
                .iter()
                .all(|field| !field.required || !field.input.value().is_empty())
    }

    /// Answer with what the fields hold, as the primary button does.
    pub fn submit(&mut self) {
        if self.can_submit() {
            self.decision.set(Some(Decision::Submit));
        }
    }

    /// Whether the person answered or backed out and
    /// [`Self::take_outcome`] has not been asked yet.
    pub fn answer_pending(&self) -> bool {
        self.decision.get().is_some()
    }

    /// Back out, as Cancel and Escape do.
    pub fn cancel(&mut self) {
        self.decision.set(Some(Decision::Cancel));
    }

    /// The person's answer, once: the secrets read out of the fields into
    /// zeroizing buffers, or the cancellation. An answer the prompt itself
    /// can tell is wrong (a short PIN, PINs that differ) is refused in place
    /// and yields nothing. The fields keep what they hold until the prompt
    /// is dropped or [`Self::wipe`]d, so a rejection can select it.
    pub fn take_outcome(&mut self) -> Option<PromptOutcome> {
        match self.decision.take()? {
            Decision::Cancel => {
                self.wipe();
                Some(PromptOutcome::Cancel)
            }
            Decision::Submit => {
                if !self.can_submit() {
                    return None;
                }
                if let Some((index, message, retype)) = self.local_refusal() {
                    self.reject_field(index, message, retype);
                    return None;
                }
                Some(PromptOutcome::Submit(self.answer()))
            }
        }
    }

    /// What the prompt can tell is wrong without asking anyone, and whether
    /// the value wants retyping (selected) rather than completing.
    fn local_refusal(&self) -> Option<(usize, String, bool)> {
        let (min, max) = self.kind.pin_range();
        for (index, field) in self.fields.iter().enumerate() {
            let length = field.input.value().chars().count();
            match field.role {
                FieldRole::Pin | FieldRole::NewPin if length < min || length > max => {
                    let text = if min == max {
                        crate::i18n!("secret-prompt-pin-length-exact", "count" => min)
                    } else {
                        crate::i18n!("secret-prompt-pin-length", "min" => min, "max" => max)
                    };
                    return Some((index, text, false));
                }
                FieldRole::Puk if length != PUK_DIGITS => {
                    return Some((
                        index,
                        crate::i18n!("secret-prompt-pin-length-exact", "count" => PUK_DIGITS),
                        false,
                    ));
                }
                FieldRole::ConfirmPin
                    if self
                        .field(FieldRole::NewPin)
                        .is_some_and(|new_pin| new_pin.value() != field.input.value()) =>
                {
                    return Some((index, crate::i18n!("secret-prompt-pins-differ"), true));
                }
                _ => {}
            }
        }
        None
    }

    fn answer(&self) -> SecretAnswer {
        let value = |role| {
            self.field(role)
                .map(|input| Zeroizing::new(input.value().to_owned()))
        };
        let secret = match self.kind {
            SecretKind::Secret | SecretKind::UsernameAndSecret | SecretKind::Fields => {
                value(FieldRole::Secret)
            }
            SecretKind::Pin { .. } | SecretKind::SimPin { .. } => value(FieldRole::Pin),
            SecretKind::SimPuk { .. } => value(FieldRole::Puk),
        };
        SecretAnswer {
            username: self
                .field(FieldRole::Username)
                .or_else(|| self.field(FieldRole::Text))
                .map(|input| input.value().to_owned()),
            secret: secret.unwrap_or_default(),
            new_pin: value(FieldRole::NewPin),
            values: self
                .fields
                .iter()
                .map(|field| Zeroizing::new(field.input.value().to_owned()))
                .collect(),
            choice: self.shows_choices().then_some(self.choice),
            option: self.option_checked(),
        }
    }

    /// Empty every field, overwriting what they held.
    pub fn wipe(&mut self) {
        for field in &mut self.fields {
            // Setting the text of a secret field zeroes the old buffer.
            field.input.set_text("");
        }
        if let Some(buffer) = &self.secret_buffer
            && let Ok(mut value) = buffer.lock()
        {
            zeroize::Zeroize::zeroize(&mut *value);
        }
        self.caps = CapsLockEstimate::new();
    }

    /// The part of the window an on-screen keyboard covers, from its bottom
    /// edge: a sheet stays above it so the field remains visible.
    pub fn set_keyboard_inset(&mut self, inset: f64) {
        self.keyboard_inset = inset.max(0.0);
    }

    /// Touch is (or is no longer) the input in use.
    pub fn set_touch_mode(&mut self, touch: bool) {
        self.touch_mode = touch;
    }

    /// Where everything landed in the last layout.
    pub fn layout(&self) -> &PromptLayout {
        &self.layout
    }

    fn focused_input_mut(&mut self) -> Option<&mut TextInput> {
        match self.focus {
            Stop::Field(index) => self.fields.get_mut(index).map(|field| &mut field.input),
            Stop::Choice | Stop::Option | Stop::Cancel | Stop::Primary => None,
        }
    }

    fn focused_input(&self) -> Option<&TextInput> {
        match self.focus {
            Stop::Field(index) => self.fields.get(index).map(|field| &field.input),
            Stop::Choice | Stop::Option | Stop::Cancel | Stop::Primary => None,
        }
    }

    /// Give the focused field the keyboard and take it from the others.
    fn sync_focus(&mut self) {
        let focus = self.focus;
        let has = self.has_focus;
        for (index, field) in self.fields.iter_mut().enumerate() {
            field.input.set_focus(has && focus == Stop::Field(index));
        }
    }

    /// The theme the prompt draws with: once a finger is in play, its
    /// fields, buttons and keypad keys are touch targets
    /// ([`InteractionEnvironment::TOUCH`]), whatever the pointer metrics say.
    fn effective_theme(&self, theme: &Theme) -> Theme {
        let coarse =
            self.touch_mode || self.environment.pointer_precision == PointerPrecision::Coarse;
        let mut theme = theme.clone();
        if coarse {
            let target = InteractionEnvironment::TOUCH.minimum_target() * theme.ui_scale;
            let metrics = &mut theme.tokens.metrics;
            metrics.min_pointer_target = metrics.min_pointer_target.max(target);
        }
        theme
    }

    fn gap(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.s2)
    }

    /// How tall a field of the prompt stands under `theme`: a host lines a
    /// control up beside it with this.
    pub fn field_height(theme: &Theme) -> f64 {
        ControlSize::Regular.height(theme)
    }

    /// How tall this prompt's fields stand under `theme` as it is now: a
    /// touch target once a finger is in play, [`Self::field_height`]
    /// otherwise.
    pub fn current_field_height(&self, theme: &Theme) -> f64 {
        Self::field_height(&self.effective_theme(theme))
    }

    fn line_height(theme: &Theme, role: TextRole) -> f64 {
        f64::from(theme.text(role).line_height_px())
    }

    fn keypad_key_height(theme: &Theme) -> f64 {
        ControlSize::Regular.height(theme)
    }

    fn keypad_height(theme: &Theme) -> f64 {
        Self::keypad_key_height(theme) * 4.0 + Self::gap(theme) * 3.0
    }

    fn button_height(theme: &Theme) -> f64 {
        ControlSize::Regular.height(theme)
    }

    /// Measure what depends on the width (the resource line's wrapping) and
    /// return the body's height.
    fn measure_body(&mut self, width: f64, text: &mut TextRenderer, theme: &Theme) -> f64 {
        self.measured_keypad = self.shows_keypad();
        self.resource_h = if self.resource.is_empty() || width <= 0.0 {
            0.0
        } else {
            let style = theme.text(TextRole::Body);
            let lines =
                text.clamp_to_lines(&self.resource, &style, crate::num::f32_of_f64(width), 3);
            Self::line_height(theme, TextRole::Body) * crate::num::f64_of_usize(lines.len())
        };
        let notice = self.notice_line().map(|(line, _)| line);
        self.notice_lines = match &notice {
            Some(line) if width > 0.0 => text
                .clamp_to_lines(
                    line,
                    &theme.text(TextRole::Caption),
                    crate::num::f32_of_f64(width),
                    NOTICE_MAX_LINES,
                )
                .len()
                .max(1),
            _ => 1,
        };
        self.measured_notice = notice;
        self.body_height(theme)
    }

    fn body_height(&self, theme: &Theme) -> f64 {
        let rect = Rect::new(0.0, 0.0, 100.0, 0.0);
        let layout = self.layout_body(rect, theme);
        let bottom = [
            layout.notice.y1,
            layout.option.map_or(0.0, |rect| rect.y1),
            layout.keypad.last().map_or(0.0, |(_, rect)| rect.y1),
            layout.primary.y1,
        ]
        .into_iter()
        .fold(0.0, f64::max);
        bottom.ceil()
    }

    /// Lay the body out in `rect`, using the last measured resource height.
    fn layout_body(&self, rect: Rect, theme: &Theme) -> PromptLayout {
        let spacing = &theme.tokens.spacing;
        let gap = Self::gap(theme);
        let label_h = Self::line_height(theme, TextRole::Label);
        let field_h = Self::field_height(theme);
        let mut y = rect.y0;
        let resource = (self.resource_h > 0.0).then(|| {
            let r = Rect::new(rect.x0, y, rect.x1, y + self.resource_h);
            y = r.y1 + f64::from(spacing.s4);
            r
        });
        let mut choice_label = None;
        let mut choices = Vec::new();
        if self.shows_choices() {
            choice_label = Some(Rect::new(rect.x0, y, rect.x1, y + label_h));
            y += label_h + f64::from(spacing.s1);
            let count = crate::num::f64_of_usize(self.choices.len());
            let w = ((rect.width() - gap * (count - 1.0)) / count).max(0.0);
            for index in 0..self.choices.len() {
                let x0 = rect.x0 + crate::num::f64_of_usize(index) * (w + gap);
                choices.push(Rect::new(x0, y, x0 + w, y + field_h));
            }
            y += field_h + f64::from(spacing.s3);
        }
        let mut labels = Vec::with_capacity(self.fields.len());
        let mut fields = Vec::with_capacity(self.fields.len());
        for index in 0..self.fields.len() {
            if index > 0 {
                y += f64::from(spacing.s3);
            }
            labels.push(Rect::new(rect.x0, y, rect.x1, y + label_h));
            y += label_h + f64::from(spacing.s1);
            fields.push(Rect::new(rect.x0, y, rect.x1, y + field_h));
            y += field_h;
        }
        y += f64::from(spacing.s1);
        let caption_h = Self::line_height(theme, TextRole::Caption)
            * crate::num::f64_of_usize(self.notice_lines.max(1));
        let notice = Rect::new(rect.x0, y, rect.x1, y + caption_h);
        y = notice.y1;
        let option = self.option.as_ref().map(|_| {
            y += gap;
            let row = Rect::new(rect.x0, y, rect.x1, y + field_h);
            y = row.y1;
            row
        });
        let mut keypad = Vec::new();
        if self.shows_keypad() {
            y += f64::from(spacing.s4);
            let key_h = Self::keypad_key_height(theme);
            let key_w = ((rect.width() - gap * 2.0) / 3.0).max(0.0);
            for (index, key) in KeypadKey::ALL.iter().enumerate() {
                let column = crate::num::f64_of_usize(index % 3);
                let row = crate::num::f64_of_usize(index / 3);
                let x0 = rect.x0 + column * (key_w + gap);
                let y0 = y + row * (key_h + gap);
                keypad.push((*key, Rect::new(x0, y0, x0 + key_w, y0 + key_h)));
            }
            y += Self::keypad_height(theme);
        }
        let (cancel, primary) = if self.presentation == Presentation::Inline {
            y += gap;
            let h = Self::button_height(theme);
            let w = ((rect.width() - gap) / 2.0).max(0.0);
            (
                Rect::new(rect.x0, y, rect.x0 + w, y + h),
                Rect::new(rect.x1 - w, y, rect.x1, y + h),
            )
        } else {
            (Rect::ZERO, Rect::ZERO)
        };
        PromptLayout {
            modal: None,
            body: rect,
            resource,
            choice_label,
            choices,
            labels,
            fields,
            notice,
            option,
            keypad,
            cancel,
            primary,
        }
    }

    fn modal_actions(&self) -> [ModalAction<'_>; 2] {
        let enabled = self.can_submit();
        let hovered = |target| {
            self.cursor.is_some_and(|point| {
                self.target_rect(target)
                    .is_some_and(|rect| rect.contains(point))
            })
        };
        [
            ModalAction::secondary(&self.cancel_label)
                .hovered(hovered(Target::Cancel))
                .pressed(self.pressed == Some(Target::Cancel))
                .focused(self.has_focus && self.focus == Stop::Cancel),
            ModalAction::primary(&self.primary_label)
                .enabled(enabled)
                .hovered(hovered(Target::Primary))
                .pressed(self.pressed == Some(Target::Primary))
                .focused(self.has_focus && self.focus == Stop::Primary),
        ]
    }

    fn modal_spec<'a>(&'a self, actions: &'a [ModalAction<'a>], body: f64) -> ModalSpec<'a> {
        ModalSpec::new(&self.title)
            .icon(self.icon)
            .actions(actions)
            .body_height(body)
            .max_width(self.max_width)
            .closable(false)
            .dismiss_on_scrim(false)
    }

    /// The room a sheet lays out in: the window above an on-screen keyboard.
    fn sheet_window(&self) -> Rect {
        let mut window = self.window;
        window.y1 = (window.y1 - self.keyboard_inset).max(window.y0);
        window
    }

    /// Lay the prompt out: a sheet in `window` (the display or the app's
    /// window); an inline prompt or a dialog body in `window` as its rect.
    pub fn place(&mut self, window: Rect, text: &mut TextRenderer, theme: &Theme) {
        let theme = self.effective_theme(theme);
        self.window = window;
        match self.presentation {
            Presentation::Sheet => {
                let room = self.sheet_window();
                let width = modal_dialog::body_width(room, self.max_width, &theme);
                let body_h = self.measure_body(width, text, &theme);
                let actions = self.modal_actions();
                let spec = self.modal_spec(&actions, body_h);
                let modal = modal_dialog::layout(room, &spec, text, &theme);
                let mut layout = self.layout_body(modal.body, &theme);
                layout.cancel = modal.actions.first().copied().unwrap_or(Rect::ZERO);
                layout.primary = modal.actions.get(1).copied().unwrap_or(Rect::ZERO);
                self.rect = modal.body;
                layout.modal = Some(modal);
                self.layout = layout;
            }
            Presentation::Inline | Presentation::DialogBody => {
                self.measure_body(window.width(), text, &theme);
                self.rect = window;
                self.layout = self.layout_body(window, &theme);
            }
        }
        self.place_fields();
        self.theme = theme;
    }

    /// The height an inline prompt or dialog body `width` wide needs.
    pub fn preferred_height(&mut self, width: f64, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let theme = self.effective_theme(theme);
        self.measure_body(width, text, &theme)
    }

    /// Take `rect` as a dialog body's or an inline prompt's, measured
    /// already ([`Self::preferred_height`] or the layout pass).
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
        self.window = rect;
        let theme = self.theme.clone();
        self.layout = self.layout_body(rect, &theme);
        self.place_fields();
    }

    fn place_fields(&mut self) {
        for (field, rect) in self.fields.iter_mut().zip(&self.layout.fields) {
            field.input.apply_layout_external(*rect);
        }
    }

    fn target_rect(&self, target: Target) -> Option<Rect> {
        match target {
            Target::Cancel => Some(self.layout.cancel).filter(|rect| rect.area() > 0.0),
            Target::Primary => Some(self.layout.primary).filter(|rect| rect.area() > 0.0),
            Target::Key(key) => self
                .layout
                .keypad
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, rect)| *rect),
            Target::Choice(index) => self.layout.choices.get(index).copied(),
            Target::Option => self.layout.option,
        }
    }

    fn chrome(&self) -> Chrome {
        Chrome {
            rect: self.rect,
            window: self.window,
            notice: self.notice_line(),
            focus: self.focus,
            has_focus: self.has_focus,
            hovered: self.cursor.and_then(|point| self.target_at(point)),
            pressed: self.pressed,
            choice: self.choice,
            option: self.option_checked(),
            keypad: self.shows_keypad(),
            can_submit: self.can_submit(),
        }
    }

    /// Whether anything around the fields changed since the last paint.
    fn chrome_changed(&self) -> bool {
        self.painted.as_ref() != Some(&self.chrome())
    }

    fn target_at(&self, point: Point) -> Option<Target> {
        if let Some((key, _)) = self
            .layout
            .keypad
            .iter()
            .find(|(_, rect)| rect.contains(point))
        {
            return Some(Target::Key(*key));
        }
        if let Some(index) = self
            .layout
            .choices
            .iter()
            .position(|rect| rect.contains(point))
        {
            return Some(Target::Choice(index));
        }
        [Target::Option, Target::Cancel, Target::Primary]
            .into_iter()
            .find(|target| {
                self.target_rect(*target)
                    .is_some_and(|rect| rect.contains(point))
            })
    }

    fn activate(&mut self, target: Target) {
        if self.busy.is_some() && target != Target::Cancel {
            return;
        }
        match target {
            Target::Cancel => self.cancel(),
            Target::Primary => self.submit(),
            Target::Choice(index) => {
                if index < self.choices.len() {
                    self.choice = index;
                }
            }
            Target::Option => {
                if let Some((_, checked)) = &mut self.option {
                    *checked = !*checked;
                }
            }
            Target::Key(KeypadKey::Enter) => self.enter_in_field(),
            Target::Key(KeypadKey::Delete) => {
                if let Some(input) = self.focused_input_mut() {
                    if input.selection_range_public().is_some() {
                        input.delete_local_selection();
                    } else {
                        let mut value = Zeroizing::new(input.value().to_owned());
                        value.pop();
                        input.set_text(&value);
                    }
                }
                self.after_edit(true);
            }
            Target::Key(KeypadKey::Digit(digit)) => {
                if !matches!(self.focus, Stop::Field(_)) {
                    self.focus = Stop::Field(self.first_empty_field());
                    self.sync_focus();
                }
                if let Some(input) = self.focused_input_mut() {
                    input.insert_at_cursor(&digit.to_string());
                }
                self.after_edit(true);
            }
        }
    }

    /// Enter in a field: on to the next field, or answer from the last.
    fn enter_in_field(&mut self) {
        match self.focus {
            Stop::Field(index) if index.saturating_add(1) < self.fields.len() => {
                self.focus = Stop::Field(index.saturating_add(1));
                self.sync_focus();
            }
            _ => self.submit(),
        }
    }

    /// After the fields may have changed: a new value makes the old error
    /// moot, and a PIN never grows past its longest.
    fn after_edit(&mut self, edited: bool) {
        let mut changed = edited;
        let (_, max) = self.kind.pin_range();
        for field in &mut self.fields {
            if let Some(copy) = field.input.take_dirty() {
                drop(Zeroizing::new(copy));
                changed = true;
            }
            let limit = match field.role {
                FieldRole::Puk => Some(PUK_DIGITS),
                FieldRole::Pin | FieldRole::NewPin | FieldRole::ConfirmPin => Some(max),
                FieldRole::Username | FieldRole::Text | FieldRole::Secret => None,
            };
            if let Some(limit) = limit
                && field.input.value().chars().count() > limit
            {
                let kept: Zeroizing<String> =
                    Zeroizing::new(field.input.value().chars().take(limit).collect());
                field.input.set_text(&kept);
            }
        }
        if changed {
            self.clear_error();
            self.publish_secret();
        }
    }

    /// Mirror the secret field into the host's buffer, if it gave one.
    fn publish_secret(&self) {
        let Some(buffer) = &self.secret_buffer else {
            return;
        };
        let Some(field) = self.fields.get(self.secret_field_index()) else {
            return;
        };
        if let Ok(mut value) = buffer.lock()
            && value.as_str() != field.input.value()
        {
            zeroize::Zeroize::zeroize(&mut *value);
            value.push_str(field.input.value());
        }
    }

    /// Move the keyboard focus to the next (or previous) part, as Tab and
    /// Shift+Tab do. `false` when a dialog body's focus leaves it.
    pub fn move_focus(&mut self, forward: bool) -> bool {
        let mut stops: Vec<Stop> = Vec::new();
        if self.shows_choices() {
            stops.push(Stop::Choice);
        }
        stops.extend((0..self.fields.len()).map(Stop::Field));
        if self.option.is_some() {
            stops.push(Stop::Option);
        }
        if self.presentation != Presentation::DialogBody {
            stops.push(Stop::Cancel);
            if self.can_submit() {
                stops.push(Stop::Primary);
            }
        }
        let Some(current) = stops.iter().position(|stop| *stop == self.focus) else {
            self.focus = stops.first().copied().unwrap_or(Stop::Field(0));
            self.sync_focus();
            return true;
        };
        let next = if forward {
            current.checked_add(1).filter(|next| *next < stops.len())
        } else {
            current.checked_sub(1)
        };
        let next = match next {
            Some(next) => next,
            // A dialog body hands Tab on to the dialog's actions.
            None if self.presentation == Presentation::DialogBody => return false,
            None if forward => 0,
            None => stops.len().saturating_sub(1),
        };
        if let Some(stop) = stops.get(next) {
            self.focus = *stop;
        }
        self.sync_focus();
        true
    }

    /// Whether the focused field is composing with an input method, which
    /// then owns Enter, Escape and the arrows.
    fn composing(&self) -> bool {
        self.focused_input().is_some_and(TextInput::is_composing)
    }

    fn handle_key(&mut self, event: &KeyEvent, ctx: &EventCtx<'_>) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        // A physical key: the keypad goes away, the keyboard is in use.
        self.touch_mode = false;
        let dialog = self.presentation == Presentation::DialogBody;
        match &event.logical_key {
            Key::Named(NamedKey::Escape) if !self.composing() => {
                if dialog {
                    return false;
                }
                self.cancel();
                return true;
            }
            Key::Named(NamedKey::Tab) => {
                let back = ctx.modifiers.state().shift_key();
                return self.move_focus(!back);
            }
            Key::Named(NamedKey::Enter) if !self.composing() => {
                return match self.focus {
                    Stop::Cancel => {
                        self.cancel();
                        true
                    }
                    Stop::Choice | Stop::Option if dialog => false,
                    Stop::Choice | Stop::Option | Stop::Primary => {
                        self.submit();
                        true
                    }
                    Stop::Field(index) => {
                        let last = index.saturating_add(1) >= self.fields.len();
                        if last && dialog {
                            // The dialog's default action answers.
                            return match &self.on_submit {
                                Some(submit) if self.busy.is_none() => {
                                    submit();
                                    true
                                }
                                Some(_) => true,
                                None => false,
                            };
                        }
                        if self.busy.is_none() {
                            self.enter_in_field();
                        }
                        true
                    }
                };
            }
            Key::Named(NamedKey::Space)
                if matches!(self.focus, Stop::Cancel | Stop::Primary | Stop::Option) =>
            {
                match self.focus {
                    Stop::Cancel => self.cancel(),
                    Stop::Option => self.activate(Target::Option),
                    _ => self.submit(),
                }
                return true;
            }
            Key::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight))
                if self.focus == Stop::Choice =>
            {
                let count = self.choices.len();
                self.choice = if *direction == NamedKey::ArrowRight {
                    self.choice.saturating_add(1).min(count.saturating_sub(1))
                } else {
                    self.choice.saturating_sub(1)
                };
                return true;
            }
            _ => {}
        }
        if self.busy.is_some() {
            return !dialog;
        }
        if let Some(text) = event.text.as_deref() {
            self.caps
                .observe_text(text, ctx.modifiers.state().shift_key());
        }
        false
    }

    fn logical_point(position: winit::dpi::PhysicalPosition<f64>, scale: f64) -> Point {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        Point::new(position.x / scale, position.y / scale)
    }

    /// The press or tap at `point` landed on a field: focus moves there.
    fn focus_field_at(&mut self, point: Point) {
        if let Some(index) = self
            .layout
            .fields
            .iter()
            .position(|rect| rect.contains(point))
        {
            self.focus = Stop::Field(index);
            self.has_focus = true;
        }
    }

    /// Route one event. In a sheet every event is the prompt's: it is modal.
    fn route(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let sheet = self.presentation == Presentation::Sheet;
        match event {
            WindowEvent::KeyboardInput { event: key, .. } => {
                if self.handle_key(key, ctx) {
                    *ctx.handled = true;
                    return;
                }
                if self.busy.is_some() {
                    return;
                }
                if let Some(input) = self.focused_input_mut() {
                    input.handle_event(event, ctx);
                }
                self.after_edit(false);
                if sheet && key.state == ElementState::Pressed {
                    *ctx.handled = true;
                }
                return;
            }
            WindowEvent::Ime(_) => {
                if self.busy.is_none()
                    && let Some(input) = self.focused_input_mut()
                {
                    input.handle_event(event, ctx);
                }
                self.after_edit(false);
                if sheet {
                    *ctx.handled = true;
                }
                return;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some(Self::logical_point(*position, ctx.scale_factor));
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.pressed = None;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let target = self.cursor.and_then(|point| self.target_at(point));
                match state {
                    ElementState::Pressed => {
                        self.touch_mode = false;
                        if let Some(target) = target {
                            self.pressed = Some(target);
                            *ctx.handled = true;
                            return;
                        }
                        if let Some(point) = self.cursor {
                            self.focus_field_at(point);
                        }
                    }
                    ElementState::Released => {
                        if let Some(pressed) = self.pressed.take() {
                            if target == Some(pressed) {
                                self.activate(pressed);
                            }
                            *ctx.handled = true;
                            return;
                        }
                    }
                }
            }
            WindowEvent::Touch(touch) => {
                let point = Self::logical_point(touch.location, ctx.scale_factor);
                self.touch_mode = true;
                match touch.phase {
                    TouchPhase::Started => {
                        if let Some(target) = self.target_at(point) {
                            self.touch = Some((touch.id, target));
                            self.pressed = Some(target);
                            *ctx.handled = true;
                            return;
                        }
                        self.focus_field_at(point);
                    }
                    TouchPhase::Moved => {
                        if self.touch.is_some_and(|(id, _)| id == touch.id) {
                            *ctx.handled = true;
                            return;
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        if let Some((id, target)) = self.touch
                            && id == touch.id
                        {
                            self.touch = None;
                            self.pressed = None;
                            if touch.phase == TouchPhase::Ended
                                && self.target_at(point) == Some(target)
                            {
                                self.activate(target);
                            }
                            *ctx.handled = true;
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
        // Pointer input reaches every field: each knows its own rect, and a
        // press places the caret in the one under it.
        if self.busy.is_none() {
            for field in &mut self.fields {
                field.input.handle_event(event, ctx);
            }
        }
        self.sync_focus();
        self.after_edit(false);
        let pointer = matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } | WindowEvent::Touch(_)
        );
        let inside = self.cursor.is_some_and(|point| self.contains(point));
        if pointer && (sheet || inside) {
            *ctx.handled = true;
        }
    }

    /// Whether `point` is on the prompt: the sheet's card, or the body.
    pub fn contains(&self, point: Point) -> bool {
        match &self.layout.modal {
            Some(modal) => modal.card.contains(point),
            None => self.rect.contains(point),
        }
    }

    /// Paint the prompt where [`Self::place`] laid it out, its answer at
    /// the frame's instant.
    pub fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.paint_at(scene, transform, text, theme, crate::motion::frame_now());
    }

    /// Paint the prompt with its answer at `now`: a host that draws outside
    /// the widget tree and keeps its own frame clock passes its instant, so
    /// a still or a retained frame shows the same pose as a full one.
    pub fn paint_at(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        now: crate::time::Instant,
    ) {
        self.answer.observe(self.notice_line(), now, theme.motion());
        if let Some(until) = self.answer.deadline() {
            // Outside the tree the frame pump polls, a host still gets the
            // answer's frames through the motion clock.
            crate::motion::want_frames_until(until);
        }
        let answer = self.answer.paint(now);
        self.painted = Some(self.chrome());
        let theme = self.effective_theme(theme);
        if let Some(modal) = self.layout.modal.clone() {
            {
                let actions = self.modal_actions();
                let spec = self.modal_spec(&actions, modal.body.height());
                modal_dialog::paint_backdrop(scene, transform, text, &theme, &modal, &spec);
            }
            let on_sheet = theme.on_ground(crate::chrome::panel_ground(&theme));
            self.paint_body(scene, transform, text, &on_sheet, &answer);
            let actions = self.modal_actions();
            let spec = self.modal_spec(&actions, modal.body.height());
            modal_dialog::paint_actions(scene, transform, text, &theme, &modal, &spec);
        } else {
            self.paint_body(scene, transform, text, &theme, &answer);
        }
        self.answer.rest_if_settled(now);
    }

    /// When the answer under the fields comes to rest; `None` once its
    /// resting frame has been painted. A host drawing outside the widget
    /// tree asks for frames until then.
    pub fn answer_deadline(&self) -> Option<crate::time::Instant> {
        self.answer.deadline()
    }

    /// A field whose answer is being checked: edged in the informational
    /// role, where a refused one takes the danger role.
    fn paint_checking_edge(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        let colors = &theme.tokens.colors;
        let ink = colors.ink_on_any(
            &[colors.surfaces.raised, colors.surfaces.content],
            colors.semantic.info,
            crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
        );
        let radius = crate::chrome::ChromeRole::Control.radius(theme, rect.height());
        scene.stroke(
            &vello::kurbo::Stroke::new(1.5),
            transform,
            ink,
            None,
            &crate::surface::edge_outline(rect, radius),
        );
    }

    /// The ink of a line under the fields in `tone`, at `alpha`.
    fn notice_ink(theme: &Theme, tone: NoticeTone, dim: Color, alpha: f32) -> Color {
        let colors = &theme.tokens.colors;
        let color = match tone {
            NoticeTone::Error => colors.semantic.danger,
            NoticeTone::Warning => colors.semantic.warning,
            NoticeTone::Busy => colors.semantic.info,
            NoticeTone::Quiet => dim,
        };
        color.with_alpha(color.components[3] * alpha)
    }

    /// One line under the fields in `rect`, in `color`.
    fn paint_notice(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        (line, color): (&str, Color),
    ) {
        let style = theme.text(TextRole::Caption);
        let line_h = Self::line_height(theme, TextRole::Caption);
        let mut y = rect.y0;
        for shown in text.clamp_to_lines(
            line,
            &style,
            crate::num::f32_of_f64(rect.width().max(1.0)),
            NOTICE_MAX_LINES,
        ) {
            text.draw_style(
                scene,
                &shown,
                color,
                transform.pre_translate(Vec2::new(rect.x0, y)),
                None,
                &style,
            );
            y += line_h;
        }
    }

    fn paint_body(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        answer: &answer::AnswerPaint,
    ) {
        let (ink, dim) = if self.presentation == Presentation::Inline {
            (theme.text_primary, theme.text_dim)
        } else {
            modal_dialog::ink(theme)
        };
        if let Some(rect) = self.layout.resource {
            let style = theme.text(TextRole::Body);
            let width = crate::num::f32_of_f64(rect.width().max(1.0));
            let lines = text.clamp_to_lines(&self.resource, &style, width, 3);
            let line_h = Self::line_height(theme, TextRole::Body);
            let mut y = rect.y0;
            for line in &lines {
                text.draw_style(
                    scene,
                    line,
                    dim,
                    transform.pre_translate(Vec2::new(rect.x0, y)),
                    None,
                    &style,
                );
                y += line_h;
            }
        }
        let label_style = theme.text(TextRole::Label);
        if let Some(label) = self.layout.choice_label {
            text.draw_style(
                scene,
                &self.choice_label,
                ink,
                transform.pre_translate(Vec2::new(label.x0, label.y0)),
                Some(crate::num::f32_of_f64(label.width().max(1.0))),
                &label_style,
            );
        }
        for (index, (choice, rect)) in self.choices.iter().zip(&self.layout.choices).enumerate() {
            let face = ButtonFace::new(*rect, choice)
                .toggled(index == self.choice)
                .hovered(self.cursor.is_some_and(|point| rect.contains(point)))
                .pressed(self.pressed == Some(Target::Choice(index)))
                .focused(self.has_focus && self.focus == Stop::Choice && index == self.choice);
            Button::paint(scene, transform, text, theme, &face);
        }
        let presence = PresenceCtx::default();
        for (field, label) in self.fields.iter_mut().zip(&self.layout.labels) {
            let shown = text.elide_to_width_style(
                &field.label,
                crate::num::f32_of_f64(label.width().max(1.0)),
                &label_style,
            );
            text.draw_style(
                scene,
                &shown,
                ink,
                transform.pre_translate(Vec2::new(label.x0, label.y0)),
                None,
                &label_style,
            );
            field.input.draw(scene, transform, text, theme, &presence);
        }
        if self.busy.is_some() {
            for rect in &self.layout.fields {
                Self::paint_checking_edge(scene, transform, theme, *rect);
            }
        }
        let rect = self
            .layout
            .notice
            .with_origin((self.layout.notice.x0 + answer.dx, self.layout.notice.y0));
        if let Some(((line, tone), alpha)) = &answer.replaced {
            let ink = Self::notice_ink(theme, *tone, dim, *alpha);
            Self::paint_notice(scene, transform, text, theme, rect, (line, ink));
        }
        if let Some((line, tone)) = &answer.shown {
            let ink = Self::notice_ink(theme, *tone, dim, answer.shown_alpha);
            Self::paint_notice(scene, transform, text, theme, rect, (line, ink));
        }
        if let Some((label, checked)) = &self.option {
            let rect = self.layout.option.unwrap_or(Rect::ZERO);
            let side = f64::from(theme.tokens.metrics.icon_md);
            let check = Rect::new(
                rect.x0,
                rect.center().y - side / 2.0,
                rect.x0 + side,
                rect.center().y + side / 2.0,
            );
            crate::widget::Checkbox::paint(
                scene,
                transform,
                theme,
                &crate::widget::CheckboxFace::new(check, *checked)
                    .hovered(self.cursor.is_some_and(|point| rect.contains(point)))
                    .focused(self.has_focus && self.focus == Stop::Option),
            );
            let style = theme.text(TextRole::Body);
            let x = check.x1 + Self::gap(theme);
            let shown = text.elide_to_width_style(
                label,
                crate::num::f32_of_f64((rect.x1 - x).max(1.0)),
                &style,
            );
            let line = Self::line_height(theme, TextRole::Body);
            text.draw_style(
                scene,
                &shown,
                ink,
                transform.pre_translate(Vec2::new(x, rect.center().y - line / 2.0)),
                None,
                &style,
            );
        }
        for (key, rect) in self.layout.keypad.clone() {
            let name = key.name();
            let face = match key {
                KeypadKey::Digit(_) => ButtonFace::new(rect, &name).role(ButtonRole::Key),
                KeypadKey::Delete => {
                    ButtonFace::icon_only(rect, Icon::Delete, &name).role(ButtonRole::Key)
                }
                KeypadKey::Enter => ButtonFace::icon_only(rect, Icon::CornerDownLeft, &name)
                    .role(ButtonRole::Primary)
                    .disabled(!self.can_submit()),
            };
            let target = Target::Key(key);
            let face = face
                .control_size(ControlSize::Large)
                .hovered(self.cursor.is_some_and(|point| rect.contains(point)))
                .pressed(self.pressed == Some(target));
            Button::paint(scene, transform, text, theme, &face);
        }
        if self.presentation == Presentation::Inline {
            let [cancel, primary] = self.modal_actions();
            for (action, rect, role) in [
                (cancel, self.layout.cancel, ButtonRole::Secondary),
                (primary, self.layout.primary, ButtonRole::Primary),
            ] {
                let label = action.label;
                Button::paint(
                    scene,
                    transform,
                    text,
                    theme,
                    &ButtonFace::new(rect, label)
                        .role(role)
                        .hovered(action.hovered && action.enabled)
                        .pressed(action.pressed && action.enabled)
                        .focused(action.focused)
                        .disabled(!action.enabled),
                );
            }
        }
    }

    fn key_path_field(key: u64) -> Option<usize> {
        key.checked_sub(FIELD_KEY)
            .filter(|index| *index < KEYPAD_KEY.saturating_sub(FIELD_KEY))
            .and_then(|index| usize::try_from(index).ok())
    }

    /// The accessible children, in order: the fields, the notice, the
    /// keypad keys and, unless a dialog draws them, the two buttons.
    fn access_children(&self) -> Vec<AccessNode> {
        let mut children = Vec::new();
        // Published before the first layout too, unplaced, so an assistive
        // technology can answer a prompt that has not been painted yet.
        for (index, choice) in self.choices.iter().enumerate() {
            let rect = self
                .layout
                .choices
                .get(index)
                .copied()
                .unwrap_or(Rect::ZERO);
            let key = CHOICE_KEY.saturating_add(u64::try_from(index).unwrap_or(0));
            let chosen = index == self.choice;
            children.push(
                AccessNode::new(Role::RadioButton)
                    .with_name(choice.clone())
                    .with_description(self.choice_label.clone())
                    .with_key(key)
                    .with_position(index.saturating_add(1), self.choices.len())
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(chosen)
                            .focused(self.has_focus && self.focus == Stop::Choice && chosen),
                    )
                    .with_bounds(rect),
            );
        }
        for (index, field) in self.fields.iter().enumerate() {
            let Some(mut node) = Widget::accessibility(&field.input) else {
                continue;
            };
            node.name = Some(field.label.clone());
            node.state.required = field.required;
            if self.busy.is_some() {
                // Checking: not editable until the asker answers.
                node.state.disabled = true;
                node.state.busy = true;
            }
            if let Some((error_index, error)) = &self.error
                && *error_index == index
            {
                node.description = Some(error.clone());
            }
            if field.input.has_reveal_control() {
                let revealed = field.input.is_revealed();
                node.children = vec![
                    AccessNode::new(Role::ToggleButton)
                        .with_name(crate::i18n!(if revealed {
                            "text-input-hide-password"
                        } else {
                            "text-input-show-password"
                        }))
                        .with_key(REVEAL_KEY)
                        .with_state(AccessState {
                            checked: Some(revealed),
                            focusable: false,
                            ..Default::default()
                        })
                        .with_bounds(field.input.reveal_control_rect().unwrap_or(Rect::ZERO)),
                ];
            }
            let key = FIELD_KEY.saturating_add(u64::try_from(index).unwrap_or(0));
            children.push(
                node.with_key(key)
                    .with_bounds(self.layout.fields.get(index).copied().unwrap_or(Rect::ZERO)),
            );
        }
        if let Some((line, tone)) = self.notice_line() {
            // A refusal and what the asker says (an instruction that can
            // need acting on now) interrupt; the rest waits its turn.
            let politeness = if tone == NoticeTone::Error || self.notice.as_ref() == Some(&line) {
                Politeness::Assertive
            } else {
                Politeness::Polite
            };
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(line)
                    .with_key(NOTICE_KEY)
                    .with_live(politeness)
                    .with_bounds(self.layout.notice),
            );
        }
        if let Some((label, checked)) = &self.option {
            let rect = self.layout.option.unwrap_or(Rect::ZERO);
            children.push(
                AccessNode::new(Role::Checkbox)
                    .with_name(label.clone())
                    .with_key(OPTION_KEY)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(*checked)
                            .focused(self.has_focus && self.focus == Stop::Option),
                    )
                    .with_bounds(rect),
            );
        }
        for (key, rect) in &self.layout.keypad {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(key.name())
                    .with_key(key.key())
                    .with_bounds(*rect),
            );
        }
        if self.presentation != Presentation::DialogBody {
            let enabled = self.can_submit();
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(self.cancel_label.clone())
                    .with_key(CANCEL_KEY)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.has_focus && self.focus == Stop::Cancel),
                    )
                    .with_bounds(self.layout.cancel),
            );
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(self.primary_label.clone())
                    .with_key(PRIMARY_KEY)
                    .with_state(
                        AccessState::new()
                            .focusable(enabled)
                            .disabled(!enabled)
                            .focused(self.has_focus && self.focus == Stop::Primary),
                    )
                    .with_bounds(self.layout.primary),
            );
        }
        children
    }

    /// The prompt's accessible node: a dialog named by the title (a group
    /// inline or inside a dialog that names itself), described by the
    /// resource line, its fields named by their labels and masked.
    pub fn access_node(&self) -> AccessNode {
        let role = if self.presentation == Presentation::Sheet {
            Role::Dialog
        } else {
            Role::Group
        };
        let mut node = AccessNode::new(role)
            .with_state(
                AccessState::new()
                    .modal(self.presentation == Presentation::Sheet)
                    .busy(self.busy.is_some()),
            )
            .with_children(self.access_children());
        if !self.title.is_empty() {
            node = node.with_name(self.title.clone());
        } else if let [field] = self.fields.as_slice() {
            // A one-field prompt in a host's form is named by its field.
            node = node.with_name(field.label.clone());
        }
        if !self.resource.is_empty() {
            node = node.with_description(self.resource.clone());
        }
        node
    }

    /// Carry out an assistive technology's or an agent's `action` on the
    /// part at `key_path` (empty for the prompt itself).
    pub fn perform_access_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        root: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let Some((&key, rest)) = key_path.split_first() else {
            return false;
        };
        if let Some(index) = Self::key_path_field(key) {
            if index >= self.fields.len() {
                return false;
            }
            self.focus = Stop::Field(index);
            self.has_focus = true;
            self.sync_focus();
            if rest == [REVEAL_KEY] {
                if matches!(action, Action::Activate | Action::ToggleCheck)
                    && let Some(field) = self.fields.get_mut(index)
                {
                    field.input.toggle_reveal();
                    return true;
                }
                return false;
            }
            if matches!(action, Action::Focus) {
                return true;
            }
            if self.busy.is_some() {
                return false;
            }
            let done = self
                .fields
                .get_mut(index)
                .is_some_and(|field| field.input.perform_action(action, rest, state, root));
            self.after_edit(false);
            return done;
        }
        let choice = key
            .checked_sub(CHOICE_KEY)
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index < self.choices.len());
        let target = match key {
            CANCEL_KEY => Target::Cancel,
            PRIMARY_KEY => Target::Primary,
            OPTION_KEY if self.option.is_some() => Target::Option,
            other => match (choice, KeypadKey::ALL.iter().find(|k| k.key() == other)) {
                (Some(index), _) => Target::Choice(index),
                (None, Some(k)) => Target::Key(*k),
                (None, None) => return false,
            },
        };
        match action {
            Action::ToggleCheck if target == Target::Option => {
                self.activate(target);
                true
            }
            Action::Focus => {
                self.focus = match target {
                    Target::Cancel => Stop::Cancel,
                    Target::Primary => Stop::Primary,
                    Target::Option => Stop::Option,
                    Target::Choice(index) => {
                        self.choice = index;
                        Stop::Choice
                    }
                    Target::Key(_) => self.focus,
                };
                self.has_focus = true;
                self.sync_focus();
                true
            }
            Action::Activate => {
                self.activate(target);
                true
            }
            _ => false,
        }
    }

    /// Where the accessible child at `path` (indices into
    /// [`Self::access_node`]'s children) is.
    fn access_child_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index, rest @ ..] = path else {
            return None;
        };
        let children = self.access_children();
        let child = children.get(*index)?;
        match rest {
            [] => child.bounds.map(crate::a11y::AccessBounds::to_rect),
            [0] => child
                .children
                .first()
                .and_then(|reveal| reveal.bounds.map(crate::a11y::AccessBounds::to_rect)),
            _ => None,
        }
    }
}

/// How the notice line reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeTone {
    /// The answer was refused.
    Error,
    /// Worth noticing: Caps Lock, the last attempt.
    Warning,
    /// Information.
    Quiet,
    /// The asker is checking the answer.
    Busy,
}

impl Drop for SecretPrompt {
    fn drop(&mut self) {
        // Each secret field zeroes its own buffer on drop too; wiping here
        // also covers a field shown as typed (an echoed secret). A host's
        // buffer is the host's: its tree is rebuilt around what it holds.
        self.secret_buffer = None;
        self.wipe();
    }
}

impl Widget for SecretPrompt {
    fn needs_redraw(&self) -> bool {
        self.chrome_changed()
            || self.answer.deadline().is_some()
            || self.fields.iter().any(|field| field.input.needs_redraw())
    }

    /// A keystroke repaints its field; the whole prompt only when what is
    /// around the fields (the line under them, focus, a button) changed.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.chrome_changed() || self.answer.deadline().is_some() {
            // A sheet paints its scrim over the whole window.
            let rect = self
                .layout
                .modal
                .as_ref()
                .map_or(self.rect, |modal| modal.scrim);
            regions.push(rect);
            return;
        }
        for field in &self.fields {
            field.input.collect_damage(regions);
        }
    }

    fn mark_drawn(&mut self) {
        for field in &mut self.fields {
            field.input.mark_drawn();
        }
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = self.effective_theme(theme);
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::auto(),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let theme = self.theme.clone();
        let height = self.measure_body(f64::from(width), text_renderer, &theme);
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.apply_layout_external(crate::widget::layout_rect(taffy, node, origin));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    /// The keypad comes and goes with touch, which changes the height.
    fn layout_needs_measure(&self) -> bool {
        self.rect == Rect::ZERO
            || self.measured_keypad != self.shows_keypad()
            || self.measured_notice != self.notice_line().map(|(line, _)| line)
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_needs_measure()
    }

    fn set_interaction_environment(&mut self, environment: InteractionEnvironment) {
        // A prompt raised while touch is the input in use opens with its
        // keypad; a later switch of input follows it.
        if environment.modality != self.environment.modality {
            self.touch_mode = environment.modality == InputModality::Touch;
        }
        self.environment = environment;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.route(event, ctx);
    }

    fn name(&self) -> &'static str {
        "SecretPrompt"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A sheet keeps focus while its answer is checked; in a host's form
    /// the inert prompt leaves the tab order until it can be typed in.
    fn is_focusable(&self) -> bool {
        self.busy.is_none() || self.presentation == Presentation::Sheet
    }

    fn is_default_keyboard_focus(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.has_focus = focused;
        if focused && !matches!(self.focus, Stop::Field(_)) {
            self.focus = Stop::Field(self.first_empty_field());
        }
        self.sync_focus();
    }

    fn is_focused(&self) -> bool {
        self.has_focus
    }

    fn traps_focus(&self) -> bool {
        self.presentation == Presentation::Sheet
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.focused_input().and_then(Widget::ime_cursor_rect)
    }

    fn ime_content(&self) -> Option<crate::ime::InputContent> {
        self.focused_input().and_then(Widget::ime_content)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.focused_input()
            .and_then(|input| input.ime_edit_fingerprint(key))
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.target_at(point).is_some() {
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.fields
            .iter()
            .find_map(|field| field.input.cursor_shape(point))
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.access_node())
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.access_child_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let Some(index) = path.first() else {
            return false;
        };
        let choices = self.layout.choices.len().min(self.choices.len());
        let focus = if *index < choices {
            self.choice = *index;
            Stop::Choice
        } else if let Some(field) = index
            .checked_sub(choices)
            .filter(|field| *field < self.fields.len())
        {
            Stop::Field(field)
        } else {
            return false;
        };
        self.focus = focus;
        self.has_focus = true;
        self.sync_focus();
        true
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if key_path.is_empty() {
            return match action {
                crate::a11y::Action::Focus => {
                    Widget::set_focus(self, true);
                    true
                }
                _ => false,
            };
        }
        self.perform_access_action(action, key_path, state, path)
    }
}

#[path = "secret_prompt_answer.rs"]
mod answer;

#[cfg(test)]
#[path = "secret_prompt_tests.rs"]
mod tests;

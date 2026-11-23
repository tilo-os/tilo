//! "Add a device": the link or ticket that makes a new device of yours one of yours, shown as a
//! code a phone camera reads.
//!
//! The sheet shows the link as a code, as text to open or paste, and a button that copies it, with
//! the minutes it still works and where Tilo on another computer takes it. The session makes the
//! link and hands it here ([`AddDeviceState::show`]): one invite is open at a time, so a link left
//! on screen still admits the device, and the code is replaced only once it stopped working. A
//! workspace no host on the web keeps has no link; the sheet shows a ticket from this computer
//! instead ([`AddDevice::ticket`]), which the new computer spends with `tilo login`.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Element, ModalDialog, QrWidget, Row, Section, Text, TextInput,
};

use crate::presentation::{Presentation, SheetState, presented};
use crate::{caption, word};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.pair-device";

/// The sheet's resource, where its actions are published. It holds nothing; the link is the
/// session's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct AddDeviceLink {}

#[resource(kind = "pair.device")]
impl AddDeviceLink {}

/// What the sheet asks of the session.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.pair-device.link")]
pub enum PairDeviceEffect {
    /// Put the link on the clipboard
    #[effect(own_device, kind = "read")]
    Copy {
        #[param("The link, as the sheet shows it")]
        link: String,
    },
    /// Close the sheet
    #[effect(own_device, kind = "read")]
    Done,
}

/// What the sheet shows: the link the session made, and how long it works.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddDevice {
    pub title: String,
    pub description: String,
    pub close: String,
    /// The link, as the person reads and copies it.
    pub link: String,
    /// What the link is called.
    pub link_label: String,
    pub copy: String,
    /// What the copy button says once it was pressed.
    pub copied: String,
    /// What the code is, for someone who cannot see it.
    pub alt: String,
    /// How long this code works, in the person's words.
    pub lasting: String,
    /// Where Tilo on another computer takes the link: the one way to use it nobody guesses.
    pub in_tilo: String,
    /// Why there is no link to show, when there is none.
    pub refusal: Option<String>,
}

impl AddDevice {
    /// The sheet showing `link`, whose invite works for `minutes` more, in the person's language:
    /// what the session hands the state each time the link or its minutes change.
    #[must_use]
    pub fn of(link: &str, minutes: u32) -> Self {
        let minutes = ui::i18n::ArgValue::Number(f64::from(minutes));
        Self {
            title: word("pair-device-title"),
            description: word("pair-device-description"),
            close: word("pair-device-close"),
            link: link.to_owned(),
            link_label: word("pair-device-link-label"),
            copy: word("pair-device-copy"),
            copied: word("pair-device-copied"),
            alt: word("pair-device-code-alt"),
            lasting: crate::t_args("pair-device-lasting", &[("minutes", minutes)]),
            in_tilo: word("pair-device-in-tilo"),
            refusal: None,
        }
    }

    /// The sheet showing `ticket`, this computer's one-use ticket for a new computer, which works
    /// for `minutes` more: what a workspace no host on the web keeps shows in place of a link.
    #[must_use]
    pub fn ticket(ticket: &str, minutes: u32) -> Self {
        Self {
            description: word("pair-device-ticket-description"),
            link_label: word("pair-device-ticket-label"),
            copy: word("pair-device-ticket-copy"),
            alt: word("pair-device-ticket-alt"),
            in_tilo: word("pair-device-ticket-in-tilo"),
            ..Self::of(ticket, minutes)
        }
    }

    /// The sheet with no link to show, saying why in the person's words.
    #[must_use]
    pub fn refusing(reason: &str) -> Self {
        Self {
            title: word("pair-device-title"),
            description: word("pair-device-description"),
            close: word("pair-device-close"),
            refusal: Some(reason.to_owned()),
            ..Self::default()
        }
    }

    /// The sheet while this computer asks for a ticket, before it has one to show.
    #[must_use]
    pub fn asking() -> Self {
        Self::refusing(&word("pair-device-ticket-asking"))
    }
}

/// The sheet's dialog, as the person sees it.
#[must_use]
pub fn sheet(
    device: &AddDevice,
    link: &LocalState<String>,
    ask: &Ask,
    presentation: Option<&Presentation>,
) -> Element {
    let setup = match &device.refusal {
        Some(reason) => Box::new(Text::new(reason).with_role(TextRole::Body)),
        None => code(device, link, ask),
    };
    let body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section)
        .push(setup);
    let dismissed = ask.clone();
    let escaped = ask.clone();
    let dialog = ModalDialog::new(device.title.clone(), body)
        .with_test_id("pair-device")
        .with_icon(Icon::MonitorSmartphone)
        .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
        .secondary_action(device.close.clone(), move || {
            dismissed(PairDeviceEffect::Done);
        })
        .on_dismiss(move || escaped(PairDeviceEffect::Done))
        .with_description(device.description.clone());
    Box::new(presented(presentation, dialog))
}

/// The code the sheet draws for `device`: the link it was handed, as squares a camera reads.
fn code_of(device: &AddDevice) -> QrWidget {
    crate::code(&device.link, "pair-device-code", &device.alt)
}

/// What the sheet asks for, as a press in it hands it back.
pub type Ask = Rc<dyn Fn(PairDeviceEffect)>;

/// The link as a code a camera reads, how long it works, and the link as text to open or paste
/// with the button that copies it.
fn code(device: &AddDevice, link: &LocalState<String>, ask: &Ask) -> Element {
    if link.get() != device.link {
        link.set(device.link.clone());
    }
    let copying = ask.clone();
    let copied = device.link.clone();
    let written = TextInput::from_state(link)
        .with_id("pair-device-link")
        .with_read_only(true)
        .with_accessible_name(device.link_label.clone());
    let parts: Vec<Element> = vec![
        crate::centred(code_of(device)),
        caption(&device.lasting),
        caption(&device.link_label),
        Box::new(written),
        Box::new(
            Row::new().with_rhythm(Rhythm::Tight).push(Box::new(
                Button::new("pair-device-copy", &device.copy)
                    .with_icon(Icon::Copy)
                    .role(ButtonRole::Primary)
                    .on_view(move || {
                        copying(PairDeviceEffect::Copy {
                            link: copied.clone(),
                        });
                    }),
            )),
        ),
        caption(&device.in_tilo),
    ];
    Box::new(Section::new(parts))
}

/// What the shell lends the sheet: whether it is up, what it shows, and where its requests go.
#[derive(Clone, Default)]
pub struct AddDeviceState {
    presentation: Presentation,
    shown: Rc<RefCell<AddDevice>>,
    lent: Rc<RefCell<Option<LocalState<AddDevice>>>>,
    asked: Rc<RefCell<Vec<PairDeviceEffect>>>,
}

impl std::fmt::Debug for AddDeviceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddDeviceState")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl SheetState for AddDeviceState {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl AddDeviceState {
    /// Show `device`: the link as it stands now.
    pub fn show(&self, device: AddDevice) {
        if *self.shown.borrow() == device {
            return;
        }
        if let Some(shown) = self.lent.borrow().as_ref() {
            shown.set(device.clone());
        }
        *self.shown.borrow_mut() = device;
        self.presentation.wake();
    }

    /// What the sheet shows now.
    #[must_use]
    pub fn device(&self) -> AddDevice {
        self.shown.borrow().clone()
    }

    /// The requests made since the session last asked, oldest first.
    #[must_use]
    pub fn take_asked(&self) -> Vec<PairDeviceEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }
}

/// The sheet as an app.
pub struct AddDeviceApp;

impl ResourceApp for AddDeviceApp {
    type Resource = AddDeviceLink;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Add a device";
    const ICON: Option<&'static str> = Some("monitor-smartphone");
    // Over the desktop: what is not the sheet shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<AddDeviceLink>) -> Element {
        let state = cx.take_lent::<AddDeviceState>();
        let initial = match &state {
            Some(state) => state.device(),
            None => cx
                .fixture::<AddDevice>()
                .map(|device| (*device).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        let link = cx.local(String::new());
        if let Some(state) = &state {
            *state.lent.borrow_mut() = Some(shown.clone());
            // The sheet follows what the shell says of its presentation: being opened, taken down,
            // put on another display.
            state.presentation.wake_with(cx.waker());
        }
        let asking = state.clone();
        let presentation = state.as_ref().map(|state| state.presentation().clone());
        cx.effects(move |effect: PairDeviceEffect, _cx| match &asking {
            Some(state) => {
                if effect == PairDeviceEffect::Done {
                    // Its own close: the card leaves the screen, and the shell tells it gone once
                    // nothing of it is drawn.
                    state.presentation.close();
                }
                state.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no session to add a device of".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let ask: Ask = Rc::new(move |effect: PairDeviceEffect| dispatch.effect(&effect));
        cx.switch(&shown, move |cx, device| {
            let device = device.clone();
            let writing = link.clone();
            let ask = ask.clone();
            let presentation = presentation.clone();
            cx.switch(&link, move |_cx, _shown| {
                sheet(&device, &writing, &ask, presentation.as_ref())
            })
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &["ready", "ticket", "refused"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<AddDeviceLink>) {
        let device = match fixture.scenario() {
            Some("ticket") => AddDevice::ticket(SAMPLE_TICKET, 9),
            Some("refused") => AddDevice::refusing(&word("pair-device-no-workspace")),
            _ => AddDevice::of(SAMPLE_LINK, 9),
        };
        fixture.provide(device);
    }
}

/// The code a sample sheet shows, in its captures.
const SAMPLE_LINK: &str = "https://tilo.coffee/#/@eu1.tilo.coffee/w/\
0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c?pair=abababababababababababababababababababab\
abababababababababababab0123456789abcdef0123456789abcdef";

/// The ticket a sample sheet shows, in its captures.
const SAMPLE_TICKET: &str = "tilo1qqqsyqcyq5rqwzqfpg9scrgwpugpzysnzs23v9ccrydpk8qarc0jqgqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(AddDeviceApp)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects,
        reason = "a failed step in a test is the assertion; pixels sit well inside the viewport"
    )]

    use super::*;

    tilo_test_env::scenario! {
        /// `code_of`: the code the sheet draws for the link it shows.
        fn the_code_drawn_carries_the_link_now_shown;

        phase the_code_carries_the_link_the_session_made;
        // The code the sheet draws carries the link the session handed it, byte for byte: a phone
        // scanning it reads the link that admits the new device.
        {
            let device = AddDevice::of(SAMPLE_LINK, 9);
            let code = code_of(&device);
            assert!(code.carries(SAMPLE_LINK.as_bytes()));
            assert!(code.error().is_none(), "the link fits a code");
            assert_eq!(code.alt(), device.alt);
        }

        phase a_refreshed_link_is_the_one_the_code_carries;
        // A link that ran out is replaced by a new one, and the code the sheet draws is that new
        // link: the person never scans a code that admits nothing.
        {
            const LATER: &str = "https://tilo.coffee/#/@eu1.tilo.coffee/w/\
                             0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c?pair=fefefefefefefefefefefefefefefefe\
                             fefefefefefefefefefefefefefefefedcba9876543210fedcba9876543210";
            let state = AddDeviceState::default();
            state.show(AddDevice::of(SAMPLE_LINK, 9));
            state.show(AddDevice::of(LATER, 10));
            let code = code_of(&state.device());
            assert!(code.carries(LATER.as_bytes()));
            assert!(!code.carries(SAMPLE_LINK.as_bytes()));
        }

        phase the_ready_sheet_says_its_minutes_and_keeps_the_link_in_its_field;
        // The ready sheet: the code, how many minutes it works as a whole sentence in each
        // language, the link with its button and the one way to use it nobody guesses; no list of
        // ways the code and the link already say. The link stops at its field's inset.
        {
            const VIEWPORT: (u32, u32) = (720, 900);
            let mut app = ui::framework::testing::HeadlessApp::<AddDeviceApp>::capture_at(
                Some("ready"),
                VIEWPORT,
            )
            .expect("the sheet is staged");
            let texts = app.texts();
            for line in [
                "Works once, for the next 9 minutes.",
                "Copy link",
                "On a computer with Tilo, paste the link into Join a workspace.",
            ] {
                assert!(texts.contains(&line.to_owned()), "{line} in {texts:?}");
            }
            assert!(!texts.contains(&"Phone or tablet".to_owned()), "{texts:?}");
            let field = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| node.role() == ui::accesskit::Role::TextInput)
                .and_then(|(_, node)| node.bounds())
                .expect("the link's field");
            let pixels = app.harness().render_rgba();
            assert_eq!(pixels.len(), 720 * 900 * 4, "drawn at one pixel a point");
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "pixels inside the viewport"
            )]
            let (right, middle) = (field.x1 as u32, f64::midpoint(field.y0, field.y1) as u32);
            let at = |x, y| ui::offscreen::OffscreenRenderer::sample(&pixels, VIEWPORT.0, x, y);
            let ground = at(right - 4, middle);
            let inset = (right - 10..right - 3)
                .all(|x| (middle - 6..middle + 6).all(|y| at(x, y) == ground));
            assert!(inset, "no ink in the field's inset");
            let one = AddDevice::of(SAMPLE_LINK, 1).lasting;
            assert_eq!(one, "Works once, for the next minute.");
            ui::i18n::I18nManager::set_locale("de-DE");
            let nine = AddDevice::of(SAMPLE_LINK, 9).lasting;
            assert_eq!(nine, "Gilt einmal, für die nächsten 9 Minuten.");
            let one = AddDevice::of(SAMPLE_LINK, 1).lasting;
            assert_eq!(one, "Gilt einmal, für die nächste Minute.");
        }
    }
}

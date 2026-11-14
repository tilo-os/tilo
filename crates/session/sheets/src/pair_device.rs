//! "Add a device": the link that makes a new device of yours one of yours, shown as a code a phone
//! camera reads.
//!
//! The sheet shows the link as a code, as text to open on a browser, and a button that copies it,
//! with the three ways a new device uses it and how long it works. The session makes the link and
//! hands it here ([`AddDeviceSeat::show`]): one invite is open at a time, so a link the person
//! leaves on screen is still the one that admits their device, and the code they see is replaced by
//! a fresh one only once the one shown stopped working.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{Effected, ResourceApp, ViewCx};
use ui::icons::Icon;
use ui::prelude::*;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Element, ListRow, ModalDialog, QrWidget, Row, Section, Text,
    TextInput,
};

use crate::presentation::{Presentation, Seat, presented};
use crate::{word, words};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.pair-device";

/// The widest the code is drawn, quiet zone and all: big enough for a phone held near the screen
/// to read, and small enough that the link and its ways, not the code, are what the sheet is about.
pub const CODE_MAX_PX: usize = 200;

/// The narrowest each module is drawn: below this the squares turn to speckle.
pub const CODE_MODULE_PX: u32 = 2;

/// The widest each module is drawn: a pairing link is a few dozen modules, and squares this wide
/// make it conspicuous rather than a stamp.
pub const CODE_MODULE_MAX_PX: u32 = 5;

/// The sheet's resource: the door its actions are published on. It holds nothing; the link is the
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

/// One way a new device uses the link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Way {
    /// What it is, as the sheet lists it.
    pub title: String,
    /// How it uses the link.
    pub detail: String,
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
    /// The ways a new device uses the link.
    pub ways: Vec<Way>,
    /// How long this code works, in the person's words.
    pub lasting: String,
    /// Why it is safe to leave on screen.
    pub note: String,
    /// Why there is no link to show, when there is none.
    pub refusal: Option<String>,
}

impl AddDevice {
    /// The sheet showing `link`, whose invite stops working `left`, in the person's language: what
    /// the session hands the seat each time the link it shows changes.
    #[must_use]
    pub fn of(link: &str, left: &str) -> Self {
        Self {
            title: word("pair-device-title"),
            description: word("pair-device-description"),
            close: word("pair-device-close"),
            link: link.to_owned(),
            link_label: word("pair-device-link-label"),
            copy: word("pair-device-copy"),
            copied: word("pair-device-copied"),
            alt: word("pair-device-code-alt"),
            ways: vec![
                Way {
                    title: word("pair-device-way-phone"),
                    detail: word("pair-device-way-phone-how"),
                },
                Way {
                    title: word("pair-device-way-browser"),
                    detail: word("pair-device-way-browser-how"),
                },
                Way {
                    title: word("pair-device-way-tilo"),
                    detail: word("pair-device-way-tilo-how"),
                },
            ],
            lasting: words("pair-device-lasting", &[("left", left)]),
            note: word("pair-device-note"),
            refusal: None,
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

    /// The sheet for a workspace no host on the web keeps: a link would lead nowhere, so it says
    /// how to add the device nearby.
    #[must_use]
    pub fn nearby() -> Self {
        Self::refusing(&word("pair-device-nearby"))
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
    let mut body = Column::new()
        .with_intrinsic_height()
        .with_no_padding()
        .with_rhythm(Rhythm::Section);
    match &device.refusal {
        Some(reason) => body = body.push(Box::new(Text::new(reason).with_role(TextRole::Body))),
        None => body = body.push(code(device, link, ask)),
    }
    let dismissed = ask.clone();
    let escaped = ask.clone();
    Box::new(presented(
        presentation,
        ModalDialog::new(device.title.clone(), body)
            .with_test_id("pair-device")
            .with_icon(Icon::MonitorSmartphone)
            .with_description(device.description.clone())
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .secondary_action(device.close.clone(), move || {
                dismissed(PairDeviceEffect::Done);
            })
            .on_dismiss(move || escaped(PairDeviceEffect::Done)),
    ))
}

/// The code the sheet draws for `device`: the link it was handed, as squares a camera reads.
fn code_of(device: &AddDevice) -> QrWidget {
    let code = QrWidget::new(device.link.clone().into_bytes())
        .with_secret()
        .with_id("pair-device-code")
        .with_alt(device.alt.clone());
    let px = module_px(code.total_modules());
    code.with_module_px(px)
}

/// The link as a code a camera reads, as text to open or paste, with the button that copies it,
/// the ways a new device uses it and how long it works.
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
    let mut parts: Vec<Element> = vec![
        Box::new(
            Row::new()
                .with_justify(ui::taffy::JustifyContent::Center)
                .push(Box::new(code_of(device))),
        ),
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
    ];
    let ways: Vec<Element> = device
        .ways
        .iter()
        .map(|way| {
            Box::new(
                ListRow::new(
                    format!("pair-device-way-{}", slug(&way.title)),
                    way.title.clone(),
                )
                .secondary(way.detail.clone()),
            ) as Element
        })
        .collect();
    parts.push(Box::new(Section::new(ways)));
    parts.push(caption(&device.note));
    Box::new(Section::new(parts))
}

/// `text` as part of a widget id: its letters and digits, joined by dashes.
fn slug(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// The size each module is drawn at, so the whole code takes about [`CODE_MAX_PX`].
#[must_use]
pub fn module_px(modules: usize) -> u32 {
    u32::try_from(CODE_MAX_PX.checked_div(modules).unwrap_or(CODE_MAX_PX))
        .unwrap_or(CODE_MODULE_MAX_PX)
        .clamp(CODE_MODULE_PX, CODE_MODULE_MAX_PX)
}

/// The words under the code and the link, small.
fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption))
}

/// What the sheet asks for, as a press in it hands it back.
pub type Ask = Rc<dyn Fn(PairDeviceEffect)>;

/// What the shell lends the sheet: whether it is up, what it shows, and where its requests go.
#[derive(Clone, Default)]
pub struct AddDeviceSeat {
    presentation: Presentation,
    shown: Rc<RefCell<AddDevice>>,
    lent: Rc<RefCell<Option<LocalState<AddDevice>>>>,
    asked: Rc<RefCell<Vec<PairDeviceEffect>>>,
}

impl std::fmt::Debug for AddDeviceSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddDeviceSeat")
            .field("presentation", &self.presentation)
            .finish_non_exhaustive()
    }
}

impl Seat for AddDeviceSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl AddDeviceSeat {
    /// Show `device`: the link as it stands now.
    pub fn show(&self, device: AddDevice) {
        if *self.shown.borrow() == device {
            return;
        }
        if let Some(shown) = self.lent.borrow().as_ref() {
            shown.set(device.clone());
        }
        *self.shown.borrow_mut() = device;
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
        let seat = cx.take_lent::<AddDeviceSeat>();
        let initial = match &seat {
            Some(seat) => seat.device(),
            None => cx
                .fixture::<AddDevice>()
                .map(|device| (*device).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(initial);
        let link = cx.local(String::new());
        if let Some(seat) = &seat {
            *seat.lent.borrow_mut() = Some(shown.clone());
            // The sheet follows what the shell says of its presentation: being opened, taken down,
            // put on another display.
            seat.presentation.wake_with(cx.waker());
        }
        let asking = seat.clone();
        let presentation = seat.as_ref().map(|seat| seat.presentation().clone());
        cx.effects(move |effect: PairDeviceEffect, _cx| match &asking {
            Some(seat) => {
                if effect == PairDeviceEffect::Done {
                    // Its own close: the card leaves the screen, and the shell tells it gone once
                    // nothing of it is drawn.
                    seat.presentation.close();
                }
                seat.asked.borrow_mut().push(effect);
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
        &["ready", "nearby", "refused"]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<AddDeviceLink>) {
        let device = match fixture.scenario() {
            Some("nearby") => AddDevice::nearby(),
            Some("refused") => AddDevice::refusing(&word("pair-device-no-workspace")),
            _ => AddDevice::of(SAMPLE_LINK, "9 min"),
        };
        fixture.provide(device);
    }
}

/// The code a sample sheet shows, in its captures.
const SAMPLE_LINK: &str = "https://tilo.coffee/#/@eu1.tilo.coffee/w/\
0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c?pair=abababababababababababababababababababab\
abababababababababababab0123456789abcdef0123456789abcdef";

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(AddDeviceApp)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::*;

    /// The code the sheet draws carries the link the session handed it, byte for byte: a phone
    /// scanning it reads the link that admits the new device.
    #[test]
    fn the_code_carries_the_link_the_session_made() {
        let device = AddDevice::of(SAMPLE_LINK, "9 min");
        let code = code_of(&device);
        assert!(code.carries(SAMPLE_LINK.as_bytes()));
        assert!(code.error().is_none(), "the link fits a code");
        assert_eq!(code.alt(), device.alt);
    }

    /// A link that ran out is replaced by a new one, and the code the sheet draws is that new
    /// link: the person never scans a code that admits nothing.
    #[test]
    fn a_refreshed_link_is_the_one_the_code_carries() {
        const LATER: &str = "https://tilo.coffee/#/@eu1.tilo.coffee/w/\
                             0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c?pair=fefefefefefefefefefefefefefefefe\
                             fefefefefefefefefefefefefefefefedcba9876543210fedcba9876543210";
        let seat = AddDeviceSeat::default();
        seat.show(AddDevice::of(SAMPLE_LINK, "9 min"));
        seat.show(AddDevice::of(LATER, "10 min"));
        let code = code_of(&seat.device());
        assert!(code.carries(LATER.as_bytes()));
        assert!(!code.carries(SAMPLE_LINK.as_bytes()));
    }
}

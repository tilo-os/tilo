//! The Super+P display sheet: where the desktop appears.
//!
//! The upper section switches the whole desktop between the familiar
//! layouts (extend, mirror, only this screen, only external) that the
//! attached displays allow, and only those: with one display it says so and
//! offers what is reachable instead. A choice stages a display change the
//! compositor applies at once and restores unless the person keeps it, so
//! Keep and Revert stand under the layouts while it waits. Between them and
//! the wireless receivers, the person's other devices' screens are listed,
//! each with whether it takes part in their shared display layout. Wireless
//! receivers are the lower section, with the chosen one's stream options
//! and, for an `AirPlay` receiver, its PIN.
//!
//! Up and Down walk the sheet in that order, Enter applies what the
//! highlight is on, Super+P moves to the next layout, 1 to 4 choose a layout
//! at once, Escape closes. The shell holds the displays, the receivers and
//! the change waiting for an answer, carries out every choice through its
//! own published display actions and tells the sheet what to show
//! ([`DisplaysSeat`]). A PIN typed here goes to the shell through the seat,
//! never through an action.

use std::cell::RefCell;
use std::rc::Rc;

use ui::advanced::WindowEvent;
use ui::framework::{Dispatcher, Effected, ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::{
    Button, ButtonRole, Column, Element, ListRow, ModalDialog, Row, Text, TextInput, Widget,
};

use crate::presentation::Presentation;
use crate::{t, t_args};

/// The app the shell mounts the sheet as.
pub const APP_ID: &str = "so.tilo.display-sheet";

/// The sheet's resource: the door its choices are published on. It holds
/// nothing; the displays are the compositor's.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct DisplaySheet {}

#[resource(kind = "display-sheet.layouts", label = "Displays")]
impl DisplaySheet {}

/// What the sheet asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "so.tilo.display-sheet.choose")]
pub enum DisplaysEffect {
    /// Spread the desktop across every display
    #[effect(own_device)]
    Extend,
    /// Show the same picture on every display
    #[effect(own_device)]
    Mirror,
    /// Show the desktop on the built-in screen only
    #[effect(own_device)]
    BuiltinOnly,
    /// Show the desktop on external displays only
    #[effect(own_device)]
    ExternalOnly,
    /// Keep the display change waiting for an answer
    #[effect(own_device, attended)]
    Keep,
    /// Revert the display change waiting for an answer
    #[effect(own_device, idempotent)]
    Revert,
    /// Include one of your other devices' screens in your shared display layout, or leave it out
    #[effect(own_device)]
    Include {
        #[param("The device the screen belongs to")]
        device: String,
        #[param("The screen, by its output name on that device")]
        output: String,
        #[param("The arrangement revision the choice was made against")]
        revision: u64,
        #[param("Whether the screen takes part in the layout")]
        include: bool,
    },
    /// Choose a wireless display to stream to
    #[effect(own_device, kind = "read")]
    Choose {
        #[param("The receiver, by its id in the list")]
        receiver: String,
    },
    /// Stream the desktop to a wireless display
    #[effect(own_device)]
    Connect {
        #[param("The receiver, by its id in the list")]
        receiver: String,
    },
    /// Stop streaming to the wireless display
    #[effect(own_device, idempotent)]
    Stop,
    /// Look for wireless displays again
    #[effect(own_device, kind = "read")]
    Search,
    /// Share the computer's sound with the wireless display
    #[effect(own_device)]
    ShareAudio {
        #[param("Whether sound goes along")]
        on: bool,
    },
    /// Let the stream's quality follow the network
    #[effect(own_device)]
    AdaptiveQuality {
        #[param("Whether quality adapts")]
        on: bool,
    },
    /// Step the stream's resolution limit to the next one
    #[effect(own_device)]
    NextResolution,
    /// Set the stream's frame rate
    #[effect(own_device)]
    FrameRate {
        #[param("Frames per second: 30 or 60")]
        fps: u32,
    },
    /// Close the display sheet
    #[effect(own_device, kind = "read")]
    Close,
}

/// A desktop layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Layout {
    Extend,
    Mirror,
    BuiltinOnly,
    ExternalOnly,
}

impl Layout {
    /// Every layout, in the order the sheet lists them and 1 to 4 choose.
    pub const ALL: [Self; 4] = [
        Self::Extend,
        Self::Mirror,
        Self::BuiltinOnly,
        Self::ExternalOnly,
    ];

    /// The choice that is this layout.
    pub const fn effect(self) -> DisplaysEffect {
        match self {
            Self::Extend => DisplaysEffect::Extend,
            Self::Mirror => DisplaysEffect::Mirror,
            Self::BuiltinOnly => DisplaysEffect::BuiltinOnly,
            Self::ExternalOnly => DisplaysEffect::ExternalOnly,
        }
    }

    /// The layout a choice is, when it is one.
    pub const fn of(effect: &DisplaysEffect) -> Option<Self> {
        match effect {
            DisplaysEffect::Extend => Some(Self::Extend),
            DisplaysEffect::Mirror => Some(Self::Mirror),
            DisplaysEffect::BuiltinOnly => Some(Self::BuiltinOnly),
            DisplaysEffect::ExternalOnly => Some(Self::ExternalOnly),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Extend => "extend",
            Self::Mirror => "mirror",
            Self::BuiltinOnly => "builtin-only",
            Self::ExternalOnly => "external-only",
        }
    }

    fn title(self) -> String {
        t(&format!("display-mode-{}", self.key()))
    }

    fn detail(self) -> String {
        t(&format!("display-mode-{}-detail", self.key()))
    }

    fn icon(self) -> ui::icons::Icon {
        match self {
            Self::Extend => ui::icons::Icon::Columns2,
            Self::Mirror => ui::icons::Icon::Copy,
            Self::BuiltinOnly => ui::icons::Icon::PanelLeft,
            Self::ExternalOnly => ui::icons::Icon::PanelRight,
        }
    }
}

/// One layout the sheet offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutRow {
    pub layout: Layout,
    /// It can be chosen now: no earlier choice is still being saved.
    pub enabled: bool,
    /// The layout the desktop is in.
    pub current: bool,
    /// Choosing it stages a change on the attached displays, which then
    /// waits for Keep or Revert; otherwise it only sets the stream's layout.
    pub stages: bool,
}

/// What stands under the layouts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Band {
    #[default]
    Idle,
    /// A layout choice is being saved.
    Saving,
    /// A staged change waits for Keep or Revert.
    Confirm {
        /// The countdown, or what became of the answer.
        status: String,
        /// An answer is being recorded: the buttons wait.
        answering: bool,
    },
    /// The last choice was refused, and why.
    Refused(String),
}

/// One of the person's other devices' screens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screen {
    pub device: String,
    pub output: String,
    /// The arrangement revision of this screen as the sheet shows it.
    pub revision: u64,
    pub name: String,
    pub number: usize,
    pub device_label: String,
    pub online: bool,
    /// Its device shows something on it.
    pub active: bool,
    /// Takes part in the shared layout.
    pub enabled: bool,
}

impl Screen {
    fn title(&self) -> String {
        t_args(
            "display-remote-title",
            &[
                ("number", ui::i18n::ArgValue::Text(self.number.to_string())),
                ("name", ui::i18n::ArgValue::Text(self.name.clone())),
                (
                    "device",
                    ui::i18n::ArgValue::Text(self.device_label.clone()),
                ),
            ],
        )
    }

    fn detail(&self) -> String {
        t(match (self.online, self.enabled, self.active) {
            (false, false, _) => "display-remote-offline-excluded",
            (false, true, _) => "display-remote-offline",
            (true, false, _) => "display-remote-out-of-layout",
            (true, true, false) => "display-remote-switched-off",
            (true, true, true) => "display-remote-in-layout",
        })
    }
}

/// Where a receiver's stream stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Link {
    #[default]
    Idle,
    Connecting,
    Connected,
    /// The last attempt with it failed, and how.
    Failed(String),
}

/// A wireless display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Receiver {
    pub id: String,
    pub name: String,
    /// How it is reached (`AirPlay`, Google Cast…).
    pub protocol: String,
    /// What else it says about itself.
    pub detail: String,
    pub link: Link,
    /// It may ask for a PIN or password before it streams.
    pub asks_pin: bool,
    /// The receiver the stream options are for.
    pub chosen: bool,
}

/// What the receiver list says while it lists none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Discovery {
    #[default]
    NoneFound,
    Searching,
    /// Looking failed: what went wrong and what to do.
    Failed {
        title: String,
        next_step: String,
    },
}

/// The chosen receiver's stream options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub audio: bool,
    pub adaptive: bool,
    /// The resolution limit, as it is named.
    pub resolution: String,
    pub fps: u32,
}

/// What the sheet shows: plain data the shell keeps current.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sheet {
    /// The layouts that can be chosen here, in their usual order; none with
    /// one display and no receiver.
    pub layouts: Vec<LayoutRow>,
    pub band: Band,
    pub screens: Vec<Screen>,
    /// The person may change the shared layout here.
    pub screens_editable: bool,
    pub receivers: Vec<Receiver>,
    pub discovery: Discovery,
    pub stream: Option<Stream>,
    /// The chosen receiver may ask for its PIN.
    pub asks_pin: bool,
}

impl Sheet {
    fn current(&self) -> Option<Layout> {
        self.layouts
            .iter()
            .find(|row| row.current)
            .map(|row| row.layout)
    }

    fn row(&self, layout: Layout) -> Option<&LayoutRow> {
        self.layouts.iter().find(|row| row.layout == layout)
    }

    fn confirming(&self) -> bool {
        matches!(self.band, Band::Confirm { .. })
    }

    fn answering(&self) -> bool {
        matches!(
            self.band,
            Band::Confirm {
                answering: true,
                ..
            }
        )
    }

    /// What the sheet says about itself before its keys: the choice being
    /// saved, or why the last one was refused.
    fn description(&self) -> String {
        let status = match &self.band {
            Band::Saving => Some(t("display-sheet-saving-layout")),
            Band::Refused(reason) if !reason.is_empty() => Some(reason.clone()),
            _ => None,
        };
        status.map_or_else(
            || t("display-sheet-a11y-description"),
            |status| {
                // A status that ends its own sentence ("Saving…") gets no
                // second full stop.
                let stop = if status.ends_with(['.', '…', '!', '?']) {
                    ""
                } else {
                    "."
                };
                format!("{status}{stop} {}", t("display-sheet-a11y-description"))
            },
        )
    }

    /// Where the keyboard can be, in the order Up and Down walk it.
    fn spots(&self) -> Vec<Spot> {
        let mut spots: Vec<Spot> = self
            .layouts
            .iter()
            .map(|row| Spot::Layout(row.layout))
            .collect();
        if self.confirming() {
            spots.extend([Spot::Keep, Spot::Revert]);
        }
        spots.extend(self.screens.iter().map(|screen| Spot::Screen {
            device: screen.device.clone(),
            output: screen.output.clone(),
        }));
        if self.asks_pin {
            spots.push(Spot::Pin);
        }
        spots.extend(self.receivers.iter().map(|receiver| Spot::Receiver {
            id: receiver.id.clone(),
        }));
        spots
    }

    /// Where the keyboard starts: the current layout, else the first that
    /// can be chosen, else the other devices' screens.
    fn first_spot(&self) -> Option<Spot> {
        self.current()
            .or_else(|| {
                self.layouts
                    .iter()
                    .find(|row| row.enabled)
                    .map(|row| row.layout)
            })
            .map(Spot::Layout)
            .or_else(|| self.spots().into_iter().next())
    }
}

/// Where the keyboard is in the sheet. Rows are known by who they are, so
/// the highlight stays on a screen another device moved in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Spot {
    Layout(Layout),
    Keep,
    Revert,
    Screen { device: String, output: String },
    Pin,
    Receiver { id: String },
}

/// What the view shows: the sheet and where the keyboard is.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shown {
    sheet: Sheet,
    /// `None` until the person moves it: the sheet's first spot.
    spot: Option<Spot>,
}

impl Shown {
    /// Where the keyboard is, on a spot the sheet still has.
    fn spot(&self) -> Option<Spot> {
        let spots = self.sheet.spots();
        self.spot
            .clone()
            .filter(|spot| spots.contains(spot))
            .or_else(|| self.sheet.first_spot())
    }
}

/// What the shell lends the sheet: whether it is up, what it shows, the
/// choices made in it and the PIN typed into it.
#[derive(Clone, Default)]
pub struct DisplaysSeat {
    presentation: Presentation,
    sheet: Rc<RefCell<Sheet>>,
    shown: Rc<RefCell<Option<LocalState<Shown>>>>,
    asked: Rc<RefCell<Vec<DisplaysEffect>>>,
    pin: Rc<RefCell<String>>,
    /// The opening the highlight was placed for.
    opening: Rc<RefCell<u64>>,
}

impl std::fmt::Debug for DisplaysSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplaysSeat")
            .field("presentation", &self.presentation)
            .field("sheet", &self.sheet.borrow())
            .finish_non_exhaustive()
    }
}

impl crate::Seat for DisplaysSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl DisplaysSeat {
    /// Show `sheet`; a new opening starts the highlight afresh.
    pub fn show(&self, sheet: Sheet) {
        let opening = self.presentation.openings();
        let fresh = *self.opening.borrow() != opening;
        if !fresh && *self.sheet.borrow() == sheet {
            return;
        }
        *self.opening.borrow_mut() = opening;
        if let Some(shown) = self.shown.borrow().as_ref() {
            shown.update(|shown| {
                shown.sheet = sheet.clone();
                if fresh {
                    shown.spot = None;
                }
            });
        }
        if fresh {
            self.pin.borrow_mut().clear();
        }
        *self.sheet.borrow_mut() = sheet;
        self.presentation.wake();
    }

    /// What the sheet shows now.
    pub fn sheet(&self) -> Sheet {
        self.sheet.borrow().clone()
    }

    /// The choices made since the shell last looked, oldest first.
    pub fn take_asked(&self) -> Vec<DisplaysEffect> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// The PIN or password typed for the chosen receiver; empty when none.
    pub fn pin(&self) -> String {
        self.pin.borrow().clone()
    }
}

/// The sheet as an app.
pub struct DisplaysApp;

impl ResourceApp for DisplaysApp {
    type Resource = DisplaySheet;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = "Displays";
    const ICON: Option<&'static str> = Some("monitor");
    // Over the desktop: the sheet's scrim shows the desktop under it.
    const BEHAVIOUR: ui::application::ComponentBehaviour = ui::application::ComponentBehaviour {
        window: ui::application::WindowRole::Sheet,
        ..ui::application::ComponentBehaviour::DEFAULT
    };
    // Nothing is kept: there is no past to look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::CATALOG)
    }

    fn view(cx: &mut ViewCx<DisplaySheet>) -> Element {
        let seat = cx.take_lent::<DisplaysSeat>();
        let initial = match &seat {
            Some(seat) => seat.sheet(),
            None => cx
                .fixture::<Sheet>()
                .map(|sheet| (*sheet).clone())
                .unwrap_or_default(),
        };
        let shown = cx.local(Shown {
            sheet: initial,
            spot: None,
        });
        let pin = cx.local(String::new());
        if let Some(seat) = &seat {
            *seat.shown.borrow_mut() = Some(shown.clone());
            seat.presentation.wake_with(cx.waker());
        }
        let asking = seat.clone();
        cx.effects(move |effect: DisplaysEffect, _cx| match &asking {
            Some(seat) => {
                if effect == DisplaysEffect::Close {
                    seat.presentation.dismissed();
                }
                seat.asked.borrow_mut().push(effect);
                Ok(Effected::done())
            }
            None => Err("no displays to arrange here".to_owned()),
        });
        let dispatch = cx.dispatcher();
        let parts = Parts {
            dispatch: dispatch.clone(),
            shown: shown.clone(),
            pin,
            seat,
        };
        let content = cx.switch(&shown, move |_cx, shown| sheet(shown, &parts));
        Box::new(Keys {
            content,
            dispatch,
            shown,
        })
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "two-displays",
            "confirming",
            "one-display",
            "other-screens",
            "receivers",
            "refused",
        ]
    }

    fn fixture(fixture: &mut ui::framework::Fixture<DisplaySheet>) {
        let sheet = match fixture.scenario() {
            Some("confirming") => Sheet {
                layouts: two_display_layouts(Layout::Mirror),
                band: Band::Confirm {
                    status: "Reverting in 12 s".to_owned(),
                    answering: false,
                },
                ..Sheet::default()
            },
            Some("one-display") => Sheet {
                discovery: Discovery::Searching,
                ..Sheet::default()
            },
            Some("other-screens") => Sheet {
                screens: sample_screens(),
                screens_editable: true,
                ..Sheet::default()
            },
            Some("receivers") => Sheet {
                layouts: [Layout::Extend, Layout::Mirror, Layout::ExternalOnly]
                    .into_iter()
                    .map(|layout| LayoutRow {
                        layout,
                        enabled: true,
                        current: layout == Layout::Mirror,
                        stages: false,
                    })
                    .collect(),
                receivers: sample_receivers(),
                stream: Some(Stream {
                    audio: true,
                    adaptive: true,
                    resolution: "1080p".to_owned(),
                    fps: 60,
                }),
                asks_pin: true,
                ..Sheet::default()
            },
            Some("refused") => Sheet {
                layouts: two_display_layouts(Layout::Extend),
                band: Band::Refused(
                    "Your workspace role no longer permits changing displays".to_owned(),
                ),
                ..Sheet::default()
            },
            _ => Sheet {
                layouts: two_display_layouts(Layout::Extend),
                ..Sheet::default()
            },
        };
        fixture.provide(sheet);
    }
}

/// A laptop with one monitor: every layout, `current` the one in use.
fn two_display_layouts(current: Layout) -> Vec<LayoutRow> {
    Layout::ALL
        .into_iter()
        .map(|layout| LayoutRow {
            layout,
            enabled: true,
            current: layout == current,
            stages: true,
        })
        .collect()
}

fn sample_screens() -> Vec<Screen> {
    vec![
        Screen {
            device: "desk-pc".to_owned(),
            output: "DP-1".to_owned(),
            revision: 3,
            name: "Dell U2723QE".to_owned(),
            number: 2,
            device_label: "Desk PC".to_owned(),
            online: true,
            active: true,
            enabled: true,
        },
        Screen {
            device: "tablet".to_owned(),
            output: "DSI-1".to_owned(),
            revision: 1,
            name: "Built-in".to_owned(),
            number: 3,
            device_label: "Tablet".to_owned(),
            online: false,
            active: false,
            enabled: false,
        },
    ]
}

fn sample_receivers() -> Vec<Receiver> {
    vec![
        Receiver {
            id: "airplay:living-room".to_owned(),
            name: "Living room TV".to_owned(),
            protocol: "AirPlay".to_owned(),
            detail: String::new(),
            link: Link::Idle,
            asks_pin: true,
            chosen: true,
        },
        Receiver {
            id: "cast:kitchen".to_owned(),
            name: "Kitchen display".to_owned(),
            protocol: "Google Cast".to_owned(),
            detail: "Nest Hub".to_owned(),
            link: Link::Idle,
            asks_pin: false,
            chosen: false,
        },
    ]
}

/// What every part of the view acts through.
#[derive(Clone)]
struct Parts {
    dispatch: Dispatcher<DisplaySheet>,
    shown: LocalState<Shown>,
    pin: LocalState<String>,
    seat: Option<DisplaysSeat>,
}

impl Parts {
    /// Carry out what the keyboard or a press chose, and move the highlight
    /// where it goes next.
    fn act(&self, spot: &Spot) {
        let Shown { sheet, .. } = self.shown.get();
        let next = activate(&sheet, spot, &self.dispatch);
        self.shown.update(|shown| shown.spot = next);
    }

    fn send(&self, effect: &DisplaysEffect) {
        self.dispatch.effect(effect);
    }

    /// The PIN is submitted: the shell connects with it, reading it from
    /// the seat when it carries out the choice, so the seat keeps it while
    /// the field is emptied.
    fn submit_pin(&self) {
        let Shown { sheet, .. } = self.shown.get();
        if let Some(receiver) = sheet.receivers.iter().find(|receiver| receiver.chosen) {
            self.send(&DisplaysEffect::Connect {
                receiver: receiver.id.clone(),
            });
        }
        self.pin.set(String::new());
    }
}

/// Carry out `spot` on `sheet`; where the highlight goes after.
fn activate(sheet: &Sheet, spot: &Spot, dispatch: &Dispatcher<DisplaySheet>) -> Option<Spot> {
    match spot {
        Spot::Layout(layout) => {
            let row = sheet.row(*layout)?;
            if !row.enabled {
                return Some(spot.clone());
            }
            dispatch.effect(&layout.effect());
            // A staged change waits for its answer, which the keyboard
            // goes to.
            Some(if row.stages { Spot::Keep } else { spot.clone() })
        }
        Spot::Keep | Spot::Revert => {
            if !sheet.answering() {
                dispatch.effect(if *spot == Spot::Keep {
                    &DisplaysEffect::Keep
                } else {
                    &DisplaysEffect::Revert
                });
            }
            None
        }
        Spot::Screen { device, output } => {
            if let Some(screen) = sheet
                .screens
                .iter()
                .find(|screen| &screen.device == device && &screen.output == output)
                && sheet.screens_editable
            {
                dispatch.effect(&DisplaysEffect::Include {
                    device: screen.device.clone(),
                    output: screen.output.clone(),
                    revision: screen.revision,
                    include: !screen.enabled,
                });
            }
            Some(spot.clone())
        }
        Spot::Pin => Some(Spot::Pin),
        Spot::Receiver { id } => {
            let receiver = sheet.receivers.iter().find(|receiver| &receiver.id == id)?;
            let receiver_id = receiver.id.clone();
            match receiver.link {
                Link::Connected => dispatch.effect(&DisplaysEffect::Stop),
                Link::Connecting => {}
                // A receiver that may ask for a PIN is chosen first, so the
                // PIN can be typed before it connects.
                _ if receiver.asks_pin && !receiver.chosen => {
                    dispatch.effect(&DisplaysEffect::Choose {
                        receiver: receiver_id,
                    });
                    return Some(Spot::Pin);
                }
                _ => dispatch.effect(&DisplaysEffect::Connect {
                    receiver: receiver_id,
                }),
            }
            Some(spot.clone())
        }
    }
}

/// The whole sheet.
fn sheet(shown: &Shown, parts: &Parts) -> Element {
    let sheet = &shown.sheet;
    let spot = shown.spot();
    let gap = ui::tokens::Spacing::BASE.s3;
    let mut body = Column::new().with_gap(gap).push(Box::new(
        Text::new(&t("display-sheet-subtitle")).with_role(TextRole::Caption),
    ));
    body = body.push(layouts(sheet, spot.as_ref(), parts));
    if let Some(band) = band(sheet, spot.as_ref(), parts) {
        body = body.push(band);
    }
    if !sheet.screens.is_empty() {
        body = body.push(screens(sheet, spot.as_ref(), parts));
    }
    body = body.push(wireless(sheet, spot.as_ref(), parts));
    body = body.push(Box::new(
        Text::new(&t("wireless-protocols")).with_role(TextRole::Caption),
    ));
    let close = parts.clone();
    Box::new(
        ModalDialog::new(t("display-sheet-title"), body)
            .with_test_id("display-sheet")
            .with_description(sheet.description())
            .with_max_width(f64::from(ui::tokens::Metrics::BASE.panel_width_sheet))
            .open(true)
            .on_dismiss(move || close.send(&DisplaysEffect::Close)),
    )
}

/// The layouts the displays allow, or why there are none.
fn layouts(sheet: &Sheet, spot: Option<&Spot>, parts: &Parts) -> Element {
    if sheet.layouts.is_empty() {
        return Box::new(
            Column::new()
                .push(Box::new(
                    Text::new(&t("display-sheet-one-display")).with_role(TextRole::Label),
                ))
                .push(Box::new(
                    Text::new(&t("display-sheet-one-display-detail")).with_role(TextRole::Caption),
                )),
        );
    }
    let mut column = Column::new().with_gap(ui::tokens::Spacing::BASE.s1);
    for row in &sheet.layouts {
        let here = Spot::Layout(row.layout);
        let press = parts.clone();
        let pressed = here.clone();
        column = column.push(Box::new(
            ListRow::new(
                format!("display-layout-{}", row.layout.key()),
                row.layout.title(),
            )
            .subtitle(row.layout.detail())
            .icon(row.layout.icon())
            .checked(row.current)
            .selected(spot == Some(&here))
            .with_enabled(row.enabled)
            .on_activate(move || press.act(&pressed)),
        ));
    }
    Box::new(column)
}

/// Keep and Revert while a staged change waits, the choice being saved, or
/// why the last one was refused.
fn band(sheet: &Sheet, spot: Option<&Spot>, parts: &Parts) -> Option<Element> {
    let caption =
        |text: &str| -> Element { Box::new(Text::new(text).with_role(TextRole::Caption)) };
    match &sheet.band {
        Band::Idle => None,
        Band::Saving => Some(caption(&t("display-sheet-saving-layout"))),
        Band::Refused(reason) => Some(caption(&t_args(
            "display-rejected",
            &[("reason", ui::i18n::ArgValue::Text(reason.clone()))],
        ))),
        Band::Confirm { status, answering } => {
            let button = |here: Spot, id: &str, label: &str| {
                let press = parts.clone();
                let pressed = here.clone();
                let mut button = Button::new(id, label)
                    .with_enabled(!answering)
                    .on_click(move || press.act(&pressed));
                button.set_focus(spot == Some(&here));
                button
            };
            let keep = button(Spot::Keep, "display-sheet-keep", &t("display-confirm-keep"))
                .role(ButtonRole::Primary);
            let revert = button(
                Spot::Revert,
                "display-sheet-revert",
                &t("display-confirm-revert"),
            );
            Some(Box::new(
                Column::new()
                    .with_gap(ui::tokens::Spacing::BASE.s2)
                    .push(Box::new(
                        Text::new(&t("display-confirm-title")).with_role(TextRole::Label),
                    ))
                    .push(caption(status))
                    .push(Box::new(
                        Row::new()
                            .with_gap(ui::tokens::Spacing::BASE.s2)
                            .push(Box::new(keep))
                            .push(Box::new(revert)),
                    )),
            ))
        }
    }
}

/// The person's other devices' screens, each with whether it is in the
/// shared layout.
fn screens(sheet: &Sheet, spot: Option<&Spot>, parts: &Parts) -> Element {
    let mut column = Column::new()
        .with_gap(ui::tokens::Spacing::BASE.s1)
        .push(Box::new(
            Text::new(&t("display-remote-heading")).with_role(TextRole::Label),
        ));
    for screen in &sheet.screens {
        let here = Spot::Screen {
            device: screen.device.clone(),
            output: screen.output.clone(),
        };
        let press = parts.clone();
        let pressed = here.clone();
        column = column.push(Box::new(
            ListRow::new(
                format!("display-screen-{}-{}", screen.device, screen.output),
                screen.title(),
            )
            .subtitle(screen.detail())
            .icon(ui::icons::Icon::Monitor)
            .checked(screen.enabled)
            .selected(spot == Some(&here))
            .with_enabled(sheet.screens_editable)
            .tooltip(t("display-remote-row-description"))
            .on_activate(move || press.act(&pressed)),
        ));
    }
    Box::new(column)
}

/// Wireless receivers: the heading with its search, the chosen one's
/// stream options and PIN, and the list or what stands in its place.
fn wireless(sheet: &Sheet, spot: Option<&Spot>, parts: &Parts) -> Element {
    let search = parts.clone();
    let heading = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(Box::new(
            Text::new(&t("wireless-heading")).with_role(TextRole::Label),
        ))
        .push(Box::new(
            Button::icon_only(
                "display-search",
                ui::icons::Icon::Refresh,
                &t("wireless-search-again"),
            )
            .on_click(move || search.send(&DisplaysEffect::Search)),
        ));
    let mut column = Column::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(Box::new(heading));
    if let Some(stream) = &sheet.stream {
        column = column.push(stream_options(stream, parts));
    }
    if sheet.asks_pin {
        column = column.push(pin_field(spot, parts));
    }
    column = column.push(receivers(sheet, spot, parts));
    Box::new(column)
}

fn stream_options(stream: &Stream, parts: &Parts) -> Element {
    let audio = parts.clone();
    let on = stream.audio;
    let share = ListRow::new("display-share-audio", t("wireless-share-audio"))
        .icon(if on {
            ui::icons::Icon::Volume2
        } else {
            ui::icons::Icon::VolumeX
        })
        .checked(on)
        .on_activate(move || audio.send(&DisplaysEffect::ShareAudio { on: !on }));
    let adaptive = parts.clone();
    let adapts = stream.adaptive;
    let resolution = parts.clone();
    let fps = parts.clone();
    let rate = stream.fps;
    let chips = Row::new()
        .with_gap(ui::tokens::Spacing::BASE.s2)
        .push(Box::new(
            Text::new(&t("wireless-stream")).with_role(TextRole::Caption),
        ))
        .push(Box::new(
            Button::new(
                "display-stream-adaptive",
                &t(if adapts {
                    "wireless-stream-auto"
                } else {
                    "wireless-stream-fixed"
                }),
            )
            .role(if adapts {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            })
            .on_click(move || adaptive.send(&DisplaysEffect::AdaptiveQuality { on: !adapts })),
        ))
        .push(Box::new(
            Button::new("display-stream-resolution", &stream.resolution)
                .on_click(move || resolution.send(&DisplaysEffect::NextResolution)),
        ))
        .push(Box::new(
            Button::new(
                "display-stream-fps",
                &t_args(
                    "wireless-stream-fps",
                    &[("fps", ui::i18n::ArgValue::Number(f64::from(rate)))],
                ),
            )
            .on_click(move || {
                fps.send(&DisplaysEffect::FrameRate {
                    fps: if rate == 60 { 30 } else { 60 },
                });
            }),
        ));
    Box::new(
        Column::new()
            .with_gap(ui::tokens::Spacing::BASE.s2)
            .push(Box::new(share))
            .push(Box::new(chips)),
    )
}

const PIN_FIELD: &str = "display-receiver-pin";

/// The chosen receiver's PIN or password: kept in the seat for the shell,
/// never in an action, and Enter connects with it.
fn pin_field(spot: Option<&Spot>, parts: &Parts) -> Element {
    let typed = parts.seat.clone();
    let submitted = parts.clone();
    let mut field = TextInput::from_state(&parts.pin)
        .with_id(PIN_FIELD)
        .with_placeholder(t("wireless-credential-placeholder"))
        .with_accessible_name(t("wireless-credential-label"))
        .with_secret(true)
        .with_enter_submits(true)
        .on_change(move |text| {
            if let Some(seat) = &typed {
                text.clone_into(&mut seat.pin.borrow_mut());
            }
        })
        .on_submit(move |_| submitted.submit_pin());
    if spot == Some(&Spot::Pin) {
        field.set_focus(true);
    }
    Box::new(
        Column::new()
            .with_gap(ui::tokens::Spacing::BASE.s1)
            .push(Box::new(
                Text::new(&t("wireless-credential-label")).with_role(TextRole::Label),
            ))
            .push(Box::new(field))
            .push(Box::new(
                Text::new(&t("wireless-credential-hint")).with_role(TextRole::Caption),
            )),
    )
}

fn receivers(sheet: &Sheet, spot: Option<&Spot>, parts: &Parts) -> Element {
    let mut column = Column::new().with_gap(ui::tokens::Spacing::BASE.s1);
    if sheet.receivers.is_empty() {
        let (message, detail, retry) = match &sheet.discovery {
            Discovery::Searching => (t("wireless-scanning"), t("wireless-scanning-detail"), None),
            Discovery::Failed { title, next_step } => (
                title.clone(),
                next_step.clone(),
                Some(t("wireless-try-again")),
            ),
            Discovery::NoneFound => (
                t("wireless-none"),
                t("wireless-none-detail"),
                Some(t("wireless-search-again")),
            ),
        };
        column = column
            .push(Box::new(Text::new(&message).with_role(TextRole::Label)))
            .push(Box::new(Text::new(&detail).with_role(TextRole::Caption)));
        if let Some(label) = retry {
            let search = parts.clone();
            column = column.push(Box::new(
                Button::new("display-search-again", &label)
                    .on_click(move || search.send(&DisplaysEffect::Search)),
            ));
        }
        return Box::new(column);
    }
    for receiver in &sheet.receivers {
        let here = Spot::Receiver {
            id: receiver.id.clone(),
        };
        let subline = match &receiver.link {
            Link::Connected => t("wireless-connected"),
            Link::Connecting => t("wireless-connecting"),
            Link::Failed(failure) => failure.clone(),
            Link::Idle if receiver.detail.is_empty() => receiver.protocol.clone(),
            Link::Idle => t_args(
                "wireless-device-detail",
                &[
                    (
                        "protocol",
                        ui::i18n::ArgValue::Text(receiver.protocol.clone()),
                    ),
                    ("detail", ui::i18n::ArgValue::Text(receiver.detail.clone())),
                ],
            ),
        };
        let (label, role) = match receiver.link {
            Link::Connected => (t("wireless-stop"), ButtonRole::Destructive),
            Link::Connecting => (t("wireless-wait"), ButtonRole::Secondary),
            _ => (t("wireless-connect"), ButtonRole::Secondary),
        };
        let button_press = parts.clone();
        let row_press = parts.clone();
        let pressed = here.clone();
        let row_pressed = here.clone();
        let action = Button::new(&format!("display-receiver-action-{}", receiver.id), &label)
            .role(role)
            .with_enabled(receiver.link != Link::Connecting)
            .on_click(move || button_press.act(&pressed));
        let mut row = ListRow::new(
            format!("display-receiver-{}", receiver.id),
            receiver.name.clone(),
        )
        .subtitle(subline)
        .icon(ui::icons::Icon::Monitor)
        .selected(spot == Some(&here))
        .trailing(Box::new(action))
        .on_activate(move || row_press.act(&row_pressed));
        row = match receiver.link {
            Link::Connected => row.active(),
            Link::Failed(_) => row.error(),
            _ => row,
        };
        column = column.push(Box::new(row));
    }
    Box::new(column)
}

/// The sheet's keys: Up and Down (and Tab) walk it, Left and Right move
/// between Keep and Revert, Enter applies the highlight, Space turns a
/// screen or the sound, Super+P moves to the next layout, 1 to 4 choose a
/// layout, R searches, A, Q and F set the stream, Escape and Super+Shift+P
/// close. A key the PIN field takes is the field's. The sheet is modal and
/// keeps the keyboard to itself.
struct Keys {
    content: Element,
    dispatch: Dispatcher<DisplaySheet>,
    shown: LocalState<Shown>,
}

impl Keys {
    fn walk(&self, down: bool, by_rows: bool) {
        let shown = self.shown.get();
        let spots = shown.sheet.spots();
        let Some(at) = shown
            .spot()
            .and_then(|spot| spots.iter().position(|s| *s == spot))
        else {
            return;
        };
        let last = spots.len().saturating_sub(1);
        let step = |by: usize| {
            if down {
                at.saturating_add(by).min(last)
            } else {
                at.saturating_sub(by)
            }
        };
        // Keep and Revert are one row to Up and Down.
        let next = match spots.get(at) {
            Some(Spot::Keep) if by_rows && down => {
                if at.saturating_add(2) <= last {
                    at.saturating_add(2)
                } else {
                    at
                }
            }
            Some(Spot::Revert) if by_rows && !down => step(2),
            _ => step(1),
        };
        if let Some(spot) = spots.get(next).cloned() {
            self.shown.update(|shown| shown.spot = Some(spot));
        }
    }

    /// Super+P: the next layout that can be chosen, from the highlighted
    /// one, around the end.
    fn cycle(&self) {
        let shown = self.shown.get();
        let from = match shown.spot() {
            Some(Spot::Layout(layout)) => Some(layout),
            _ => shown.sheet.current(),
        };
        let enabled: Vec<Layout> = shown
            .sheet
            .layouts
            .iter()
            .filter(|row| row.enabled)
            .map(|row| row.layout)
            .collect();
        let next = from
            .and_then(|from| {
                let at = Layout::ALL.iter().position(|layout| *layout == from)?;
                Layout::ALL
                    .iter()
                    .cycle()
                    .skip(at.saturating_add(1))
                    .take(Layout::ALL.len())
                    .find(|layout| enabled.contains(layout))
                    .copied()
            })
            .or_else(|| enabled.first().copied());
        if let Some(next) = next {
            self.shown
                .update(|shown| shown.spot = Some(Spot::Layout(next)));
        }
    }

    /// The highlight is on the PIN: the keyboard is the field's. The sheet
    /// keeps the keyboard to itself, so it hands the field its focus here.
    fn focus_pin(&mut self) {
        fn find(widget: &mut dyn Widget) -> bool {
            if widget.test_id() == Some(PIN_FIELD) {
                widget.set_focus(true);
                return true;
            }
            widget.children_mut().into_iter().any(find)
        }
        find(self.content.as_mut());
    }

    fn act(&self, spot: &Spot) {
        let sheet = self.shown.get().sheet;
        let next = activate(&sheet, spot, &self.dispatch);
        self.shown.update(|shown| shown.spot = next);
    }

    fn stream(&self, key: &str) -> bool {
        let Some(stream) = self.shown.get().sheet.stream else {
            return false;
        };
        let effect = match key {
            "a" => DisplaysEffect::AdaptiveQuality {
                on: !stream.adaptive,
            },
            "q" => DisplaysEffect::NextResolution,
            "f" => DisplaysEffect::FrameRate {
                fps: if stream.fps == 60 { 30 } else { 60 },
            },
            _ => DisplaysEffect::ShareAudio { on: !stream.audio },
        };
        self.dispatch.effect(&effect);
        true
    }
}

impl ui::widget::Decorator for Keys {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use ui::winit::keyboard::{Key, NamedKey};
        let (key, repeat) = match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ui::winit::event::ElementState::Pressed =>
            {
                (key.logical_key.clone(), key.repeat)
            }
            _ => {
                if matches!(event, WindowEvent::Ime(_))
                    && self.shown.get().spot() == Some(Spot::Pin)
                {
                    self.focus_pin();
                }
                self.content.handle_event(event, ctx);
                return;
            }
        };
        let modifiers = ctx.modifiers.state();
        let on_pin = self.shown.get().spot() == Some(Spot::Pin);
        // Typing, Space, Enter and editing in the PIN field are the field's.
        let typing = matches!(
            &key,
            Key::Character(_)
                | Key::Named(
                    NamedKey::Space
                        | NamedKey::Enter
                        | NamedKey::Backspace
                        | NamedKey::Delete
                        | NamedKey::ArrowLeft
                        | NamedKey::ArrowRight
                        | NamedKey::Home
                        | NamedKey::End
                )
        ) && !modifiers.super_key();
        if on_pin && typing {
            self.focus_pin();
            self.content.handle_event(event, ctx);
            *ctx.handled = true;
            return;
        }
        let spot = self.shown.get().spot();
        match &key {
            Key::Named(NamedKey::Escape) => self.dispatch.effect(&DisplaysEffect::Close),
            Key::Character(c) if modifiers.super_key() && c.eq_ignore_ascii_case("p") => {
                if modifiers.shift_key() {
                    // Super+Shift+P belongs to picture in picture.
                    self.dispatch.effect(&DisplaysEffect::Close);
                } else if !repeat {
                    self.cycle();
                }
            }
            Key::Named(NamedKey::ArrowUp) => self.walk(false, true),
            Key::Named(NamedKey::ArrowDown) => self.walk(true, true),
            Key::Named(NamedKey::Tab) => self.walk(!modifiers.shift_key(), false),
            Key::Named(NamedKey::ArrowLeft) if spot == Some(Spot::Revert) => {
                self.shown.update(|shown| shown.spot = Some(Spot::Keep));
            }
            Key::Named(NamedKey::ArrowRight) if spot == Some(Spot::Keep) => {
                self.shown.update(|shown| shown.spot = Some(Spot::Revert));
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(spot) = spot {
                    self.act(&spot);
                }
            }
            Key::Named(NamedKey::Space) => match spot {
                Some(spot @ Spot::Screen { .. }) => self.act(&spot),
                _ => {
                    self.stream(" ");
                }
            },
            Key::Character(c) if c.eq_ignore_ascii_case("r") => {
                self.dispatch.effect(&DisplaysEffect::Search);
            }
            Key::Character(c) if ["a", "q", "f"].contains(&c.to_lowercase().as_str()) => {
                self.stream(&c.to_lowercase());
            }
            Key::Character(c) if matches!(c.as_str(), "1" | "2" | "3" | "4") => {
                let index = c.parse::<usize>().unwrap_or(1).saturating_sub(1);
                if let Some(layout) = Layout::ALL.get(index) {
                    self.act(&Spot::Layout(*layout));
                }
            }
            _ => self.content.handle_event(event, ctx),
        }
        *ctx.handled = true;
    }
}

/// The app's definition, which the shell mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DisplaysApp)
}

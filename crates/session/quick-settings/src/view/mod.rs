//! The panel's views: the controls page and the page each section opens in
//! its place, one accessory's page, the clipboard opened to paste and the
//! arrangement editor. The notifications have no page here: their row opens
//! the shell's centre, the one list they are read in. Built from the toolkit's widgets;
//! every control is bound to one of the panel's actions or effects.

mod edit;
mod now_playing;
mod pages;
mod root;
mod sections;

use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{Effected, ViewCx};
use ui::icons::Icon;
use ui::tokens::Rhythm;
use ui::widget::{
    Button, ButtonRole, Column, Container, Element, FloatingPanel, IconWidget, PageSlide, Row,
    Scrollable, Section, Spacer, Text,
};

use crate::QsSection;
use crate::QuickSettingsState;
use crate::effect::QsEffect;
use crate::faces::{self, Item, RowFace, Sliders, TileFace};
use crate::host::{FixtureHost, PanelHost, Showing};
use crate::live::Live;
use crate::readings::Readings;
use crate::resource::{Page, QuickSettings};

/// What each part of the panel reads, kept current from the readings: one
/// projection each, made once for the panel's life.
#[derive(Clone)]
pub(crate) struct Parts {
    sliders: LocalState<Sliders>,
    tiles: LocalState<Vec<TileFace>>,
    /// The radios' switches their sections' pages carry in their headers.
    switches: LocalState<Vec<TileFace>>,
    rows: LocalState<Vec<RowFace>>,
    sections: Rc<HashMap<QsSection, LocalState<Vec<Item>>>>,
    accessory: LocalState<Accessory>,
    arrangement: LocalState<Arrangement>,
    /// What was typed to search the clipboard's copies.
    query: LocalState<String>,
    /// What plays: the transport's song and playhead, and the choices
    /// around it.
    now_playing: now_playing::Parts,
    /// Why the last press did not reach the device, while it stands.
    refusal: LocalState<Option<String>>,
}

/// The Bluetooth accessories and the person's devices, as the readings
/// publish them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Accessory {
    devices: Vec<connectivity::model::Accessory>,
    targets: Option<Vec<crate::HandoffTarget>>,
}

/// The arrangement the editor shows: the controls on the page, in order,
/// and those this machine could add.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Arrangement {
    active: Vec<settings_store::hardware::QuickSettingsControl>,
    available: Vec<settings_store::hardware::QuickSettingsControl>,
}

impl Arrangement {
    fn of(state: &QuickSettingsState) -> Self {
        use settings_store::hardware::QuickSettingsControl;
        let arranged = &state.arrangement.controls;
        let active = arranged
            .iter()
            .copied()
            .filter(|control| faces::available(*control, state))
            .collect();
        let available = QuickSettingsControl::ALL
            .into_iter()
            .filter(|control| !arranged.contains(control) && faces::available(*control, state))
            .collect();
        Self { active, available }
    }
}

impl Parts {
    fn new(readings: &Readings) -> Self {
        let sections = QsSection::ALL
            .into_iter()
            .map(|section| {
                (
                    section,
                    readings.project(move |state| faces::section(section, state)),
                )
            })
            .collect();
        Self {
            sliders: readings.project(Sliders::of),
            tiles: readings.project(faces::tiles),
            switches: readings.project(faces::section_switches),
            rows: readings.project(faces::rows),
            sections: Rc::new(sections),
            accessory: readings.project(|state| Accessory {
                devices: state.bluetooth.accessories.clone(),
                targets: state.handoff_targets.clone(),
            }),
            arrangement: readings.project(Arrangement::of),
            query: LocalState::new(String::new()),
            now_playing: now_playing::Parts::new(readings),
            refusal: readings.project(|state| state.refusal.clone()),
        }
    }

    fn section(&self, section: QsSection) -> LocalState<Vec<Item>> {
        self.sections
            .get(&section)
            .cloned()
            .unwrap_or_else(|| LocalState::new(Vec::new()))
    }
}

/// What the panel reads and changes: the session's published resources when
/// the session started it ([`Live`]); a screenshot's stand-in; or, for a
/// panel run on its own, one with stand-in readings.
fn host_of(cx: &ViewCx<QuickSettings>) -> Rc<dyn PanelHost> {
    if let Some(crate::host::Lent(host)) = cx.take_lent::<crate::host::Lent>() {
        return host;
    }
    if let Some(fixture) = cx.fixture::<FixtureHost>() {
        return fixture;
    }
    if let Some(host) = Live::of(cx.waker()) {
        return host;
    }
    Rc::new(FixtureHost::new(QuickSettingsState::fixture()))
}

/// The whole panel.
pub(crate) fn panel(cx: &ViewCx<QuickSettings>) -> Element {
    let host = host_of(cx);
    let readings = Readings::new(QuickSettingsState::fixture());
    host.attach(readings.feed());
    answer_effects(cx, &host, &readings);
    let parts = Parts::new(&readings);
    let showing = cx.value(QuickSettings::showing);
    // Opened to paste, the panel is the clipboard and nothing else: no footer to reach past the
    // copies the person is choosing from. The card is built again as that changes, because a
    // footer is part of the card, not of the page it shows.
    let pasting = cx.value(|panel: &QuickSettings| panel.showing().pasting);
    let told = Rc::clone(&host);
    cx.switch(&pasting, move |cx, &pasting| {
        let told = Rc::clone(&told);
        let parts = parts.clone();
        let showing = showing.clone();
        let page = cx.switch(&showing, move |cx, showing| {
            told.showing(showing);
            // The page stands one deep when it is not the controls, and the
            // page it replaces leaves as it arrives: Back runs it the other
            // way.
            Box::new(PageSlide::new(
                usize::from(showing.is_deeper()),
                with_refusal(cx, showing, &parts),
            ))
        });
        // One inset for the whole page, so the tiles, the sliders and the rows
        // share one left and right edge inside the card.
        let body = Container::new()
            .with_no_padding()
            .with_inset(Rhythm::Related)
            .with_child(page);
        // The panel is the floating card every surface of its kind stands on,
        // and the whole of its surface: as tall as the page it shows, growing
        // and shrinking with it as a section opens and closes, up to the room
        // the desk gives a popover — a page with more than that scrolls in the
        // body.
        let card = FloatingPanel::new(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_label(crate::i18n::t("qs-title"))
                .with_child(Box::new(body)),
        ))
        .fill_width()
        .fit_height();
        if pasting {
            Box::new(card) as Element
        } else {
            Box::new(card.footer(root::footer(cx))) as Element
        }
    })
}

/// `page`, told why the last press did not reach the device when it did not:
/// the reason stands over the page it belongs to until the next press or
/// the person puts it away.
fn with_refusal(cx: &ViewCx<QuickSettings>, showing: &Showing, parts: &Parts) -> Element {
    let refusal = parts.refusal.clone();
    let showing = showing.clone();
    let parts = parts.clone();
    cx.switch(&refusal, move |cx, refusal| {
        let page = page(cx, &showing, &parts);
        let Some(reason) = refusal.as_ref() else {
            return page;
        };
        let dismiss = cx.actions().bind_effect_invocation(
            Button::icon_only(
                "qs-refusal-dismiss",
                Icon::X,
                &crate::i18n::t("qs-refused-dismiss"),
            )
            .role(ButtonRole::Plain),
            &QsEffect::DismissRefusal,
        );
        let row = Row::new()
            .no_wrap()
            .with_align_items(ui::taffy::AlignItems::Center)
            .with_rhythm(Rhythm::Related)
            .push(Box::new(IconWidget::new(Icon::TriangleAlert)))
            .push(Box::new(Text::new(reason).with_id("qs-refusal")))
            .push(Box::new(Spacer::new()))
            .push(Box::new(dismiss));
        Column::new()
            .with_intrinsic_height()
            .with_no_padding()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(Section::new(vec![Box::new(row)])))
            .push(page)
            .into_widget()
    })
}

/// The page `showing` names.
fn page(cx: &ViewCx<QuickSettings>, showing: &Showing, parts: &Parts) -> Element {
    if showing.pasting {
        return sections::clipboard(cx, parts, true);
    }
    if showing.editing {
        return edit::page(cx, parts);
    }
    match showing.page {
        // A section opened from the controls replaces them, so a narrow
        // panel shows one page at a time and Back returns.
        Page::Root => match showing.section {
            Some(section) => sections::page(cx, section, parts),
            None => root::page(cx, parts),
        },
        Page::Accessory => pages::accessory(cx, &showing.accessory, parts),
    }
}

/// Every effect goes to the host, which carries it out on the device; the
/// readings say which ones the device can do now and what each is called
/// ([`crate::effect::statuses`]).
fn answer_effects(cx: &ViewCx<QuickSettings>, host: &Rc<dyn PanelHost>, readings: &Readings) {
    let host = Rc::clone(host);
    let readings = readings.clone();
    cx.effects(move |effect: QsEffect, _| {
        host.run(effect)?;
        Ok(Effected::done())
    })
    .status(move || readings.read(crate::effect::statuses));
}

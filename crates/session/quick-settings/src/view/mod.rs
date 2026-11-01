//! The panel's views: the controls page with its sections opening in place,
//! the notifications, one accessory's page, the clipboard opened to paste
//! and the arrangement editor. Built from the toolkit's widgets; every
//! control is bound to one of the panel's actions or effects.

mod edit;
mod now_playing;
mod pages;
mod root;
mod sections;

use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::framework::{Effected, Upkeep, ViewCx};
use ui::tokens::Rhythm;
use ui::widget::{Column, Element, Scrollable};

use crate::QuickSettingsState;
use crate::effect::QsEffect;
use crate::faces::{self, Item, RowFace, Sliders, TileFace};
use crate::host::{FixtureHost, PanelHost, Showing};
use crate::live::Live;
use crate::readings::Readings;
use crate::resource::{Page, QuickSettings};
use crate::{NotificationsView, QsSection};

/// What each part of the panel reads, kept current from the readings: one
/// projection each, made once for the panel's life.
#[derive(Clone)]
pub(crate) struct Parts {
    sliders: LocalState<Sliders>,
    tiles: LocalState<Vec<TileFace>>,
    rows: LocalState<Vec<RowFace>>,
    sections: Rc<HashMap<QsSection, LocalState<Vec<Item>>>>,
    notifications: LocalState<NotificationsView>,
    accessory: LocalState<Accessory>,
    arrangement: LocalState<Arrangement>,
    /// What was typed to search the clipboard's copies.
    query: LocalState<String>,
    /// The app groups folded on the notifications page.
    folded: LocalState<Vec<String>>,
    /// What plays: the transport's song and playhead, and the choices
    /// around it.
    now_playing: now_playing::Parts,
}

/// The accessories and the person's devices one can move to.
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
            rows: readings.project(faces::rows),
            sections: Rc::new(sections),
            notifications: readings.project(|state| state.notifications.clone()),
            accessory: readings.project(|state| Accessory {
                devices: state.bluetooth.accessories.clone(),
                targets: state.handoff_targets.clone(),
            }),
            arrangement: readings.project(Arrangement::of),
            query: LocalState::new(String::new()),
            folded: LocalState::new(Vec::new()),
            now_playing: now_playing::Parts::new(readings),
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
    let synced = parts.now_playing.clone();
    let told = Rc::clone(&host);
    let page = cx.switch(&showing, move |cx, showing| {
        told.showing(showing);
        page(cx, showing, &parts)
    });
    let content = Column::new()
        .with_intrinsic_height()
        .with_padding_rhythm(Some(Rhythm::Section), Some(Rhythm::Section))
        .with_id("quick-settings")
        .push(page);
    let panel = Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_label(crate::i18n::t("qs-title"))
            .with_child(Box::new(content)),
    ) as Element;
    // The readings arrive on the worker; the transport band is this
    // process's own, so it is shown what plays here, before the widgets
    // sync.
    cx.upkeep(
        Upkeep::new().syncing(move || {
            let playing = readings.read(|state| state.now_playing.clone());
            synced.show(playing.as_ref());
        }),
        panel,
    )
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
        Page::Root => root::page(cx, showing.section, parts),
        Page::Notifications => pages::notifications(cx, parts),
        Page::Notification => pages::reader(cx, showing.notification, parts),
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

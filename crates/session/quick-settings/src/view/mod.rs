//! The panel's views: the controls page with its sections opening in place,
//! the notifications, one accessory's page, the clipboard opened to paste
//! and the arrangement editor. Built from the toolkit's widgets; every
//! control is bound to one of the panel's actions or effects.

mod edit;
mod pages;
mod root;
mod sections;

use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::PresenceCtx;
use ui::framework::{Effected, ViewCx};
use ui::taffy::{NodeId, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::Rhythm;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point};
use ui::widget::{Column, Decorator, Disclosing, Element, Scrollable, Widget};

use crate::QuickSettingsState;
use crate::effect::QsEffect;
use crate::faces::{self, Item, RowFace, Sliders, TileFace};
use crate::host::{FixtureHost, Lent, PanelHost, Readings, Showing};
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
}

/// The accessories and the person's devices one can move to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Accessory {
    devices: Vec<crate::BtDevice>,
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
                devices: state.bt_devices.clone(),
                targets: state.handoff_targets.clone(),
            }),
            arrangement: readings.project(Arrangement::of),
            query: LocalState::new(String::new()),
            folded: LocalState::new(Vec::new()),
        }
    }

    fn section(&self, section: QsSection) -> LocalState<Vec<Item>> {
        self.sections
            .get(&section)
            .cloned()
            .unwrap_or_else(|| LocalState::new(Vec::new()))
    }
}

/// The host the desktop lent the panel; a screenshot's stand-in; or, for a
/// panel run on its own, one with stand-in readings.
fn host_of(cx: &ViewCx<QuickSettings>) -> Rc<dyn PanelHost> {
    if let Some(Lent(host)) = cx.take_lent::<Lent>() {
        return host;
    }
    if let Some(fixture) = cx.fixture::<FixtureHost>() {
        return fixture;
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
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_label(crate::i18n::t("qs-title"))
            .with_child(Box::new(Measured {
                content,
                host,
                reported: None,
            })),
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

/// The panel's content, telling the host how tall it lays out: the host
/// draws the popover's surface that tall. While a section opens or closes
/// the content is drawn taller or shorter than it is laid out, and the
/// surface follows it frame by frame.
struct Measured {
    content: Column,
    host: Rc<dyn PanelHost>,
    reported: Option<f64>,
}

impl Measured {
    /// Tell the host the card is `height` tall, when that changed.
    fn report(&mut self, height: f64) {
        if self
            .reported
            .is_none_or(|reported| (reported - height).abs() > 0.5)
        {
            self.reported = Some(height);
            self.host.laid_out(height);
        }
    }
}

impl Decorator for Measured {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.content
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.content
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.content.build_layout(tree, theme, nodes)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.content.apply_layout(tree, node, origin);
        let height = Widget::get_rect(&self.content).height();
        self.report(height);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.content.draw(scene, transform, text, theme, presence);
        let grows = match Widget::disclosing(&self.content) {
            Some(Disclosing::Grows(by)) => by,
            _ => 0.0,
        };
        self.report(Widget::get_rect(&self.content).height() + grows);
    }
}

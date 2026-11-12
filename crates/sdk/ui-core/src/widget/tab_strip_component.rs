//! [`TabStrip`] as a described component: a frame's row of tabs, the shell's
//! own strip ([`super::TabBar`]).
//!
//! A strip's pills are the window bar's pills: each as wide as its title, the
//! chosen one lifted off the strip's ground as a selector, presence on each
//! and a close chip on the one under the pointer or the one in front. The
//! strip squeezes them to fit however narrow its frame is, so a frame with
//! many tabs shows every title it can and paints no pill over another.
//!
//! What the pills report comes back as the shell's own actions: choosing a
//! pill, closing one, dropping one at a new place in the strip, and letting
//! one go outside it — that last with where, so the shell moves the tab to the
//! frame under the pointer or makes one.

use std::collections::HashMap;

use crate::component::{Component, Described, Event, Person, ResourceId, Value};
use crate::icons::Icon;
use crate::presence::user_color;
use crate::widget::{TabBar, TabDescriptor, TabHolder, TabMark};

crate::component::part_slot!(TabItemProps);

/// One tab of a frame: the resource it shows, and what its pill says.
#[derive(Component, Default, Clone)]
#[component(name = "TabItem", role = "Tab", label = "title")]
pub struct TabItemProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// The resource the tab shows: what choosing the pill reports, and which
    /// tab its close chip closes.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What the pill says, from the resource's title.
    pub title: Option<String>,
    /// A glyph before the title, from the resource's type.
    pub icon: Option<Icon>,
    /// The frame is waiting for the person on this tab: a dot at the pill's
    /// trailing edge.
    pub attention: bool,
    /// Its content has unsaved changes: the close chip stands as the mark.
    pub modified: bool,
    /// The content is loading: the pill's leading glyph turns.
    pub busy: bool,
    /// Who else has this resource open now, drawn on the pill: their faces, from the
    /// resource's lenses.
    #[component(slot(default))]
    pub presence: Vec<PresenceFaceProps>,
}

/// One face on a tab: who is at the tab's resource now.
///
/// The strip draws their initials in their presence colour, the same faces a window bar's pills
/// carry, so a person sees at a glance who else is in a frame with them.
#[derive(Component, Default, Clone)]
#[component(name = "TabFace", role = "Image", label = "name")]
pub struct PresenceFaceProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// Who it is: a person or an agent.
    #[component(positional)]
    pub who: Option<Person>,
    /// What the room calls them, when it differs from their name here.
    pub name: Option<String>,
}

crate::component::part_slot!(PresenceFaceProps);

impl TabItemProps {
    /// The pill as the strip takes it: its title, and the glyph its type names.
    fn descriptor(&self) -> TabDescriptor {
        TabDescriptor {
            id: id_of(&self.key),
            title: self.title.clone().unwrap_or_default(),
            app_kind: self
                .icon
                .and_then(|icon| Icon::from_name(icon.name()))
                .map(crate::widget::AppKind::App),
            icon: None,
            attention: self.attention,
            named: false,
            app_name: None,
        }
    }

    /// The same props again: a strip remembers the tabs it was built with, to
    /// tell a change of them from a frame of the window.
    fn again(&self) -> Self {
        Self {
            key: self.key.clone(),
            item: self.item.clone(),
            title: self.title.clone(),
            icon: self.icon,
            attention: self.attention,
            modified: self.modified,
            busy: self.busy,
            presence: self.presence.clone(),
        }
    }
}

/// A frame's tabs, in one row above its content: the shell's tab strip.
///
/// The chosen tab stands lifted off the strip's ground, and every pill says
/// who is at its resource now. Choosing a pill activates that tab; a pill's
/// close chip closes it; dragging one within the strip reorders it, and
/// letting one go outside the strip hands it to the frame under the pointer.
#[derive(Component, Default)]
#[component(name = "TabStrip", role = "TabList")]
pub struct TabStripProps {
    /// Its stable key.
    #[component(key)]
    pub key: String,
    /// The tabs, in order.
    #[component(slot(default))]
    pub tabs: Vec<TabItemProps>,
    /// The resource whose tab shows: the tab in front.
    #[component(positional)]
    pub active: Option<ResourceId>,
    /// Whether this frame has the keyboard: its tab strip says so.
    pub focused: bool,
    /// Told a tab was chosen: carries the resource of the pill chosen.
    #[component(event(value = ResourceId))]
    pub activate: Event,
    /// Told a tab's close chip was pressed: carries that tab's resource.
    #[component(event(value = ResourceId))]
    pub close: Event,
    /// Told a pill was dropped at a new place in the strip: carries the tab
    /// moved and its place among the strip's, from 0.
    #[component(event(value = ResourceId, at = i64))]
    pub reorder: Event,
    /// Told a pill was dragged out of the strip and let go: carries the tab
    /// and where it was let go, as [`pack_point`] packs the window's
    /// coordinates. The shell puts the tab in the frame there, or makes one.
    #[component(event(value = ResourceId, at = i64))]
    pub detach: Event,
}

/// A strip's tabs: [`TabItemProps`] only.

/// A tab's strip id: its key's hash, so the strip knows a tab again after its
/// view runs and the same tab keeps its pill.
fn id_of(key: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The pill showing `active`, the resource in front: the first tab of it, or the first tab
/// while nothing is in front.
fn active_id(order: &[(u64, TabItemProps)], active: Option<&str>) -> u64 {
    let front = active.and_then(|resource| {
        order
            .iter()
            .find(|(_, tab)| tab.item.as_ref().is_some_and(|item| item.0 == resource))
            .map(|(id, _)| *id)
    });
    front
        .or_else(|| order.first().map(|(id, _)| *id))
        .unwrap_or(0)
}

/// A point packed as the one number an event's `at` carries: `y` in whole
/// logical pixels above `x`, each below a million, so a window's coordinates
/// survive the round trip.
#[must_use]
pub fn pack_point(x: f64, y: f64) -> i64 {
    let clamp = |v: f64| i64::from(v.max(0.0).round() as u32).min(999_999);
    clamp(y) * 1_000_000 + clamp(x)
}

/// The point [`pack_point`] packed.
#[must_use]
pub fn unpack_point(packed: i64) -> (f64, f64) {
    (
        packed.rem_euclid(1_000_000) as f64,
        packed.div_euclid(1_000_000) as f64,
    )
}

/// One strip's tabs, as its view gave them: what a pill reports, by its
/// resource.
#[derive(Default)]
struct Tabs {
    /// Each tab's resource, by the strip's id for it.
    resources: HashMap<u64, String>,
    /// The tabs in order.
    order: Vec<(u64, TabItemProps)>,
    /// What the view last said, to tell a change of the tabs from a frame.
    props: Option<TabStripProps>,
    /// The view's actions, as this build of the strip fires them.
    events: Events,
}

/// What a strip's view gave it to run, when a pill is chosen, closed, moved
/// in the strip or let go outside it.
#[derive(Clone, Default)]
struct Events {
    activate: Event,
    close: Event,
    reorder: Event,
    detach: Event,
}

impl Events {
    fn of(props: &TabStripProps) -> Self {
        Self {
            activate: props.activate.clone(),
            close: props.close.clone(),
            reorder: props.reorder.clone(),
            detach: props.detach.clone(),
        }
    }

    /// Tell the view what a pill did: `at` is its new place in the strip, or
    /// where a drop let it go, packed as [`pack_point`] does.
    fn fire(&self, what: &str, resource: &str, at: i64) {
        let value = |at: Option<i64>| {
            let resource = Value::Resource(resource.to_owned());
            match at {
                Some(at) => Value::List(vec![resource, Value::Int(at)]),
                None => resource,
            }
        };
        match what {
            "activate" => self.activate.fire(Some(value(None))),
            "close" => self.close.fire(Some(value(None))),
            "reorder" => self.reorder.fire(Some(value(Some(at)))),
            "detach" => self.detach.fire(Some(value(Some(at)))),
            _ => {}
        }
    }
}

impl Tabs {
    /// Remember `props`: its tabs in order, each one's resource, and what the
    /// view runs when a pill is used.
    fn remember(&mut self, props: &TabStripProps) {
        self.resources.clear();
        self.order.clear();
        self.events = Events::of(props);
        for tab in &props.tabs {
            let id = id_of(&tab.key);
            if let Some(resource) = &tab.item {
                self.resources.insert(id, resource.0.clone());
            }
            self.order.push((id, tab.again()));
        }
        self.props = Some(props.again());
    }

    /// Whether `props` changes something the strip draws or reports.
    fn differs(&self, props: &TabStripProps) -> bool {
        let Some(held) = &self.props else {
            return true;
        };
        held.key != props.key
            || held.focused != props.focused
            || held.active.as_ref().map(|r| &r.0) != props.active.as_ref().map(|r| &r.0)
            || held.tabs.len() != props.tabs.len()
            || held.tabs.iter().zip(&props.tabs).any(|(a, b)| {
                a.key != b.key
                    || a.title != b.title
                    || a.icon != b.icon
                    || a.attention != b.attention
                    || a.modified != b.modified
                    || a.busy != b.busy
                    || a.item.as_ref().map(|r| &r.0) != b.item.as_ref().map(|r| &r.0)
                    || a.presence.len() != b.presence.len()
                    || a.presence
                        .iter()
                        .zip(&b.presence)
                        .any(|(x, y)| x.who != y.who || x.name != y.name)
            })
    }
}

/// A frame's tab strip.
///
/// It is the shell's strip ([`TabBar`]) with its view's reports turned into the view's
/// events: the strip itself draws, lays out, takes input and answers assistive technology
/// exactly as the window bar's does.
pub struct TabStrip {
    /// The shell's strip, drawing the pills.
    bar: TabBar,
    /// Its tabs, as its view gave them.
    tabs: Tabs,
}

impl crate::widget::decorator::concrete::Decorator for TabStrip {
    type Inner = TabBar;

    fn inner(&self) -> &TabBar {
        &self.bar
    }

    fn inner_mut(&mut self) -> &mut TabBar {
        &mut self.bar
    }

    // The bar marks what the person did with its pills as the input arrives, and the view
    // hears it only when the strip reports: every way input reaches the bar reports in the
    // same pass, so a press, a drop or a key is the view's event in that frame.

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        crate::widget::Widget::handle_event(&mut self.bar, event, ctx);
        self.report();
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let handled =
            crate::widget::Widget::perform_action(&mut self.bar, action, key_path, state, path);
        self.report();
        handled
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let handled = crate::widget::Widget::perform_semantic_action(
            &mut self.bar,
            target,
            action,
            state,
            path,
        );
        self.report();
        handled
    }

    /// A host that keeps its tree in step each frame hears what the pills reported since the
    /// last one, as one that only takes events does from [`Self::handle_event`].
    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        crate::widget::Widget::sync(&mut self.bar, state, path);
        self.report();
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let changed = crate::widget::Widget::update(&mut self.bar, state, path);
        self.report();
        changed
    }
}

/// The strip's props again: what its view last said, its events left out.
impl TabStripProps {
    fn again(&self) -> Self {
        Self {
            key: self.key.clone(),
            tabs: self.tabs.iter().map(TabItemProps::again).collect(),
            active: self.active.clone(),
            focused: self.focused,
            activate: Event::None,
            close: Event::None,
            reorder: Event::None,
            detach: Event::None,
        }
    }
}

impl Described for TabStrip {
    type Props = TabStripProps;

    fn from_props(key: &str, props: TabStripProps) -> Self {
        let mut tabs = Tabs::default();
        tabs.remember(&props);
        let front = props.active.as_ref().map(|resource| resource.0.as_str());
        let active = active_id(&tabs.order, front);
        let mut bar = TabBar::new(
            key,
            props.tabs.iter().map(TabItemProps::descriptor).collect(),
            active,
        );
        // A frame's strip holds tabs only: the window's controls, the new-tab
        // control and the search field are the frame's own, beside it.
        bar = bar.without_new_tab();
        bar.set_show_maximize(false);
        let mut strip = Self { bar, tabs };
        strip.draw();
        strip
    }

    fn set_props(&mut self, props: TabStripProps) -> bool {
        if self.tabs.differs(&props) {
            return false;
        }
        self.tabs.remember(&props);
        self.draw();
        true
    }
}

impl TabStrip {
    /// The window bar's strip this one is, as the window bar has it: what the strip draws,
    /// lays out and reports.
    #[must_use]
    pub fn bar(&self) -> &TabBar {
        &self.bar
    }

    /// Bring the strip in step with the tabs it remembers.
    fn draw(&mut self) {
        let Some(props) = self.tabs.props.as_ref().map(TabStripProps::again) else {
            return;
        };
        self.bar.set_closable(props.tabs.len() > 1);
        self.bar.set_group_focused(props.focused);
        self.bar.set_active(active_id(
            &self.tabs.order,
            props.active.as_ref().map(|resource| resource.0.as_str()),
        ));
        for (id, tab) in &self.tabs.order {
            self.bar.set_attention(*id, tab.attention);
            self.bar.set_mark(*id, tab.busy.then_some(TabMark::Busy));
            self.bar.set_modified(*id, tab.modified);
            // The bar shows one face beside the title; a strip with several people at one
            // tab shows the first, as the bar's pill does, and every one of them to assistive
            // technology through the faces the view drew.
            let holder = tab.presence.first().and_then(|face| {
                let who = face.who.as_ref()?;
                let name = face.name.clone().unwrap_or_else(|| who.name.clone());
                Some(TabHolder {
                    name,
                    rgb: user_color(&who.identity),
                })
            });
            self.bar.set_holder(*id, holder);
        }
    }

    /// Tell the view what the pills reported since the last call.
    pub fn report(&mut self) {
        for (what, resource, at) in self.take_reported() {
            self.tabs.events.fire(what, &resource, at);
        }
    }

    /// What the pills reported since the last call: the event's name, the
    /// resource it is about, and where a drop let it go.
    pub fn take_reported(&mut self) -> Vec<(&'static str, String, i64)> {
        let mut out = Vec::new();
        while let Some(id) = self.bar.take_activation() {
            if let Some(resource) = self.tabs.resources.get(&id) {
                out.push(("activate", resource.clone(), 0));
            }
        }
        while let Some(id) = self.bar.take_close() {
            if let Some(resource) = self.tabs.resources.get(&id) {
                out.push(("close", resource.clone(), 0));
            }
        }
        if let Some((from, to)) = self.bar.take_reorder()
            && let Some((id, _)) = self.tabs.order.get(from)
            && let Some(resource) = self.tabs.resources.get(id)
        {
            out.push(("reorder", resource.clone(), i64::try_from(to).unwrap_or(0)));
        }
        if let Some((id, point)) = self.bar.take_detach()
            && let Some(resource) = self.tabs.resources.get(&id)
        {
            out.push(("detach", resource.clone(), pack_point(point.x, point.y)));
        }
        out
    }
}

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

use crate::widget::{TabBar, TabDescriptor, TabFaceSpec, TabMark};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::{
    component::Component, component::Described, component::Event, component::Person,
    component::ResourceId, component::Value,
};

tilo_ui_base::part_type!(TabItemProps);

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
    /// What the tab itself is called, where the pill says its window's name instead: a window
    /// named for its title and app (`Groceries — Notes`) whose tab is the one showing `Groceries`.
    /// A reader hears the pill as its title, and a caller finds this tab here by its own name.
    pub name: Option<String>,
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
    #[component(part(default))]
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
    /// What the workspace calls them, when it differs from their name here.
    pub name: Option<String>,
    /// The colour the face wears, as `#rrggbb`, where the view knows it without a person: who
    /// else is at a window, from presence. Without it, the hue their identity gives them.
    pub colour: Option<String>,
}

tilo_ui_base::part_type!(PresenceFaceProps);

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
            name: self.name.clone(),
        }
    }

    /// The same props again: a strip remembers the tabs it was built with, to
    /// tell a change of them from a frame of the window.
    fn again(&self) -> Self {
        Self {
            key: self.key.clone(),
            item: self.item.clone(),
            title: self.title.clone(),
            name: self.name.clone(),
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
    #[component(part(default))]
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
    /// The strip's own key, as the drag bus names what a tab drag carries.
    source: String,
    activate: Event,
    close: Event,
    reorder: Event,
    detach: Event,
}

impl Events {
    fn of(props: &TabStripProps) -> Self {
        Self {
            source: props.key.clone(),
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
                    || a.name != b.name
                    || a.icon != b.icon
                    || a.attention != b.attention
                    || a.modified != b.modified
                    || a.busy != b.busy
                    || a.item.as_ref().map(|r| &r.0) != b.item.as_ref().map(|r| &r.0)
                    || a.presence.len() != b.presence.len()
                    || a.presence
                        .iter()
                        .zip(&b.presence)
                        .any(|(x, y)| x.who != y.who || x.name != y.name || x.colour != y.colour)
            })
    }
}

/// One face a tab's view gave its pill: who they are, what they are called here and the colour
/// they wear. A face needs a name: its own, or the person's it stands for. A tab given no face
/// falls back to the live presence feed's people.
fn face_spec(face: &PresenceFaceProps) -> Option<TabFaceSpec> {
    let who = face.who.as_ref();
    let name = face
        .name
        .clone()
        .or_else(|| who.map(|who| who.name.clone()))?;
    if name.is_empty() {
        return None;
    }
    Some(TabFaceSpec {
        name,
        hue: who.map_or(tilo_ui_base::tokens::PresenceHue::Violet, |who| who.hue),
        agent: who.is_some_and(|who| {
            !who.identity
                .member_id()
                .is_some_and(tilo_action_policy::MemberId::is_person)
        }),
        rgb: face.colour.as_deref().and_then(hex_rgb),
    })
}

/// The colour `#rrggbb` names, where it names one.
fn hex_rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let digits = hex.strip_prefix('#').unwrap_or(hex);
    if digits.len() != 6 {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
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
    /// The tab this strip carries on the drag bus while the pointer is outside its bar: the
    /// resource it stands for, and whether it was published already.
    carried: Option<String>,
    /// A tab let go outside the bar whose drop no frame took, and the point it was let go at:
    /// the strip waits a moment for a frame under the pointer to take it, then reports the
    /// `detach` its view answers (a window of its own).
    fallback: Option<(String, i64, crate::time::Instant)>,
    /// Where the last detach let its tab go, in the strip's coordinates: the point the drop
    /// lands at.
    detach_point: Option<vello::kurbo::Point>,
}

impl tilo_ui_base::widget::identity::concrete::Decorator for TabStrip {
    type Inner = TabBar;

    fn inner(&self) -> &TabBar {
        &self.bar
    }

    fn inner_mut(&mut self) -> &mut TabBar {
        &mut self.bar
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) {
        tilo_ui_base::widget::Widget::handle_event(&mut self.bar, event, ctx);
        self.report();
    }

    fn perform_action(
        &mut self,
        action: &tilo_ui_base::a11y::Action,
        key_path: &[u64],
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let handled = tilo_ui_base::widget::Widget::perform_action(
            &mut self.bar,
            action,
            key_path,
            state,
            path,
        );
        self.report();
        handled
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        state: &mut tilo_ui_base::widget::Edits<'_>,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let handled = tilo_ui_base::widget::Widget::perform_semantic_action(
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
    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        tilo_ui_base::widget::Widget::sync(&mut self.bar, state, path);
        self.report();
    }

    fn update(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let changed = tilo_ui_base::widget::Widget::update(&mut self.bar, state, path);
        self.report();
        changed
    }

    /// A tab let go over nothing waits a moment to find out that no frame took it: the frames
    /// that tell are the ones this asks for.
    fn needs_animation_frame(&self) -> bool {
        self.fallback.is_some() || tilo_ui_base::widget::Widget::needs_animation_frame(&self.bar)
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
        bar = bar.without_new_tab();
        bar.set_show_maximize(false);
        let mut strip = Self {
            bar,
            tabs,
            carried: None,
            fallback: None,
            detach_point: None,
        };
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

    /// The bar's strip, to answer a press the frame around it takes for the whole window.
    pub fn bar_mut(&mut self) -> &mut TabBar {
        &mut self.bar
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
            self.bar
                .set_faces(*id, tab.presence.iter().filter_map(face_spec).collect());
        }
    }

    /// Tell the view what the pills reported since the last call, and keep the drag bus in step
    /// while a pill is carried: a tab dragged out of its strip is over the frames the pointer
    /// crosses, which say so, and the frame it is let go over takes it.
    pub fn report(&mut self) {
        let reported = self.take_reported();
        let point = self.detach_point.take();
        let mut landed_here = false;
        for (what, resource, at) in reported {
            if what == "detach"
                && let Some(carried) = self.carried.take()
            {
                if let Some(point) = point {
                    self.land(&carried, point);
                }
                // A frame under the pointer may take the drop on this frame or the next; the
                // strip gives it a moment before its own `detach` makes a window of the tab.
                self.fallback = Some((resource, at, crate::time::now()));
                landed_here = true;
                continue;
            }
            self.tabs.events.fire(what, &resource, at);
        }
        self.carry_on_the_bus();
        if self.carried.is_some() && self.bar.carrying().is_none() && !landed_here {
            // The carry was cancelled (Escape, the window losing the pointer): nothing lands.
            self.carried = None;
            crate::widget::drag_bus::end(None);
        }
        let waited = crate::time::Duration::from_millis(50);
        if let Some((resource, at, let_go)) = self.fallback.take() {
            let now = crate::time::now();
            if now.saturating_duration_since(let_go) < waited {
                self.fallback = Some((resource, at, let_go));
            } else if crate::widget::drag_bus::landed().is_some() {
                // Nothing under the pointer took the drop: the view's own `detach` gives the
                // tab a window of its own.
                crate::widget::drag_bus::end(None);
                self.tabs.events.fire("detach", &resource, at);
            }
        }
    }

    /// Publish the pill a person carries on the drag bus once it leaves the strip, so every
    /// frame under the pointer says it takes it, and move it with the pointer.
    fn carry_on_the_bus(&mut self) {
        let Some((id, point)) = self.bar.carrying() else {
            return;
        };
        let Some(resource) = self.tabs.resources.get(&id).cloned() else {
            return;
        };
        let inside = self.bar.bar_rect().contains(point);
        if self.carried.is_none() && inside {
            return;
        }
        let title = self
            .tabs
            .order
            .iter()
            .find(|(held, _)| *held == id)
            .and_then(|(_, tab)| tab.title.clone())
            .unwrap_or_default();
        let rect = self.bar.bar_rect();
        if self.carried.as_deref() == Some(resource.as_str()) {
            crate::widget::drag_bus::moved(point, (self.tabs.events.source.clone(), rect));
            return;
        }
        let payload = tilo_ui_base::dnd::DragPayload::Custom {
            kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
            blob: resource.clone().into_bytes(),
        };
        self.carried = Some(resource);
        crate::widget::drag_bus::carry_payload(
            self.tabs.events.source.clone(),
            payload,
            title,
            point,
            rect,
        );
    }

    /// Let the carried tab go at `point`: the drop the frame there takes.
    fn land(&mut self, resource: &str, point: vello::kurbo::Point) {
        crate::widget::drag_bus::end(Some(crate::widget::drag_bus::Landed {
            source: self.tabs.events.source.clone(),
            payload: tilo_ui_base::dnd::DragPayload::Custom {
                kind: crate::widget::drag_source::RESOURCE_PAYLOAD.to_owned(),
                blob: resource.as_bytes().to_vec(),
            },
            pointer: point,
        }));
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
            self.detach_point = Some(point);
            out.push(("detach", resource.clone(), pack_point(point.x, point.y)));
        }
        out
    }
}

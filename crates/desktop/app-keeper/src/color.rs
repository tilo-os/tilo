//! `wp_color_manager_v1`: an HDR or colour-managed app keeps its colour across a restart.
//!
//! The app is offered colour management once a desktop offered it, at no newer a version, and is
//! told that desktop's render intents, features, primaries and transfer functions (never ICC,
//! which the keeper cannot hold). Its requests are checked by the rules the compositor checks by
//! (`tilo_color_params`), so an app breaks nothing on the desktop that a desktop would accept.
//!
//! Every image description is held as what it is, a [`Desc`]: the parameters an app gave, the
//! scRGB description, or what a desktop said about one it made (its `get_information` answer).
//! Equal descriptions share one entry and one identity, which the app is told and which stays the
//! same across desktops. A description is made on each desktop from what is held; its `ready` or
//! `failed` there is what the app hears for the descriptions it creates, and what one it set on a
//! surface waits for: a surface whose description is not ready on the desktop sends no content,
//! so its first frame after a restart already carries it. One a later desktop cannot serve fails
//! there the way the protocol defines; the surface is shown without it, and the app is pointed at
//! the description the desktop prefers (`preferred_changed`).
//!
//! What the desktop prefers for a surface and what each output displays are fetched from the
//! desktop as information and kept while it is away; the app's `get_preferred` and
//! `get_image_description` are answered from them, and a new desktop that says something else is
//! announced (`preferred_changed`, `image_description_changed`). Whatever the app creates while no
//! desktop is attached waits for the next one.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::{Arc, Mutex};

use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::{Cacheable, with_states};
use tilo_color_params::{Params, Set, Support, feature};
use wayland_client::protocol as c;
use wayland_client::{Connection, Dispatch as ClientDispatch, QueueHandle, WEnum};
use wayland_protocols::wp::color_management::v1::client as up;
use wayland_protocols::wp::color_management::v1::server as down;

use down::wp_color_management_output_v1::WpColorManagementOutputV1 as AppOutput;
use down::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1 as AppFeedback;
use down::wp_image_description_v1::{Cause, WpImageDescriptionV1 as AppDesc};
use up::wp_image_description_v1::WpImageDescriptionV1 as UpDesc;

use crate::state::State;

/// The newest version the keeper speaks.
pub(crate) const VERSION: u32 = 1;

/// One image description, and the identity the app is told for it.
pub(crate) struct Desc {
    identity: u32,
    kind: Kind,
}

#[derive(PartialEq)]
enum Kind {
    Parametric(Params),
    Scrgb,
    Info(Info),
}

/// What a desktop said about a description it made, in the order it said it.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Info(Vec<Fact>);

#[derive(Clone, PartialEq)]
enum Fact {
    Icc(Icc),
    Primaries([i32; 8]),
    PrimariesNamed(u32),
    TfPower(u32),
    TfNamed(u32),
    Luminances(u32, u32, u32),
    TargetPrimaries([i32; 8]),
    TargetLuminance(u32, u32),
    TargetMaxCll(u32),
    TargetMaxFall(u32),
}

/// An ICC profile a desktop sent, passed on as the descriptor it came in.
#[derive(Clone)]
struct Icc(Arc<OwnedFd>, u32);

impl PartialEq for Icc {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) && self.1 == other.1
    }
}

/// What became of a description on the attached desktop.
#[derive(Clone)]
enum UpState {
    Pending,
    Ready,
    Failed(u32, String),
}

/// A held description and its object on the attached desktop.
struct Entry {
    desc: Arc<Desc>,
    up: Option<(Option<UpDesc>, UpState)>,
}

/// An app's description that has not been told `ready` or `failed` yet.
enum Waiting {
    /// One the app created: it waits for the desktop's answer.
    Made(AppDesc),
    /// The description the desktop prefers for a surface.
    Preferred(AppDesc, ObjectId),
    /// What an output displays.
    Output(AppDesc, AppOutput),
}

/// The keeper's colour state, kept across desktops.
#[derive(Default)]
pub(crate) struct Color {
    global: Option<GlobalId>,
    /// What the last desktop that managed colour supports: what an app binding now is told.
    support: Option<Arc<Support>>,
    entries: Vec<Entry>,
    last_identity: u32,
    waiting: Vec<Waiting>,
    /// The app's feedback objects, by the surface they are for.
    feedbacks: HashMap<ObjectId, Vec<AppFeedback>>,
    /// The surfaces that have a colour-management surface object.
    taken: HashSet<ObjectId>,
    /// The identity each surface was last told is preferred.
    told: HashMap<ObjectId, u32>,
    /// What the desktop prefers for each surface it has, and for the ones it has none of yet.
    preferred: HashMap<ObjectId, Arc<Desc>>,
    preferred_any: Option<Arc<Desc>>,
    /// The app's output objects: the output's name and the identity it was last told.
    outputs: Vec<(AppOutput, Option<String>, Option<u32>)>,
    /// What each output displays, by its name.
    displayed: HashMap<String, Arc<Desc>>,
}

/// The desktop's colour manager.
pub(crate) struct UpColor {
    manager: up::wp_color_manager_v1::WpColorManagerV1,
    version: u32,
    building: Support,
    support: Option<Arc<Support>>,
    /// A surface with no role, whose feedback says what the desktop prefers for surfaces it has
    /// none of yet.
    probe: Option<c::wl_surface::WlSurface>,
    outputs: Vec<u32>,
}

impl UpColor {
    /// Bind the desktop's colour manager, once.
    pub(crate) fn bind(
        up: &mut crate::upstream::Upstream,
        registry: &c::wl_registry::WlRegistry,
        name: u32,
        version: u32,
    ) {
        if up.color.is_some() {
            return;
        }
        let version = version.min(VERSION);
        up.color = Some(Self {
            manager: registry.bind(name, version, &up.qh, ()),
            version,
            building: Support::default(),
            support: None,
            probe: None,
            outputs: Vec::new(),
        });
    }
}

/// A surface's colour objects on the desktop.
#[derive(Default)]
pub(crate) struct UpSurfaceColor {
    surface: Option<up::wp_color_management_surface_v1::WpColorManagementSurfaceV1>,
    feedback:
        Option<up::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1>,
}

/// A surface's description and rendering intent: double-buffered, applied on commit.
#[derive(Clone, Default)]
pub(crate) struct Attached(Option<(Arc<Desc>, u32)>);

impl Cacheable for Attached {
    fn commit(&mut self, _: &DisplayHandle) -> Self {
        self.clone()
    }

    fn merge_into(self, into: &mut Self, _: &DisplayHandle) {
        *into = self;
    }
}

/// The description and rendering intent `surface`'s last commit applied.
fn attached(surface: &WlSurface) -> Option<(Arc<Desc>, u32)> {
    with_states(surface, |states| {
        states.cached_state.get::<Attached>().current().0.clone()
    })
}

/// What an app's description object is, once known, and whether it was told `ready`.
#[derive(Default)]
pub(crate) struct DescData(Mutex<(Option<Arc<Desc>>, bool)>);

impl DescData {
    fn of(desc: Arc<Desc>) -> Self {
        Self(Mutex::new((Some(desc), false)))
    }

    fn get(&self) -> (Option<Arc<Desc>>, bool) {
        self.0
            .lock()
            .map(|held| (held.0.clone(), held.1))
            .unwrap_or_default()
    }

    fn ready(&self, desc: Arc<Desc>) {
        if let Ok(mut held) = self.0.lock() {
            *held = (Some(desc), true);
        }
    }
}

/// An app's parametric creator: what it was told, checked against what its manager advertised.
pub(crate) struct ParamsData {
    params: Mutex<Params>,
    support: Arc<Support>,
}

/// An app's surface or feedback object: its surface, and what its manager advertised.
pub(crate) struct SurfaceData {
    surface: WlSurface,
    support: Arc<Support>,
}

/// What the desktop's description object is for.
#[derive(Clone)]
pub(crate) enum Fetch {
    /// A held description, by identity.
    Held(u32),
    /// What the desktop prefers for a surface (none: for surfaces it has none of yet).
    Preferred(Option<ObjectId>),
    /// What the desktop's output, by its global's number, displays.
    Output(u32),
}

/// The desktop's information object and the facts it said so far.
pub(crate) struct Collect(Fetch, Mutex<Info>);

/// The desktop's feedback object of the probe surface.
pub(crate) struct Probe;

fn raw<T: Into<u32>>(value: WEnum<T>) -> u32 {
    match value {
        WEnum::Value(value) => value.into(),
        WEnum::Unknown(value) => value,
    }
}

impl State {
    /// The keeper's own number for `kind`: the one an equal description already has.
    fn intern(&mut self, kind: Kind) -> Arc<Desc> {
        // A description nothing holds any more goes, with its object on the desktop.
        self.color.entries.retain(|entry| {
            let kept = Arc::strong_count(&entry.desc) > 1;
            if !kept && let Some((Some(desc), _)) = &entry.up {
                desc.destroy();
            }
            kept
        });
        if let Some(entry) = self
            .color
            .entries
            .iter()
            .find(|entry| entry.desc.kind == kind)
        {
            return entry.desc.clone();
        }
        self.color.last_identity = self.color.last_identity.wrapping_add(1).max(1);
        let desc = Arc::new(Desc {
            identity: self.color.last_identity,
            kind,
        });
        self.color.entries.push(Entry {
            desc: desc.clone(),
            up: None,
        });
        desc
    }

    /// The attached desktop's colour support, once it said it.
    fn up_support(&self) -> Option<Arc<Support>> {
        self.up.as_ref()?.color.as_ref()?.support.clone()
    }

    /// The attached desktop finished its first state without colour management.
    fn desktop_without_color(&self) -> bool {
        self.up
            .as_ref()
            .is_some_and(|up| up.ready && up.color.is_none())
    }

    /// What became of `desc` on the attached desktop, making it there first; `None` while that
    /// cannot be known yet.
    fn up_state(&mut self, desc: &Arc<Desc>) -> Option<UpState> {
        let support = self.up_support()?;
        let up = self.up.as_ref()?;
        let color = up.color.as_ref()?;
        let entry = self
            .color
            .entries
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.desc, desc))?;
        if let Some((_, state)) = &entry.up {
            return Some(state.clone());
        }
        let made = make_up(&color.manager, &support, &desc.kind, &up.qh, desc.identity);
        let state = match made {
            Some(_) => UpState::Pending,
            None => UpState::Failed(
                Cause::Unsupported.into(),
                "the desktop does not support this image description".to_owned(),
            ),
        };
        entry.up = Some((made, state.clone()));
        Some(state)
    }

    /// Answer every waiting description whose answer is known now.
    pub(crate) fn resolve_color(&mut self) {
        for waiting in std::mem::take(&mut self.color.waiting) {
            if let Some(still) = self.resolve(waiting) {
                self.color.waiting.push(still);
            }
        }
    }

    fn resolve(&mut self, waiting: Waiting) -> Option<Waiting> {
        let without = self.desktop_without_color();
        let away = Cause::OperatingSystem;
        match waiting {
            Waiting::Made(app) => {
                let desc = app.data::<DescData>()?.get().0?;
                if !app.is_alive() {
                    return None;
                }
                if without {
                    app.failed(away, "the desktop does not manage colour".to_owned());
                    return None;
                }
                match self.up_state(&desc) {
                    Some(UpState::Ready) => tell_ready(&app, desc),
                    Some(UpState::Failed(cause, message)) => {
                        app.failed(Cause::try_from(cause).unwrap_or(away), message);
                    }
                    Some(UpState::Pending) | None => return Some(Waiting::Made(app)),
                }
                None
            }
            Waiting::Preferred(app, surface) => {
                if !app.is_alive() {
                    return None;
                }
                if let Some(desc) = self.preferred_for(&surface) {
                    self.color.told.insert(surface, desc.identity);
                    tell_ready(&app, desc);
                } else if without {
                    app.failed(away, "the desktop does not manage colour".to_owned());
                } else {
                    return Some(Waiting::Preferred(app, surface));
                }
                None
            }
            Waiting::Output(app, output) => {
                if !app.is_alive() {
                    return None;
                }
                let known = self
                    .color
                    .outputs
                    .iter_mut()
                    .find(|(known, _, _)| known.id() == output.id());
                let name = known.as_ref().and_then(|(_, name, _)| name.clone());
                let desc = name
                    .as_ref()
                    .and_then(|name| self.color.displayed.get(name).cloned());
                if let Some(desc) = desc {
                    if let Some((_, _, told)) = known {
                        *told = Some(desc.identity);
                    }
                    tell_ready(&app, desc);
                } else if without {
                    app.failed(away, "the desktop does not manage colour".to_owned());
                } else if name.is_none()
                    || self.up.as_ref().is_some_and(|up| {
                        up.ready
                            && !up
                                .outputs
                                .iter()
                                .any(|output| output.name.as_ref() == name.as_ref())
                    })
                {
                    app.failed(Cause::NoOutput, "the output is gone".to_owned());
                } else {
                    return Some(Waiting::Output(app, output));
                }
                None
            }
        }
    }

    /// What the desktop prefers for `surface`: its own answer, or the one for surfaces it has
    /// none of yet.
    fn preferred_for(&self, surface: &ObjectId) -> Option<Arc<Desc>> {
        self.color
            .preferred
            .get(surface)
            .or(self.color.preferred_any.as_ref())
            .cloned()
    }

    /// Tell `surface`'s feedback objects when what is preferred for it is not what they heard.
    fn announce_preferred(&mut self, surface: &ObjectId, always: bool) {
        let Some(desc) = self.preferred_for(surface) else {
            return;
        };
        let Some(told) = self.color.told.get(surface).copied() else {
            return;
        };
        if told == desc.identity && !always {
            return;
        }
        self.color.told.insert(surface.clone(), desc.identity);
        for feedback in self.color.feedbacks.get(surface).into_iter().flatten() {
            if feedback.is_alive() {
                feedback.preferred_changed(desc.identity);
            }
        }
    }

    /// The desktop said everything its colour manager supports: the app is offered the global,
    /// and the desktop is asked what it prefers and displays.
    pub(crate) fn desktop_color_done(&mut self) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        let Some(color) = up.color.as_mut() else {
            return;
        };
        let mut support = std::mem::take(&mut color.building);
        support
            .features
            .retain(|feature| *feature != feature::ICC_V2_V4);
        let support = Arc::new(support);
        color.support = Some(support.clone());
        let version = color.version;
        if color.probe.is_none()
            && let Some(compositor) = &up.compositor
        {
            let probe = compositor.create_surface(&up.qh, Probe);
            let feedback = color
                .manager
                .get_surface_feedback(&probe, &up.qh, None::<ObjectId>);
            ask_preferred(&feedback, &support, &up.qh, None);
            color.probe = Some(probe);
        }
        self.color.support = Some(support);
        if self.color.global.is_none() {
            self.color.global = Some(
                self.dh
                    .create_global::<Self, down::wp_color_manager_v1::WpColorManagerV1, ()>(
                        version,
                        (),
                    ),
            );
        }
        self.ensure_up_color_outputs();
        let surfaces: Vec<ObjectId> = self.color.feedbacks.keys().cloned().collect();
        for surface in &surfaces {
            self.ensure_up_feedback(surface);
        }
        self.resolve_color();
        self.push_unsettled_colors();
    }

    /// The desktop's first state arrived: one without colour management takes the global away
    /// from apps that have not bound it, and answers what waits.
    pub(crate) fn desktop_color_ready(&mut self) {
        if self.desktop_without_color()
            && let Some(global) = self.color.global.take()
        {
            self.dh.remove_global::<Self>(global);
        }
        self.resolve_color();
    }

    /// Ask the desktop what each of its outputs displays, once.
    pub(crate) fn ensure_up_color_outputs(&mut self) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        let Some(color) = up.color.as_mut() else {
            return;
        };
        for output in &up.outputs {
            if color.outputs.contains(&output.global) {
                continue;
            }
            color.outputs.push(output.global);
            let object = color.manager.get_output(&output.wl, &up.qh, output.global);
            object.get_image_description(&up.qh, Fetch::Output(output.global));
        }
    }

    /// Ask the desktop what it prefers for the surface `id`, once the app asked and the
    /// desktop has the surface.
    pub(crate) fn ensure_up_feedback(&mut self, id: &ObjectId) {
        if self.color.feedbacks.get(id).is_none_or(Vec::is_empty) {
            return;
        }
        let Some(support) = self.up_support() else {
            return;
        };
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(color) = up.color.as_ref() else {
            return;
        };
        let Some(us) = self.surfaces.get_mut(id).and_then(|kept| kept.up.as_mut()) else {
            return;
        };
        if us.color.feedback.is_some() {
            return;
        }
        let feedback = color
            .manager
            .get_surface_feedback(&us.surface, &up.qh, Some(id.clone()));
        ask_preferred(&feedback, &support, &up.qh, Some(id.clone()));
        us.color.feedback = Some(feedback);
    }

    /// Set what the surface's last commit attached on the desktop's surface, once the
    /// description is ready there; one the desktop failed is shown without.
    pub(crate) fn push_color(&mut self, id: &ObjectId) {
        let Some(kept) = self.surfaces.get(id) else {
            return;
        };
        let held = attached(&kept.surface);
        let wanted = held.as_ref().map(|(desc, intent)| (desc.identity, *intent));
        if kept
            .up
            .as_ref()
            .is_none_or(|us| us.pushed.color == Some(wanted))
        {
            return;
        }
        let Some(support) = self.up_support() else {
            return;
        };
        let state = match &held {
            Some((desc, _)) => match self.up_state(desc) {
                Some(state) => Some(state),
                None => return,
            },
            None => None,
        };
        let up_desc = held.as_ref().and_then(|(desc, _)| {
            self.color
                .entries
                .iter()
                .find(|entry| Arc::ptr_eq(&entry.desc, desc))
                .and_then(|entry| entry.up.as_ref())
                .and_then(|(made, _)| made.clone())
        });
        let Some(up) = self.up.as_ref() else {
            return;
        };
        let Some(color) = up.color.as_ref() else {
            return;
        };
        let Some(us) = self.surfaces.get_mut(id).and_then(|kept| kept.up.as_mut()) else {
            return;
        };
        let failed = match (state, up_desc, held) {
            (Some(UpState::Pending), _, _) => return,
            (Some(UpState::Ready), Some(made), Some((_, intent))) => {
                let object = us
                    .color
                    .surface
                    .get_or_insert_with(|| color.manager.get_surface(&us.surface, &up.qh, ()));
                // Perceptual is the one intent every desktop supports.
                let intent = if support.intent(intent) { intent } else { 0 };
                if let Ok(intent) = up::wp_color_manager_v1::RenderIntent::try_from(intent) {
                    object.set_image_description(&made, intent);
                }
                false
            }
            (state, _, _) => {
                if let Some(object) = &us.color.surface {
                    object.unset_image_description();
                }
                matches!(state, Some(UpState::Failed(..)))
            }
        };
        us.pushed.color = Some(wanted);
        if failed {
            self.announce_preferred(id, true);
        }
    }

    /// Whether the surface's description is not ready on the desktop yet: its content waits.
    pub(crate) fn color_waits(&self, id: &ObjectId) -> bool {
        let Some(kept) = self.surfaces.get(id) else {
            return false;
        };
        let Some(us) = kept.up.as_ref() else {
            return false;
        };
        let wanted = attached(&kept.surface).map(|(desc, intent)| (desc.identity, intent));
        wanted.is_some()
            && us.pushed.color != Some(wanted)
            && self.up.as_ref().is_some_and(|up| up.color.is_some())
    }

    /// Push every shown surface whose description was waiting.
    fn push_unsettled_colors(&mut self) {
        let waiting: Vec<ObjectId> = self
            .surfaces
            .keys()
            .filter(|id| self.color_waits(id))
            .cloned()
            .collect();
        for id in waiting {
            self.push(&id);
        }
    }

    /// The desktop went away: what was made there is gone with it.
    pub(crate) fn hold_color(&mut self) {
        for entry in &mut self.color.entries {
            entry.up = None;
        }
    }

    /// The app destroyed a surface.
    pub(crate) fn color_surface_gone(&mut self, id: &ObjectId) {
        self.color.feedbacks.remove(id);
        self.color.told.remove(id);
        self.color.preferred.remove(id);
        self.color.taken.remove(id);
    }

    /// The desktop answered for a description of the keeper's.
    fn desktop_desc(&mut self, fetch: &Fetch, desc: &UpDesc, answer: UpState) {
        match fetch {
            Fetch::Held(identity) => {
                if let Some(entry) = self.color.entries.iter_mut().find(|entry| {
                    entry.desc.identity == *identity
                        && entry
                            .up
                            .as_ref()
                            .is_some_and(|(made, _)| made.as_ref() == Some(desc))
                }) && let Some((_, state)) = entry.up.as_mut()
                {
                    *state = answer;
                }
                self.resolve_color();
                self.push_unsettled_colors();
            }
            Fetch::Preferred(_) | Fetch::Output(_) => {
                match answer {
                    UpState::Ready => {
                        if let Some(up) = self.up.as_ref() {
                            desc.get_information(
                                &up.qh,
                                Collect(fetch.clone(), Mutex::new(Info::default())),
                            );
                        }
                    }
                    UpState::Failed(_, message) => {
                        tracing::warn!(%message, "the desktop could not describe its colour");
                    }
                    UpState::Pending => return,
                }
                desc.destroy();
            }
        }
    }

    /// The desktop said all about one of its descriptions.
    fn desktop_info(&mut self, fetch: &Fetch, info: Info) {
        let desc = self.intern(Kind::Info(info));
        match fetch {
            Fetch::Held(_) => {}
            Fetch::Preferred(Some(surface)) => {
                if self.surfaces.contains_key(surface) {
                    self.color.preferred.insert(surface.clone(), desc);
                    self.announce_preferred(surface, false);
                }
            }
            Fetch::Preferred(None) => {
                self.color.preferred_any = Some(desc);
                let surfaces: Vec<ObjectId> = self
                    .color
                    .feedbacks
                    .keys()
                    .filter(|surface| !self.color.preferred.contains_key(*surface))
                    .cloned()
                    .collect();
                for surface in surfaces {
                    self.announce_preferred(&surface, false);
                }
            }
            Fetch::Output(global) => {
                let name = self.up.as_ref().and_then(|up| {
                    up.outputs
                        .iter()
                        .find(|output| output.global == *global)
                        .and_then(|output| output.name.clone())
                });
                if let Some(name) = name {
                    for (object, known, told) in &self.color.outputs {
                        if known.as_ref() == Some(&name)
                            && told.is_some_and(|told| told != desc.identity)
                            && object.is_alive()
                        {
                            object.image_description_changed();
                        }
                    }
                    self.color.displayed.insert(name, desc);
                }
            }
        }
        self.resolve_color();
    }
}

/// Tell the app its description is ready.
fn tell_ready(app: &AppDesc, desc: Arc<Desc>) {
    let identity = desc.identity;
    if let Some(data) = app.data::<DescData>() {
        data.ready(desc);
    }
    app.ready(identity);
}

/// Ask the desktop's feedback object for what it prefers now, parametric where it can be.
fn ask_preferred(
    feedback: &up::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
    support: &Support,
    qh: &QueueHandle<State>,
    surface: Option<ObjectId>,
) {
    if support.has(feature::PARAMETRIC) {
        feedback.get_preferred_parametric(qh, Fetch::Preferred(surface));
    } else {
        feedback.get_preferred(qh, Fetch::Preferred(surface));
    }
}

/// Make `kind` on the desktop, unless it cannot take it.
fn make_up(
    manager: &up::wp_color_manager_v1::WpColorManagerV1,
    support: &Support,
    kind: &Kind,
    qh: &QueueHandle<State>,
    identity: u32,
) -> Option<UpDesc> {
    let params = match kind {
        Kind::Scrgb => {
            return support
                .has(feature::WINDOWS_SCRGB)
                .then(|| manager.create_windows_scrgb(qh, Fetch::Held(identity)));
        }
        Kind::Parametric(params) => params.clone(),
        Kind::Info(info) => info.params(support)?,
    };
    let requests = params.requests();
    if !params.served_by(support) || !requests.iter().all(|set| sendable(*set)) {
        return None;
    }
    let creator = manager.create_parametric_creator(qh, ());
    for set in requests {
        send(&creator, set);
    }
    Some(creator.create(qh, Fetch::Held(identity)))
}

fn sendable(set: Set) -> bool {
    match set {
        Set::TfNamed(tf) => up::wp_color_manager_v1::TransferFunction::try_from(tf).is_ok(),
        Set::PrimariesNamed(named) => up::wp_color_manager_v1::Primaries::try_from(named).is_ok(),
        _ => true,
    }
}

fn send(
    creator: &up::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    set: Set,
) {
    match set {
        Set::TfNamed(tf) => {
            if let Ok(tf) = up::wp_color_manager_v1::TransferFunction::try_from(tf) {
                creator.set_tf_named(tf);
            }
        }
        Set::TfPower(eexp) => creator.set_tf_power(eexp),
        Set::PrimariesNamed(named) => {
            if let Ok(named) = up::wp_color_manager_v1::Primaries::try_from(named) {
                creator.set_primaries_named(named);
            }
        }
        Set::Primaries([rx, ry, gx, gy, bx, by, wx, wy]) => {
            creator.set_primaries(rx, ry, gx, gy, bx, by, wx, wy);
        }
        Set::Luminances(min, max, reference) => creator.set_luminances(min, max, reference),
        Set::MasteringPrimaries([rx, ry, gx, gy, bx, by, wx, wy]) => {
            creator.set_mastering_display_primaries(rx, ry, gx, gy, bx, by, wx, wy);
        }
        Set::MasteringLuminance(min, max) => creator.set_mastering_luminance(min, max),
        Set::MaxCll(cll) => creator.set_max_cll(cll),
        Set::MaxFall(fall) => creator.set_max_fall(fall),
    }
}

impl Info {
    /// The parameters that describe the same as these facts, as far as `support` lets them be
    /// said; `None` for an ICC profile or what cannot be said at all.
    fn params(&self, support: &Support) -> Option<Params> {
        let mut named_primaries = None;
        let mut chromaticities = None;
        let mut named_tf = None;
        let mut power = None;
        let mut rest = Vec::new();
        for fact in &self.0 {
            match fact {
                Fact::Icc(_) => return None,
                Fact::Primaries(xy) => chromaticities = Some(*xy),
                Fact::PrimariesNamed(named) => named_primaries = Some(*named),
                Fact::TfPower(eexp) => power = Some(*eexp),
                Fact::TfNamed(tf) => named_tf = Some(*tf),
                Fact::Luminances(min, max, reference) if support.has(feature::SET_LUMINANCES) => {
                    rest.push(Set::Luminances(*min, *max, *reference));
                }
                Fact::TargetPrimaries(xy)
                    if support.has(feature::SET_MASTERING_DISPLAY_PRIMARIES) =>
                {
                    rest.push(Set::MasteringPrimaries(*xy));
                }
                Fact::TargetLuminance(min, max)
                    if support.has(feature::SET_MASTERING_DISPLAY_PRIMARIES) =>
                {
                    rest.push(Set::MasteringLuminance(*min, *max));
                }
                Fact::TargetMaxCll(cll) => rest.push(Set::MaxCll(*cll)),
                Fact::TargetMaxFall(fall) => rest.push(Set::MaxFall(*fall)),
                _ => {}
            }
        }
        let primaries = match (named_primaries, chromaticities) {
            (Some(named), _) if support.primaries.contains(&named) => Set::PrimariesNamed(named),
            (_, Some(xy)) => Set::Primaries(xy),
            _ => return None,
        };
        let transfer = match (named_tf, power) {
            (Some(tf), _) if support.transfers.contains(&tf) => Set::TfNamed(tf),
            (_, Some(eexp)) => Set::TfPower(eexp),
            _ => return None,
        };
        let mut params = Params::default();
        for set in [primaries, transfer].into_iter().chain(rest) {
            params.set(set, support).ok()?;
        }
        params.check_complete().ok()?;
        Some(params)
    }

    /// Say these facts on the app's information object, then `done`.
    fn tell(&self, info: &down::wp_image_description_info_v1::WpImageDescriptionInfoV1) {
        use down::wp_color_manager_v1::{Primaries, TransferFunction};
        for fact in &self.0 {
            match fact {
                Fact::Icc(Icc(fd, size)) => info.icc_file(fd.as_fd(), *size),
                Fact::Primaries([rx, ry, gx, gy, bx, by, wx, wy]) => {
                    info.primaries(*rx, *ry, *gx, *gy, *bx, *by, *wx, *wy);
                }
                Fact::PrimariesNamed(named) => {
                    if let Ok(named) = Primaries::try_from(*named) {
                        info.primaries_named(named);
                    }
                }
                Fact::TfPower(eexp) => info.tf_power(*eexp),
                Fact::TfNamed(tf) => {
                    if let Ok(tf) = TransferFunction::try_from(*tf) {
                        info.tf_named(tf);
                    }
                }
                Fact::Luminances(min, max, reference) => info.luminances(*min, *max, *reference),
                Fact::TargetPrimaries([rx, ry, gx, gy, bx, by, wx, wy]) => {
                    info.target_primaries(*rx, *ry, *gx, *gy, *bx, *by, *wx, *wy);
                }
                Fact::TargetLuminance(min, max) => info.target_luminance(*min, *max),
                Fact::TargetMaxCll(cll) => info.target_max_cll(*cll),
                Fact::TargetMaxFall(fall) => info.target_max_fall(*fall),
            }
        }
        info.done();
    }
}

// The app's side.

impl GlobalDispatch<down::wp_color_manager_v1::WpColorManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<down::wp_color_manager_v1::WpColorManagerV1>,
        (): &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let support = state.color.support.clone().unwrap_or_default();
        let manager = data_init.init(resource, support.clone());
        tilo_color_params::server::advertise(&manager, &support);
    }
}

impl Dispatch<down::wp_color_manager_v1::WpColorManagerV1, Arc<Support>> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        manager: &down::wp_color_manager_v1::WpColorManagerV1,
        request: down::wp_color_manager_v1::Request,
        support: &Arc<Support>,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use down::wp_color_manager_v1::{Error, Request};
        let unsupported = |what: &str| {
            manager.post_error(
                Error::UnsupportedFeature,
                format!("{what} is not supported"),
            );
        };
        match request {
            Request::GetOutput { id, output } => {
                let name =
                    smithay::output::Output::from_resource(&output).map(|output| output.name());
                let object = data_init.init(id, ());
                state.color.outputs.push((object, name, None));
            }
            Request::GetSurface { id, surface } => {
                let support = support.clone();
                data_init.init(
                    id,
                    SurfaceData {
                        surface: surface.clone(),
                        support,
                    },
                );
                if !state.color.taken.insert(surface.id()) {
                    manager.post_error(
                        Error::SurfaceExists,
                        "the surface already has a colour-management object",
                    );
                }
            }
            Request::GetSurfaceFeedback { id, surface } => {
                let feedback = data_init.init(
                    id,
                    SurfaceData {
                        surface: surface.clone(),
                        support: support.clone(),
                    },
                );
                state
                    .color
                    .feedbacks
                    .entry(surface.id())
                    .or_default()
                    .push(feedback);
                state.ensure_up_feedback(&surface.id());
            }
            Request::CreateIccCreator { obj } => {
                // The new object exists before the error ends the app's connection.
                data_init.init(obj, ());
                unsupported("an ICC profile");
            }
            Request::CreateParametricCreator { obj } => {
                data_init.init(
                    obj,
                    ParamsData {
                        params: Mutex::new(Params::default()),
                        support: support.clone(),
                    },
                );
                if !support.has(feature::PARAMETRIC) {
                    unsupported("a parametric image description");
                }
            }
            Request::CreateWindowsScrgb { image_description } => {
                if !support.has(feature::WINDOWS_SCRGB) {
                    data_init.init(image_description, DescData::default());
                    unsupported("the Windows scRGB description");
                    return;
                }
                let desc = state.intern(Kind::Scrgb);
                let app = data_init.init(image_description, DescData::of(desc));
                state.color.waiting.push(Waiting::Made(app));
                state.resolve_color();
            }
            _ => {}
        }
    }
}

impl Dispatch<down::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1, ()>
    for State
{
    fn request(
        _: &mut Self,
        _: &Client,
        _: &down::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
        _: down::wp_image_description_creator_icc_v1::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        // Never reached: making one is a protocol error that ends the app's connection.
    }
}

impl
    Dispatch<
        down::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        ParamsData,
    > for State
{
    fn request(
        state: &mut Self,
        _: &Client,
        creator: &down::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        request: down::wp_image_description_creator_params_v1::Request,
        data: &ParamsData,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let set = match tilo_color_params::server::set_of(request) {
            Ok(set) => set,
            Err(down::wp_image_description_creator_params_v1::Request::Create {
                image_description,
            }) => {
                let params = data
                    .params
                    .lock()
                    .map(|params| params.clone())
                    .unwrap_or_default();
                if let Err(error) = params.check_complete() {
                    data_init.init(image_description, DescData::default());
                    creator.post_error(error.code(), error.message());
                    return;
                }
                let desc = state.intern(Kind::Parametric(params));
                let app = data_init.init(image_description, DescData::of(desc));
                state.color.waiting.push(Waiting::Made(app));
                state.resolve_color();
                return;
            }
            Err(_) => return,
        };
        let taken = data
            .params
            .lock()
            .map_or(Ok(()), |mut params| params.set(set, &data.support));
        if let Err(error) = taken {
            creator.post_error(error.code(), error.message());
        }
    }
}

impl Dispatch<AppDesc, DescData> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        app: &AppDesc,
        request: down::wp_image_description_v1::Request,
        data: &DescData,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use down::wp_image_description_v1::{Error, Request};
        if let Request::GetInformation { information } = request {
            let info = data_init.init(information, ());
            match data.get() {
                (Some(desc), true) => match &desc.kind {
                    Kind::Info(facts) => facts.tell(&info),
                    Kind::Parametric(_) | Kind::Scrgb => app.post_error(
                        Error::NoInformation,
                        "a description the app made gives no information",
                    ),
                },
                _ => app.post_error(Error::NotReady, "the image description is not ready"),
            }
        }
    }
}

impl Dispatch<down::wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> for State {
    fn request(
        _: &mut Self,
        _: &Client,
        _: &down::wp_image_description_info_v1::WpImageDescriptionInfoV1,
        _: down::wp_image_description_info_v1::Request,
        (): &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<down::wp_color_management_surface_v1::WpColorManagementSurfaceV1, SurfaceData>
    for State
{
    fn request(
        _: &mut Self,
        _: &Client,
        object: &down::wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        request: down::wp_color_management_surface_v1::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        use down::wp_color_management_surface_v1::{Error, Request};
        let next = match request {
            Request::Destroy => None,
            _ if !data.surface.is_alive() => {
                object.post_error(Error::Inert, "the surface is gone");
                return;
            }
            Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let intent = raw(render_intent);
                if !data.support.intent(intent) {
                    object.post_error(
                        Error::RenderIntent,
                        "the rendering intent was not advertised",
                    );
                    return;
                }
                let held = image_description.data::<DescData>().map(DescData::get);
                let Some((Some(desc), true)) = held else {
                    object.post_error(
                        Error::ImageDescription,
                        "the image description is not ready",
                    );
                    return;
                };
                Some((desc, intent))
            }
            Request::UnsetImageDescription => None,
            _ => return,
        };
        if data.surface.is_alive() {
            with_states(&data.surface, |states| {
                states.cached_state.get::<Attached>().pending().0 = next;
            });
        }
    }

    fn destroyed(
        state: &mut Self,
        _: ClientId,
        _: &down::wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        data: &SurfaceData,
    ) {
        state.color.taken.remove(&data.surface.id());
    }
}

impl Dispatch<AppFeedback, SurfaceData> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        feedback: &AppFeedback,
        request: down::wp_color_management_surface_feedback_v1::Request,
        data: &SurfaceData,
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use down::wp_color_management_surface_feedback_v1::{Error, Request};
        let (image_description, parametric) = match request {
            Request::GetPreferred { image_description } => (image_description, false),
            Request::GetPreferredParametric { image_description } => (image_description, true),
            _ => return,
        };
        let app = data_init.init(image_description, DescData::default());
        if !data.surface.is_alive() {
            feedback.post_error(Error::Inert, "the surface is gone");
        } else if parametric && !data.support.has(feature::PARAMETRIC) {
            feedback.post_error(
                Error::UnsupportedFeature,
                "parametric image descriptions are not supported",
            );
        } else {
            state
                .color
                .waiting
                .push(Waiting::Preferred(app, data.surface.id()));
            state.resolve_color();
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, feedback: &AppFeedback, data: &SurfaceData) {
        if let Some(feedbacks) = state.color.feedbacks.get_mut(&data.surface.id()) {
            feedbacks.retain(|known| known.id() != feedback.id());
        }
    }
}

impl Dispatch<AppOutput, ()> for State {
    fn request(
        state: &mut Self,
        _: &Client,
        output: &AppOutput,
        request: down::wp_color_management_output_v1::Request,
        (): &(),
        _: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let down::wp_color_management_output_v1::Request::GetImageDescription {
            image_description,
        } = request
        {
            let app = data_init.init(image_description, DescData::default());
            state
                .color
                .waiting
                .push(Waiting::Output(app, output.clone()));
            state.resolve_color();
        }
    }

    fn destroyed(state: &mut Self, _: ClientId, output: &AppOutput, (): &()) {
        state
            .color
            .outputs
            .retain(|(known, _, _)| known.id() != output.id());
    }
}

// The desktop's side.

impl ClientDispatch<up::wp_color_manager_v1::WpColorManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &up::wp_color_manager_v1::WpColorManagerV1,
        event: up::wp_color_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::wp_color_manager_v1::Event;
        let Some(color) = state.up.as_mut().and_then(|up| up.color.as_mut()) else {
            return;
        };
        let support = &mut color.building;
        match event {
            Event::SupportedIntent { render_intent } => support.intents.push(raw(render_intent)),
            Event::SupportedFeature { feature } => support.features.push(raw(feature)),
            Event::SupportedTfNamed { tf } => support.transfers.push(raw(tf)),
            Event::SupportedPrimariesNamed { primaries } => {
                support.primaries.push(raw(primaries));
            }
            Event::Done => state.desktop_color_done(),
            _ => {}
        }
    }
}

impl ClientDispatch<UpDesc, Fetch> for State {
    fn event(
        state: &mut Self,
        desc: &UpDesc,
        event: up::wp_image_description_v1::Event,
        fetch: &Fetch,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let answer = match event {
            up::wp_image_description_v1::Event::Ready { .. } => UpState::Ready,
            up::wp_image_description_v1::Event::Failed { cause, msg } => {
                UpState::Failed(raw(cause), msg)
            }
            _ => return,
        };
        state.desktop_desc(fetch, desc, answer);
    }
}

impl ClientDispatch<up::wp_image_description_info_v1::WpImageDescriptionInfoV1, Collect> for State {
    fn event(
        state: &mut Self,
        _: &up::wp_image_description_info_v1::WpImageDescriptionInfoV1,
        event: up::wp_image_description_info_v1::Event,
        Collect(fetch, info): &Collect,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use up::wp_image_description_info_v1::Event;
        let fact = match event {
            Event::Done => {
                let Ok(mut held) = info.lock() else {
                    return;
                };
                let facts = std::mem::take(&mut *held);
                drop(held);
                state.desktop_info(fetch, facts);
                return;
            }
            Event::IccFile { icc, icc_size } => Fact::Icc(Icc(Arc::new(icc), icc_size)),
            Event::Primaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => Fact::Primaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
            Event::PrimariesNamed { primaries } => Fact::PrimariesNamed(raw(primaries)),
            Event::TfPower { eexp } => Fact::TfPower(eexp),
            Event::TfNamed { tf } => Fact::TfNamed(raw(tf)),
            Event::Luminances {
                min_lum,
                max_lum,
                reference_lum,
            } => Fact::Luminances(min_lum, max_lum, reference_lum),
            Event::TargetPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => Fact::TargetPrimaries([r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y]),
            Event::TargetLuminance { min_lum, max_lum } => Fact::TargetLuminance(min_lum, max_lum),
            Event::TargetMaxCll { max_cll } => Fact::TargetMaxCll(max_cll),
            Event::TargetMaxFall { max_fall } => Fact::TargetMaxFall(max_fall),
            _ => return,
        };
        if let Ok(mut info) = info.lock() {
            info.0.push(fact);
        }
    }
}

impl
    ClientDispatch<
        up::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        Option<ObjectId>,
    > for State
{
    fn event(
        state: &mut Self,
        feedback: &up::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        event: up::wp_color_management_surface_feedback_v1::Event,
        surface: &Option<ObjectId>,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let up::wp_color_management_surface_feedback_v1::Event::PreferredChanged { .. } = event
            && let Some(support) = state.up_support()
        {
            ask_preferred(feedback, &support, qh, surface.clone());
        }
    }
}

impl ClientDispatch<up::wp_color_management_output_v1::WpColorManagementOutputV1, u32> for State {
    fn event(
        _: &mut Self,
        output: &up::wp_color_management_output_v1::WpColorManagementOutputV1,
        event: up::wp_color_management_output_v1::Event,
        global: &u32,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            up::wp_color_management_output_v1::Event::ImageDescriptionChanged
        ) {
            output.get_image_description(qh, Fetch::Output(*global));
        }
    }
}

impl ClientDispatch<c::wl_surface::WlSurface, Probe> for State {
    fn event(
        _: &mut Self,
        _: &c::wl_surface::WlSurface,
        _: c::wl_surface::Event,
        _: &Probe,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

wayland_client::delegate_noop!(State: ignore up::wp_color_management_surface_v1::WpColorManagementSurfaceV1);
wayland_client::delegate_noop!(State: ignore up::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1);

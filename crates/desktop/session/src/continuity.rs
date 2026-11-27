//! What another of the person's devices has in front, offered here to continue: the bar's chip,
//! what pressing it opens on this device, and what putting it away says in the record of the
//! person's devices.
//!
//! The same record carries what each of their devices has in front ([`ContinuityOffer`]): a device
//! writes its own as the window in front moves, and each device reads the newest offer of another
//! of theirs that it has neither taken nor put away. The reading here is that one offer, drawn as
//! `session:continuity`; taking it is one of the resource's own verbs, which answers the record and
//! asks the loop to open the app at the resource it showed, as any other open here is, and putting
//! it away answers the record alone.
//!
//! An offer made to this device alone is a window the person showed or moved here from another of
//! their devices: it draws no chip, and is opened on the pass that reads it, in the workspace and
//! on the screen it names, and answered with whether it opened and why not.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, Actor, ApiError, MemberId, ResourceSpec};
use tilo_service::{Service, Started, decode, offer, reading};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::{ContinuityOffer, Devices};

use crate::followed_resource::FollowedResource;
use crate::platform::fabric::Continuity;

pub use tilo_device_surfaces::continuity::{ContinuityAction, ContinuityView};

/// The resource another device's offer is published as.
pub const RESOURCE: &str = "session:continuity";

/// How long an offer stands from the front window it names, ms: each change of what is in front
/// renews it, and one a person keeps in front for longer is renewed again before it runs out.
const HOLD_MS: i64 = 60 * 60 * 1_000;

/// How long after it was written an unchanged front window's offer is written again, ms: the
/// device that keeps the app in front goes on offering it, without the record moving as it types.
const RENEW_MS: i64 = 30 * 60 * 1_000;

/// The offer resource's own row, as the session declares it.
#[derive(tilo_resource::Row)]
#[row(kind = "session.continuity", label = "Continuity")]
pub(crate) struct Offer;

/// What another device's offer is published as: its reading and the two verbs that answer it.
#[must_use]
pub fn resource() -> ResourceSpec {
    offer::<ContinuityAction>(
        tilo_resource::LiveRow::typed::<Offer>(RESOURCE, crate::i18n::t("session-continuity"))
            .into_resource(),
    )
}

/// The app this device has in front, as it is offered and as another device takes it: the app, the
/// resource it shows and the view it shows it through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Front {
    pub(crate) app_id: String,
    /// The app's name, as the person here reads it.
    pub(crate) app_name: String,
    /// The resource it shows; empty for an app opened on nothing of its own.
    pub(crate) resource: String,
    /// The view it shows it through; empty for its primary one.
    pub(crate) view: String,
}

/// The record of the person's devices, and this device in it, while the person lets apps continue.
#[derive(Clone)]
struct Attached {
    devices: FollowedResource<Devices>,
    /// The person's root key.
    person: [u8; 32],
    /// This device's own key.
    local: [u8; 32],
    continuity: Continuity,
}

/// This device's side of the offers their devices make each other.
#[derive(Default)]
pub(crate) struct Across {
    record: Option<Attached>,
    /// What this device offered last: the front, and when it was written.
    offered: Option<(Front, i64)>,
    /// The front window this device last looked at: its tab, and the resource it showed.
    looked: Option<(u64, String)>,
    /// The apps the person took here since the loop last opened them.
    asked: Vec<Front>,
    /// The windows other devices showed here that this device opened, by the device and the
    /// moment: each is opened once, before the record carries its answer back.
    opened: BTreeSet<(String, i64)>,
}

impl Across {
    /// Keep what this device offers in the record of the person's devices followed as `devices`,
    /// as the device `cert` names, while the person's `continuity` lets apps continue.
    pub(crate) fn attach(
        &mut self,
        devices: FollowedResource<Devices>,
        cert: &DeviceCert,
        continuity: Continuity,
    ) {
        self.record = Some(Attached {
            devices,
            person: cert.user_public_key,
            local: cert.device_public_key,
            continuity,
        });
    }

    /// The record and this device in it, while the person lets apps continue.
    fn attached(&self) -> Option<&Attached> {
        self.record
            .as_ref()
            .filter(|attached| attached.continuity.apps_permitted())
    }

    /// The apps the person took here since this was last taken, for the loop to open.
    pub(crate) fn take_asked(&mut self) -> Vec<Front> {
        std::mem::take(&mut self.asked)
    }

    /// Offer `front` to the person's other devices. `None` keeps what this device offered before,
    /// written again once half its hour is gone, so an app the person keeps in front goes on being
    /// offered; a front window that did not move is no write at all.
    pub(crate) fn offer(&mut self, front: Option<Front>, now_ms: i64) {
        let Some(attached) = self.attached().cloned() else {
            return;
        };
        let front = if let Some(front) = front {
            front
        } else {
            // Nothing in front moved: what this device offered stands, written again before its
            // hour is half gone.
            let Some((offered, at)) = self.offered.clone() else {
                return;
            };
            if now_ms.saturating_sub(at) < RENEW_MS {
                return;
            }
            offered
        };
        if let Some((offered, at)) = &self.offered
            && *offered == front
            && now_ms.saturating_sub(*at) < RENEW_MS
        {
            return;
        }
        self.offered = Some((front.clone(), now_ms));
        let offer = ContinuityOffer {
            from: hex::encode(attached.local),
            app_id: front.app_id,
            app_name: front.app_name,
            resource: front.resource,
            view: front.view,
            shown_ms: now_ms,
            until_ms: now_ms.saturating_add(HOLD_MS),
            ..ContinuityOffer::default()
        };
        attached
            .devices
            .invoke_unwaited(&Devices::offering_continuity(&offer), |made| {
                if let Err(error) = made {
                    tracing::warn!(%error, "the app this device has in front was not offered");
                }
            });
    }

    /// Whether the front window `tab` showing `resource` is worth looking at: the app it shows and
    /// its view are read from the layout only when it moved.
    pub(crate) fn looks_at(&mut self, tab: Option<u64>, resource: &str) -> bool {
        let Some(tab) = tab else {
            return false;
        };
        let looked = (tab, resource.to_owned());
        if self.looked.as_ref() == Some(&looked) {
            return false;
        }
        self.looked = Some(looked);
        true
    }

    /// The newest offer another of the person's devices makes that this device has neither taken
    /// nor put away, and when it stops standing: what the bar's chip draws, and `None` while there
    /// is nothing to continue.
    fn showing(&self, now_ms: i64) -> (ContinuityView, Option<i64>) {
        let Some(attached) = self.attached() else {
            return (ContinuityView::default(), None);
        };
        let local = hex::encode(attached.local);
        let record = attached.devices.get();
        let labels: BTreeMap<String, String> = record
            .listed(&attached.person)
            .into_iter()
            .map(|listed| (listed.key, listed.device.called(&listed.cert)))
            .collect();
        let mut best: Option<(&str, &ContinuityOffer)> = None;
        for (id, kept) in record.continuity.iter() {
            let from = id.as_str();
            // An offer made to one device is a window shown there, never a chip.
            if from == local
                || !kept.to.is_empty()
                || !labels.contains_key(from)
                || !kept.shown_at(now_ms)
            {
                continue;
            }
            if kept.answer_of(&local).is_some() {
                continue;
            }
            if best.is_none_or(|(_, offer)| kept.shown_ms > offer.shown_ms) {
                best = Some((from, kept));
            }
        }
        let Some((from, kept)) = best else {
            return (ContinuityView::default(), None);
        };
        (
            ContinuityView {
                offer: offer_id(from, kept.shown_ms),
                app: kept.app_name.clone(),
                app_id: kept.app_id.clone(),
                device: labels.get(from).cloned().unwrap_or_default(),
                resource: kept.resource.clone(),
                view: kept.view.clone(),
            },
            Some(kept.until_ms),
        )
    }

    /// The windows another device of the person's showed on this one that it has not opened yet,
    /// each once. Shown whether or not the person lets apps continue: they asked for each.
    fn shown_here(&mut self, now_ms: i64) -> Vec<ContinuityOffer> {
        let Some(attached) = self.record.as_ref() else {
            return Vec::new();
        };
        let local = hex::encode(attached.local);
        // The record keeps only offers its own devices made.
        attached
            .devices
            .get()
            .continuity
            .iter()
            .filter(|(_, kept)| {
                kept.to == local && kept.shown_at(now_ms) && kept.answer_of(&local).is_none()
            })
            .map(|(_, kept)| kept.clone())
            .filter(|kept| self.opened.insert((kept.from.clone(), kept.shown_ms)))
            .collect()
    }

    /// Say in the record that this device took the offer `from` made at `at_ms`, or did not, and
    /// why not where it could not open it.
    fn answer(&self, from: &str, at_ms: i64, taken: bool, reason: Option<&str>) {
        let Some(attached) = self.record.as_ref() else {
            return;
        };
        let answering = Devices::answering_continuity(from, at_ms, &attached.local, taken, reason);
        attached.devices.invoke_unwaited(&answering, |made| {
            if let Err(error) = made {
                tracing::warn!(%error, "an answer to an offer was not kept in the record");
            }
        });
    }

    /// The offer `id` names, as the record says it: the device that made it and the offer itself,
    /// while it is one this device may take or put away.
    fn offered_as(&self, id: &str, now_ms: i64) -> Option<(String, ContinuityOffer)> {
        let attached = self.attached()?;
        let local = hex::encode(attached.local);
        let (from, at_ms) = split_id(id)?;
        let record = attached.devices.get();
        let known = record.listed(&attached.person);
        let kept = record
            .continuity
            .get(&tilo_resource::Id::from_raw(from.clone()))?;
        if !known.iter().any(|listed| listed.key == from)
            || kept.shown_ms != at_ms
            || !kept.shown_at(now_ms)
            || kept.answer_of(&local).is_some()
        {
            return None;
        }
        Some((from, kept.clone()))
    }
}

/// Now, in ms since the epoch, as the record of the person's devices keeps times.
fn now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

/// The id an offer is named by: the device that made it, and the moment its front changed.
fn offer_id(from: &str, at_ms: i64) -> String {
    format!("{from}:{at_ms}")
}

/// The device and moment an offer's id names.
fn split_id(id: &str) -> Option<(String, i64)> {
    let (from, at) = id.rsplit_once(':')?;
    Some((from.to_owned(), at.parse().ok()?))
}

/// The session reaching the record of the person's devices: what this device has in front, offered
/// there, and what their other devices offer here.
pub(crate) type Shared = Rc<RefCell<Across>>;

/// The offer as the session publishes it: read from the record each time it moves, and answered
/// through the resource's own verbs.
pub(crate) struct Served {
    across: Shared,
    /// The reading as last published.
    published: ContinuityView,
    /// The record's revision the reading was read at.
    read: Option<u64>,
    /// When the published offer stops standing, ms; `None` while it shows nothing.
    until_ms: Option<i64>,
    /// The offer the person answered here since it was published: the chip goes at once, before
    /// the record carries the answer back.
    answered: Option<String>,
}

impl Served {
    pub(crate) fn new(across: Shared) -> Self {
        Self {
            across,
            published: ContinuityView::default(),
            read: None,
            until_ms: None,
            answered: None,
        }
    }

    /// The offer the person takes or puts away, as the reading names it.
    fn answering(&self, id: &str) -> Result<(String, ContinuityOffer), ApiError> {
        let across = self.across.borrow();
        across
            .offered_as(id, now_ms())
            .ok_or_else(|| ApiError::Unavailable {
                code: "no-offer".to_owned(),
                reason: "that device offers nothing to continue now".to_owned(),
            })
    }

    /// Say in the record that the person took the offer `from` made here, or put it away here.
    fn answer(&self, from: &str, at_ms: i64, taken: bool) -> Result<(), ApiError> {
        let across = self.across.borrow();
        if across.attached().is_none() {
            return Err(ApiError::Unavailable {
                code: "no-record".to_owned(),
                reason: "this device reaches none of your other devices".to_owned(),
            });
        }
        across.answer(from, at_ms, taken, None);
        Ok(())
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        let address = ActionAddress::new(resource, action);
        if resource != RESOURCE {
            return Err(ApiError::Unknown(address));
        }
        match decode::<ContinuityAction>(&self.resources(), resource, action, arguments)? {
            ContinuityAction::Take { offer } => {
                let (from, kept) = self.answering(&offer)?;
                self.answer(&from, kept.shown_ms, true)?;
                self.across.borrow_mut().asked.push(Front {
                    app_id: kept.app_id,
                    app_name: kept.app_name,
                    resource: kept.resource,
                    view: kept.view,
                });
                self.answered = Some(offer);
                self.read = None;
            }
            ContinuityAction::Dismiss { offer } => {
                let (from, kept) = self.answering(&offer)?;
                self.answer(&from, kept.shown_ms, false)?;
                self.answered = Some(offer);
                self.read = None;
            }
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        let now = now_ms();
        let expired = self.until_ms.is_some_and(|until| now >= until);
        let across = self.across.borrow();
        let revision = across
            .attached()
            .map(|attached| attached.devices.revision());
        if self.read == revision && !expired {
            return false;
        }
        self.read = revision;
        let (mut showing, until) = across.showing(now);
        // What the person answered here goes at once: the record carries the answer a pass or two
        // later, and the chip is never put to them twice.
        if !showing.offer.is_empty() && Some(showing.offer.as_str()) == self.answered.as_deref() {
            showing = ContinuityView::default();
        } else {
            self.answered = None;
        }
        self.until_ms = until;
        if self.published == showing {
            return false;
        }
        self.published = showing;
        true
    }
}

impl crate::Shared {
    /// Offer what this device has in front to the person's other devices, through the record of
    /// them, when the front window or the resource it shows moved: the app, the resource and the
    /// view it shows it through.
    pub(crate) fn offer_front(&self) {
        // The window in front is the one the keyboard is in, when a window holds it: a shell that
        // draws its windows itself (the tilolang shell) holds the keyboard, and the window in
        // front there is the one the layout's own record says the person brought forward
        // ([`tilo_session_core::layout::Layout::tab_in_front`]).
        let tab = self
            .desktop
            .with_policy(|policy| policy.focused_tab())
            .or_else(|| self.layout.borrow().tab_in_front());
        let resources = self.layout.borrow().window_resources();
        let resource = tab
            .and_then(|tab| resources.get(&tab).cloned())
            .unwrap_or_default();
        let mut across = self.continuity.borrow_mut();
        let moved = across.looks_at(tab, &resource);
        let front = moved
            .then(|| {
                let key = tab.map(|tab| tab.to_string());
                let reading = self.layout.borrow().reading();
                let place = reading
                    .places
                    .into_iter()
                    .find(|place| Some(&place.key) == key.as_ref())?;
                Some(Front {
                    app_name: crate::app_names::app_name(&place.shows),
                    app_id: place.shows,
                    resource: place.resource,
                    view: place.shown_as,
                })
            })
            .flatten()
            .filter(|front| !front.app_id.is_empty());
        across.offer(front, now_ms());
    }

    /// Open here what the person took from another of their devices: the app at the resource it
    /// showed there, through the view it showed it through, or the app alone when it showed none.
    /// A window another device showed here opens without asking, and the record hears whether it
    /// opened, and why not.
    pub(crate) fn carry_out_continuity(&self) {
        let takes = self.continuity.borrow_mut().take_asked();
        let shown = self.continuity.borrow_mut().shown_here(now_ms());
        let shown = shown.into_iter().map(|offer| {
            let take = Front {
                app_id: offer.app_id.clone(),
                app_name: offer.app_name.clone(),
                resource: offer.resource.clone(),
                view: offer.view.clone(),
            };
            (take, Some(offer))
        });
        for (take, shown) in takes.into_iter().map(|take| (take, None)).chain(shown) {
            if let Some(offer) = &shown {
                self.go_to(offer);
            }
            let (address, arguments) = if take.resource.is_empty() {
                let mut arguments = Map::new();
                arguments.insert("app".into(), take.app_id.clone().into());
                (
                    ActionAddress::new(
                        tilo_session_core::desktop::verbs::CHROME_RESOURCE,
                        "open-app",
                    ),
                    arguments,
                )
            } else {
                let mut arguments = Map::new();
                arguments.insert("resource".into(), take.resource.into());
                if !take.view.is_empty() {
                    arguments.insert("view".into(), take.view.into());
                }
                (
                    ActionAddress::new(tilo_session_core::layout::RESOURCE, "open"),
                    arguments,
                )
            };
            let actor = Actor::from(MemberId::person(self.person.clone()));
            let opened = self.published.invoke_as(
                &actor,
                &address,
                &arguments,
                &mut tilo_session_core::surfaces::Look::none(),
            );
            if let Err(error) = &opened {
                tracing::warn!(%error, %take.app_id, "taking an offer opened nothing");
            }
            if let Some(offer) = shown {
                let why = opened.is_err().then(|| self.not_opened(&offer.app_id));
                self.continuity.borrow().answer(
                    &offer.from,
                    offer.shown_ms,
                    why.is_none(),
                    why.as_deref(),
                );
            }
        }
    }

    /// Bring forward where a window shown here from another device opens: the workspace it was in
    /// when this device holds it, and the screen of this device it names.
    fn go_to(&self, offer: &ContinuityOffer) {
        if let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&offer.workspace)
            && self
                .handle
                .read(|tree| tree.workspaces.iter().any(|held| held.id() == workspace))
        {
            let _ = self
                .handle
                .dispatch(tilo_session_core::desktop::navigation::Verb::Chrome(
                    tilo_session_core::desktop::navigation::ChromeEffect::SwitchWorkspace {
                        workspace: Some(workspace.to_string()),
                        delta: None,
                    },
                ));
        }
        let Some(devices) = self.owner_devices.as_ref() else {
            return;
        };
        let record = devices.devices();
        let screen = record
            .devices
            .get(&tilo_resource::Id::from_raw(devices.who().1))
            .and_then(|device| device.facts.get(crate::shared_displays::DISPLAYS))
            .and_then(|value| crate::shared_displays::decoded(value))
            .and_then(|shown| shown.into_iter().find(|screen| screen.id == offer.output));
        if let Some(display) = screen.and_then(|screen| {
            self.handle
                .display_at_origin(f64::from(screen.x), f64::from(screen.y))
        }) {
            self.handle.focus_display(display);
        }
    }

    /// Why the app `app` shown here from another device did not open: it is not installed here,
    /// or it did not start.
    fn not_opened(&self, app: &str) -> String {
        if app_catalogue::apps::read_one_from_env(app)
            .ok()
            .flatten()
            .is_some()
        {
            return crate::i18n::t("window-move-reason-no-start");
        }
        let device = self
            .owner_devices
            .as_ref()
            .map(|devices| devices.cert().device_label.clone())
            .unwrap_or_default();
        crate::i18n::t_args(
            "window-move-reason-not-installed",
            &[
                ("app", crate::app_names::app_name(app).into()),
                ("device", device.into()),
            ],
        )
    }
}

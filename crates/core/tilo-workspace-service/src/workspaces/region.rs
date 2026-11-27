//! The person's region and formats and their time zone: a resource of the person's own
//! workspace ([`REGION`]), so every device of theirs reads the same.
//!
//! A view a device of theirs renders for the person, or for their agent, shows dates and
//! clocks the way their own windows do ([`Workspaces::region`]). Their page keeps it current
//! through a device of theirs ([`formats::PROVIDER`]): the formats they choose in Region &
//! Formats, and the zone their browser is in, which a zone they chose themselves overrides.
//! Each is a typed action on the resource ([`Region`]), so a change has an author and a history
//! and reaches their other devices as any workspace resource does. Only the person on a device
//! of theirs reads or changes it; an agent's request is refused, unknown and forbidden being
//! the same answer, and a device that is not theirs keeps none of it.
//!
//! The device that keeps their workspace answers them itself ([`register`]), so a page's own
//! call needs no host: the one region every view this device renders for them reads.

use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_action_services::formats::{self, BrowserZone, ChooseZone, Formats, SetFormats};
use tilo_actions::remote::ActionCallError::{self, UnknownAction};
use tilo_actions::remote::{
    ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt, InvocationState,
    InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId};
use tilo_resource::{Invoke, Model, Refusal, resource};
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::DeviceCert;

use super::WeakWorkspaces;
use super::Workspaces;
use super::person_workspace;
use super::providers::Reach;
use super::requests::{answer, argument};

/// The resource of the person's own workspace their region is in.
pub const REGION: (&str, &str) = ("so.tilo.region", "region");

/// Why a device that is not the person's neither reads nor changes their region.
const NOT_THEIRS: &str = "your region is on your own devices, and this is not one of them";
/// Why a device of the person cannot change their region before it has received it.
const NOT_YET: &str = "this device has not received your region yet; try again in a moment";

/// The `locale` settings section's keys a [`Region`] keeps, in its field order.
const SECTION_KEYS: [&str; 4] = [
    "date_format",
    "time_format",
    "number_style",
    "first_day_of_week",
];

/// The person's region and formats and their zones.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// Whether a device of theirs sent their formats yet.
    pub formats_sent: bool,
    /// The `locale` section's `date_format`.
    pub date_format: String,
    /// Its `time_format`.
    pub time_format: String,
    /// Its `number_style`; empty while the language's convention applies.
    pub number_style: String,
    /// Its `first_day_of_week`.
    pub first_day_of_week: String,
    /// The zone they chose themselves; empty while they follow their browser's.
    pub chosen_zone: String,
    /// The zone their browser said it is in last; empty until it said.
    pub browser_zone: String,
}

impl Region {
    /// The formats as the `locale` settings section keeps them; none until a device of theirs
    /// sent any.
    #[must_use]
    pub fn section(&self) -> Option<Map<String, Value>> {
        self.formats_sent.then(|| {
            SECTION_KEYS
                .into_iter()
                .zip([
                    &self.date_format,
                    &self.time_format,
                    &self.number_style,
                    &self.first_day_of_week,
                ])
                .map(|(key, value)| {
                    let value = if value.is_empty() {
                        Value::Null
                    } else {
                        Value::String(value.clone())
                    };
                    (key.to_owned(), value)
                })
                .collect()
        })
    }

    /// What the resource holds, as [`formats::READ`] answers it.
    #[must_use]
    pub fn formats(&self) -> Formats {
        let named = |zone: &str| (!zone.is_empty()).then(|| zone.to_owned());
        Formats {
            formats: self.section(),
            chosen_zone: named(&self.chosen_zone),
            browser_zone: named(&self.browser_zone),
        }
    }
}

#[resource(kind = "tilo.region", read_content = tilo_resource::read::fields)]
impl Region {
    /// Keep the person's formats, as the `locale` settings section names them; a value that
    /// is not one of its format's is not kept, and its format's default is.
    #[action(external)]
    pub fn set_formats(
        &mut self,
        date_format: String,
        time_format: String,
        number_style: String,
        first_day_of_week: String,
    ) -> Result<(), Refusal> {
        let section: Map<String, Value> = SECTION_KEYS
            .into_iter()
            .zip([date_format, time_format, number_style, first_day_of_week])
            .map(|(key, value)| (key.to_owned(), Value::String(value)))
            .collect();
        let kept = tilo_formats::RegionFormats::from_section(section).to_section();
        let value = |key: &str| {
            kept.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        self.date_format = value("date_format");
        self.time_format = value("time_format");
        self.number_style = value("number_style");
        self.first_day_of_week = value("first_day_of_week");
        self.formats_sent = true;
        Ok(())
    }

    /// Keep the zone the person chose, or empty to follow their browser's again.
    #[action(external)]
    pub fn choose_zone(&mut self, zone: String) -> Result<(), Refusal> {
        if !zone.is_empty() {
            zone_named(&zone)?;
        }
        self.chosen_zone = zone;
        Ok(())
    }

    /// Keep the zone the person's browser says it is in.
    #[action(external)]
    pub fn browser_zone(&mut self, zone: String) -> Result<(), Refusal> {
        zone_named(&zone)?;
        self.browser_zone = zone;
        Ok(())
    }
}

/// Refused unless `zone` names a zone as the time-zone database does.
fn zone_named(zone: &str) -> Result<(), Refusal> {
    if tilo_local_time::is_zone_name(zone) {
        Ok(())
    } else {
        Err(Refusal::new(
            "a zone is named as the time-zone database names it, such as Europe/Vienna",
        ))
    }
}

impl Workspaces {
    /// The workspace `person`'s region is kept in, when this device is one of theirs: a
    /// device of anybody else's keeps none of it.
    fn region_workspace(&self, person: &[u8; 32]) -> Option<String> {
        (&self.device_cert().user_public_key == person).then(|| person_workspace_id(person))
    }

    /// What `person` has of their region, as this device reads it; none where this device
    /// is not theirs or does not hold their resource yet.
    async fn read_region(&self, person: &[u8; 32]) -> Option<Region> {
        let workspace = self.region_workspace(person)?;
        if !person_workspace::holds_person_workspace(self).await {
            return None;
        }
        match person_workspace::read_on::<Region>(self, &workspace, REGION).await {
            Ok(region) => Some(region),
            Err(error) => {
                tracing::warn!(%error, "the person's region is not read");
                None
            }
        }
    }

    /// The formats and the zone a view rendered here for `person`, or for their agent, shows
    /// dates in: the formats they keep (the defaults where they keep none), and the zone they
    /// chose, else their browser's. None where they keep nothing this device reads.
    pub async fn region(
        &self,
        person: &[u8; 32],
    ) -> Option<(tilo_formats::RegionFormats, Option<String>)> {
        let kept = self.read_region(person).await?.formats();
        if kept == Formats::default() {
            return None;
        }
        let zone = kept.zone().map(str::to_owned);
        let formats = kept
            .formats
            .map(tilo_formats::RegionFormats::from_section)
            .unwrap_or_default();
        Some((formats, zone))
    }

    /// The zone `person` lives in, as the time-zone database names it: the one they keep in
    /// their region, else, on a device of theirs, the zone that device is in. None on a device
    /// that is not theirs and reads no region of theirs: its own zone is never theirs.
    pub async fn zone_of(&self, person: &[u8; 32]) -> Option<String> {
        if let Some(zone) = self.region(person).await.and_then(|(_, zone)| zone) {
            return Some(zone);
        }
        self.region_workspace(person)
            .and_then(|_| tilo_local_time::local_zone_name())
    }

    /// Apply `action` to `person`'s region, on a device of theirs that has received
    /// it.
    pub(crate) async fn change_region(
        &self,
        person: &[u8; 32],
        action: Invoke<Region>,
    ) -> Result<(), String> {
        let workspace = self.region_workspace(person).ok_or(NOT_THEIRS)?;
        if !person_workspace::holds_person_workspace(self).await {
            return Err(NOT_YET.to_owned());
        }
        person_workspace::apply_on(self, &workspace, REGION, action).await
    }

    /// Answer a [`formats::PROVIDER`] request: the person's own region, read or changed by
    /// them on a device of theirs and by nobody else.
    pub(super) async fn formats_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let person = super::requests::asking_person(caller, certificate, workspace_id)?;
        self.answer_formats(&person, request).await
    }

    /// Answer `request`, one of [`formats`]'s actions, for `person`: the one answer a page on a
    /// device of theirs, another device of theirs and this device's own dispatch all get.
    async fn answer_formats(
        &self,
        person: &[u8; 32],
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.address.resource != formats::RESOURCE {
            return Err(UnknownAction);
        }
        let action = match request.target.address.action.as_str() {
            formats::READ => None,
            formats::SET => {
                let SetFormats { formats } = argument(request, "the formats to keep")?;
                let value = |key: &str| {
                    formats
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                Some(Region::SET_FORMATS.with((
                    value("date_format"),
                    value("time_format"),
                    value("number_style"),
                    value("first_day_of_week"),
                )))
            }
            formats::CHOOSE_ZONE => {
                let ChooseZone { zone } = argument(request, "the zone chosen, or none")?;
                Some(Region::CHOOSE_ZONE.with(zone.unwrap_or_default()))
            }
            formats::BROWSER_ZONE => {
                let BrowserZone { zone } = argument(request, "the zone the browser is in")?;
                Some(Region::BROWSER_ZONE.with(zone))
            }
            _ => return Err(UnknownAction),
        };
        let changed = match action {
            Some(action) => self.change_region(person, action).await,
            None if self.region_workspace(person).is_none() => Err(NOT_THEIRS.to_owned()),
            None => Ok(()),
        };
        let kept = match changed {
            Ok(()) => Ok(self
                .read_region(person)
                .await
                .map(|region| region.formats())
                .unwrap_or_default()),
            Err(reason) => Err(reason),
        };
        answer(&kept)
    }
}

/// Answer the person's formats on this device from now on, for the workspace it keeps for them:
/// a page tells a device of theirs the clock and zone they chose, and every view this device
/// renders for them reads it, with no host to ask.
///
/// # Panics
/// Called outside a reactor: a call reaches the region on the reactor it was registered on.
pub fn register(workspaces: &Workspaces) {
    let runtime = tokio::runtime::Handle::current();
    workspaces.providers().provide(
        formats::PROVIDER,
        formats::PROVIDER,
        Reach::ThisDevice,
        Arc::new(RegionActions {
            workspaces: workspaces.downgrade(),
            runtime,
        }),
    );
}

/// The person's region, as this device answers it for them on it.
struct RegionActions {
    workspaces: WeakWorkspaces,
    /// Where a call reaches the region: reading and changing it reads the workspace, which is
    /// asynchronous.
    runtime: tokio::runtime::Handle,
}

impl ActionProvider for RegionActions {
    fn serves(&self, provider: &str) -> bool {
        provider == formats::PROVIDER
    }

    /// The person asks for their formats by address, as their page does: nothing is listed.
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::from_rows(
            "0".to_owned(),
            Vec::new(),
            &query,
        )))
    }

    /// Answered once the action ran: its receipt says how it ended, and nothing is left to
    /// follow.
    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(workspaces) = self.workspaces.upgrade() else {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "this device is shutting down".into(),
            }));
        };
        // The formats and zone are the person's alone: no agent and no app asks them through a
        // device's dispatch, and only a device that is theirs answers them.
        let person = workspaces.device_cert().user_public_key;
        let theirs = caller.acting().is_person()
            && caller.acting().app().is_none()
            && caller.acting().person_id() == Some(hex::encode(person).as_str());
        if !theirs {
            return Pending::ready(Err(UnknownAction));
        }
        let (done, answer) = tilo_actions::remote::pending();
        let invocation = request.invocation;
        let runtime = self.runtime.clone();
        runtime.spawn(async move {
            let status = match Box::pin(workspaces.answer_formats(&person, &request)).await {
                Ok(value) => ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
                Err(error) => ReceiptStatus::Rejected { error },
            };
            done.complete(Ok(InvocationReceipt { invocation, status }));
        });
        answer
    }

    /// Answered once it ran: there is nothing to follow.
    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

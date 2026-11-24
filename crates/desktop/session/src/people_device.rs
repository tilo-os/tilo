//! Who is here, as the shell reads it: every member of the workspace in front who is here now, the
//! person at this device, whom they follow and whom the bar has to tell them about.
//!
//! The roster is the session's reading of presence and the workspace's member rows, published as
//! `session:people` for a shell in another language to draw. Nothing is kept beyond what a reading
//! cannot derive: the card the person opened (shared with the cursor the shell's own press leaves),
//! and the last wave or arrival the bar still shows.
//!
//! What each person's row says: their name, the colour the workspace gives them, where they are
//! (the window they are in, or the app when this shell shows no window of it), what they say they
//! are doing, how available they are, the call they are in, whether this person follows them and
//! whether they follow this person. Software is a member too: an agent's row says the window it
//! works in. What the row's words are is the shell's, so each language says them its own way; the
//! reading carries the facts.

use std::cell::RefCell;
use std::rc::Rc;

use collab_types::Member;
use serde_json::{Map, Value};
use tilo_actions::ApiError;
use tilo_desktop_model::Layout;
use tilo_device_surfaces::people::{People, PeopleEffect, Person};
use tilo_service::{Service, Started, decode, offer, reading};

/// The resource this session publishes the roster as.
pub(crate) const RESOURCE: &str = "session:people";

/// The card the roster shows for the person at this device, instead of anyone else's.
pub(crate) const SELF: &str = "self";

/// How long the bar says who waved at the person: the waver's face bounces meanwhile.
pub(crate) const WAVE_SHOWN: ui::time::Duration = ui::time::Duration::from_secs(6);
/// How long the bar says who arrived where the person is.
pub(crate) const ARRIVAL_SHOWN: ui::time::Duration = ui::time::Duration::from_secs(4);

/// The people resource, as the session's catalogue holds it.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "People", effects = PeopleEffect)]
pub struct PeopleRoot;

/// Who is here, and the verbs that change what the bar says of them.
pub(crate) struct PeopleDevice {
    handle: tilo_session_core::desktop::state::DesktopHandle,
    /// Who is here, placed into a copy of the layout as the members of the workspace in front: the
    /// tree the loop holds lists nobody.
    place_here: Rc<dyn Fn(&mut Layout)>,
    /// The live session: who is here, whom this person follows, and what the bar must tell them.
    sync: Option<Rc<crate::sync::Client>>,
    /// What a person's card asked of the session, carried out on the next pass: the queue the
    /// roster and the bar share.
    asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
    /// The surfaces the shell shows over its windows, and whose card the roster shows.
    surfaces: crate::device_lists::Surfaces,
    /// The person at this device, by the id their own presence goes by.
    person: String,
    /// What the reading said last, so a pass that changed nothing is no revision.
    published: RefCell<Option<People>>,
    /// What moved the reading last: presence, the layout, the card, the person and the notices.
    /// A pass whose key is unchanged reads nothing.
    seen: RefCell<Option<Key>>,
}

/// Everything that changes the reading, cheaply: who is here, what the shell shows, this person
/// and what the bar still has to say.
type Key = (
    u64,
    u64,
    String,
    String,
    String,
    String,
    String,
    Vec<String>,
    (String, String),
);

impl PeopleDevice {
    pub(crate) fn new(
        handle: tilo_session_core::desktop::state::DesktopHandle,
        place_here: Rc<dyn Fn(&mut Layout)>,
        sync: Option<Rc<crate::sync::Client>>,
        asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
        surfaces: crate::device_lists::Surfaces,
        person: String,
    ) -> Self {
        Self {
            handle,
            place_here,
            sync,
            asks,
            surfaces,
            person,
            published: RefCell::new(None),
            seen: RefCell::new(None),
        }
    }

    /// What moves the reading, without building it: a pass whose key is unchanged has nothing new
    /// to say.
    fn key(&self) -> Key {
        let own = self.sync.as_ref().map(|sync| sync.own());
        let (waved, arrived, arrived_seen) = self.notices();
        (
            self.handle.revision(),
            self.sync.as_ref().map_or(0, |sync| sync.people_revision()),
            self.surfaces.person.borrow().clone(),
            own.as_ref()
                .map_or_else(String::new, |own| own.label.clone()),
            own.as_ref()
                .map_or_else(String::new, |own| own.availability.id().to_owned()),
            own.as_ref()
                .and_then(|own| own.status.as_ref())
                .map(|status| status.text.clone())
                .unwrap_or_default(),
            own.and_then(|own| own.following).unwrap_or_default(),
            arrived,
            (waved, arrived_seen),
        )
    }

    /// Everyone here in the workspace in front, as the roster lists them: each member presence
    /// says is here, with what their row says of them.
    fn rows(&self) -> Vec<Person> {
        let mut tree = self.handle.read(Clone::clone);
        (self.place_here)(&mut tree);
        let Some(workspace) = tree.active_workspace() else {
            return Vec::new();
        };
        let mut rows: Vec<Person> = workspace
            .members
            .iter()
            .filter_map(|member| {
                let here = workspace
                    .present
                    .iter()
                    .find(|present| present.member == member.id)?;
                Some(person_of(workspace, member, here))
            })
            .collect();
        // People first, software after, each in the order the workspace lists them.
        rows.sort_by_key(|person| person.agent);
        rows
    }

    /// The person at this device, as their own card shows them.
    fn me(&self) -> Person {
        let Some(sync) = &self.sync else {
            let (name, identity) = crate::people::own_face(None, &self.person);
            let (red, green, blue) = crate::people::own_rgb(&identity);
            return Person {
                key: SELF.to_owned(),
                name,
                colour: colour_of((red, green, blue)),
                me: true,
                availability: "available".to_owned(),
                ..Person::default()
            };
        };
        let own = sync.own();
        let (name, identity) = crate::people::own_face(Some(&own), &self.person);
        let (red, green, blue) = crate::people::own_live_rgb(Some(sync), &identity);
        Person {
            key: SELF.to_owned(),
            name,
            colour: colour_of((red, green, blue)),
            me: true,
            availability: own.availability.id().to_owned(),
            doing: own
                .status
                .as_ref()
                .map(|status| status.text.trim().to_owned())
                .filter(|line| !line.is_empty())
                .unwrap_or_default(),
            call: own
                .call
                .as_ref()
                .map_or_else(String::new, |call| call.call.name.clone()),
            ..Person::default()
        }
    }

    /// What the bar still says of a wave or an arrival, as the notices read now.
    fn notices(&self) -> (String, Vec<String>, String) {
        let Some(sync) = &self.sync else {
            return (String::new(), Vec::new(), String::new());
        };
        // The waves heard since the last look, which this shell says itself: the same queue the
        // desktop's own bar reads.
        sync.show_waves();
        let now = ui::time::now();
        let waved = sync
            .wave_notice()
            .filter(|notice| now.saturating_duration_since(notice.at) < WAVE_SHOWN);
        let arrived = sync
            .arrival_notice()
            .filter(|notice| now.saturating_duration_since(notice.at) < ARRIVAL_SHOWN);
        (
            waved.map_or_else(String::new, |notice| notice.from),
            arrived
                .as_ref()
                .map(|notice| notice.names.clone())
                .unwrap_or_default(),
            arrived.and_then(|notice| notice.seen).unwrap_or_default(),
        )
    }

    /// How long until a wave or an arrival stops showing, so the pass that clears it wakes then:
    /// whichever still has time to run, ending soonest.
    fn noticed_in(&self) -> Option<std::time::Duration> {
        let sync = self.sync.as_ref()?;
        let now = ui::time::now();
        let remaining = |shown: ui::time::Duration, at: ui::time::Instant| {
            (now.saturating_duration_since(at) < shown)
                .then(|| shown.saturating_sub(now.saturating_duration_since(at)))
        };
        let wave = sync
            .wave_notice()
            .and_then(|notice| remaining(WAVE_SHOWN, notice.at));
        let arrived = sync
            .arrival_notice()
            .and_then(|notice| remaining(ARRIVAL_SHOWN, notice.at));
        match (wave, arrived) {
            (Some(wave), Some(arrived)) => Some(wave.min(arrived)),
            (wave, arrived) => wave.or(arrived),
        }
    }

    /// The reading as it stands.
    fn read(&self) -> People {
        let (waved, arrived, arrived_seen) = self.notices();
        People {
            rows: self.rows(),
            me: self.me(),
            showing: self.surfaces.person.borrow().clone(),
            following: self
                .sync
                .as_ref()
                .and_then(|sync| sync.own().following)
                .unwrap_or_default(),
            waved,
            arrived,
            arrived_seen,
        }
    }

    /// Carry out what a card or the bar asked of the people.
    fn act(&self, effect: &PeopleEffect) -> Result<Started, ApiError> {
        match effect {
            PeopleEffect::Show => {
                if self.surfaces.overlays.here()
                    == tilo_session_core::desktop::overlays::Overlay::People
                {
                    self.surfaces.close("people");
                } else {
                    self.surfaces.person.borrow_mut().clear();
                    self.surfaces.open("people");
                }
            }
            PeopleEffect::Close => {
                self.surfaces.person.borrow_mut().clear();
                self.surfaces.close("people");
            }
            PeopleEffect::ShowPerson { person } => {
                // An empty key comes back from a card to the roster; anyone else's opens on them.
                *self.surfaces.person.borrow_mut() = person.trim().to_owned();
                self.surfaces.open("people");
            }
            PeopleEffect::Follow { person } => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Follow {
                        who: person.trim().to_owned(),
                        following: true,
                    });
            }
            PeopleEffect::Unfollow => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Follow {
                        who: String::new(),
                        following: false,
                    });
            }
            PeopleEffect::Wave { person } => {
                self.asks.borrow_mut().push(crate::people::PeopleAsk::Wave {
                    who: person.trim().to_owned(),
                });
            }
            PeopleEffect::Message { person } => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Message {
                        key: person.trim().to_owned(),
                    });
            }
            PeopleEffect::GoTo { person } => {
                self.asks.borrow_mut().push(crate::people::PeopleAsk::GoTo {
                    who: person.trim().to_owned(),
                });
            }
            PeopleEffect::SetName { name } => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Rename(name.clone()));
            }
            PeopleEffect::SetDoing { text } => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Status(text.clone()));
            }
            PeopleEffect::SetAvailability { availability } => {
                let chosen = collab_types::presence::Availability::from_id(availability.trim())
                    .ok_or_else(|| {
                        ApiError::InvalidArguments(format!(
                            "`{availability}` is not one of available, focusing, away, idle"
                        ))
                    })?;
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Availability(chosen));
            }
            PeopleEffect::PairDevice => {
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::PairDevice);
            }
            PeopleEffect::OpenSettings { link } => {
                let link = link
                    .as_deref()
                    .map(str::trim)
                    .filter(|link| !link.is_empty())
                    .unwrap_or("settings://");
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Settings(link.to_owned()));
            }
        }
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }
}

/// What the row of `member`, here through `here` in `workspace`, says of them.
fn person_of(
    workspace: &tilo_desktop_model::Workspace,
    member: &Member,
    here: &tilo_desktop_model::Present,
) -> Person {
    let agent = !member.id.is_person();
    let (red, green, blue) = collab_types::presence::identity_rgb(member.colour);
    let titled = here
        .tab
        .and_then(|tab| workspace.tabs().find(|candidate| candidate.id == tab))
        .map(crate::app_names::tab_title)
        .filter(|title| !title.trim().is_empty());
    let app = here
        .app
        .as_deref()
        .map(crate::app_names::app_name)
        .filter(|name| !name.trim().is_empty());
    Person {
        key: here.device.clone(),
        name: member.name.clone(),
        colour: colour_of((red, green, blue)),
        agent,
        me: false,
        availability: here.availability.id().to_owned(),
        doing: here
            .status
            .as_ref()
            .map(|status| status.text.trim().to_owned())
            .filter(|line| !line.is_empty())
            .unwrap_or_default(),
        activity: titled.unwrap_or_default(),
        in_app: app.unwrap_or_default(),
        call: here
            .call
            .as_ref()
            .map_or_else(String::new, |call| call.name.clone()),
        following: here.followed,
        follower: here.follows_you,
    }
}

/// A colour as the reading writes it, `rrggbb`.
fn colour_of((red, green, blue): (u8, u8, u8)) -> String {
    format!("{red:02x}{green:02x}{blue:02x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::MemberId;
    use collab_types::presence::Availability;
    use tilo_desktop_model::{Present, Status, Workspace, WorkspaceId};

    /// A row says what presence and the member row say of someone: where they are, what they are
    /// doing, how available they are, and who follows whom — never the app's id for its name.
    #[test]
    fn a_row_says_where_someone_is_and_who_follows_whom() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Studio");
        let ada = MemberId::person("ada");
        workspace.member_here(tilo_desktop_model::Member::named(
            ada.clone(),
            "Ada Lovelace",
        ));
        let mut here = Present::of(ada);
        here.device = "device-key".to_owned();
        here.app = Some("so.tilo.notes".to_owned());
        here.status = Some(Status {
            text: "Rewriting the launch note".to_owned(),
            automatic: false,
        });
        here.availability = Availability::Away;
        here.followed = true;
        here.follows_you = true;
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert_eq!(row.key, "device-key");
        assert_eq!(row.name, "Ada Lovelace");
        assert_eq!(row.availability, "away");
        assert_eq!(row.doing, "Rewriting the launch note");
        assert_eq!(row.colour.len(), 6, "a colour the shell can paint");
        assert!(row.following, "this person follows them");
        assert!(row.follower, "they follow this person");
        assert!(!row.agent && !row.me);
        assert_ne!(row.in_app, "so.tilo.notes", "the app's name, never its id");
    }

    /// An agent's row says the window it works in, not what a person's does.
    #[test]
    fn an_agents_row_says_the_window_it_works_in() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(2), "Studio");
        let fieldwork = MemberId::agent("fieldwork");
        workspace.member_here(tilo_desktop_model::Member::named(
            fieldwork.clone(),
            "Fieldwork",
        ));
        let mut here = Present::of(fieldwork);
        here.device = "agent-key".to_owned();
        here.app = Some("so.tilo.tasks".to_owned());
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(row.agent);
        assert_ne!(row.in_app, "so.tilo.tasks", "the app's name, never its id");
    }
}

impl Service for PeopleDevice {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![offer::<PeopleEffect>(tilo_resource::typed_resource::<
            PeopleRoot,
        >(
            RESOURCE,
            crate::i18n::t("session-people"),
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.read()))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        let effect = decode::<PeopleEffect>(&self.resources(), resource, action, arguments)?;
        self.act(&effect)
    }

    /// Whether the roster, the person here or what the bar says about a wave or an arrival moved.
    fn poll(&mut self) -> bool {
        let key = self.key();
        if self.seen.borrow().as_ref() == Some(&key) {
            return false;
        }
        let now = self.read();
        *self.seen.borrow_mut() = Some(key);
        if self.published.borrow().as_ref() == Some(&now) {
            return false;
        }
        *self.published.borrow_mut() = Some(now);
        true
    }

    /// When a wave or an arrival stops showing: the pass that clears it has to wake then.
    fn wakes_in(&self) -> Option<std::time::Duration> {
        self.noticed_in()
            .map(|left| left.max(std::time::Duration::from_millis(16)))
    }
}

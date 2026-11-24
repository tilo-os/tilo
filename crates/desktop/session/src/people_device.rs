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
//! are doing, how available they are, the call they are in, whom they follow and when they joined,
//! whether this person follows them and whether they follow this person. Software is a member too:
//! an agent's or a bot's row says which it is, and an agent's says the window it works in. What the
//! row's words are is the shell's, so each language says them its own way; the reading carries the
//! facts.

use std::cell::{Cell, RefCell};
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
/// How long the wave this person sent shows as sent on the row it went to.
pub(crate) const WAVE_SENT_SHOWN: ui::time::Duration = ui::time::Duration::from_millis(2400);
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
    /// Whether this person may invite someone into the workspace in front, as the loop last
    /// answered it: the roster offers its invite only then.
    invitable: Rc<Cell<bool>>,
    /// The person at this device, by the id their own presence goes by.
    person: String,
    /// Whom the wave this person sent just now went to, and when: their row says it went for a
    /// moment.
    waved_to: RefCell<Option<(String, ui::time::Instant)>>,
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
    bool,
    String,
);

impl PeopleDevice {
    pub(crate) fn new(
        handle: tilo_session_core::desktop::state::DesktopHandle,
        place_here: Rc<dyn Fn(&mut Layout)>,
        sync: Option<Rc<crate::sync::Client>>,
        asks: Rc<RefCell<Vec<crate::people::PeopleAsk>>>,
        surfaces: crate::device_lists::Surfaces,
        invitable: Rc<Cell<bool>>,
        person: String,
    ) -> Self {
        Self {
            handle,
            place_here,
            sync,
            asks,
            surfaces,
            invitable,
            person,
            waved_to: RefCell::new(None),
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
            self.invitable.get(),
            self.waved_to(),
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
        let mut raw: Vec<(Person, Option<u64>)> = workspace
            .members
            .iter()
            .filter_map(|member| {
                let here = workspace
                    .present
                    .iter()
                    .find(|present| present.member == member.id)?;
                Some((person_of(workspace, member, here), member.joined_at))
            })
            .collect();
        // Someone nobody named is told apart by when they joined: their name and face are every
        // unnamed person's.
        name_the_unnamed(&mut raw);
        let mut rows: Vec<Person> = raw.into_iter().map(|(person, _)| person).collect();
        // Whom each person here follows, as their presence says: "Following Ada" on the row of
        // someone following Ada, "Following you" on the row of someone following this person.
        if let Some(sync) = &self.sync {
            let workspace_id = workspace.id().to_string();
            let mut pairs: Vec<(String, String)> = sync
                .members(&workspace_id)
                .into_iter()
                .filter_map(|member| Some((member.publisher, member.following?)))
                .collect();
            if let Some(following) = sync.own().following {
                pairs.push((sync.publisher().to_string(), following));
            }
            note_following(&mut rows, &pairs, Some(&sync.publisher().to_string()));
        }
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

    /// Whom the wave this person sent just now went to, while their row still says it went.
    fn waved_to(&self) -> String {
        self.waved_to
            .borrow()
            .as_ref()
            .filter(|(_, at)| ui::time::now().saturating_duration_since(*at) < WAVE_SENT_SHOWN)
            .map(|(key, _)| key.clone())
            .unwrap_or_default()
    }

    /// How long until a wave or an arrival stops showing, so the pass that clears it wakes then:
    /// whichever still has time to run, ending soonest.
    fn noticed_in(&self) -> Option<std::time::Duration> {
        let now = ui::time::now();
        let remaining = |shown: ui::time::Duration, at: ui::time::Instant| {
            (now.saturating_duration_since(at) < shown)
                .then(|| shown.saturating_sub(now.saturating_duration_since(at)))
        };
        let wave = self
            .sync
            .as_ref()
            .and_then(|sync| sync.wave_notice())
            .and_then(|notice| remaining(WAVE_SHOWN, notice.at));
        let arrived = self
            .sync
            .as_ref()
            .and_then(|sync| sync.arrival_notice())
            .and_then(|notice| remaining(ARRIVAL_SHOWN, notice.at));
        let sent = self
            .waved_to
            .borrow()
            .as_ref()
            .and_then(|(_, at)| remaining(WAVE_SENT_SHOWN, *at));
        [wave, arrived, sent].into_iter().flatten().min()
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
            waved_to: self.waved_to(),
            arrived,
            arrived_seen,
            may_invite: self.invitable.get(),
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
                let who = person.trim().to_owned();
                self.waved_to.replace(Some((who.clone(), ui::time::now())));
                self.asks
                    .borrow_mut()
                    .push(crate::people::PeopleAsk::Wave { who });
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
    let bot = member.id.is_bot();
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
        bot,
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
        // Filled in once every row is read: following is a relation between rows.
        follows: String::new(),
        joined: String::new(),
        following: here.followed,
        follower: here.follows_you,
    }
}

/// Give each row nobody named the clock they joined by, from the member row the workspace keeps:
/// their name and face are every unnamed person's, so when they joined is the one fact that tells
/// them apart. Two people admitted within one second would read alike otherwise, so those two say
/// the millisecond as well. A named person's row says nothing of it.
fn name_the_unnamed(rows: &mut [(Person, Option<u64>)]) {
    let joined: Vec<(usize, u64)> = rows
        .iter()
        .enumerate()
        .filter(|(_, (row, _))| ui::presence::is_unnamed_person(&row.name))
        .filter_map(|(index, (_, at))| Some((index, (*at)?)))
        .collect();
    let seconds: Vec<String> = joined
        .iter()
        .map(|(_, at)| clock(*at, ui::format::time_of_with_seconds))
        .collect();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for text in &seconds {
        let count = counts.entry(text.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    for ((index, at), text) in joined.iter().zip(&seconds) {
        let shown = if counts.get(text.as_str()).copied().unwrap_or_default() > 1 {
            clock_millis(*at)
        } else {
            text.clone()
        };
        if let Some((row, _)) = rows.get_mut(*index)
            && !shown.trim().is_empty()
        {
            row.joined = shown;
        }
    }
}

/// The local clock `at` (Unix milliseconds) reads by, to the second, through the person's own
/// time format.
fn clock(at: u64, read: fn(&ui::format::UnixSecs) -> String) -> String {
    i64::try_from(at / 1000)
        .map(|seconds| read(&ui::format::UnixSecs(seconds)))
        .unwrap_or_default()
}

/// The local clock `at` reads by, to the millisecond: what tells two people who joined within one
/// second apart.
fn clock_millis(at: u64) -> String {
    i64::try_from(at)
        .map(|millis| ui::format::time_of_with_millis(&ui::format::UnixMillis(millis)))
        .unwrap_or_default()
}

/// Say on each follower's row whom they follow, from `pairs` of `(follower, followed)` presence
/// keys: the followed row's key, or `self` for this person. A pair naming someone the roster does
/// not list says nothing, and a row already annotated keeps the first pair that names it.
fn note_following(rows: &mut [Person], pairs: &[(String, String)], own: Option<&str>) {
    for (follower, followed) in pairs {
        let target = if own == Some(followed.as_str()) {
            SELF.to_owned()
        } else if rows.iter().any(|row| row.key == *followed) {
            followed.clone()
        } else {
            continue;
        };
        let Some(row) = rows.iter_mut().find(|row| row.key == *follower) else {
            continue;
        };
        if row.follows.is_empty() {
            row.follows = target;
        }
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
        assert!(
            !row.bot,
            "an agent is a run, never one of the workspace's bots"
        );
        assert_ne!(row.in_app, "so.tilo.tasks", "the app's name, never its id");
    }

    /// A bot is software with an installation of its own: its row says so, where an agent's does
    /// not, and neither is ever read as a person.
    #[test]
    fn a_bot_s_row_is_tagged_as_a_bot_and_an_agents_is_not() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(3), "Studio");
        let bot = MemberId::bot("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6");
        workspace.member_here(tilo_desktop_model::Member::of_software(
            bot.clone(),
            "Standup",
        ));
        let mut here = Present::of(bot);
        here.device = "bot-key".to_owned();
        workspace.place_presence(here);
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(row.bot && row.agent, "a bot is software, tagged Bot");

        let mut workspace = Workspace::new(WorkspaceId::from_u128(4), "Studio");
        let ada = MemberId::person("ada");
        workspace.member_here(tilo_desktop_model::Member::named(ada.clone(), "Ada"));
        workspace.place_presence(Present::of(ada));
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert!(!row.bot && !row.agent);
    }

    /// An agent's row names the window it works in, by the title the shell shows, and the app it
    /// runs; a person's row names the window they look at the same way.
    #[test]
    fn an_agents_row_names_the_window_it_works_in() {
        use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
        let notes = Tab {
            id: 7,
            title: "Launch notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let mut main = Strip::new(StripId(1), "Main");
        main.window_groups.push(WindowGroup {
            id: 8,
            active_tab: notes.id,
            tabs: vec![notes],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let mut workspace = Workspace::new(WorkspaceId::from_u128(5), "Design");
        workspace.strips = vec![main];
        let claude = MemberId::agent("claude");
        workspace.member_here(tilo_desktop_model::Member::named(
            claude.clone(),
            "Claude Code",
        ));
        workspace.place_presence(Present {
            tab: Some(7),
            ..Present::of(claude)
        });
        let row = person_of(&workspace, &workspace.members[0], &workspace.present[0]);
        assert_eq!(
            row.activity, "Launch notes",
            "the window, named as every surface names it"
        );
        assert_eq!(row.in_app, "Notes", "and the app it runs, by its name");
    }

    /// A follower's row names whom they follow, for any pair: someone following this person says
    /// `self`, the followed row is not annotated, and a pair naming someone the roster does not
    /// list says nothing.
    #[test]
    fn a_followers_row_says_whom_they_follow() {
        let mut rows = vec![
            person("ada", "Ada", false),
            person("bob", "Bob", false),
            person("cy", "Cy", false),
        ];
        let pairs = [
            ("ada".to_owned(), "bob".to_owned()),
            ("cy".to_owned(), "me".to_owned()),
            ("bob".to_owned(), "gone".to_owned()),
        ];
        note_following(&mut rows, &pairs, Some("me"));
        assert_eq!(rows[0].follows, "bob", "ada follows bob");
        assert_eq!(rows[1].follows, "", "bob is followed, not following");
        assert_eq!(rows[2].follows, SELF, "cy follows this person");
    }

    /// Someone nobody named is told apart by when they joined, never a bare "Someone": two admitted
    /// within one second say the millisecond as well, and a named person's row says none of it.
    #[test]
    fn a_row_nobody_named_says_when_they_joined() {
        ui::i18n::I18nManager::init("en-US");
        let unnamed = ui::presence::unnamed_person_label();
        let mut rows = vec![
            (
                person("early", &unnamed, false),
                Some(1_765_000_000_000_u64),
            ),
            (person("late", &unnamed, false), Some(1_765_000_000_402)),
        ];
        name_the_unnamed(&mut rows);
        let told: Vec<&String> = rows.iter().map(|(row, _)| &row.joined).collect();
        assert_ne!(told[0], told[1], "the two rows read apart: {told:?}");
        assert!(
            told[0].contains(':') && told[1].contains(':'),
            "the clock carries seconds: {told:?}"
        );
        assert!(
            told[0].contains('.') && told[1].contains('.'),
            "and the millisecond where two came within one second: {told:?}"
        );

        let mut rows = vec![
            (
                person("first", &unnamed, false),
                Some(1_765_000_000_000_u64),
            ),
            (person("later", &unnamed, false), Some(1_765_000_007_000)),
        ];
        name_the_unnamed(&mut rows);
        assert!(
            !rows[0].0.joined.contains('.') && !rows[1].0.joined.contains('.'),
            "seconds tell them apart, so no millisecond is needed"
        );
        assert_ne!(rows[0].0.joined, rows[1].0.joined);

        let mut rows = vec![(person("ada", "Ada", false), Some(1_765_000_000_000_u64))];
        name_the_unnamed(&mut rows);
        assert_eq!(
            rows[0].0.joined, "",
            "a named person says when they joined by name"
        );
    }

    /// A row the reading builds for a test: a person or software, named or not.
    fn person(key: &str, name: &str, agent: bool) -> Person {
        Person {
            key: key.to_owned(),
            name: name.to_owned(),
            agent,
            ..Person::default()
        }
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

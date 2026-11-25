//! A workspace's calendar entries as its members and their agents reach them. A change to an
//! entry is written to the calendar server the entry is kept on, so it reaches outside: an
//! agent's change asks its person, and goes ahead only once they allowed it, while the person's
//! own change runs as they make it. What an agent may change at all is an entry it put in
//! itself: what its person entered (with another client of theirs, so the entry came from the
//! server) and an invitation that reached the calendar another way are refused to it with why.
//! It files its person's entry once they allowed it, except one its person invites others to,
//! which filing would have the server write to.
//!
//! The person's answer is the dispatch's: a call that asks reaches it ([`Asks`]) and waits there,
//! and once the person allowed it the dispatch delivers it again as approved.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_action_policy::{MemberAccess, Role, WorkspacePolicy};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::ids::MemberId;
use tilolang_runtime::origin::OriginWrite;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, RuntimeError, Value};
use tilolang_tilo::publish::Caller;

use crate::fixtures::{CALENDAR_SET, Replica, Workspace};

const APP: &str = "so.tilo.calendar";
const WORKSPACE: &str = "workspace";
/// The address her calendar account signs in as.
const ADDRESS: &str = "ada@example.test";

/// The calls the dispatch was handed to ask a person about, each left waiting.
#[derive(Clone, Default)]
struct Asks(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Asks {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let mut asked = self.0.borrow_mut();
        asked.push(invocation.clone());
        Invoked::Pending(format!("asked-{}", asked.len()))
    }
}

impl Asks {
    fn count(&self) -> usize {
        self.0
            .borrow()
            .iter()
            .filter(|asked| matches!(asked, Invocation::Action { asks: true, .. }))
            .count()
    }

    /// What the last question showed, each as its label and value.
    fn shown(&self) -> Vec<(String, String)> {
        match self.0.borrow().last() {
            Some(Invocation::Action { shows, .. }) => shows
                .iter()
                .map(|shown| (shown.label.clone(), shown.value.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// Ada's workspace: she owns it, Bo edits it, and her calendar account's Work calendar holds
/// what her server sent.
struct Ada {
    replica: Replica,
    asks: Asks,
    root: ResourceRef,
}

fn herself() -> Actor {
    Actor::from(MemberId::person("ada"))
}

/// The agent `person`'s device handed its session to.
fn assistant_of(person: &str) -> Caller {
    Caller {
        actor: MemberId::agent("assistant", person).into(),
        agent: true,
        on_behalf_of: Some(person.to_owned()),
        handed: true,
        app: None,
    }
}

impl Ada {
    fn new() -> Self {
        let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
        policy.members.insert("bo".to_owned(), Role::Editor);
        let access = MemberAccess::person(policy, "ada");
        let (replica, _) = Replica::access("ada", Workspace::new(), CALENDAR_SET, Some(access));
        let asks = Asks::default();
        replica.host.runtime().set_foreign_invoker(asks.clone());
        let root = replica.host.root_of(APP).expect("the calendar's root");
        replica.commit();
        let ada = Self {
            replica,
            asks,
            root,
        };
        ada.origin(vec![OriginWrite::Insert {
            owner: ada.root.clone(),
            field: ada.field("Calendars", "accounts"),
            ty: ada.ty("Account"),
            fields: vec![
                (ada.field("Account", "key"), Value::text("work")),
                (ada.field("Account", "address"), Value::text(ADDRESS)),
                (ada.field("Account", "name"), Value::text("Mine")),
            ],
        }]);
        ada.origin(vec![OriginWrite::Insert {
            owner: ada.root.clone(),
            field: ada.field("Calendars", "calendars"),
            ty: ada.ty("Calendar"),
            fields: vec![
                (ada.field("Calendar", "key"), Value::text("/dav/work/")),
                (ada.field("Calendar", "account"), Value::text("work")),
                (ada.field("Calendar", "name"), Value::text("Work")),
                (ada.field("Calendar", "writable"), Value::Bool(true)),
            ],
        }]);
        for agent in ["ada", "bo"] {
            ada.replica.host.admit_agent(&assistant_of(agent));
        }
        ada
    }

    fn meta(&self) -> tilolang::PackageMeta {
        self.replica.host.package(APP).expect("Calendar runs").meta
    }

    fn ty(&self, name: &str) -> String {
        self.meta()
            .types
            .iter()
            .find(|ty| ty.name == name)
            .unwrap_or_else(|| panic!("Calendar declares {name}"))
            .id
            .clone()
    }

    fn field(&self, ty: &str, name: &str) -> String {
        self.meta()
            .types
            .iter()
            .find(|declared| declared.name == ty)
            .and_then(|declared| declared.fields.iter().find(|field| field.name == name))
            .unwrap_or_else(|| panic!("{ty} declares {name}"))
            .id
            .clone()
    }

    fn action(&self, name: &str) -> String {
        self.meta()
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("Calendar declares {name}"))
            .id
            .clone()
    }

    /// What her calendar server sent, written as the calendar origin writes it.
    fn origin(&self, writes: Vec<OriginWrite>) -> Vec<Option<ResourceRef>> {
        let made = self
            .replica
            .host
            .runtime()
            .apply_origin("caldav", writes, None)
            .expect("the origin writes");
        self.replica.commit();
        made
    }

    fn items(&self, field: &str) -> Vec<ResourceRef> {
        match self
            .replica
            .host
            .runtime()
            .read(&self.root, &self.field("Calendars", field))
            .unwrap()
        {
            Value::List(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("{field} reads {other:?}"),
        }
    }

    fn work(&self) -> ResourceRef {
        self.items("calendars")
            .into_iter()
            .find(|k| self.text(k, "Calendar", "account") == "work")
            .expect("her Work calendar")
    }

    fn text(&self, at: &ResourceRef, ty: &str, field: &str) -> String {
        match self
            .replica
            .host
            .runtime()
            .read(at, &self.field(ty, field))
            .unwrap()
        {
            Value::Text(text) => text,
            other => panic!("{field} reads {other:?}"),
        }
    }

    /// An entry her server holds in her Work calendar, as another client of hers left it: it
    /// came from the server, and nobody here put it in. `invites` names the people it invites,
    /// `organizer` who organises it.
    fn entry_from_her_server(
        &self,
        uid: &str,
        title: &str,
        organizer: &str,
        invites: &[&str],
    ) -> ResourceRef {
        let event = self
            .origin(vec![OriginWrite::Insert {
                owner: self.root.clone(),
                field: self.field("Calendars", "events"),
                ty: self.ty("Event"),
                fields: vec![
                    (self.field("Event", "key"), Value::text(uid)),
                    (
                        self.field("Event", "href"),
                        Value::text(format!("/dav/work/{uid}.ics")),
                    ),
                    (self.field("Event", "title"), Value::text(title)),
                    (
                        self.field("Event", "start"),
                        Value::text("2026-11-20T10:00:00"),
                    ),
                    (
                        self.field("Event", "end"),
                        Value::text("2026-11-20T11:00:00"),
                    ),
                    (self.field("Event", "day"), Value::text("2026-11-20")),
                    (self.field("Event", "end_day"), Value::text("2026-11-20")),
                    (self.field("Event", "organizer"), Value::text(organizer)),
                    (
                        self.field("Event", "calendar"),
                        Value::Opt(Some(Box::new(Value::resource(&self.work())))),
                    ),
                ],
            }])
            .remove(0)
            .expect("the entry");
        for address in invites {
            self.origin(vec![OriginWrite::Insert {
                owner: event.clone(),
                field: self.field("Event", "attendees"),
                ty: self.ty("Attendee"),
                fields: vec![
                    (self.field("Attendee", "name"), Value::text(*address)),
                    (self.field("Attendee", "email"), Value::text(*address)),
                ],
            }]);
        }
        event
    }

    fn categories(&self, at: &ResourceRef) -> Vec<String> {
        match self
            .replica
            .host
            .runtime()
            .read(at, &self.field("Event", "categories"))
            .unwrap()
        {
            Value::List(items) => items
                .into_iter()
                .map(|item| match item {
                    Value::Text(text) => text,
                    other => panic!("a category reads {other:?}"),
                })
                .collect(),
            other => panic!("categories read {other:?}"),
        }
    }

    /// `who`'s call of `action` with `args`, as the dispatch hands it over unanswered.
    fn call(
        &self,
        who: &Actor,
        action: &str,
        args: Vec<Value>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let outcome = self
            .replica
            .host
            .runtime()
            .invoke_action(who, (self.action(action).as_str(), args));
        self.replica.commit();
        outcome
    }

    /// `who`'s call once its person allowed it, as the dispatch delivers it again.
    fn allowed(
        &self,
        who: &Actor,
        action: &str,
        args: Vec<Value>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let outcome = self.replica.host.runtime().invoke_approved_through(
            who,
            None,
            (self.action(action).as_str(), args),
            None,
        );
        self.replica.commit();
        outcome
    }

    /// Why the guard of `action` refuses `who` on `at`, if it does: what the catalogue says of
    /// the action before anybody is asked about it.
    fn refusal(&self, action: &str, at: &ResourceRef, who: &Actor) -> Option<String> {
        self.replica
            .host
            .runtime()
            .refusal(&self.action(action), at, who)
            .expect("the guard reads")
    }

    fn filing(at: &ResourceRef, categories: &[&str]) -> Vec<Value> {
        vec![
            Value::resource(at),
            Value::List(categories.iter().map(|c| Value::text(*c)).collect()),
        ]
    }

    fn taking_in(&self, text: &str) -> Vec<Value> {
        vec![Value::resource(&self.root), Value::text(text)]
    }

    fn entry_under(&self, uid: &str) -> Option<ResourceRef> {
        self.items("events")
            .into_iter()
            .find(|e| self.text(e, "Event", "key") == uid)
    }
}

fn asked(what: &str, outcome: Result<InvokeOutcome, RuntimeError>) {
    assert!(
        matches!(outcome, Ok(InvokeOutcome::Asked(_))),
        "{what} waits for its person's answer: {outcome:?}"
    );
}

fn refused(what: &str, outcome: Result<InvokeOutcome, RuntimeError>) -> String {
    match outcome {
        Err(RuntimeError::Trap(reason)) => reason,
        other => panic!("{what} is refused: {other:?}"),
    }
}

/// An invitation `organizer` sends, as a message carries it under `method`.
fn invitation(method: &str, uid: &str, organizer: &str, invited: &[&str]) -> String {
    use std::fmt::Write as _;
    let people = invited.iter().fold(String::new(), |mut people, address| {
        let _ = write!(
            people,
            "ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{address}\r\n"
        );
        people
    });
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
         SUMMARY:Offsite\r\nDTSTART;TZID=Europe/Berlin:20261119T140000\r\nDURATION:PT30M\r\n\
         ORGANIZER;CN=Organiser:mailto:{organizer}\r\n{people}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

tilo_test_env::scenario! {
    fn every_members_agent_asks_and_changes_only_what_it_put_in;

    phase an_agents_change_asks_its_person_and_hers_runs_as_she_makes_it;
    {
        let ada = Ada::new();
        let board = ada.entry_from_her_server("board-7@example.org", "Board meeting", "", &[]);
        let assistant = assistant_of("ada").actor;

        // Her assistant files what she entered herself only once she allowed it: with no
        // answer, or her no, it stays as it was.
        asked("filing her own entry", ada.call(&assistant, "set_categories", Ada::filing(&board, &["Cancelled"])));
        assert_eq!(ada.asks.count(), 1, "the change asked her");
        assert!(ada.categories(&board).is_empty(), "nothing she did not allow");
        let done = ada.allowed(&assistant, "set_categories", Ada::filing(&board, &["Board", "Quarter"]));
        assert!(matches!(done, Ok(InvokeOutcome::Done(_))), "{done:?}");
        assert_eq!(ada.categories(&board), ["Board", "Quarter"]);

        // Her own filing asks her nothing.
        let hers = ada.call(&herself(), "set_categories", Ada::filing(&board, &["Board"]));
        assert!(matches!(hers, Ok(InvokeOutcome::Done(_))), "{hers:?}");
        assert_eq!(ada.categories(&board), ["Board"]);
        assert_eq!(ada.asks.count(), 1, "her own change asks her nothing");

        // Bo's agent asks too: an agent's change reaches the server whoever's agent it is.
        asked(
            "another member's agent filing",
            ada.call(&assistant_of("bo").actor, "set_categories", Ada::filing(&board, &["Bo's"])),
        );
        assert_eq!(ada.categories(&board), ["Board"]);

        // Somebody who is no member of the workspace is nobody to its calendar.
        let stranger = Actor::from(MemberId::person("zed"));
        let theirs = ada.call(&stranger, "set_categories", Ada::filing(&board, &["Zed's"]));
        assert!(matches!(theirs, Ok(InvokeOutcome::Denied)), "{theirs:?}");
        assert_eq!(ada.categories(&board), ["Board"]);
    }

    phase what_she_entered_herself_is_not_her_assistants_to_change_or_remove;
    {
        let ada = Ada::new();
        let board = ada.entry_from_her_server("board-7@example.org", "Board meeting", "", &[]);
        let assistant = assistant_of("ada").actor;
        let why = ada
            .refusal("organize", &board, &assistant)
            .expect("her own entry is refused to her assistant before she is asked");
        assert!(why.contains("its person"), "{why}");
        assert_eq!(ada.refusal("organize", &board, &herself()), None, "she changes what is hers");
        // Removing it, even were she asked, takes nothing out.
        let why = refused(
            "removing her own entry",
            ada.allowed(&assistant, "remove", vec![Value::resource(&ada.root), Value::resource(&board)]),
        );
        assert!(why.contains("its person"), "{why}");
        assert!(ada.entry_under("board-7@example.org").is_some(), "the entry stays");
        // She removes it herself.
        let hers = ada.call(&herself(), "remove", vec![Value::resource(&ada.root), Value::resource(&board)]);
        assert!(matches!(hers, Ok(InvokeOutcome::Done(_))), "{hers:?}");
        assert!(ada.entry_under("board-7@example.org").is_none());
    }

    phase filing_what_she_invites_others_to_is_not_her_assistants;
    {
        let ada = Ada::new();
        let dinner = ada.entry_from_her_server("dinner-3@example.org", "Dinner", ADDRESS, &["bo@example.test"]);
        let why = ada
            .refusal("set_categories", &dinner, &assistant_of("ada").actor)
            .expect("an entry she invites others to is refused to her assistant");
        assert!(why.contains("its person"), "{why}");
        assert_eq!(ada.refusal("set_categories", &dinner, &herself()), None);
        assert_eq!(ada.asks.count(), 0, "she is not asked to allow what would have her server write to people");
    }

    phase an_invitation_goes_in_once_she_allowed_it_and_is_its_agents_own;
    {
        let ada = Ada::new();
        let assistant = assistant_of("ada").actor;
        let offsite = invitation("REQUEST", "offsite-9@example.org", "bo@example.test", &[ADDRESS, "cy@example.test"]);

        asked("an invitation put in", ada.call(&assistant, "take_invitation", ada.taking_in(&offsite)));
        assert!(ada.entry_under("offsite-9@example.org").is_none(), "nothing goes in unallowed");
        let shown = ada.asks.shown();
        assert!(
            shown.iter().any(|(_, value)| value.contains("ORGANIZER;CN=Organiser:mailto:bo@example.test")
                && value.contains("cy@example.test")),
            "she reads who sends it and whom it invites: {shown:?}"
        );

        let made = ada.allowed(&assistant, "take_invitation", ada.taking_in(&offsite));
        assert!(matches!(made, Ok(InvokeOutcome::Done(_))), "{made:?}");
        let its_own = ada.entry_under("offsite-9@example.org").expect("the invitation went in");
        assert_eq!(ada.text(&its_own, "Event", "organizer"), "bo@example.test");
        assert_eq!(ada.refusal("organize", &its_own, &assistant), None, "what it put in is its own to change");

        // Its sender's later version goes in once she allowed it; a cancellation from somebody
        // else takes nothing out.
        let later = invitation("REQUEST", "offsite-9@example.org", "bo@example.test", &[ADDRESS]);
        let moved = ada.allowed(&assistant, "take_invitation", ada.taking_in(&later));
        assert!(matches!(moved, Ok(InvokeOutcome::Done(_))), "{moved:?}");
        let forged = invitation("CANCEL", "offsite-9@example.org", "mallory@elsewhere.test", &[ADDRESS]);
        let why = refused("a cancellation from another sender", ada.allowed(&assistant, "take_invitation", ada.taking_in(&forged)));
        assert!(why.contains("sender"), "{why}");
        assert!(ada.entry_under("offsite-9@example.org").is_some());
    }

    phase an_invitation_that_names_people_shows_her_every_address_before_it_goes_in;
    {
        let ada = Ada::new();
        let assistant = assistant_of("ada").actor;
        let rows = |shown: Vec<(String, String)>, label: &str| -> Vec<String> {
            shown
                .into_iter()
                .filter(|(l, _)| l == label)
                .map(|(_, value)| value)
                .collect()
        };
        let dinner = invitation("REQUEST", "dinner-3@example.org", "bo@example.test", &[ADDRESS, "eve@elsewhere.test"]);

        // Her assistant's call waits for her, showing how many addresses it names, who
        // organises it and each address it invites.
        asked("an invitation that names people", ada.call(&assistant, "take_invitation", ada.taking_in(&dinner)));
        let shown = ada.asks.shown();
        assert_eq!(rows(shown.clone(), "Addresses named"), ["3"], "{shown:?}");
        assert_eq!(rows(shown.clone(), "Organised by"), ["bo@example.test"], "{shown:?}");
        assert_eq!(rows(shown.clone(), "Invites"), [format!("{ADDRESS}\neve@elsewhere.test")], "{shown:?}");
        assert!(ada.entry_under("dinner-3@example.org").is_none(), "nothing unallowed");

        // Bo's agent is asked about the same rows; somebody who is no member is nobody here.
        asked(
            "another member's agent",
            ada.call(&assistant_of("bo").actor, "take_invitation", ada.taking_in(&dinner)),
        );
        assert_eq!(ada.asks.count(), 2);
        assert_eq!(rows(ada.asks.shown(), "Organised by"), ["bo@example.test"]);
        let stranger = Actor::from(MemberId::person("zed"));
        let theirs = ada.call(&stranger, "take_invitation", ada.taking_in(&dinner));
        assert!(matches!(theirs, Ok(InvokeOutcome::Denied)), "{theirs:?}");

        // More addresses than one question shows, and an address she could misread: refused
        // with nobody asked.
        let guests: Vec<String> = (0..21).map(|guest| format!("guest{guest}@example.test")).collect();
        let guests: Vec<&str> = guests.iter().map(String::as_str).collect();
        let crowd = invitation("REQUEST", "crowd-5@example.org", "bo@example.test", &guests);
        let why = format!(
            "{:?}",
            ada.call(&assistant, "take_invitation", ada.taking_in(&crowd)).expect_err("too many to show")
        );
        assert!(why.contains("22 addresses"), "{why}");
        let misread = invitation("REQUEST", "misread-6@example.org", "bo@example.test", &["b\u{f6}@example.test"]);
        let why = format!(
            "{:?}",
            ada.call(&assistant, "take_invitation", ada.taking_in(&misread)).expect_err("one she could misread")
        );
        assert!(why.contains("plain ASCII"), "{why}");
        assert_eq!(ada.asks.count(), 2, "she is asked nothing about what cannot be shown whole");
        assert!(ada.entry_under("crowd-5@example.org").is_none() && ada.entry_under("misread-6@example.org").is_none());

        // Her own word asks her nothing, whatever it names.
        let hers = ada.call(&herself(), "take_invitation", ada.taking_in(&crowd));
        assert!(matches!(hers, Ok(InvokeOutcome::Done(_))), "{hers:?}");
        assert_eq!(ada.asks.count(), 2);

        // Inviting somebody to an entry its assistant put in shows her the entry, who organises
        // it, who comes already and whom it would invite.
        let made = ada.allowed(&assistant, "take_invitation", ada.taking_in(&dinner));
        assert!(matches!(made, Ok(InvokeOutcome::Done(_))), "{made:?}");
        let its_own = ada.entry_under("dinner-3@example.org").expect("the invitation went in");
        asked(
            "inviting somebody",
            ada.call(
                &assistant,
                "invite",
                vec![Value::resource(&its_own), Value::text("Cy"), Value::text("cy@example.test")],
            ),
        );
        let shown = ada.asks.shown();
        assert_eq!(rows(shown.clone(), "Title"), ["Offsite"], "{shown:?}");
        assert_eq!(rows(shown.clone(), "Organizer"), ["bo@example.test"], "{shown:?}");
        assert_eq!(rows(shown.clone(), "Email"), ["cy@example.test"], "{shown:?}");
        assert_eq!(rows(shown, "Guests"), [""], "nobody comes yet");
    }

    phase every_members_agent_reads_the_calendars_and_their_notes_and_changes_nothing;
    {
        let ada = Ada::new();
        let review = ada.entry_from_her_server("review-1@example.test", "Review", "bo@elsewhere.test", &[ADDRESS]);
        ada.origin(vec![OriginWrite::Set {
            target: review.clone(),
            fields: vec![(ada.field("Event", "notes"), Value::text("Bring the numbers, all of them."))],
        }]);
        let read = |reader: &Actor| {
            ada.replica.host.read_resource(
                &tilolang_tilo::ReadAsk::of(Some(&tilolang_tilo::publish::resource_id(APP, &review.id_string()))),
                &ada.replica.host.reads_for(reader),
            )
        };
        for (who, caller) in [("her assistant", assistant_of("ada")), ("Bo's agent", assistant_of("bo"))] {
            let entry = read(&caller.reader()).unwrap_or_else(|why| panic!("{who} reads the entry: {why}"));
            let text = entry.to_string();
            assert!(text.contains("Review"), "{who}: {text}");
            assert!(text.contains("Bring the numbers, all of them."), "{who} reads its notes: {text}");
        }
        for stranger in [Actor::from(MemberId::person("zed")), Actor::from(MemberId::agent("assistant", "zed"))] {
            assert!(read(&stranger).is_err(), "{stranger:?} reads nothing of the workspace's calendars");
        }
        assert!(ada.replica.host.runtime().pending_effects().is_empty(), "a read tells her server nothing");
        assert_eq!(ada.asks.count(), 0, "and asks nobody");
    }

    phase busy_times_are_read_by_the_workspaces_members_and_their_agents_alone;
    {
        let ada = Ada::new();
        ada.entry_from_her_server("review-1@example.test", "Review", "bo@elsewhere.test", &[ADDRESS]);
        let week = |who: &Actor| {
            ada.call(
                who,
                "busy",
                vec![
                    Value::resource(&ada.root),
                    Value::text("2026-11-16"),
                    Value::text("2026-11-22"),
                ],
            )
        };
        for (who, caller) in [
            ("she", herself()),
            ("her assistant", assistant_of("ada").actor),
            ("Bo's agent", assistant_of("bo").actor),
        ] {
            match week(&caller) {
                Ok(InvokeOutcome::Done(Value::List(taken))) => {
                    assert_eq!(taken.len(), 1, "{who} reads the one time the week holds taken: {taken:?}");
                    let shown = format!("{taken:?}");
                    assert!(!shown.contains("Review"), "a busy time says nothing of what the entry is: {shown}");
                }
                other => panic!("{who} reads when the calendars are busy: {other:?}"),
            }
        }
        let theirs = week(&Actor::from(MemberId::person("zed")));
        assert!(matches!(theirs, Ok(InvokeOutcome::Denied)), "who is no member reads nothing: {theirs:?}");
        let agents = week(&Actor::from(MemberId::agent("assistant", "zed")));
        assert!(matches!(agents, Ok(InvokeOutcome::Denied)), "nor does their agent: {agents:?}");
        assert_eq!(ada.asks.count(), 0, "a read asks nobody");
        assert!(ada.replica.host.runtime().pending_effects().is_empty(), "and tells her server nothing");
    }

    phase an_invitation_her_assistant_did_not_put_in_is_not_its_to_change;
    {
        let ada = Ada::new();
        // Bo's review, as her calendar server filed it.
        ada.entry_from_her_server("review-5@example.org", "Review", "bo@example.test", &[ADDRESS]);
        let assistant = assistant_of("ada").actor;
        for method in ["CANCEL", "REQUEST"] {
            let text = invitation(method, "review-5@example.org", "bo@example.test", &[ADDRESS]);
            let why = refused(method, ada.allowed(&assistant, "take_invitation", ada.taking_in(&text)));
            assert!(why.contains("its person"), "{method}: {why}");
        }
        // She changes it by her own word.
        let hers = ada.call(
            &herself(),
            "take_invitation",
            ada.taking_in(&invitation("REQUEST", "review-5@example.org", "bo@example.test", &[ADDRESS])),
        );
        assert!(matches!(hers, Ok(InvokeOutcome::Done(_))), "{hers:?}");
        assert_eq!(ada.asks.count(), 0, "her own word asks her nothing");
    }

    phase a_calendar_connected_for_reading_only_refuses_every_change_before_anybody_is_asked;
    {
        // Her calendar account is connected for reading only: its server's calendar is one
        // that is not written, as the calendar origin writes it.
        let ada = Ada::new();
        ada.origin(vec![OriginWrite::Set {
            target: ada.work(),
            fields: vec![(ada.field("Calendar", "writable"), Value::Bool(false))],
        }]);
        let board = ada.entry_from_her_server("board-7@example.org", "Board meeting", "", &[]);
        let dinner = ada.entry_from_her_server("dinner-3@example.org", "Dinner", ADDRESS, &["bo@example.test"]);
        let guest = match ada
            .replica
            .host
            .runtime()
            .read(&dinner, &ada.field("Event", "attendees"))
            .unwrap()
        {
            Value::List(items) => items
                .into_iter()
                .find_map(|item| match item {
                    Value::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .expect("Bo is invited"),
            other => panic!("the guests read {other:?}"),
        };
        let changes: Vec<(&str, &ResourceRef, Vec<Value>)> = vec![
            ("set_categories", &board, Ada::filing(&board, &["Board"])),
            ("organize", &board, vec![Value::resource(&board), Value::text("bo@example.test")]),
            ("set_categories", &dinner, Ada::filing(&dinner, &["Team"])),
            ("uninvite", &dinner, vec![Value::resource(&dinner), Value::resource(&guest)]),
            ("answer", &dinner, vec![Value::resource(&dinner), Value::resource(&guest), Value::text("accepted")]),
        ];

        // Her own change is refused in words that say why, before it runs.
        for (action, at, args) in &changes {
            let why = ada
                .refusal(action, at, &herself())
                .unwrap_or_else(|| panic!("{action} is refused to her before she is asked"));
            assert!(why.contains("reading only"), "{action}: {why}");
            let why = refused(action, ada.call(&herself(), action, args.clone()));
            assert!(why.contains("reading only"), "{action}: {why}");
        }
        let why = refused(
            "removing her own entry",
            ada.call(&herself(), "remove", vec![Value::resource(&ada.root), Value::resource(&board)]),
        );
        assert!(why.contains("reading only"), "{why}");

        // Her assistant's is refused before anybody is asked, also where she would allow it.
        let assistant = assistant_of("ada").actor;
        for (action, at, args) in &changes {
            assert!(ada.refusal(action, at, &assistant).is_some(), "{action}");
            refused(action, ada.allowed(&assistant, action, args.clone()));
        }
        assert_eq!(ada.asks.count(), 0, "nobody is asked about what is refused");

        // Nothing changed.
        assert!(ada.entry_under("board-7@example.org").is_some(), "the entry stays");
        assert!(ada.categories(&board).is_empty() && ada.categories(&dinner).is_empty());
        assert_eq!(ada.text(&board, "Event", "organizer"), "");
        assert_eq!(ada.text(&guest, "Attendee", "email"), "bo@example.test");
    }
}

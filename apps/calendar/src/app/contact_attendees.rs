//! The person's shared Contacts as attendee choices for one event, in the
//! system picker. A choice is bound to the calendar's own `invite` verb, so
//! adding someone is checked, confirmed, recorded and undone as typing their
//! address is. Only the reviewed name and address enter the calendar; which
//! contact they came from stays on this device.

use tilo_contact_directory::{ContactDirectory, EmailHint};
use ui::framework::LiveRow;
use ui::tilo_resource::{ActionDecl, SearchEntry};

use super::CalendarApp;
use crate::model::{Calendar, Id};
use crate::resource::{CalendarEffect, t, t_args};

/// The picker's scope and the row its choices hang under.
const SCOPE: &str = "calendar.contacts";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct Choice {
    event: Id,
    #[serde(flatten)]
    hint: EmailHint,
}

impl Choice {
    /// The reviewed facts themselves, so a changed contact or event names
    /// another row and an earlier confirmation cannot rebind to it.
    fn id(&self) -> String {
        serde_json::to_vec(self).map_or_else(
            |_| String::new(),
            |bytes| format!("{SCOPE}:{}", hex::encode(bytes)),
        )
    }

    fn label(&self) -> String {
        t_args(
            "calendar-contact-attendee",
            &[
                ("person", self.hint.label.clone().into()),
                ("email", self.hint.email.clone().into()),
            ],
        )
    }

    fn declaration(&self) -> Option<ActionDecl<Calendar>> {
        let params = crate::resource::calendar::Invite::paths();
        let declaration = Calendar::INVITE
            .bind(params.event, crate::model::key(self.event))
            .and(params.email, self.hint.email.clone())
            .and(params.name, self.hint.label.clone())
            .declaration(Calendar::INVITE.id, t("calendar-contact-add"))?;
        // A name and an address from a private address book become visible
        // to everyone the calendar is shared with: the person agrees first.
        Some(declaration.describe(self.label()).destructive())
    }
}

impl CalendarApp {
    pub(crate) fn follow_contacts(
        &mut self,
        record: Option<ui::framework::PersonRecord<ContactDirectory>>,
    ) {
        self.contact_directory = record;
    }

    /// Every contact address not yet among the attendees of the event the
    /// picker is open for.
    fn contact_choices(&self) -> Vec<Choice> {
        let (Some(event), Some(record)) = (
            self.contact_event.and_then(|id| self.model.event(id)),
            &self.contact_directory,
        ) else {
            return Vec::new();
        };
        record.with(|directory| {
            directory
                .email_hints()
                .into_iter()
                .filter(|hint| {
                    !event
                        .attendees
                        .iter()
                        .any(|attendee| attendee.email.eq_ignore_ascii_case(&hint.email))
                })
                .map(|hint| Choice {
                    event: event.id,
                    hint,
                })
                .collect()
        })
    }

    fn contact_choice(&self, id: &str) -> Option<Choice> {
        self.contact_choices()
            .into_iter()
            .find(|choice| choice.id() == id)
    }

    pub(crate) fn contact_rows(&self) -> Vec<LiveRow> {
        let choices = self.contact_choices();
        if choices.is_empty() {
            return Vec::new();
        }
        let collection = choices.iter().fold(
            LiveRow::new(SCOPE, SCOPE, t("calendar-contact-attendees")),
            |collection, choice| {
                collection.row(LiveRow::new(
                    choice.id(),
                    "calendar.contact-attendee",
                    choice.label(),
                ))
            },
        );
        vec![collection]
    }

    pub(crate) fn contact_row(&self, id: &str) -> Option<LiveRow> {
        if id == SCOPE {
            return self.contact_rows().pop();
        }
        let choice = self.contact_choice(id)?;
        Some(LiveRow::new(
            choice.id(),
            "calendar.contact-attendee",
            choice.label(),
        ))
    }

    /// Adding the contact a row offers, to the event it was offered for,
    /// while both still stand as they were.
    pub(crate) fn contact_declarations(&self, row: &str) -> Vec<ActionDecl<Calendar>> {
        self.contact_choice(row)
            .and_then(|choice| choice.declaration())
            .into_iter()
            .collect()
    }

    pub(crate) fn contact_search_entries(&self) -> Vec<SearchEntry> {
        self.contact_choices()
            .into_iter()
            .map(|choice| {
                SearchEntry::new(choice.hint.label.clone(), choice.hint.email.clone())
                    .inside(SCOPE)
                    .anchored(choice.id())
                    .invoking(Calendar::INVITE.on(choice.id()))
            })
            .collect()
    }

    /// Show the picker of contacts to invite to `event`.
    pub(crate) fn open_contact_attendees(&mut self, event: &str) -> Result<(), String> {
        let event = event
            .parse::<Id>()
            .ok()
            .filter(|id| self.model.event(*id).is_some())
            .ok_or_else(|| "that event is gone".to_string())?;
        self.contact_event = Some(event);
        self.search_opener.open(ui::search::SearchRequest::within(
            ui::search::ScopeDecl::new(SCOPE, t("calendar-contact-attendees")),
        ));
        Ok(())
    }

    /// The Guests field's Contacts button asked for the picker: open it for
    /// the event the editor shows.
    pub(crate) fn take_contacts_request(&self) {
        if !self.contacts_requested.take() {
            return;
        }
        if let Some(super::Sel::Event(event)) = self.selection {
            let effect = CalendarEffect::ContactAttendees {
                event_id: event.to_string(),
            };
            self.invoke_effect(&effect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::{ResourceApp, ViewCx, testing::TestApp};
    use ui::tilo_resource::Id as RecordId;
    use ui::widget::{Element, Text};

    /// Contacts, the owner of the person's address book, in its own window:
    /// Calendar reads the same record without being able to edit it.
    struct ContactsOwner;
    impl ResourceApp for ContactsOwner {
        type Resource = ContactDirectory;
        const ID: &'static str = "so.tilo.contacts";
        const TITLE: &'static str = "Contacts fixture";
        fn view(_: &mut ViewCx<ContactDirectory>) -> Element {
            Box::new(Text::new("Contacts fixture"))
        }
    }

    const FIRST: &str = "00000000-0000-4000-8000-000000000001";
    const SECOND: &str = "00000000-0000-4000-8000-000000000002";

    fn fixture() -> (TestApp<ContactsOwner>, TestApp<crate::CalendarShell>, Id) {
        let mut owner = TestApp::<ContactsOwner>::new().unwrap();
        let directory = owner.shared_person::<ContactDirectory>().unwrap();
        for person in [FIRST, SECOND] {
            let person = RecordId::from_raw(person);
            directory
                .invoke(&ContactDirectory::PROJECT_CONTACT.with((person.clone(), "Alex".into())))
                .unwrap();
            directory
                .invoke(
                    &ContactDirectory::PROJECT_EMAILS
                        .with((person, vec!["Alex@EXAMPLE.test".into()])),
                )
                .unwrap();
        }
        let mut calendar = TestApp::<crate::CalendarShell>::new().unwrap();
        let id = uuid::Uuid::new_v4();
        calendar
            .invoke(
                Calendar::NEW_EVENT.with(crate::resource::calendar::NewEvent {
                    event: crate::actions::EventData {
                        id: id.to_string(),
                        title: "Planning".into(),
                        start: "2026-03-02T09:00".into(),
                        end: "2026-03-02T10:00".into(),
                        ..crate::actions::EventData::default()
                    },
                }),
            )
            .unwrap();
        calendar
            .surface_mut::<CalendarApp>()
            .follow_contacts(Some(directory));
        calendar.settle();
        (owner, calendar, id)
    }

    fn open(calendar: &mut TestApp<crate::CalendarShell>, event: Id) -> Vec<Choice> {
        calendar
            .effect(&CalendarEffect::ContactAttendees {
                event_id: event.to_string(),
            })
            .unwrap();
        calendar.surface_mut::<CalendarApp>().contact_choices()
    }

    #[test]
    fn a_confirmed_contact_choice_invites_through_the_calendars_own_verb() {
        let (owner, mut calendar, event) = fixture();
        assert!(
            calendar
                .surface_mut::<CalendarApp>()
                .contact_choices()
                .is_empty(),
            "nothing is offered before the picker opens for an event"
        );
        let choices = open(&mut calendar, event);
        assert_eq!(
            choices.len(),
            2,
            "two people sharing a name stay two choices"
        );
        let chosen = choices
            .iter()
            .find(|choice| choice.hint.person == SECOND)
            .unwrap()
            .clone();
        assert!(
            calendar.agent_surface().to_string().contains(&chosen.id()),
            "the choice is published where agents and the palette find it"
        );
        assert!(
            calendar
                .search_index_entries()
                .iter()
                .any(|entry| entry.title == "Alex"),
            "the picker finds the contact by name"
        );
        assert!(
            calendar
                .call_on(&chosen.id(), Calendar::INVITE.id, serde_json::json!({}))
                .is_err(),
            "adding a contact needs the person's confirmation"
        );
        assert!(calendar.state().event(event).unwrap().attendees.is_empty());
        calendar
            .call_confirmed_on(&chosen.id(), Calendar::INVITE.id, serde_json::json!({}))
            .unwrap();
        calendar.settle();
        let attendees = calendar.state().event(event).unwrap().attendees.clone();
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].name, "Alex");
        assert_eq!(attendees[0].email, "Alex@example.test");
        assert_ne!(
            attendees[0].id.to_string(),
            SECOND,
            "the contact's id stays in the address book"
        );
        assert!(
            open(&mut calendar, event).is_empty(),
            "an invited address is no longer offered, by either person"
        );
        assert!(
            calendar
                .call_confirmed_on(&choices[0].id(), Calendar::INVITE.id, serde_json::json!({}))
                .is_err(),
            "an address already invited is not invited twice"
        );
        drop(owner);
    }

    #[test]
    fn a_changed_or_removed_contact_refuses_its_earlier_choice() {
        let (mut owner, mut calendar, event) = fixture();
        let chosen = open(&mut calendar, event)[0].clone();
        let directory = owner.shared_person::<ContactDirectory>().unwrap();
        directory
            .invoke(
                &ContactDirectory::PROJECT_CONTACT
                    .with((RecordId::from_raw(chosen.hint.person.clone()), "Sam".into())),
            )
            .unwrap();
        calendar.settle();
        assert!(
            calendar
                .call_confirmed_on(&chosen.id(), Calendar::INVITE.id, serde_json::json!({}))
                .is_err(),
            "a renamed contact is chosen again, under its new name"
        );
        let chosen = open(&mut calendar, event)
            .into_iter()
            .find(|choice| choice.hint.label == "Sam")
            .unwrap();
        directory
            .invoke(
                &ContactDirectory::REMOVE_CONTACT
                    .with(RecordId::from_raw(chosen.hint.person.clone())),
            )
            .unwrap();
        calendar.settle();
        assert!(
            calendar
                .call_confirmed_on(&chosen.id(), Calendar::INVITE.id, serde_json::json!({}))
                .is_err()
        );
        assert!(calendar.state().event(event).unwrap().attendees.is_empty());
        assert!(
            calendar
                .shared_person::<ContactDirectory>()
                .unwrap()
                .invoke(&ContactDirectory::REMOVE_CONTACT.with(RecordId::from_raw(FIRST)))
                .is_err(),
            "Calendar cannot edit the address book"
        );
    }
}

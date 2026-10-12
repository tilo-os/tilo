//! Private Contacts hints in event-scoped system pickers. Only reviewed names
//! and addresses enter the shared calendar; person and sender IDs stay local.
use tilo_contact_directory::{ContactDirectory, canonical_email_hint};
use ui::framework::LiveRow;
use ui::tilo_resource::{Effect, Invoke, SearchEntry};

use super::CalendarApp;
use crate::model::{Calendar, Id, key};
use crate::resource::CalendarEffect;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Choice {
    Attendee {
        event: String,
        event_label: String,
        person: String,
        label: String,
        email: String,
    },
    Invitation {
        event: String,
        event_snapshot: String,
        event_label: String,
        attendee: String,
        email: String,
        sender_account: String,
        sender_email: String,
    },
}

impl Choice {
    fn payload(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    fn id(&self) -> String {
        format!("calendar.contact-choice:{}", hex::encode(self.payload()))
    }

    fn effect(&self) -> CalendarEffect {
        match self {
            Self::Attendee { .. } => CalendarEffect::AddContactAttendee {
                choice: self.payload(),
            },
            Self::Invitation { .. } => CalendarEffect::PrepareInMailFrom {
                choice: self.payload(),
            },
        }
    }

    fn scope(&self) -> String {
        match self {
            Self::Attendee { event, .. } => format!("calendar.contact-attendees:{event}"),
            Self::Invitation {
                event, attendee, ..
            } => format!("calendar.invitation-senders:{event}:{attendee}"),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Attendee {
                event_label,
                person,
                label,
                email,
                ..
            } => crate::i18n::t_args(
                "calendar-contact-choice",
                &[
                    ("person", label.clone().into()),
                    ("id", person.clone().into()),
                    ("email", email.clone().into()),
                    ("event", event_label.clone().into()),
                ],
            ),
            Self::Invitation {
                email,
                event_label,
                sender_account,
                sender_email,
                ..
            } => crate::i18n::t_args(
                "calendar-invitation-choice",
                &[
                    ("email", email.clone().into()),
                    ("event", event_label.clone().into()),
                    ("sender", sender_email.clone().into()),
                    ("account", sender_account.clone().into()),
                ],
            ),
        }
    }

    fn row(&self) -> LiveRow {
        let mut action = self.effect().bound();
        action.label = crate::i18n::t(match self {
            Self::Attendee { .. } => "calendar-contact-add",
            Self::Invitation { .. } => "calendar-invitation-prepare",
        });
        action.description = Some(self.label());
        LiveRow::new(self.id(), "calendar.contact-choice", self.label()).operation(action)
    }
}

impl CalendarApp {
    pub(crate) fn follow_contacts(
        &mut self,
        record: Option<ui::framework::PersonRecord<ContactDirectory>>,
    ) {
        self.contact_directory = record;
    }

    fn contact_choices(&self) -> Vec<Choice> {
        let mut choices = Vec::new();
        if let Some(event) = self.contact_event.and_then(|id| self.model.event(id))
            && let Some(record) = &self.contact_directory
        {
            let directory = record.get();
            for hint in directory.email_hints() {
                if !event.attendees.iter().any(|attendee| {
                    canonical_email_hint(&attendee.email).as_ref() == Some(&hint.email)
                }) {
                    choices.push(Choice::Attendee {
                        event: event.id.to_string(),
                        event_label: event.display_title().into(),
                        person: hint.person.as_str().into(),
                        label: hint.label,
                        email: hint.email,
                    });
                }
            }
        }
        if let Some((event, attendee)) = self.invitation_attendee
            && let Some(event) = self.model.event(event)
            && let Some(attendee) = event.attendees.iter().find(|item| item.id == attendee)
            && let Some((record, _)) = &self.person_accounts
        {
            for (id, account) in record.get().accounts.iter() {
                if account.enabled
                    && account.type_id == "email"
                    && let Some(email) = account
                        .config
                        .0
                        .get("email")
                        .and_then(serde_json::Value::as_str)
                    && canonical_email_hint(email).is_some()
                {
                    choices.push(Choice::Invitation {
                        event: event.id.to_string(),
                        event_snapshot: serde_json::to_string(
                            &crate::actions::EventData::from_event(event),
                        )
                        .unwrap_or_default(),
                        event_label: crate::i18n::t_args(
                            "calendar-invitation-event",
                            &[
                                ("title", event.display_title().into()),
                                ("start", event.start.to_string().into()),
                                ("timezone", event.timezone.clone().into()),
                            ],
                        ),
                        attendee: attendee.id.to_string(),
                        email: attendee.email.clone(),
                        sender_account: id.as_str().into(),
                        sender_email: email.into(),
                    });
                }
            }
        }
        choices.sort_by_key(Choice::id);
        choices
    }

    fn current_choice(&self, payload: &str) -> Result<Choice, String> {
        self.contact_choices()
            .into_iter()
            .find(|choice| choice.payload() == payload)
            .ok_or_else(|| crate::i18n::t("calendar-contact-stale"))
    }

    pub(crate) fn contact_operation_rows(&self) -> Vec<LiveRow> {
        let mut rows = std::collections::BTreeMap::<String, LiveRow>::new();
        for choice in self.contact_choices() {
            let scope = choice.scope();
            let row = rows.remove(&scope).unwrap_or_else(|| {
                LiveRow::new(
                    scope.clone(),
                    "calendar.contact-scope",
                    crate::i18n::t(match choice {
                        Choice::Attendee { .. } => "calendar-contact-attendees",
                        Choice::Invitation { .. } => "calendar-invitation-senders",
                    }),
                )
            });
            rows.insert(scope, row.row(choice.row()));
        }
        let mut rows: Vec<_> = rows.into_values().collect();
        rows.extend(self.model.events.iter().map(|event| {
            let mut action = CalendarEffect::ContactAttendees {
                event_id: event.id.to_string(),
            }
            .bound();
            action.label = crate::i18n::t("calendar-contact-attendees");
            LiveRow::new(
                crate::actions::event_resource_id(event.id),
                "calendar.event",
                event.display_title(),
            )
            .operation(action)
        }));
        rows
    }

    pub(crate) fn contact_operation_entries(&self) -> Vec<SearchEntry> {
        self.contact_choices()
            .into_iter()
            .map(|choice| {
                let mut entry = SearchEntry::new(choice.label(), choice.label())
                    .opening(choice.id(), &choice.effect());
                entry.within = Some(choice.scope());
                entry
            })
            .collect()
    }

    pub(crate) fn open_contact_attendees(&mut self, event: &str) -> Result<(), String> {
        let id = event
            .parse::<Id>()
            .ok()
            .filter(|id| self.model.event(*id).is_some())
            .ok_or_else(|| crate::i18n::t("calendar-contact-event-missing"))?;
        self.contact_event = Some(id);
        self.invitation_attendee = None;
        self.search_opener.open(ui::search::SearchRequest::within(
            ui::search::ScopeDecl::new(
                format!("calendar.contact-attendees:{id}"),
                crate::i18n::t("calendar-contact-attendees"),
            ),
        ));
        Ok(())
    }

    pub(crate) fn open_invitation_senders(
        &mut self,
        event: &str,
        attendee: &str,
    ) -> Result<(), String> {
        let event = event
            .parse::<Id>()
            .ok()
            .filter(|id| self.model.event(*id).is_some())
            .ok_or_else(|| crate::i18n::t("calendar-contact-event-missing"))?;
        let attendee = attendee
            .parse::<Id>()
            .ok()
            .filter(|id| {
                self.model
                    .event(event)
                    .is_some_and(|event| event.attendees.iter().any(|item| item.id == *id))
            })
            .ok_or_else(|| crate::i18n::t("calendar-contact-attendee-missing"))?;
        self.contact_event = None;
        self.invitation_attendee = Some((event, attendee));
        self.search_opener.open(ui::search::SearchRequest::within(
            ui::search::ScopeDecl::new(
                format!("calendar.invitation-senders:{event}:{attendee}"),
                crate::i18n::t("calendar-invitation-senders"),
            ),
        ));
        Ok(())
    }

    pub(crate) fn contact_attendee_action(
        &self,
        payload: &str,
    ) -> Result<Invoke<Calendar>, String> {
        let Choice::Attendee {
            event,
            label,
            email,
            ..
        } = self.current_choice(payload)?
        else {
            return Err(crate::i18n::t("calendar-contact-stale"));
        };
        let event = event
            .parse::<Id>()
            .map_err(|_| crate::i18n::t("calendar-contact-event-missing"))?;
        Ok(Calendar::INVITE.with((key(event), email, label)))
    }

    pub(crate) fn prepare_reviewed_invitation(
        &mut self,
        payload: &str,
    ) -> Result<serde_json::Value, String> {
        let Choice::Invitation {
            event,
            attendee,
            sender_account,
            sender_email,
            ..
        } = self.current_choice(payload)?
        else {
            return Err(crate::i18n::t("calendar-contact-stale"));
        };
        let mut handoff = self.prepare_in_mail(&event, &attendee)?;
        let arguments = handoff
            .get_mut("arguments")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| crate::i18n::t("calendar-contact-stale"))?;
        arguments.insert("sender_account".into(), sender_account.into());
        arguments.insert("sender_email".into(), sender_email.into());
        Ok(handoff)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use accounts::person::PersonAccounts;
    use ui::framework::{ResourceApp, ViewCx, testing::TestApp};
    use ui::tilo_resource::Id as ResourceId;
    use ui::widget::{Element, Text};

    struct ContactsOwner;
    impl ResourceApp for ContactsOwner {
        type Resource = ContactDirectory;
        const ID: &'static str = "so.tilo.contacts";
        const TITLE: &'static str = "Contacts fixture";
        fn view(_: &mut ViewCx<ContactDirectory>) -> Element {
            Box::new(Text::new("Fixture"))
        }
    }
    struct AccountsOwner;
    impl ResourceApp for AccountsOwner {
        type Resource = PersonAccounts;
        const ID: &'static str = "so.tilo.settings";
        const TITLE: &'static str = "Accounts fixture";
        fn view(_: &mut ViewCx<PersonAccounts>) -> Element {
            Box::new(Text::new("Fixture"))
        }
    }

    pub(crate) fn attach_senders(
        app: &mut TestApp<crate::CalendarShell>,
    ) -> ui::framework::PersonRecord<PersonAccounts> {
        let mut owner = TestApp::<AccountsOwner>::new().unwrap();
        let source = owner.shared_person::<PersonAccounts>().unwrap();
        for id in ["sender-work", "sender-personal"] {
            source
                .invoke(
                    &PersonAccounts::SAVE_ACCOUNT.with((
                        id.into(),
                        "email".into(),
                        id.into(),
                        serde_json::to_value(mail::model::AccountConfig {
                            email: "sender@example.test".into(),
                            ..Default::default()
                        })
                        .unwrap(),
                        true,
                        0,
                    )),
                )
                .unwrap();
        }
        let read = app.shared_person::<PersonAccounts>().unwrap();
        assert!(!read.editable());
        read.record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        app.surface_mut::<CalendarApp>()
            .follow_person_accounts(read);
        app.settle();
        source
    }

    pub(crate) fn selected_sender(
        app: &mut TestApp<crate::CalendarShell>,
    ) -> (String, CalendarEffect) {
        let choice = app.surface_mut::<CalendarApp>().contact_choices().into_iter().find(|choice| {
            matches!(choice, Choice::Invitation { sender_account, .. } if sender_account == "sender-personal")
        }).unwrap();
        (choice.id(), choice.effect())
    }

    fn fixture() -> (
        tempfile::TempDir,
        ui::framework::PersonRecord<ContactDirectory>,
        TestApp<crate::CalendarShell>,
        Id,
    ) {
        let mut owner = TestApp::<ContactsOwner>::new().unwrap();
        let source = owner.shared_person::<ContactDirectory>().unwrap();
        for suffix in ["001", "002"] {
            let id = ResourceId::from_raw(format!("00000000-0000-4000-8000-000000000{suffix}"));
            source
                .invoke(&ContactDirectory::PROJECT_CONTACT.with((id.clone(), "Alex".into())))
                .unwrap();
            source
                .invoke(
                    &ContactDirectory::PROJECT_EMAILS.with((id, vec!["Alex@EXAMPLE.test".into()])),
                )
                .unwrap();
        }
        let profile = tempfile::tempdir().unwrap();
        let mut app = TestApp::<crate::CalendarShell>::open_data_dir(profile.path()).unwrap();
        let read = app.shared_person::<ContactDirectory>().unwrap();
        assert!(!read.editable());
        read.record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        app.surface_mut::<CalendarApp>().follow_contacts(Some(read));
        let event = crate::model::seed_demo(chrono::NaiveDate::from_ymd_opt(2026, 10, 6).unwrap())
            .events
            .remove(0);
        let mut data = crate::actions::EventData::from_event(&event);
        data.attendees.clear();
        data.project.clear();
        data.calendar_id.clear();
        app.invoke(Calendar::NEW_EVENT.with(data)).unwrap();
        app.settle();
        app.effect_on(
            &crate::actions::event_resource_id(event.id),
            &CalendarEffect::ContactAttendees {
                event_id: event.id.to_string(),
            },
        )
        .unwrap();
        (profile, source, app, event.id)
    }

    #[test]
    fn contact_attendee_review_preserves_ambiguity_and_keeps_private_ids_out_of_shared_event() {
        let (_profile, source, mut app, event) = fixture();
        let choices = app.surface_mut::<CalendarApp>().contact_choices();
        assert_eq!(
            choices.len(),
            2,
            "a unique address does not decide which same-name person was intended"
        );
        let selected = choices[1].clone();
        assert!(app.agent_surface().to_string().contains(&selected.id()));
        assert!(app.effect_on(&selected.id(), &selected.effect()).is_err());
        assert!(
            app.effect_confirmed(&selected.effect()).is_err(),
            "root invocation cannot replace exact row review"
        );
        assert!(app.state().event(event).unwrap().attendees.is_empty());
        app.set_role(ui::permissions::Role::Viewer);
        assert!(
            app.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err(),
            "contact selection grants no event edit permission"
        );
        app.set_role(ui::permissions::Role::Owner);
        app.effect_on_confirmed(&selected.id(), &selected.effect())
            .unwrap();
        app.settle();
        let state = app.state();
        let attendees = &state.event(event).unwrap().attendees;
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].email, "Alex@example.test");
        assert_eq!(attendees[0].name, "Alex");
        let Choice::Attendee { person, .. } = selected else {
            panic!("contact choice")
        };
        assert_ne!(attendees[0].id.to_string(), person);
        let projected = serde_json::to_string(&crate::actions::EventData::from_event(
            state.event(event).unwrap(),
        ))
        .unwrap();
        assert!(
            !projected.contains(&person),
            "the address-book UUID is not a shared event field"
        );
        assert!(
            app.surface_mut::<CalendarApp>()
                .contact_choices()
                .is_empty(),
            "duplicate address cannot add another attendee"
        );
        let consumer = app.shared_person::<ContactDirectory>().unwrap();
        assert!(!consumer.editable());
        assert!(
            consumer
                .invoke(
                    &ContactDirectory::PROJECT_CONTACT
                        .with((ResourceId::from_raw(person), "Changed".into()))
                )
                .is_err()
        );
        assert!(
            source
                .get()
                .people
                .iter()
                .all(|(_, person)| person.endpoints.is_empty()),
            "adding an attendee establishes no identity proof"
        );
        assert!(app.state().uploads.is_empty());
        assert!(app.state().deletes.is_empty());
        drop(consumer);
        let attendee_id = attendees[0].id;
        let mut reopened = app.reopen().unwrap();
        assert_eq!(
            reopened.state().event(event).unwrap().attendees[0].id,
            attendee_id
        );
        assert_eq!(
            reopened.state().event(event).unwrap().attendees[0].email,
            "Alex@example.test"
        );
        let read = reopened.shared_person::<ContactDirectory>().unwrap();
        assert!(!read.editable());
        assert_eq!(read.get().email_candidates("Alex@example.test").len(), 2);
    }

    #[test]
    fn contact_attendee_stale_hint_and_deleted_person_refuse_without_document_mutation() {
        let (_profile, source, mut app, _) = fixture();
        let selected = app.surface_mut::<CalendarApp>().contact_choices()[0].clone();
        let Choice::Attendee { person, .. } = &selected else {
            panic!("contact choice")
        };
        source
            .invoke(&ContactDirectory::PROJECT_EMAILS.with((
                ResourceId::from_raw(person.clone()),
                vec!["reassigned@example.test".into()],
            )))
            .unwrap();
        app.shared_person::<ContactDirectory>()
            .unwrap()
            .record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        app.settle();
        assert!(
            app.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err()
        );
        let next = app
            .surface_mut::<CalendarApp>()
            .contact_choices()
            .into_iter()
            .find(|choice| matches!(choice, Choice::Attendee { person: id, .. } if id == person))
            .unwrap();
        source
            .invoke(&ContactDirectory::REMOVE_CONTACT.with(ResourceId::from_raw(person.clone())))
            .unwrap();
        app.shared_person::<ContactDirectory>()
            .unwrap()
            .record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        app.settle();
        assert!(app.effect_on_confirmed(&next.id(), &next.effect()).is_err());
        assert!(
            app.state()
                .events
                .iter()
                .all(|event| event.attendees.is_empty())
        );
    }

    #[test]
    fn invitation_sender_review_keeps_same_email_accounts_distinct_and_refuses_stale_sources() {
        let (_profile, _, mut app, event) = fixture();
        let choice = app.surface_mut::<CalendarApp>().contact_choices()[0].clone();
        app.effect_on_confirmed(&choice.id(), &choice.effect())
            .unwrap();
        app.settle();
        let attendee = app.state().event(event).unwrap().attendees[0].id;
        let source = attach_senders(&mut app);
        app.effect(&CalendarEffect::PrepareInMail {
            event_id: event.to_string(),
            attendee_id: attendee.to_string(),
        })
        .unwrap();
        assert_eq!(app.surface_mut::<CalendarApp>().contact_choices().len(), 2);
        let (row, effect) = selected_sender(&mut app);
        assert!(app.effect_on(&row, &effect).is_err());
        let output = app
            .effect_on_confirmed(&row, &effect)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(output["arguments"]["sender_account"], "sender-personal");
        assert_eq!(output["arguments"]["sender_email"], "sender@example.test");
        assert_eq!(output["delivery"], "not_sent");
        let before = crate::actions::EventData::from_event(app.state().event(event).unwrap());
        assert!(
            !serde_json::to_string(&before)
                .unwrap()
                .contains("sender-personal")
        );
        source
            .invoke(&PersonAccounts::REMOVE_ACCOUNT.with(ResourceId::from_raw("sender-personal")))
            .unwrap();
        app.shared_person::<PersonAccounts>()
            .unwrap()
            .record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        app.settle();
        assert!(app.effect_on_confirmed(&row, &effect).is_err());
        assert_eq!(
            crate::actions::EventData::from_event(app.state().event(event).unwrap()),
            before
        );
    }
    #[test]
    fn calendar_invitation_handoff_prepares_pinned_mail_draft_without_sending() {
        let (_profile, _, mut calendar, event) = fixture();
        let choice = calendar.surface_mut::<CalendarApp>().contact_choices()[0].clone();
        calendar
            .effect_on_confirmed(&choice.id(), &choice.effect())
            .unwrap();
        calendar.settle();
        let attendee = calendar.state().event(event).unwrap().attendees[0].id;
        let source = attach_senders(&mut calendar);
        calendar
            .effect(&CalendarEffect::PrepareInMail {
                event_id: event.to_string(),
                attendee_id: attendee.to_string(),
            })
            .unwrap();
        let (row, effect) = selected_sender(&mut calendar);
        let handoff = calendar
            .effect_on_confirmed(&row, &effect)
            .unwrap()
            .value
            .unwrap();
        let mut mailbox = TestApp::<mail::MailShell>::new().unwrap();
        mailbox
            .shared_person::<PersonAccounts>()
            .unwrap()
            .record()
            .merge_peer_document(source.record().document().unwrap())
            .unwrap();
        mailbox.settle();
        let specs = mail::actions::MailEffect::specs();
        let spec = specs
            .iter()
            .find(|spec| Some(spec.id.as_str()) == handoff["target"]["action"].as_str())
            .unwrap();
        let args = handoff["arguments"].as_object().unwrap();
        let encoded = spec.build_json(args).unwrap();
        let effect = mail::actions::MailEffect::from_payload(&encoded.payload).unwrap();
        assert!(
            mailbox.effect(&effect).is_err(),
            "Mail also requires review before storing the invitation"
        );
        mailbox.effect_confirmed(&effect).unwrap();
        mailbox.settle();
        let draft = mailbox.state().draft.draft();
        assert_eq!(draft.to, "Alex@example.test");
        assert_eq!(draft.sender.unwrap().account, "sender-personal");
        assert_eq!(
            draft.calendar_ics.as_deref(),
            handoff["arguments"]["calendar_ics"].as_str()
        );
        assert!(mailbox.state().outbox.is_empty());
        assert!(
            mailbox.effect_confirmed(&effect).is_err(),
            "an existing invitation draft cannot be overwritten"
        );
    }
    #[test]
    fn contact_attendee_picker_reaches_native_scoped_search_in_both_themes() {
        for theme in [ui::Theme::dark(), ui::Theme::light()] {
            let (_profile, _, mut app, _) = fixture();
            app.harness().set_theme(theme);
            app.settle();
            let inspection = app.harness().inspection_json().unwrap();
            assert!(
                inspection.contains("\"test_id\": \"action-palette\""),
                "{inspection}"
            );
            assert!(
                inspection.contains("Only Add attendee from Contacts"),
                "{inspection}"
            );
        }
    }

    #[test]
    fn contact_attendee_changed_event_context_and_attendee_refuse_old_rows() {
        let (_profile, _, mut app, event) = fixture();
        let selected = app.surface_mut::<CalendarApp>().contact_choices()[0].clone();
        let mut data = crate::actions::EventData::from_event(app.state().event(event).unwrap());
        data.id = uuid::Uuid::new_v4().to_string();
        let other = data.id.clone();
        app.invoke(Calendar::NEW_EVENT.with(data)).unwrap();
        app.settle();
        app.effect(&CalendarEffect::ContactAttendees { event_id: other })
            .unwrap();
        assert!(
            app.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err()
        );
        app.effect(&CalendarEffect::ContactAttendees {
            event_id: event.to_string(),
        })
        .unwrap();
        app.effect_on_confirmed(&selected.id(), &selected.effect())
            .unwrap();
        app.settle();
        let attendee = app.state().event(event).unwrap().attendees[0].id;
        let _source = attach_senders(&mut app);
        app.effect(&CalendarEffect::PrepareInMail {
            event_id: event.to_string(),
            attendee_id: attendee.to_string(),
        })
        .unwrap();
        let (row, effect) = selected_sender(&mut app);
        app.invoke(
            Calendar::paths()
                .events
                .item(&key(event))
                .title
                .set("Changed event".into())
                .into(),
        )
        .unwrap();
        app.settle();
        assert!(
            app.effect_on_confirmed(&row, &effect).is_err(),
            "an invitation review covers the event details too"
        );
        let (row, effect) = selected_sender(&mut app);
        app.invoke(
            Calendar::paths()
                .events
                .item(&key(event))
                .attendees
                .item(&key(attendee))
                .email
                .set("changed@example.test".into())
                .into(),
        )
        .unwrap();
        app.settle();
        assert!(app.effect_on_confirmed(&row, &effect).is_err());
    }
}

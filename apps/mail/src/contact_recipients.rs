//! Shared address-book choices in the system resource/search picker. Every row
//! is an exact person/address/sender choice, reviewed through the action door.
//! Email hints are deliberately independent of confirmed messaging identities.

use tilo_contact_directory::{ContactDirectory, canonical_email_hint};
use ui::framework::LiveRow;
use ui::tilo_resource::{Effect, Id, SearchEntry};

use super::MailApp;
use crate::actions::MailEffect;
use crate::model::{Draft, DraftSender};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Choice {
    person: String,
    label: String,
    email: String,
    sender_account: String,
    sender_email: String,
}

impl Choice {
    fn id(&self) -> String {
        // Include reviewed facts, not a name-derived identity or a hash. A
        // changed contact/account produces a different row and invalidates
        // an outstanding action rather than silently rebinding it.
        serde_json::to_vec(self).map_or_else(
            |_| String::new(),
            |bytes| format!("mail.contact-draft:{}", hex::encode(bytes)),
        )
    }

    fn effect(&self) -> MailEffect {
        MailEffect::PrepareContactDraft {
            person: self.person.clone(),
            label: self.label.clone(),
            email: self.email.clone(),
            sender_account: self.sender_account.clone(),
            sender_email: self.sender_email.clone(),
        }
    }

    fn label(&self) -> String {
        crate::i18n::t_args(
            "mail-contact-draft-choice",
            &[
                ("person", self.label.clone().into()),
                ("id", self.person.clone().into()),
                ("email", self.email.clone().into()),
                ("sender", self.sender_email.clone().into()),
                ("sender_account", self.sender_account.clone().into()),
            ],
        )
    }

    fn row(&self) -> LiveRow {
        let mut action = self.effect().bound();
        action.label = crate::i18n::t("mail-contact-prepare");
        action.description = Some(self.label());
        LiveRow::new(self.id(), "mail.contact-draft", self.label()).operation(action)
    }
}

fn choices(directory: &ContactDirectory, accounts: &[crate::account::Account]) -> Vec<Choice> {
    let mut choices = Vec::new();
    for (id, person) in directory
        .people
        .iter()
        .filter(|(_, person)| !person.removed)
    {
        let mut emails: Vec<_> = person
            .emails
            .0
            .iter()
            .filter_map(|email| canonical_email_hint(email))
            .collect();
        emails.sort();
        emails.dedup();
        for email in emails {
            for account in accounts {
                if canonical_email_hint(&account.config.email).is_some() {
                    choices.push(Choice {
                        person: id.as_str().into(),
                        label: person.label.clone(),
                        email: email.clone(),
                        sender_account: account.id.clone(),
                        sender_email: account.config.email.clone(),
                    });
                }
            }
        }
    }
    choices.sort_by_key(Choice::id);
    choices
}

impl MailApp {
    fn contact_choices(&self) -> Vec<Choice> {
        self.contact_directory
            .as_ref()
            .map_or_else(Vec::new, |record| {
                record.with(|directory| choices(directory, &self.shared.borrow().accounts.accounts))
            })
    }

    pub(crate) fn contact_recipient_rows(&self) -> Vec<LiveRow> {
        let mut collection = LiveRow::new(
            "mail.contacts",
            "mail.contacts",
            crate::i18n::t("mail-contact-recipients"),
        );
        for choice in self.contact_choices() {
            collection = collection.row(choice.row());
        }
        vec![collection]
    }

    pub(crate) fn contact_recipient_row(&self, id: &str) -> Option<LiveRow> {
        if id == "mail.contacts" {
            return self.contact_recipient_rows().pop();
        }
        self.contact_choices()
            .into_iter()
            .find(|choice| choice.id() == id)
            .map(|choice| choice.row())
    }

    pub(crate) fn contact_recipient_search_entries(&self) -> Vec<SearchEntry> {
        self.contact_choices()
            .into_iter()
            .map(|choice| {
                let mut entry = SearchEntry::new(choice.label(), choice.email.clone())
                    .opening(choice.id(), &choice.effect());
                entry.within = Some("mail.contacts".into());
                entry
            })
            .collect()
    }

    pub(crate) fn open_contact_recipients(&mut self) {
        self.search.open(ui::search::SearchRequest::within(
            ui::search::ScopeDecl::new("mail.contacts", crate::i18n::t("mail-contact-recipients")),
        ));
    }

    pub(crate) fn prepare_contact_draft(
        &self,
        person: &str,
        label: &str,
        email: &str,
        sender_account: &str,
        sender_email: &str,
    ) -> Result<Draft, String> {
        if self.compose.open || self.has_contact_draft_conflict || !self.reveals.is_empty() {
            return Err(crate::i18n::t("mail-contact-existing-draft"));
        }
        let directory = self
            .contact_directory
            .as_ref()
            .ok_or_else(|| crate::i18n::t("mail-contact-unavailable"))?
            .get();
        let person = directory
            .people
            .get(&Id::from_raw(person))
            .filter(|person| !person.removed)
            .ok_or_else(|| crate::i18n::t("mail-contact-person-unavailable"))?;
        if person.label != label
            || canonical_email_hint(email).as_deref() != Some(email)
            || !person
                .emails
                .0
                .iter()
                .any(|hint| canonical_email_hint(hint).as_deref() == Some(email))
        {
            return Err(crate::i18n::t("mail-contact-recipient-changed"));
        }
        let index = self
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .position(|account| {
                account.id == sender_account
                    && account.config.email == sender_email
                    && canonical_email_hint(sender_email).is_some()
            })
            .ok_or_else(|| crate::i18n::t("mail-contact-account-changed"))?;
        self.shared.borrow_mut().accounts.set_active(index);
        Ok(Draft {
            to: email.into(),
            sender: Some(DraftSender {
                account: sender_account.into(),
                email: sender_email.into(),
            }),
            ..Draft::default()
        })
    }

    pub(crate) fn contact_sender_matches(&self, draft: &Draft) -> bool {
        draft.sender.as_ref().is_none_or(|sender| {
            let state = self.shared.borrow();
            state
                .accounts
                .accounts
                .get(state.accounts.active)
                .is_some_and(|account| {
                    account.id == sender.account && account.config.email == sender.email
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::{ResourceApp, ViewCx, testing::TestApp};
    use ui::tilo_resource::Invoke;
    use ui::widget::{Element, Text};

    // An isolated Contacts owner exercises the actual person-record boundary;
    // Mail consumes this same record without acquiring editor authority.
    struct DirectoryOwner;
    impl ResourceApp for DirectoryOwner {
        type Resource = ContactDirectory;
        const ID: &'static str = "so.tilo.contacts";
        const TITLE: &'static str = "Contacts fixture";
        fn view(_: &mut ViewCx<ContactDirectory>) -> Element {
            Box::new(Text::new("Contacts fixture"))
        }
    }

    fn fixture() -> (TestApp<DirectoryOwner>, TestApp<crate::MailShell>) {
        let mut owner = TestApp::<DirectoryOwner>::new().unwrap();
        let directory = owner.shared_person::<ContactDirectory>().unwrap();
        for id in [
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        ] {
            let person = Id::from_raw(id);
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
        let mut mail = TestApp::<crate::MailShell>::new().unwrap();
        let app = mail.surface_mut::<MailApp>();
        app.contact_directory = Some(directory);
        app.shared.borrow_mut().accounts.accounts = ["work@example.test", "personal@example.test"]
            .into_iter()
            .map(|email| {
                crate::account::Account::new(crate::model::AccountConfig {
                    email: email.into(),
                    ..crate::model::AccountConfig::default()
                })
            })
            .collect();
        mail.settle();
        (owner, mail)
    }

    #[test]
    fn contact_picker_reads_replicated_persisted_contacts_without_editor_grant() {
        use tilo_automerge::ApplyIncoming as _;
        let profile = tempfile::tempdir().unwrap();
        let mut owner = TestApp::<DirectoryOwner>::open_data_dir(profile.path()).unwrap();
        let directory = owner.shared_person::<ContactDirectory>().unwrap();
        let person = Id::from_raw("00000000-0000-4000-8000-000000000003");
        directory
            .invoke(&ContactDirectory::PROJECT_CONTACT.with((person.clone(), "Morgan".into())))
            .unwrap();
        directory
            .invoke(
                &ContactDirectory::PROJECT_EMAILS
                    .with((person.clone(), vec!["Morgan@example.test".into()])),
            )
            .unwrap();
        let mut incoming = directory.record().read(Clone::clone).unwrap();
        let consumer_profile = tempfile::tempdir().unwrap();
        let mut mail = TestApp::<crate::MailShell>::open_data_dir(consumer_profile.path()).unwrap();
        let consumer = mail.shared_person::<ContactDirectory>().unwrap();
        assert!(!consumer.editable());
        // Simulate the host's incoming replica transfer, outside app actions.
        // An app profile is app-specific; the owner and consumer never share
        // a primary mailbox or bypass its binding by reusing a data directory.
        consumer
            .record()
            .edit(false, |doc| {
                doc.merge_incoming(&mut incoming)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        mail.flush().unwrap();
        drop(consumer);
        drop(mail);
        let mut mail = TestApp::<crate::MailShell>::open_data_dir(consumer_profile.path()).unwrap();
        let directory = mail.shared_person::<ContactDirectory>().unwrap();
        assert!(!directory.editable());
        assert_eq!(
            directory.get().email_candidates("Morgan@example.test"),
            vec![person.clone()]
        );
        assert_eq!(directory.get().people.get(&person).unwrap().label, "Morgan");
        assert_eq!(
            mail.surface_mut::<MailApp>()
                .contact_directory
                .as_ref()
                .unwrap()
                .get()
                .email_candidates("Morgan@example.test"),
            vec![person]
        );
    }

    #[test]
    fn contact_picker_requires_confirmation_and_preserves_person_and_sender_choice() {
        let (mut owner, mut mail) = fixture();
        let choices = mail.surface_mut::<MailApp>().contact_choices();
        assert_eq!(
            choices.len(),
            4,
            "both same-name people and both sender accounts remain choices"
        );
        let selected = choices
            .iter()
            .find(|choice| {
                choice.person.ends_with('2') && choice.sender_email == "personal@example.test"
            })
            .unwrap()
            .clone();
        let surface = mail.agent_surface().to_string();
        assert!(
            surface.contains(&selected.id()),
            "selected recipient is published through the real action surface"
        );
        assert!(
            mail.effect_on(&selected.id(), &selected.effect()).is_err(),
            "selection requires review"
        );
        assert!(mail.read_document(|doc| crate::resource::TestMail::load_draft(doc).to.is_empty()));
        assert!(
            mail.effect_confirmed(&selected.effect()).is_err(),
            "row choices cannot be invoked as an unbound root action"
        );
        let output = mail
            .effect_on_confirmed(&selected.id(), &selected.effect())
            .unwrap();
        assert_eq!(
            output
                .value
                .as_ref()
                .and_then(|value| value.get("person"))
                .and_then(serde_json::Value::as_str),
            Some(selected.person.as_str())
        );
        mail.settle();
        let draft = mail.read_document(crate::resource::TestMail::load_draft);
        assert_eq!(draft.to, "Alex@example.test");
        assert_eq!(
            draft.sender.as_ref().unwrap().account,
            "personal@example.test"
        );
        assert!(
            mail.read_document(|doc| crate::resource::TestMail::outbox(doc).is_empty()),
            "preparing a draft never sends mail"
        );
        assert!(
            owner
                .shared_person::<ContactDirectory>()
                .unwrap()
                .get()
                .people
                .iter()
                .all(|(_, person)| person.endpoints.is_empty()),
            "selection never confirms an identity"
        );
        assert!(
            mail.shared_person::<ContactDirectory>()
                .unwrap()
                .invoke(
                    &ContactDirectory::PROJECT_CONTACT
                        .with((Id::from_raw(selected.person), "Changed".into()))
                )
                .is_err(),
            "Mail has no directory edit grant"
        );
        assert!(
            mail.effect_on_confirmed(&choices[0].id(), &choices[0].effect())
                .is_err(),
            "an existing composition is never overwritten"
        );
        assert_eq!(
            mail.read_document(crate::resource::TestMail::load_draft),
            draft
        );
        assert_sender_guard(&mut mail, &draft);
    }

    fn assert_sender_guard(mail: &mut TestApp<crate::MailShell>, draft: &Draft) {
        mail.surface_mut::<MailApp>()
            .shared
            .borrow_mut()
            .accounts
            .set_active(0);
        assert!(!mail.surface_mut::<MailApp>().contact_sender_matches(draft));
        let mut delivery_doc = mail.peer_document();
        mail.surface_mut::<MailApp>()
            .send_compose(&mut ui::widget::Edits::lend(&mut delivery_doc));
        assert!(
            crate::resource::TestMail::outbox(&delivery_doc).is_empty(),
            "the production send path refuses a changed sender"
        );
        assert_eq!(
            mail.surface_mut::<MailApp>()
                .shared
                .borrow()
                .status
                .as_deref(),
            Some(crate::i18n::t("mail-contact-sender-changed").as_str())
        );
        mail.surface_mut::<MailApp>()
            .shared
            .borrow_mut()
            .accounts
            .set_active(1);
        assert!(mail.surface_mut::<MailApp>().contact_sender_matches(draft));
    }

    #[test]
    fn contact_picker_drafts_folder_reopen_preserves_sender_and_single_projection() {
        let (_, mut mail) = fixture();
        let choice = mail
            .surface_mut::<MailApp>()
            .contact_choices()
            .into_iter()
            .find(|choice| choice.sender_email == "work@example.test")
            .unwrap();
        mail.effect_on_confirmed(&choice.id(), &choice.effect())
            .unwrap();
        mail.settle();
        mail.settle();
        let mut doc = mail.peer_document();
        let app = mail.surface_mut::<MailApp>();
        assert_eq!(
            app.shared
                .borrow()
                .mailbox
                .count(crate::model::Folder::Drafts),
            1
        );
        app.save_compose_draft(&mut ui::widget::Edits::lend(&mut doc));
        let id = app.editing_draft.unwrap();
        app.open_draft(id);
        let draft = app.compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(
            draft.sender.as_ref().unwrap().account,
            choice.sender_account
        );
        assert_eq!(
            app.shared
                .borrow()
                .mailbox
                .count(crate::model::Folder::Drafts),
            1
        );
        app.shared.borrow_mut().accounts.set_active(1);
        assert!(!app.contact_sender_matches(&draft));
    }

    #[test]
    fn contact_picker_delivery_uses_exact_sender_when_accounts_share_an_email() {
        let (_, mut mail) = fixture();
        let app = mail.surface_mut::<MailApp>();
        app.shared.borrow_mut().accounts.accounts = ["first-service", "second-service"]
            .into_iter()
            .map(|id| {
                let mut account = crate::account::Account::new(crate::model::AccountConfig {
                    email: "same@example.test".into(),
                    username: id.into(),
                    ..crate::model::AccountConfig::default()
                });
                account.id = id.into();
                account
            })
            .collect();
        let choices = app.contact_choices();
        assert_eq!(choices.len(), 4);
        let selected = choices
            .iter()
            .find(|choice| choice.sender_account == "second-service")
            .unwrap();
        assert!(selected.label().contains("second-service"));
        let draft = Draft {
            to: selected.email.clone(),
            sender: Some(DraftSender {
                account: selected.sender_account.clone(),
                email: selected.sender_email.clone(),
            }),
            ..Draft::default()
        };
        let outgoing = crate::actions::Outgoing::new("same@example.test", &draft);
        let (tx, rx) = std::sync::mpsc::channel();
        let (_incoming, incoming_rx) = std::sync::mpsc::channel();
        app.engine = Some(super::super::EngineHandle {
            tx,
            rx: incoming_rx,
        });
        app.deliver(outgoing.clone(), Box::new(|_| {}));
        let crate::engine::EngineRequest::Send { account, .. } = rx.try_recv().unwrap() else {
            panic!("expected the production send request");
        };
        assert_eq!(
            account.username, "second-service",
            "the actual transport receives the reviewed source account"
        );
        app.shared
            .borrow_mut()
            .accounts
            .accounts
            .retain(|account| account.id != "second-service");
        let refusal = std::rc::Rc::new(std::cell::RefCell::new(None));
        let observed = refusal.clone();
        app.deliver(
            outgoing,
            Box::new(move |result| *observed.borrow_mut() = Some(result)),
        );
        assert!(refusal.borrow().as_ref().unwrap().is_err());
        assert!(
            rx.try_recv().is_err(),
            "removing the reviewed sender cannot fall back to another account with the same address"
        );
    }

    #[test]
    fn contact_picker_refuses_stale_address_deleted_person_and_replaced_sender() {
        let (mut owner, mut mail) = fixture();
        let selected = mail.surface_mut::<MailApp>().contact_choices()[0].clone();
        let record = owner.shared_person::<ContactDirectory>().unwrap();
        record
            .invoke(&ContactDirectory::PROJECT_EMAILS.with((
                Id::from_raw(selected.person.clone()),
                vec!["new@example.test".into()],
            )))
            .unwrap();
        assert!(
            mail.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err()
        );
        let selected = mail.surface_mut::<MailApp>().contact_choices()[0].clone();
        record
            .invoke(&ContactDirectory::REMOVE_CONTACT.with(Id::from_raw(selected.person.clone())))
            .unwrap();
        assert!(
            mail.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err()
        );
        let selected = mail.surface_mut::<MailApp>().contact_choices()[0].clone();
        let app = mail.surface_mut::<MailApp>();
        let mut state = app.shared.borrow_mut();
        let sender = state
            .accounts
            .accounts
            .iter_mut()
            .find(|account| account.id == selected.sender_account)
            .unwrap();
        sender.config.email = "replacement@example.test".into();
        drop(state);
        assert!(
            mail.effect_on_confirmed(&selected.id(), &selected.effect())
                .is_err()
        );
        assert!(mail.read_document(|doc| crate::resource::TestMail::load_draft(doc).to.is_empty()));
        assert!(mail.read_document(|doc| crate::resource::TestMail::outbox(doc).is_empty()));
    }

    #[test]
    fn contact_picker_saved_draft_blocks_preparation_and_sender_survives_compose_save() {
        let (_, mut mail) = fixture();
        let choice = mail.surface_mut::<MailApp>().contact_choices()[0].clone();
        let existing = Draft {
            bcc: "existing@example.test".into(),
            ..Draft::default()
        };
        let before = mail.state().draft;
        for edit in crate::resource::Mail::paths()
            .draft
            .path()
            .edits(&before, &before.saved(&existing))
        {
            mail.invoke(Invoke::edit(&edit).unwrap()).unwrap();
        }
        assert!(
            mail.effect_on_confirmed(&choice.id(), &choice.effect())
                .is_err()
        );
        assert_eq!(
            mail.read_document(crate::resource::TestMail::load_draft)
                .bcc,
            existing.bcc
        );
        let mut compose = crate::compose::ComposeWindow::new(crate::model::ComposeFormat::Text);
        let draft = Draft {
            to: choice.email,
            sender: Some(DraftSender {
                account: choice.sender_account,
                email: choice.sender_email,
            }),
            ..Draft::default()
        };
        compose.open_with_draft(&draft);
        let mut doc = crate::resource::TestMail::doc();
        let saved = compose.to_draft(&mut ui::widget::Edits::lend(&mut doc));
        assert_eq!(saved.sender, draft.sender);
        crate::resource::TestMail::store_draft(&mut doc, &saved);
        assert_eq!(
            crate::resource::TestMail::load_draft(&doc).sender,
            draft.sender
        );
        compose.open_blank();
        assert!(
            compose
                .to_draft(&mut ui::widget::Edits::lend(&mut doc))
                .sender
                .is_none()
        );
        crate::resource::TestMail::clear_draft(&mut doc);
        assert!(crate::resource::TestMail::load_draft(&doc).sender.is_none());
    }
}

#[cfg(test)]
mod chrome_tests {
    use ui::framework::testing::TestApp;

    #[test]
    fn contact_picker_header_uses_native_chrome_in_both_themes() {
        for theme in [ui::Theme::dark(), ui::Theme::light()] {
            let mut mail = TestApp::<crate::MailShell>::capture(None).unwrap();
            mail.harness().set_theme(theme);
            mail.harness().resize((1280, 800)).unwrap();
            mail.harness().relayout();
            assert!(mail.harness().a11y_nodes().iter().any(
                |(_, node)| node.label() == Some(crate::i18n::t("mail-contact-write").as_str())
            ));
            mail.harness().click_named("mail-list-action-3").unwrap();
            mail.settle();
            let inspection = mail.harness().inspection_json().unwrap();
            assert!(
                inspection.contains("\"test_id\": \"action-palette\""),
                "{inspection}"
            );
            assert!(
                inspection.contains("Only Contacts"),
                "the native picker must retain its contact scope: {inspection}"
            );
        }
    }
}

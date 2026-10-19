//! Account editing through the shared directory, with review before mutation.
//! The address-book surface remains mounted so navigation never discards drafts.

use std::rc::Rc;

use tilo_contact_directory::ContactDirectory;
use ui::LocalState;
use ui::framework::{Actions, Dispatcher, Effected, LiveRow, PersonRecord, ViewCx};
use ui::tilo_resource::{self, Effect, Id};
use ui::widget::{
    Button, Column, Dropdown, Element, ModelView, Property, PropertyList, Row, Scrollable, Text,
    TextInput,
};
use uuid::Uuid;

use crate::resource::Contacts;

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US"]);

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "contacts.accounts")]
pub enum AccountEffect {
    /// Manage a contact's account associations.
    #[effect(view, idempotent)]
    OpenAccounts { person: String },
    /// Return to the address book.
    #[effect(view, idempotent)]
    CloseAccounts,
    /// Select the contact whose accounts are being edited.
    #[effect(view, idempotent)]
    SelectAccountPerson { value: String },
    /// Review a proposed account association before confirming it.
    #[effect(view)]
    ReviewAccount {
        person: String,
        service: String,
        recipient: String,
        scope: String,
        revision: String,
    },
    /// Edit an existing account association without changing it yet.
    #[effect(view)]
    EditAccount { person: String, association: String },
    /// Review revocation of an existing account association.
    #[effect(view)]
    ReviewAccountRevocation { person: String, association: String },
    /// Confirm the exact pending account review.
    #[effect(row, destructive)]
    ConfirmAccountReview { token: String },
    /// Cancel the pending review without changing any account association.
    #[effect(view, idempotent)]
    CancelAccountReview,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    token: String,
    person: String,
    service: String,
    recipient: String,
    scope: String,
    revision: String,
    revoke: Option<String>,
    previous: Option<Previous>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Previous {
    id: String,
    service: String,
    recipient: String,
    scope: String,
    revision: String,
}

impl Previous {
    fn from_endpoint(id: &Id, endpoint: &tilo_contact_directory::Endpoint) -> Self {
        Self {
            id: id.as_str().to_owned(),
            service: endpoint.service.clone(),
            recipient: endpoint.recipient.clone(),
            scope: endpoint.scope.clone(),
            revision: endpoint.revision.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Selection {
    open: bool,
    person: String,
    pending: Option<Pending>,
    editing: Option<Previous>,
}

#[derive(Clone)]
pub(crate) struct Editor {
    directory: Option<PersonRecord<ContactDirectory>>,
    selection: LocalState<Selection>,
    pub(crate) visible: LocalState<bool>,
}

impl Editor {
    pub(crate) fn sync_visibility(&self) {
        let mut selected = self.selection.get();
        if selected.open != self.visible.get() {
            selected.open = self.visible.get();
            selected.pending = None;
            selected.editing = None;
            self.selection.set(selected);
        }
    }

    fn current(&self) -> Result<Rc<ContactDirectory>, String> {
        self.directory
            .as_ref()
            .map(PersonRecord::get)
            .ok_or_else(|| CATALOG.t("contacts-accounts-unavailable"))
    }

    fn answer(&self, effect: AccountEffect) -> Result<Effected, String> {
        let mut selected = self.selection.get();
        match effect {
            AccountEffect::OpenAccounts { person }
            | AccountEffect::SelectAccountPerson { value: person } => {
                if !person.is_empty() {
                    self.person(&person)?;
                }
                selected = Selection {
                    open: true,
                    person,
                    pending: None,
                    editing: None,
                };
            }
            AccountEffect::CloseAccounts => {
                selected.open = false;
                selected.pending = None;
            }
            AccountEffect::CancelAccountReview => {
                selected.pending = None;
                selected.editing = None;
            }
            AccountEffect::ReviewAccount {
                person,
                service,
                recipient,
                scope,
                revision,
            } => {
                selected.pending =
                    Some(self.propose(&selected, person, service, recipient, scope, revision)?);
            }
            AccountEffect::EditAccount {
                person,
                association,
            } => {
                self.person(&person)?;
                if !selected.open || selected.person != person {
                    return Err(CATALOG.t("contacts-account-stale"));
                }
                let directory = self.current()?;
                let endpoint = directory
                    .people
                    .get(&Id::from_raw(&person))
                    .and_then(|p| p.endpoints.get(&Id::from_raw(&association)))
                    .filter(|e| !e.removed)
                    .ok_or_else(|| CATALOG.t("contacts-account-stale"))?;
                selected.editing = Some(Previous::from_endpoint(
                    &Id::from_raw(association),
                    endpoint,
                ));
                selected.pending = None;
            }
            AccountEffect::ReviewAccountRevocation {
                person,
                association,
            } => {
                self.person(&person)?;
                if !selected.open || selected.person != person {
                    return Err(CATALOG.t("contacts-account-stale"));
                }
                let directory = self.current()?;
                let endpoint = directory
                    .people
                    .get(&Id::from_raw(&person))
                    .and_then(|p| p.endpoints.get(&Id::from_raw(&association)))
                    .filter(|e| !e.removed)
                    .ok_or_else(|| CATALOG.t("contacts-account-stale"))?;
                selected.pending = Some(Pending {
                    token: Uuid::new_v4().to_string(),
                    person,
                    service: endpoint.service.clone(),
                    recipient: endpoint.recipient.clone(),
                    scope: endpoint.scope.clone(),
                    revision: endpoint.revision.clone(),
                    revoke: Some(association),
                    previous: None,
                });
            }
            AccountEffect::ConfirmAccountReview { token } => {
                self.confirm(&selected, &token)?;
                selected.pending = None;
                selected.editing = None;
            }
        }
        self.visible.set(selected.open);
        self.selection.set(selected);
        Ok(Effected::done())
    }

    fn propose(
        &self,
        selected: &Selection,
        person: String,
        service: String,
        recipient: String,
        scope: String,
        revision: String,
    ) -> Result<Pending, String> {
        self.person(&person)?;
        if !selected.open
            || selected.person != person
            || !["tilo", "whatsapp", "signal", "sms"].contains(&service.as_str())
            || recipient.trim().is_empty()
            || recipient.len() > 512
            || recipient
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
            || scope.is_empty() != revision.is_empty()
            || [&scope, &revision]
                .iter()
                .any(|s| s.len() > 512 || s.chars().any(char::is_control))
        {
            return Err(CATALOG.t("contacts-account-invalid"));
        }
        let directory = self.current()?;
        let previous = directory.people.get(&Id::from_raw(&person)).and_then(|p| {
            p.endpoints.iter().find(|(_, e)| {
                !e.removed
                    && e.service == service
                    && e.scope == scope
                    && e.recipient == recipient
                    && e.revision != revision
            })
        });
        Ok(Pending {
            token: Uuid::new_v4().to_string(),
            person,
            service,
            recipient,
            scope,
            revision,
            revoke: None,
            previous: selected
                .editing
                .clone()
                .or_else(|| previous.map(|(id, e)| Previous::from_endpoint(id, e))),
        })
    }

    fn confirm(&self, selected: &Selection, token: &str) -> Result<(), String> {
        let pending = selected
            .pending
            .as_ref()
            .filter(|p| p.token == token && p.person == selected.person)
            .ok_or_else(|| CATALOG.t("contacts-account-stale"))?;
        self.person(&pending.person)?;
        let record = self
            .directory
            .as_ref()
            .ok_or_else(|| CATALOG.t("contacts-accounts-unavailable"))?;
        let person = Id::from_raw(&pending.person);
        let result = if let Some(association) = &pending.revoke {
            let directory = record.get();
            let endpoint = directory
                .people
                .get(&person)
                .and_then(|p| p.endpoints.get(&Id::from_raw(association)))
                .filter(|e| {
                    !e.removed
                        && e.service == pending.service
                        && e.recipient == pending.recipient
                        && e.scope == pending.scope
                        && e.revision == pending.revision
                })
                .ok_or_else(|| CATALOG.t("contacts-account-stale"))?;
            debug_assert!(!endpoint.removed);
            record.invoke(
                &ContactDirectory::FORGET_ENDPOINT.with((person, Id::from_raw(association))),
            )
        } else if let Some(previous) = &pending.previous {
            let directory = record.get();
            let endpoint = directory
                .people
                .get(&person)
                .and_then(|p| p.endpoints.get(&Id::from_raw(&previous.id)))
                .filter(|e| {
                    !e.removed
                        && e.service == previous.service
                        && e.scope == previous.scope
                        && e.recipient == previous.recipient
                        && e.revision == previous.revision
                })
                .ok_or_else(|| CATALOG.t("contacts-account-stale"))?;
            debug_assert!(!endpoint.removed);
            if pending.scope.is_empty() {
                record.invoke(&ContactDirectory::REPLACE_ENDPOINT.with((
                    person,
                    Id::from_raw(&previous.id),
                    Id::from_raw(&pending.token),
                    pending.service.clone(),
                    pending.recipient.clone(),
                )))
            } else {
                record.invoke(&ContactDirectory::REPLACE_ACCOUNT.with((
                    person,
                    Id::from_raw(&previous.id),
                    Id::from_raw(&pending.token),
                    pending.service.clone(),
                    pending.scope.clone(),
                    pending.recipient.clone(),
                    pending.revision.clone(),
                )))
            }
        } else if pending.scope.is_empty() {
            record.invoke(&ContactDirectory::CONFIRM_ENDPOINT.with((
                person,
                Id::from_raw(&pending.token),
                pending.service.clone(),
                pending.recipient.clone(),
            )))
        } else {
            record.invoke(&ContactDirectory::CONFIRM_ACCOUNT.with((
                person,
                Id::from_raw(&pending.token),
                pending.service.clone(),
                pending.scope.clone(),
                pending.recipient.clone(),
                pending.revision.clone(),
            )))
        };
        result.map_err(|_| CATALOG.t("contacts-account-conflict"))?;
        Ok(())
    }

    fn person(&self, person: &str) -> Result<(), String> {
        let directory = self.current()?;
        directory
            .people
            .get(&Id::from_raw(person))
            .filter(|p| !p.removed)
            .map(|_| ())
            .ok_or_else(|| CATALOG.t("contacts-account-stale"))
    }
}

pub(crate) fn build(cx: &ViewCx<Contacts>) -> Element {
    let editor = Editor {
        directory: if cx.mode().is_live() {
            cx.shared_person::<ContactDirectory>()
        } else {
            None
        },
        selection: LocalState::new(Selection::default()),
        visible: LocalState::new(false),
    };
    let answer = editor.clone();
    cx.effects(move |effect: AccountEffect, _| answer.answer(effect));

    let dispatcher = cx.dispatcher();
    let observed = editor.clone();
    let detail_dispatcher = dispatcher;
    let actions = cx.actions();
    let pane = ModelView::from_document(
        move |_, _| {
            (
                observed
                    .directory
                    .as_ref()
                    .map_or(0, PersonRecord::revision),
                observed.selection.get(),
            )
        },
        {
            let editor = editor.clone();
            move |(_, selection)| panel(&editor, &detail_dispatcher, &actions, selection)
        },
    );
    let app = crate::app::ContactsApp::new().with_accounts(editor, Box::new(pane));
    // A picked search hit (or an agent's `open`) shows its contact.
    cx.on_open_item(app.opener());
    cx.surface(app)
}

pub(crate) fn review_rows(editor: &Editor) -> Vec<LiveRow> {
    editor.selection.get().pending.map_or_else(Vec::new, |p| {
        vec![
            LiveRow::new(
                format!("contacts.account-review:{}", p.token),
                "contacts.account-review",
                CATALOG.t_args(
                    "contacts-account-review-description",
                    &[
                        (
                            "action",
                            CATALOG
                                .t(if p.revoke.is_some() {
                                    "contacts-account-remove"
                                } else {
                                    "contacts-account-review"
                                })
                                .into(),
                        ),
                        ("person", p.person.clone().into()),
                        ("service", p.service.clone().into()),
                        ("recipient", p.recipient.clone().into()),
                        ("scope", p.scope.clone().into()),
                        ("revision", p.revision.clone().into()),
                        (
                            "previous",
                            p.previous
                                .as_ref()
                                .map_or("", |old| old.revision.as_str())
                                .into(),
                        ),
                    ],
                ),
            )
            .effect(&AccountEffect::ConfirmAccountReview { token: p.token }),
        ]
    })
}

fn person_label(directory: &ContactDirectory, id: &Id, label: &str) -> String {
    if directory
        .people
        .iter()
        .filter(|(_, p)| !p.removed && p.label == label)
        .count()
        > 1
    {
        let suffix = id
            .as_str()
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!("{label} ({suffix})")
    } else {
        label.to_owned()
    }
}

fn panel(
    editor: &Editor,
    dispatcher: &Dispatcher<Contacts>,
    actions: &Actions<Contacts>,
    selected: &Selection,
) -> Element {
    let content = panel_contents(editor, dispatcher, actions, selected);
    if selected.open {
        Box::new(Scrollable::new_vertical().with_child(content))
    } else {
        content
    }
}

fn panel_contents(
    editor: &Editor,
    dispatcher: &Dispatcher<Contacts>,
    actions: &Actions<Contacts>,
    selected: &Selection,
) -> Element {
    let mut body = Column::new().with_intrinsic_height();
    if !selected.open {
        return Box::new(body.with_no_padding());
    }
    let back = dispatcher.clone();
    body = body.push(Box::new(
        Button::new("contacts.accounts.back", "contacts-account-close").on_view(move || {
            back.effect(&AccountEffect::CloseAccounts);
        }),
    ));
    let Ok(directory) = editor.current() else {
        return Box::new(body.push(Box::new(Text::new("contacts-accounts-unavailable"))));
    };
    let value = LocalState::new(selected.person.clone());
    let mut people = Dropdown::from_state(Vec::new(), &value)
        .with_id("contacts.accounts.person")
        .with_accessible_name(CATALOG.t("contacts-account-person"))
        .with_placeholder(&CATALOG.t("contacts-account-person"));
    people.set_value_options(
        directory
            .people
            .iter()
            .filter(|(_, p)| !p.removed)
            .map(|(id, p)| {
                (
                    id.as_str().to_owned(),
                    person_label(&directory, id, &p.label),
                )
            })
            .collect(),
    );
    let people = actions.bind_effect(people, AccountEffect::SELECT_ACCOUNT_PERSON);
    body = body.push(Box::new(people));
    let Some(person) = directory
        .people
        .get(&Id::from_raw(&selected.person))
        .filter(|p| !p.removed)
    else {
        return Box::new(body);
    };
    body = body.push(endpoint_list(person, &directory, selected, dispatcher));
    if let Some(pending) = &selected.pending {
        return Box::new(body.push(review(pending, dispatcher)));
    }
    let service = LocalState::new(
        selected
            .editing
            .as_ref()
            .map_or_else(|| "tilo".to_owned(), |old| old.service.clone()),
    );
    let recipient = LocalState::new(
        selected
            .editing
            .as_ref()
            .map_or_else(String::new, |old| old.recipient.clone()),
    );
    let mut services = Dropdown::from_state(Vec::new(), &service)
        .with_id("contacts.accounts.service")
        .with_accessible_name(CATALOG.t("contacts-account-service"));
    services.set_value_options(
        [
            ("tilo", "Tilo"),
            ("whatsapp", "WhatsApp"),
            ("signal", "Signal"),
            ("sms", "SMS"),
        ]
        .into_iter()
        .map(|(id, label)| (id.into(), label.into()))
        .collect(),
    );
    let door = dispatcher.clone();
    let person = selected.person.clone();
    let typed_recipient = recipient.clone();
    let add = Button::new("contacts.accounts.add", "contacts-account-add").on_view(move || {
        door.effect(&AccountEffect::ReviewAccount {
            person: person.clone(),
            service: service.get(),
            recipient: typed_recipient.get(),
            scope: String::new(),
            revision: String::new(),
        });
    });
    let recipient_input = TextInput::from_state(&recipient)
        .with_id("contacts.accounts.recipient")
        .with_accessible_name(CATALOG.t("contacts-account-recipient"));
    Box::new(
        body.push(Box::new(Text::new("contacts-account-hint")))
            .push(Box::new(services))
            .push(Box::new(recipient_input))
            .push(Box::new(add)),
    )
}

fn endpoint_list(
    person: &tilo_contact_directory::Person,
    directory: &ContactDirectory,
    selected: &Selection,
    dispatcher: &Dispatcher<Contacts>,
) -> Element {
    let mut body = Column::new().with_intrinsic_height().with_no_padding();
    for (id, endpoint) in person.endpoints.iter().filter(|(_, e)| !e.removed) {
        let status = if endpoint.scope.is_empty() {
            "contacts-account-unverified"
        } else if directory
            .resolve_account(
                &endpoint.service,
                &endpoint.scope,
                &endpoint.recipient,
                &endpoint.revision,
            )
            .as_ref()
            == Ok(&Id::from_raw(&selected.person))
        {
            "contacts-account-linked"
        } else {
            "contacts-account-conflicted"
        };
        let door = dispatcher.clone();
        let effect = AccountEffect::ReviewAccountRevocation {
            person: selected.person.clone(),
            association: id.as_str().to_owned(),
        };
        let edit_door = dispatcher.clone();
        let edit = AccountEffect::EditAccount {
            person: selected.person.clone(),
            association: id.as_str().to_owned(),
        };
        body = body.push(Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(Box::new(PropertyList::new(account_properties(
                    &endpoint.service,
                    &endpoint.recipient,
                ))))
                .push(Box::new(Text::new(status)))
                .push(Box::new(
                    Column::new()
                        .with_intrinsic_height()
                        .with_no_padding()
                        .push(Box::new(
                            Button::new(
                                &format!("contacts.accounts.edit:{}", id.as_str()),
                                "contacts-account-edit",
                            )
                            .on_view(move || {
                                edit_door.effect(&edit);
                            }),
                        ))
                        .push(Box::new(
                            Button::new(
                                &format!("contacts.accounts.revoke:{}", id.as_str()),
                                "contacts-account-remove",
                            )
                            .on_view(move || {
                                door.effect(&effect);
                            }),
                        )),
                )),
        ));
    }
    Box::new(body)
}

fn account_properties(service: &str, recipient: &str) -> Vec<Property> {
    vec![
        Property::new(CATALOG.t("contacts-account-service"), service),
        Property::new(CATALOG.t("contacts-account-recipient"), recipient)
            .with_kind(ui::text_render::TextKind::Identifier),
    ]
}

fn review(pending: &Pending, dispatcher: &Dispatcher<Contacts>) -> Element {
    let confirm = dispatcher.clone();
    let token = pending.token.clone();
    let cancel = dispatcher.clone();
    let mut properties = account_properties(&pending.service, &pending.recipient);
    if !pending.scope.is_empty() {
        properties.push(Property::new(
            CATALOG.t("contacts-account-scope"),
            pending.scope.clone(),
        ));
    }
    if let Some(previous) = &pending.previous {
        properties.push(Property::new(
            CATALOG.t("contacts-account-old-reference"),
            previous.revision.clone(),
        ));
    }
    if !pending.revision.is_empty() {
        properties.push(Property::new(
            CATALOG.t("contacts-account-reference"),
            pending.revision.clone(),
        ));
    }
    let mut body = Column::new()
        .with_intrinsic_height()
        .push(Box::new(Text::new("contacts-account-review")))
        .push(Box::new(PropertyList::new(properties)))
        .push(Box::new(Text::new(if pending.revoke.is_some() {
            "contacts-account-revoke-hint"
        } else {
            "contacts-account-confirm-hint"
        })));
    if pending.previous.is_some() && pending.revision.is_empty() {
        body = body.push(Box::new(Text::new("contacts-account-edited-unverified")));
    }
    Box::new(
        body.push(Box::new(
            Row::new()
                .push(Box::new(
                    Button::new("contacts.accounts.cancel", "contacts-account-cancel").on_view(
                        move || {
                            cancel.effect(&AccountEffect::CancelAccountReview);
                        },
                    ),
                ))
                .push(Box::new(
                    Button::new("contacts.accounts.confirm", "contacts-account-confirm").on_view(
                        move || {
                            confirm.effect_on(
                                &format!("contacts.account-review:{token}"),
                                &AccountEffect::ConfirmAccountReview {
                                    token: token.clone(),
                                },
                            );
                        },
                    ),
                )),
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ContactsApp;
    use crate::model::Contact;
    use crate::resource::ContactsShell;
    use tilo_contact_directory::routing::{SenderAccount, routes};
    use ui::framework::ResourceSurface;
    use ui::framework::testing::TestApp;

    fn fixture() -> (TestApp<ContactsShell>, String, String) {
        let mut app =
            TestApp::<ContactsShell>::for_person("contacts-account-editor-fixture").unwrap();
        let mut one = Contact::new();
        one.first = "Alex".into();
        let mut two = Contact::new();
        two.first = "Alex".into();
        for person in [&one, &two] {
            app.invoke(crate::resource::Contacts::add(person)).unwrap();
        }
        app.settle();
        (app, one.id.to_string(), two.id.to_string())
    }

    fn pending(app: &mut TestApp<ContactsShell>) -> Pending {
        app.with_surface::<ContactsApp, _>(|surface| {
            surface
                .account_editor
                .as_ref()
                .unwrap()
                .selection
                .get()
                .pending
                .unwrap()
        })
        .unwrap()
    }

    fn confirm(app: &mut TestApp<ContactsShell>) -> Pending {
        let p = pending(app);
        app.settle();
        let row = format!("contacts.account-review:{}", p.token);
        let published = app
            .with_surface::<ContactsApp, _>(|surface| {
                surface.live_row(&row).unwrap().into_resource()
            })
            .unwrap();
        assert!(published.label.contains(&p.person));
        assert!(published.label.contains(&p.service));
        assert!(published.label.contains(&p.recipient));
        assert!(published.label.contains(&p.scope));
        assert!(published.label.contains(&p.revision));
        if let Some(previous) = &p.previous {
            assert!(published.label.contains(&previous.revision));
        }
        let action = AccountEffect::ConfirmAccountReview {
            token: p.token.clone(),
        };
        assert!(
            app.effect_on(&row, &action).is_err(),
            "confirmation is required"
        );
        app.effect_on_confirmed(&row, &action).unwrap();
        p
    }

    fn review(app: &mut TestApp<ContactsShell>, person: &str, revision: &str) {
        app.effect(&AccountEffect::OpenAccounts {
            person: person.into(),
        })
        .unwrap();
        app.effect(&AccountEffect::ReviewAccount {
            person: person.into(),
            service: "signal".into(),
            recipient: "aci:alice".into(),
            scope: "signal-main".into(),
            revision: revision.into(),
        })
        .unwrap();
    }

    #[test]
    fn account_editor_add_review_cancel_revoke_and_revision_journey() {
        let (mut app, one, two) = fixture();
        let directory = app.shared_person::<ContactDirectory>().unwrap();
        review(&mut app, &one, "identity-1");
        assert!(
            directory
                .get()
                .resolve_account("signal", "signal-main", "aci:alice", "identity-1")
                .is_err()
        );
        let canceled = pending(&mut app);
        app.effect(&AccountEffect::CancelAccountReview).unwrap();
        assert!(
            app.effect_on_confirmed(
                &format!("contacts.account-review:{}", canceled.token),
                &AccountEffect::ConfirmAccountReview {
                    token: canceled.token
                }
            )
            .is_err()
        );
        review(&mut app, &one, "identity-1");
        let original = confirm(&mut app);
        review(&mut app, &one, "identity-1b");
        let replacement = confirm(&mut app);
        assert!(
            directory
                .get()
                .people
                .get(&Id::from_raw(&one))
                .unwrap()
                .endpoints
                .get(&Id::from_raw(&original.token))
                .unwrap()
                .removed
        );
        assert_eq!(
            directory
                .get()
                .resolve_account("signal", "signal-main", "aci:alice", "identity-1b"),
            Ok(Id::from_raw(&one))
        );
        review(&mut app, &two, "identity-1b");
        let conflict = pending(&mut app);
        assert!(
            app.effect_on_confirmed(
                &format!("contacts.account-review:{}", conflict.token),
                &AccountEffect::ConfirmAccountReview {
                    token: conflict.token
                }
            )
            .is_err()
        );
        assert_eq!(
            directory
                .get()
                .resolve_account("signal", "signal-main", "aci:alice", "identity-1b"),
            Ok(Id::from_raw(&one))
        );
        app.effect(&AccountEffect::OpenAccounts {
            person: one.clone(),
        })
        .unwrap();
        app.effect(&AccountEffect::ReviewAccountRevocation {
            person: one.clone(),
            association: replacement.token,
        })
        .unwrap();
        confirm(&mut app);
        assert!(
            directory
                .get()
                .people
                .get(&Id::from_raw(&one))
                .unwrap()
                .endpoints
                .get(&Id::from_raw(&original.token))
                .unwrap()
                .removed
        );
        review(&mut app, &two, "identity-2");
        confirm(&mut app);
        assert_eq!(
            directory
                .get()
                .resolve_account("signal", "signal-main", "aci:alice", "identity-2"),
            Ok(Id::from_raw(&two))
        );
        assert!(
            directory
                .get()
                .resolve_account("signal", "signal-main", "aci:alice", "identity-1")
                .is_err()
        );
    }

    #[test]
    fn closing_the_shared_inspector_invalidates_its_pending_review() {
        let (mut app, one, _) = fixture();
        app.harness().resize((1180, 820)).unwrap();
        review(&mut app, &one, "pending-inspector-reference");
        let staged = pending(&mut app);
        assert!(
            app.with_widget::<ui::widget::NavShell, _>(|shell| shell.is_inspector_open())
                .unwrap()
        );
        app.harness().click_named("nav-shell-inspector").unwrap();
        app.settle();
        assert!(
            !app.with_widget::<ui::widget::NavShell, _>(|shell| shell.is_inspector_open())
                .unwrap()
        );
        assert!(
            app.with_surface::<ContactsApp, _>(|surface| surface
                .account_editor
                .as_ref()
                .unwrap()
                .selection
                .get()
                .pending
                .is_none())
                .unwrap()
        );
        assert!(
            app.effect_on_confirmed(
                &format!("contacts.account-review:{}", staged.token),
                &AccountEffect::ConfirmAccountReview {
                    token: staged.token
                }
            )
            .is_err()
        );
        assert!(
            app.shared_person::<ContactDirectory>()
                .unwrap()
                .get()
                .resolve_account(
                    "signal",
                    "signal-main",
                    "aci:alice",
                    "pending-inspector-reference"
                )
                .is_err()
        );
    }

    #[test]
    fn editing_an_address_preserves_history_and_clears_old_identity_evidence() {
        let (mut app, one, _) = fixture();
        review(&mut app, &one, "identity-before-edit");
        let original = confirm(&mut app);
        app.effect(&AccountEffect::EditAccount {
            person: one.clone(),
            association: original.token.clone(),
        })
        .unwrap();
        app.effect(&AccountEffect::ReviewAccount {
            person: one.clone(),
            service: "signal".into(),
            recipient: "changed-subject".into(),
            scope: String::new(),
            revision: String::new(),
        })
        .unwrap();
        let directory = app.shared_person::<ContactDirectory>().unwrap();
        assert!(
            !directory
                .get()
                .people
                .get(&Id::from_raw(&one))
                .unwrap()
                .endpoints
                .get(&Id::from_raw(&original.token))
                .unwrap()
                .removed
        );
        let updated = confirm(&mut app);
        let current = directory.get();
        let endpoints = &current.people.get(&Id::from_raw(&one)).unwrap().endpoints;
        assert!(
            endpoints
                .get(&Id::from_raw(original.token))
                .unwrap()
                .removed
        );
        let new = endpoints.get(&Id::from_raw(updated.token)).unwrap();
        assert_eq!(new.recipient, "changed-subject");
        assert!(new.scope.is_empty() && new.revision.is_empty());
        assert!(
            routes(
                &current,
                &Id::from_raw(one),
                "signal",
                &[SenderAccount {
                    id: Id::from_raw("sender"),
                    service: "signal".into(),
                    scope: "signal-scope".into(),
                    enabled: true
                }]
            )
            .is_empty()
        );
    }

    #[test]
    fn review_selection_and_sender_accounts_cannot_be_silently_substituted() {
        let (mut app, one, two) = fixture();
        review(&mut app, &one, "identity-1");
        let old = pending(&mut app);
        app.effect(&AccountEffect::SelectAccountPerson { value: two })
            .unwrap();
        assert!(
            app.effect_on_confirmed(
                &format!("contacts.account-review:{}", old.token),
                &AccountEffect::ConfirmAccountReview { token: old.token }
            )
            .is_err()
        );
        review(&mut app, &one, "identity-1");
        confirm(&mut app);
        let directory = app.shared_person::<ContactDirectory>().unwrap();
        let senders = ["personal", "work"].map(|id| SenderAccount {
            id: Id::from_raw(id),
            service: "signal".into(),
            scope: "signal-main".into(),
            enabled: true,
        });
        let available = routes(&directory.get(), &Id::from_raw(&one), "signal", &senders);
        assert_eq!(available.len(), 2);
    }

    #[test]
    fn account_editor_controls_remain_visible_in_narrow_and_multiple_themes() {
        for (width, theme) in [
            (420, ui::theme::Theme::dark()),
            (1180, ui::theme::Theme::light()),
            (420, ui::theme::Theme::high_contrast()),
        ] {
            let (mut app, one, _) = fixture();
            app.harness().set_theme(theme);
            app.harness().resize((width, 820)).unwrap();
            app.effect(&AccountEffect::OpenAccounts {
                person: one.clone(),
            })
            .unwrap();
            assert!(
                app.with_widget::<ui::widget::NavShell, _>(|shell| shell.is_inspector_open())
                    .unwrap()
            );
            // The inspector slides in; land it so its controls are measured
            // where they come to rest, without racing a wall clock.
            app.with_widget::<ui::widget::NavShell, _>(ui::widget::NavShell::snap)
                .expect("the contacts shell is hosted");
            app.harness().relayout();
            let nodes = app.harness().a11y_nodes();
            for label in [
                "Contact",
                "Account address or identifier",
                "Review association",
                "Close accounts",
            ] {
                let node = nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(label))
                    .unwrap_or_else(|| panic!("missing {label}"));
                let rect = node.1.bounds().unwrap();
                assert!(
                    rect.x0 >= 0.0
                        && rect.x1 <= f64::from(width)
                        && rect.y0 >= 0.0
                        && rect.y1 <= 820.0,
                    "{label}: {rect:?}"
                );
            }
            review(&mut app, &one, "identity-narrow-fixture");
            confirm(&mut app);
            for label in ["Edit association", "Remove association"] {
                let control = app
                    .harness()
                    .controls()
                    .into_iter()
                    .find(|control| control.name.as_deref() == Some(label))
                    .unwrap();
                let rect = control.bounds.unwrap();
                assert!(
                    rect.x0 >= 0.0 && rect.x1 <= f64::from(width),
                    "{label}: {rect:?}"
                );
            }
            app.effect(&AccountEffect::CloseAccounts).unwrap();
            assert!(
                app.with_surface::<ContactsApp, _>(|_| ()).is_some(),
                "address-book surface remains mounted"
            );
        }
    }
}

#[cfg(test)]
mod input_journey {
    use super::*;
    use crate::app::ContactsApp;
    use crate::model::Contact;
    use crate::resource::ContactsShell;
    use ui::framework::testing::TestApp;

    #[test]
    fn typing_and_reviewing_an_account_uses_shared_controls_without_mutating_early() {
        let mut app = TestApp::<ContactsShell>::for_person("contacts-input-fixture").unwrap();
        let mut contact = Contact::new();
        contact.first = "Sam".into();
        app.invoke(crate::resource::Contacts::add(&contact))
            .unwrap();
        app.settle();
        let open = AccountEffect::OpenAccounts {
            person: contact.id.to_string(),
        };
        app.effect(&open).unwrap();
        app.settle();
        assert!(
            app.with_widget::<ui::widget::NavShell, _>(|shell| shell.is_inspector_open())
                .unwrap()
        );
        // The inspector slides in; land it so its controls are measured
        // where they come to rest, without racing a wall clock.
        app.with_widget::<ui::widget::NavShell, _>(ui::widget::NavShell::snap)
            .expect("the contacts shell is hosted");
        app.harness().relayout();
        app.harness()
            .click_named("contacts.accounts.recipient")
            .unwrap();
        app.harness().type_text("stable-user-id");
        let input = app
            .harness()
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some("Account address or identifier"))
            .unwrap();
        assert_eq!(input.value.as_deref(), Some("stable-user-id"), "{input:?}");
        app.harness().click_named("contacts.accounts.add").unwrap();
        app.settle();
        let pending = app
            .with_surface::<ContactsApp, _>(|surface| {
                surface
                    .account_editor
                    .as_ref()
                    .unwrap()
                    .selection
                    .get()
                    .pending
                    .unwrap()
            })
            .unwrap();
        assert_eq!(pending.person, contact.id.to_string());
        assert_eq!(pending.recipient, "stable-user-id");
        let directory = app.shared_person::<ContactDirectory>().unwrap();
        assert!(directory.get().resolve("tilo", "stable-user-id").is_err());
        let row = format!("contacts.account-review:{}", pending.token);
        app.effect_on_confirmed(
            &row,
            &AccountEffect::ConfirmAccountReview {
                token: pending.token,
            },
        )
        .unwrap();
        assert_eq!(
            directory.get().resolve("tilo", "stable-user-id"),
            Ok(Id::from_raw(contact.id.to_string()))
        );
    }
}

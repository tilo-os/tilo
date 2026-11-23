// The account-association panel: which service account of the workspace is which contact.
//
// The workspace's contact directory is a root of this app's package that tilolang publishes as a
// platform root (`contacts.directory`, its type `so.tilo.contacts#Identities`): `crates/session/
// session` publishes the record's reading and answers its verbs as the person sitting here, so
// Mail, Calendar, Messages and this panel change the same people.
use so.tilo.contacts.directory as contacts

/// The account-association panel of a contact: what the workspace's directory holds for the
/// person this card is, each account as the directory answered it, and the controls that review,
/// change and forget one.
///
/// A name or a number that reads alike is not evidence, and the panel says so: only a
/// confirmation the person made binds a service account to this contact.
view associations(c: Contact) {
    let d = contacts.directory
    state changing: Text = ""
    stack(gap: .related) {
        if let person = for_card(d, c) {
            if person.endpoints.is_empty() {
                text(
                    "No account of the workspace is this contact yet.",
                    role: .caption,
                    secondary: true,
                )
            }
            for endpoint in person.endpoints {
                if not endpoint.removed {
                    stack(gap: .tight) {
                        association(
                            endpoint,
                            on_change: set(changing, endpoint.id),
                            on_forget: forget(d, person, endpoint),
                        )
                        if changing == endpoint.id {
                            change_form(d, person, endpoint, on_cancel: set(changing, ""))
                        }
                    }
                }
            }
            if changing == "" {
                review(d, person)
            }
            row(gap: .related, align: .center) {
                ui.Button("Share this card with the apps", icon: .refresh, role: .plain) {
                    on press: share(d, person, c)
                }
                ui.Button("Stop sharing this card", icon: .trash, role: .plain) {
                    on press: unshare(d, person)
                }
            }
        } else {
            text(
                "The workspace's apps do not know this contact yet. Sharing the card lets them name it; an account is associated by reviewing it.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// One account the directory holds for the person: what it is, whether it was confirmed, and the
/// controls the caller answers.
view association(
    endpoint: contacts.Endpoint,
    on_change: Option<Action> = .none,
    on_forget: Option<Action> = .none,
) {
    ui.Card {
        row(gap: .related, align: .center) {
            ui.Icon(.link, size: .small, secondary: true)
            ui.Text(endpoint.service, role: .label)
            ui.Text(endpoint.recipient, role: .body)
            spacer()
            if not endpoint.revision.is_empty() {
                ui.Badge("Associated", tone: .success)
            } else {
                ui.Badge("Needs review", tone: .warning)
            }
        }
        if not endpoint.scope.is_empty() {
            text("Service account {endpoint.scope}", role: .caption, secondary: true)
        }
        if let change = on_change {
            row(gap: .related, align: .center) {
                ui.Button("Change", icon: .pencil, role: .plain) {
                    on press: change
                }
                if let remove = on_forget {
                    ui.Button("Remove association", icon: .trash, role: .destructive) {
                        on press: remove
                    }
                }
            }
        }
    }
}

/// The form that associates an account with the person: the service, its account scope where the
/// service names one, and the account itself.
view review(d: contacts.Identities, person: contacts.Person) {
    state typed_service: Text = ""
    state typed_scope: Text = ""
    state typed_account: Text = ""
    stack(gap: .tight) {
        row(gap: .tight, align: .center) {
            ui.TextInput(bind: typed_service, placeholder: "Service, such as Messages", fill)
            ui.TextInput(
                bind: typed_scope,
                placeholder: "Service account, where the service names one",
                fill,
            )
        }
        ui.TextInput(bind: typed_account, placeholder: "Account address or identifier")
        ui.Button("Review this account", icon: .check, role: .primary) {
            on press: confirm(d, person, typed_service, typed_scope, typed_account)
        }
        consent()
    }
}

/// The form that changes what an association names, starting from what it names now; the caller
/// closes it.
view change_form(
    d: contacts.Identities,
    person: contacts.Person,
    endpoint: contacts.Endpoint,
    on_cancel: Option<Action> = .none,
) {
    state typed_service: Text = endpoint.service
    state typed_scope: Text = endpoint.scope
    state typed_account: Text = endpoint.recipient
    stack(gap: .tight) {
        row(gap: .tight, align: .center) {
            ui.TextInput(bind: typed_service, placeholder: "Service, such as Messages", fill)
            ui.TextInput(
                bind: typed_scope,
                placeholder: "Service account, where the service names one",
                fill,
            )
        }
        ui.TextInput(bind: typed_account, placeholder: "Account address or identifier")
        row(gap: .related, align: .center) {
            ui.Button("Keep this account", icon: .check) {
                on press: change(d, person, endpoint, typed_service, typed_scope, typed_account)
            }
            if let cancel = on_cancel {
                ui.Button("Cancel", icon: .x, role: .plain) {
                    on press: cancel
                }
            }
        }
        consent()
    }
}

/// What associating an account means, under the review's controls.
view consent() {
    text(
        "Tilo does not check that the account is really theirs. Associating it shares no files and grants no permissions.",
        role: .caption,
        secondary: true,
    )
}

/// The directory's entry for the person this card is, by the numbers and mailboxes the card
/// holds: the person the workspace's apps already know, or none until this card is shared.
fn for_card(d: contacts.Identities, c: Contact) -> Option<contacts.Person> {
    d.people.find(
        p -> not p.removed
            and (p.phones.any(phone -> c.phones.any(mine -> same_number(mine.value, phone)))
                or p.emails.any(mail -> c.emails.any(mine -> same_address(mine.value, mail)))),
    )
}

/// Binds `account` of `service` to the person: the directory's `confirm-endpoint`, or
/// `confirm-account` where the service names an account scope.
action confirm(
    d: contacts.Identities,
    person: contacts.Person,
    service: Text,
    scope: Text,
    account: Text,
) (icon: .check, label: "Associate account", did: "associated an account") {
    require not service.trim().is_empty() "A service names the account's kind"
    require not account.trim().is_empty() "An account address or identifier is needed"
    let endpoint = endpoint_id(person, service, account)
    if scope.trim().is_empty() {
        contacts.confirm_endpoint(d, person.id, endpoint, service.trim(), account.trim())
    } else {
        contacts.confirm_account(
            d,
            person.id,
            endpoint,
            service.trim(),
            scope.trim(),
            account.trim(),
            revision_of(person, service, account),
        )
    }
}

/// Changes the account an association names, under the directory's rule that a changed address
/// needs review before anything is sent to it.
action change(
    d: contacts.Identities,
    person: contacts.Person,
    endpoint: contacts.Endpoint,
    service: Text,
    scope: Text,
    account: Text,
) (icon: .pencil, label: "Change association", did: "changed an association") {
    require not account.trim().is_empty() "An account address or identifier is needed"
    let other = endpoint_id(person, service, account)
    if scope.trim().is_empty() {
        contacts.replace_endpoint(d, person.id, endpoint.id, other, service.trim(), account.trim())
    } else {
        contacts.replace_account(
            d,
            person.id,
            endpoint.id,
            other,
            service.trim(),
            scope.trim(),
            account.trim(),
            revision_of(person, service, account),
        )
    }
}

/// Takes an association away: messages from that account stop reaching this contact, and what was
/// already received is unchanged.
destructive action forget(
    d: contacts.Identities,
    person: contacts.Person,
    endpoint: contacts.Endpoint,
) (icon: .trash, label: "Remove association", did: "removed an association") {
    contacts.forget_endpoint(d, person.id, endpoint.id)
}

/// Shares this card with the workspace's apps, under the person it is: the name, the numbers and
/// the mailboxes another app reads of a contact before it writes to them.
action share(d: contacts.Identities, person: contacts.Person, c: Contact) (
    icon: .refresh,
    label: "Share this card",
    did: "shared a card",
) {
    contacts.project_contact(d, person.id, c.name)
    contacts.project_phones(d, person.id, c.phones.map(p -> p.value))
    contacts.project_emails(d, person.id, c.emails.map(e -> e.value))
}

/// Takes the contact out of the directory, leaving its card in the book: what the apps read of it
/// stops naming it.
destructive action unshare(d: contacts.Identities, person: contacts.Person) (
    icon: .trash,
    label: "Stop sharing this card",
    did: "stopped sharing a card",
) {
    contacts.remove_contact(d, person.id)
}

/// The account a review names: the person, the service and the account together, so reviewing the
/// same account twice lands on the same one rather than a second.
fn endpoint_id(person: contacts.Person, service: Text, account: Text) -> Text {
    "{service.trim().lower()}:{account.trim().lower()}"
}

/// The identity reference the directory last saw for an account, or none: what a change keeps
/// until the service names a new one.
fn revision_of(person: contacts.Person, service: Text, account: Text) -> Text {
    let held = person.endpoints.find(
        e -> e.service == service.trim().lower() and e.recipient == account.trim(),
    )
    if let endpoint = held { endpoint.revision } else { "" }
}

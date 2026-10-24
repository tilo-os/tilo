//! One person-owned directory shared by Contacts and its application consumers.
//! Names are labels. Only explicit, confirmed endpoint associations resolve
//! identity; concurrent conflicting confirmations remain ambiguous and refuse
//! routing rather than choosing a winner. No credentials or histories live here.

use serde::{Deserialize, Serialize};
use tilo_resource::{
    Collection, Id, JsonSchema, Model, ModelError, Refusal, SharedRecord, resource,
};

/// How many people one projection may carry: a large imported book in one
/// change, bounded so one payload stays a reasonable size.
pub const MAX_PROJECTED: usize = 10_000;

/// How much text one person's picture may take: Contacts keeps a small
/// portrait, far below this.
pub const MAX_PHOTO_BYTES: usize = 96 * 1024;

/// One Contacts identity as Contacts projects it: its label and the phone
/// and mailbox hints it holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Projected {
    /// The Contacts UUID.
    pub person: Id,
    pub label: String,
    pub phones: Vec<String>,
    pub emails: Vec<String>,
    /// The person's picture as Contacts keeps it (a small JPEG, base64);
    /// empty for none.
    #[serde(default)]
    pub photo: String,
}

#[cfg(test)]
mod tests;

pub mod routing;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ContactDirectory {
    pub people: Collection<Person>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Person {
    /// Stable Contacts UUID, also the collection key. Never derived from name.
    pub label: String,
    pub removed: bool,
    /// International phone hints: candidate discovery only, never proof.
    pub phones: Vec<String>,
    /// Address-book mailboxes, never identity evidence or a sending grant.
    pub emails: Vec<String>,
    /// The picture Contacts shows for the person (a small JPEG, base64), for
    /// apps that show them; empty for none. Never fetched from anywhere.
    pub photo: String,
    pub endpoints: Collection<Endpoint>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Endpoint {
    pub service: String,
    pub recipient: String,
    /// Provider namespace; empty only for the compatibility endpoint API.
    pub scope: String,
    /// Provider identity-key fingerprint or assignment generation.
    pub revision: String,
    /// Retained tombstone; the association id must never be reassigned.
    pub removed: bool,
}

/// Bring the person `obj` of a directory of version 1 up to this one: their
/// phone and mail hints were one JSON string each, and are lists now.
///
/// # Errors
/// Returns why a stored list of hints does not read as one.
pub fn upgrade_person(
    doc: &mut tilo_resource::Doc,
    obj: &automerge::ObjId,
) -> Result<(), ModelError> {
    tilo_resource::evolution::retype_json::<Vec<String>>(doc, obj, "phones")?;
    tilo_resource::evolution::retype_json::<Vec<String>>(doc, obj, "emails")?;
    Ok(())
}

/// Bring a directory of version 1 up to this one ([`upgrade_person`]).
fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no contact directory migration from {from}"
        )));
    }
    for person in tilo_resource::evolution::collection_items(doc, &automerge::ROOT, "people")? {
        upgrade_person(doc, &person)?;
    }
    Ok(())
}

#[resource(
    kind = "tilo.contacts.identities",
    label = "Contact identities",
    version = 2,
    migrate = upgrade
)]
impl ContactDirectory {
    /// Update the label of a Contacts identity, without merging similar names
    /// or reviving an identity that was deleted.
    #[action(external)]
    pub fn project_contact(&mut self, person: Id, label: String) -> Result<(), Refusal> {
        validate_person(&person)?;
        if label.len() > 4096 {
            return Err(refused());
        }
        if let Some(existing) = self.people.get_mut(&person) {
            if !existing.removed {
                existing.label = label;
            }
        } else {
            self.people.push(
                person,
                Person {
                    label,
                    ..Person::default()
                },
            );
        }
        Ok(())
    }

    /// Tombstone a removed Contacts identity. Delayed projections cannot
    /// recreate it and its endpoints cannot be used for sending.
    #[action(external)]
    pub fn remove_contact(&mut self, person: Id) -> Result<(), Refusal> {
        validate_person(&person)?;
        if let Some(existing) = self.people.get_mut(&person) {
            existing.removed = true;
        } else {
            self.people.push(
                person,
                Person {
                    removed: true,
                    ..Person::default()
                },
            );
        }
        Ok(())
    }

    /// Explicitly confirm that this service recipient belongs to this person.
    /// An uncertain candidate never becomes an association merely by appearing
    /// in a contact search or having a matching name/phone number.
    // Messages sent to a wrong association reach someone else, which Undo
    // cannot recall, so the association is asked first.
    #[action(destructive, irreversible)]
    pub fn confirm_endpoint(
        &mut self,
        person: Id,
        endpoint: Id,
        service: String,
        recipient: String,
    ) -> Result<(), Refusal> {
        self.associate(
            &person,
            endpoint,
            service,
            String::new(),
            recipient,
            String::new(),
        )
    }

    /// Confirm the exact provider-scoped account and observed identity revision.
    /// This is an address-book decision, not cryptographic proof or permission.
    // Messages sent to a wrong association reach someone else, which Undo
    // cannot recall, so the association is asked first.
    #[action(destructive, irreversible)]
    pub fn confirm_account(
        &mut self,
        person: Id,
        endpoint: Id,
        service: String,
        scope: String,
        recipient: String,
        revision: String,
    ) -> Result<(), Refusal> {
        if scope.is_empty() || revision.is_empty() {
            return Err(refused());
        }
        self.associate(&person, endpoint, service, scope, recipient, revision)
    }

    /// Replace an address after review, clearing unavailable identity evidence.
    // Messages sent to a wrong association reach someone else, which Undo
    // cannot recall, so the association is asked first.
    #[action(destructive, irreversible)]
    pub fn replace_endpoint(
        &mut self,
        person: Id,
        previous: Id,
        endpoint: Id,
        service: String,
        recipient: String,
    ) -> Result<(), Refusal> {
        if endpoint == previous {
            return Err(refused());
        }
        let mut candidate = self.clone();
        candidate.forget_endpoint(person.clone(), previous)?;
        candidate.confirm_endpoint(person, endpoint, service, recipient)?;
        *self = candidate;
        Ok(())
    }

    /// Replace a reviewed association atomically, retaining the old tombstone.
    // Messages sent to a wrong association reach someone else, which Undo
    // cannot recall, so the association is asked first.
    #[action(destructive, irreversible)]
    pub fn replace_account(
        &mut self,
        person: Id,
        previous: Id,
        endpoint: Id,
        service: String,
        scope: String,
        recipient: String,
        revision: String,
    ) -> Result<(), Refusal> {
        if endpoint == previous {
            return Err(refused());
        }
        let mut candidate = self.clone();
        candidate.forget_endpoint(person.clone(), previous)?;
        candidate.confirm_account(person, endpoint, service, scope, recipient, revision)?;
        *self = candidate;
        Ok(())
    }

    /// Project phone hints from Contacts. These can suggest candidates only.
    #[action(external)]
    pub fn project_phones(&mut self, person: Id, phones: Vec<String>) -> Result<(), Refusal> {
        hints_bounded(&phones)?;
        let selected = self
            .people
            .get_mut(&person)
            .filter(|person| !person.removed)
            .ok_or_else(refused)?;
        selected.phones = phones;
        Ok(())
    }

    /// Project mailbox hints without creating any service association.
    #[action(external)]
    pub fn project_emails(&mut self, person: Id, emails: Vec<String>) -> Result<(), Refusal> {
        hints_bounded(&emails)?;
        let selected = self
            .people
            .get_mut(&person)
            .filter(|person| !person.removed)
            .ok_or_else(refused)?;
        selected.emails = emails;
        Ok(())
    }

    /// Project many Contacts identities as one change: each person's label,
    /// phone and mailbox hints as [`Self::project_contact`],
    /// [`Self::project_phones`] and [`Self::project_emails`] would, and each
    /// of `removed` tombstoned as [`Self::remove_contact`] would. A large
    /// import costs one change instead of three per person.
    #[action(external)]
    pub fn project_contacts(
        &mut self,
        people: Vec<Projected>,
        removed: Vec<Id>,
    ) -> Result<(), Refusal> {
        if people.len().saturating_add(removed.len()) > MAX_PROJECTED {
            return Err(refused());
        }
        for projected in people {
            validate_person(&projected.person)?;
            if projected.label.len() > 4096 {
                return Err(refused());
            }
            hints_bounded(&projected.phones)?;
            hints_bounded(&projected.emails)?;
            if projected.photo.len() > MAX_PHOTO_BYTES {
                return Err(refused());
            }
            match self.people.get_mut(&projected.person) {
                Some(existing) if existing.removed => {}
                Some(existing) => {
                    existing.label = projected.label;
                    existing.phones = projected.phones;
                    existing.emails = projected.emails;
                    if existing.photo != projected.photo {
                        existing.photo = projected.photo;
                    }
                }
                None => {
                    self.people.push(
                        projected.person,
                        Person {
                            label: projected.label,
                            phones: projected.phones,
                            emails: projected.emails,
                            photo: projected.photo,
                            ..Person::default()
                        },
                    );
                }
            }
        }
        for person in removed {
            self.remove_contact(person)?;
        }
        Ok(())
    }

    /// Remove a reviewed association. No implicit transfer to another person.
    #[action(destructive)]
    pub fn forget_endpoint(&mut self, person: Id, endpoint: Id) -> Result<(), Refusal> {
        let person = self
            .people
            .get_mut(&person)
            .filter(|person| !person.removed)
            .ok_or_else(refused)?;
        person
            .endpoints
            .get_mut(&endpoint)
            .ok_or_else(refused)?
            .removed = true;
        Ok(())
    }
}

/// One address-book mailbox of one person, as a picker offers it: a choice
/// for the person to review, never proof of identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailHint {
    pub person: String,
    pub label: String,
    pub email: String,
}

impl ContactDirectory {
    /// Every live person's mailboxes, each canonical and once per person.
    /// Different people with one address stay separate choices.
    pub fn email_hints(&self) -> Vec<EmailHint> {
        let mut hints = Vec::new();
        for (id, person) in self.people.iter().filter(|(_, person)| !person.removed) {
            let mut emails: Vec<_> = person
                .emails
                .iter()
                .filter_map(|email| canonical_email_hint(email))
                .collect();
            emails.sort();
            emails.dedup();
            hints.extend(emails.into_iter().map(|email| EmailHint {
                person: id.as_str().to_owned(),
                label: person.label.clone(),
                email,
            }));
        }
        hints.sort_by(|a, b| (&a.person, &a.email).cmp(&(&b.person, &b.email)));
        hints
    }

    /// Whether `hint` still stands as it was reviewed: the person is in the
    /// book under that name and still has that address. A changed or
    /// removed contact makes an earlier choice stale.
    pub fn holds(&self, hint: &EmailHint) -> bool {
        canonical_email_hint(&hint.email).as_deref() == Some(hint.email.as_str())
            && self
                .people
                .get(&Id::from_raw(hint.person.as_str()))
                .is_some_and(|person| {
                    !person.removed
                        && person.label == hint.label
                        && person
                            .emails
                            .iter()
                            .any(|email| canonical_email_hint(email).as_ref() == Some(&hint.email))
                })
    }

    /// The picture of the one person in the book with mailbox `email`, if
    /// they have one. Several people sharing the mailbox show none: a picture
    /// never guesses between them.
    pub fn photo_for_email(&self, email: &str) -> Option<&str> {
        let candidates = self.email_candidates(email);
        let [person] = candidates.as_slice() else {
            return None;
        };
        let person = self.people.get(person)?;
        (!person.removed && !person.photo.is_empty()).then_some(person.photo.as_str())
    }

    /// Every matching person remains a candidate, including a unique match.
    /// Local-part case and provider-specific aliases are never guessed.
    pub fn email_candidates(&self, email: &str) -> Vec<Id> {
        self.candidates(email, |person| &person.emails, canonical_email_hint)
    }
    fn associate(
        &mut self,
        person: &Id,
        endpoint: Id,
        service: String,
        scope: String,
        recipient: String,
        revision: String,
    ) -> Result<(), Refusal> {
        validate_endpoint(&service, &recipient)?;
        if endpoint.as_str().trim().is_empty()
            || endpoint.as_str().len() > 512
            || !valid_metadata(&scope)
            || !valid_metadata(&revision)
            || scope.is_empty() != revision.is_empty()
        {
            return Err(refused());
        }
        if self.people.iter().any(|(id, candidate)| {
            candidate.endpoints.iter().any(|(key, value)| {
                (key == &endpoint && (id != person || value.removed || candidate.removed))
                    || (!candidate.removed
                        && id != person
                        && !value.removed
                        && value.service == service
                        && value.scope == scope
                        && value.recipient == recipient)
            })
        }) {
            return Err(Refusal::from(
                "this account association already exists; review and revoke it first",
            ));
        }
        let selected = self
            .people
            .get_mut(person)
            .filter(|person| !person.removed)
            .ok_or_else(refused)?;
        if let Some(existing) = selected.endpoints.get(&endpoint) {
            if existing.removed
                || existing.service != service
                || existing.scope != scope
                || existing.recipient != recipient
                || existing.revision != revision
            {
                return Err(refused());
            }
            return Ok(());
        }
        if selected.endpoints.values().any(|value| {
            !value.removed
                && value.service == service
                && value.scope == scope
                && value.recipient == recipient
        }) {
            return Err(Refusal::from(
                "revoke the previous identity revision before confirming another",
            ));
        }
        selected.endpoints.push(
            endpoint,
            Endpoint {
                service,
                scope,
                recipient,
                revision,
                removed: false,
            },
        );
        Ok(())
    }
}

impl ContactDirectory {
    /// Resolve one exact endpoint only if all replicas currently agree on its
    /// unique person. This recheck belongs immediately before freezing a send.
    ///
    /// # Errors
    /// Refuses missing/deleted identities or conflicting confirmations.
    pub fn resolve(&self, service: &str, recipient: &str) -> Result<Id, Refusal> {
        validate_endpoint(service, recipient)?;
        let mut matches = self
            .people
            .iter()
            .filter(|(_, person)| {
                !person.removed
                    && person.endpoints.values().any(|endpoint| {
                        !endpoint.removed
                            && endpoint.scope.is_empty()
                            && endpoint.revision.is_empty()
                            && endpoint.service == service
                            && endpoint.recipient == recipient
                    })
            })
            .map(|(id, _)| id.clone());
        let person = matches.next().ok_or_else(refused)?;
        if matches.next().is_some() {
            return Err(Refusal::from(
                "conflicting contact associations require review",
            ));
        }
        Ok(person)
    }

    /// Revision-aware resolver for provider adapters. Never silently downgrades
    /// a versioned association to the compatibility endpoint API.
    ///
    /// # Errors
    /// Refuses malformed, missing, revoked, ambiguous or changed identities.
    pub fn resolve_account(
        &self,
        service: &str,
        scope: &str,
        recipient: &str,
        revision: &str,
    ) -> Result<Id, Refusal> {
        validate_endpoint(service, recipient)?;
        if scope.is_empty()
            || revision.is_empty()
            || !valid_metadata(scope)
            || !valid_metadata(revision)
        {
            return Err(refused());
        }
        let mut matching = self
            .people
            .iter()
            .filter(|(_, person)| !person.removed)
            .flat_map(|(id, person)| {
                person
                    .endpoints
                    .values()
                    .filter(move |endpoint| {
                        !endpoint.removed
                            && endpoint.service == service
                            && endpoint.scope == scope
                            && endpoint.recipient == recipient
                    })
                    .map(move |endpoint| (id, endpoint))
            });
        let (person, endpoint) = matching.next().ok_or_else(refused)?;
        if endpoint.revision != revision
            || matching.any(|(id, endpoint)| id != person || endpoint.revision != revision)
        {
            return Err(Refusal::from(
                "account identity changed or associations conflict; confirm again",
            ));
        }
        Ok(person.clone())
    }

    /// Phone matches always remain candidates, including a unique match.
    pub fn phone_candidates(&self, phone: &str) -> Vec<Id> {
        self.candidates(phone, |person| &person.phones, canonical_phone)
    }

    /// Every live person with a value of `values` that canonicalizes like
    /// `wanted`, in id order.
    fn candidates(
        &self,
        wanted: &str,
        values: impl Fn(&Person) -> &Vec<String>,
        canonical: fn(&str) -> Option<String>,
    ) -> Vec<Id> {
        let Some(wanted) = canonical(wanted) else {
            return Vec::new();
        };
        let mut candidates = self
            .people
            .iter()
            .filter(|(_, person)| {
                !person.removed
                    && values(person)
                        .iter()
                        .any(|value| canonical(value).as_ref() == Some(&wanted))
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        candidates
    }
}

impl SharedRecord for ContactDirectory {
    const OWNER: &'static str = "so.tilo.contacts";
    const EDITORS: &'static [&'static str] = &["so.tilo.messages"];
}

fn validate_person(person: &Id) -> Result<(), Refusal> {
    let id = uuid::Uuid::parse_str(person.as_str()).map_err(|_| refused())?;
    if id.is_nil() {
        return Err(refused());
    }
    Ok(())
}

/// Conservative ASCII dot-atom mailbox key for address-book suggestions.
/// This is not deliverability validation. Quoted/EAI mailboxes stay available
/// through consumers' manual entry; their equivalence is not guessed here.
/// RFC 5321 §2.4 preserves local-part case and folds only the DNS domain.
pub fn canonical_email_hint(value: &str) -> Option<String> {
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    let value = value.trim();
    if value.len() > 254 || !value.is_ascii() {
        return None;
    }
    let (local, domain) = value.split_once('@')?;
    if local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || domain.len() > 253
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
        || !local
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&c))
        || !domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return None;
    }
    Some(format!("{local}@{}", domain.to_ascii_lowercase()))
}

/// Phone or mailbox hints of one person: at most 128, each bounded.
fn hints_bounded(hints: &[String]) -> Result<(), Refusal> {
    if hints.len() > 128 || hints.iter().any(|hint| hint.len() > 512) {
        return Err(refused());
    }
    Ok(())
}

fn refused() -> Refusal {
    Refusal::from("contact identity or recipient is unavailable")
}

fn validate_endpoint(service: &str, recipient: &str) -> Result<(), Refusal> {
    if !["tilo", "whatsapp", "signal", "sms"].contains(&service)
        || recipient.is_empty()
        || recipient.len() > 512
        || recipient
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
    {
        return Err(refused());
    }
    Ok(())
}

fn valid_metadata(value: &str) -> bool {
    value.len() <= 512
        && !value.chars().any(char::is_control)
        && (value.is_empty() || !value.trim().is_empty())
}

/// Strict international spelling only; no country or extension guessing.
pub fn canonical_phone(value: &str) -> Option<String> {
    let value = value.trim();
    if !value.starts_with('+') || value.len() > 64 {
        return None;
    }
    let mut digits = String::new();
    for c in value.chars().skip(1) {
        if c.is_ascii_digit() {
            digits.push(c);
        } else if !matches!(c, ' ' | '-' | '(' | ')' | '.') {
            return None;
        }
    }
    (digits.len() >= 2 && digits.len() <= 15 && !digits.starts_with('0'))
        .then(|| format!("+{digits}"))
}

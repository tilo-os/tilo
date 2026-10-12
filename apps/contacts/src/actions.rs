//! What Contacts does beside editing the book — its effects — and the
//! address-book logic its verbs and views share: finding likely
//! duplicates, the reviewed merge of two contacts, and the book's groups.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use ui::tilo_resource::{self, Effect, JsonSchema};

use crate::model::{Address, Contact, ContactId, Labeled, group_names_equal, normalize_text};

/// Potential duplicate review resources are intentionally capped.  The full
/// detector stays linear in contacts and their fields, but discovery must not
/// turn a large imported book into thousands of palette entries.
pub const MAX_CONTEXT_DUPLICATE_RESOURCES: usize = 32;

/// What Contacts does that is no edit of the address book: showing a
/// group or the favorites, reviewing a likely duplicate, and
/// moving cards in and out through the system file chooser.
#[derive(Debug, Clone, PartialEq, Eq, Effect)]
#[effect(operation = "contacts.effect")]
pub enum ContactsEffect {
    /// Open group.
    #[effect(view, row, idempotent)]
    OpenGroup {
        #[param("The group's name")]
        name: String,
    },
    /// Open favorites.
    #[effect(view, row, idempotent)]
    OpenFavorites,
    /// Review possible duplicate.
    #[effect(view, row, idempotent)]
    OpenReview {
        #[param("The contact that stays")]
        survivor: uuid::Uuid,
        #[param("The contact merged into it")]
        source: uuid::Uuid,
    },
    /// Import vCard.
    #[effect(job)]
    ImportVcard,
    /// Export all contacts.
    #[effect(job, sensitive)]
    ExportAllVcard,
    /// Export contact.
    #[effect(sensitive)]
    ExportContact {
        #[param("The contact's id")]
        contact: uuid::Uuid,
    },
    /// Keep existing contact.
    #[effect(row, idempotent)]
    KeepExisting {
        #[param("The pending import review")]
        token: String,
    },
    /// Import as new contact.
    #[effect(row, destructive)]
    ImportAsNew {
        #[param("The pending import review")]
        token: String,
    },
}

/// One field on which a duplicate review can deliberately keep the other
/// contact's scalar value.  Repeated fields are always unioned by their
/// normalized value, so no unique phone, email, group, or address is silently
/// thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum MergeField {
    First,
    Last,
    Organization,
    Title,
    Website,
    Birthday,
    Notes,
}

/// Why two contacts were offered for review.  These are derived locally from
/// already-synced contact data and are not written into the shared document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateReason {
    Email,
    Phone,
    Name,
}

impl DuplicateReason {
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Email => "matching email",
            Self::Phone => "matching phone",
            Self::Name => "matching name",
        }
    }
}

/// A bounded, deterministic potential duplicate.  The lower UUID is always
/// the default survivor, which makes the review URL and resource identity
/// stable across peers; the person may still choose every conflicting scalar
/// from either side before confirming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateCandidate {
    pub survivor_id: ContactId,
    pub source_id: ContactId,
    pub reasons: Vec<DuplicateReason>,
}

/// Unicode/case-insensitive email key.  The mail local part can technically
/// be case-sensitive; Contacts follows the ordinary address-book expectation
/// here only to *suggest* review — it never merges without a confirmation.
fn normalized_email(value: &str) -> String {
    normalize_text(value.trim())
}

/// Formatting-insensitive phone key.  Keeping only decimal digits lets
/// `+1 (555) 0100` and `15550100` meet, while an empty/non-numeric value never
/// creates a spurious candidate.
fn normalized_phone(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).collect()
}

fn normalized_name(contact: &Contact) -> String {
    normalize_text(&format!("{} {}", contact.first.trim(), contact.last.trim()))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find likely duplicate pairs without allocating a full quadratic result for
/// a huge imported book.  The visible resource page caps its children, while
/// this pure helper returns at most `limit` deterministic pairs for callers
/// that have already chosen a bounded presentation budget.
pub fn duplicate_candidates(contacts: &[Contact], limit: usize) -> Vec<DuplicateCandidate> {
    if limit == 0 {
        return Vec::new();
    }
    let mut ordered: Vec<&Contact> = contacts.iter().collect();
    ordered.sort_by_key(|contact| contact.id);
    let mut email_owner = HashMap::<String, ContactId>::new();
    let mut phone_owner = HashMap::<String, ContactId>::new();
    let mut name_owner = HashMap::<String, ContactId>::new();
    let mut pairs = BTreeMap::<(ContactId, ContactId), Vec<DuplicateReason>>::new();
    let mut add = |left: ContactId, right: ContactId, reason: DuplicateReason| {
        let pair = left.min(right);
        let other = left.max(right);
        let reasons = pairs.entry((pair, other)).or_default();
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    };
    for contact in ordered {
        for key in contact
            .emails
            .iter()
            .map(|entry| normalized_email(&entry.value))
            .filter(|key| !key.is_empty())
        {
            if let Some(existing) = email_owner.insert(key, contact.id) {
                add(existing, contact.id, DuplicateReason::Email);
            }
        }
        for key in contact
            .phones
            .iter()
            .map(|entry| normalized_phone(&entry.value))
            .filter(|key| !key.is_empty())
        {
            if let Some(existing) = phone_owner.insert(key, contact.id) {
                add(existing, contact.id, DuplicateReason::Phone);
            }
        }
        let name = normalized_name(contact);
        if !name.is_empty()
            && let Some(existing) = name_owner.insert(name, contact.id)
        {
            add(existing, contact.id, DuplicateReason::Name);
        }
    }
    pairs
        .into_iter()
        .take(limit)
        .map(|((survivor_id, source_id), reasons)| DuplicateCandidate {
            survivor_id,
            source_id,
            reasons,
        })
        .collect()
}

/// Resolve one exact candidate without building a catalogue page.  Resource
/// lookup uses this path so a stale/deep link costs only two contacts' fields,
/// not an unbounded scan result allocation.
pub fn duplicate_candidate(survivor: &Contact, source: &Contact) -> Option<DuplicateCandidate> {
    if survivor.id >= source.id {
        return None;
    }
    let mut reasons = Vec::new();
    let source_emails: HashSet<_> = source
        .emails
        .iter()
        .map(|entry| normalized_email(&entry.value))
        .filter(|key| !key.is_empty())
        .collect();
    if survivor
        .emails
        .iter()
        .map(|entry| normalized_email(&entry.value))
        .any(|key| source_emails.contains(&key))
    {
        reasons.push(DuplicateReason::Email);
    }
    let source_phones: HashSet<_> = source
        .phones
        .iter()
        .map(|entry| normalized_phone(&entry.value))
        .filter(|key| !key.is_empty())
        .collect();
    if survivor
        .phones
        .iter()
        .map(|entry| normalized_phone(&entry.value))
        .any(|key| source_phones.contains(&key))
    {
        reasons.push(DuplicateReason::Phone);
    }
    let left_name = normalized_name(survivor);
    if !left_name.is_empty() && left_name == normalized_name(source) {
        reasons.push(DuplicateReason::Name);
    }
    (!reasons.is_empty()).then_some(DuplicateCandidate {
        survivor_id: survivor.id,
        source_id: source.id,
        reasons,
    })
}

fn same_labeled(left: &Labeled, right: &Labeled, phone: bool) -> bool {
    normalize_text(left.label.trim()) == normalize_text(right.label.trim())
        && if phone {
            normalized_phone(&left.value) == normalized_phone(&right.value)
        } else {
            normalized_email(&left.value) == normalized_email(&right.value)
        }
}

fn same_address(left: &Address, right: &Address) -> bool {
    [
        &left.label,
        &left.street,
        &left.city,
        &left.region,
        &left.postal,
        &left.country,
    ]
    .into_iter()
    .zip([
        &right.label,
        &right.street,
        &right.city,
        &right.region,
        &right.postal,
        &right.country,
    ])
    .all(|(left, right)| normalize_text(left.trim()) == normalize_text(right.trim()))
}

fn merged_scalar<S: std::hash::BuildHasher>(
    survivor: &str,
    source: &str,
    field: MergeField,
    choices: &HashSet<MergeField, S>,
) -> String {
    if survivor.trim().is_empty() || choices.contains(&field) {
        source.to_string()
    } else {
        survivor.to_string()
    }
}

/// Materialise the exact reviewed survivor contact.  Empty scalar values fill
/// from the other record; a non-empty conflict stays with the survivor until
/// the review selects `MergeField`.  Repeated rows are a lossless union —
/// the source rows keep their ids, so they join the survivor's rows rather
/// than replacing them.
pub fn reviewed_merge_contact<S: std::hash::BuildHasher>(
    survivor: &Contact,
    source: &Contact,
    choices: &HashSet<MergeField, S>,
) -> Contact {
    let mut merged = survivor.clone();
    merged.first = merged_scalar(&survivor.first, &source.first, MergeField::First, choices);
    merged.last = merged_scalar(&survivor.last, &source.last, MergeField::Last, choices);
    merged.org = merged_scalar(
        &survivor.org,
        &source.org,
        MergeField::Organization,
        choices,
    );
    merged.title = merged_scalar(&survivor.title, &source.title, MergeField::Title, choices);
    merged.website = merged_scalar(
        &survivor.website,
        &source.website,
        MergeField::Website,
        choices,
    );
    merged.notes = merged_scalar(&survivor.notes, &source.notes, MergeField::Notes, choices);
    merged.birthday = if survivor.birthday.is_none() || choices.contains(&MergeField::Birthday) {
        source.birthday
    } else {
        survivor.birthday
    };
    for group in &source.groups {
        if !merged
            .groups
            .iter()
            .any(|existing| group_names_equal(&existing.value, &group.value))
        {
            merged.groups.push(group.clone());
        }
    }
    for phone in &source.phones {
        if !merged
            .phones
            .iter()
            .any(|existing| same_labeled(existing, phone, true))
        {
            merged.phones.push(phone.clone());
        }
    }
    for email in &source.emails {
        if !merged
            .emails
            .iter()
            .any(|existing| same_labeled(existing, email, false))
        {
            merged.emails.push(email.clone());
        }
    }
    for address in &source.addresses {
        if !merged
            .addresses
            .iter()
            .any(|existing| same_address(existing, address))
        {
            merged.addresses.push(address.clone());
        }
    }
    merged.favorite |= source.favorite;
    merged
}

/// Split a typed full name into first / rest-as-last, so a palette "New
/// contact" / "Rename" that collects a single name string lands the same way
/// the form's two fields would.
pub(crate) fn split_name(name: &str) -> (String, String) {
    let name = name.trim();
    match name.split_once(char::is_whitespace) {
        Some((first, rest)) => (first.trim().to_string(), rest.trim().to_string()),
        None => (name.to_string(), String::new()),
    }
}

/// The id of the pending review of one imported card whose UID collides with
/// an existing contact.
pub fn import_conflict_resource_id(token: &str) -> String {
    format!("contacts.import-conflict:{token}")
}

/// A pending import review, published while it waits for a decision. It is
/// not document state: an untrusted card never enters the shared book before
/// the person chooses to keep the existing contact or import it as new.
pub(crate) fn import_conflict_row(token: &str, incoming_name: &str) -> ui::framework::LiveRow {
    ui::framework::LiveRow::new(
        import_conflict_resource_id(token),
        "contacts.import-conflict",
        format!("Import review: {incoming_name}"),
    )
    .effect(&ContactsEffect::KeepExisting {
        token: token.to_owned(),
    })
    .effect(&ContactsEffect::ImportAsNew {
        token: token.to_owned(),
    })
}

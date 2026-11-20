//! The mailbox's attended recovery action reads our earlier copy and writes
//! only ordinary Mail fields through the published workspace action.

use std::rc::Rc;

use durable_store::DurableStore;
use tilolang_tilo::{
    action::outbox_entries,
    bundle::Bundle,
    chunk::Chunks,
    host::Host,
    instance::document::archived_member_document,
    runtime::{Actor, ResourceRef, Value, ids::MemberId},
};

pub(crate) const APP: &str = "so.tilo.lang.mail";
pub(crate) const ACTION: &str = "recover_previous";
const DRAFT_FIELDS: [&str; 7] = [
    "from",
    "to",
    "cc",
    "subject",
    "body",
    "in_reply_to",
    "references",
];
const BATCH: usize = 16;
const THREAD_FIELDS: &[&str] = &[
    "account",
    "subject",
    "latest",
    "snippet",
    "senders",
    "unread",
    "flagged",
    "snoozed_until",
    "muted",
];
const MESSAGE_FIELDS: &[&str] = &[
    "subject",
    "from",
    "from_name",
    "to",
    "cc",
    "received",
    "snippet",
    "body",
    "message_id",
    "references",
    "attachments",
    "unread",
    "flagged",
    "mailbox",
];

pub(crate) fn arguments(
    documents: &Rc<dyn DurableStore>,
    route: &collaboration::RouteSpec,
    person: &str,
    bundle: &Bundle,
    live: &Host,
) -> Result<String, String> {
    let mut drafts = Vec::new();
    let mut labels = Vec::new();
    let mut conversations = Vec::new();
    let mut signatures = Vec::new();
    let earlier = archived_member_document(documents, &route.app_id, APP, route.cluster_id, person)
        .map_err(|error| error.to_string())?;
    if let Some((first, rest)) = earlier {
        let mut chunks = Chunks::of(first.clone());
        for (index, part) in rest {
            chunks.insert(index, part);
        }
        let archive = Host::new(&first, Actor::from(MemberId::person(person)), || None)
            .map_err(|error| error.to_string())?;
        archive
            .load_bundle(bundle)
            .map_err(|error| error.to_string())?;
        archive.adopt(chunks.clone());
        let previous = archive.root_of(APP).map_err(|error| error.to_string())?;
        let current = live.root_of(APP).map_err(|error| error.to_string())?;
        conversations = archived_conversations(&archive, &previous, live, &current, person)?;
        signatures = archived_signatures(&archive, &previous, live, &current, person)?;
        let imported = match live.runtime().read(&current, "recovered_drafts") {
            Ok(Value::List(keys)) => keys,
            Ok(_) => return Err("the mailbox's recovery keys could not be read".into()),
            Err(error) => return Err(error.to_string()),
        };
        for draft in items(&archive, &previous, "drafts")? {
            let id = draft.id_string();
            let key = tilo_content::hash_bytes(format!("mail-archive:{person}:{id}").as_bytes());
            if imported
                .iter()
                .any(|value| value.as_text() == Some(key.as_str()))
            {
                continue;
            }
            let mut row = vec![key];
            for field in DRAFT_FIELDS {
                row.push(text(&archive, &draft, field)?);
            }
            let sent = archive
                .runtime()
                .read(&draft, "sent")
                .map_err(|error| error.to_string())?;
            if matches!(sent, Value::Bool(false)) || known_unsent(&chunks, &draft, &row[1..]) {
                drafts.push(row);
            }
            if drafts.len() == BATCH {
                break;
            }
        }
        let known = items(live, &current, "labels")?
            .iter()
            .map(|label| text(live, label, "name"))
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        for label in items(&archive, &previous, "labels")? {
            let name = text(&archive, &label, "name")?;
            if !name.is_empty() && !known.contains(&name) && !labels.contains(&name) {
                labels.push(name);
            }
            if labels.len() == BATCH {
                break;
            }
        }
    }
    let payload = serde_json::json!({
        "drafts": drafts,
        "labels": labels,
        "conversations": conversations,
        "signatures": signatures,
    });
    Ok(format!("{}{payload}", tilo_actions::ARGUMENTS_JSON_PREFIX))
}

fn archived_signatures(
    archive: &Host,
    previous: &ResourceRef,
    live: &Host,
    current: &ResourceRef,
    person: &str,
) -> Result<Vec<[String; 3]>, String> {
    let mut available = std::collections::HashSet::new();
    for account in items(live, current, "accounts")? {
        if text(live, &account, "signature")?.is_empty() {
            available.insert(text(live, &account, "key")?);
        }
    }
    let recovered = text_list(live, current, "recovered_signatures")?;
    let mut signatures = Vec::new();
    for account in items(archive, previous, "accounts")? {
        let key = text(archive, &account, "key")?;
        if key.is_empty() || !available.contains(&key) {
            continue;
        }
        let stamp = tilo_content::hash_bytes(format!("mail-signature:{person}:{key}").as_bytes());
        let signature = text(archive, &account, "signature")?;
        if !signature.is_empty() && !recovered.contains(&stamp) {
            available.remove(&key);
            signatures.push([stamp, key, signature]);
        }
        if signatures.len() == BATCH {
            break;
        }
    }
    Ok(signatures)
}

fn archived_conversations(
    archive: &Host,
    previous: &ResourceRef,
    live: &Host,
    current: &ResourceRef,
    person: &str,
) -> Result<Vec<Vec<Vec<String>>>, String> {
    let recovered = text_list(live, current, "recovered_conversations")?;
    let mut conversations = Vec::new();
    for account in items(archive, previous, "accounts")? {
        for thread in items(archive, &account, "threads")? {
            let id = thread.id_string();
            let key =
                tilo_content::hash_bytes(format!("mail-conversation:{person}:{id}").as_bytes());
            if recovered.contains(&key) {
                continue;
            }
            let mut header = vec![key];
            header.extend(scalars(archive, &thread, THREAD_FIELDS)?);
            let mut rows = vec![header];
            for field in ["labels", "boxes"] {
                let mut row = vec![String::new()];
                row.extend(text_list(archive, &thread, field)?);
                rows.push(row);
            }
            for message in items(archive, &thread, "messages")? {
                let mut row = vec!["message".to_owned()];
                row.extend(scalars(archive, &message, MESSAGE_FIELDS)?);
                rows.push(row);
            }
            conversations.push(rows);
            if conversations.len() == BATCH {
                return Ok(conversations);
            }
        }
    }
    Ok(conversations)
}

fn scalars(host: &Host, at: &ResourceRef, fields: &[&str]) -> Result<Vec<String>, String> {
    fields
        .iter()
        .map(|field| {
            match host
                .runtime()
                .read(at, field)
                .map_err(|error| error.to_string())?
            {
                Value::Text(value) if matches!(*field, "latest" | "received" | "snoozed_until") => {
                    tilolang_tilo::tilolang::calendar::instant(&value)
                        .or_else(|| value.is_empty().then_some(0))
                        .map(|seconds| seconds.to_string())
                        .ok_or_else(|| format!("the mailbox's {field} contains an invalid time"))
                }
                Value::Text(value) => Ok(value),
                Value::Bool(value) => Ok(value.to_string()),
                _ => Err(format!("the mailbox's {field} contains an invalid value")),
            }
        })
        .collect()
}

fn text_list(host: &Host, at: &ResourceRef, field: &str) -> Result<Vec<String>, String> {
    let Value::List(values) = host
        .runtime()
        .read(at, field)
        .map_err(|error| error.to_string())?
    else {
        return Err(format!("the mailbox's {field} could not be read"));
    };
    values
        .into_iter()
        .map(|value| match value {
            Value::Text(text) => Ok(text),
            _ => Err(format!("the mailbox's {field} contains an invalid item")),
        })
        .collect()
}

/// SMTP jobs for draft items are kept on their containing document. Match
/// all seven message fields so an unrelated queued job cannot recover a sent draft.
fn known_unsent(chunks: &Chunks, draft: &ResourceRef, fields: &[String]) -> bool {
    let Some(doc) = chunks.doc(&draft.doc) else {
        return false;
    };
    let payload = serde_json::json!(fields);
    let mut matching = outbox_entries(doc, APP, &draft.doc)
        .into_iter()
        .filter(|job| job.capability == "smtp.send" && job.payload == payload);
    let unsent = |job: tilolang_tilo::action::StoredEntry| {
        matches!(
            job.delivery().state.as_str(),
            "waiting" | "held" | "rejected" | "withdrawn"
        )
    };
    matching.next().is_some_and(unsent) && matching.all(unsent)
}

fn items(host: &Host, at: &ResourceRef, field: &str) -> Result<Vec<ResourceRef>, String> {
    let Value::List(values) = host
        .runtime()
        .read(at, field)
        .map_err(|error| error.to_string())?
    else {
        return Err(format!("the mailbox's {field} could not be read"));
    };
    values
        .into_iter()
        .map(|value| match value {
            Value::Ref(item) => Ok((*item.target).clone()),
            _ => Err(format!("the mailbox's {field} contains an invalid item")),
        })
        .collect()
}

fn text(host: &Host, at: &ResourceRef, field: &str) -> Result<String, String> {
    match host
        .runtime()
        .read(at, field)
        .map_err(|error| error.to_string())?
    {
        Value::Text(value) => Ok(value),
        _ => Err(format!("the mailbox's {field} could not be read as text")),
    }
}

//! Import the current member's previous mailbox into the workspace.

use serde::{Deserialize, Serialize};
use tilo_content::ContentStore;
use ui::framework::{Chain, EffectCx, ResourceOpener};
use ui::tilo_resource::{self, JsonSchema, Model as _};

use crate::actions::DraftData;
use crate::message_states::MessageStates;
use crate::resource::{Mail, mailbox};

const DRAFTS_PER_IMPORT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreviousFolder {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreviousDraft {
    pub id: String,
    pub message: DraftData,
    pub in_reply_to: Option<String>,
    pub notes: String,
}

pub(crate) fn import(
    opener: &ResourceOpener<Mail>,
    cx: &EffectCx<Mail>,
) -> Result<serde_json::Value, String> {
    let dispatch = cx.dispatcher();
    if !dispatch.may(&tilo_actions::ActionNeed::of(
        tilo_actions::ActionKind::Undoable,
    )) {
        return Err("the workspace must grant permission to import a mailbox".into());
    }
    let Some((record, legacy, source)) = opener.legacy_member_content()? else {
        return Ok(serde_json::json!({ "folders": 0, "drafts": 0, "waiting": 0 }));
    };
    let content = tilo_resource::content::current();
    let current = cx.live().get();
    let mut folders = Vec::new();
    for (key, folder) in legacy.folders.iter() {
        let id = entry_id(&record, "folder", key.as_str())?;
        if !current.imported.contains(&id) {
            folders.push(PreviousFolder {
                id,
                name: folder.name.clone(),
            });
        }
    }
    let all_drafts = std::iter::once(("composer", &legacy.draft))
        .chain(legacy.drafts.iter().map(|(id, draft)| (id.as_str(), draft)));
    let drafts = all_drafts
        .filter(|(_, draft)| !draft.is_empty())
        .map(|(key, draft)| {
            (
                "draft",
                key,
                DraftData::from_draft(&draft.draft()),
                draft.in_reply_to.clone(),
                draft.notes.clone(),
            )
        });
    let pending = legacy.outbox.pending().map(|(id, outgoing)| {
        (
            "send",
            id.as_str(),
            outgoing.draft.clone(),
            outgoing.in_reply_to.clone(),
            String::new(),
        )
    });
    let mut recovered = Vec::new();
    let mut waiting = 0;
    for (kind, key, mut message, in_reply_to, notes) in drafts.chain(pending) {
        let id = entry_id(&record, kind, key)?;
        if current.imported.contains(&id) {
            continue;
        }
        if recovered.len() == DRAFTS_PER_IMPORT
            || !rekey_attachments(&mut message, &source, content.as_ref())?
        {
            waiting += 1;
            continue;
        }
        recovered.push(PreviousDraft {
            id,
            message,
            in_reply_to,
            notes,
        });
    }
    let counts = serde_json::json!({
        "folders": folders.len(),
        "drafts": recovered.len(),
        "waiting": waiting,
    });
    if !folders.is_empty() || !recovered.is_empty() {
        let action = Mail::IMPORT_MAILBOX_ENTRIES.with(mailbox::ImportMailboxEntries {
            folders,
            drafts: recovered,
        });
        if !dispatch.may_invoke(&action) {
            return Err("the workspace must grant permission to import mailbox entries".into());
        }
        cx.submit(action);
    }
    Ok(counts)
}

/// Recover the older mailbox's message flags and locally delivered mail.
pub(crate) fn import_messages(
    opener: &ResourceOpener<Mail>,
    chain: &Chain<MessageStates>,
    cx: &EffectCx<Mail>,
) -> Result<serde_json::Value, String> {
    const MESSAGES_PER_IMPORT: usize = 32;
    if !cx.dispatcher().may(&tilo_actions::ActionNeed::of(
        tilo_actions::ActionKind::Undoable,
    )) {
        return Err("the workspace must grant permission to import messages".into());
    }
    let Some((_, previous, source)) = opener.legacy_member_parts::<MessageStates>()? else {
        return Ok(serde_json::json!({ "messages": 0, "waiting": 0 }));
    };
    let current = chain.parts();
    let previous = MessageStates::of(&previous);
    let content = tilo_resource::content::current();
    let mut imported = 0;
    let mut waiting = 0;
    for (id, state) in previous.messages.iter() {
        if current.iter().any(|(_, part)| part.holds(id)) {
            continue;
        }
        if imported == MESSAGES_PER_IMPORT {
            waiting += 1;
            continue;
        }
        let mut body = previous.held.get(id).cloned().filter(|_| !state.purged);
        if let Some(body) = &mut body
            && let Some(original) = &body.original
        {
            let Some(bytes) = original.read(&source) else {
                waiting += 1;
                continue;
            };
            let store = content
                .as_ref()
                .ok_or("the workspace has no mail content store")?;
            let reference = store.put(&bytes).map_err(|error| error.to_string())?;
            body.original = Some(tilo_resource::Bytes::kept_as(&bytes, &reference));
        }
        let mut state = state.clone();
        state.server = None;
        let action = MessageStates::IMPORT_PREVIOUS_MESSAGE.with((
            id.as_str().to_owned(),
            state.to_json(),
            body.as_ref().map(tilo_resource::Model::to_json),
        ));
        let written = chain.invoke(&action).map_err(|error| error.to_string())?;
        if written.is_none() {
            waiting += 1;
            break;
        }
        imported += 1;
    }
    Ok(serde_json::json!({ "messages": imported, "waiting": waiting }))
}

fn entry_id(record: &str, kind: &str, id: &str) -> Result<String, String> {
    let hash = tilo_content::hash_bytes(format!("mail/previous/{record}/{kind}/{id}").as_bytes());
    hash.get(..32)
        .and_then(|hex| uuid::Uuid::parse_str(hex).ok())
        .map(|id| id.to_string())
        .ok_or_else(|| "an imported mailbox entry could not be named".to_owned())
}

fn rekey_attachments(
    message: &mut DraftData,
    source: &ContentStore,
    target: Option<&ContentStore>,
) -> Result<bool, String> {
    use crate::attachment_store::{MAX_ATTACHMENT_BYTES, PREFIX};
    if message.attachments.is_empty() {
        return Ok(true);
    }
    let target = target.ok_or("the workspace has no mail content store")?;
    let mut attachments = Vec::new();
    for file in &message.attachments {
        let reference = file
            .content_id
            .strip_prefix(PREFIX)
            .ok_or("an older attachment has an invalid reference")?;
        let Some(bytes) = source.read(reference) else {
            return Ok(false);
        };
        if bytes.len() > MAX_ATTACHMENT_BYTES || u64::try_from(bytes.len()).ok() != Some(file.size)
        {
            return Err("an older attachment has an invalid size".to_owned());
        }
        attachments.push(bytes);
    }
    for (file, bytes) in message.attachments.iter_mut().zip(attachments) {
        let reference = target.put(&bytes).map_err(|error| error.to_string())?;
        file.content_id = format!("{PREFIX}{reference}");
    }
    Ok(true)
}

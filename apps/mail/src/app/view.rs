//! The view of the mailbox: what builds the [`MailApp`] window over the
//! framework, answers effects from the [`Mailroom`] it shares with it and
//! publishes what the mailbox does not hold.

use std::cell::RefCell;
use std::rc::Rc;

use accounts::person::PersonAccounts;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, Publishes, ViewCx};
use ui::portal::{FileFilter, PickRequest};
use ui::tilo_resource::SearchEntry;
use ui::widget::Element;

use super::{MailApp, Mailroom};
use crate::actions::{MailEffect, Outgoing, ServerUpdate};
use crate::resource::Mail;
use crate::state::MailState;

/// Whether an account set up on this device sends from `address`.
fn holds_account(state: &MailState, address: &str) -> bool {
    state
        .accounts
        .accounts
        .iter()
        .any(|account| account.config.email == address)
}

/// The window as the view lays it out: it follows the person's records in
/// a live window, carries out the document's server updates and outbox
/// from a device with the account, answers [`MailEffect`]s from the
/// [`Mailroom`] it shares with the window, and publishes the rows and
/// search entries the mailbox does not hold.
pub(crate) fn window(cx: &ViewCx<Mail>, mut window: MailApp) -> Element {
    window.live = Some(cx.live().clone());
    window.attach_framework(cx.dispatcher());
    // A screenshot shows its fixture as it is, whatever the profile holds.
    if cx.mode().is_live() {
        window.wake_from_workers(cx.waker());
        window.follow_preferences(cx.settings::<crate::resource::MailPreferences>());
        if let Some(trusted) = cx.person::<crate::trusted_senders::TrustedSenders>() {
            window.follow_trusted_senders(trusted);
        }
        window.state().contact_directory =
            cx.shared_person::<tilo_contact_directory::ContactDirectory>();
        if let Some(accounts) = cx.shared_person::<PersonAccounts>() {
            window.follow_person_accounts(accounts);
        }
        if let Some(copies) = cx.person::<crate::copies::MailCopies>() {
            window.share_copies(copies);
        }
    }
    // Every change the document records that a mail server does not hold
    // yet — whoever made it — goes to the server from a live window on a
    // device with the account, and the engine's acknowledgement records it
    // as applied.
    // A window that reaches no server (a past version, one opened offline)
    // takes none of it: refused there, it would be settled as failed and no
    // window that can reach the server would carry it out.
    let outbox = window.server_outbox();
    let (shared, carrier) = (Rc::clone(&window.shared), window.room());
    let projection = cx.outbox(
        Mail::paths().server,
        move |_, update: &ServerUpdate| {
            carrier.borrow().reaches_servers() && holds_account(&shared.borrow(), update.account())
        },
        move |_, update: ServerUpdate, cx| {
            let reply = cx.reply();
            outbox.borrow_mut().push(
                update,
                Box::new(move |result| {
                    reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
                }),
            );
            Ok(Effected::Pending)
        },
    );
    window.follow_projection(projection);
    let outbox = window.server_outbox();
    let (shared, carrier) = (Rc::clone(&window.shared), window.room());
    let delivery = cx.outbox(
        Mail::paths().outbox,
        move |_, outgoing: &Outgoing| {
            carrier.borrow().reaches_servers() && holds_account(&shared.borrow(), &outgoing.account)
        },
        move |_, outgoing: Outgoing, cx| {
            let reply = cx.reply();
            outbox.borrow_mut().deliver(
                outgoing,
                Box::new(move |result| {
                    reply.finish(result.map(|settled| tilo_actions::ActionOutput {
                        value: serde_json::to_value(&settled).ok(),
                        resources: Vec::new(),
                    }));
                }),
            );
            Ok(Effected::Pending)
        },
    );
    window.follow_delivery(delivery);
    // A search hit, or an agent's `open`, shows the folder or message.
    cx.on_open(window.opener());
    let room = window.room();
    {
        let (answer, status) = (Rc::clone(&room), Rc::clone(&room));
        cx.effects(move |effect: MailEffect, cx| answer_effect(&answer, effect, cx))
            .status(move || status.borrow().effect_statuses());
    }
    {
        let room = Rc::clone(&room);
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario {
                room.borrow_mut().request_stage(scenario);
            }
        });
    }
    let (rows, row, entries, query) = (
        Rc::clone(&room),
        Rc::clone(&room),
        Rc::clone(&room),
        Rc::clone(&room),
    );
    let (listed, named) = (cx.live().clone(), cx.live().clone());
    cx.publishing(
        Publishes::new()
            // The accounts, an in-flight send, a search-index fault, and
            // each draft as the item its verbs name.
            .live_rows(move || {
                let mut rows = rows.borrow().live_rows();
                rows.extend(draft_rows(&listed.get()));
                rows
            })
            // A loaded message, thread, attachment or account the mailbox
            // does not publish, or a draft.
            .live_row(move |id| match id.strip_prefix("mail.draft:") {
                Some(draft) => draft_row(&named.get(), draft),
                None => row.borrow().live_row(id),
            })
            .search_entries(move || entries.borrow().search_entries())
            // Someone searches: the servers are asked too, for mail older
            // than what this device keeps; what they find joins the entries.
            .search_query(move |asked| query.borrow_mut().search_servers(asked.text()))
            // The servers are still looking: a search showing waits for what
            // they find.
            .search_pending(move || room.borrow().searching_servers()),
        Box::new(window),
    )
}

/// Every draft, as the item (`mail.draft:<id>`) its verbs name.
fn draft_rows(mail: &Mail) -> Vec<ui::framework::LiveRow> {
    mail.drafts_listed()
        .into_iter()
        .map(|(id, draft)| {
            ui::framework::LiveRow::from_resource(crate::actions::draft_resource(&id, draft))
        })
        .collect()
}

/// The draft `id`, as the item its verbs name, while it is there.
fn draft_row(mail: &Mail, id: &str) -> Option<ui::framework::LiveRow> {
    let id = crate::resource::draft_named(id);
    let draft = mail.draft_at(Some(id))?;
    (!crate::resource::is_composer_draft(id) || mail.has_draft())
        .then(|| ui::framework::LiveRow::from_resource(crate::actions::draft_resource(id, draft)))
}

/// Answer `effect` from what the window shares with the view; what needs
/// the window waits in `room` for its next sync.
fn answer_effect(
    room: &Rc<RefCell<Mailroom>>,
    effect: MailEffect,
    cx: &mut EffectCx<Mail>,
) -> EffectAnswer {
    // Held mail is read from the record alone, in a window or a worker.
    if let Some(answer) = crate::held::effect(&effect, &cx.live().get()) {
        return answer;
    }
    // Messages are read, searched and answered from what this device keeps
    // of its servers and what the record holds, whatever a window shows.
    match &effect {
        MailEffect::SearchMessages { .. } => {
            let query = crate::search::MessageQuery::of(&effect)?;
            let mail = cx.live().get();
            return Ok(Effected::value(room.borrow().search_synced(&mail, &query)));
        }
        MailEffect::ReadMessage { message } => {
            let mail = cx.live().get();
            let found = room
                .borrow()
                .synced_message(&mail, message)
                .ok_or_else(|| no_such_message(message))?;
            return Ok(Effected::value(super::message_read(&found)));
        }
        MailEffect::DraftReply {
            message,
            body,
            all,
            notes,
        } => {
            return draft_reply(
                room,
                message,
                body,
                all.unwrap_or(false),
                notes.as_deref(),
                cx,
            );
        }
        MailEffect::Content { message, index } => {
            let mail = cx.live().get();
            return room.borrow().attachment_content(&mail, message, *index);
        }
        MailEffect::OpenAttachment {
            message,
            index,
            group,
        } => {
            let mail = cx.live().get();
            let handed = room.borrow().attachment_content(&mail, message, *index)?;
            return open_on_desktop(handed, group.as_deref(), cx);
        }
        _ => {}
    }
    if let MailEffect::Save { message, index } = &effect {
        let (name, bytes) = room.borrow().attachment_to_save(message, *index)?;
        let resource = format!("mail.attachment:{message}:{index}");
        let request = PickRequest::save("Save attachment", "Save", &name)
            .thread_name("mail-portal-save-attachment")
            .filter(FileFilter::all_files());
        let room = Rc::clone(room);
        return Ok(cx.save_file(request, bytes, move |(), saved, _| {
            Ok(room
                .borrow_mut()
                .finish_attachment_save(saved, &name, resource))
        }));
    }
    let mut room = room.borrow_mut();
    if matches!(effect, MailEffect::PrepareContactDraft { .. }) {
        room.has_contact_draft_conflict = cx.live().get().has_draft();
    }
    let invocation = cx.invocation().unwrap_or_default();
    let answer = room.begin_effect(invocation, effect)?;
    if matches!(answer, Effected::Pending) {
        room.await_effect(invocation, cx.reply());
    }
    room.dirty = true;
    Ok(answer)
}

/// Open the attachment `handed` over as workspace content through the
/// desktop's `open`, in the app registered for its media type: as a tab of
/// the window `group`, else a window of its own. The desktop decides it as
/// this effect's caller and answers once the window is up; the answer is
/// the window's, with the content it shows.
fn open_on_desktop(handed: Effected, group: Option<&str>, cx: &mut EffectCx<Mail>) -> EffectAnswer {
    use ui::desktop::{OPEN, PROVIDER, RESOURCE};
    let Effected::Done(handed) = handed else {
        return Err("the attachment was not handed over".into());
    };
    let attachment = handed.value.unwrap_or_default();
    let content = attachment
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or("the attachment has no content address")?
        .to_owned();
    let mut arguments = serde_json::Map::new();
    arguments.insert("target".into(), content.clone().into());
    if let Some(group) = group {
        arguments.insert("place".into(), "tab-of".into());
        arguments.insert("group".into(), group.into());
    }
    let open = ui::framework::Call::host(PROVIDER, RESOURCE, OPEN).with(arguments);
    Ok(cx.call(&open, move |(), opened, _| {
        let mut answer = opened?.value.unwrap_or_default();
        if let Some(fields) = answer.as_object_mut() {
            for key in ["content", "name", "media_type"] {
                if let Some(value) = attachment.get(key) {
                    fields.insert(key.into(), value.clone());
                }
            }
        }
        Ok(Effected::Done(tilo_actions::ActionOutput {
            value: Some(answer),
            resources: vec![content],
        }))
    }))
}

/// Why `message` names nothing to read.
fn no_such_message(message: &str) -> String {
    format!("no message `{message}` is kept here: search-messages lists the messages there are")
}

/// Start a new draft answering `message`, from the account it came
/// through, as the `create-draft` verb records it. Refused, before
/// anything is recorded, when the message is not kept here or the draft
/// would not be one.
fn draft_reply(
    room: &Rc<RefCell<Mailroom>>,
    message: &str,
    body: &str,
    all: bool,
    notes: Option<&str>,
    cx: &EffectCx<Mail>,
) -> EffectAnswer {
    let mail = cx.live().get();
    let room = room.borrow();
    let answered = room
        .synced_message(&mail, message)
        .ok_or_else(|| no_such_message(message))?;
    let mut drafted = crate::held::Drafted::reply(&answered, body, all, &answered.account);
    notes.unwrap_or("").clone_into(&mut drafted.notes);
    drafted.sender = room.sender_for(&answered.account);
    let id = uuid::Uuid::new_v4().to_string();
    let create = crate::resource::mailbox::CreateDraft {
        id: id.clone(),
        message: crate::actions::DraftData {
            sender: drafted.sender.clone(),
            to: drafted.to.clone(),
            cc: drafted.cc.clone(),
            subject: drafted.subject.clone(),
            body: drafted.body.clone(),
            ..crate::actions::DraftData::default()
        },
        in_reply_to: drafted.in_reply_to.clone(),
        notes: Some(drafted.notes),
    };
    // What the verb would refuse is refused here, so the answer never
    // claims a draft the record did not take.
    let mut probe = (*mail).clone();
    probe
        .create_draft(
            create.id.clone(),
            create.message.clone(),
            create.in_reply_to.clone(),
            create.notes.clone(),
        )
        .map_err(|refusal| refusal.to_string())?;
    let summary = probe
        .draft_at(Some(&id))
        .map(|draft| crate::held::draft_summary(&id, draft))
        .ok_or("the draft was not kept")?;
    cx.submit(Mail::CREATE_DRAFT.with(create));
    Ok(Effected::value(serde_json::json!({ "draft": summary })))
}

impl Mailroom {
    /// Whether this window reaches mail servers: it runs the engine that
    /// carries out what the document asks of them.
    pub(crate) const fn reaches_servers(&self) -> bool {
        self.engine.is_some()
    }

    /// The message `item` names, fetched or held: the record's held mail
    /// first, then what this device keeps of its servers. `None` once it
    /// was deleted for good.
    pub(crate) fn synced_message(&self, mail: &Mail, item: &str) -> Option<crate::model::Message> {
        if let Some(held) = mail.held_shown(item) {
            return Some(held);
        }
        let id = crate::held::message_named(item.trim())?;
        if mail.knows(id) && mail.message(id).is_none() {
            return None;
        }
        self.shared.borrow().mailbox.get(id.parse().ok()?).cloned()
    }

    /// What `query` finds in the mail this device keeps and the record's
    /// held mail, each message once.
    pub(crate) fn search_synced(
        &self,
        mail: &Mail,
        query: &crate::search::MessageQuery,
    ) -> serde_json::Value {
        let shared = self.shared.borrow();
        let held: Vec<crate::model::Message> = mail
            .held_all()
            .into_iter()
            .filter(|message| shared.mailbox.get(message.id).is_none())
            .collect();
        query.answer(
            shared
                .mailbox
                .messages
                .values()
                .filter(|message| {
                    let id = message.id.to_string();
                    !mail.knows(&id) || mail.message(&id).is_some()
                })
                .chain(held.iter()),
        )
    }

    /// The account `address` names, as a draft's sender.
    pub(crate) fn sender_for(&self, address: &str) -> Option<crate::model::DraftSender> {
        let shared = self.shared.borrow();
        shared
            .accounts
            .accounts
            .iter()
            .find(|account| !address.is_empty() && account.config.email == address)
            .map(|account| crate::model::DraftSender {
                account: account.id.clone(),
                email: account.config.email.clone(),
            })
    }

    /// Keep attachment `index` of `message` in the workspace's content and
    /// answer its address, name and media type.
    pub(crate) fn attachment_content(
        &self,
        mail: &Mail,
        message: &str,
        index: usize,
    ) -> EffectAnswer {
        let found = self
            .synced_message(mail, message)
            .ok_or_else(|| no_such_message(message))?;
        let attachment = found
            .attachments
            .get(index)
            .ok_or_else(|| format!("the message has no attachment {index}"))?;
        let media_type = attachment.mime.clone();
        let (name, bytes) = self.attachment_to_save(&found.id.to_string(), index)?;
        let store = ui::tilo_resource::content::current()
            .ok_or("this window keeps no content to hand the attachment over in")?;
        let hash = store
            .put(&bytes)
            .map_err(|error| format!("{name} could not be kept: {error}"))?;
        let content =
            tilo_actions::content_resource(&hash).ok_or("the attachment has no content address")?;
        Ok(Effected::Done(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "content": content,
                "name": name,
                "media_type": media_type,
                "size": bytes.len(),
            })),
            resources: vec![content],
        }))
    }

    /// Every message already loaded, outside Trash: its subject, sender and
    /// body come from the engine, not the document. And the contacts a
    /// draft can be prepared for.
    fn search_entries(&self) -> Vec<SearchEntry> {
        let mut entries = crate::search::message_entries(&self.shared_mailbox());
        entries.extend(self.contact_recipient_search_entries());
        entries
    }

    /// Fetching and preparing an invitation need a connected account.
    fn effect_statuses(&self) -> Vec<EffectStatus> {
        if self.can_sync() {
            return Vec::new();
        }
        vec![
            EffectStatus::unavailable(
                MailEffect::REFRESH,
                "account_offline",
                "No connected mail account is available",
            ),
            EffectStatus::unavailable(
                MailEffect::PREPARE_CALENDAR_INVITATION,
                "account_offline",
                "Connect a Mail account before preparing an invitation",
            ),
        ]
    }

    /// Stage screenshot `scenario` on the next sync, which has the document.
    pub(crate) fn request_stage(&mut self, scenario: &str) {
        self.pending_stage = Some(scenario.to_owned());
        self.dirty = true;
    }

    pub(crate) fn has_pending_send(&self) -> bool {
        !self.pending_send.is_empty()
    }

    /// The loaded mailbox, which search reads its messages from.
    pub(crate) fn shared_mailbox(&self) -> std::cell::Ref<'_, crate::model::Mailbox> {
        std::cell::Ref::map(self.shared.borrow(), |state| &state.mailbox)
    }
}

//! The view of the mailbox: what builds the [`MailApp`] window over the
//! framework, answers effects from the [`Mailroom`] it shares with it and
//! publishes what the mailbox does not hold.

use std::cell::RefCell;
use std::rc::Rc;

#[cfg(feature = "live")]
use accounts::person::PersonAccounts;
use ui::framework::{EffectAnswer, EffectCx, EffectStatus, Effected, Publishes, ViewCx};
use ui::portal::{FileFilter, PickRequest};
use ui::tilo_resource::SearchEntry;
use ui::widget::Element;

use super::{MailApp, Mailroom};
use crate::actions::{MailCheck, MailEffect, Outgoing, ServerUpdate};
use crate::i18n::CATALOG;
use crate::message_states::MessageStates;
use crate::resource::Mail;
use crate::state::MailState;

/// Carry out the checks for new mail that a window reaching no server asked,
/// when this window reaches the account's server; and have this window ask
/// those that do when it reaches none itself.
fn carry_checks(cx: &ViewCx<Mail>, window: &mut MailApp) {
    // A check for new mail a window that reaches no server asked: fetched
    // here when this window reaches the account's server, and what it
    // fetched reaches that window through the mailbox and the kept copies.
    let outbox = window.server_outbox();
    let (shared, carrier) = (Rc::clone(&window.shared), window.room());
    let checks = cx.outbox(
        Mail::paths().checks,
        move |_, check: &MailCheck| {
            carrier.borrow().reaches_servers() && holds_account(&shared.borrow(), &check.account)
        },
        move |_, check: MailCheck, cx| {
            let reply = cx.reply();
            outbox.borrow_mut().check(
                check,
                Box::new(move |result| {
                    reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
                }),
            );
            Ok(Effected::Pending)
        },
    );
    window.follow_checks(checks);
    // A live window that reaches no server itself asks those that do.
    let asks = cx.mode().is_live() && !window.state().reaches_servers();
    window.state().checks.asks = asks;
}

/// What the messages' servers are owed is read off the parts the messages
/// are kept in, and carried out by a window that reaches the account's
/// server; each confirmation lands in the part holding its message.
fn carry_owed(cx: &ViewCx<Mail>, window: &MailApp) {
    let Some(chain) = cx.chain::<MessageStates>() else {
        return;
    };
    window.shared.borrow_mut().chain = Some(chain.clone());
    let outbox = window.server_outbox();
    let reaches = reaches(window);
    cx.owed(
        &chain,
        crate::message_states::owed,
        move |update: &ServerUpdate| reaches(update.account()),
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
}

/// Whether this window reaches the server of an account now: it talks to
/// mail servers, and this device holds the account.
fn reaches(window: &MailApp) -> impl Fn(&str) -> bool + 'static {
    let (shared, carrier) = (Rc::clone(&window.shared), window.room());
    move |account| carrier.borrow().reaches_servers() && holds_account(&shared.borrow(), account)
}

fn holds_account(state: &MailState, address: &str) -> bool {
    state
        .accounts
        .accounts
        .iter()
        .any(|account| account.config.email == address)
}

fn carry_server_updates(cx: &ViewCx<Mail>, window: &mut MailApp) {
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
    carry_owed(cx, window);
    let outbox = window.server_outbox();
    let reaches = reaches(window);
    let delivery = cx.outbox(
        Mail::paths().outbox,
        move |_, outgoing: &Outgoing| reaches(&outgoing.account),
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
}

pub(crate) fn window(cx: &ViewCx<Mail>, mut window: MailApp) -> Element {
    window.live = Some(cx.live().clone());
    window.attach_framework(cx.dispatcher());
    let trust = cx.resource_opener::<crate::trusted_senders::TrustedSenders>(ui::apps::MAIL);
    let saved_copies = cx.resource_opener::<crate::copies::MailCopies>(ui::apps::MAIL);
    let mailbox = cx.resource_opener::<Mail>(ui::apps::MAIL);
    if cx.mode().is_live() {
        window.wake_from_workers(cx.waker());
        window.follow_preferences(cx.settings::<crate::resource::MailPreferences>());
        {
            let mut state = window.shared.borrow_mut();
            state.sender_trust = Some(trust);
            state.sync_trusted_senders();
        }
        window.state().contact_directory =
            cx.shared_person::<tilo_contact_directory::ContactDirectory>();
        // A page keeps no accounts of its own: it reads the ones the host
        // that runs its workspace keeps, through the host's mail session.
        #[cfg(feature = "live")]
        if let Some(accounts) = cx.shared_person::<PersonAccounts>() {
            window.follow_person_accounts(accounts);
        } else {
            tracing::warn!("the person's accounts are not open: Mail cannot follow them");
        }
        window.copies_opener = Some(saved_copies);
        window.open_peer_copies();
    }
    carry_server_updates(cx, &mut window);
    carry_checks(cx, &mut window);
    cx.on_open(window.opener());
    let shared = window.room();
    {
        let (answer, status) = (Rc::clone(&shared), Rc::clone(&shared));
        let import = cx.mode().is_live();
        let workspace_mailbox = cx
            .document_id()
            .is_some_and(ui::resource::DocumentId::is_package_primary);
        let import_chain = cx.chain::<MessageStates>();
        cx.effects(move |effect: MailEffect, cx| {
            if matches!(effect, MailEffect::ImportMailbox) {
                if !import || !workspace_mailbox {
                    return Err("a mailbox is imported from its workspace mailbox window".into());
                }
                return Ok(Effected::value(crate::mailbox_migration::import(
                    &mailbox, cx,
                )?));
            }
            if matches!(effect, MailEffect::ImportMessageState) {
                if !import || !workspace_mailbox {
                    return Err("messages are imported from the workspace mailbox".into());
                }
                let chain = import_chain
                    .as_ref()
                    .ok_or("the workspace messages are still arriving")?;
                return Ok(Effected::value(crate::mailbox_migration::import_messages(
                    &mailbox, chain, cx,
                )?));
            }
            answer_effect(&answer, effect, cx)
        })
        .status(move || status.borrow().effect_statuses());
    }
    {
        let staged = Rc::clone(&shared);
        cx.on_stage(move |scenario| {
            if let Some(scenario) = scenario {
                staged.borrow_mut().request_stage(scenario);
            }
        });
    }
    publish(cx, window, shared)
}

fn publish(cx: &ViewCx<Mail>, window: MailApp, shared: Rc<RefCell<Mailroom>>) -> Element {
    let (rows, row, entries, query, statuses) = (
        Rc::clone(&shared),
        Rc::clone(&shared),
        Rc::clone(&shared),
        Rc::clone(&shared),
        Rc::clone(&shared),
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
            // What opening a row shows, and an attachment whose bytes are not
            // here yet.
            .row_status(move |id| statuses.borrow().row_status(id))
            .search_entries(move || entries.borrow().search_entries())
            // Someone searches: the servers are asked too, for mail older
            // than what this device keeps; what they find joins the entries.
            .search_query(move |asked| query.borrow_mut().search_servers(asked.text()))
            // The servers are still looking: a search showing waits for what
            // they find.
            .search_pending(move || shared.borrow().searching_servers()),
        Box::new(window),
    )
}

fn draft_rows(mail: &Mail) -> Vec<ui::framework::LiveRow> {
    mail.drafts_listed()
        .into_iter()
        .map(|(id, draft)| crate::actions::draft_row(&id, draft))
        .collect()
}

fn draft_row(mail: &Mail, id: &str) -> Option<ui::framework::LiveRow> {
    let id = crate::resource::draft_named(id);
    let draft = mail.draft_at(Some(id))?;
    (!crate::resource::is_composer_draft(id) || mail.has_draft())
        .then(|| crate::actions::draft_row(id, draft))
}

/// In a page, a film, a recording or a file too large for an app here goes
/// from the server to the browser, which plays or keeps it as it streams; the
/// page never holds it. `false` when it stays for an app here.
fn carried_from_the_server(mailroom: &Mailroom, message: &str, index: usize) -> bool {
    let opening = mailroom.attachment_opening(message, index);
    if opening == super::PageOpening::Here || !ui::carry_out::available() {
        return false;
    }
    let Some(ask) = ui::tilo_resource::content::current()
        .and_then(|store| mailroom.attachment_link(message, index, &store))
    else {
        return false;
    };
    if opening == super::PageOpening::Shown {
        ui::carry_out::show(ask)
    } else {
        ui::carry_out::save(ask)
    }
}

fn answer_effect(
    mailroom: &Rc<RefCell<Mailroom>>,
    effect: MailEffect,
    cx: &mut EffectCx<Mail>,
) -> EffectAnswer {
    let messages = mailroom.borrow().shared.borrow().messages();
    if let Some(answer) = crate::held::effect(&effect, &cx.live().get(), &messages) {
        return answer;
    }
    // Messages are read, searched and answered from what this device keeps
    // of its servers and what the resource holds, whatever a window shows.
    match &effect {
        MailEffect::SearchMessages { .. } => {
            let query = crate::search::MessageQuery::of(&effect)?;
            return Ok(Effected::value(
                mailroom.borrow().search_synced(&messages, &query),
            ));
        }
        MailEffect::ReadMessage { message } => {
            let found = mailroom
                .borrow()
                .synced_message(&messages, message)
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
                mailroom,
                message,
                body,
                all.unwrap_or(false),
                notes.as_deref(),
                cx,
            );
        }
        MailEffect::Content { message, index } => {
            return mailroom
                .borrow()
                .attachment_content(&messages, message, *index);
        }
        MailEffect::OpenAttachment {
            message,
            index,
            group,
        } => {
            if carried_from_the_server(&mailroom.borrow(), message, *index) {
                return Ok(Effected::done());
            }
            let handed = mailroom
                .borrow()
                .attachment_content(&messages, message, *index)?;
            return open_on_desktop(handed, group.as_deref(), cx);
        }
        _ => {}
    }
    if let MailEffect::Save { message, index } = &effect {
        // Where the page carries files out of the workspace (a browser), a
        // file kept apart from its message streams from the server straight to
        // the person's disk, never through the page.
        if ui::carry_out::available()
            && let Some(store) = ui::tilo_resource::content::current()
            && let Some(ask) = mailroom.borrow().attachment_link(message, *index, &store)
        {
            // The browser's Save dialog and its download are what the
            // person watches; the page tells them when one fails.
            if ui::carry_out::save(ask) {
                return Ok(Effected::done());
            }
        }
        let (name, bytes) = mailroom
            .borrow()
            .attachment_to_save(message, *index)
            .map_err(|cause| cause.message())?;
        let resource = format!("mail.attachment:{message}:{index}");
        let (title, accept) = (
            CATALOG.t("mail-save-attachment"),
            CATALOG.t("mail-attachment-save-accept"),
        );
        let request = PickRequest::save(&title, &accept, &name)
            .thread_name("mail-portal-save-attachment")
            .filter(FileFilter::all_files());
        let mailroom = Rc::clone(mailroom);
        return Ok(cx.save_file(request, bytes, move |(), saved, _| {
            Ok(mailroom
                .borrow_mut()
                .finish_attachment_save(saved, &name, resource))
        }));
    }
    let mut mailroom = mailroom.borrow_mut();
    if matches!(effect, MailEffect::PrepareContactDraft { .. }) {
        mailroom.has_contact_draft_conflict = cx.live().get().has_draft();
    }
    let invocation = cx.invocation().unwrap_or_default();
    let answer = mailroom.begin_effect(invocation, effect)?;
    if matches!(answer, Effected::Pending) {
        mailroom.await_effect(invocation, cx.reply());
    }
    mailroom.dirty = true;
    Ok(answer)
}

fn open_on_desktop(handed: Effected, group: Option<&str>, cx: &mut EffectCx<Mail>) -> EffectAnswer {
    use ui::shell::{OPEN, PROVIDER, RESOURCE};
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

fn no_such_message(message: &str) -> String {
    format!("no message `{message}` is kept here: search-messages lists the messages there are")
}

fn draft_reply(
    mailroom: &Rc<RefCell<Mailroom>>,
    message: &str,
    body: &str,
    all: bool,
    notes: Option<&str>,
    cx: &EffectCx<Mail>,
) -> EffectAnswer {
    let mailroom = mailroom.borrow();
    let messages = mailroom.shared.borrow().messages();
    let answered = mailroom
        .synced_message(&messages, message)
        .ok_or_else(|| no_such_message(message))?;
    let own = mailroom.shared.borrow().own_addresses();
    let mut drafted = crate::held::Drafted::reply(&answered, body, all, &own);
    notes.unwrap_or("").clone_into(&mut drafted.notes);
    drafted.sender = mailroom.sender_for(&answered.account);
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
    // claims a draft the resource did not take.
    let mut probe = (*cx.live().get()).clone();
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
    pub(crate) const fn reaches_servers(&self) -> bool {
        self.engine.is_some()
    }

    pub(crate) fn synced_message(
        &self,
        mail: &MessageStates,
        item: &str,
    ) -> Option<mail_engine::model::Message> {
        if let Some(held) = mail.held_shown(item) {
            return Some(held);
        }
        let id = crate::held::message_named(item.trim())?;
        if mail.knows(id) && mail.message(id).is_none() {
            return None;
        }
        let shared = self.shared.borrow();
        let id = id.parse().ok()?;
        shared.may_read_message(id).then_some(())?;
        shared.mailbox.get(id).cloned()
    }

    pub(crate) fn search_synced(
        &self,
        mail: &MessageStates,
        query: &crate::search::MessageQuery,
    ) -> serde_json::Value {
        let shared = self.shared.borrow();
        let held: Vec<mail_engine::model::Message> = mail
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
                    shared.may_read_message(message.id)
                        && (!mail.knows(&id) || mail.message(&id).is_some())
                })
                .chain(held.iter()),
        )
    }

    pub(crate) fn sender_for(&self, address: &str) -> Option<mail_engine::model::DraftSender> {
        let shared = self.shared.borrow();
        shared
            .accounts
            .accounts
            .iter()
            .find(|account| !address.is_empty() && account.config.email == address)
            .map(|account| mail_engine::model::DraftSender {
                account: account.id.clone(),
                email: account.config.email.clone(),
            })
    }

    pub(crate) fn attachment_content(
        &self,
        mail: &MessageStates,
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
        let store = ui::tilo_resource::content::current()
            .ok_or("this window keeps no content to hand the attachment over in")?;
        // Kept a part at a time: an attachment of any size is handed over
        // as content, never whole in memory or in one piece of content.
        let (name, kept) = self
            .keep_attachment(&found.id.to_string(), index, &store)
            .map_err(|cause| cause.message())?;
        let content = tilo_actions::content_resource(&kept.hash)
            .ok_or("the attachment has no content address")?;
        Ok(Effected::Done(tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "content": content,
                "name": name,
                "media_type": media_type,
                "size": kept.len,
            })),
            resources: vec![content],
        }))
    }

    fn search_entries(&self) -> Vec<SearchEntry> {
        let shared = self.shared.borrow();
        let mut entries: Vec<_> = shared
            .mailbox
            .messages
            .values()
            .filter(|message| {
                message.folder != mail_engine::model::Folder::Trash
                    && !shared.workspace_messages.contains(&message.id)
            })
            .map(crate::search::message_entry)
            .collect();
        drop(shared);
        entries.extend(self.contact_recipient_search_entries());
        entries
    }

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

    pub(crate) fn request_stage(&mut self, scenario: &str) {
        self.pending_stage = Some(scenario.to_owned());
        self.dirty = true;
    }

    pub(crate) fn has_pending_send(&self) -> bool {
        !self.pending_send.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn shared_mailbox(&self) -> std::cell::Ref<'_, mail_engine::model::Mailbox> {
        std::cell::Ref::map(self.shared.borrow(), |state| &state.mailbox)
    }
}

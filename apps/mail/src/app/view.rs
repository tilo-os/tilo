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
    let outbox = window.server_outbox();
    let shared = Rc::clone(&window.shared);
    let projection = cx.outbox(
        Mail::paths().server,
        move |_, update: &ServerUpdate| holds_account(&shared.borrow(), update.account()),
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
    let shared = Rc::clone(&window.shared);
    let delivery = cx.outbox(
        Mail::paths().outbox,
        move |_, outgoing: &Outgoing| holds_account(&shared.borrow(), &outgoing.account),
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
    cx.publishing(
        Publishes::new()
            // The accounts, an in-flight send and a search-index fault.
            .live_rows(move || rows.borrow().live_rows())
            // A loaded message, thread, attachment or account the mailbox
            // does not publish.
            .live_row(move |id| row.borrow().live_row(id))
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

impl Mailroom {
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

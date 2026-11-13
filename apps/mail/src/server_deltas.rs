//! What the account's server changed since this device last looked: the
//! messages a folder no longer holds, the ones another client read or
//! starred, and a server search's finds joining what is held here.
//!
//! Kept apart from [`crate::app`] so the rules that decide what a sync
//! deletes or rewrites are readable without the widget around them.

use std::collections::{HashMap, HashSet};

use crate::model::{AccountConfig, Folder, Message, MessageId};
use crate::state::MailState;

pub(super) fn server_changes(
    mailbox: &crate::model::Mailbox,
    account_id: &str,
    folder: Folder,
    synced: &crate::sync::Synced,
) -> (Vec<MessageId>, Vec<Message>) {
    let held_here = |message: &Message| {
        message.account == account_id
            && message.folder == folder
            && message.server.is_some_and(|at| at.folder == folder)
    };
    // What the server no longer holds goes: everything of a folder that
    // started over and is not in its new page, and every message of the
    // known window its server did not report.
    let mut gone: Vec<MessageId> = Vec::new();
    if synced.reset {
        let fresh: std::collections::HashSet<MessageId> =
            synced.messages.iter().map(|message| message.id).collect();
        gone.extend(
            mailbox
                .of_account(account_id)
                .filter(|message| held_here(message) && !fresh.contains(&message.id))
                .map(|message| message.id),
        );
    }
    let mut reflagged: Vec<Message> = Vec::new();
    if let Some((oldest, newest)) = synced.window {
        let reported: HashMap<u32, (bool, bool)> = synced
            .flags
            .iter()
            .map(|(uid, seen, flagged)| (*uid, (*seen, *flagged)))
            .collect();
        for message in mailbox.of_account(account_id).filter(|m| held_here(m)) {
            let Some(uid) = message.server.map(|at| at.uid) else {
                continue;
            };
            if uid < oldest || uid > newest {
                continue;
            }
            match reported.get(&uid) {
                None => gone.push(message.id),
                // Another client read or starred it: the record decides
                // whether that or a change made here wins.
                Some(&(seen, flagged)) if seen != message.read || flagged != message.starred => {
                    let mut reported = message.clone();
                    reported.read = seen;
                    reported.starred = flagged;
                    reflagged.push(reported);
                }
                Some(_) => {}
            }
        }
    }
    (gone, reflagged)
}

fn draft_copies(mail: &crate::resource::Mail) -> HashSet<MessageId> {
    mail.draft_copy_ids()
        .iter()
        .filter_map(|id| id.parse().ok())
        .collect()
}

pub(super) fn pass_over_draft_copies(
    st: &mut MailState,
    mail: &crate::resource::Mail,
    account_id: &str,
    folder: Folder,
    synced: &mut crate::sync::Synced,
) {
    let copies = draft_copies(mail);
    if copies.is_empty() {
        return;
    }
    let (passed, kept): (Vec<Message>, Vec<Message>) = std::mem::take(&mut synced.messages)
        .into_iter()
        .partition(|message| copies.contains(&message.id));
    synced.messages = kept;
    if passed.is_empty() {
        return;
    }
    synced.originals.retain(|(id, _)| !copies.contains(id));
    if let Some(window) = crate::sync::Window::of_messages(&passed, folder) {
        let key = (account_id.to_owned(), folder);
        let earlier = st.passed_over.get(&key).copied();
        st.passed_over.insert(key, window.with(earlier));
    }
}

pub(super) fn take_found(
    doc: &mut ui::widget::Edits<'_>,
    st: &mut MailState,
    account_id: &str,
    messages: Vec<Message>,
) {
    let copies = draft_copies(&crate::resource::read_mail(doc.doc()).unwrap_or_default());
    let fresh: Vec<Message> = messages
        .into_iter()
        .filter(|message| st.mailbox.get(message.id).is_none() && !copies.contains(&message.id))
        .collect();
    st.ingest_fetched(doc, account_id, &fresh);
    for message in fresh {
        st.mailbox.insert(message);
    }
}

pub(super) fn first_fetch(config: &AccountConfig) -> ui::arriving::ArrivalClock {
    ui::arriving::ArrivalClock::new(
        ui::time::now(),
        ui::arriving::Source::host(config.imap_host.clone()),
    )
}

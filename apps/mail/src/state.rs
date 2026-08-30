//! Shared, render-free mail state, owned by the app and borrowed by each of the
//! three [`crate::pane::Pane`]s through an `Rc<RefCell<_>>`.
//!
//! The panes are siblings inside a [`ui::widget::NavShell`], so they can't call
//! each other directly. Instead they mutate this shared state and raise
//! [`Intent`]s; after routing an event through the shell the app drains the
//! intents and drives the cross-pane effects (push the detail pane, open the
//! composer, post an engine request, …).

use std::cell::RefCell;
use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use vello::kurbo::Point;

use ui::{ActionHost, ImageStore};

use crate::account::AccountStore;
use crate::actions::{
    MAIL_ACTION_TYPE, MailAction, move_action, save_draft_action, set_read_action,
    set_starred_action,
};
use crate::doc::MailDoc;
use crate::engine::EngineRequest;
use crate::model::{Draft, Folder, Mailbox, Message, MessageId, Thread};

/// Cheap-to-clone handle every pane holds.
pub type Shared = Rc<RefCell<MailState>>;

/// A single message action, shared as the source of truth by the row context
/// menu, the swipe gestures, and the reader's action bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CtxAction {
    Reply,
    ReplyAll,
    Forward,
    ToggleRead,
    ToggleStar,
    Archive,
    Delete,
    MoveToInbox,
    Junk,
}

impl CtxAction {
    pub fn label(self, read: bool, starred: bool) -> String {
        match self {
            CtxAction::Reply => "Reply".into(),
            CtxAction::ReplyAll => "Reply all".into(),
            CtxAction::Forward => "Forward".into(),
            CtxAction::ToggleRead => {
                if read {
                    "Mark unread".into()
                } else {
                    "Mark read".into()
                }
            }
            CtxAction::ToggleStar => {
                if starred {
                    "Remove star".into()
                } else {
                    "Star".into()
                }
            }
            CtxAction::Archive => "Archive".into(),
            CtxAction::Delete => "Delete".into(),
            CtxAction::MoveToInbox => "Move to Inbox".into(),
            CtxAction::Junk => "Mark as junk".into(),
        }
    }
}

/// A cross-pane effect a pane wants the app to perform after event routing.
#[derive(Clone, Debug)]
pub enum Intent {
    /// A message was opened — push the detail pane (narrow) / keep it in view.
    ShowDetail,
    /// Return to the overview (e.g. the open message was archived on narrow).
    ShowOverview,
    /// A folder/account was chosen from the narrow drawer — close it.
    CloseDrawer,
    /// Open a blank composer.
    Compose,
    /// Open a reply / reply-all / forward composer for a message.
    Reply(MessageId),
    ReplyAll(MessageId),
    Forward(MessageId),
    /// Open an existing draft (a message in the Drafts folder) for editing.
    EditDraft(MessageId),
    /// Open the row context menu at a point.
    ContextMenu(MessageId, Point),
    /// Open the account setup sheet for the given account index (`None` = add).
    Settings(Option<usize>),
    /// Ask the app to post a request to the engine worker.
    Engine(EngineRequest),
    /// Surface a transient status toast.
    Toast(String),
}

/// The whole app's mutable state, minus the transient overlays (composer,
/// context menu, setup sheet) which the app owns directly.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (shell-derived layout flags the app writes, focus/seed bookkeeping, and the repaint flag); grouping them would need a real design pass, left for a follow-up rather than forced here"
)]
pub struct MailState {
    pub mailbox: Mailbox,
    pub accounts: AccountStore,
    /// "Now" in minutes since the epoch, for relative timestamps. Pinned in the
    /// sample corpus; wall-clock in the live app.
    pub now_min: i64,
    pub folder: Folder,
    pub selected: Option<MessageId>,
    pub search: String,
    pub status: Option<String>,

    /// Remote avatar loader (Gravatar). No-op fetcher unless built `live`.
    pub images: ImageStore,

    // --- shell-derived flags the app writes each layout, panes read ---
    pub narrow: bool,
    pub detail_pushed: bool,
    /// Whether the shell's sidebar is currently revealed. The list pane reads
    /// this so it can reserve room for the collapse/hamburger toggle when the
    /// sidebar is closed (and the list becomes the leftmost pane, with the
    /// toggle over its header).
    pub sidebar_open: bool,
    /// The list pane owns the search field; it publishes focus here so the app
    /// knows whether a keyboard shortcut should fire.
    pub search_focused: bool,
    /// Rising-edge request (from the `/` shortcut) for the list to focus search.
    pub focus_search_request: bool,

    /// Whether the CRDT has been seeded from this corpus yet.
    pub seeded: bool,
    /// Cursor in the app's logical coordinate space, for hover feedback.
    pub last_mouse: Point,
    /// Any pane sets this to request a repaint.
    pub dirty: bool,

    /// Framework-owned action dispatch: every document-mutating message
    /// operation (mark read/unread, star, move/archive/delete-to-trash) routes
    /// through here so it brackets one undo boundary, replays byte-identically on
    /// peers, and shares its [`crate::actions::MailAction`] with the Ctrl+K
    /// palette. See [`crate::actions`].
    pub actions: ActionHost,

    intents: Vec<Intent>,
}

impl MailState {
    pub fn shared(state: MailState) -> Shared {
        Rc::new(RefCell::new(state))
    }

    pub fn new_sample(images: ImageStore) -> Self {
        let mailbox = crate::sample::sample_mailbox();
        let accounts = crate::sample::sample_account_store();
        let mut s = Self::empty(mailbox, accounts, crate::sample::SAMPLE_NOW_MIN, images);
        // Preselect a conversation so the reader is populated on launch.
        let threads = s.current_threads();
        let pick = threads
            .iter()
            .find(|t| t.message_ids.len() > 1)
            .or_else(|| threads.first());
        if let Some(&newest) = pick.and_then(|t| t.message_ids.last()) {
            s.selected = Some(newest);
            s.mailbox.mark_read(newest, true);
        }
        s
    }

    /// The live app: real account list, empty corpus until the engine fetches.
    /// No fabricated messages.
    pub fn new_live(accounts: AccountStore, images: ImageStore) -> Self {
        let cfg = accounts.active_config();
        Self::empty(Mailbox::new(cfg), accounts, now_minutes(), images)
    }

    fn empty(mailbox: Mailbox, accounts: AccountStore, now_min: i64, images: ImageStore) -> Self {
        Self {
            mailbox,
            accounts,
            now_min,
            folder: Folder::Inbox,
            selected: None,
            search: String::new(),
            status: None,
            images,
            narrow: false,
            detail_pushed: false,
            sidebar_open: true,
            search_focused: false,
            focus_search_request: false,
            seeded: false,
            last_mouse: Point::ZERO,
            dirty: true,
            actions: ActionHost::new(),
            intents: Vec::new(),
        }
    }

    /// Hand the framework host the heads captured before this event's first
    /// message action, so the app records one undo boundary bracketing it. Wired
    /// through [`ui::widget::Widget::take_committed_action`]. Rising-edge.
    pub fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    /// Dispatch one [`MailAction`] — the single CRDT-mutation path shared with the
    /// Ctrl+K palette. The in-memory mailbox is updated by the caller for
    /// immediate feedback and reconciled from the document on the next `sync`.
    fn dispatch_action(&mut self, doc: &mut AutoCommit, action: &MailAction) {
        self.actions.dispatch(doc, MAIL_ACTION_TYPE, action);
    }

    /// Persist the in-progress draft as an action (undo boundary + replay + the
    /// same [`MailAction`] the palette's draft actions use). The visible Drafts
    /// message is materialised separately by the app.
    pub fn save_draft(&mut self, doc: &mut AutoCommit, draft: &Draft) {
        self.dispatch_action(doc, &save_draft_action(draft));
    }

    // ---- intents ----

    pub fn push_intent(&mut self, i: Intent) {
        self.intents.push(i);
        self.dirty = true;
    }

    pub fn take_intents(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.intents)
    }

    // ---- derived views ----

    pub fn current_threads(&self) -> Vec<Thread> {
        self.mailbox.threads_in(self.folder)
    }

    /// Rows to show in the list: `(message_id, thread_len)`. Search results are
    /// flat (one row per hit); otherwise one row per thread (its newest msg).
    pub fn list_rows(&self) -> Vec<(MessageId, usize)> {
        if self.search.trim().is_empty() {
            self.current_threads()
                .into_iter()
                .filter_map(|t| {
                    t.message_ids
                        .last()
                        .copied()
                        .map(|id| (id, t.message_ids.len()))
                })
                .collect()
        } else {
            self.mailbox
                .search(&self.search, None)
                .into_iter()
                .map(|m| (m.id, 1))
                .collect()
        }
    }

    pub fn selected_thread_messages(&self) -> Vec<&crate::model::Message> {
        let Some(sel) = self.selected.and_then(|id| self.mailbox.get(id)) else {
            return Vec::new();
        };
        let subj = sel.thread_subject();
        let mut v: Vec<&crate::model::Message> = self
            .mailbox
            .messages
            .values()
            .filter(|m| m.thread_subject() == subj && m.folder == sel.folder)
            .collect();
        v.sort_by_key(|m| m.timestamp_min);
        v
    }

    // ---- mutations (write through to the CRDT) ----

    pub fn persist_flags(&self, doc: &mut AutoCommit, id: MessageId) {
        if let Some(m) = self.mailbox.get(id) {
            MailDoc::store_flags(doc, &m.clone());
        }
    }

    pub fn select_message(&mut self, doc: &mut AutoCommit, id: MessageId) {
        let was_unread = self.mailbox.get(id).is_some_and(|m| !m.read);
        self.selected = Some(id);
        self.mailbox.mark_read(id, true);
        self.persist_flags(doc, id);
        MailDoc::set_selected(doc, Some(&id.to_string()));
        // Reading a message flips \Seen on the server (no-op offline).
        if was_unread {
            self.sync_flag_to_server(id);
        }
        self.push_intent(Intent::ShowDetail);
    }

    /// Push a live-path request to mirror a message's flags to the server
    /// (`UID STORE`). A no-op under the offline stub backend, but it means the
    /// mark-read/star state genuinely reaches IMAP in the `live` build. Called
    /// after the local flag mutation so the pushed message carries the new
    /// state (and its current, source folder for the `SELECT`).
    fn sync_flag_to_server(&mut self, id: MessageId) {
        let Some(msg) = self.mailbox.get(id).cloned() else {
            return;
        };
        let account = self.accounts.active_config();
        if account.email.is_empty() {
            return;
        }
        self.push_intent(Intent::Engine(EngineRequest::StoreFlag {
            account,
            message: Box::new(msg),
        }));
    }

    /// Push a live-path request to move a message to another folder
    /// (`UID MOVE`). A no-op under the offline stub backend. `source` is the
    /// message as it was **before** the local move, so the live backend can
    /// `SELECT` its origin mailbox and locate it by `Message-ID`.
    fn sync_move_to_server(&mut self, source: Message, to: Folder) {
        let account = self.accounts.active_config();
        if account.email.is_empty() {
            return;
        }
        self.push_intent(Intent::Engine(EngineRequest::Move {
            account,
            message: Box::new(source),
            to,
        }));
    }

    pub fn set_active_account(&mut self, doc: &mut AutoCommit, idx: usize) {
        self.accounts.set_active(idx);
        MailDoc::set_active_account(doc, self.accounts.active);
    }

    pub fn toggle_account_collapsed(&mut self, idx: usize) {
        self.accounts.toggle_collapsed(idx);
        self.dirty = true;
    }

    pub fn select_folder(&mut self, doc: &mut AutoCommit, folder: Folder) {
        self.folder = folder;
        self.search.clear();
        MailDoc::set_folder(doc, folder);
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        match self.selected {
            Some(sel) => MailDoc::set_selected(doc, Some(&sel.to_string())),
            None => MailDoc::set_selected(doc, None),
        }
        // Ask the live engine to fetch this folder (a no-op offline).
        let account = self.accounts.active_config();
        if !account.email.is_empty() {
            self.push_intent(Intent::Engine(EngineRequest::Fetch { account, folder }));
        }
        self.dirty = true;
    }

    pub fn reselect_after_remove(&mut self, doc: &mut AutoCommit) {
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        match self.selected {
            Some(sel) => MailDoc::set_selected(doc, Some(&sel.to_string())),
            None => MailDoc::set_selected(doc, None),
        }
        self.dirty = true;
    }

    pub fn context_items_for(&self, id: MessageId) -> Vec<CtxAction> {
        let in_inbox = self
            .mailbox
            .get(id)
            .is_none_or(|m| m.folder == Folder::Inbox);
        let mut items = vec![
            CtxAction::Reply,
            CtxAction::ReplyAll,
            CtxAction::Forward,
            CtxAction::ToggleRead,
            CtxAction::ToggleStar,
            CtxAction::Archive,
            CtxAction::Delete,
        ];
        if !in_inbox {
            items.push(CtxAction::MoveToInbox);
        }
        items.push(CtxAction::Junk);
        items
    }

    // ---- action-routed message mutations ----
    //
    // Each mutates the in-memory mailbox for immediate feedback AND dispatches
    // the matching [`MailAction`] through the framework host, so the flag change
    // is one undo boundary, replays byte-identically on peers, and is the same
    // action the Ctrl+K palette dispatches. The `sync_*_to_server` hooks remain
    // an *external* effect (IMAP UID STORE/MOVE), separate from the CRDT action.

    /// Set a message's read flag through the action path.
    pub fn set_read(&mut self, doc: &mut AutoCommit, id: MessageId, read: bool) {
        if self.mailbox.get(id).is_none() {
            return;
        }
        self.mailbox.mark_read(id, read);
        self.dispatch_action(doc, &set_read_action(vec![id.to_string()], read));
        self.sync_flag_to_server(id);
        self.dirty = true;
    }

    /// Toggle a message's star through the action path.
    pub fn toggle_star(&mut self, doc: &mut AutoCommit, id: MessageId) {
        let Some(target) = self.mailbox.get(id).map(|m| !m.starred) else {
            return;
        };
        self.mailbox.toggle_star(id);
        self.dispatch_action(doc, &set_starred_action(vec![id.to_string()], target));
        self.sync_flag_to_server(id);
        self.dirty = true;
    }

    /// Move a message to `folder` through the action path.
    pub fn move_message(&mut self, doc: &mut AutoCommit, id: MessageId, folder: Folder) {
        let Some(source) = self.mailbox.get(id).cloned() else {
            return;
        };
        self.mailbox.move_to(id, folder);
        self.dispatch_action(doc, &move_action(vec![id.to_string()], folder));
        self.sync_move_to_server(source, folder);
    }

    /// Apply a message action. Reply/forward raise an intent (the app owns the
    /// composer); the rest route through the action path above.
    pub fn apply_message_action(&mut self, doc: &mut AutoCommit, id: MessageId, action: CtxAction) {
        match action {
            CtxAction::Reply => self.push_intent(Intent::Reply(id)),
            CtxAction::ReplyAll => self.push_intent(Intent::ReplyAll(id)),
            CtxAction::Forward => self.push_intent(Intent::Forward(id)),
            CtxAction::ToggleRead => {
                let now = self.mailbox.get(id).is_some_and(|m| m.read);
                self.set_read(doc, id, !now);
            }
            CtxAction::ToggleStar => {
                self.toggle_star(doc, id);
            }
            CtxAction::Archive => {
                let was_selected = self.selected == Some(id);
                self.move_message(doc, id, Folder::Archive);
                self.after_remove(doc, was_selected);
            }
            CtxAction::Delete => {
                let was_selected = self.selected == Some(id);
                // Snapshot the source before the local move so a live UID MOVE
                // can find it in its origin mailbox.
                let source = self.mailbox.get(id).cloned();
                // Move to Trash, or purge if it was already there. The move is a
                // replayable action; a purge drops the in-memory corpus copy only
                // (message content never lived in the CRDT), so it is not one.
                self.mailbox.delete(id);
                let moved_to = self.mailbox.get(id).map(|m| m.folder);
                if let Some(to) = moved_to {
                    self.dispatch_action(doc, &move_action(vec![id.to_string()], to));
                    if let Some(src) = source {
                        self.sync_move_to_server(src, to);
                    }
                }
                self.after_remove(doc, was_selected);
            }
            CtxAction::MoveToInbox => {
                let was_selected = self.selected == Some(id);
                self.move_message(doc, id, Folder::Inbox);
                self.after_remove(doc, was_selected);
            }
            CtxAction::Junk => {
                let was_selected = self.selected == Some(id);
                self.move_message(doc, id, Folder::Junk);
                self.after_remove(doc, was_selected);
            }
        }
    }

    fn after_remove(&mut self, doc: &mut AutoCommit, was_selected: bool) {
        self.reselect_after_remove(doc);
        // If the open message left the current folder while its detail was
        // pushed on narrow, return to the list.
        if was_selected && self.narrow && self.detail_pushed {
            self.push_intent(Intent::ShowOverview);
        }
    }

    pub fn move_selection(&mut self, doc: &mut AutoCommit, delta: i32) {
        let rows = self.list_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self
            .selected
            .and_then(|sel| {
                rows.iter().position(|(id, _)| {
                    *id == sel
                        || self
                            .mailbox
                            .get(sel)
                            .map(super::model::Message::thread_subject)
                            == self
                                .mailbox
                                .get(*id)
                                .map(super::model::Message::thread_subject)
                })
            })
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, rows.len() as i32 - 1) as usize;
        // `next` is clamped into range above, but the emptiness guard that
        // makes that clamp valid is 20 lines up — keep the lookup total here.
        let Some(row) = rows.get(next) else {
            return;
        };
        let id = row.0;
        // Browsing the list with the arrows updates the reader but does not
        // push the narrow detail (that's a tap).
        let was_unread = self.mailbox.get(id).is_some_and(|m| !m.read);
        self.selected = Some(id);
        self.mailbox.mark_read(id, true);
        self.persist_flags(doc, id);
        MailDoc::set_selected(doc, Some(&id.to_string()));
        if was_unread {
            self.sync_flag_to_server(id);
        }
        self.dirty = true;
    }
}

/// Wall-clock "now" in minutes since the Unix epoch.
pub fn now_minutes() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| (d.as_secs() / 60) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineRequest;

    fn state() -> MailState {
        MailState::new_sample(crate::app::avatar_store_offline())
    }

    fn first_inbox_id(st: &MailState) -> Option<MessageId> {
        st.mailbox.messages_in(Folder::Inbox).first().map(|m| m.id)
    }

    #[test]
    fn star_action_emits_server_store_flag() {
        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        st.apply_message_action(&mut doc, id, CtxAction::ToggleStar);
        let intents = st.take_intents();
        assert!(
            intents
                .iter()
                .any(|i| matches!(i, Intent::Engine(EngineRequest::StoreFlag { .. }))),
            "starring must post a StoreFlag to the engine (UID STORE in the live build)"
        );
    }

    #[test]
    fn archive_action_emits_server_move_to_archive() {
        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        st.apply_message_action(&mut doc, id, CtxAction::Archive);
        let intents = st.take_intents();
        assert!(
            intents.iter().any(|i| matches!(
                i,
                Intent::Engine(EngineRequest::Move {
                    to: Folder::Archive,
                    ..
                })
            )),
            "archiving must post a Move to Archive to the engine (UID MOVE in live)"
        );
    }

    #[test]
    fn junk_action_emits_server_move_to_junk() {
        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        st.apply_message_action(&mut doc, id, CtxAction::Junk);
        let intents = st.take_intents();
        assert!(intents.iter().any(|i| matches!(
            i,
            Intent::Engine(EngineRequest::Move {
                to: Folder::Junk,
                ..
            })
        )));
    }

    #[test]
    fn reply_all_action_raises_distinct_reply_all_intent() {
        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        st.apply_message_action(&mut doc, id, CtxAction::ReplyAll);
        let intents = st.take_intents();
        assert!(
            intents
                .iter()
                .any(|i| matches!(i, Intent::ReplyAll(mid) if *mid == id)),
            "Reply-all must raise ReplyAll, not the plain Reply intent"
        );
        assert!(!intents.iter().any(|i| matches!(i, Intent::Reply(_))));
    }
}

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

use automerge::AutoCommit;
use vello::kurbo::Point;

use ui::ImageStore;

use crate::account::AccountStore;
use crate::doc::MailDoc;
use crate::engine::EngineRequest;
use crate::model::{Folder, Mailbox, MessageId, Thread};

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
            CtxAction::ToggleRead => if read { "Mark unread".into() } else { "Mark read".into() },
            CtxAction::ToggleStar => if starred { "Remove star".into() } else { "Star".into() },
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
    /// Open a reply / forward composer for a message.
    Reply(MessageId),
    Forward(MessageId),
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
            search_focused: false,
            focus_search_request: false,
            seeded: false,
            last_mouse: Point::ZERO,
            dirty: true,
            intents: Vec::new(),
        }
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
        if !self.search.trim().is_empty() {
            self.mailbox
                .search(&self.search, None)
                .into_iter()
                .map(|m| (m.id, 1))
                .collect()
        } else {
            self.current_threads()
                .into_iter()
                .filter_map(|t| t.message_ids.last().copied().map(|id| (id, t.message_ids.len())))
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
        self.selected = Some(id);
        self.mailbox.mark_read(id, true);
        self.persist_flags(doc, id);
        MailDoc::set_selected(doc, Some(&id.to_string()));
        self.push_intent(Intent::ShowDetail);
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
        let account_id = self.accounts.active_config().email;
        self.push_intent(Intent::Engine(EngineRequest::Fetch { account_id, folder }));
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
        let in_inbox = self.mailbox.get(id).map(|m| m.folder == Folder::Inbox).unwrap_or(true);
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

    /// Apply a message action. Reply/forward raise an intent (the app owns the
    /// composer); the rest mutate + persist here.
    pub fn apply_message_action(&mut self, doc: &mut AutoCommit, id: MessageId, action: CtxAction) {
        match action {
            CtxAction::Reply | CtxAction::ReplyAll => self.push_intent(Intent::Reply(id)),
            CtxAction::Forward => self.push_intent(Intent::Forward(id)),
            CtxAction::ToggleRead => {
                let now = self.mailbox.get(id).map(|m| m.read).unwrap_or(false);
                self.mailbox.mark_read(id, !now);
                self.persist_flags(doc, id);
                self.dirty = true;
            }
            CtxAction::ToggleStar => {
                self.mailbox.toggle_star(id);
                self.persist_flags(doc, id);
                self.dirty = true;
            }
            CtxAction::Archive => {
                let was_selected = self.selected == Some(id);
                self.mailbox.archive(id);
                self.persist_flags(doc, id);
                self.after_remove(doc, was_selected);
            }
            CtxAction::Delete => {
                let was_selected = self.selected == Some(id);
                self.mailbox.delete(id);
                self.persist_flags(doc, id);
                self.after_remove(doc, was_selected);
            }
            CtxAction::MoveToInbox => {
                self.mailbox.move_to(id, Folder::Inbox);
                self.persist_flags(doc, id);
                let was_selected = self.selected == Some(id);
                self.after_remove(doc, was_selected);
            }
            CtxAction::Junk => {
                let was_selected = self.selected == Some(id);
                self.mailbox.move_to(id, Folder::Junk);
                self.persist_flags(doc, id);
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
                        || self.mailbox.get(sel).map(|m| m.thread_subject())
                            == self.mailbox.get(*id).map(|x| x.thread_subject())
                })
            })
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, rows.len() as i32 - 1) as usize;
        let id = rows[next].0;
        // Browsing the list with the arrows updates the reader but does not
        // push the narrow detail (that's a tap).
        self.selected = Some(id);
        self.mailbox.mark_read(id, true);
        self.persist_flags(doc, id);
        MailDoc::set_selected(doc, Some(&id.to_string()));
        self.dirty = true;
    }
}

/// Wall-clock "now" in minutes since the Unix epoch.
pub fn now_minutes() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 60) as i64)
        .unwrap_or(0)
}

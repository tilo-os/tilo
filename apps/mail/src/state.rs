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

#[cfg(test)]
use automerge::AutoCommit;
use vello::kurbo::{Point, Rect};

use ui::ImageStore;
use ui::icons::Icon;

use crate::account::AccountStore;
use ui::framework::Dispatcher;

use crate::actions::{
    MailAction, move_action, purge_action, save_draft_action, set_read_action, set_starred_action,
};
use crate::engine::EngineRequest;
use crate::model::{Draft, Folder, Mailbox, MessageId, Thread};
use crate::resource::Mail;

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

/// A deliberately small set of inbox-triage operations that can act on a
/// local selection.  The selection itself is presentation state; every actual
/// mailbox change below is still emitted as a bounded canonical `MailAction`
/// so it converges, is undoable, and reaches the IMAP mirror exactly as the
/// corresponding one-message control does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BulkAction {
    Archive,
    /// A reversible move. It intentionally leaves rows already in Trash
    /// alone; permanent removal must use the separately named `Purge` action.
    Delete,
    /// Permanently delete only messages already in Trash. This is never the
    /// fallback meaning of `Delete`, including in virtual mixed-folder views.
    Purge,
    MarkRead,
    MarkUnread,
}

impl BulkAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Archive => "Archive",
            Self::Delete => "Move to Trash",
            Self::Purge => "Delete permanently",
            Self::MarkRead => "Mark read",
            Self::MarkUnread => "Mark unread",
        }
    }
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

    /// Shared visual treatment for the reader toolbar and its context menu.
    /// Keeping the icon mapping beside the action makes a command mean the
    /// same thing whether it is invoked by pointer, keyboard, or AT. `read`
    /// is the message's state: marking it unread closes the envelope,
    /// marking it read opens it.
    pub fn icon(self, read: bool) -> Icon {
        match self {
            CtxAction::Reply => Icon::Reply,
            CtxAction::ReplyAll => Icon::ReplyAll,
            CtxAction::Forward => Icon::Forward,
            CtxAction::ToggleRead if read => Icon::Mail,
            CtxAction::ToggleRead => Icon::MailOpen,
            CtxAction::ToggleStar => Icon::Star,
            CtxAction::Archive => Icon::Archive,
            CtxAction::Delete => Icon::Trash,
            CtxAction::MoveToInbox => Icon::Inbox,
            CtxAction::Junk => Icon::Ban,
        }
    }

    /// A visible hint only — command dispatch remains in `MailApp` so these
    /// shortcuts never bypass the collaborative action boundary.
    pub fn shortcut(self) -> Option<&'static str> {
        match self {
            CtxAction::Reply => Some("R"),
            CtxAction::ToggleRead => Some("U"),
            CtxAction::ToggleStar => Some("S"),
            CtxAction::Archive => Some("E"),
            CtxAction::Delete => Some("Del"),
            CtxAction::ReplyAll | CtxAction::Forward | CtxAction::MoveToInbox | CtxAction::Junk => {
                None
            }
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
    /// Open the row menu at a point, beside the row it acts on.
    ContextMenu(MessageId, Point, Rect),
    /// Open the account setup sheet for the given account index (`None` = add).
    Settings(Option<usize>),
    /// Ask the app-owned portal boundary to save one hydrated attachment.
    /// Panes intentionally carry only a stable message id + attachment index;
    /// the root revalidates both before it opens the host chooser.
    SaveAttachment(MessageId, usize),
    /// Ask the app to post a request to the engine worker.
    Engine(EngineRequest),
    /// Surface a transient status toast.
    Toast(String),
    /// Ask the app-owned modal layer to confirm an all-Trash bulk purge. The
    /// selection remains local; no purge action is authored until confirmed.
    ConfirmBulkPurge,
}

/// A command from the window's chrome (the sidebar, the header toolbars):
/// queued by the control and applied by the app with the document in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Browse explicit shared address-book recipient and sender choices.
    ContactRecipients,
    /// Show `folder` of account `index`.
    OpenFolder(usize, Folder),
    /// Open the account sheet for an account (`None` adds one).
    AccountSettings(Option<usize>),
    /// Start a new message.
    Compose,
    /// Open the platform search, narrowed to the open folder.
    OpenSearch,
    /// Fetch the open folder from the server again.
    Refresh,
    /// Enter or leave selecting several messages.
    ToggleBulk,
    /// Select every message the list shows.
    SelectAllVisible,
    /// Apply an action to the selected messages.
    Bulk(BulkAction),
    /// Apply an action to the open message.
    Message(CtxAction),
    /// Offer every action on the open message in a menu.
    MessageMenu,
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
    /// IDs selected for the compact bulk-triage surface.  This is explicitly
    /// device-local presentation state: a collaborator seeing the same inbox
    /// must not have their selection/focus changed.  The resulting archive,
    /// delete, or flag mutations do replicate through `MailAction`.
    pub bulk_selection: Vec<MessageId>,
    pub bulk_selecting: bool,
    /// The selected message was explicitly inspected rather than opened. This
    /// is transient presentation state (never collaborative data): it makes
    /// the no-side-effect mode visible in the reader and prevents the normal
    /// read-on-open action from being queued.
    pub inspection: Option<MessageId>,
    pub status: Option<String>,

    /// Remote avatar loader (Gravatar). No-op fetcher unless built `live`.
    pub images: ImageStore,

    // --- shell-derived flags the app writes each layout, panes read ---
    /// The overview and the message are separate screens (a narrow window).
    pub narrow: bool,
    /// The message is the front screen of a narrow window.
    pub detail_pushed: bool,

    /// Whether the CRDT has been seeded from this corpus yet.
    pub seeded: bool,
    /// Cursor in the app's logical coordinate space, for hover feedback.
    pub last_mouse: Point,
    /// Any pane sets this to request a repaint.
    pub dirty: bool,

    /// The framework dispatcher every document-mutating message operation
    /// (mark read/unread, star, move/archive/delete-to-trash) goes through,
    /// so it is checked, one undo step, replayed on peers and the same
    /// [`crate::actions::MailAction`] the palette and agents invoke. `None`
    /// until the surface is attached; edits before then change nothing.
    pub(crate) dispatcher: Option<Dispatcher<Mail>>,

    intents: Vec<Intent>,
    commands: Vec<Command>,
}

impl MailState {
    pub fn shared(state: MailState) -> Shared {
        Rc::new(RefCell::new(state))
    }

    /// The bundled sample mailbox and accounts, editing the document it is
    /// handed.
    #[cfg(test)]
    pub fn new_sample(images: ImageStore) -> Self {
        let mut state = Self::from_fixture(&crate::sample::MailFixture::bundled(), images);
        state.dispatcher = Some(ui::framework::testing::dispatcher("so.tilo.mail"));
        state
    }

    /// What a screenshot shows: the fixture's mailbox, accounts and clock,
    /// with a conversation selected so the reader is populated.
    pub fn from_fixture(fixture: &crate::sample::MailFixture, images: ImageStore) -> Self {
        let mut s = Self::empty(
            fixture.mailbox.clone(),
            fixture.accounts.clone(),
            fixture.now_min,
            images,
        );
        let threads = s.current_threads();
        let pick = threads
            .iter()
            .find(|t| t.message_ids.len() > 1)
            .or_else(|| threads.first());
        if let Some(&newest) = pick.and_then(|t| t.message_ids.last()) {
            s.selected = Some(newest);
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
            bulk_selection: Vec::new(),
            bulk_selecting: false,
            inspection: None,
            status: None,
            images,
            narrow: false,
            detail_pushed: false,
            seeded: false,
            last_mouse: Point::ZERO,
            dirty: true,
            dispatcher: None,
            intents: Vec::new(),
            commands: Vec::new(),
        }
    }

    /// Dispatch one [`MailAction`] — the single document-mutation path shared
    /// with the palette and agents. The in-memory mailbox is updated by the
    /// caller for immediate feedback and reconciled from the document on the
    /// next `sync`.
    fn dispatch_action(&self, doc: &mut ui::widget::Edits<'_>, action: &MailAction) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        let context = tilo_actions::ActionContext::for_action("so.tilo.mail", "Mail", action);
        if let Err(error) = dispatcher.apply_now_with(doc, action, context) {
            tracing::warn!(%error, "mail: an edit was refused");
        }
    }

    /// Record content from outside — what a mail server reported or
    /// confirmed — as one document action.
    pub(crate) fn dispatch_external(&self, doc: &mut ui::widget::Edits<'_>, action: &MailAction) {
        self.dispatch_action(doc, action);
    }

    /// Record server baselines as one document action
    /// ([`MailAction::IngestBaselines`](crate::actions::MailAction::IngestBaselines)).
    /// Existing flags win, so a later fetch cannot overwrite a `MailAction`.
    pub(crate) fn ingest_messages(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        messages: &[crate::model::Message],
    ) {
        if messages.is_empty() {
            return;
        }
        self.dispatch_action(
            doc,
            &crate::actions::MailAction::IngestBaselines {
                messages: messages
                    .iter()
                    .map(crate::actions::IngestBaseline::from_message)
                    .collect(),
            },
        );
    }

    /// Record what `account`'s server reported for `messages`: where each
    /// lives there and its flags ([`IngestBaseline::fetched`](crate::actions::IngestBaseline::fetched)).
    pub(crate) fn ingest_fetched(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        account: &str,
        messages: &[crate::model::Message],
    ) {
        if messages.is_empty() {
            return;
        }
        self.dispatch_action(
            doc,
            &crate::actions::MailAction::IngestBaselines {
                messages: messages
                    .iter()
                    .map(|message| crate::actions::IngestBaseline::fetched(message, account))
                    .collect(),
            },
        );
    }

    pub(crate) fn register_message_target(&self, doc: &mut ui::widget::Edits<'_>, id: &str) {
        self.dispatch_action(
            doc,
            &crate::actions::MailAction::IngestBaselines {
                messages: vec![crate::actions::IngestBaseline::register_only(id)],
            },
        );
    }

    /// Ensure an action target discovered in the already-loaded external
    /// corpus has an eligibility record before emitting its action. Normal UI
    /// flow seeds these records during sync; this tiny idempotent bridge also
    /// covers a just-hydrated message acted on before the next frame.
    fn register_loaded_target(&self, doc: &mut ui::widget::Edits<'_>, id: MessageId) {
        if self.mailbox.get(id).is_some() {
            self.register_message_target(doc, &id.to_string());
        }
    }

    /// Persist the in-progress draft as an action (undo boundary + replay + the
    /// same [`MailAction`] the palette's draft actions use). The visible Drafts
    /// message is materialised separately by the app.
    pub fn save_draft(&mut self, doc: &mut ui::widget::Edits<'_>, draft: &Draft) {
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

    pub fn push_command(&mut self, command: Command) {
        self.commands.push(command);
        self.dirty = true;
    }

    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    // ---- derived views ----

    pub fn current_threads(&self) -> Vec<Thread> {
        self.mailbox.threads_in(self.folder)
    }

    /// Rows to show in the list: `(message_id, thread_len)`, one row per
    /// thread (its newest message).
    pub fn list_rows(&self) -> Vec<(MessageId, usize)> {
        self.current_threads()
            .into_iter()
            .filter_map(|t| {
                t.message_ids
                    .last()
                    .copied()
                    .map(|id| (id, t.message_ids.len()))
            })
            .collect()
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

    /// Open a message through the user-visible read boundary.
    ///
    /// This is deliberately an interaction/domain method, rather than a
    /// render-time side effect.  Callers which offer normal opening (rows,
    /// keyboard activation, and the admitted resource operation) all use it,
    /// so an unread message produces the same canonical `SetRead` action.
    /// [`Self::preview_message`] is the only read-neutral alternative.
    pub fn open_message(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId) -> bool {
        let Some(folder) = self.mailbox.get(id).map(|message| message.folder) else {
            return false;
        };
        let was_unread = self.mailbox.get(id).is_some_and(|m| !m.read);
        self.folder = folder;
        self.selected = Some(id);
        self.inspection = None;
        // Opening is the user-visible read boundary. Use the exact same typed,
        // replayable flag action as the row/reader/palette instead of a private
        // direct write; `set_read` also mirrors \Seen to IMAP. Indexing and
        // prefetch never call this method and remain side-effect free.
        if was_unread {
            self.set_read(doc, id, true);
        }
        self.push_intent(Intent::ShowDetail);
        true
    }

    /// Compatibility entry point for the existing painted list controls.
    /// New code should name the domain boundary explicitly with
    /// [`Self::open_message`].
    pub fn select_message(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId) {
        let _ = self.open_message(doc, id);
    }

    /// Select a device-local account. Account identity/credentials live in the
    /// shared Tilo account store + secret vault, never in the collaborative mail
    /// document, so this deliberately has no Automerge write.
    pub fn set_active_account(&mut self, idx: usize) {
        self.accounts.set_active(idx);
        self.dirty = true;
    }

    pub fn toggle_account_collapsed(&mut self, idx: usize) {
        self.accounts.toggle_collapsed(idx);
        self.dirty = true;
    }

    pub fn select_folder(&mut self, _doc: &mut ui::widget::Edits<'_>, folder: Folder) {
        self.folder = folder;
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        // Ask the live engine to fetch this folder (a no-op offline).
        let account = self.accounts.active_config();
        if !account.email.is_empty() {
            self.push_intent(Intent::Engine(EngineRequest::fetch(account, folder)));
        }
        self.dirty = true;
    }

    /// Reveal a folder as local presentation state. Canonical resource `View`
    /// operations use this path, so navigation does not create a document edit.
    pub fn reveal_folder(&mut self, folder: Folder) {
        self.folder = folder;
        self.selected = self
            .current_threads()
            .first()
            .and_then(|thread| thread.message_ids.last().copied());
        let account = self.accounts.active_config();
        if !account.email.is_empty() {
            self.push_intent(Intent::Engine(EngineRequest::fetch(account, folder)));
        }
        self.push_intent(Intent::ShowOverview);
        self.dirty = true;
    }

    /// Inspect a loaded message without changing collaborative message state.
    /// This is intentionally reserved for the explicitly-labelled Preview
    /// operation; normal opens must call [`Self::open_message`].
    pub fn preview_message(&mut self, id: MessageId) -> bool {
        let Some(folder) = self.mailbox.get(id).map(|message| message.folder) else {
            return false;
        };
        self.folder = folder;
        self.selected = Some(id);
        self.inspection = Some(id);
        self.push_intent(Intent::ShowDetail);
        self.dirty = true;
        true
    }

    /// Enter or leave the local multi-select mode. Leaving always clears the
    /// local selection so a later accidental shortcut cannot act on messages
    /// the person no longer sees selected.
    pub fn set_bulk_selecting(&mut self, selecting: bool) {
        self.bulk_selecting = selecting;
        if !selecting {
            self.bulk_selection.clear();
        }
        self.dirty = true;
    }

    /// Toggle one already-visible loaded message in the local bulk selection.
    /// A stale row is ignored instead of retaining an id that could later be
    /// reused by an unrelated hydrated message.
    pub fn toggle_bulk_selection(&mut self, id: MessageId) -> bool {
        if self.mailbox.get(id).is_none() {
            return false;
        }
        self.bulk_selecting = true;
        if let Some(index) = self
            .bulk_selection
            .iter()
            .position(|selected| *selected == id)
        {
            self.bulk_selection.remove(index);
        } else {
            self.bulk_selection.push(id);
        }
        self.dirty = true;
        true
    }

    /// Select every currently visible row, bounded by the list's own result
    /// cap. This is a selection operation only; no mailbox state changes until
    /// an explicit bulk action is chosen.
    pub fn select_all_visible(&mut self) {
        self.bulk_selecting = true;
        self.bulk_selection = self
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.mailbox.get(*id).is_some())
            .collect();
        self.dirty = true;
    }

    pub fn selected_bulk_ids(&self) -> Vec<MessageId> {
        self.bulk_selection
            .iter()
            .copied()
            .filter(|id| self.mailbox.get(*id).is_some())
            .collect()
    }

    /// Apply a bounded multi-message operation using the exact same action
    /// payload families as the individual controls. Returns how many loaded
    /// messages were affected. Archive/move-to-Trash/purge clear the local selection because
    /// those rows have left the current projection; flag changes intentionally
    /// retain it so a person can triage the same set in successive steps.
    pub fn apply_bulk_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        action: BulkAction,
    ) -> usize {
        let ids = self.selected_bulk_ids();
        if ids.is_empty() {
            return 0;
        }
        // Keep the visible path as atomic as the canonical reducer: a mixed
        // selection is not an instruction to quietly purge the subset already
        // in Trash. The user can either move the other messages first or make
        // an explicit all-Trash selection. Check before registering targets so
        // a refusal creates no document mutation at all.
        if action == BulkAction::Purge
            && ids.iter().any(|id| {
                self.mailbox
                    .get(*id)
                    .is_none_or(|message| message.folder != Folder::Trash)
            })
        {
            return 0;
        }
        for id in &ids {
            self.register_loaded_target(doc, *id);
        }
        let encoded_ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let affected = match action {
            BulkAction::MarkRead | BulkAction::MarkUnread => {
                let read = action == BulkAction::MarkRead;
                for id in &ids {
                    self.mailbox.mark_read(*id, read);
                }
                self.dispatch_action(doc, &set_read_action(encoded_ids, read));
                ids.len()
            }
            BulkAction::Archive => {
                for id in &ids {
                    self.mailbox.move_to(*id, Folder::Archive);
                }
                self.dispatch_action(doc, &move_action(encoded_ids, Folder::Archive));
                self.bulk_selection.clear();
                self.reselect_after_remove(doc);
                ids.len()
            }
            BulkAction::Delete => {
                // This path is *only* a move to Trash. A virtual Starred view
                // can contain Inbox and Trash rows at once, so calling Delete
                // here must never silently change into permanent deletion for
                // the latter. The explicit `Purge` variant below is separately
                // labelled, destructive in the resource tree, and folder-gated
                // again by the canonical reducer.
                let move_to_trash: Vec<MessageId> = ids
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.mailbox
                            .get(*id)
                            .is_some_and(|mail| mail.folder != Folder::Trash)
                    })
                    .collect();
                if !move_to_trash.is_empty() {
                    for id in &move_to_trash {
                        self.mailbox.move_to(*id, Folder::Trash);
                    }
                    self.dispatch_action(
                        doc,
                        &move_action(
                            move_to_trash.iter().map(ToString::to_string).collect(),
                            Folder::Trash,
                        ),
                    );
                }
                self.bulk_selection.clear();
                self.reselect_after_remove(doc);
                move_to_trash.len()
            }
            BulkAction::Purge => {
                // `ids` passed the all-Trash preflight above. The action
                // reducer repeats it against the document to cover a stale
                // remotely-authored envelope.
                for id in &ids {
                    self.mailbox.purge(*id);
                }
                self.dispatch_action(doc, &purge_action(encoded_ids));
                self.bulk_selection.clear();
                self.reselect_after_remove(doc);
                ids.len()
            }
        };
        self.dirty = true;
        affected
    }

    pub fn reselect_after_remove(&mut self, _doc: &mut ui::widget::Edits<'_>) {
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
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
    // action the Ctrl+K palette dispatches. The mail server learns of it from
    // the document, as it does of a change the palette or a peer made.

    /// Set a message's read flag through the action path.
    pub fn set_read(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId, read: bool) {
        if self.mailbox.get(id).is_none() {
            return;
        }
        self.register_loaded_target(doc, id);
        self.mailbox.mark_read(id, read);
        self.dispatch_action(doc, &set_read_action(vec![id.to_string()], read));
        self.dirty = true;
    }

    /// Toggle a message's star through the action path.
    pub fn toggle_star(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId) {
        let Some(target) = self.mailbox.get(id).map(|m| !m.starred) else {
            return;
        };
        self.register_loaded_target(doc, id);
        self.mailbox.toggle_star(id);
        self.dispatch_action(doc, &set_starred_action(vec![id.to_string()], target));
        self.dirty = true;
    }

    /// Move a message to `folder` through the action path.
    pub fn move_message(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId, folder: Folder) {
        if self.mailbox.get(id).is_none() {
            return;
        }
        self.register_loaded_target(doc, id);
        self.mailbox.move_to(id, folder);
        self.dispatch_action(doc, &move_action(vec![id.to_string()], folder));
    }

    /// Apply a message action. Reply/forward raise an intent (the app owns the
    /// composer); the rest route through the action path above.
    pub fn apply_message_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        id: MessageId,
        action: CtxAction,
    ) {
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
                // Move to Trash, or purge if it was already there. Both are
                // replayable actions the server learns of from the document.
                let purging = self
                    .mailbox
                    .get(id)
                    .is_some_and(|message| message.folder == Folder::Trash);
                self.register_loaded_target(doc, id);
                self.mailbox.delete(id);
                let moved_to = self.mailbox.get(id).map(|m| m.folder);
                if let Some(to) = moved_to {
                    self.dispatch_action(doc, &move_action(vec![id.to_string()], to));
                } else if purging {
                    self.dispatch_action(doc, &purge_action(vec![id.to_string()]));
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

    fn after_remove(&mut self, doc: &mut ui::widget::Edits<'_>, was_selected: bool) {
        self.reselect_after_remove(doc);
        // If the open message left the current folder while its detail was
        // pushed on narrow, return to the list.
        if was_selected && self.narrow && self.detail_pushed {
            self.push_intent(Intent::ShowOverview);
        }
    }

    pub fn move_selection(&mut self, _doc: &mut ui::widget::Edits<'_>, delta: i32) {
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
        // Browsing the list with the arrows is presentation-only: it previews
        // the candidate in the reader without claiming that the user opened
        // it. Enter / list activation goes through `open_message` and emits
        // the explicit collaborative `SetRead(true)` action instead.
        self.selected = Some(id);
        self.dirty = true;
    }
}

/// Wall-clock "now" in minutes since the Unix epoch.
pub fn now_minutes() -> i64 {
    (ui::time::unix_now() / 60) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;

    fn state() -> MailState {
        MailState::new_sample(crate::app::avatar_store_offline())
    }

    fn first_inbox_id(st: &MailState) -> Option<MessageId> {
        st.mailbox.messages_in(Folder::Inbox).first().map(|m| m.id)
    }

    /// The undo steps written to `doc` since it had `before` versions: what
    /// each did, leaving out content from outside (fetched baselines).
    fn steps_since(doc: &AutoCommit, before: usize) -> Vec<String> {
        ui::tilo_resource::history::versions(&mut doc.clone())
            .into_iter()
            .skip(before)
            .filter_map(|version| version.context)
            .filter(|context| context.change.is_step())
            .map(|context| context.action_label)
            .collect()
    }

    fn version_count(doc: &AutoCommit) -> usize {
        ui::tilo_resource::history::versions(&mut doc.clone()).len()
    }

    /// Fetched messages are recorded as a mail action whose server flags
    /// are content from outside: in the document, on nobody's undo stack.
    #[test]
    fn ingesting_records_server_baselines_outside_undo() {
        let st = state();
        let message = st
            .mailbox
            .messages_in(Folder::Inbox)
            .into_iter()
            .next()
            .cloned()
            .expect("sample inbox");
        let mut doc = automerge::AutoCommit::new();
        st.ingest_messages(
            &mut ui::widget::Edits::lend(&mut doc),
            std::slice::from_ref(&message),
        );
        assert_eq!(
            crate::doc::MailDoc::get_read(&doc, &message.id.to_string()),
            Some(message.read)
        );
        assert_eq!(version_count(&doc), 1, "one ingest is one version");
        assert!(steps_since(&doc, 0).is_empty(), "and no undo step");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn reader_selection_is_read_neutral_and_reply_all_is_distinct() {
        // Phase `initial_reader_selection_is_read_state_neutral`.
        {
            let st = state();
            let selected = st
                .selected
                .expect("sample selects an initial reader message");
            assert!(
                st.mailbox
                    .get(selected)
                    .is_some_and(|message| !message.read),
                "the sample's initially-rendered unread message must stay unread until a deliberate Open"
            );
        }

        // Phase `arrow_browsing_previews_an_unread_message_without_a_read_action`.
        {
            let mut st = state();
            let rows = st.list_rows();
            let unread_index = rows
                .iter()
                .position(|(id, _)| st.mailbox.get(*id).is_some_and(|message| !message.read))
                .expect("sample contains an unread row");
            if unread_index > 0 {
                st.selected = Some(rows[unread_index - 1].0);
            } else {
                st.selected = Some(rows[1].0);
            }
            let target = rows[unread_index].0;
            let delta = if unread_index > 0 { 1 } else { -1 };
            let mut doc = AutoCommit::new();
            let heads_before = doc.get_heads();
            st.move_selection(&mut ui::widget::Edits::lend(&mut doc), delta);
            assert_eq!(st.selected, Some(target));
            assert!(st.mailbox.get(target).is_some_and(|message| !message.read));
            assert_eq!(doc.get_heads(), heads_before);
        }

        // Phase `reply_all_action_raises_distinct_reply_all_intent`.
        {
            let mut st = state();
            let mut doc = AutoCommit::new();
            let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
            st.apply_message_action(
                &mut ui::widget::Edits::lend(&mut doc),
                id,
                CtxAction::ReplyAll,
            );
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

    /// `id` as `you@example.test`'s server reported it.
    fn fetch(st: &mut MailState, doc: &mut AutoCommit, id: MessageId) {
        let message = st.mailbox.get_mut(id).expect("loaded message");
        message.rfc_message_id = Some(format!("{id}@example.test"));
        let message = message.clone();
        st.ingest_fetched(
            &mut ui::widget::Edits::lend(doc),
            "you@example.test",
            std::slice::from_ref(&message),
        );
    }

    /// A triage control posts nothing to the engine itself: it changes the
    /// document, and what the server has to be told follows from that.
    #[test]
    fn message_actions_reach_the_server_through_the_document() {
        use crate::actions::ServerUpdate;
        let posts_nothing = |st: &mut MailState| {
            assert!(
                !st.take_intents()
                    .iter()
                    .any(|intent| matches!(intent, Intent::Engine(_))),
                "a control never talks to the engine itself"
            );
        };

        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        fetch(&mut st, &mut doc, id);
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::ToggleStar,
        );
        posts_nothing(&mut st);
        assert!(matches!(
            crate::doc::MailDoc::server_updates(&doc).as_slice(),
            [ServerUpdate::Flags {
                starred: true,
                folder: Folder::Inbox,
                ..
            }]
        ));

        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        fetch(&mut st, &mut doc, id);
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::Archive,
        );
        posts_nothing(&mut st);
        assert!(matches!(
            crate::doc::MailDoc::server_updates(&doc).as_slice(),
            [ServerUpdate::Move {
                from: Folder::Inbox,
                to: Folder::Archive,
                ..
            }]
        ));

        let mut st = state();
        let mut doc = AutoCommit::new();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        fetch(&mut st, &mut doc, id);
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::Delete,
        );
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::Delete,
        );
        posts_nothing(&mut st);
        assert!(
            matches!(
                crate::doc::MailDoc::server_updates(&doc).as_slice(),
                [ServerUpdate::Remove {
                    folder: Folder::Inbox,
                    ..
                }]
            ),
            "a message deleted for good is removed from where the server has it"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bulk_actions_are_bounded_canonical_and_never_partial() {
        // Phase `bulk_archive_is_one_bounded_canonical_move_action_and_keeps_selection_local`.
        {
            let mut st = state();
            let mut doc = AutoCommit::new();
            let ids: Vec<MessageId> = st
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .take(2)
                .collect();
            assert_eq!(ids.len(), 2, "sample exposes two list rows");
            for id in &ids {
                assert!(st.toggle_bulk_selection(*id));
            }
            let archive_before = st.mailbox.count(Folder::Archive);
            let versions_before = version_count(&doc);
            assert_eq!(
                st.apply_bulk_action(&mut ui::widget::Edits::lend(&mut doc), BulkAction::Archive),
                2
            );
            assert_eq!(st.mailbox.count(Folder::Archive), archive_before + 2);
            assert!(
                st.bulk_selection.is_empty(),
                "moved rows cannot stay selected"
            );
            assert_eq!(
                steps_since(&doc, versions_before).len(),
                1,
                "two archives are one undo step"
            );
            for id in &ids {
                assert_eq!(
                    crate::doc::MailDoc::get_folder_for(&doc, &id.to_string()),
                    Some(Folder::Archive)
                );
            }
            assert!(
                doc.get(&automerge::ROOT, "bulk_selection")
                    .expect("document remains readable")
                    .is_none(),
                "device-local selection is never written to the shared document"
            );
        }

        // Phase `bulk_marking_read_retains_the_local_selection_for_follow_up_triage`.
        {
            let mut st = state();
            let mut doc = AutoCommit::new();
            let ids: Vec<MessageId> = st
                .list_rows()
                .into_iter()
                .map(|(id, _)| id)
                .take(2)
                .collect();
            for id in &ids {
                st.toggle_bulk_selection(*id);
            }
            assert_eq!(
                st.apply_bulk_action(&mut ui::widget::Edits::lend(&mut doc), BulkAction::MarkRead),
                2
            );
            assert!(
                ids.iter()
                    .all(|id| st.mailbox.get(*id).is_some_and(|m| m.read))
            );
            assert_eq!(st.selected_bulk_ids(), ids);
        }

        // Phase `bulk_move_to_trash_never_purges_trash_rows_from_a_mixed_virtual_view`.
        {
            let mut st = state();
            let mut doc = AutoCommit::new();
            let ids: Vec<MessageId> = st
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .take(2)
                .map(|message| message.id)
                .collect();
            let (already_trash, inbox) = (ids[0], ids[1]);
            st.mailbox.move_to(already_trash, Folder::Trash);
            // Starred is a virtual folder in the real app, so this deliberately
            // models a selection whose physical rows come from different folders.
            st.folder = Folder::Starred;
            st.mailbox.get_mut(already_trash).unwrap().starred = true;
            st.mailbox.get_mut(inbox).unwrap().starred = true;
            st.toggle_bulk_selection(already_trash);
            st.toggle_bulk_selection(inbox);

            assert_eq!(
                st.apply_bulk_action(&mut ui::widget::Edits::lend(&mut doc), BulkAction::Delete),
                1
            );
            assert_eq!(
                st.mailbox.get(already_trash).map(|message| message.folder),
                Some(Folder::Trash),
                "Delete means move-to-Trash even when a mixed selection includes Trash"
            );
            assert_eq!(
                st.mailbox.get(inbox).map(|message| message.folder),
                Some(Folder::Trash)
            );
            assert_eq!(
                crate::doc::MailDoc::get_folder_for(&doc, &inbox.to_string()),
                Some(Folder::Trash)
            );
            assert!(
                crate::doc::MailDoc::is_present_message(&doc, &already_trash.to_string()),
                "the row already in Trash is never purged"
            );
        }

        // Phase `bulk_purge_refuses_mixed_or_stale_rows_without_a_partial_mutation`.
        {
            let mut st = state();
            let mut doc = AutoCommit::new();
            let ids: Vec<MessageId> = st
                .mailbox
                .messages_in(Folder::Inbox)
                .into_iter()
                .take(2)
                .map(|message| message.id)
                .collect();
            let (trash, inbox) = (ids[0], ids[1]);
            st.mailbox.move_to(trash, Folder::Trash);
            st.toggle_bulk_selection(trash);
            st.toggle_bulk_selection(inbox);
            // A stale id can never be materialized into a later batch.
            st.bulk_selection.push(uuid::Uuid::new_v4());

            let heads_before = doc.get_heads();
            assert_eq!(
                st.apply_bulk_action(&mut ui::widget::Edits::lend(&mut doc), BulkAction::Purge),
                0
            );
            assert!(st.mailbox.get(trash).is_some());
            assert!(st.mailbox.get(inbox).is_some());
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "a rejected mixed/stale purge never creates a partial document mutation"
            );
        }
    }
}

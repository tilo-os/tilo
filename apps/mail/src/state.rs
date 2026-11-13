//! Shared, render-free mail state, owned by the app and borrowed by each of the
//! three panes through an `Rc<RefCell<_>>`.
//!
//! The panes are siblings inside a [`ui::widget::NavShell`], so they can't call
//! each other directly. Instead they mutate this shared state and raise
//! [`Intent`]s; after routing an event through the shell the app drains the
//! intents and drives the cross-pane effects (push the detail pane, open the
//! composer, post an engine request, …).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(test)]
use automerge::AutoCommit;
use vello::kurbo::{Point, Rect};

use ui::icons::Icon;
use ui::widget::toast::ToastKind;

use crate::account::AccountStore;
use ui::framework::Dispatcher;

use crate::actions::IngestBaseline;
use crate::engine::EngineRequest;
use crate::model::{AccountConfig, Draft, Folder, Mailbox, MessageId, Thread};
use crate::resource::{Mail, mailbox};
use ui::framework::Submit;

pub type Shared = Rc<RefCell<MailState>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub kind: ToastKind,
}

impl Notice {
    fn new(kind: ToastKind, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self::new(ToastKind::Success, text)
    }

    pub fn info(text: impl Into<String>) -> Self {
        Self::new(ToastKind::Info, text)
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self::new(ToastKind::Warning, text)
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self::new(ToastKind::Error, text)
    }
}

impl std::ops::Deref for Notice {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

/// Why an account's mail cannot be checked, as its last check found it. It
/// stays in the window until a check works again, so a server that cannot be
/// reached is a state the list shows rather than a notice that fades.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncFailure {
    /// The account the failed check was for, as the person knows it.
    pub account: String,
    /// What stopped the check, which decides what the list offers beside the
    /// words.
    pub cause: SyncCause,
}

/// What stopped a check, read from the words the engine failed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncCause {
    /// The server wants the person to sign in again.
    SignedOut,
    /// This device has no network.
    Offline,
    /// The server did not answer.
    Unreachable,
}

pub(crate) fn moved_to(folder: Folder) -> String {
    crate::i18n::t_args(
        "mail-status-moved",
        &[(
            "folder",
            ui::i18n::ArgValue::from(crate::i18n::t(folder.message())),
        )],
    )
}

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BulkAction {
    Archive,
    Delete,
    Purge,
    MarkRead,
    MarkUnread,
}

impl BulkAction {
    /// What the bar calls it: the same verb the one-message action uses,
    /// except where acting on many says something else ("Move to Trash").
    pub fn label(self) -> String {
        match self {
            Self::Archive => CtxAction::Archive.label(false, false),
            Self::Delete => crate::i18n::t("mail-move-to-trash"),
            Self::Purge => crate::i18n::t("mail-delete-permanently"),
            Self::MarkRead => CtxAction::ToggleRead.label(false, false),
            Self::MarkUnread => CtxAction::ToggleRead.label(true, false),
        }
    }
}

impl CtxAction {
    /// The action's name in the window's language: the context menu, the
    /// row's hover buttons, the bar, the reader and the tooltips all say this
    /// one, so a German window names them all in German.
    pub fn label(self, read: bool, starred: bool) -> String {
        crate::i18n::t(match self {
            CtxAction::Reply => "mail-reply",
            CtxAction::ReplyAll => "mail-reply-all",
            CtxAction::Forward => "mail-forward",
            CtxAction::ToggleRead if read => "mail-mark-unread-message",
            CtxAction::ToggleRead => "mail-mark-read-message",
            CtxAction::ToggleStar if starred => "mail-unstar",
            CtxAction::ToggleStar => "mail-star",
            CtxAction::Archive => "mail-archive-message",
            CtxAction::Delete => "mail-delete",
            CtxAction::MoveToInbox => "mail-move-to-inbox",
            CtxAction::Junk => "mail-junk",
        })
    }

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderChoice {
    OpenLink(String),
    CopyLink(String),
    Copy,
    SelectAll,
    ShowPlainText,
    ShowFormatted,
}

#[derive(Clone, Debug)]
pub enum Intent {
    ShowDetail,
    ShowOverview,
    CloseDrawer,
    Compose,
    Reply(MessageId),
    ReplyAll(MessageId),
    Forward(MessageId),
    EditDraft(MessageId),
    ContextMenu(MessageId, Point, Rect),
    /// Ask the app-owned portal boundary to save one hydrated attachment.
    SaveAttachment(MessageId, usize),
    OpenAttachment(MessageId, usize),
    Engine(EngineRequest),
    OpenLink(String),
    ReaderMenu(MessageId, Point, Vec<ReaderChoice>),
    ConfirmBulkPurge,
    AccountSettings,
    /// Check the open account's mail again: the way out of a failed check.
    CheckMail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedMessage {
    pub id: String,
    pub account: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub reason: Option<String>,
}

impl QueuedMessage {
    pub fn state_line(&self) -> String {
        self.reason
            .clone()
            .unwrap_or_else(|| crate::i18n::t("mail-outbox-waiting"))
    }
}

/// A command from the window's chrome (the sidebar, the header toolbars):
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    ContactRecipients,
    OpenOutbox(usize),
    OpenFolder(usize, Folder),
    OpenAllInboxes,
    AccountSettings,
    Compose,
    CloseCompose,
    DiscardCompose,
    ToggleBulk,
    SelectAllVisible,
    Bulk(BulkAction),
    Message(CtxAction),
    CancelSend(String),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (shell-derived layout flags the app writes, focus/seed bookkeeping, and the repaint flag); grouping them would need a real design pass, left for a follow-up rather than forced here"
)]
pub struct MailState {
    pub mailbox: Mailbox,
    pub accounts: AccountStore,
    pub now_min: i64,
    pub folder: Folder,
    pub selected: Option<MessageId>,
    pub bulk_selection: Vec<MessageId>,
    pub bulk_selecting: bool,
    pub selecting_all: bool,
    pub more_older: bool,
    pub inspection: Option<MessageId>,
    pub status: Option<Notice>,
    pub status_undo: bool,
    pub all_inboxes: bool,
    pub outbox_open: bool,
    pub outbox_account: usize,
    pub outbox: Vec<QueuedMessage>,
    pub outbox_selected: Option<String>,
    /// Why the active account cannot check mail, as last found; `None` while
    /// it works. The list says it until a check works again.
    pub sync_error: Option<SyncFailure>,
    pub wants_older: std::cell::Cell<bool>,
    pub first_fetch: Option<ui::arriving::ArrivalClock>,
    pub passed_over: HashMap<(String, Folder), crate::sync::Window>,
    pub remote_senders: std::collections::BTreeSet<String>,
    pub remote_messages: std::collections::HashSet<MessageId>,
    pub plain_text: std::collections::HashSet<MessageId>,
    pub(crate) kept: crate::store::Store,
    pub reader_requests: Vec<ReaderChoice>,

    pub avatars: crate::avatars::Avatars,

    // --- shell-derived flags the app writes each layout, panes read ---
    /// The overview and the message are separate screens (a narrow window).
    pub narrow: bool,
    pub detail_pushed: bool,

    pub seeded: bool,
    pub last_mouse: Point,
    pub dirty: bool,

    pub(crate) dispatcher: Option<Dispatcher<Mail>>,

    intents: Vec<Intent>,
    commands: Vec<Command>,
}

impl MailState {
    pub fn shared(state: MailState) -> Shared {
        Rc::new(RefCell::new(state))
    }

    /// The account `account` could not be checked, for `cause`: the list says
    /// so until a check works again. A check that fails the same way changes
    /// nothing, so a window refreshing into the same failure stays quiet.
    pub fn note_sync_failure(&mut self, account: String, cause: SyncCause) {
        let failure = SyncFailure { account, cause };
        if self.sync_error.as_ref() != Some(&failure) {
            self.sync_error = Some(failure);
            self.dirty = true;
        }
        // What was still arriving is not coming: the failure is what there is
        // to say about this account now.
        self.first_fetch = None;
    }

    /// The account works again: the failure it showed is over.
    pub fn clear_sync_failure(&mut self) {
        if self.sync_error.take().is_some() {
            self.dirty = true;
        }
    }

    pub fn remote_allowed(&self, message: &crate::model::Message) -> bool {
        self.remote_messages.contains(&message.id)
            || self
                .remote_senders
                .contains(&message.from.email.to_ascii_lowercase())
    }

    pub fn load_remote(&mut self, id: MessageId) {
        self.remote_messages.insert(id);
        self.dirty = true;
    }

    pub fn always_load_remote(&mut self, id: MessageId) {
        if let Some(sender) = self
            .mailbox
            .get(id)
            .map(|message| message.from.email.to_ascii_lowercase())
        {
            self.remote_senders.insert(sender);
            self.dirty = true;
        }
    }

    pub fn stop_remote(&mut self, id: MessageId) {
        if let Some(sender) = self
            .mailbox
            .get(id)
            .map(|message| message.from.email.to_ascii_lowercase())
        {
            self.remote_senders.remove(&sender);
        }
        self.remote_messages.remove(&id);
        self.dirty = true;
    }

    pub fn inline_parts(
        &self,
        message: &crate::model::Message,
    ) -> Vec<crate::html_mail::InlinePart> {
        message
            .attachments
            .iter()
            .enumerate()
            .filter_map(|(index, attachment)| {
                let id = attachment.inline_id.clone()?;
                let bytes = if attachment.content.is_empty() && attachment.kept {
                    self.kept.attachment(&message.account, message.id, index)?
                } else {
                    attachment.content.clone()
                };
                Some(crate::html_mail::InlinePart {
                    id,
                    mime: attachment.mime.clone(),
                    bytes,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub fn new_sample(avatars: crate::avatars::Avatars) -> Self {
        let mut state = Self::from_fixture(&crate::sample::MailFixture::bundled(), avatars);
        state.dispatcher = Some(ui::framework::testing::dispatcher("so.tilo.mail"));
        state
    }

    #[cfg(feature = "samples")]
    pub fn from_fixture(
        fixture: &crate::sample::MailFixture,
        avatars: crate::avatars::Avatars,
    ) -> Self {
        let mut s = Self::empty(
            fixture.mailbox.clone(),
            fixture.accounts.clone(),
            fixture.now_min,
            avatars,
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
    pub fn new_live(accounts: AccountStore, avatars: crate::avatars::Avatars) -> Self {
        let cfg = accounts.active_config();
        Self::empty(Mailbox::new(cfg), accounts, now_minutes(), avatars)
    }

    fn empty(
        mailbox: Mailbox,
        accounts: AccountStore,
        now_min: i64,
        avatars: crate::avatars::Avatars,
    ) -> Self {
        Self {
            mailbox,
            accounts,
            now_min,
            folder: Folder::Inbox,
            selected: None,
            bulk_selection: Vec::new(),
            bulk_selecting: false,
            selecting_all: false,
            more_older: false,
            remote_senders: std::collections::BTreeSet::new(),
            remote_messages: std::collections::HashSet::new(),
            plain_text: std::collections::HashSet::new(),
            kept: crate::store::Store::default(),
            reader_requests: Vec::new(),
            inspection: None,
            status: None,
            status_undo: false,
            all_inboxes: false,
            outbox_open: false,
            outbox_account: 0,
            outbox: Vec::new(),
            outbox_selected: None,
            sync_error: None,
            wants_older: std::cell::Cell::new(false),
            first_fetch: None,
            passed_over: HashMap::new(),
            avatars,
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

    pub(crate) fn submit(&self, doc: &mut ui::widget::Edits<'_>, submit: impl Submit<Mail>) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if let Err(error) = dispatcher.submit_now(doc, submit) {
            tracing::warn!(%error, "mail: an edit was refused");
        }
    }

    pub(crate) fn submit_gesture(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        submit: impl Submit<Mail>,
    ) {
        let Some(dispatcher) = &self.dispatcher else {
            return;
        };
        if let Err(error) = dispatcher.submit_gesture(doc, submit) {
            tracing::warn!(%error, "mail: an edit was refused");
        }
    }

    pub(crate) fn ingest_messages(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        messages: &[crate::model::Message],
    ) {
        if messages.is_empty() {
            return;
        }
        self.ingest(
            doc,
            messages.iter().map(IngestBaseline::from_message).collect(),
        );
    }

    pub(crate) fn ingest_fetched(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        account: &str,
        messages: &[crate::model::Message],
    ) {
        if messages.is_empty() {
            return;
        }
        self.ingest(
            doc,
            messages
                .iter()
                .map(|message| IngestBaseline::fetched(message, account))
                .collect(),
        );
    }

    pub(crate) fn ingest_baselines(
        &self,
        doc: &mut ui::widget::Edits<'_>,
        baselines: Vec<IngestBaseline>,
    ) {
        self.ingest(doc, baselines);
    }

    fn ingest(&self, doc: &mut ui::widget::Edits<'_>, messages: Vec<IngestBaseline>) {
        self.submit(doc, Mail::INGEST.with(mailbox::Ingest { messages }));
    }

    pub(crate) fn register_message_target(&self, doc: &mut ui::widget::Edits<'_>, id: &str) {
        self.ingest(doc, vec![IngestBaseline::register_only(id)]);
    }

    fn register_loaded_target(&self, doc: &mut ui::widget::Edits<'_>, id: MessageId) {
        let known = crate::resource::read_mail(doc.doc())
            .is_some_and(|mail| mail.message(&id.to_string()).is_some());
        if !known && self.mailbox.get(id).is_some() {
            self.register_message_target(doc, &id.to_string());
        }
    }

    pub fn save_draft(&mut self, doc: &mut ui::widget::Edits<'_>, draft: &Draft) {
        self.save_draft_as(doc, None, draft);
    }

    pub fn save_draft_as(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        id: Option<&str>,
        draft: &Draft,
    ) {
        self.submit(
            doc,
            Mail::SAVE_DRAFT.with(mailbox::SaveDraft {
                message: crate::actions::DraftData::from_draft(draft),
                draft: id.map(str::to_owned),
            }),
        );
    }

    fn message_paths(
        id: MessageId,
    ) -> <crate::resource::MessageState as ui::tilo_resource::Model>::Paths<Mail> {
        Mail::paths()
            .messages
            .item(&ui::tilo_resource::Id::from_raw(id.to_string()))
    }

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

    pub fn current_threads(&self) -> Vec<Thread> {
        self.mailbox.threads_in(self.folder)
    }

    pub fn list_rows(&self) -> Vec<(MessageId, usize)> {
        self.mailbox
            .threads(self.folder)
            .iter()
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
        // The conversation the list shows the message in: the open folder's
        // when it lists the message (Starred lists messages of every
        // folder), otherwise the message's own folder's.
        let thread = self
            .mailbox
            .thread_of(self.folder, sel.id)
            .or_else(|| self.mailbox.thread_of(sel.folder, sel.id));
        let Some(thread) = thread else {
            return vec![sel];
        };
        let mut messages: Vec<&crate::model::Message> = thread
            .message_ids
            .iter()
            .filter_map(|id| self.mailbox.get(*id))
            .collect();
        // What the person answered is part of the conversation, though it
        // is kept in Sent.
        if sel.folder != Folder::Sent
            && let Some(sent) = self.mailbox.thread_rooted(Folder::Sent, &thread.id)
        {
            messages.extend(
                sent.message_ids
                    .iter()
                    .filter_map(|id| self.mailbox.get(*id)),
            );
            messages.sort_by_key(|message| message.timestamp_min);
        }
        messages
    }

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

    pub fn set_active_account(&mut self, idx: usize) {
        self.accounts.set_active(idx);
        self.all_inboxes = false;
        self.mailbox.set_account(self.accounts.active_config());
    }

    pub fn toggle_account_collapsed(&mut self, idx: usize) {
        self.accounts.toggle_collapsed(idx);
        self.dirty = true;
    }

    /// Show every account's inbox in one list, newest first.
    pub fn open_outbox(&mut self, account: usize) {
        self.set_active_account(account);
        self.outbox_account = account;
        self.outbox_open = true;
        self.all_inboxes = false;
        self.bulk_selecting = false;
        self.selected = None;
        self.outbox_selected = self.queued().last().map(|queued| queued.id.clone());
        self.push_intent(Intent::ShowOverview);
        self.dirty = true;
    }

    pub fn close_outbox(&mut self) {
        self.outbox_open = false;
        self.outbox_selected = None;
        self.dirty = true;
    }

    pub fn queued(&self) -> Vec<&QueuedMessage> {
        let email = self
            .accounts
            .accounts
            .get(self.outbox_account)
            .map_or_else(String::new, |account| account.config.email.clone());
        self.outbox
            .iter()
            .filter(|queued| queued.account == email)
            .collect()
    }

    pub fn queued_count(&self, account: usize) -> usize {
        let Some(account) = self.accounts.accounts.get(account) else {
            return 0;
        };
        self.outbox
            .iter()
            .filter(|queued| queued.account == account.config.email)
            .count()
    }

    pub fn open_queued(&self) -> Option<&QueuedMessage> {
        let id = self.outbox_selected.as_deref()?;
        self.queued().into_iter().find(|queued| queued.id == id)
    }

    pub fn select_queued(&mut self, id: &str) {
        self.outbox_selected = Some(id.to_owned());
        self.push_intent(Intent::ShowDetail);
        self.dirty = true;
    }

    pub fn cancel_send(&mut self, doc: &mut ui::widget::Edits<'_>, id: &str) {
        self.submit(
            doc,
            Mail::CANCEL_SEND.with(mailbox::CancelSend { id: id.to_owned() }),
        );
        self.outbox.retain(|queued| queued.id != id);
        if self.outbox_selected.as_deref() == Some(id) {
            self.outbox_selected = self.queued().last().map(|queued| queued.id.clone());
        }
        self.status = Some(Notice::success(crate::i18n::t(
            "mail-status-outbox-cancelled",
        )));
        self.dirty = true;
    }

    pub fn open_all_inboxes(&mut self) {
        self.close_outbox();
        self.selecting_all = false;
        self.all_inboxes = true;
        self.folder = Folder::Inbox;
        self.mailbox.set_account(AccountConfig::default());
        self.selected = self
            .current_threads()
            .first()
            .and_then(|thread| thread.message_ids.last().copied());
        let accounts: Vec<AccountConfig> = self
            .accounts
            .accounts
            .iter()
            .map(|account| account.config.clone())
            .filter(|config| !config.email.is_empty())
            .collect();
        for account in accounts {
            self.push_intent(Intent::Engine(EngineRequest::fetch(account, Folder::Inbox)));
        }
        self.push_intent(Intent::ShowOverview);
        self.dirty = true;
    }

    pub fn account_of(&self, id: MessageId) -> Option<AccountConfig> {
        let owner = &self.mailbox.get(id)?.account;
        self.accounts
            .accounts
            .iter()
            .find(|account| !owner.is_empty() && account.config.email == *owner)
            .map(|account| account.config.clone())
    }

    pub fn select_folder(&mut self, _doc: &mut ui::widget::Edits<'_>, folder: Folder) {
        self.close_outbox();
        self.selecting_all = false;
        self.folder = folder;
        self.selected = self
            .current_threads()
            .first()
            .and_then(|t| t.message_ids.last().copied());
        let account = self.accounts.active_config();
        if !account.email.is_empty() {
            self.push_intent(Intent::Engine(EngineRequest::fetch(account, folder)));
        }
        self.dirty = true;
    }

    pub fn reveal_folder(&mut self, folder: Folder) {
        self.close_outbox();
        self.selecting_all = false;
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

    pub fn set_bulk_selecting(&mut self, selecting: bool) {
        // The outbox lists messages that have not left, not a folder's rows:
        if selecting && self.outbox_open {
            return;
        }
        self.bulk_selecting = selecting;
        if !selecting {
            self.bulk_selection.clear();
            self.selecting_all = false;
        }
    }

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

    pub fn select_all_visible(&mut self) {
        if self.outbox_open {
            return;
        }
        self.bulk_selecting = true;
        self.select_every_row_held();
        self.dirty = true;
    }

    fn select_every_row_held(&mut self) {
        self.bulk_selection = self
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.mailbox.get(*id).is_some())
            .collect();
    }

    pub fn extend_selection(&mut self) -> bool {
        if !self.selecting_all || !self.bulk_selecting {
            return false;
        }
        let before = self.bulk_selection.len();
        let held: Vec<MessageId> = self
            .list_rows()
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| self.mailbox.get(*id).is_some())
            .collect();
        for id in held {
            if !self.bulk_selection.contains(&id) {
                self.bulk_selection.push(id);
            }
        }
        let grew = self.bulk_selection.len() != before;
        self.dirty |= grew;
        grew
    }

    pub fn finish_selection(&mut self, folder: Option<&str>) {
        self.extend_selection();
        self.selecting_all = false;
        let count = self.selected_bulk_ids().len();
        self.status = Some(Notice::success(match folder {
            Some(folder) => crate::i18n::t_args(
                "mail-status-selected-all",
                &[
                    ("count", ui::i18n::ArgValue::from(count)),
                    ("folder", ui::i18n::ArgValue::from(folder)),
                ],
            ),
            None => crate::i18n::t_args(
                "mail-status-selected-all-inboxes",
                &[("count", ui::i18n::ArgValue::from(count))],
            ),
        }));
        self.status_undo = false;
        self.dirty = true;
    }

    pub fn selection_still_arriving(&mut self) -> bool {
        if !(self.selecting_all && self.more_older) {
            return false;
        }
        self.status = Some(Notice::warning(crate::i18n::t_args(
            "mail-status-reading-folder",
            &[(
                "count",
                ui::i18n::ArgValue::from(self.selected_bulk_ids().len()),
            )],
        )));
        self.status_undo = false;
        self.dirty = true;
        true
    }

    pub fn selected_bulk_ids(&self) -> Vec<MessageId> {
        self.bulk_selection
            .iter()
            .copied()
            .filter(|id| self.mailbox.get(*id).is_some())
            .collect()
    }

    pub fn apply_bulk_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        action: BulkAction,
    ) -> usize {
        if self.selection_still_arriving() {
            return 0;
        }
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
                self.submit(
                    doc,
                    Mail::MARK_READ.with(mailbox::MarkRead {
                        messages: encoded_ids,
                        read,
                    }),
                );
                ids.len()
            }
            BulkAction::Archive => {
                for id in &ids {
                    self.mailbox.move_to(*id, Folder::Archive);
                }
                self.submit(
                    doc,
                    Mail::ARCHIVE.with(mailbox::Archive {
                        messages: encoded_ids,
                    }),
                );
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
                    self.submit(
                        doc,
                        Mail::TRASH.with(mailbox::Trash {
                            messages: move_to_trash.iter().map(ToString::to_string).collect(),
                        }),
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
                self.submit(
                    doc,
                    Mail::PURGE.with(mailbox::Purge {
                        messages: encoded_ids,
                    }),
                );
                self.bulk_selection.clear();
                self.reselect_after_remove(doc);
                ids.len()
            }
        };
        // What moved or changed state can be taken back from its notice; a
        // purge is gone for good.
        self.status_undo = affected > 0 && action != BulkAction::Purge;
        self.dirty = true;
        affected
    }

    pub(crate) fn say_undoable(
        &mut self,
        kind: ui::widget::toast::ToastKind,
        text: impl Into<String>,
    ) {
        self.status = Some(Notice::new(kind, text));
        self.status_undo = true;
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

    // ---- message changes through the mailbox ----
    //
    // Each mutates the in-memory mailbox for immediate feedback AND submits
    // the matching field edit or verb through the framework host, so the
    // change is one undo boundary, replays on peers, and is what the Ctrl+K
    // palette and agents submit. The mail server learns of it from the
    // document, as it does of a change the palette or a peer made.

    pub fn set_read(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId, read: bool) {
        if self.mailbox.get(id).is_none() {
            return;
        }
        self.register_loaded_target(doc, id);
        self.mailbox.mark_read(id, read);
        self.submit(
            doc,
            Mail::MARK_READ.with(mailbox::MarkRead {
                messages: vec![id.to_string()],
                read,
            }),
        );
    }

    pub fn toggle_star(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId) {
        let Some(target) = self.mailbox.get(id).map(|m| !m.starred) else {
            return;
        };
        self.register_loaded_target(doc, id);
        self.mailbox.toggle_star(id);
        self.submit(doc, Self::message_paths(id).starred.set(target));
        self.dirty = true;
    }

    pub fn move_message(&mut self, doc: &mut ui::widget::Edits<'_>, id: MessageId, folder: Folder) {
        if self.mailbox.get(id).is_none() {
            return;
        }
        self.register_loaded_target(doc, id);
        self.mailbox.move_to(id, folder);
        self.submit(doc, Self::message_paths(id).folder.set(folder));
    }

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
                self.say_undoable(
                    ToastKind::Success,
                    crate::i18n::t_args(
                        "mail-status-archived",
                        &[("count", ui::i18n::ArgValue::from(1u32))],
                    ),
                );
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
                    self.submit(doc, Self::message_paths(id).folder.set(to));
                    self.say_undoable(
                        ToastKind::Success,
                        crate::i18n::t_args(
                            "mail-status-trashed",
                            &[("count", ui::i18n::ArgValue::from(1u32))],
                        ),
                    );
                } else if purging {
                    self.submit(
                        doc,
                        Mail::PURGE.with(mailbox::Purge {
                            messages: vec![id.to_string()],
                        }),
                    );
                }
                self.after_remove(doc, was_selected);
            }
            CtxAction::MoveToInbox => {
                let was_selected = self.selected == Some(id);
                self.move_message(doc, id, Folder::Inbox);
                self.after_remove(doc, was_selected);
                self.say_undoable(ToastKind::Success, moved_to(Folder::Inbox));
            }
            CtxAction::Junk => {
                let was_selected = self.selected == Some(id);
                self.move_message(doc, id, Folder::Junk);
                self.after_remove(doc, was_selected);
                self.say_undoable(ToastKind::Success, moved_to(Folder::Junk));
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

pub fn now_minutes() -> i64 {
    (ui::time::unix_now() / 60) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ReadDoc;

    fn state() -> MailState {
        MailState::new_sample(crate::avatars::Avatars::new())
    }

    fn first_inbox_id(st: &MailState) -> Option<MessageId> {
        st.mailbox.messages_in(Folder::Inbox).first().map(|m| m.id)
    }

    fn steps_since(doc: &AutoCommit, before: usize) -> Vec<String> {
        ui::tilo_resource::history::versions(&mut doc.clone())
            .into_iter()
            .skip(before)
            .filter_map(|version| version.context)
            .filter(|context| context.change.is_step())
            .filter_map(|context| match context.what {
                Some(ui::tilo_actions::Did::Verb(verb)) => Some(verb.action.action),
                _ => None,
            })
            .collect()
    }

    fn version_count(doc: &AutoCommit) -> usize {
        ui::tilo_resource::history::versions(&mut doc.clone()).len()
    }

    #[test]
    fn select_all_waits_for_the_pages_that_are_still_arriving() {
        let mut st = state();
        let held = st.list_rows().len();
        assert!(held > 0, "the sample inbox holds rows");
        // What the app does on Select all when the folder has more: the rows
        // held are chosen, and the rest of the folder is on its way.
        st.select_all_visible();
        st.more_older = true;
        st.selecting_all = true;
        assert!(st.selecting_all, "the rest of the folder is being read");
        assert_eq!(st.selected_bulk_ids().len(), held);

        let mut doc = crate::resource::TestMail::doc();
        let mut edits = ui::widget::Edits::lend(&mut doc);
        assert_eq!(
            st.apply_bulk_action(&mut edits, BulkAction::MarkRead),
            0,
            "a part of the folder is not the folder"
        );
        assert_eq!(
            st.status.as_deref(),
            Some(format!("Still reading this folder; {held} selected so far").as_str())
        );

        let mut arriving = st
            .mailbox
            .messages_in(Folder::Starred)
            .into_iter()
            .next()
            .cloned()
            .expect("a message outside the inbox");
        arriving.folder = Folder::Inbox;
        arriving.id = uuid::Uuid::new_v4();
        arriving.subject = "A page that arrived while selecting".into();
        arriving.timestamp_min += 1;
        // Mail that is only here (the account the mailbox is open on shows
        // it), and its own conversation rather than a reply into one held.
        arriving.account = String::new();
        arriving.in_reply_to = None;
        arriving.rfc_message_id = None;
        st.mailbox.insert(arriving);
        let rows: Vec<MessageId> = st.list_rows().into_iter().map(|(id, _)| id).collect();
        assert!(rows.len() > held, "the folder holds one more row");
        assert!(st.extend_selection(), "the new rows joined the selection");
        assert_eq!(
            st.selected_bulk_ids().len(),
            rows.len(),
            "every row the device now holds is chosen"
        );

        st.more_older = false;
        st.finish_selection(Some("Inbox"));
        assert!(!st.selecting_all);
        assert_eq!(st.selected_bulk_ids().len(), rows.len());
        assert_eq!(
            st.status.as_deref(),
            Some(format!("Selected all {} messages in Inbox", rows.len()).as_str())
        );

        assert!(!st.selection_still_arriving());
    }

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
        let mut doc = crate::resource::TestMail::doc();
        let before = version_count(&doc);
        st.ingest_messages(
            &mut ui::widget::Edits::lend(&mut doc),
            std::slice::from_ref(&message),
        );
        assert_eq!(
            crate::resource::TestMail::get_read(&doc, &message.id.to_string()),
            Some(message.read)
        );
        assert_eq!(version_count(&doc), before + 1, "one ingest is one version");
        assert!(steps_since(&doc, before).is_empty(), "and no undo step");
    }

    #[test]
    fn reader_selection_is_read_neutral_and_reply_all_is_distinct() {
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
            let mut doc = crate::resource::TestMail::doc();
            let heads_before = doc.get_heads();
            st.move_selection(&mut ui::widget::Edits::lend(&mut doc), delta);
            assert_eq!(st.selected, Some(target));
            assert!(st.mailbox.get(target).is_some_and(|message| !message.read));
            assert_eq!(doc.get_heads(), heads_before);
        }

        {
            let mut st = state();
            let mut doc = crate::resource::TestMail::doc();
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
        let mut doc = crate::resource::TestMail::doc();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        fetch(&mut st, &mut doc, id);
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::ToggleStar,
        );
        posts_nothing(&mut st);
        assert!(matches!(
            crate::resource::TestMail::server_updates(&doc).as_slice(),
            [ServerUpdate::Flags {
                starred: true,
                folder: Folder::Inbox,
                ..
            }]
        ));

        let mut st = state();
        let mut doc = crate::resource::TestMail::doc();
        let id = first_inbox_id(&st).expect("sample corpus has an inbox message");
        fetch(&mut st, &mut doc, id);
        st.apply_message_action(
            &mut ui::widget::Edits::lend(&mut doc),
            id,
            CtxAction::Archive,
        );
        posts_nothing(&mut st);
        assert!(matches!(
            crate::resource::TestMail::server_updates(&doc).as_slice(),
            [ServerUpdate::Move {
                from: Folder::Inbox,
                to: Folder::Archive,
                ..
            }]
        ));

        let mut st = state();
        let mut doc = crate::resource::TestMail::doc();
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
        let dispatcher = st.dispatcher.clone().expect("a dispatcher");
        crate::resource::TestMail::confirm(&mut doc, &dispatcher);
        posts_nothing(&mut st);
        assert!(
            matches!(
                crate::resource::TestMail::server_updates(&doc).as_slice(),
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
        {
            let mut st = state();
            let mut doc = crate::resource::TestMail::doc();
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
                    crate::resource::TestMail::get_folder_for(&doc, &id.to_string()),
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

        {
            let mut st = state();
            let mut doc = crate::resource::TestMail::doc();
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

        {
            let mut st = state();
            let mut doc = crate::resource::TestMail::doc();
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
                crate::resource::TestMail::get_folder_for(&doc, &inbox.to_string()),
                Some(Folder::Trash)
            );
            assert!(
                crate::resource::TestMail::is_present_message(&doc, &already_trash.to_string()),
                "the row already in Trash is never purged"
            );
        }

        {
            let mut st = state();
            let mut doc = crate::resource::TestMail::doc();
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

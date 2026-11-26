//! The inboxes this host watches for its workspaces, with no Mail window
//! open.
//!
//! The host keeps no copy of the mail. The account's server stays the
//! store, and what the host keeps, in the workspace's store beside its
//! accounts, is where each watch has read up to and a bounded log of what
//! arrived: for each message its envelope and the start of its words, never
//! the whole message. Whoever needs the message reads it from its server,
//! through the host's mail actions, by where the arrival says it is held. A
//! device or a page takes the message into the mailbox when it next reads
//! the folder, as it always did.
//!
//! An inbox is watched only while something listens for its workspace
//! ([`Workspaces::listen_for_mail`]): a program with a mail trigger, or a
//! member or an agent asking what arrived. Mail that was there before an
//! inbox's first watch is no arrival, and a watch that starts again tells
//! what arrived while none ran.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::BuildHasher as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tilo_actions::remote::person_accounts::{FetchedMessage, MailArrival, MailArrivals};
use tilo_lock::MutexRecover as _;
use tilo_mail_session::{InboxHeard, InboxSeen, Watching};

use super::Workspaces;
use super::vault::VaultAt;

/// How many arrivals a workspace's log keeps, newest last.
const ARRIVALS_KEPT: usize = 500;
/// How much of a message's words an arrival keeps.
const EXCERPT_CHARS: usize = 2000;
/// The folder every arrival is in: an inbox is what a watch watches.
const INBOX: &str = "inbox";

/// An account whose inbox is watched, with the sign-in its watch runs with.
type WatchedAccount = (
    String,
    accounts::Account,
    tilo_actions::remote::person_vault::ItemRef,
);

/// What the host keeps of a workspace's arriving mail.
#[derive(Default, Serialize, Deserialize)]
struct Kept {
    /// The place the newest arrival took; the next takes the one after it.
    #[serde(default)]
    newest: u64,
    #[serde(default)]
    arrivals: VecDeque<MailArrival>,
    /// Where each account's watch has read up to, by the account's id.
    #[serde(default)]
    seen: BTreeMap<String, Seen>,
}

/// Where a watch has read up to, as it is kept.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct Seen {
    uid_validity: u32,
    uid: u32,
}

fn kept_at(root: &Path) -> PathBuf {
    root.join("mail").join("arrivals.json")
}

/// What is kept under `root`. A log that does not read starts over: its
/// watches begin where the inboxes stand now, and nothing arrives twice.
fn read_kept(root: &Path) -> Kept {
    let bytes = match std::fs::read(kept_at(root)) {
        Ok(bytes) => bytes,
        // Nothing arrived yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Kept::default(),
        Err(error) => {
            tracing::warn!(%error, "mail: a workspace's arrival log could not be read and starts over");
            return Kept::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(kept) => kept,
        Err(error) => {
            tracing::warn!(%error, "mail: a workspace's arrival log does not read and starts over");
            Kept::default()
        }
    }
}

/// Keep `kept` under `root`, for its owner alone to read.
fn write_kept(root: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&kept_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// The watches this host keeps, and who listens for them.
#[derive(Default)]
pub(super) struct MailWatches {
    state: Arc<State>,
}

#[derive(Default)]
struct State {
    /// For each workspace somebody listens for: how many do, and the watch
    /// of each of its accounts.
    listened: Mutex<HashMap<String, Listened>>,
    /// The workspaces somebody asked what arrived in, which is listening for
    /// as long as this host runs.
    asked: Mutex<HashSet<String>>,
    logs: Arc<Logs>,
    /// One bringing of the watches in line at a time: each reads the sign-ins from the
    /// workspace's vault, so two would take one sign-in out twice.
    lining_up: tokio::sync::Mutex<()>,
    /// How many bringings in line wait or run.
    pending: tokio::sync::watch::Sender<usize>,
}

/// What every watch writes into: a workspace's log, one writer at a time, and
/// the count that moves whenever a log grew.
struct Logs {
    writing: Mutex<()>,
    grew: tokio::sync::watch::Sender<u64>,
}

impl Default for Logs {
    fn default() -> Self {
        Self {
            writing: Mutex::default(),
            grew: tokio::sync::watch::channel(0).0,
        }
    }
}

#[derive(Default)]
struct Listened {
    listeners: usize,
    /// Each watched account's watch, with what it signs in by: an account
    /// whose server or sign-in changed is watched anew.
    watches: HashMap<String, (String, Watching)>,
}

impl Listened {
    /// One more listens.
    fn add(&mut self) {
        self.listeners = self.listeners.saturating_add(1);
    }
}

/// Somebody listening for a workspace's mail. While at least one is held for
/// a workspace, the host watches its inboxes.
pub struct MailListening {
    state: Arc<State>,
    workspace: String,
}

impl Drop for MailListening {
    fn drop(&mut self) {
        let mut listened = self.state.listened.lock_recover();
        let Some(entry) = listened.get_mut(&self.workspace) else {
            return;
        };
        entry.listeners = entry.listeners.saturating_sub(1);
        if entry.listeners == 0 {
            // The watches go with their last listener.
            listened.remove(&self.workspace);
        }
    }
}

impl Workspaces {
    /// Listen for `workspace`'s mail: for as long as the answer is held,
    /// this host watches the inbox of each account the workspace keeps here
    /// with its sign-in, and what arrives joins its log
    /// ([`Self::mail_arrivals`]).
    #[must_use]
    pub fn listen_for_mail(&self, workspace: &str) -> MailListening {
        let state = Arc::clone(&self.inner.mail_watches.state);
        state
            .listened
            .lock_recover()
            .entry(workspace.to_owned())
            .or_default()
            .add();
        self.watch_inboxes_of(workspace);
        MailListening {
            state,
            workspace: workspace.to_owned(),
        }
    }

    /// What arrived in `workspace` after place `after` of its log, oldest
    /// first, with the newest place the log has given out. Each arrival
    /// keeps its place and its id for good, so whoever reads with the place
    /// it got to takes each arrival once.
    #[must_use]
    pub fn mail_arrivals(&self, workspace: &str, after: u64) -> MailArrivals {
        self.arrivals_kept(workspace, after)
    }

    /// [`Self::mail_arrivals`] for what an automation hands an agent of
    /// `person`'s: where the workspace's policy does not let `person` read
    /// its mail, the answer is the newest place alone, so nothing of it is
    /// handed over and a reader that may later reads on from there.
    pub async fn mail_arrivals_for(
        &self,
        workspace: collab_types::WorkspaceId,
        person: &[u8; 32],
        after: u64,
    ) -> MailArrivals {
        self.mail_arrivals_as(
            workspace,
            (person, super::workspace_stores::ReadFor::Person),
            after,
        )
        .await
    }

    pub(crate) async fn mail_arrivals_as(
        &self,
        workspace: collab_types::WorkspaceId,
        reader: (&[u8; 32], super::workspace_stores::ReadFor<'_>),
        after: u64,
    ) -> MailArrivals {
        let mut arrived = self.arrivals_kept(&workspace.to_string(), after);
        if self
            .person_store_answer(
                workspace,
                reader,
                super::workspace_stores::Store::MAIL,
                tilo_actions::remote::person_accounts::ARRIVED,
                tilo_action_policy::ActionKind::Read,
            )
            .await
            != tilo_action_policy::Answer::Allow
        {
            arrived.arrivals.clear();
        }
        arrived
    }

    /// Moves whenever any workspace's arrival log grew: what a reader of
    /// [`Self::mail_arrivals`] wakes on.
    #[must_use]
    pub fn mail_arrival_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.mail_watches.state.logs.grew.subscribe()
    }

    /// [`Self::mail_arrivals`] as a request reads it, which is also
    /// listening: from the first time anybody asks what arrived in a
    /// workspace, this host watches its inboxes for as long as it runs.
    pub(super) fn arrived_after(&self, workspace: &str, after: u64) -> MailArrivals {
        let state = &self.inner.mail_watches.state;
        if state.asked.lock_recover().insert(workspace.to_owned()) {
            state
                .listened
                .lock_recover()
                .entry(workspace.to_owned())
                .or_default()
                .add();
            self.watch_inboxes_of(workspace);
        }
        self.arrivals_kept(workspace, after)
    }

    fn arrivals_kept(&self, workspace: &str, after: u64) -> MailArrivals {
        let state = &self.inner.mail_watches.state;
        let kept = {
            let _writing = state.logs.writing.lock_recover();
            read_kept(&self.workspace_root(workspace))
        };
        MailArrivals {
            arrivals: kept
                .arrivals
                .into_iter()
                .filter(|arrival| arrival.seq > after)
                .collect(),
            newest: kept.newest,
        }
    }

    /// Bring the watches of every workspace somebody listens for in line
    /// with the accounts it keeps now: an account added is watched, one removed is
    /// not, and one whose sign-in changed is watched with the new one.
    pub(super) fn watch_listened_inboxes(&self) {
        let workspaces: Vec<String> = self
            .inner
            .mail_watches
            .state
            .listened
            .lock_recover()
            .keys()
            .cloned()
            .collect();
        for workspace in workspaces {
            self.watch_inboxes_of(&workspace);
        }
    }

    /// Watch the inbox of each account `workspace` keeps here with a
    /// password, when somebody listens for it, and stop watching what it no
    /// longer keeps: soon after, once its sign-ins are read from the
    /// workspace's vault.
    fn watch_inboxes_of(&self, workspace: &str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("mail: inboxes are watched only where the host runs");
            return;
        };
        let state = Arc::clone(&self.inner.mail_watches.state);
        state
            .pending
            .send_modify(|pending| *pending = pending.saturating_add(1));
        let (workspaces, workspace) = (self.clone(), workspace.to_owned());
        runtime.spawn(async move {
            {
                let _one = state.lining_up.lock().await;
                workspaces.line_up_watches(&workspace).await;
            }
            state
                .pending
                .send_modify(|pending| *pending = pending.saturating_sub(1));
        });
    }

    /// Wait until every watch asked for is in line with the accounts kept: what a test reads
    /// the watches after.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn mail_watches_settled(&self) {
        let mut pending = self.inner.mail_watches.state.pending.subscribe();
        let _ = pending.wait_for(|pending| *pending == 0).await;
    }

    /// [`Self::watch_inboxes_of`], now. Each account's sign-in is resolved from the workspace's
    /// vault, its use written, only where its watch starts: a watch that runs with the sign-in
    /// its entry holds now reads nothing more than a mark of it.
    async fn line_up_watches(&self, workspace: &str) {
        use super::person_settings::kept_sign_ins::{SIGN_IN, places_of};

        let state = &self.inner.mail_watches.state;
        if !state.listened.lock_recover().contains_key(workspace) {
            return;
        }
        let root = self.workspace_root(workspace);
        let vault = self.vault_at(workspace);
        let Some(wanted) = Self::wanted_watches(vault).await else {
            return;
        };
        // The watches that run with what is kept now stay, and only the others are started.
        let starting: Vec<_> = {
            let mut listened = state.listened.lock_recover();
            let Some(entry) = listened.get_mut(workspace) else {
                return;
            };
            entry.watches.retain(|account, (signs_in, _)| {
                wanted.get(account).is_some_and(|(now, ..)| now == signs_in)
            });
            let starting = wanted
                .into_iter()
                .filter(|(id, _)| !entry.watches.contains_key(id))
                .collect();
            drop(listened);
            starting
        };
        let mut signed_in = Vec::new();
        for (id, (signs_in, account, reference)) in starting {
            let released = match places_of(&account) {
                Ok(places) => {
                    vault
                        .use_sign_in(
                            &reference,
                            (accounts::vault_items::ItemKind::Login, &places),
                            "",
                            SIGN_IN,
                        )
                        .await
                }
                Err(reason) => Err(reason),
            };
            match released {
                Ok(released) => signed_in.push((
                    id,
                    signs_in,
                    account,
                    accounts::Secret::password(released.secret.to_string()),
                )),
                Err(reason) => {
                    tracing::warn!(%reason, "mail: an inbox is not watched without its sign-in");
                }
            }
        }
        let seen = {
            let _writing = state.logs.writing.lock_recover();
            read_kept(&root).seen
        };
        let session = self.mail_session();
        let mut listened = state.listened.lock_recover();
        let Some(entry) = listened.get_mut(workspace) else {
            return;
        };
        for (id, signs_in, account, secret) in signed_in {
            let from = seen.get(&id).map(|seen| InboxSeen {
                uid_validity: seen.uid_validity,
                uid: seen.uid,
            });
            let heard = Arc::new(Heard {
                root: root.clone(),
                account: id.clone(),
                logs: Arc::clone(&state.logs),
            });
            match session.watch(&account, &secret, from, heard) {
                Ok(watching) => {
                    entry.watches.insert(id, (signs_in, watching));
                }
                Err(reason) => {
                    tracing::warn!(%reason, "mail: an inbox could not be watched");
                }
            }
        }
        drop(listened);
    }

    /// The accounts `workspace` keeps here whose inboxes are watched now, with
    /// the sign-in each runs with: none where a sign-in could not be read,
    /// which stops the watches being brought in line.
    async fn wanted_watches(vault: VaultAt<'_>) -> Option<HashMap<String, WatchedAccount>> {
        use super::person_settings::kept_sign_ins::sign_in_of;

        let held = match vault.accounts().await {
            Ok(held) => held,
            Err(reason) => {
                tracing::warn!(%reason, "mail: a workspace's accounts could not be read, so its inboxes are not watched");
                return None;
            }
        };
        let mut wanted = HashMap::new();
        for account in &held.of_type(accounts::email::TYPE_ID) {
            let setting = |name: &str| account.config.get(name);
            let waits =
                setting("import_pending").and_then(serde_json::Value::as_bool) == Some(true);
            let signs_in_elsewhere = setting("auth")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|auth| auth.eq_ignore_ascii_case("oauth"));
            // An account that waits for its sign-in, or signs in with a
            // provider this host holds no grant for, has nothing to watch
            // with.
            if waits || signs_in_elsewhere {
                continue;
            }
            let Some(reference) = sign_in_of(&account.config) else {
                continue;
            };
            let mark = match vault.sign_in_mark(&reference).await {
                Ok(Some(mark)) => mark,
                Ok(None) => continue,
                Err(reason) => {
                    tracing::warn!(%reason, "mail: a workspace's sign-ins could not be read, so its inboxes are not watched");
                    return None;
                }
            };
            wanted.insert(
                account.id.to_string(),
                (signs_in_by(account, &mark), account.clone(), reference),
            );
        }
        Some(wanted)
    }
}

/// What tells one watch of an account from another: where and as whom it
/// signs in, and a mark of its sign-in's entry that says nothing about it outside
/// this process.
fn signs_in_by(account: &accounts::Account, mark: &[u8; 32]) -> String {
    static MARKS: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    let setting = |name: &str| {
        account
            .config
            .get(name)
            .map(ToString::to_string)
            .unwrap_or_default()
    };
    format!(
        "{}\n{}\n{}\n{:x}",
        setting("imap_host"),
        setting("imap_port"),
        setting("username"),
        MARKS
            .get_or_init(std::collections::hash_map::RandomState::new)
            .hash_one(mark)
    )
}

/// What one account's watch tells: its arrivals join the workspace's log.
struct Heard {
    root: PathBuf,
    account: String,
    logs: Arc<Logs>,
}

impl InboxHeard for Heard {
    fn arrived(&self, messages: Vec<FetchedMessage>, seen: InboxSeen) {
        let _writing = self.logs.writing.lock_recover();
        let mut kept = read_kept(&self.root);
        let at_ms = i64::try_from(tilo_clock::unix_now_ns() / 1_000_000).unwrap_or(i64::MAX);
        let mut grew = false;
        for message in messages {
            let id = format!("{}:{}:{}", self.account, seen.uid_validity, message.uid);
            // An arrival told twice (a host that stopped between hearing
            // and keeping) is kept once.
            if kept.arrivals.iter().any(|arrival| arrival.id == id) {
                continue;
            }
            kept.newest = kept.newest.saturating_add(1);
            kept.arrivals.push_back(arrival_of(
                kept.newest,
                id,
                &self.account,
                seen.uid_validity,
                at_ms,
                message,
            ));
            grew = true;
        }
        while kept.arrivals.len() > ARRIVALS_KEPT {
            kept.arrivals.pop_front();
        }
        kept.seen.insert(
            self.account.clone(),
            Seen {
                uid_validity: seen.uid_validity,
                uid: seen.uid,
            },
        );
        if let Err(reason) = write_kept(&self.root, &kept) {
            // Not kept: the watch tells these again when it next starts,
            // and they are kept once then.
            tracing::warn!(%reason, "mail: what arrived could not be kept");
            return;
        }
        if grew {
            self.logs
                .grew
                .send_modify(|count| *count = count.wrapping_add(1));
        }
    }

    fn lost(&self, reason: &str) {
        tracing::debug!(%reason, "mail: an inbox's watch lost its server and tries again");
    }
}

/// One message a watch told, as the log keeps it: what it is about and
/// where its server holds it, with the start of its words.
fn arrival_of(
    seq: u64,
    id: String,
    account: &str,
    uid_validity: u32,
    at_ms: i64,
    message: FetchedMessage,
) -> MailArrival {
    MailArrival {
        seq,
        id,
        at_ms,
        account: account.to_owned(),
        folder: INBOX.to_owned(),
        uid: message.uid,
        uid_validity,
        message_id: message.message_id,
        in_reply_to: message.in_reply_to,
        from: message.from,
        to: message.to,
        cc: message.cc,
        subject: message.subject,
        excerpt: message.body.chars().take(EXCERPT_CHARS).collect(),
        attachments: message.attachments,
        minutes: message.minutes,
    }
}

/// Mail servers a test stands in for.
/// A message as an inbox holds one, for a test that stands in for the mail
/// servers: `uid` in its folder, from the person `from` names by their name
/// and their address.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn held_message(
    uid: u32,
    from: (&str, &str),
    subject: &str,
    body: &str,
) -> tilo_actions::remote::person_accounts::FetchedMessage {
    tilo_actions::remote::person_accounts::FetchedMessage {
        uid,
        message_id: Some(format!("{uid}@example.test")),
        from: tilo_actions::remote::person_accounts::MailAddress {
            name: Some(from.0.to_owned()),
            email: from.1.to_owned(),
        },
        subject: subject.to_owned(),
        body: body.to_owned(),
        minutes: 100,
        ..Default::default()
    }
}

/// A message as a send begins one, for the same tests: from `account`, to
/// `to` with `cc` in copy.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn begun_message(
    account: &str,
    (to, cc): (&str, &str),
    subject: &str,
    body: &str,
) -> serde_json::Value {
    serde_json::json!({"message": {
        "account": account,
        "fromAddress": account,
        "to": to,
        "cc": cc,
        "subject": subject,
        "body": body,
    }})
}

#[cfg(test)]
pub(crate) mod testing {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use tilo_actions::remote::person_accounts::{
        FetchMail, FetchedMail, FetchedMessage, FoundFolder, FoundMail, MarkMail, ReadMail,
        SearchMail, SendMail, SentMail,
    };
    use tilo_mail_session::{
        Attachment, InboxHeard, InboxSeen, Leaving, MailSession, Recipient, Watching,
    };

    /// One watch the host started: the account, where it starts, what it
    /// tells, and whether it was stopped.
    pub(crate) struct Started {
        pub(crate) account: String,
        pub(crate) password: String,
        pub(crate) seen: Option<InboxSeen>,
        pub(crate) heard: Arc<dyn InboxHeard>,
        pub(crate) stopped: Arc<AtomicBool>,
    }

    /// Servers a test stands in for: every watch the host starts is kept, so
    /// a test tells what arrives. A request is answered only by the ones
    /// [`Watched::answering`] makes, which keep what they were asked.
    #[derive(Default)]
    pub(crate) struct Watched {
        pub(crate) watches: Mutex<Vec<Started>>,
        answers: bool,
        asked: Mutex<Vec<String>>,
    }

    impl Watched {
        /// Servers that answer every request with one message, and keep
        /// what they were asked.
        pub(crate) fn answering() -> Self {
            Self {
                answers: true,
                ..Self::default()
            }
        }

        /// What the servers were asked, oldest first.
        pub(crate) fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }

        /// Keep `asked`, when these servers answer at all.
        fn ask(&self, asked: String) -> Result<(), String> {
            if !self.answers {
                return Err(UNASKED.into());
            }
            self.asked.lock().unwrap().push(asked);
            Ok(())
        }

        /// The watches that still run, as (account, where they started).
        pub(crate) fn running(&self) -> Vec<(String, Option<InboxSeen>)> {
            self.watches
                .lock()
                .unwrap()
                .iter()
                .filter(|started| !started.stopped.load(Ordering::SeqCst))
                .map(|started| (started.account.clone(), started.seen))
                .collect()
        }

        /// What the running watch of `account` tells its host.
        pub(crate) fn heard(&self, account: &str) -> Arc<dyn InboxHeard> {
            self.watches
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|started| {
                    started.account == account && !started.stopped.load(Ordering::SeqCst)
                })
                .map(|started| Arc::clone(&started.heard))
                .expect("the account's inbox is watched")
        }
    }

    const UNASKED: &str = "this test asks no server for that";

    impl MailSession for Watched {
        fn fetch(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            asking: &FetchMail,
        ) -> Result<FetchedMail, String> {
            self.ask(format!("fetch {} {}", account.id, asking.folder))?;
            Ok(FetchedMail {
                account: asking.account.clone(),
                folder: asking.folder.clone(),
                uid_validity: 7,
                newest_uid: 4,
                messages: vec![told(4, "bo@example.test", "Lunch", "Friday at one?")],
                ..FetchedMail::default()
            })
        }

        fn read(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            reading: &ReadMail,
        ) -> Result<FetchedMessage, String> {
            self.ask(format!(
                "read {} {} {}",
                account.id, reading.folder, reading.uid
            ))?;
            Ok(told(
                reading.uid,
                "bo@example.test",
                "Lunch",
                "Friday at one?",
            ))
        }

        fn search(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            searching: &SearchMail,
        ) -> Result<FoundMail, String> {
            let text = searching.text.as_deref().unwrap_or_default();
            self.ask(format!("search {} {text}", account.id))?;
            Ok(FoundMail {
                folders: vec![FoundFolder {
                    folder: "inbox".into(),
                    uid_validity: 7,
                    messages: vec![told(4, "bo@example.test", "Lunch", "Friday at one?")],
                }],
            })
        }

        fn save_draft(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            message: &SendMail,
            _: &[Attachment],
            _: Option<&str>,
        ) -> Result<(), String> {
            self.ask(format!(
                "draft {} to {}: {}",
                account.id, message.to, message.subject
            ))
        }

        fn send(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            leaving: &Leaving,
            _: &[Attachment],
            _: Option<&str>,
        ) -> Result<SentMail, String> {
            // Whom it is sent as when that is not the account itself, and
            // the addresses of every list it is handed over for: what a
            // test compares with what the person was shown.
            let listed = |list: &[Recipient]| {
                let addresses: Vec<&str> = list.iter().map(Recipient::address).collect();
                addresses.join(", ")
            };
            let mut whom = Vec::new();
            if leaving.sender() != account.id.as_str() {
                whom.push(format!("as {}", leaving.sender()));
            }
            whom.push(format!("to {}", listed(leaving.to())));
            for (word, copies) in [("copy to", leaving.cc()), ("blind copy to", leaving.bcc())] {
                if !copies.is_empty() {
                    whom.push(format!("{word} {}", listed(copies)));
                }
            }
            self.ask(format!(
                "send {} {}: {}",
                account.id,
                whom.join(" "),
                leaving.message().subject
            ))?;
            Ok(SentMail::default())
        }

        fn mark(
            &self,
            account: &accounts::Account,
            _: &accounts::Secret,
            _: &MarkMail,
        ) -> Result<(), String> {
            self.ask(format!("mark {}", account.id))
        }

        fn watch(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            seen: Option<InboxSeen>,
            heard: Arc<dyn InboxHeard>,
        ) -> Result<Watching, String> {
            let stopped = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&stopped);
            self.watches.lock().unwrap().push(Started {
                account: account.id.to_string(),
                password: secret.password.clone(),
                seen,
                heard,
                stopped,
            });
            Ok(Watching::new(move || stop.store(true, Ordering::SeqCst)))
        }
    }

    /// `address` kept in `workspace`'s accounts with its sign-in, an entry of the workspace's
    /// vault, as adding it from a window keeps one: kept again, it signs in with the new
    /// password under the same entry.
    pub(crate) async fn keep(
        workspaces: &crate::workspaces::Workspaces,
        workspace: &str,
        address: &str,
        password: &str,
    ) {
        use crate::workspaces::person_settings::kept_sign_ins::{places_of, sign_in_of};

        let id = accounts::AccountId::from(address.to_owned());
        let mut account = accounts::Account::new(id, accounts::email::TYPE_ID);
        account.config = serde_json::json!({
            "email": address,
            "username": address,
            "imap_host": "imap.example.test",
            "imap_port": 993,
            "smtp_host": "smtp.example.test",
            "smtp_port": 465,
        });
        let before = workspaces
            .workspace_accounts(workspace)
            .await
            .unwrap()
            .account(address)
            .and_then(|kept| sign_in_of(&kept.config));
        // Added with its password: bound to the servers it names.
        let reference = workspaces
            .vault_at(workspace)
            .keep_sign_in(
                address,
                (
                    accounts::vault_items::ItemKind::Login,
                    places_of(&account).unwrap(),
                ),
                address,
                zeroize::Zeroizing::new(password.to_owned()),
                before.as_ref(),
            )
            .await
            .unwrap();
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
        workspaces
            .keep_workspace_account(workspace, &account)
            .await
            .unwrap();
    }

    /// One message as a watch tells it.
    pub(crate) fn told(uid: u32, from: &str, subject: &str, body: &str) -> FetchedMessage {
        FetchedMessage {
            uid,
            message_id: Some(format!("{uid}@example.test")),
            from: tilo_actions::remote::person_accounts::MailAddress {
                name: None,
                email: from.to_owned(),
            },
            subject: subject.to_owned(),
            body: body.to_owned(),
            minutes: 100,
            ..FetchedMessage::default()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use tilo_mail_session::InboxSeen;

    use super::testing::{Watched, keep, told};
    use crate::workspaces::tests::{governor, test_host};

    /// Two workspaces this host keeps, by their ids.
    async fn two(workspaces: &crate::workspaces::Workspaces) -> (String, String) {
        let ada = workspaces.create("Ada").await.unwrap().workspace_id;
        let bob = workspaces.create("Bob").await.unwrap().workspace_id;
        (ada.to_string(), bob.to_string())
    }

    /// With nobody listening no inbox is watched. Listening watches the
    /// workspace's inboxes, however many listen, with its own sign-in. The
    /// watch stops with its last listener and starts again where it had
    /// read up to.
    #[tokio::test]
    async fn an_inbox_is_watched_while_somebody_listens_for_its_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let (ada, bob) = two(workspaces).await;
        let servers = Arc::new(Watched::default());
        workspaces.carry_mail_through(servers.clone());
        keep(workspaces, &ada, "ada@example.test", "adas-password").await;
        keep(workspaces, &bob, "bob@example.test", "bobs-password").await;
        assert!(servers.running().is_empty(), "nobody listens yet");

        let listening = workspaces.listen_for_mail(&ada);
        let also_listening = workspaces.listen_for_mail(&ada);
        workspaces.mail_watches_settled().await;
        assert_eq!(
            servers.running(),
            [("ada@example.test".to_owned(), None)],
            "one watch per account however many listen, and none for a workspace nobody listens for"
        );
        assert_eq!(
            servers.watches.lock().unwrap()[0].password,
            "adas-password",
            "the watch signs in with the workspace's own sign-in"
        );
        servers.heard("ada@example.test").arrived(
            Vec::new(),
            InboxSeen {
                uid_validity: 7,
                uid: 5,
            },
        );

        // The watch outlives one of two listeners and stops with the last.
        drop(listening);
        assert_eq!(servers.running().len(), 1);
        drop(also_listening);
        assert!(servers.running().is_empty());
        assert!(
            servers.watches.lock().unwrap()[0]
                .stopped
                .load(Ordering::SeqCst)
        );

        // Listened for again, the inbox is watched from where it was read
        // up to, so what arrived meanwhile is told and nothing twice.
        let _listening = workspaces.listen_for_mail(&ada);
        workspaces.mail_watches_settled().await;
        assert_eq!(
            servers.running(),
            [(
                "ada@example.test".to_owned(),
                Some(InboxSeen {
                    uid_validity: 7,
                    uid: 5
                })
            )]
        );
    }

    /// What arrives is kept once, with a place and an id that stay, as the
    /// start of the message and never the whole, and another workspace's log
    /// holds none of it.
    #[tokio::test]
    async fn what_arrives_is_kept_once_for_its_workspace_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let (ada, bob) = two(workspaces).await;
        let servers = Arc::new(Watched::default());
        workspaces.carry_mail_through(servers.clone());
        keep(workspaces, &ada, "ada@example.test", "adas-password").await;
        keep(workspaces, &bob, "bob@example.test", "bobs-password").await;
        let _listening = workspaces.listen_for_mail(&ada);
        workspaces.mail_watches_settled().await;
        let mut grew = workspaces.mail_arrival_changes();
        grew.mark_unchanged();

        // The first look tells where the watch starts, and no arrival.
        let heard = servers.heard("ada@example.test");
        heard.arrived(
            Vec::new(),
            InboxSeen {
                uid_validity: 7,
                uid: 3,
            },
        );
        assert!(workspaces.mail_arrivals(&ada, 0).arrivals.is_empty());
        assert!(!grew.has_changed().unwrap());

        let long = "word ".repeat(2000);
        heard.arrived(
            vec![
                told(
                    4,
                    "jonas@northzone.example",
                    "Cap table",
                    "Could you send it?",
                ),
                told(5, "ana@veeza.example", "Long", &long),
            ],
            InboxSeen {
                uid_validity: 7,
                uid: 5,
            },
        );
        assert!(grew.has_changed().unwrap(), "a reader is woken");
        let arrived = workspaces.mail_arrivals(&ada, 0).arrivals;
        assert_eq!(
            arrived
                .iter()
                .map(|arrival| (arrival.seq, arrival.id.as_str(), arrival.subject.as_str()))
                .collect::<Vec<_>>(),
            [
                (1, "ada@example.test:7:4", "Cap table"),
                (2, "ada@example.test:7:5", "Long"),
            ]
        );
        assert_eq!(arrived[0].account, "ada@example.test");
        assert_eq!(arrived[0].folder, "inbox");
        assert_eq!((arrived[0].uid, arrived[0].uid_validity), (4, 7));
        assert_eq!(arrived[0].from.email, "jonas@northzone.example");
        assert_eq!(arrived[0].excerpt, "Could you send it?");
        assert_eq!(
            arrived[1].excerpt.chars().count(),
            2000,
            "an arrival keeps the start of a message, never the whole"
        );

        // Told again, an arrival is kept once, and a reader that names the
        // place it got to takes only what came after.
        heard.arrived(
            vec![told(5, "ana@veeza.example", "Long", &long)],
            InboxSeen {
                uid_validity: 7,
                uid: 5,
            },
        );
        assert_eq!(workspaces.mail_arrivals(&ada, 0).arrivals.len(), 2);
        assert_eq!(
            workspaces
                .mail_arrivals(&ada, 1)
                .arrivals
                .iter()
                .map(|arrival| arrival.seq)
                .collect::<Vec<_>>(),
            [2]
        );
        assert!(
            workspaces.mail_arrivals(&bob, 0).arrivals.is_empty(),
            "a workspace's arrivals are its own"
        );
    }

    /// The watches follow the accounts a listened workspace keeps: one
    /// added is watched, one removed is not, and one whose sign-in changed
    /// is watched with the new one.
    #[tokio::test]
    async fn the_watches_follow_the_accounts_a_workspace_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_host(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let (ada, _) = two(workspaces).await;
        let servers = Arc::new(Watched::default());
        workspaces.carry_mail_through(servers.clone());
        keep(workspaces, &ada, "ada@example.test", "first-password").await;
        let _listening = workspaces.listen_for_mail(&ada);
        workspaces.mail_watches_settled().await;
        assert_eq!(servers.running().len(), 1);

        keep(workspaces, &ada, "ada@work.example.test", "work-password").await;
        workspaces.person_settings_changed();
        workspaces.mail_watches_settled().await;
        let mut watched: Vec<String> = servers
            .running()
            .into_iter()
            .map(|(account, _)| account)
            .collect();
        watched.sort();
        assert_eq!(watched, ["ada@example.test", "ada@work.example.test"]);

        // A changed sign-in stops the watch that signed in with the old
        // one and starts one with the new.
        keep(workspaces, &ada, "ada@example.test", "second-password").await;
        workspaces.person_settings_changed();
        workspaces.mail_watches_settled().await;
        let passwords: Vec<String> = servers
            .watches
            .lock()
            .unwrap()
            .iter()
            .filter(|started| {
                started.account == "ada@example.test" && !started.stopped.load(Ordering::SeqCst)
            })
            .map(|started| started.password.clone())
            .collect();
        assert_eq!(passwords, ["second-password"]);

        assert!(
            workspaces
                .remove_workspace_account(&ada, "ada@work.example.test")
                .await
                .unwrap()
        );
        workspaces.person_settings_changed();
        workspaces.mail_watches_settled().await;
        assert_eq!(
            servers
                .running()
                .into_iter()
                .map(|(account, _)| account)
                .collect::<Vec<_>>(),
            ["ada@example.test"]
        );
    }
}

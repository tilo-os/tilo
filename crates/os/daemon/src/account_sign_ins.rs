//! The accounts of the workspaces this daemon holds, and their sign-ins, for this login's cloud
//! service and for the origin connectors this daemon starts ([`accounts::sign_ins`]).
//!
//! The accounts come from each workspace's published accounts record. A sign-in is released as
//! any app's is ([`Workspaces::account_secret`]): for an enabled account that names the entry, at
//! its servers, decided by the person's grant on Passwords and on the app asking, with a use
//! written into the entry first.
//!
//! One socket below the runtime directory, which only this account's processes reach, answers
//! the cloud service: it is told the file accounts of every workspace, and the use names Files,
//! whose `~/Remote` folders the mounts are. An origin connector is handed a connection of its own
//! when the daemon starts it ([`AccountSignIns::hand`]): it is told its workspace's accounts, and
//! the use names its app.
//!
//! The model manager reaches a socket of its own beside the cloud service's: it is told the
//! workspaces' Hugging Face accounts, and the use names Settings, whose Agent Models downloads
//! sign in with them. It alone may keep a Hugging Face sign-in made in a terminal, and forget the
//! account with its sign-in, as Settings keeps them.
//!
//! Each of them reports how an account it is told synced, which the daemon writes into the
//! workspace's accounts as this device's report.
//!
//! An ask for accounts that changed since a revision is held until a workspace's accounts record
//! changes, as the daemon's own feed of record writes tells it, and let go once the asker goes
//! away.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use accounts::sign_ins::{self, Answer, Ask, Held};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_socket::tokio::{Listener, Stream};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::account_logins::{AccountLogin, accounts_document};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::sync::broadcast::error::RecvError;

/// What a connector's ask for the sign-in of another workspace's account is refused with.
pub const OTHER_WORKSPACE: &str = "That account is another workspace's";

/// What an ask for a sign-in that names no vault entry is refused with.
pub const NOT_AN_ENTRY: &str = "This account's secret is not available to this app";

/// What a report of an account the connection is not told is refused with.
pub const NOT_TOLD: &str = "That account is not one this connection is told";

/// What an ask to keep or forget a sign-in is refused with on a connection that may not.
pub const KEEPS_NONE: &str = "This connection keeps no account's sign-in";

/// Whose accounts a connection is told, and whose sign-ins its asks are.
struct Scope {
    /// The one workspace whose accounts it is told; every workspace this device holds, without.
    workspace: Option<String>,
    /// Which accounts it is told.
    keep: fn(&accounts::Account) -> bool,
    /// The app a sign-in's use says signed in.
    signs_in_for: String,
    /// Whether it may keep the sign-in of the Hugging Face account, and forget the accounts it
    /// is told with theirs: the model manager's alone.
    keeps_sign_ins: bool,
}

impl Scope {
    /// This login's cloud service: the file accounts of every workspace, for Files' mounts.
    fn cloud_service() -> Self {
        Self {
            workspace: None,
            keep: sign_ins::is_file_account,
            signs_in_for: tilo_app_id::apps::FILE_EXPLORER.to_owned(),
            keeps_sign_ins: false,
        }
    }

    /// This login's model manager: the Hugging Face accounts of every workspace, for the Agent
    /// Models Settings shows, whose downloads they sign in to.
    fn model_manager() -> Self {
        Self {
            workspace: None,
            keep: sign_ins::is_model_hub_account,
            signs_in_for: tilo_app_id::apps::SETTINGS.to_owned(),
            keeps_sign_ins: true,
        }
    }

    /// The origin connector of `app` in `workspace`: every account of that workspace, for `app`.
    fn connector(app: String, workspace: String) -> Self {
        Self {
            workspace: Some(workspace),
            keep: |_| true,
            signs_in_for: app,
            keeps_sign_ins: false,
        }
    }
}

/// What answers the connections of the account sign-ins.
struct Answering {
    workspaces: Workspaces,
    by: Caller,
    /// How many asks wait on a change of the accounts now.
    waiting: Arc<AtomicUsize>,
}

/// The account sign-ins socket while it is served: stops taking connections, and lets go of the
/// ones it answers, when dropped.
pub struct AccountSignIns {
    path: PathBuf,
    serving: tokio::task::JoinHandle<()>,
    /// The model manager's socket, served beside the cloud service's.
    models: tokio::task::JoinHandle<()>,
    answering: Arc<Answering>,
    waiting: Arc<AtomicUsize>,
}

impl AccountSignIns {
    /// Where the socket is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many asks for changed accounts are held open now, waiting on a change of a
    /// workspace's accounts record.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    /// Hand the origin connector of `app` in `workspace` behind `connector` its account
    /// sign-ins: a connection told that workspace's accounts alone, whose sign-ins' uses name
    /// `app`. It is made here, so it needs no vouch; it ends when the connector lets go of it.
    ///
    /// # Errors
    /// The connection cannot be made, or the connector took none.
    pub fn hand(
        &self,
        connector: &runtime_portal::origin_connector::Descriptor,
        app: &str,
        workspace: &str,
    ) -> std::io::Result<()> {
        hand(&self.answering, connector, app, workspace)
    }

    /// What hands a connector its account sign-ins as [`Self::hand`] does, for as long as it is
    /// held, served or not.
    #[must_use]
    pub fn hands(
        &self,
    ) -> impl Fn(&runtime_portal::origin_connector::Descriptor, &str, &str) -> std::io::Result<()>
    + Send
    + Sync
    + 'static {
        let answering = Arc::clone(&self.answering);
        move |connector, app, workspace| hand(&answering, connector, app, workspace)
    }
}

/// Hand the connector behind `connector` a connection `answering` answers as `app`'s connector in
/// `workspace`.
fn hand(
    answering: &Arc<Answering>,
    connector: &runtime_portal::origin_connector::Descriptor,
    app: &str,
    workspace: &str,
) -> std::io::Result<()> {
    let (ours, theirs) = tilo_socket::pair()?;
    runtime_portal::origin_connector::send_sign_ins(
        connector,
        runtime_portal::packet::descriptor_of(theirs),
    )?;
    let answering = Arc::clone(answering);
    let scope = Scope::connector(app.to_owned(), workspace.to_owned());
    // Taken on the runtime that answers it: the caller may be on no runtime at all.
    tilo_rt::app().spawn(async move {
        let answering_it = async {
            let (read, write) = tokio::io::split(tilo_socket::tokio::from_std(ours)?);
            answered(BufReader::new(read), write, &scope, &answering).await
        };
        if let Err(error) = answering_it.await {
            tilo_failure::ignored!(
                "a connection that ends is its connector's to end, and nothing here waits on it",
                tracing::debug!(%error, "a connector's account sign-ins ended")
            );
        }
    });
    Ok(())
}

impl Drop for AccountSignIns {
    fn drop(&mut self) {
        self.serving.abort();
        self.models.abort();
    }
}

/// Serve the account sign-ins at `path`, and the model manager's at `models`, from the
/// workspaces `workspaces` holds, answering as `by`, the daemon's person.
///
/// # Errors
/// A socket's directory or a socket cannot be made.
pub fn serve(
    path: &Path,
    models: &Path,
    workspaces: Workspaces,
    by: Caller,
) -> std::io::Result<AccountSignIns> {
    let bound = bind(path)?;
    let models_bound = bind(models)?;
    let waiting = Arc::new(AtomicUsize::new(0));
    let answering = Arc::new(Answering {
        workspaces,
        by,
        waiting: Arc::clone(&waiting),
    });
    let served = |bound, at: &Path, scope: fn() -> Scope| {
        let at = at.to_owned();
        let served = Arc::clone(&answering);
        tilo_rt::app().spawn(async move {
            if let Err(error) = serving(bound, served, scope).await {
                tracing::warn!(%error, path = %at.display(), "an account sign-ins socket stopped");
            }
        })
    };
    let serving = served(bound, path, Scope::cloud_service);
    let models = served(models_bound, models, Scope::model_manager);
    Ok(AccountSignIns {
        path: path.to_owned(),
        serving,
        models,
        answering,
        waiting,
    })
}

/// Bind `path` in a directory only this account reaches, the socket itself only this account's.
fn bind(path: &Path) -> std::io::Result<tilo_socket::Listener> {
    let directory = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    tilo_files::native::private::directory(&directory)?;
    tilo_files::native::private::bind_socket(path)
}

/// Answer every connection on `bound` within the scope `scope` makes; the connections answered
/// end with it.
async fn serving(
    bound: tilo_socket::Listener,
    answering: Arc<Answering>,
    scope: fn() -> Scope,
) -> std::io::Result<()> {
    let listener: Listener = tilo_socket::tokio::listener(bound)?;
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let stream = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            Some(_) = connections.join_next() => continue,
        };
        if !own(&stream) {
            tracing::warn!("a process of another user reached the account sign-ins socket");
            continue;
        }
        let answering = Arc::clone(&answering);
        connections.spawn(async move {
            if let Err(error) = connection(stream, &answering, &scope()).await {
                tilo_failure::ignored!(
                    "a connection that ends is its asker's to end, and nothing here waits on it",
                    tracing::debug!(%error, "an account sign-ins connection ended")
                );
            }
        });
    }
}

/// Whether the peer is a process of this login's user: on Windows, every peer the pipe took.
fn own(stream: &Stream) -> bool {
    #[cfg(unix)]
    {
        stream
            .peer_cred()
            .is_ok_and(|peer| peer.uid() == rustix::process::getuid().as_raw())
    }
    #[cfg(windows)]
    {
        let _ = stream;
        true
    }
}

/// A connection to a socket, answered within `scope`.
async fn connection(stream: Stream, answering: &Answering, scope: &Scope) -> std::io::Result<()> {
    let (read, write) = tokio::io::split(stream);
    answered(BufReader::new(read), write, scope, answering).await
}

/// Answer every ask on `read` within `scope`, on `write`, until the asker goes away.
async fn answered(
    mut read: BufReader<ReadHalf<Stream>>,
    mut write: WriteHalf<Stream>,
    scope: &Scope,
    answering: &Answering,
) -> std::io::Result<()> {
    loop {
        let mut line = zeroize::Zeroizing::new(Vec::new());
        let limit = u64::try_from(sign_ins::MAX_LINE).map_or(u64::MAX, |max| max.saturating_add(1));
        if (&mut read).take(limit).read_until(b'\n', &mut line).await? == 0 {
            return Ok(());
        }
        if line.pop() != Some(b'\n') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "a line of the account sign-ins socket is too long",
            ));
        }
        let answer = match serde_json::from_slice::<Ask>(&line) {
            // An asker that went away is answered nothing: what its ask waited on is let go.
            Ok(ask) => tokio::select! {
                biased;
                answer = answer(answering, scope, ask) => answer,
                gone = gone(&mut read) => return gone,
            },
            Err(error) => Answer::Refused {
                error: format!("That is no ask of the account sign-ins socket: {error}"),
            },
        };
        write.write_all(&sign_ins::answer_line(&answer)?).await?;
    }
}

/// Once the asker on `read` went away: the end of its connection, or a read of it that failed.
/// An asker that asks again before its answer is kept waiting for it.
async fn gone(read: &mut BufReader<ReadHalf<Stream>>) -> std::io::Result<()> {
    if read.fill_buf().await?.is_empty() {
        return Ok(());
    }
    std::future::pending().await
}

/// An ask counted as waiting on a change of the accounts, for as long as it is held.
struct Waiting<'a>(&'a AtomicUsize);

impl<'a> Waiting<'a> {
    fn on(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The answer to `ask` within `scope`. An ask for accounts that changed since a revision is held
/// until a workspace's accounts record changes, and answered with what the accounts are then, or
/// with what they still are at [`sign_ins::LONGEST_WAIT`].
async fn answer(answering: &Answering, scope: &Scope, ask: Ask) -> Answer {
    match ask {
        Ask::Accounts { after } => {
            let now = tokio::time::Instant::now();
            let deadline = now.checked_add(sign_ins::LONGEST_WAIT).unwrap_or(now);
            // Followed before the accounts are read, so a change while they are is not missed.
            let mut writes = answering.workspaces.document_writes();
            let record = accounts_document();
            loop {
                let accounts: Vec<Held> = answering
                    .workspaces
                    .held_accounts(scope.keep)
                    .await
                    .into_iter()
                    .filter(|(workspace, _, _)| {
                        scope
                            .workspace
                            .as_ref()
                            .is_none_or(|only| only == workspace)
                    })
                    .map(|(workspace, account, sync)| Held {
                        workspace,
                        account,
                        signed_in: sync.is_some_and(|sync| sync.signed_in_ms.is_some()),
                    })
                    .collect();
                let revision = sign_ins::revision_of(&accounts);
                if after.as_deref() != Some(revision.as_str()) {
                    return Answer::Accounts { revision, accounts };
                }
                let _waiting = Waiting::on(&answering.waiting);
                loop {
                    match tokio::time::timeout_at(deadline, writes.recv()).await {
                        Ok(Ok(write))
                            if write.app == accounts::person::ACCOUNTS_OWNER
                                && write.record == record =>
                        {
                            break;
                        }
                        Ok(Ok(_)) => {}
                        // Writes went by untold: the accounts are read again.
                        Ok(Err(RecvError::Lagged(_))) => break,
                        Ok(Err(RecvError::Closed)) | Err(_) => {
                            return Answer::Accounts { revision, accounts };
                        }
                    }
                }
            }
        }
        Ask::SignIn { workspace, item } => {
            if scope
                .workspace
                .as_ref()
                .is_some_and(|only| *only != workspace)
            {
                return Answer::Refused {
                    error: OTHER_WORKSPACE.into(),
                };
            }
            match released(answering, &workspace, &scope.signs_in_for, &item).await {
                Ok(secret) => Answer::SignIn { secret },
                Err(error) => Answer::Refused { error },
            }
        }
        Ask::ReportSync {
            workspace,
            account,
            health,
            signed_in,
            message,
        } => {
            if let Err(error) = told(answering, scope, &workspace, &account).await {
                return Answer::Refused { error };
            }
            let by = answering.by.clone().in_workspace(workspace.clone());
            match answering
                .workspaces
                .report_account_sync(
                    (&workspace, by.acting(), None),
                    &account,
                    health,
                    signed_in,
                    &message,
                )
                .await
            {
                Ok(()) => Answer::Reported,
                Err(error) => Answer::Refused { error },
            }
        }
        Ask::Keep {
            workspace,
            secret,
            username,
            name,
        } if scope.keeps_sign_ins => {
            keep_model_hub(answering, scope, workspace, secret, (username, name))
                .await
                .unwrap_or_else(|error| Answer::Refused { error })
        }
        Ask::Forget { workspace, account } if scope.keeps_sign_ins => {
            if let Err(error) = told(answering, scope, &workspace, &account).await {
                return Answer::Refused { error };
            }
            let by = answering.by.clone().in_workspace(workspace.clone());
            match answering
                .workspaces
                .forget_workspace_account(&workspace, (&by, None), &account)
                .await
            {
                Ok(_) => Answer::Reported,
                Err(error) => Answer::Refused { error },
            }
        }
        Ask::Keep { .. } | Ask::Forget { .. } => Answer::Refused {
            error: KEEPS_NONE.into(),
        },
    }
}

/// Whether the connection within `scope` is told `workspace`'s account `account`: a connection
/// reports and forgets only the accounts it is told.
///
/// # Errors
/// It is not, said in words.
async fn told(
    answering: &Answering,
    scope: &Scope,
    workspace: &str,
    account: &str,
) -> Result<(), String> {
    if scope
        .workspace
        .as_ref()
        .is_some_and(|only| only != workspace)
    {
        return Err(OTHER_WORKSPACE.into());
    }
    let told = answering
        .workspaces
        .held_accounts(scope.keep)
        .await
        .into_iter()
        .any(|(held, kept, _)| held == workspace && kept.id.as_str() == account);
    if told { Ok(()) } else { Err(NOT_TOLD.into()) }
}

/// Keep `secret` as the sign-in of `workspace`'s Hugging Face account, signed in as `username`
/// (and `name`), as Settings keeps it: the secret in the account's entry of the workspace's vault
/// (a new one where it names none), bound to Hugging Face alone, and the account naming it.
/// Without a workspace, the one whose Hugging Face account it replaces, else the person's own.
async fn keep_model_hub(
    answering: &Answering,
    scope: &Scope,
    workspace: Option<String>,
    secret: sign_ins::Withheld,
    (username, name): (String, Option<String>),
) -> Result<Answer, String> {
    let workspace = match workspace {
        Some(workspace) => workspace,
        None => {
            let kept = answering.workspaces.held_accounts(scope.keep).await;
            match kept.into_iter().next() {
                Some((workspace, _, _)) => workspace,
                None => answering
                    .by
                    .user_public_key()
                    .map(tilo_server_protocol::servers::person_workspace_id)
                    .ok_or_else(|| NOT_AN_ENTRY.to_owned())?,
            }
        }
    };
    let by = answering.by.clone().in_workspace(workspace.clone());
    // A sign-in made anew replaces the one the account's entry keeps.
    let item = answering
        .workspaces
        .workspace_accounts(&workspace)
        .await?
        .account(sign_ins::MODEL_HUB_ACCOUNT)
        .and_then(|kept| accounts::email::password_item(&kept.config).map(str::to_owned))
        .and_then(|item| ItemRef::parse(&item));
    let place = accounts::vault_items::Place::parse(sign_ins::MODEL_HUB_ORIGIN)
        .map_err(|error| error.to_string())?;
    let kept = answering
        .workspaces
        .keep_account_password(
            &workspace,
            (&by, None),
            item.as_ref(),
            AccountLogin {
                title: sign_ins::MODEL_HUB_ACCOUNT.to_owned(),
                username: username.clone(),
                places: vec![place],
            },
            secret.0,
        )
        .await?;
    let account = sign_ins::model_hub_account(&kept.to_string(), Some(&username), name.as_deref());
    answering
        .workspaces
        .keep_workspace_account((&workspace, by.acting(), None), &account)
        .await?;
    Ok(Answer::Reported)
}

/// The sign-in kept in the vault entry `item` names, for `app` in `workspace` as the daemon's
/// person: what [`Workspaces::account_secret`] releases, logged as every release of an account's
/// password is.
async fn released(
    answering: &Answering,
    workspace: &str,
    app: &str,
    item: &str,
) -> Result<zeroize::Zeroizing<String>, String> {
    const LOG: &str = tilo_startup_metrics::ACCOUNT_PASSWORDS_TARGET;
    let item = ItemRef::parse(item).ok_or(NOT_AN_ENTRY)?;
    let by = answering.by.clone().in_workspace(workspace.to_owned());
    match answering
        .workspaces
        .account_secret(workspace, &by, app, &item)
        .await
    {
        Ok(released) => {
            tracing::info!(target: LOG, %item, app, "released an account's sign-in from the workspace's vault");
            Ok(released.secret)
        }
        Err(error) => {
            tracing::warn!(target: LOG, %item, app, %error, "an account's sign-in was not released");
            Err(error)
        }
    }
}

//! The accounts of the workspaces this device holds, and their sign-ins, for this login's cloud
//! service and for the origin connectors the session starts ([`accounts::sign_ins`]).
//!
//! The accounts come from each workspace's published accounts record, and a sign-in is released
//! as any app's engine's is ([`crate::account_passwords`]): for an enabled account that names the
//! entry, at its servers, decided by the person's grant on Passwords, with a use written into the
//! entry first.
//!
//! The cloud service reaches a socket below the runtime directory: it is told the file accounts of
//! every workspace, and the use names Files, whose `~/Remote` folders the mounts are. Each origin
//! connector is handed a connection of its own when it starts ([`hand`]), scoped to its
//! workspace and app: it is told that workspace's accounts, and the use names the app.
//!
//! An ask for accounts that changed since a revision is held until a workspace's accounts record
//! changes, as the session's one stream of record changes tells it, and let go once the asker
//! goes away.

use accounts::sign_ins::{self, Answer, Ask, Held};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::account_logins::DECLINED;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::platform::records::Changes;

/// The longest a refused sign-in is held to be decided again as the workspace's copy on this
/// device catches up ([`sign_in`]).
const SIGN_IN_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// What a connector's ask for the sign-in of another workspace's account is refused with.
const OTHER_WORKSPACE: &str = "That account is another workspace's";

/// Whose accounts a connection is told, and whose sign-ins its asks are.
#[derive(Clone)]
pub(crate) struct Scope {
    /// The one workspace whose accounts it is told; every workspace this device holds, without.
    workspace: Option<String>,
    /// Which accounts it is told.
    keep: fn(&accounts::Account) -> bool,
    /// The app a sign-in's use says signed in.
    signs_in_for: String,
    /// The entries whose sign-in the person said no to while this connection lasts: not asked
    /// about again until the app asks anew.
    declined: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
}

impl Scope {
    /// This login's cloud service: the file accounts of every workspace, for Files' mounts.
    fn cloud_service() -> Self {
        Self {
            workspace: None,
            keep: sign_ins::is_file_account,
            signs_in_for: tilo_app_id::apps::FILE_EXPLORER.to_owned(),
            declined: std::sync::Arc::default(),
        }
    }

    /// The origin connector of `app` in `workspace`: every account of that workspace, for `app`.
    pub(crate) fn connector(app: &str, workspace: &str) -> Self {
        Self {
            workspace: Some(workspace.to_owned()),
            keep: |_| true,
            signs_in_for: app.to_owned(),
            declined: std::sync::Arc::default(),
        }
    }
}

/// What the connections handed to connectors are answered with: until this session's workspaces
/// are open, they wait.
enum Served {
    Waiting(Vec<(std::os::unix::net::UnixStream, Scope)>),
    Open(Box<Answering>),
}

/// What answers the connections of the account sign-ins.
#[derive(Clone)]
struct Answering {
    workspaces: Workspaces,
    by: Caller,
    changes: Changes,
    tasks: tokio::runtime::Handle,
    /// How long a refused sign-in is held to be decided again: [`SIGN_IN_WAIT`].
    sign_in_wait: std::time::Duration,
}

static SERVED: std::sync::Mutex<Served> = std::sync::Mutex::new(Served::Waiting(Vec::new()));

/// Hand the origin connector of `app` in `workspace` on `connector` its account sign-ins,
/// answered now when this session's workspaces are open, else once they are.
pub(crate) fn hand(connector: &OwnedFd, app: &str, workspace: &str) -> std::io::Result<()> {
    let (stream, connectors) = std::os::unix::net::UnixStream::pair()?;
    runtime_portal::origin_connector::send_sign_ins(connector, connectors.into())?;
    let scope = Scope::connector(app, workspace);
    let mut served = SERVED.lock_recover();
    match &mut *served {
        Served::Waiting(waiting) => waiting.push((stream, scope)),
        Served::Open(open) => open.answer(stream, scope),
    }
    Ok(())
}

impl Answering {
    /// Answering as `by`, on this runtime.
    fn here(workspaces: Workspaces, by: Caller, changes: Changes) -> Self {
        Self {
            workspaces,
            by,
            changes,
            tasks: tokio::runtime::Handle::current(),
            sign_in_wait: SIGN_IN_WAIT,
        }
    }

    /// Answer `stream` within `scope` until its asker ends it.
    fn answer(&self, stream: std::os::unix::net::UnixStream, scope: Scope) {
        let answering = self.clone();
        self.tasks.spawn(async move {
            let stream = match stream
                .set_nonblocking(true)
                .and_then(|()| UnixStream::from_std(stream))
            {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(%error, app = scope.signs_in_for, "an account sign-ins connection is not answered");
                    return;
                }
            };
            if let Err(error) = connection(stream, &answering, &scope).await {
                tilo_failure::ignored!(
                    "a connection that ends is its asker's to end, and nothing here waits on it",
                    tracing::debug!(%error, app = scope.signs_in_for, "an account sign-ins connection ended")
                );
            }
        });
    }
}

/// Serve the socket below this login's runtime directory, and the connections handed to origin
/// connectors, answering as `by`, the device's person, and holding an ask for changed accounts
/// until `changes` tells of one.
pub(crate) fn serve(workspaces: Workspaces, by: Caller, changes: Changes) {
    let answering = open(Answering::here(workspaces, by, changes));
    let Some(path) = sign_ins::socket() else {
        tracing::warn!("no runtime directory, so the cloud service reaches no account's sign-in");
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = serving(&path, &answering).await {
            tracing::warn!(%error, path = %path.display(), "the account sign-ins socket stopped");
        }
    });
}

/// Answer the connections handed to connectors with `answering`, those waiting and those to come.
fn open(answering: Answering) -> Answering {
    let open = Served::Open(Box::new(answering.clone()));
    let before = std::mem::replace(&mut *SERVED.lock_recover(), open);
    if let Served::Waiting(waiting) = before {
        for (stream, scope) in waiting {
            answering.answer(stream, scope);
        }
    }
    answering
}

/// Bind `path` and answer every connection on it.
///
/// # Errors
/// The socket's directory or the socket cannot be made.
async fn serving(path: &Path, answering: &Answering) -> std::io::Result<()> {
    let listener = bind(path)?;
    loop {
        let (stream, _) = listener.accept().await?;
        if !own(&stream) {
            tracing::warn!("a process of another user reached the account sign-ins socket");
            continue;
        }
        answering.answer(stream.into_std()?, Scope::cloud_service());
    }
}

fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let directory = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let bound = tilo_files::native::socket::bind(path)?;
    bound.set_nonblocking(true)?;
    let listener = UnixListener::from_std(bound)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Whether the peer is a process of this login's user.
fn own(stream: &UnixStream) -> bool {
    stream
        .peer_cred()
        .is_ok_and(|peer| peer.uid() == rustix::process::getuid().as_raw())
}

async fn connection(
    stream: UnixStream,
    answering: &Answering,
    scope: &Scope,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
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
async fn gone(read: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> std::io::Result<()> {
    if read.fill_buf().await?.is_empty() {
        return Ok(());
    }
    std::future::pending().await
}

/// The answer to `ask` within `scope`. An ask for accounts that changed since a revision is held
/// until a workspace's accounts record changes, as `changes` tells it, and answered with what the
/// accounts are then, or with what they still are at [`sign_ins::LONGEST_WAIT`].
async fn answer(answering: &Answering, scope: &Scope, ask: Ask) -> Answer {
    match ask {
        Ask::Accounts { after } => {
            let now = tokio::time::Instant::now();
            let deadline = now.checked_add(sign_ins::LONGEST_WAIT).unwrap_or(now);
            // Followed before the accounts are read, so a change while they are is not missed.
            let mut told = answering.changes.waiter();
            let record = tilo_workspace_service::workspaces::account_logins::accounts_document();
            loop {
                let accounts: Vec<Held> = answering
                    .workspaces
                    .held_accounts(scope.keep)
                    .await
                    .into_iter()
                    .filter(|(workspace, _)| {
                        scope
                            .workspace
                            .as_ref()
                            .is_none_or(|only| only == workspace)
                    })
                    .map(|(workspace, account)| Held { workspace, account })
                    .collect();
                let revision = sign_ins::revision_of(&accounts);
                if after.as_deref() != Some(revision.as_str()) {
                    return Answer::Accounts { revision, accounts };
                }
                loop {
                    match tokio::time::timeout_at(deadline, told.recv()).await {
                        Ok(Some(change))
                            if change.app == accounts::person::ACCOUNTS_OWNER
                                && change.record == record =>
                        {
                            break;
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => return Answer::Accounts { revision, accounts },
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
            let by = answering.by.clone().in_workspace(workspace.clone());
            sign_in(answering, scope, (&workspace, &item), &by).await
        }
    }
}

/// The sign-in kept in `item` for `workspace`'s account, as `by`: released, once the workspace's
/// host asked its person where its rules ask first, or refused. A refusal is decided again each time the workspace's records or rules change on this
/// device, until [`SIGN_IN_WAIT`] passed: a device that just joined, or just had the entry kept
/// on another device, holds the workspace's rules, the vault's keys and its document a moment
/// after the account names the entry, and a sign-in asked in that moment is answered once they
/// are here rather than refused for good.
async fn sign_in(
    answering: &Answering,
    scope: &Scope,
    (workspace, item): (&str, &str),
    by: &Caller,
) -> Answer {
    if scope.declined.lock_recover().contains(item) {
        return Answer::Refused {
            error: DECLINED.into(),
        };
    }
    let now = tokio::time::Instant::now();
    let deadline = now.checked_add(answering.sign_in_wait).unwrap_or(now);
    // Followed before the first decision, so a change while it is made is not missed.
    let mut records = answering.changes.waiter();
    let mut rules = answering.workspaces.governance_changes();
    loop {
        let command = Command::ReleaseAccountSecret {
            item: item.to_owned(),
        };
        let refused = match crate::account_passwords::answer(
            &answering.workspaces,
            workspace,
            &scope.signs_in_for,
            by,
            command,
        )
        .await
        {
            Ok(Reply::AccountSecret { secret, .. }) => return Answer::SignIn { secret },
            Ok(_) => crate::account_passwords::REFUSED.to_owned(),
            // The person said no when the workspace's host asked them: it is theirs to ask again.
            Err(error) if error == DECLINED => {
                scope.declined.lock_recover().insert(item.to_owned());
                return Answer::Refused { error };
            }
            Err(error) => error,
        };
        let changed = async {
            loop {
                tokio::select! {
                    told = records.recv() => match told {
                        Some(changed) if changed.workspace == workspace => return,
                        Some(_) => {}
                        None => std::future::pending::<()>().await,
                    },
                    told = rules.recv() => match told {
                        // Behind on the rules: one missed may be this workspace's.
                        Ok(changed) if changed == workspace => return,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            std::future::pending::<()>().await;
                        }
                    },
                }
            }
        };
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            return Answer::Refused { error: refused };
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

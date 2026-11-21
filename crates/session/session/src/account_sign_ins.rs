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
//! connector is handed a connection of its own when it starts ([`serve_link`]), scoped to its
//! workspace and app: it is told that workspace's accounts, and the use names the app.
//!
//! An ask for accounts that changed since a revision is held until a workspace's accounts record
//! changes, as the session's one stream of record changes tells it, and let go once the asker
//! goes away.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use accounts::sign_ins::{self, Answer, Ask, Held};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use tilo_actions::remote::Caller;
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::workspaces::Workspaces;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::platform::records::Changes;

/// What a sign-in's use says signed in for the cloud service: the app whose folders the mounts
/// are.
const SIGNS_IN_FOR: &str = tilo_app_id::apps::FILE_EXPLORER;

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
}

impl Scope {
    /// This login's cloud service: the file accounts of every workspace, for Files.
    fn cloud_service() -> Self {
        Self {
            workspace: None,
            keep: sign_ins::is_file_account,
            signs_in_for: SIGNS_IN_FOR.to_owned(),
        }
    }

    /// The origin connector of `app` in `workspace`: every account of that workspace, for `app`.
    pub(crate) fn connector(app: &str, workspace: &str) -> Self {
        Self {
            workspace: Some(workspace.to_owned()),
            keep: |_| true,
            signs_in_for: app.to_owned(),
        }
    }
}

/// What the connections handed to connectors are answered with: until this session's workspaces
/// are open, they wait.
enum Served {
    Waiting(Vec<(std::os::unix::net::UnixStream, Scope)>),
    Open(Box<Answering>),
}

/// What answers the connections handed to connectors.
struct Answering {
    workspaces: Workspaces,
    by: Caller,
    changes: Changes,
    tasks: tokio::runtime::Handle,
}

static SERVED: std::sync::Mutex<Served> = std::sync::Mutex::new(Served::Waiting(Vec::new()));

/// Answer `stream`, the session's end of the connection handed to an origin connector, within
/// `scope`: now when this session's workspaces are open, else once they are.
pub(crate) fn serve_link(stream: std::os::unix::net::UnixStream, scope: Scope) {
    let mut served = SERVED.lock_recover();
    match &mut *served {
        Served::Waiting(waiting) => waiting.push((stream, scope)),
        Served::Open(open) => answer_link(open, stream, scope),
    }
}

fn answer_link(answering: &Answering, stream: std::os::unix::net::UnixStream, scope: Scope) {
    let workspaces = answering.workspaces.clone();
    let by = answering.by.clone();
    let changes = answering.changes.clone();
    answering.tasks.spawn(async move {
        let stream = match stream
            .set_nonblocking(true)
            .and_then(|()| UnixStream::from_std(stream))
        {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!(%error, app = scope.signs_in_for, "a connector's account sign-ins are not answered");
                return;
            }
        };
        if let Err(error) = connection(stream, &workspaces, &by, &changes, &scope).await {
            tilo_failure::ignored!(
                "a connection that ends is its connector's to end, and nothing here waits on it",
                tracing::debug!(%error, "a connector's account sign-ins connection ended")
            );
        }
    });
}

/// Serve the socket below this login's runtime directory, and the connections handed to origin
/// connectors, answering as `by`, the device's person, and holding an ask for changed accounts
/// until `changes` tells of one.
pub(crate) fn serve(workspaces: Workspaces, by: Caller, changes: Changes) {
    {
        let answering = Answering {
            workspaces: workspaces.clone(),
            by: by.clone(),
            changes: changes.clone(),
            tasks: tokio::runtime::Handle::current(),
        };
        let mut served = SERVED.lock_recover();
        if let Served::Waiting(waiting) = &mut *served {
            for (stream, scope) in std::mem::take(waiting) {
                answer_link(&answering, stream, scope);
            }
        }
        *served = Served::Open(Box::new(answering));
    }
    let Some(path) = sign_ins::socket() else {
        tracing::warn!("no runtime directory, so the cloud service reaches no account's sign-in");
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = serving(&path, workspaces, by, changes).await {
            tracing::warn!(%error, path = %path.display(), "the account sign-ins socket stopped");
        }
    });
}

/// Bind `path` and answer every connection on it.
///
/// # Errors
/// The socket's directory or the socket cannot be made.
pub(crate) async fn serving(
    path: &Path,
    workspaces: Workspaces,
    by: Caller,
    changes: Changes,
) -> std::io::Result<()> {
    let listener = bind(path)?;
    loop {
        let (stream, _) = listener.accept().await?;
        if !own(&stream) {
            tracing::warn!("a process of another user reached the account sign-ins socket");
            continue;
        }
        let workspaces = workspaces.clone();
        let by = by.clone();
        let changes = changes.clone();
        tokio::spawn(async move {
            let scope = Scope::cloud_service();
            if let Err(error) = connection(stream, &workspaces, &by, &changes, &scope).await {
                tilo_failure::ignored!(
                    "a connection that ends is its client's to end, and nothing here waits on it",
                    tracing::debug!(%error, "an account sign-ins connection ended")
                );
            }
        });
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
    workspaces: &Workspaces,
    by: &Caller,
    changes: &Changes,
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
                answer = answer(workspaces, by, changes, scope, ask) => answer,
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
async fn gone(
    read: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> std::io::Result<()> {
    if read.fill_buf().await?.is_empty() {
        return Ok(());
    }
    std::future::pending().await
}

/// The answer to `ask`, asked by `by` within `scope`. An ask for accounts that changed since a revision is held
/// until a workspace's accounts record changes, as `changes` tells it, and answered with what the
/// accounts are then, or with what they still are at [`sign_ins::LONGEST_WAIT`].
async fn answer(
    workspaces: &Workspaces,
    by: &Caller,
    changes: &Changes,
    scope: &Scope,
    ask: Ask,
) -> Answer {
    match ask {
        Ask::Accounts { after } => {
            let now = tokio::time::Instant::now();
            let deadline = now.checked_add(sign_ins::LONGEST_WAIT).unwrap_or(now);
            // Followed before the accounts are read, so a change while they are is not missed.
            let mut told = changes.waiter();
            let record = tilo_workspace_service::workspaces::account_logins::accounts_document();
            loop {
                let accounts: Vec<Held> = workspaces
                    .held_accounts(scope.keep)
                    .await
                    .into_iter()
                    .filter(|(workspace, _)| {
                        scope.workspace.as_ref().is_none_or(|only| only == workspace)
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
            if scope.workspace.as_ref().is_some_and(|only| *only != workspace) {
                return Answer::Refused {
                    error: OTHER_WORKSPACE.into(),
                };
            }
            let by = by.clone().in_workspace(workspace.clone());
            let command = Command::ReleaseAccountSecret { item };
            match crate::account_passwords::answer(
                workspaces,
                &workspace,
                &scope.signs_in_for,
                &by,
                command,
            )
            .await
            {
                Ok(Reply::AccountSecret { secret, .. }) => Answer::SignIn { secret },
                Ok(_) => Answer::Refused {
                    error: crate::account_passwords::REFUSED.into(),
                },
                Err(error) => Answer::Refused { error },
            }
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

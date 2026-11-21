//! The file accounts of the workspaces this device holds, and their sign-ins, for this login's
//! cloud service ([`accounts::sign_ins`]).
//!
//! The accounts come from each workspace's published accounts record, and a sign-in is released
//! as any app's engine's is ([`crate::account_passwords`]): for an enabled account that names the
//! entry, at its servers, decided by the person's grant on Passwords, with a use written into the
//! entry first. The use names Files, whose `~/Remote` folders the mounts are.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use accounts::sign_ins::{self, Answer, Ask, Held};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use tilo_actions::remote::Caller;
use tilo_workspace_service::workspaces::Workspaces;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// What a sign-in's use says signed in: the app whose folders the mounts are.
const SIGNS_IN_FOR: &str = tilo_app_id::apps::FILE_EXPLORER;

/// How often a held ask for accounts that changed reads them again.
const RECHECK: Duration = Duration::from_secs(2);

/// Serve the socket below this login's runtime directory, answering as `by`, the device's person.
pub(crate) fn serve(workspaces: Workspaces, by: Caller) {
    let Some(path) = sign_ins::socket() else {
        tracing::warn!("no runtime directory, so the cloud service reaches no account's sign-in");
        return;
    };
    tokio::spawn(async move {
        if let Err(error) = serving(&path, workspaces, by).await {
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
        tokio::spawn(async move {
            if let Err(error) = connection(stream, &workspaces, &by).await {
                tracing::debug!(%error, "an account sign-ins connection ended");
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
    let listener = UnixListener::bind(path)?;
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
            Ok(ask) => answer(workspaces, by, ask).await,
            Err(error) => Answer::Refused {
                error: format!("That is no ask of the account sign-ins socket: {error}"),
            },
        };
        write.write_all(&sign_ins::answer_line(&answer)?).await?;
    }
}

/// The answer to `ask`, asked by `by`.
pub(crate) async fn answer(workspaces: &Workspaces, by: &Caller, ask: Ask) -> Answer {
    match ask {
        Ask::Accounts { after } => {
            let now = tokio::time::Instant::now();
            let deadline = now.checked_add(sign_ins::LONGEST_WAIT).unwrap_or(now);
            loop {
                let accounts: Vec<Held> = workspaces
                    .held_accounts(sign_ins::is_file_account)
                    .await
                    .into_iter()
                    .map(|(workspace, account)| Held { workspace, account })
                    .collect();
                let revision = sign_ins::revision_of(&accounts);
                if after.as_deref() != Some(revision.as_str())
                    || tokio::time::Instant::now() >= deadline
                {
                    return Answer::Accounts { revision, accounts };
                }
                tokio::time::sleep(RECHECK).await;
            }
        }
        Ask::SignIn { workspace, item } => {
            let by = by.clone().in_workspace(workspace.clone());
            let command = Command::ReleaseAccountSecret { item };
            match crate::account_passwords::answer(
                workspaces,
                &workspace,
                SIGNS_IN_FOR,
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

//! The environment's own agent door: where a coding agent inside the guest
//! reaches the desktop's actions (`tilo-runtime --connect`).
//!
//! The session's own door serves only programs outside every sandbox, so the
//! supervisor asks the session for a door bound to this environment
//! ([`runtime_portal::environment_door`]) as the session starts, binds it into
//! the guest at [`tilo_paths::login::GUEST_AGENT_SOCKET`] and names it in
//! [`tilo_paths::login::AGENT_SOCKET_ENV`]. The session serves a program there
//! as an agent of this environment, in the environment's workspace, once the
//! person allows it. The door is the lease's: it closes when the session ends.
//!
//! Outside a Tilo login, or when the session refuses, the guest simply has no
//! door; nothing else about it changes.

use std::path::{Path, PathBuf};

use runtime_portal::environment_door::{Lease, Request, open};
use tilo_environment::Environment;

use crate::store::Store;
use crate::workspace::{EnvironmentId, environment_workspace};

/// The workspace the environment `id` works in, as its record says
/// ([`environment_workspace`]); `None` for the primary workspace, which the
/// session knows by no id of its own, so it chooses the workspace in front of
/// the person as the environment opens.
fn workspace_of(store: &Store, id: &str) -> Option<String> {
    let id = match EnvironmentId::parse(id) {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "ush: the environment's id names no environment");
            return None;
        }
    };
    match environment_workspace(store, &id) {
        Ok(workspace) if workspace.is_primary() => None,
        Ok(workspace) => Some(workspace.to_string()),
        Err(error) => {
            tracing::warn!(%error, environment = %id, "ush: the environment's workspace could not be read");
            None
        }
    }
}

/// The session's own agent door, where this login's session listens.
fn session_door() -> Option<PathBuf> {
    Some(
        tilo_paths::login::session_dir(
            tilo_paths::login::login_runtime_dir().map(PathBuf::into_os_string),
        )?
        .join(tilo_paths::login::AGENT_SOCKET_FILE),
    )
}

/// Ask the session for the door of `environment`, the environment `id`
/// (`<workspace>/<record>`, [`crate::workspace::ENVIRONMENT_ENV`]) of
/// `store`: the lease that keeps it open, or `None` when there is no session
/// to ask or it refused.
#[must_use]
pub fn open_for(store: &Store, id: &str, environment: &Environment) -> Option<Lease> {
    open_at(&session_door()?, store, id, environment)
}

/// [`open_for`], asking the session whose door is `door`.
#[must_use]
pub fn open_at(door: &Path, store: &Store, id: &str, environment: &Environment) -> Option<Lease> {
    if !door.exists() {
        tracing::debug!(door = %door.display(), "ush: no session door; the guest has no agent door");
        return None;
    }
    let workspace = workspace_of(store, id);
    match open(
        door,
        &Request::new(id, &environment.name, workspace.as_deref()),
    ) {
        Ok(lease) => {
            tracing::info!(
                environment = id,
                workspace = lease.workspace(),
                door = %lease.path().display(),
                "ush: the environment's agent door is open"
            );
            Some(lease)
        }
        Err(error) => {
            tracing::warn!(%error, environment = id, "ush: the session opened no agent door for this environment");
            None
        }
    }
}

/// The names a coding agent's MCP configuration runs inside a guest:
/// `tilo-runtime --connect`, as it does on the host, served here by this
/// runtime's own binary, since the environment's images carry no Tilo.
pub const CONNECT_COMMANDS: [&str; 2] = ["tilo-runtime", "tilo-runtime-mcp"];

/// `tilo-runtime --connect` inside a guest: relay this process's standard
/// input and output to the environment's door, which
/// [`tilo_paths::login::AGENT_SOCKET_ENV`] names.
///
/// # Errors
/// Anything but `--connect` was asked, the environment has no door, or the
/// door does not answer.
pub fn connect_main(args: &[String]) -> anyhow::Result<i32> {
    anyhow::ensure!(
        args.iter().map(String::as_str).eq(["--connect"]),
        "inside an environment, tilo-runtime only connects to the desktop: run `tilo-runtime --connect`"
    );
    let door = std::env::var_os(tilo_paths::login::AGENT_SOCKET_ENV)
        .map(PathBuf::from)
        .filter(|door| door.is_absolute())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this environment has no agent door: the desktop opens one as the environment starts"
            )
        })?;
    let stream = tilo_files::native::socket::connect(&door).map_err(|error| {
        anyhow::anyhow!(
            "the environment's agent door, {}, does not answer: {error}",
            door.display()
        )
    })?;
    relay(&stream, std::io::stdin(), &mut std::io::stdout().lock())?;
    Ok(0)
}

/// Relay `input` to `stream`, and `stream` to `output`, until the door
/// closes; each read is passed on at once, as a client waits for each reply.
///
/// # Errors
/// Reading the door or writing `output` failed.
pub fn relay(
    stream: &std::os::unix::net::UnixStream,
    mut input: impl std::io::Read + Send + 'static,
    output: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let mut writer = stream.try_clone()?;
    std::thread::Builder::new()
        .name("ush-agent-door-input".into())
        .spawn(move || {
            let _ = copy_flushing(&mut input, &mut writer);
            let _ = writer.shutdown(std::net::Shutdown::Write);
        })?;
    copy_flushing(&mut &*stream, output)
}

fn copy_flushing(
    from: &mut impl std::io::Read,
    to: &mut impl std::io::Write,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        to.write_all(buffer.get(..read).unwrap_or_default())?;
        to.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_relays_the_clients_lines_to_the_door_and_its_answers_back() {
        use std::io::{BufRead as _, Write as _};
        let (ours, door) = std::os::unix::net::UnixStream::pair().unwrap();
        let session = std::thread::spawn(move || {
            let mut line = String::new();
            std::io::BufReader::new(door.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            (&door).write_all(b"{\"answer\":1}\n").unwrap();
            line
        });
        let mut output = Vec::new();
        relay(&ours, &b"{\"ask\":1}\n"[..], &mut output).unwrap();
        assert_eq!(session.join().unwrap(), "{\"ask\":1}\n");
        assert_eq!(output, b"{\"answer\":1}\n");
    }

    #[test]
    fn connect_only_connects() {
        let error = connect_main(&["--daemon".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("--connect"), "{error}");
    }
}

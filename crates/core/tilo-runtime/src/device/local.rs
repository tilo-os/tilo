//! The action wire on the daemon's native endpoint ([`crate::daemon`]): a connection whose first
//! line is a [`RemoteActionFrame`] has each line answered by [`tilo_actions::remote::line`], over
//! the tree its account is admitted to ([`crate::seats`]). It is what `tilo`'s workspace commands,
//! `tilolang repl` and `tilo-app` call actions over, as the MCP projection beside it is what
//! agents call them over.

use std::io::{self, BufRead, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{self, ActionProvider, Caller, RemoteActionFrame};

use super::services::Daemon;
use super::unclaimed::DeviceOnly;

/// How long past the longest remote wait a frame's answer is waited for.
const SLACK: Duration = Duration::from_secs(5);

/// The tree a connection's frames reach.
type Tree = Arc<dyn ActionProvider + Send + Sync>;

/// What a connection's frames are answered by: the tree they reach and the caller they are
/// there, and the uploads begun on any connection, so one a lost connection cut is taken up on
/// the next.
#[derive(Clone)]
pub(crate) struct Frames {
    tree: Arc<dyn Fn() -> io::Result<(Tree, Caller)> + Send + Sync>,
    uploads: Arc<remote::HostedContentPuts>,
}

impl Frames {
    /// Frames answered over the tree `tree` makes for each connection.
    pub(crate) fn new(
        tree: impl Fn() -> io::Result<(Tree, Caller)> + Send + Sync + 'static,
    ) -> Self {
        Self {
            tree: Arc::new(tree),
            uploads: Arc::default(),
        }
    }

    /// The device's own actions alone ([`DeviceOnly`]): what an account the daemon serves no
    /// person for reaches, and every account before the person's services run.
    pub(crate) fn of_the_device(answers: DeviceOnly) -> Self {
        Self::new(move || Ok((Arc::new(answers.clone()) as Tree, Caller::local_system())))
    }

    /// The person's workspace and account, as `daemon` serves them
    /// ([`Daemon::person_tree`]).
    pub(crate) fn of_the_person(daemon: &Arc<Daemon>) -> Self {
        let daemon = Arc::downgrade(daemon);
        Self::new(move || {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.person_tree()
        })
    }

    /// Answer `stream`'s frames, `first` and then each line `input` reads, until it ends
    /// ([`remote::serve_lines`]).
    pub(crate) fn serve(&self, mut stream: UnixStream, first: &[u8], input: impl BufRead) {
        // The tree's actions run their steps on the process's runtime, as they do for MCP.
        let _runtime = tilo_rt::app().handle().enter();
        let (tree, caller) = match (self.tree)() {
            Ok(served) => served,
            Err(why) => {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let _ = writeln!(
                    stream,
                    "{}",
                    remote::line_refusal("unavailable", &why.to_string())
                );
                return;
            }
        };
        remote::serve_lines(
            &tree,
            &caller,
            &self.uploads,
            Some(first.to_vec()),
            input,
            stream,
            SLACK,
        );
    }
}

/// Whether `line` is an action frame rather than MCP or the daemon's own requests.
pub(crate) fn is_frame(line: &[u8]) -> bool {
    RemoteActionFrame::decode(line).is_ok()
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;

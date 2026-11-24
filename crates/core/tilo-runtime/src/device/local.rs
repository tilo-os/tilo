//! The action wire on the daemon's native endpoint ([`crate::daemon`]): a connection whose first
//! line is a [`RemoteActionFrame`] has each line answered by [`tilo_actions::remote::line`], over
//! the tree its account is admitted to ([`crate::seats`]). It is what `tilo`'s workspace commands,
//! `tilolang repl` and `tilo-app` call actions over, as the MCP projection beside it is what
//! agents call them over.
//!
//! One connection serves every workspace the device holds: each frame is answered in the
//! workspace it names ([`RemoteActionFrame::workspace`]), admitted there as whoever the
//! connection is (the person, or a key signed in for someone else) and decided by what that
//! workspace lets them do; a frame naming none is answered in the device's own. A workspace that
//! does not admit them refuses the call. A read is decided by the workspace its address names
//! wherever it is asked, so one whose workspace does not admit them is read as the connection's
//! own run, whose reader refuses what that workspace does not let them read.

use std::collections::HashMap;
use std::io::{self, BufRead, Write as _};
use std::sync::Arc;
use std::time::Duration;
use tilo_socket::Stream;

use tilo_actions::remote::{
    self, ActionCallError, ActionProvider, Caller, RemoteActionFrame, RemoteActionMessage,
};

use super::services::Daemon;
use super::unclaimed::DeviceOnly;

/// How long past the longest remote wait a frame's answer is waited for.
const SLACK: Duration = Duration::from_secs(5);

/// The tree a connection's frames reach.
type Tree = Arc<dyn ActionProvider + Send + Sync>;

/// The tree a connection's frames reach in a workspace (none: the device's own), and the caller
/// they are there.
type Trees = dyn Fn(Option<&str>) -> io::Result<(Tree, Caller)> + Send + Sync;

/// What a connection's frames are answered by: the tree they reach in each workspace and the
/// caller they are there, and the uploads begun on any connection, so one a lost connection cut
/// is taken up on the next.
#[derive(Clone)]
pub(crate) struct Frames {
    tree: Arc<Trees>,
    uploads: Arc<remote::HostedContentPuts>,
    /// What answers a release of a vault item to the app process the connection is, when it is
    /// one the daemon started.
    own_process: Option<(Tree, Caller)>,
    /// The call lane an app's connection carries, when the connection is an app's: its setup, or
    /// why this device will carry no call for it.
    lane: Option<Result<tilo_live::app_call::CallSetup, String>>,
}

impl Frames {
    /// Frames answered over the tree `tree` makes in each workspace a connection's frames name.
    pub(crate) fn new(
        tree: impl Fn(Option<&str>) -> io::Result<(Tree, Caller)> + Send + Sync + 'static,
    ) -> Self {
        Self {
            tree: Arc::new(tree),
            uploads: Arc::default(),
            own_process: None,
            lane: None,
        }
    }

    /// The device's own actions alone ([`DeviceOnly`]): what an account the daemon serves no
    /// person for reaches, and every account before the person's services run. It is in no
    /// workspace, so a workspace a frame names reaches the same actions.
    pub(crate) fn of_the_device(answers: DeviceOnly) -> Self {
        Self::new(move |_| Ok((Arc::new(answers.clone()) as Tree, Caller::local_system())))
    }

    /// The person's workspaces and account, as a plain connection from their own account reaches
    /// them ([`Daemon::connection_tree_in`]): the person, admitted from who is on the other end
    /// of the daemon's socket, in each workspace a frame names.
    pub(crate) fn of_the_person(daemon: &Arc<Daemon>) -> Self {
        let daemon = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.connection_tree_in(workspace)
        })
    }

    /// The person's workspaces and account, as a program of this device reaches them
    /// ([`Daemon::program_tree`]): acting as the person, but never as a plain connection's own
    /// act, so what a workspace asks of them is put to them. An agent's connection answers action
    /// frames this way.
    pub(crate) fn of_a_persons_program(daemon: &Arc<Daemon>) -> Self {
        let daemon = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.program_tree(workspace)
        })
    }

    /// An app the daemon started, in `workspace`: the workspace's broker decides its calls with
    /// the app's grants, never the person's own rights ([`Daemon::app_tree`]). The app acts in
    /// that workspace alone; a frame naming another is refused. A release, a find or a web call
    /// of a vault item is answered for the very process the connection is
    /// ([`super::vault_release`]), on this connection alone.
    pub(crate) fn of_an_app(daemon: &Arc<Daemon>, at: &crate::seats::AppAt) -> Self {
        let own = super::vault_release::OwnProcess::new(daemon, at.pid, &at.app, &at.workspace);
        let caller = Caller::local_app(&at.app).in_workspace(&at.workspace);
        let lane = daemon.app_call_setup(at);
        let (daemon, app, workspace) =
            (Arc::downgrade(daemon), at.app.clone(), at.workspace.clone());
        let mut frames = Self::new(move |named| {
            if named.is_some_and(|named| named != workspace) {
                return Err(io::Error::other(format!(
                    "{app} was started in another workspace and acts only there"
                )));
            }
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.app_tree(&app, &workspace)
        });
        frames.own_process = Some((Arc::new(own) as Tree, caller));
        frames.lane = Some(lane);
        frames
    }

    /// The device's workspaces as the key `key` (certified by `cert`) reaches them: what another
    /// person signed in on the computer calls actions in ([`Daemon::key_tree`]).
    pub(crate) fn of_a_key(
        daemon: &Arc<Daemon>,
        key: ed25519_dalek::SigningKey,
        cert: tilo_user_identity::DeviceCert,
    ) -> Self {
        let daemon = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            let (tree, caller) = daemon.key_tree(&key, &cert, workspace)?;
            Ok((Arc::new(tree) as Tree, caller))
        })
    }

    /// Answer `stream`'s frames, `first` and then each line `input` reads, until it ends
    /// ([`remote::serve_lines`]), each in the workspace it names: admitted there once, on its
    /// first frame.
    pub(crate) fn serve(&self, mut stream: Stream, first: &[u8], input: impl BufRead) {
        // The tree's actions run their steps on the process's runtime, as they do for MCP.
        let _runtime = tilo_rt::app().handle().enter();
        let own = match (self.tree)(None) {
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
        let trees = Arc::clone(&self.tree);
        let mut admitted: HashMap<String, (Tree, Caller)> = HashMap::new();
        let own_process = self.own_process.clone();
        let in_workspace = move |frame: &RemoteActionFrame| {
            if let Some(own_process) = &own_process
                && super::vault_release::is_answered_for_this_process(frame)
                && frame
                    .workspace()
                    .is_none_or(|named| own_process.1.workspace() == Some(named))
            {
                return Ok(own_process.clone());
            }
            let Some(workspace) = frame.workspace() else {
                return Ok(own.clone());
            };
            if own.1.workspace() == Some(workspace) {
                return Ok(own.clone());
            }
            if let Some(served) = admitted.get(workspace) {
                return Ok(served.clone());
            }
            match trees(Some(workspace)) {
                Ok(served) => {
                    admitted.insert(workspace.to_owned(), served.clone());
                    Ok(served)
                }
                Err(_) if matches!(frame.message, RemoteActionMessage::ResourceRead { .. }) => {
                    Ok(own.clone())
                }
                Err(why) => Err(ActionCallError::Refused {
                    reason: format!(
                        "the workspace {workspace} does not let you act in it from this \
                         computer: {why}"
                    ),
                }),
            }
        };
        // An app's connection carries the call lane of the workspace's live session; any other
        // connection carries none.
        let out = self.lane.is_some().then(|| {
            stream.try_clone().ok().map(|writer| {
                let writer = Arc::new(std::sync::Mutex::new(writer));
                Arc::new(move |line: String| {
                    if let Ok(mut writer) = writer.lock() {
                        let _ = writeln!(writer, "{line}");
                    }
                }) as tilo_live::app_call::Out
            })
        });
        let lane = match (&self.lane, out) {
            (Some(granted), Some(Some(out))) => {
                Some(tilo_live::app_call::AppLane::new(granted.clone(), out))
            }
            _ => None,
        };
        remote::serve_lines_with(
            in_workspace,
            &self.uploads,
            Some(first.to_vec()),
            input,
            stream,
            SLACK,
            lane.as_ref()
                .map(|lane| lane.as_ref() as &dyn remote::CallLanes),
        );
    }
}

/// Whether `line` is an action frame rather than MCP or the daemon's own requests.
pub(crate) fn is_frame(line: &[u8]) -> bool {
    RemoteActionFrame::decode(line).is_ok()
}

/// Tell `stream`'s peer why it is not served, and close.
pub(crate) fn refuse(mut stream: Stream, why: &str) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = writeln!(stream, "{}", remote::line_refusal("not_permitted", why));
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;

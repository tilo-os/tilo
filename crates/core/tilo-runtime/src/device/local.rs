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
use std::io::{self, BufRead, Read as _, Write as _};
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

    /// A workspace VM's tenant, in each workspace a frame names
    /// ([`Daemon::tenant_tree_in`]): the person's unnamed agent, never the person, so an asked
    /// action still asks.
    pub(crate) fn of_the_tenant(daemon: &Arc<Daemon>) -> Self {
        let daemon = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.tenant_tree_in(workspace)
        })
    }

    /// An app the daemon started, in `workspace`: the workspace's broker decides its calls with
    /// the app's grants, never the person's own rights ([`Daemon::app_tree`]). The app acts in
    /// that workspace alone; a frame naming another is refused. A release, a find or a web call
    /// of a vault item is answered for the very process the connection is
    /// ([`super::vault_release`]), on this connection alone.
    pub(crate) fn of_an_app(daemon: &Arc<Daemon>, at: &crate::seats::AppAt) -> Self {
        let own = super::vault_release::OwnProcess::new(daemon, at.pid, &at.app, &at.workspace);
        let caller = Caller::launched(&at.app).in_workspace(&at.workspace);
        // The app's signed manifest declares it signs in to the workspace's accounts: the grant
        // that names it for them is the one its connection holds, never a person's.
        let caller = if daemon.app_declares(&at.app, tilo_app_package::manifest::ACCOUNT_SIGN_INS) {
            caller.granting(vec![tilo_action_policy::Grant::new(
                format!("manifest:{}:{}", at.app, at.workspace),
                tilo_action_policy::Grantee::app(at.app.as_str()),
                tilo_action_policy::TreePath::workspace(at.workspace.clone())
                    .app(tilo_actions::remote::person_accounts::PROVIDER),
                tilo_action_policy::KindSet::READ,
                tilo_action_policy::Answer::Allow,
                tilo_action_policy::Grantee::person(daemon.person_id()),
            )])
        } else {
            caller
        };
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

/// Hand the connector behind `connector` its connection to this device's dispatch, answered as
/// `at`, the app process the daemon started for it, and serve it on a thread of its own: what a
/// program that signs in calls the published actions over, decided under that app's grants.
///
/// # Errors
/// The connection cannot be made, or the connector took none.
pub(crate) fn hand_to_connector(
    daemon: &Arc<Daemon>,
    connector: &runtime_portal::process_link::Descriptor,
    at: &crate::seats::AppAt,
) -> io::Result<()> {
    let (ours, theirs) = tilo_socket::pair()?;
    runtime_portal::origin_connector::send_actions(
        connector,
        runtime_portal::packet::descriptor_of(theirs),
    )?;
    let frames = Frames::of_an_app(daemon, at);
    std::thread::Builder::new()
        .name("tilo-connector-actions".into())
        .spawn(move || {
            let reader = match ours.try_clone() {
                Ok(reader) => reader,
                Err(error) => {
                    tracing::debug!(%error, "a connector's action connection");
                    return;
                }
            };
            serve_app_frames(&frames, ours, reader);
        })?;
    Ok(())
}

/// Serve `stream` as an app's own connection: its first line is read here, as the call the app
/// makes first, and the rest follows. A line nobody sent never reaches the wire, and the first
/// call is answered.
fn serve_app_frames(frames: &Frames, stream: Stream, reader: Stream) {
    let mut reader = io::BufReader::new(reader);
    let mut first = Vec::new();
    if let Err(error) = (&mut reader)
        .take(crate::daemon::FIRST_LINE)
        .read_until(b'\n', &mut first)
    {
        tracing::debug!(%error, "an app's connection");
        return;
    }
    let line = first.strip_suffix(b"\n").unwrap_or(&first).to_vec();
    frames.serve(stream, &line, reader);
}

#[cfg(feature = "test-support")]
impl Daemon {
    /// This process's own connection to the daemon's dispatch, answered as `app` in `workspace`
    /// and admitted as the app's held connection there: what a test of an app that signs in
    /// speaks the published actions over, instead of starting the app's process.
    ///
    /// # Errors
    /// The connection cannot be made, or cannot be served.
    pub fn app_connection_for_test(
        self: &Arc<Self>,
        app: &str,
        workspace: &str,
    ) -> io::Result<Stream> {
        let pid = std::process::id();
        self.apps().record_connection_for_test(pid, app, workspace);
        let (ours, theirs) = tilo_socket::pair()?;
        let frames = Frames::of_an_app(
            self,
            &crate::seats::AppAt {
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                pid,
            },
        );
        let reader = ours.try_clone()?;
        std::thread::Builder::new()
            .name("tilo-test-app-connection".into())
            .spawn(move || serve_app_frames(&frames, ours, reader))?;
        Ok(theirs)
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
pub(crate) mod tests;

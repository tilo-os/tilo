//! The action wire on the daemon's native endpoint ([`crate::daemon`]): a connection whose first
//! line is a [`RemoteActionFrame`] has each line answered by [`tilo_action_line`], over
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
use tilo_action_line_serve::shells::AppShells;
use tilo_socket::Stream;

use tilo_actions::remote::{
    self, ActionCallError, ActionProvider, Caller, RemoteActionFrame, RemoteActionMessage,
};

use super::services::Daemon;
use super::unclaimed::DeviceOnly;

/// The tree a connection's frames reach.
type Tree = Arc<dyn ActionProvider + Send + Sync>;

/// The tree a connection's frames reach in a workspace (none: the device's own), and the caller
/// they are there.
type Trees = dyn Fn(Option<&str>) -> io::Result<(Tree, Caller)> + Send + Sync;

/// Whether this computer runs a workspace's programs, or why not: asked on each shell an app
/// opens there.
type RunsHere = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// What an app's connection carries shells by: the workspace it was started in, its grant or
/// why its package has none, where it runs, and where this device's shells come from.
type ShellsCarried = (
    String,
    Result<(), String>,
    RunsHere,
    Arc<dyn tilo_action_line_serve::shells::ShellSource>,
);

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
    /// The sound output lane an app's connection carries: the device's output as that app
    /// process plays it.
    audio: Option<Arc<super::audio_output::AudioLane>>,
    /// The call lane an app's connection carries: the device's call service as that app process
    /// takes part in its workspace's calls.
    media: Option<tilo_app_host::collab_app_broker::CallMediaConnection>,
    /// The presence lane an app's connection carries: the device's live presence as that app's
    /// views say where their person is, and hear where everyone else is.
    app_presence: Option<super::app_presence::Setup>,
    /// Whether an app's connection carries shells, when the connection is an app's: the
    /// workspace it was started in, its grant, or why its package has none, and where this
    /// device's shells come from.
    shells: Option<ShellsCarried>,
    /// Whether a connection may be carried to another of the person's devices
    /// ([`super::carry`]): the person's own alone.
    carrying: super::carry::Carrying,
    /// Whose API answers the connection's requests of `/v1` ([`super::api_lane`]).
    api: super::api_lane::Answering,
    /// The daemon a program that dialled its socket itself is answered by instead, for the
    /// person's own frames ([`Self::for_a_program`]).
    programs: Option<std::sync::Weak<Daemon>>,
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
            audio: None,
            media: None,
            app_presence: None,
            shells: None,
            carrying: super::carry::Carrying::Refused(super::carry::NOT_THE_PERSON),
            api: super::api_lane::Answering::None,
            programs: None,
        }
    }

    /// What answers the frames of process `pid` instead of the person's own frames, when it is
    /// none of the daemon's known paths: a program, which acts as the person but is never their
    /// own act ([`Caller::from_program`]). `None` for frames that are not the person's own.
    pub(crate) fn for_a_program(&self, pid: Option<u32>) -> Option<Self> {
        let daemon = self.programs.clone()?;
        // The program's path, the kernel's " (deleted)" kept, names it on a card: never an
        // authority, only what the person reads.
        let program = pid
            .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
            .map_or_else(
                || crate::i18n::t("asked-by-a-program"),
                |exe| {
                    crate::i18n::t_args(
                        "asked-by-program",
                        &[("path", exe.display().to_string().into())],
                    )
                },
            );
        Some(Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.program_tree_from(workspace, &program)
        }))
    }

    /// These frames, their connection's requests of `/v1` answered by an API of its own over
    /// the same trees, named `label` in History.
    fn answering_as(mut self, daemon: &Arc<Daemon>, label: &str) -> Self {
        self.api = super::api_lane::Answering::Own(Arc::downgrade(daemon), label.to_owned());
        self
    }

    /// The device's own actions alone ([`DeviceOnly`]): what an account the daemon serves no
    /// person for reaches, and every account before the person's services run. It is in no
    /// workspace, so a workspace a frame names reaches the same actions.
    pub(crate) fn of_the_device(answers: DeviceOnly) -> Self {
        let mut frames =
            Self::new(move |_| Ok((Arc::new(answers.clone()) as Tree, Caller::local_system())));
        frames.carrying = super::carry::Carrying::Refused(super::carry::DEVICE_ONLY);
        frames
    }

    /// The person's workspaces and account, as a plain connection from their own account reaches
    /// them ([`Daemon::connection_tree_in`]): the person, admitted from who is on the other end
    /// of the daemon's socket, in each workspace a frame names.
    pub(crate) fn of_the_person(daemon: &Arc<Daemon>) -> Self {
        let carrying = super::carry::Carrying::of(daemon);
        let login = super::api_lane::Answering::Person(Arc::downgrade(daemon));
        let daemon = Arc::downgrade(daemon);
        let programs = daemon.clone();
        let mut frames = Self::new(move |workspace| {
            let daemon = daemon
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.connection_tree_in(workspace)
        });
        frames.carrying = carrying;
        frames.api = login;
        frames.programs = Some(programs);
        frames
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
        let weak = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = weak
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            daemon.tenant_tree_in(workspace)
        })
        .answering_as(daemon, "A workspace VM")
    }

    /// An app the daemon started, in `workspace`: the workspace's broker decides its calls with
    /// the app's grants, never the person's own rights ([`Daemon::app_tree`]). The app acts in
    /// that workspace alone; a frame naming another is refused. A release, a find or a web call
    /// of a vault item is answered for the very process the connection is
    /// ([`super::vault_release`]), on this connection alone.
    pub(crate) fn of_an_app(daemon: &Arc<Daemon>, at: &crate::seats::AppAt) -> Self {
        let own = super::vault_release::OwnProcess::new(daemon, at.pid, &at.app, &at.workspace);
        // The app, holding the grant its signed manifest declares where it signs in to the
        // workspace's accounts: the one its connection holds, never a person's.
        let caller = daemon.app_caller(&at.app, &at.workspace);
        let lane = daemon.app_call_setup(at);
        let audio = super::audio_output::lane_for(daemon, at).map(Arc::new);
        let media = super::call_media::connection_for(daemon, at);
        let app_presence = super::app_presence::Setup::of(daemon, at);
        // The shells the device's runtime runs, for an app whose signed manifest declares it opens
        // them: the manifest is the ceiling, never the app's request.
        let shells = if daemon.app_declares(&at.app, tilo_app_package::manifest::SHELLS) {
            Ok(())
        } else {
            Err(format!(
                "{} may not open shells: its package does not declare that it does (`uses shell`)",
                at.app
            ))
        };
        let source = super::host_shells::source(daemon);
        // A workspace other people are members of runs its programs in a VM on a host, never
        // on this computer: asked on each open, as its people change.
        let held = Arc::downgrade(daemon);
        let runs_here = {
            let workspace = at.workspace.clone();
            Arc::new(move || {
                let daemon = held
                    .upgrade()
                    .ok_or_else(|| "this computer's daemon is stopping".to_owned())?;
                let workspace = workspace.clone();
                super::workspaces::block_on_owned(async move {
                    daemon.workspaces().runs_programs_here(&workspace).await
                })
            }) as RunsHere
        };
        let (weak, app, workspace) = (Arc::downgrade(daemon), at.app.clone(), at.workspace.clone());
        let mut frames = Self::new(move |named| {
            if named.is_some_and(|named| named != workspace) {
                return Err(io::Error::other(format!(
                    "{app} was started in another workspace and acts only there"
                )));
            }
            let daemon = weak
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            let (provider, caller) = daemon.app_tree(&app, &workspace)?;
            let paths: Tree = Arc::new(super::paths::Paths::wrapping(provider, workspace.clone()));
            Ok((paths, caller))
        })
        .answering_as(daemon, &at.app);
        frames.own_process = Some((Arc::new(own) as Tree, caller));
        frames.lane = Some(lane);
        frames.audio = audio;
        frames.media = media;
        frames.app_presence = app_presence;
        frames.shells = Some((at.workspace.clone(), shells, runs_here, source));
        frames
    }

    /// The device's workspaces as the key `key` (certified by `cert`) reaches them: what another
    /// person signed in on the computer calls actions in ([`Daemon::key_tree`]).
    pub(crate) fn of_a_key(
        daemon: &Arc<Daemon>,
        key: ed25519_dalek::SigningKey,
        cert: tilo_user_identity::DeviceCert,
    ) -> Self {
        let label = cert.device_label.clone();
        let weak = Arc::downgrade(daemon);
        Self::new(move |workspace| {
            let daemon = weak
                .upgrade()
                .ok_or_else(|| io::Error::other("this computer's daemon is stopping"))?;
            let (tree, caller) = daemon.key_tree(&key, &cert, workspace)?;
            Ok((Arc::new(tree) as Tree, caller))
        })
        .answering_as(daemon, &label)
    }

    /// Carry `stream`, which asked for `carry` on its first line, to the device it names, reading
    /// its frames from `input`; or tell it why it is not carried ([`super::carry`]).
    pub(crate) fn carry(
        &self,
        stream: Stream,
        carry: &tilo_action_line::Carry,
        input: impl BufRead,
    ) {
        self.carrying.serve(stream, carry, input);
    }

    /// Answer `stream`'s frames, `first` and then each line `input` reads, until it ends
    /// ([`tilo_action_line_serve::serve_lines`]), each in the workspace it names: admitted there
    /// once, on its first frame. `ending`, when the part serves the connection, counts each call
    /// it answers until its answer (a part that ends waits for those, never for the connection a
    /// shell keeps open after them) and says when the part takes no new call.
    #[expect(
        clippy::too_many_lines,
        reason = "one connection's lanes beside the action wire, each named where it is served"
    )]
    pub(crate) fn serve(
        &self,
        mut stream: Stream,
        first: &[u8],
        input: impl BufRead,
        ending: Option<&dyn tilo_action_line_serve::Ending>,
    ) {
        // The tree's actions run their steps on the process's runtime, as they do for MCP.
        let _runtime = tilo_rt::app().handle().enter();
        let own = match (self.tree)(None) {
            Ok(served) => served,
            Err(why) => {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let _ = writeln!(
                    stream,
                    "{}",
                    tilo_action_line::line_refusal("unavailable", &why.to_string())
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
        // Every connection carries the API; an app's also carries the call lane of the
        // workspace's live session, its presence and its shells, which no other connection does.
        // The lanes and the action wire write whole lines to the one connection, never into
        // each other's.
        let writer = Arc::new(std::sync::Mutex::new(stream));
        let out = Some({
            let writer = Arc::clone(&writer);
            Arc::new(move |line: String| {
                if let Ok(mut writer) = writer.lock() {
                    let _ = writer.write_all(format!("{line}\n").as_bytes());
                }
            }) as tilo_live::app_call::Out
        });
        let api = out.as_ref().map(|out| {
            super::api_lane::ApiLane::new(self.api.clone(), Arc::clone(&self.tree), Arc::clone(out))
        });
        let call_media = match (&self.media, &out) {
            (Some(media), Some(out)) => Some(super::call_media::CallMediaLane::new(
                media.clone(),
                Arc::clone(out),
            )),
            _ => None,
        };
        let presence = match (&self.app_presence, &out) {
            (Some(setup), Some(out)) => Some(super::app_presence::PresenceLane::new(
                setup.clone(),
                Arc::clone(out),
            )),
            _ => None,
        };
        let shells = match (&self.shells, &out) {
            (Some((workspace, granted, runs_here, source)), Some(out)) => Some(AppShells::new(
                workspace,
                granted.clone(),
                Arc::clone(runs_here),
                Arc::clone(source),
                Arc::clone(out),
            )),
            _ => None,
        };
        let call = match (&self.lane, out) {
            (Some(granted), Some(out)) => {
                Some(tilo_live::app_call::AppLane::new(granted.clone(), out))
            }
            _ => None,
        };
        let lanes = Lanes {
            api,
            call,
            audio: self.audio.as_deref(),
            media: call_media.as_ref(),
            presence: presence.as_ref(),
            shells,
        };
        tilo_action_line_serve::serve_lines_with(
            in_workspace,
            &self.uploads,
            Some(first.to_vec()),
            input,
            Lines {
                writer,
                pending: Vec::new(),
            },
            ending,
            Some(&lanes as &dyn tilo_action_line_serve::CallLanes),
        );
    }
}

/// The action wire's side of a connection whose lanes write to it too: each line it writes goes
/// out whole, once it is flushed.
struct Lines {
    writer: Arc<std::sync::Mutex<Stream>>,
    pending: Vec<u8>,
}

impl io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("the connection's writer failed"))?;
        writer.write_all(&self.pending)?;
        self.pending.clear();
        writer.flush()
    }
}

/// The lanes a connection carries beside the action wire: the API, and on an app's its outside
/// calls, its sound output, the call its views take part in, and its shells.
struct Lanes<'a> {
    api: Option<super::api_lane::ApiLane>,
    call: Option<Arc<tilo_live::app_call::AppLane>>,
    audio: Option<&'a super::audio_output::AudioLane>,
    media: Option<&'a super::call_media::CallMediaLane>,
    presence: Option<&'a super::app_presence::PresenceLane>,
    shells: Option<AppShells>,
}

impl tilo_action_line_serve::CallLanes for Lanes<'_> {
    fn take(&self, message: &RemoteActionMessage) -> tilo_action_line_serve::Take {
        if let Some(api) = &self.api {
            match tilo_action_line_serve::CallLanes::take(api, message) {
                tilo_action_line_serve::Take::NotCall => {}
                taken => return taken,
            }
        }
        if let Some(presence) = self.presence {
            match tilo_action_line_serve::CallLanes::take(presence, message) {
                tilo_action_line_serve::Take::NotCall => {}
                taken => return taken,
            }
        }
        if let Some(media) = self.media {
            match tilo_action_line_serve::CallLanes::take(media, message) {
                tilo_action_line_serve::Take::NotCall => {}
                taken => return taken,
            }
        }
        if let Some(shells) = &self.shells {
            match tilo_action_line_serve::CallLanes::take(shells, message) {
                tilo_action_line_serve::Take::NotCall => {}
                taken => return taken,
            }
        }
        if let Some(audio) = self.audio {
            match tilo_action_line_serve::CallLanes::take(audio, message) {
                tilo_action_line_serve::Take::NotCall => {}
                taken => return taken,
            }
        }
        self.call
            .as_ref()
            .map_or(tilo_action_line_serve::Take::NotCall, |call| {
                tilo_action_line_serve::CallLanes::take(call.as_ref(), message)
            })
    }

    fn body_len(&self, message: &RemoteActionMessage) -> Option<usize> {
        self.audio
            .and_then(|audio| tilo_action_line_serve::CallLanes::body_len(audio, message))
    }

    fn take_body(
        &self,
        message: &RemoteActionMessage,
        body: &[u8],
    ) -> tilo_action_line_serve::Take {
        if let (Some(api), RemoteActionMessage::ApiRequest { .. }) = (&self.api, message) {
            return tilo_action_line_serve::CallLanes::take_body(api, message, body);
        }
        self.audio
            .map_or(tilo_action_line_serve::Take::NotCall, |audio| {
                tilo_action_line_serve::CallLanes::take_body(audio, message, body)
            })
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        if let Some(audio) = &self.audio {
            // The app's process ended, or its connection did: nothing it opened sounds any more.
            audio.disconnect();
        }
        if let Some(media) = &self.media {
            // Nor is the device still in a call it joined, or capturing for it.
            media.disconnect();
        }
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
    frames.serve(stream, &line, reader, None);
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
    let _ = writeln!(
        stream,
        "{}",
        tilo_action_line::line_refusal("not_permitted", why)
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
#[path = "local_tests.rs"]
pub(crate) mod tests;

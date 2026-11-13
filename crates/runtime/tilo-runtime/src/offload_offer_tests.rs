use super::*;

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};

use tilo_actions::remote::{InvokeArgument, InvokeRequest, Pending, pending};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_peer_state::placement::{Answer, Choice, Power, State, pick};
use tilo_relay::offload::{BindError, MediaFuture};

/// The workspace the fixtures invoke in.
const WORKSPACE: &str = "3f2a1b7c-4d8e-4f60-9a51-2b3c4d5e6f70";
/// The app whose document media the invocation reads.
const APP: &str = "so.tilo.photos";
/// The device that answers the round.
const SERVER: [u8; 32] = [7; 32];
/// How long the transform took the one time it ran here.
const LOCAL_RUN: Duration = Duration::from_millis(4_000);
/// What the job reads: enough that moving it is worth weighing.
const INPUT_BYTES: u64 = 4 * 1024 * 1024;

/// The workspace's app content on this device.
#[derive(Default)]
struct Media {
    content: Mutex<HashMap<String, Vec<u8>>>,
    /// The scopes the door reached the content through, as (workspace, app).
    scopes: Mutex<Vec<(String, String)>>,
}

impl Media {
    /// Hold `bytes` as this workspace's app content, and name them.
    fn hold(&self, bytes: &[u8]) -> String {
        let digest = digest(bytes);
        self.content
            .lock_recover()
            .insert(digest.clone(), bytes.to_vec());
        digest
    }

    /// What this device holds under `digest`, if anything.
    fn held(&self, digest: &str) -> Option<Vec<u8>> {
        self.content.lock_recover().get(digest).cloned()
    }

    /// The scopes reached since the last call.
    fn scopes(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.scopes.lock_recover())
    }

    fn note(&self, scope: &BlobScope) {
        self.scopes
            .lock_recover()
            .push((scope.workspace().to_owned(), scope.app().to_owned()));
    }
}

impl WorkspaceMedia for Media {
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        self.note(scope);
        let len = self
            .content
            .lock_recover()
            .get(&hex::encode(digest))
            .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Box::pin(async move { len.ok_or(BindError::NotLocal) })
    }

    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        self.note(scope);
        let bytes = self
            .content
            .lock_recover()
            .get(&hex::encode(digest))
            .cloned();
        Box::pin(async move { bytes.ok_or(BindError::NotLocal) })
    }

    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        self.note(scope);
        let kept = *blake3::hash(&bytes).as_bytes();
        self.content.lock_recover().insert(hex::encode(kept), bytes);
        Box::pin(async move { Ok(kept) })
    }
}

/// What the answering device does with an invocation it is asked to run.
enum Reply {
    /// It never answers.
    Silent,
    /// It ran it, and produced `content`.
    Ran {
        output: ActionOutput,
        content: Vec<u8>,
    },
    /// It refused.
    Failed(String),
    /// It answers when the test completes it.
    Later(Pending<Result<RanAway, String>>),
}

/// The workspace's other devices, as the host's plane serves them.
struct Peers {
    reply: Mutex<Reply>,
    asked: Mutex<Vec<(String, Route, Work)>>,
    invoked: Mutex<Vec<(String, [u8; 32], InvokeRequest)>>,
}

impl Peers {
    fn new(reply: Reply) -> Self {
        Self {
            reply: Mutex::new(reply),
            asked: Mutex::default(),
            invoked: Mutex::default(),
        }
    }

    /// What the door has asked since the last call.
    fn asked(&self) -> Vec<(String, Route, Work)> {
        std::mem::take(&mut *self.asked.lock_recover())
    }

    /// What the door has handed to a device since the last call.
    fn invoked(&self) -> Vec<(String, [u8; 32], InvokeRequest)> {
        std::mem::take(&mut *self.invoked.lock_recover())
    }
}

impl OffloadPeers for Peers {
    fn ask(
        &self,
        workspace: &str,
        route: &Route,
        work: &Work,
        _invocation: InvocationId,
        _within: Duration,
    ) -> Pending<Choice> {
        self.asked
            .lock_recover()
            .push((workspace.to_owned(), route.clone(), work.clone()));
        // The other device holds the workspace's content too: the same
        // content, synced, so nothing of the job moves to it.
        let answer = Answer {
            state: State {
                requester: false,
                online: true,
                app_installed: true,
                data_saving: false,
                critical_battery: false,
                priority: 0,
                power: Power::External,
                load_permille: 0,
                free_slots: Some(1),
                latency_ms: 20,
            },
            holds: work.inputs.iter().map(|(digest, _)| *digest).collect(),
        };
        Pending::ready(pick([(SERVER, answer)], work))
    }

    fn run(
        &self,
        workspace: &str,
        device: [u8; 32],
        invocation: InvokeRequest,
    ) -> Pending<Result<RanAway, String>> {
        self.invoked
            .lock_recover()
            .push((workspace.to_owned(), device, invocation));
        let reply = std::mem::replace(&mut *self.reply.lock_recover(), Reply::Silent);
        match reply {
            Reply::Silent => never_answered(),
            Reply::Ran { output, content } => Pending::ready(Ok(RanAway {
                device,
                output,
                outputs: vec![content],
            })),
            Reply::Failed(reason) => Pending::ready(Err(reason)),
            Reply::Later(answer) => answer,
        }
    }
}

/// An answer nobody ever gives.
fn never_answered<T>() -> Pending<T> {
    let (completer, answer) = pending();
    drop(completer);
    answer
}

/// One step of `future`, without a reactor: every step of the door answers at
/// once unless the test itself holds the answer back.
fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

/// `future`'s answer, when it has one.
fn drive<F: Future>(future: F) -> Option<F::Output> {
    let mut future = Box::pin(future);
    match poll_once(future.as_mut()) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

/// The job's input: content of its own, big enough that moving it is worth
/// weighing.
fn input() -> Vec<u8> {
    vec![7; usize::try_from(INPUT_BYTES).unwrap()]
}

/// The bytes' content hash, as the workspace's apps name them.
fn digest(bytes: &[u8]) -> String {
    hex::encode(blake3::hash(bytes).as_bytes())
}

/// The offloadable invocation the fixtures make: a thumbnail of the photo
/// `input`.
fn request(input: &str) -> OffloadRequest {
    OffloadRequest {
        workspace: WORKSPACE.to_owned(),
        app_id: APP.to_owned(),
        address: ActionAddress::new("photo", "thumbnail"),
        offload: Offload {
            app: "tilo.transcode".to_owned(),
            entrypoint: "thumbnail".to_owned(),
            gpu: false,
            cpu_time_ms: 5_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 64 * 1024 * 1024,
        },
        inputs: vec![input.to_owned()],
    }
}

/// The invocation a person makes of `request` in the app.
fn invoked(request: &OffloadRequest) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::application_action_target(
            WORKSPACE,
            APP,
            request.address.resource.clone(),
            request.address.action.clone(),
        ),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// The door over `media` and `peers`, on a device that has already run
/// `request` here once in [`LOCAL_RUN`]: the run the round weighs moving the
/// inputs against.
fn door(
    media: &Arc<Media>,
    peers: &Arc<Peers>,
    request: &OffloadRequest,
) -> Arc<dyn ActionOffload> {
    let route = offer_offload(
        Arc::clone(media) as Arc<dyn WorkspaceMedia>,
        Arc::clone(peers) as Arc<dyn OffloadPeers>,
    )
    .expect("offload is on");
    let first = drive(route.offload_invocation(request.clone(), invoked(request)))
        .expect("a job never run here answers at once");
    assert!(
        matches!(first, Offloaded::RunHere { .. }),
        "a job this device has never run stays here: {first:?}"
    );
    route.ran_here(request, LOCAL_RUN);
    peers.asked();
    peers.invoked();
    media.scopes();
    route
}

/// The round weighs what this device holds: each input's bytes as the
/// invoking app reads them, and how long the same transform last took here.
#[test]
fn the_round_is_weighed_with_the_inputs_as_the_app_reads_them() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Silent));
    let route = door(&media, &peers, &request);

    let _ = drive(route.offload_invocation(request.clone(), invoked(&request)));
    let asked = peers.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let (workspace, asked_route, work) = &asked[0];
    assert_eq!(workspace, WORKSPACE);
    assert_eq!(asked_route, &Route::new(APP, "photo", "thumbnail"));
    let mut named = [0; 32];
    hex::decode_to_slice(&input, &mut named).unwrap();
    assert_eq!(work.inputs, vec![(named, INPUT_BYTES)]);
    assert_eq!(work.input_bytes, INPUT_BYTES);
    assert_eq!(
        work.local_wall_ms,
        Some(u64::try_from(LOCAL_RUN.as_millis()).unwrap()),
        "the run here is what moving the inputs has to beat"
    );
    assert_eq!(
        media.scopes(),
        vec![(WORKSPACE.to_owned(), APP.to_owned())],
        "the inputs are read as the invoking app reads them, in its workspace"
    );
}

/// The invocation runs on the device the round picks, and what it produced is
/// kept where the invoking app reads its output.
#[test]
fn the_device_that_wins_the_round_runs_it_and_its_output_is_kept_for_the_app() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let thumbnail = vec![9; 1_024];
    let produced = digest(&thumbnail);
    let expected = ActionOutput {
        value: Some(serde_json::json!({ "thumbnail": "done" })),
        resources: vec![format!("tilo.content:{produced}")],
    };
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Ran {
        output: expected.clone(),
        content: thumbnail.clone(),
    }));
    let route = door(&media, &peers, &request);

    let person = invoked(&request);
    let ran =
        drive(route.offload_invocation(request.clone(), person.clone())).expect("answers at once");
    let Offloaded::Ran { output, device, .. } = ran else {
        panic!("the round picked a device: {ran:?}");
    };
    assert_eq!(output, expected, "the app gets the answer made elsewhere");
    assert_eq!(device, hex::encode(SERVER));
    assert_eq!(
        media.held(&produced),
        Some(thumbnail),
        "the app reads the answer by the address the run names"
    );
    assert_eq!(
        media.scopes(),
        vec![(WORKSPACE.to_owned(), APP.to_owned()); 2],
        "the inputs are read, and the output kept, as the invoking app's own content"
    );
    assert_eq!(
        peers.invoked(),
        vec![(WORKSPACE.to_owned(), SERVER, person)],
        "the device that won runs the invocation the person made, once"
    );
}

/// A job this device has never run is not sent away: there is no run here to
/// beat, so it runs here and the device's own time teaches the next round.
#[test]
fn a_job_never_run_here_is_not_sent_away() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Ran {
        output: ActionOutput::default(),
        content: Vec::new(),
    }));
    let route = offer_offload(
        Arc::clone(&media) as Arc<dyn WorkspaceMedia>,
        Arc::clone(&peers) as Arc<dyn OffloadPeers>,
    )
    .expect("offload is on");

    let ran = drive(route.offload_invocation(request.clone(), invoked(&request)))
        .expect("answers at once");
    let Offloaded::RunHere { reason } = ran else {
        panic!("nothing has run here yet: {ran:?}");
    };
    assert!(reason.contains("nothing is quicker"), "{reason}");
    assert!(peers.invoked().is_empty(), "no device was given the job");
    assert!(
        peers.asked().is_empty(),
        "a run with nothing to beat asks nobody: the round would only add waiting"
    );
}

/// A device that cannot run it leaves the job here, and says why.
#[test]
fn a_failing_device_leaves_the_job_here_with_the_reason() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Failed("no such app".to_owned())));
    let route = door(&media, &peers, &request);

    let ran = drive(route.offload_invocation(request.clone(), invoked(&request)))
        .expect("answers at once");
    let Offloaded::RunHere { reason } = ran else {
        panic!("the device that was picked failed: {ran:?}");
    };
    assert!(reason.contains("did not run it: no such app"), "{reason}");
    assert!(
        reason.contains(&hex::encode(&SERVER[..4])),
        "the reason names the device that failed: {reason}"
    );
}

/// Bytes that are not the content an answer names never become the app's
/// output.
#[test]
fn bytes_that_are_not_the_content_it_names_never_become_the_output() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let named = digest(&[9; 1_024]);
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Ran {
        output: ActionOutput {
            value: None,
            resources: vec![format!("tilo.content:{named}")],
        },
        content: vec![1, 2, 3],
    }));
    let route = door(&media, &peers, &request);

    let ran = drive(route.offload_invocation(request.clone(), invoked(&request)))
        .expect("answers at once");
    let Offloaded::RunHere { reason } = ran else {
        panic!("the answer is not the content it names: {ran:?}");
    };
    assert!(reason.contains("not the content it names"), "{reason}");
    assert_eq!(media.held(&named), None);
}

/// An invocation that comes back to this device is not sent away again: it is
/// already running where it was sent, and the app runs this one here.
#[test]
fn an_invocation_that_comes_back_is_not_sent_away_again() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let request = request(&input);
    let (completer, answered) = pending();
    let peers = Arc::new(Peers::new(Reply::Later(answered)));
    let route = door(&media, &peers, &request);

    let person = invoked(&request);
    let mut sent = route.offload_invocation(request.clone(), person.clone());
    assert!(
        drive(&mut sent).is_none(),
        "it waits on the device that won the round"
    );
    let back = drive(route.offload_invocation(request.clone(), person))
        .expect("a job already away answers at once");
    let Offloaded::RunHere { reason } = back else {
        panic!("the invocation is already running elsewhere: {back:?}");
    };
    assert!(reason.contains("already running"), "{reason}");

    completer.complete(Ok(RanAway {
        device: SERVER,
        output: ActionOutput::default(),
        outputs: Vec::new(),
    }));
    assert!(
        matches!(drive(&mut sent), Some(Offloaded::Ran { .. })),
        "the device that ran it answered the person's invocation"
    );
}

/// A job whose input is not on this device, or is not a content hash at all,
/// runs here: content this device cannot read is nothing it can weigh moving.
#[test]
fn a_job_whose_input_is_not_here_runs_here() {
    let media = Arc::new(Media::default());
    let peers = Arc::new(Peers::new(Reply::Ran {
        output: ActionOutput::default(),
        content: Vec::new(),
    }));
    let route = offer_offload(
        Arc::clone(&media) as Arc<dyn WorkspaceMedia>,
        Arc::clone(&peers) as Arc<dyn OffloadPeers>,
    )
    .expect("offload is on");

    let elsewhere = request(&digest(&[7; 16]));
    let ran = drive(route.offload_invocation(elsewhere.clone(), invoked(&elsewhere))).unwrap();
    let Offloaded::RunHere { reason } = ran else {
        panic!("the input is not here: {ran:?}");
    };
    assert!(reason.contains("not on this device yet"), "{reason}");

    let wrong = request("not a content hash");
    let ran = drive(route.offload_invocation(wrong.clone(), invoked(&wrong))).unwrap();
    let Offloaded::RunHere { reason } = ran else {
        panic!("that is not content: {ran:?}");
    };
    assert!(reason.contains("not a content hash"), "{reason}");
    assert!(peers.asked().is_empty(), "nothing weighable was asked");
}

/// A route handed no invocation runs the job here: without the invocation
/// there is nothing to send to another device.
#[test]
fn a_route_handed_no_invocation_runs_here() {
    let media = Arc::new(Media::default());
    let input = media.hold(&input());
    let request = request(&input);
    let peers = Arc::new(Peers::new(Reply::Ran {
        output: ActionOutput::default(),
        content: Vec::new(),
    }));
    let route = door(&media, &peers, &request);

    let ran = drive(route.offload(request.clone())).expect("answers at once");
    let Offloaded::RunHere { reason } = ran else {
        panic!("there is no invocation to send: {ran:?}");
    };
    assert!(reason.contains("not handed to the route"), "{reason}");
    assert!(peers.asked().is_empty());
}

/// Offload turned off installs no route at all: every job runs here.
#[test]
fn offload_turned_off_installs_no_route() {
    let media: Arc<dyn WorkspaceMedia> = Arc::new(Media::default());
    let peers: Arc<dyn OffloadPeers> = Arc::new(Peers::new(Reply::Silent));
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(OFFLOAD_ENV, "off");
    assert!(offer_offload(Arc::clone(&media), Arc::clone(&peers)).is_none());
    env.remove(OFFLOAD_ENV);
    assert!(offer_offload(media, peers).is_some());
}

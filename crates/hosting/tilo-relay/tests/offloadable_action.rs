//! An offloadable action — thumbnailing a batch of large images — invoked on a
//! device of a workspace runs on the workspace's host when the offer round
//! picks it there: the host answers with its own standing and the inputs it
//! already keeps, the invocation runs there as a compute job over that
//! content, and the answer is exactly what the action's own handler answered
//! on the invoking device. A host that does not hold the inputs is no quicker
//! than the run here, and work it cannot stage is refused, never run over
//! content that is not there.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until the test ends"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use launcher::capture::{CapturedRunOutput, CapturedTermination};
use runtime_fs::blobs::BlobScope;
use thumbnail::batch::{ENTRYPOINT, PACKAGE};
use tilo_actions::offload::{OffloadRequest, Offloaded, content_inputs};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, application_action_target};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_clock::unix_now_ns;
use tilo_peer_state::actions::{ActionPlane, NoActions};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_relay::host::HostMode;
use tilo_relay::host_jobs::ComputeJobs;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::workspaces::IssueInvitationRequest;
use tilo_relay::{ComputeContentStore, ComputeSandbox, ComputeSandboxInvocation};
use tilo_runtime::offload::{
    BindError, MediaFuture, OutputContent, PlanePeers, WorkspaceMedia, offer_offload,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::compute::{FixtureCatalogue, installed_package};
use crate::common::{Hosted, TestDevice, TestUser, device_endpoint, host_on};

/// Images in the batch, and their size: camera originals, roughly.
const IMAGES: usize = 6;
const WIDTH: u32 = 1200;
const HEIGHT: u32 = 800;

/// The photos app, whose `make-thumbnails` this workspace publishes.
const PHOTOS: &str = "so.tilo.photos";

/// The thumbnails package's entrypoint as the worker's sandbox runs it,
/// without a nested user namespace: the very function the package binary
/// calls, over the job directory the worker staged.
struct ThumbnailSandbox;

impl ComputeSandbox for ThumbnailSandbox {
    fn gpu_available(&self) -> bool {
        false
    }

    fn launch(
        &self,
        invocation: ComputeSandboxInvocation<'_>,
    ) -> Result<CapturedRunOutput, String> {
        use std::fmt::Write as _;
        use std::os::unix::process::ExitStatusExt;
        if invocation.entrypoint != ENTRYPOINT {
            return Err(format!("no catalogue command {}", invocation.entrypoint));
        }
        let io = invocation.host_job_dir.join(".tilo-compute");
        let (code, stderr) = match thumbnail::batch::run_compute_request(
            &io.join("request.json"),
            &io.join("outputs"),
        ) {
            Ok(failed) => (
                0,
                failed
                    .iter()
                    .fold(String::new(), |mut stderr, failure| {
                        let _ = writeln!(stderr, "thumbnail-batch: {failure}");
                        stderr
                    })
                    .into_bytes(),
            ),
            Err(error) => (i32::from(error.exit_code()), error.to_string().into_bytes()),
        };
        Ok(CapturedRunOutput {
            termination: CapturedTermination::Exited(std::process::ExitStatus::from_raw(
                code.checked_mul(256).unwrap(),
            )),
            stdout: Vec::new(),
            stderr,
        })
    }
}

/// A large photograph-like PNG, different for each `seed`: smooth
/// gradients under sensor-like grain.
fn photograph(seed: u32) -> Vec<u8> {
    let pixels = usize::try_from(WIDTH.wrapping_mul(HEIGHT)).unwrap();
    let mut rgba = Vec::with_capacity(pixels.wrapping_mul(4));
    let mut noise = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let channel = |value: u32, grain: u8| u8::try_from(value % 224).unwrap().wrapping_add(grain);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            noise ^= noise.wrapping_shl(13);
            noise ^= noise.wrapping_shr(17);
            noise ^= noise.wrapping_shl(5);
            let grain = u8::try_from(noise & 0x1f).unwrap();
            rgba.extend_from_slice(&[
                channel(x.wrapping_add(seed.wrapping_mul(37)), grain),
                channel(y.wrapping_add(seed.wrapping_mul(11)), grain),
                channel(x.wrapping_add(y), grain),
                255,
            ]);
        }
    }
    thumbnail::encode_png(&thumbnail::RgbaThumb {
        rgba,
        width: WIDTH,
        height: HEIGHT,
    })
    .unwrap()
}

/// The workspace's content, as every device of it reaches it: one place
/// holding the bytes of each digest, the way a host's store and the content
/// its members share are the workspace's own. The invoking device reads and
/// keeps its app's media here; the host reads the job's inputs and keeps its
/// outputs here.
#[derive(Default)]
struct Media(Mutex<BTreeMap<[u8; 32], Vec<u8>>>);

impl Media {
    /// Keep `bytes` and answer their digest.
    fn store(&self, bytes: &[u8]) -> [u8; 32] {
        let digest = *blake3::hash(bytes).as_bytes();
        self.0.lock().unwrap().insert(digest, bytes.to_vec());
        digest
    }

    /// The bytes of `digest`, when they are here.
    fn bytes_of(&self, digest: [u8; 32]) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(&digest).cloned()
    }
}

impl WorkspaceMedia for Media {
    fn len<'a>(
        &'a self,
        _scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        Box::pin(async move {
            self.bytes_of(digest)
                .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .ok_or(BindError::NotLocal)
        })
    }

    fn read<'a>(
        &'a self,
        _scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        Box::pin(async move { self.bytes_of(digest).ok_or(BindError::NotLocal) })
    }

    fn keep<'a>(
        &'a self,
        _scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move { Ok(self.store(&bytes)) })
    }
}

impl OutputContent for Media {
    fn content<'a>(
        &'a self,
        _workspace: &'a str,
        _app: &'a str,
        digest: [u8; 32],
        _within: Duration,
    ) -> MediaFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { self.bytes_of(digest) })
    }
}

impl ComputeContentStore for Media {
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        Ok(self.bytes_of(digest))
    }

    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        if blake3::hash(bytes).as_bytes() != &digest {
            return Err("content does not match its digest".into());
        }
        self.0.lock().unwrap().insert(digest, bytes.to_vec());
        Ok(())
    }
}

/// Admit `laptop` to `workspace` as its invited member device, through the
/// workspace-join exchange the workspace's host answers, so the host knows the
/// device that asks it for work.
async fn invited(
    hosted: &Hosted,
    workspace: &str,
    user: &TestUser,
    laptop: &TestDevice,
    endpoint: &iroh::Endpoint,
) {
    let invitation = hosted
        .host
        .workspaces()
        .issue(
            workspace,
            hosted.host.device_cert(),
            IssueInvitationRequest {
                request_id: [3; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(user.user_pk),
                invitee_name: None,
                expires_at_ns: unix_now_ns().checked_add(600_000_000_000).unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        Some(user.user_pk),
        &RevocationList::new(),
        unix_now_ns(),
    )
    .unwrap();
    let admission = WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            laptop.cert.clone(),
            &laptop.sk,
            [7; 32],
        )
        .await
        .unwrap();
    assert_eq!(
        admission.bootstrap.workspace_id.to_string(), workspace,
        "the workspace this device joined"
    );
}

/// `laptop`'s plane: it asks the workspace's other devices and answers
/// nothing itself, and holds the workspace as the devices its governor
/// certified.
fn asking_plane(laptop: &TestDevice, host: &TestDevice, workspace: &str) -> ActionPlane {
    let now = std::time::SystemTime::now();
    let roster = vec![laptop.cert.clone(), host.cert.clone()];
    let mut peer = StatePeer::new(
        workspace,
        laptop.cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &laptop.cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &laptop.sk,
        now,
    )
    .unwrap();
    let plane = ActionPlane::new(laptop.sk.clone(), Arc::new(NoActions));
    plane.hold(Replica::new(peer));
    plane
}

/// The resource Photos publishes `make-thumbnails` on.
const LIBRARY: &str = "photos.library";

/// The arguments of a `make-thumbnails` invocation over `hashes`.
fn photos_argument(hashes: &[String]) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([("photos".to_owned(), serde_json::json!(hashes))])
}

/// Photos' published `make-thumbnails` in `workspace` and a request for it
/// over `hashes`, as the door builds one from an admitted invocation's
/// arguments.
fn make_thumbnails_request(workspace: &str, hashes: &[String]) -> OffloadRequest {
    use tilo_resource::Effect as _;
    let spec = photos::actions::PhotosEffect::spec("make-thumbnails")
        .expect("Photos publishes make-thumbnails");
    OffloadRequest {
        workspace: workspace.into(),
        app_id: PHOTOS.into(),
        address: ActionAddress::new(LIBRARY, "make-thumbnails"),
        offload: spec.contract.offload.clone().expect("declared offloadable"),
        inputs: content_inputs(spec.params(), &photos_argument(hashes)).unwrap(),
    }
}

/// `workspace`'s `make-thumbnails` over `hashes`, as Photos' door sends it.
fn invocation(workspace: &str, hashes: &[String]) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: application_action_target(workspace, PHOTOS, LIBRARY, "make-thumbnails"),
        argument: InvokeArgument::Json(photos_argument(hashes)),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one invocation journey, here then on the host, that reads top to bottom"
)]
async fn a_host_that_wins_the_offer_runs_the_action_as_a_job_and_answers_its_output() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_ACTION_OFFLOAD");
    let owner = TestUser::fresh("offload owner");
    let desk = owner.enrol("desk", DeviceCapabilities::primary());
    let desk_key = desk.device_pubkey();
    let laptop = owner.enrol("laptop", DeviceCapabilities::member());
    let root = tempfile::tempdir().unwrap();

    // The host governs the workspace and keeps its content: the photographs
    // are the workspace's, so the device that asks reads them exactly where
    // the host does.
    let hosted = host_on(&owner, &desk, HostMode::Device).await;
    let created = hosted.host.workspaces().create("Photos").await.unwrap();
    let workspace = created.workspace.clone();
    let content = Arc::new(Media::default());
    let photographs: Vec<Vec<u8>> = (0..IMAGES)
        .map(|seed| photograph(u32::try_from(seed).unwrap()))
        .collect();
    let mut hashes = Vec::new();
    for bytes in &photographs {
        hashes.push(hex::encode(content.store(bytes)));
    }
    let (package, _) = installed_package(&root.path().join("host"), PACKAGE, ENTRYPOINT);
    let jobs = |content: Arc<Media>, dir: &str| {
        Arc::new(ComputeJobs::new(
            content as Arc<dyn ComputeContentStore>,
            Arc::new(FixtureCatalogue(package.clone())),
            Arc::new(ThumbnailSandbox),
            desk_key,
            root.path().join(format!("{dir}-data")),
            root.path().join(format!("{dir}-state")),
        ))
    };
    hosted
        .host
        .workspaces()
        .run_jobs(jobs(Arc::clone(&content), "host"));

    // The laptop is a member device of the workspace: it joined it, as any
    // device does, so the host answers it.
    let laptop_endpoint = device_endpoint(&laptop).await;
    invited(&hosted, &workspace, &owner, &laptop, &laptop_endpoint).await;
    let plane = asking_plane(&laptop, &desk, &workspace);
    let host_addr = hosted.host.node_addr();
    let elsewhere = host_addr.clone();
    let peers = PlanePeers::new(
        plane.clone(),
        laptop_endpoint.clone(),
        move |_device| host_addr.clone(),
        Arc::clone(&content) as Arc<dyn OutputContent>,
        tokio::runtime::Handle::current(),
    );
    let route = offer_offload(
        Arc::clone(&content) as Arc<dyn WorkspaceMedia>,
        Arc::new(peers),
    )
    .expect("offload is not turned off");

    // An agent asks for thumbnails of every photograph, one named twice.
    let mut named = hashes.clone();
    named.push(hashes[0].clone());
    let request = make_thumbnails_request(&workspace, &named);
    assert_eq!(request.inputs.len(), IMAGES + 1);

    // The round is asked before the action has ever run here: moving the
    // inputs is weighed against nothing, so nothing is quicker than it, and
    // the action stays here.
    let first = tilo_test_env::within(
        "the first round",
        route.offload_invocation(request.clone(), invocation(&workspace, &named)),
    )
    .await;
    let Offloaded::RunHere { reason } = first else {
        panic!("nothing is quicker than a run it knows nothing of: {first:?}");
    };
    assert!(
        reason.contains("nothing is quicker than a run it knows nothing of"),
        "{reason}"
    );

    // Photos' handler runs it here, and the door reports how long it took.
    let app_content = tilo_content::ContentStore::memory();
    let held: Vec<tilo_resource::Bytes> = photographs
        .iter()
        .zip(&hashes)
        .map(|(bytes, hash)| {
            app_content.put(bytes).unwrap();
            tilo_resource::Bytes::kept_as(bytes, hash)
        })
        .collect();
    let mut here_named = held.clone();
    here_named.push(held[0].clone());
    let started = Instant::now();
    let here = photos::actions::make_thumbnails(&here_named, &app_content).unwrap();
    let took = started.elapsed();
    assert_eq!(here.resources.len(), IMAGES, "one thumbnail per photograph");
    route.ran_here(&request, took);

    // The host holds every input, so moving them costs nothing: it wins the
    // round, runs the transform as a compute job over the content it keeps,
    // and answers with exactly what Photos answered here.
    let second = tilo_test_env::within(
        "the offloaded run",
        route.offload_invocation(request.clone(), invocation(&workspace, &named)),
    )
    .await;
    let Offloaded::Ran { output, device, .. } = second else {
        panic!("the host is better placed: {second:?}");
    };
    assert_eq!(device, hex::encode(desk_key));
    assert_eq!(output, here, "the same output, wherever it ran");
    for resource in &output.resources {
        let hash = tilo_actions::content_hash(resource).unwrap();
        let mut digest = [0; 32];
        hex::decode_to_slice(hash, &mut digest).unwrap();
        assert_eq!(
            content.bytes_of(digest),
            app_content.read(hash),
            "byte for byte the local thumbnail, kept where the workspace reads it"
        );
    }

    // A host that holds none of the inputs is weighed as receiving all of
    // them, which takes longer than the run here took, so it is not picked
    // and the action stays here.
    hosted
        .host
        .workspaces()
        .run_jobs(jobs(Arc::new(Media::default()), "empty"));
    let third = tilo_test_env::within(
        "the round the host no longer wins",
        route.offload_invocation(request.clone(), invocation(&workspace, &named)),
    )
    .await;
    let Offloaded::RunHere { reason } = third else {
        panic!("a host that holds nothing is no quicker: {third:?}");
    };
    assert!(reason.contains("no faster than the run here"), "{reason}");

    // A device that is not connected here now is not asked: the round has
    // nobody to ask, so the action stays here rather than waiting for an
    // answer that cannot come.
    let unconnected = offer_offload(
        Arc::clone(&content) as Arc<dyn WorkspaceMedia>,
        Arc::new(
            PlanePeers::new(
                plane,
                laptop_endpoint,
                move |_device| elsewhere.clone(),
                Arc::clone(&content) as Arc<dyn OutputContent>,
                tokio::runtime::Handle::current(),
            )
            .reachable(|_, _| false),
        ),
    )
    .expect("offload is not turned off");
    // What a run here takes is what the round weighs: this door learns it
    // the way any door does, from the invocation it ran here.
    let warm = unconnected
        .offload_invocation(request.clone(), invocation(&workspace, &named))
        .await;
    assert!(
        matches!(warm, Offloaded::RunHere { .. }),
        "nothing is quicker on a door that knows nothing yet: {warm:?}"
    );
    unconnected.ran_here(&request, took);
    let fourth = tilo_test_env::within(
        "the round with nobody connected",
        unconnected.offload_invocation(request.clone(), invocation(&workspace, &named)),
    )
    .await;
    let Offloaded::RunHere { reason } = fourth else {
        panic!("nobody was asked: {fourth:?}");
    };
    assert!(reason.contains("no device offered to run it"), "{reason}");
}

/// A host takes on as many jobs as it says it does: with one slot, the second
/// job is refused where it is sent, and the offer a full host answers says so.
#[tokio::test]
async fn a_host_at_its_capacity_takes_no_second_job() {
    use tilo_room_host::workspaces::HostJobs as _;
    let desk = TestUser::fresh("capacity host").enrol("desk", DeviceCapabilities::primary());
    let root = tempfile::tempdir().unwrap();
    let content = Arc::new(Media::default());
    let (package, _) = installed_package(&root.path().join("host"), PACKAGE, ENTRYPOINT);
    let jobs = ComputeJobs::new(
        Arc::clone(&content) as Arc<dyn ComputeContentStore>,
        Arc::new(FixtureCatalogue(package)),
        Arc::new(ThumbnailSandbox),
        desk.device_pubkey(),
        root.path().join("data"),
        root.path().join("state"),
    )
    .with_capacity(1);

    let workspace = "33333333-3333-4333-8333-333333333333";
    let photograph = photograph(1);
    let saved = content.store(&photograph);
    let bytes = u64::try_from(photograph.len()).unwrap();
    let named = vec![hex::encode(saved)];
    let declared = make_thumbnails_request(workspace, &named);
    let work = neighbour_wire::offer::Work {
        inputs: vec![(saved, bytes)],
        input_bytes: bytes,
        local_wall_ms: Some(1_000),
        offload: Some(declared.offload),
    };
    assert!(jobs.runs(workspace, &work), "the host takes the work");
    assert_eq!(jobs.free_slots(), Some(1), "it has a slot for it");

    // It takes the first job, and with it the one slot it has: an offer
    // arriving now is answered as taken by nobody, so the device that asks
    // does not send the job to a host that would refuse it.
    let requester = [0x11; 32];
    let first = jobs.run(workspace, &work, requester, invocation(workspace, &named));
    assert_eq!(jobs.free_slots(), Some(0), "the slot the first job took");

    // A second job arriving now is refused at the door, never queued behind
    // the first or run over its slot.
    let second = jobs
        .run(workspace, &work, requester, invocation(workspace, &named))
        .await
        .expect("the host answers the job it was sent");
    let Err(tilo_actions::remote::ActionHostError::Unavailable { reason }) = second else {
        panic!("a host at its capacity takes no more: {second:?}");
    };
    assert!(reason.contains("as many jobs as it takes on"), "{reason}");

    let ran = first.await.expect("the host answers the job it took");
    assert!(ran.is_ok(), "the job it took ran: {ran:?}");
    assert_eq!(jobs.free_slots(), Some(1), "the slot is free again");
}

/// A host that takes work in still refuses the job when the inputs it was
/// offered are not in its store: the device that asked is told the job did not
/// run, and runs the action itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_refuses_a_job_it_does_not_hold_the_inputs_for() {
    use tilo_room_host::workspaces::HostJobs as _;
    let desk = TestUser::fresh("job host").enrol("desk", DeviceCapabilities::primary());
    let root = tempfile::tempdir().unwrap();
    let (package, _) = installed_package(&root.path().join("host"), PACKAGE, ENTRYPOINT);
    let jobs = ComputeJobs::new(
        Arc::new(Media::default()) as Arc<dyn ComputeContentStore>,
        Arc::new(FixtureCatalogue(package)),
        Arc::new(ThumbnailSandbox),
        desk.device_pubkey(),
        root.path().join("data"),
        root.path().join("state"),
    );
    let workspace = "11111111-1111-4111-8111-111111111111";
    let named = vec!["ab".repeat(32)];
    let declared = make_thumbnails_request(workspace, &named);
    let mut digest = [0; 32];
    hex::decode_to_slice(&named[0], &mut digest).unwrap();
    let work = neighbour_wire::offer::Work {
        inputs: vec![(digest, 1_024)],
        input_bytes: 1_024,
        local_wall_ms: None,
        offload: Some(declared.offload),
    };

    assert!(jobs.runs(workspace, &work), "the package is installed here");
    assert!(
        jobs.holds(workspace, &work).is_empty(),
        "and the input it names is not"
    );

    let ran = jobs
        .run(workspace, &work, [9; 32], invocation(workspace, &named))
        .await
        .expect("the host answers the job it was sent");
    let Err(tilo_actions::remote::ActionHostError::Refused { reason }) = ran else {
        panic!("a job over content the host does not have is refused: {ran:?}");
    };
    assert!(reason.contains("the job did not run"), "{reason}");
}

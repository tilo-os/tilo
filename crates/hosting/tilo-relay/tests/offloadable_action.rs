//! An offloadable action — thumbnailing a batch of large images — invoked on a
//! device of a workspace runs on the workspace's host when the offer round
//! picks it there: the host answers with its own standing and the inputs it
//! already keeps, the invocation runs there as a compute job over that
//! content, and the answer is exactly what the action's own handler answered
//! on the invoking device. A host that does not take the work, or takes it and
//! cannot run it, leaves the action where it was asked and says why.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until the test ends"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::protocol::Router;
use launcher::capture::{CapturedRunOutput, CapturedTermination};
use runtime_fs::blobs::BlobScope;
use thumbnail::batch::{ENTRYPOINT, PACKAGE, thumbnail_batch};
use tilo_actions::offload::{OffloadRequest, Offloaded, content_inputs};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, application_action_target};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_clock::unix_now_ns;
use tilo_peer_state::actions::{ActionPlane, NoActions};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_relay::host::{HostConfig, HostIdentity, HostMode, RoomHost};
use tilo_relay::host_jobs::ComputeJobs;
use tilo_relay::rooms::IssueInvitationRequest;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{ComputeContentStore, ComputeSandbox, ComputeSandboxInvocation};
use tilo_runtime::offload::{
    BindError, MediaFuture, OutputContent, PlanePeers, RoomMedia, offer_offload,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::TestDevice;
use crate::common::TestUser;
use crate::common::compute::{FixtureCatalogue, installed_package};

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
    fn put(&self, bytes: &[u8]) -> [u8; 32] {
        let digest = *blake3::hash(bytes).as_bytes();
        self.0.lock().unwrap().insert(digest, bytes.to_vec());
        digest
    }

    /// The bytes of `digest`, when they are here.
    fn get(&self, digest: [u8; 32]) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(&digest).cloned()
    }
}

impl RoomMedia for Media {
    fn len<'a>(
        &'a self,
        _scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        Box::pin(async move {
            self.get(digest)
                .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .ok_or(BindError::NotLocal)
        })
    }

    fn read<'a>(
        &'a self,
        _scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        Box::pin(async move { self.get(digest).ok_or(BindError::NotLocal) })
    }

    fn keep<'a>(
        &'a self,
        _scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move { Ok(self.put(&bytes)) })
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
        Box::pin(async move { self.get(digest) })
    }
}

impl ComputeContentStore for Media {
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        Ok(Media::get(self, digest))
    }

    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        if blake3::hash(bytes).as_bytes() != &digest {
            return Err("content does not match its digest".into());
        }
        self.0.lock().unwrap().insert(digest, bytes.to_vec());
        Ok(())
    }
}

/// An endpoint whose key is the device's own, as a device dials with.
async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A host governing its own workspaces, on its own endpoint, routed as its
/// embedder routes it.
struct Hosted {
    host: RoomHost,
    router: Router,
    _state: tempfile::TempDir,
}

async fn host_on(user: &TestUser, device: &TestDevice) -> Hosted {
    let state = tempfile::tempdir().unwrap();
    let endpoint = device_endpoint(device).await;
    let host = RoomHost::open(
        HostConfig::new(state.path().to_path_buf(), HostMode::Device),
        HostIdentity {
            user_public_key: user.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .unwrap();
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Hosted {
        host,
        router: router.spawn(),
        _state: state,
    }
}

/// Admit `laptop` to `room` as its invited member device, through the
/// workspace-join exchange the room's governor answers, so the host knows the
/// device that asks it for work.
async fn invited(
    hosted: &Hosted,
    room: &str,
    user: &TestUser,
    laptop: &TestDevice,
    endpoint: &iroh::Endpoint,
) {
    let invitation = hosted
        .host
        .rooms()
        .issue(
            room,
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
        admission.bootstrap.room, room,
        "the room this device joined"
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

/// Photos' published `make-thumbnails` in `workspace` and a request for it
/// over `hashes`, as the door builds one from an admitted invocation's
/// arguments.
fn make_thumbnails_request(workspace: &str, hashes: &[String]) -> OffloadRequest {
    use tilo_resource::Effect as _;
    let spec = photos::actions::PhotosEffect::specs()
        .into_iter()
        .find(|spec| spec.id == "make-thumbnails")
        .expect("Photos publishes make-thumbnails");
    let arguments = serde_json::json!({ "photos": hashes });
    OffloadRequest {
        workspace: workspace.into(),
        app_id: PHOTOS.into(),
        address: ActionAddress::new("photos.library:main", "make-thumbnails"),
        offload: spec.contract.offload.clone().expect("declared offloadable"),
        inputs: content_inputs(spec.params(), arguments.as_object().unwrap()).unwrap(),
    }
}

/// `workspace`'s `make-thumbnails` over `hashes`, as Photos' door sends it.
fn invocation(workspace: &str, hashes: &[String]) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: application_action_target(
            workspace,
            PHOTOS,
            "photos.library:main",
            "make-thumbnails",
        ),
        argument: InvokeArgument::Json(serde_json::json!({ "photos": hashes })),
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
    let relay = owner.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay.device_pubkey();
    let laptop = owner.enrol("laptop", DeviceCapabilities::primary());
    let root = tempfile::tempdir().unwrap();

    // The host governs the workspace and keeps its content: the photographs
    // are the workspace's, so the device that asks reads them exactly where
    // the host does.
    let hosted = host_on(&owner, &relay).await;
    let room = hosted.host.rooms().create("Photos").await.unwrap();
    let workspace = room.room.clone();
    let content = Arc::new(Media::default());
    let photographs: Vec<Vec<u8>> = (0..IMAGES)
        .map(|seed| photograph(u32::try_from(seed).unwrap()))
        .collect();
    let mut hashes = Vec::new();
    for bytes in &photographs {
        hashes.push(hex::encode(content.put(bytes)));
    }
    let (package, _) = installed_package(&root.path().join("host"), PACKAGE, ENTRYPOINT);
    let jobs = |content: Arc<Media>, dir: &str| {
        Arc::new(ComputeJobs::new(
            content as Arc<dyn ComputeContentStore>,
            Arc::new(FixtureCatalogue(package.clone())),
            Arc::new(ThumbnailSandbox),
            relay_key,
            root.path().join(format!("{dir}-data")),
            root.path().join(format!("{dir}-state")),
        ))
    };
    hosted
        .host
        .rooms()
        .run_jobs(jobs(Arc::clone(&content), "host"));

    // The laptop is a member device of the workspace: it joined it, as any
    // device does, so the host answers it.
    let laptop_endpoint = device_endpoint(&laptop).await;
    invited(&hosted, &workspace, &owner, &laptop, &laptop_endpoint).await;
    let plane = asking_plane(&laptop, &relay, &workspace);
    let host_addr = hosted.host.node_addr();
    let peers = PlanePeers::new(
        plane,
        laptop_endpoint,
        move |_device| host_addr.clone(),
        Arc::clone(&content) as Arc<dyn OutputContent>,
        tokio::runtime::Handle::current(),
    );
    let route = offer_offload(Arc::clone(&content) as Arc<dyn RoomMedia>, Arc::new(peers))
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
    assert!(reason.contains("no faster than the run here"), "{reason}");

    // Photos' handler runs it here, and the door reports how long it took.
    let app_content = tilo_content::ContentStore::memory();
    for bytes in &photographs {
        app_content.put(bytes).unwrap();
    }
    let here = photos::actions::make_thumbnails(&named, &app_content).unwrap();
    assert_eq!(here.resources.len(), IMAGES, "one thumbnail per photograph");
    route.ran_here(&request, Duration::from_millis(400));

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
    assert_eq!(device, hex::encode(relay_key));
    assert_eq!(output, here, "the same output, wherever it ran");
    for resource in &output.resources {
        let hash = tilo_actions::content_hash(resource).unwrap();
        let mut digest = [0; 32];
        hex::decode_to_slice(hash, &mut digest).unwrap();
        assert_eq!(
            content.get(digest),
            app_content.read(hash),
            "byte for byte the local thumbnail, kept where the workspace reads it"
        );
    }

    // A host that takes the work and then finds it holds none of its inputs
    // cannot run it: the action still runs here, and the answer says what
    // the host did.
    hosted
        .host
        .rooms()
        .run_jobs(jobs(Arc::new(Media::default()), "empty"));
    route.ran_here(&request, Duration::from_secs(30));
    let third = tilo_test_env::within(
        "the round the host cannot run",
        route.offload_invocation(request.clone(), invocation(&workspace, &named)),
    )
    .await;
    let Offloaded::RunHere { reason } = third else {
        panic!("a host that cannot run it leaves the action here: {third:?}");
    };
    assert!(reason.contains("did not run it"), "{reason}");
}

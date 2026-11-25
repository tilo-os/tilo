//! An offloadable action — the thumbnail engine's batch of large images —
//! invoked on a phone runs on a server of the person's that offers compute: the
//! phone asks the workspace's devices it reaches to take the invocation's turn,
//! the server claims it, dispatches the action to the engine's provider on its
//! own dispatch, and answers exactly what the action's own handler answered on
//! the phone. A server that may not take the turn says why, and the action runs
//! on the phone.

#![allow(
    clippy::significant_drop_tightening,
    reason = "hosts, routers and fixtures deliberately stay alive until the test ends"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use runtime_fs::blobs::BlobScope;
use tilo_actions::offload::{OffloadRequest, Offloaded, content_inputs};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, application_action_target};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_clock::unix_now_ns;
use tilo_peer_state::actions::{ActionService, NoActions};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_resource::Id;
use tilo_runtime::offload::{
    ActionServicePeers, MediaFuture, OutputContent, WorkspaceMedia, turn_offload,
};
use tilo_server::WorkspaceId;
use tilo_server::servers::{self, Servers, person_workspace_id};
use tilo_server::workspace_join::WorkspaceJoinClient;
use tilo_server::workspaces::IssueInvitationRequest;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tilo_workspace_service::work::{Job, KeepsContent};
use tilo_workspace_service::workspaces::providers::Reach;

use crate::common::{Hosted, TestDevice, TestUser, device_endpoint, serve_on};

/// Images in the batch, and their size: camera originals, roughly.
const IMAGES: usize = 6;
const WIDTH: u32 = 1200;
const HEIGHT: u32 = 800;

/// The photos app, whose `make-thumbnails` this workspace publishes.
const PHOTOS: &str = "so.tilo.photos";

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
/// holding the bytes of each digest, the way a server's store and the content
/// its members share are the workspace's own. The phone keeps its app's media
/// here; the server reads the job's inputs and keeps its outputs here.
#[derive(Clone, Default)]
struct Media(Arc<Mutex<BTreeMap<[u8; 32], Vec<u8>>>>);

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

impl thumbnail::action::Content for Media {
    fn read(&self, _workspace: &str, _app: &str, digest: [u8; 32]) -> Option<Vec<u8>> {
        self.bytes_of(digest)
    }

    fn keep(&self, _workspace: &str, _app: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
        Ok(self.store(bytes))
    }
}

/// The content keeper the server installs beside its engine: the content its
/// actions read is the workspace's, which every device of it reaches.
impl KeepsContent for Media {
    fn holds(&self, _workspace: &str, job: &Job) -> bool {
        job.inputs
            .iter()
            .all(|digest| self.bytes_of(*digest).is_some())
    }
}

/// Serve the server's turns: the thumbnail engine's provider on its dispatch,
/// over `content`, and what it keeps of that content.
fn serve_jobs(hosted: &Hosted, content: &Media) {
    let workspaces = hosted.host.workspaces();
    workspaces.run_jobs(Arc::new(content.clone()));
    let surface = collab_types::surface::canonical(&serde_json::json!({
        "format": 2,
        "resources": [thumbnail::action::Thumbnails::surface()],
    }));
    workspaces.declare_package_surface(thumbnail::action::APP, surface.as_bytes());
    workspaces.providers().provide(
        thumbnail::action::PROVIDER,
        thumbnail::action::APP,
        Reach::Devices,
        Arc::new(thumbnail::action::Thumbnails::new(Arc::new(
            content.clone(),
        ))),
    );
}

/// The person's entry for `tower` among their servers, offering compute.
async fn offers_compute(hosted: &Hosted, owner: &TestUser, tower: &TestDevice) {
    let workspaces = hosted.host.workspaces();
    workspaces
        .create_workspace("Servers", Some(&person_workspace_id(&owner.user_pk)))
        .await
        .unwrap();
    servers::apply(
        workspaces,
        Servers::ENROLL_SERVER.with((
            hex::encode(tower.device_pubkey()),
            "tower".into(),
            serde_json::to_string(&hosted.host.node_addr()).unwrap(),
        )),
    )
    .await
    .unwrap();
    servers::apply(
        workspaces,
        Servers::SET_APPETITE.with((Id::from_raw(hex::encode(tower.device_pubkey())), 0, true, 0)),
    )
    .await
    .unwrap();
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
                role: Role::Editor,
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
        admission.bootstrap.workspace_id,
        WorkspaceId::parse(workspace).unwrap(),
        "the workspace this device joined"
    );
}

/// `laptop`'s action service: it asks the workspace's other devices and answers
/// nothing itself, and keeps its copy of the workspace as its member list
/// names the devices.
fn asking_action_service(laptop: &TestDevice, host: &TestDevice, workspace: &str) -> ActionService {
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
    peer.select_exact_admissions().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &laptop.cert, &roster).unwrap(),
            documents: Vec::new(),
        },
        &laptop.sk,
        now,
    )
    .unwrap();
    let action_service = ActionService::new(laptop.sk.clone(), Arc::new(NoActions));
    action_service.hold(Replica::new(peer));
    action_service
}

/// The resource Photos publishes `make-thumbnails` on.
const LIBRARY: &str = "photos.library";

/// The arguments of a `make-thumbnails` invocation over `hashes`.
fn photos_argument(hashes: &[String]) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([("photos".to_owned(), serde_json::json!(hashes))])
}

/// Photos' published `make-thumbnails` in `workspace` and a request for it
/// over `hashes`, as the action API builds one from an admitted invocation's
/// arguments.
fn make_thumbnails_request(workspace: &str, hashes: &[String]) -> OffloadRequest {
    let spec = <fixture::TestEffect as tilo_resource::Effect>::spec("make-thumbnails")
        .expect("the test's action publishes make-thumbnails");
    OffloadRequest {
        workspace: workspace.into(),
        app_id: PHOTOS.into(),
        address: ActionAddress::new(LIBRARY, "make-thumbnails"),
        offload: spec.contract.offload.clone().expect("declared offloadable"),
        inputs: content_inputs(spec.params(), &photos_argument(hashes)).unwrap(),
    }
}

/// `workspace`'s `make-thumbnails` over `hashes`, as Photos' API sends it.
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
        signed: None,
    }
}

/// Where `route` places `request`, the invocation `invocation`.
async fn placed(
    route: &Arc<dyn tilo_actions::offload::ActionOffload>,
    request: &OffloadRequest,
    invocation: InvokeRequest,
) -> Offloaded {
    tilo_test_env::within(
        "the turn",
        route.offload_invocation(request.clone(), invocation),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one invocation journey, on the phone then on the server, that reads top to bottom"
)]
async fn a_phone_asks_and_the_server_that_offers_compute_runs_the_action() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.remove("TILO_ACTION_OFFLOAD");
    let owner = TestUser::fresh("offload owner");
    let tower = owner.enrol("tower", DeviceCapabilities::primary());
    let phone = owner.enrol("phone", DeviceCapabilities::member());

    // The server serves the workspace and keeps its content: the
    // photographs are the workspace's, so the phone reads them exactly where
    // the server does.
    let hosted = serve_on(&owner, &tower, WorkSetting::own_person()).await;
    let workspace = hosted
        .host
        .workspaces()
        .create("Photos")
        .await
        .unwrap()
        .workspace;
    let content = Media::default();
    let photographs: Vec<Vec<u8>> = (0..IMAGES)
        .map(|seed| photograph(u32::try_from(seed).unwrap()))
        .collect();
    let hashes: Vec<String> = photographs
        .iter()
        .map(|bytes| hex::encode(content.store(bytes)))
        .collect();
    serve_jobs(&hosted, &content);
    assert!(
        hosted
            .host
            .workspaces()
            .providers()
            .published(
                &application_action_target(&workspace, PHOTOS, LIBRARY, "make-thumbnails"),
                Reach::Devices,
            )
            .is_some(),
        "the server's dispatch answers the action the engine publishes"
    );

    // The phone is a member device of the workspace: it joined it, as any
    // device does, so the server answers it.
    let phone_endpoint = device_endpoint(&phone).await;
    invited(&hosted, &workspace, &owner, &phone, &phone_endpoint).await;
    let action_service = asking_action_service(&phone, &tower, &workspace);
    let host_addr = hosted.host.node_addr();
    let takers = |reachable: bool| {
        let host_addr = host_addr.clone();
        ActionServicePeers::new(
            action_service.clone(),
            phone_endpoint.clone(),
            move |_device| host_addr.clone(),
            Arc::new(content.clone()) as Arc<dyn OutputContent>,
            tokio::runtime::Handle::current(),
        )
        .reachable(move |_, _| reachable)
    };
    let route = turn_offload(Arc::new(content.clone()), Arc::new(takers(true)))
        .expect("offload is not turned off");

    // An agent asks for thumbnails of every photograph, one named twice.
    let mut named = hashes.clone();
    named.push(hashes[0].clone());
    let request = make_thumbnails_request(&workspace, &named);
    assert_eq!(request.inputs.len(), IMAGES + 1);

    // The server's entry offers no compute yet: it does not take the turn,
    // and the action runs on the phone.
    let Offloaded::RunHere { reason } =
        placed(&route, &request, invocation(&workspace, &named)).await
    else {
        panic!("a server that offers no compute takes no turn");
    };
    assert!(reason.contains("offers no compute"), "{reason}");

    // What Photos' own handler answers on the phone.
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
    let here = fixture::make_thumbnails(&here_named, &app_content).unwrap();
    assert_eq!(here.resources.len(), IMAGES, "one thumbnail per photograph");

    // Once its entry offers compute, the server claims the turn, dispatches the
    // engine's action over the content it keeps, and answers exactly what
    // Photos answered on the phone.
    offers_compute(&hosted, &owner, &tower).await;
    let took = placed(&route, &request, invocation(&workspace, &named)).await;
    let Offloaded::Ran { output, device, .. } = took else {
        panic!("the server that offers compute takes the turn: {took:?}");
    };
    assert_eq!(
        device,
        hex::encode(tower.device_pubkey()),
        "it ran on the server"
    );
    assert_eq!(output, here, "the same output, wherever it ran");
    for resource in &output.resources {
        let hash = tilo_actions::content_hash(resource).unwrap();
        let mut digest = [0; 32];
        hex::decode_to_slice(hash, &mut digest).unwrap();
        assert_eq!(
            content.bytes_of(digest),
            app_content.read(hash),
            "byte for byte the phone's thumbnail, kept where the phone reads it"
        );
    }

    // A server that does not keep the inputs does not take the turn: nothing
    // moves to run the action.
    hosted
        .host
        .workspaces()
        .run_jobs(Arc::new(Media::default()));
    let Offloaded::RunHere { reason } =
        placed(&route, &request, invocation(&workspace, &named)).await
    else {
        panic!("a server without the inputs takes no turn");
    };
    assert!(reason.contains("does not keep every input"), "{reason}");

    // A device that is not connected here is not asked, so nothing is waited
    // for.
    let unconnected = turn_offload(Arc::new(content.clone()), Arc::new(takers(false)))
        .expect("offload is not turned off");
    let Offloaded::RunHere { reason } =
        placed(&unconnected, &request, invocation(&workspace, &named)).await
    else {
        panic!("nobody was asked");
    };
    assert_eq!(reason, "no other device of the workspace is reachable");
}

/// The offloadable action this test drives, kept beside the test that runs it: a batch of
/// thumbnails, declaring what it needs of a device, answered byte for byte on the device and on a
/// server that dispatches the thumbnail engine's own action. It is the shape an app declares;
/// nothing here imports an app.
mod fixture {
    use tilo_resource::Bytes;

    #[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
    #[effect(operation = "test.offloadable", catalogue = "tests/locales")]
    pub enum TestEffect {
        /// Make thumbnails: a small PNG of each picture, in order, the same bytes whichever of
        /// the person's devices makes them.
        #[effect(
            content,
            idempotent,
            kind = "read",
            offload(cpu_ms = 120000, memory_mb = 1024, output_mb = 16)
        )]
        MakeThumbnails {
            #[param(content, "The pictures' content, in order")]
            photos: Vec<Bytes>,
        },
    }

    fn content_hex(hash: &[u8; 32]) -> String {
        hash.iter()
            .fold(String::with_capacity(64), |mut hex, byte| {
                let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
                hex
            })
    }

    fn failure(photo: &str, reason: &str) -> serde_json::Value {
        serde_json::json!({"photo": photo, "reason": reason})
    }

    pub fn make_thumbnails(
        photos: &[Bytes],
        store: &tilo_content::ContentStore,
    ) -> Result<tilo_actions::ActionOutput, String> {
        use std::collections::BTreeSet;
        let mut distinct = BTreeSet::new();
        let mut named: Vec<(String, &Bytes)> = Vec::with_capacity(photos.len());
        for photo in photos {
            let hash = photo.hash();
            if distinct.insert(hash) {
                named.push((content_hex(&hash), photo));
            }
        }
        let mut inputs = Vec::with_capacity(named.len());
        let mut failed = Vec::new();
        for (photo, content) in &named {
            match content.read(store) {
                Some(bytes) => inputs.push((photo.as_str(), bytes)),
                None => failed.push(failure(photo, "is not on this device")),
            }
        }
        let distinct: Vec<Vec<u8>> = inputs.iter().map(|(_, bytes)| bytes.clone()).collect();
        let mut resources = Vec::with_capacity(inputs.len());
        for ((photo, _), outcome) in inputs
            .iter()
            .zip(thumbnail::batch::thumbnail_batch(&distinct))
        {
            match outcome {
                Ok(thumbnail) => {
                    let hash = store.put(&thumbnail).map_err(|error| {
                        tracing::warn!(%error, "a thumbnail could not be stored");
                        "a thumbnail could not be stored".to_string()
                    })?;
                    resources.push(tilo_actions::content_resource(&hash).ok_or_else(|| {
                        "the content provider returned an invalid address".to_string()
                    })?);
                }
                Err(error) => {
                    tracing::warn!(%photo, %error, "a picture made no thumbnail");
                    failed.push(failure(photo, "is not an image"));
                }
            }
        }
        Ok(tilo_actions::ActionOutput {
            value: (!failed.is_empty()).then(|| serde_json::json!({ "failed": failed })),
            resources,
        })
    }
}

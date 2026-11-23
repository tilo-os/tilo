use super::*;

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
use tilo_actions::{ActionAddress, Offload};
use tilo_lock::MutexRecover as _;

/// The workspace the fixtures invoke in.
const WORKSPACE: &str = "3f2a1b7c-4d8e-4f60-9a51-2b3c4d5e6f70";
/// The app whose content the invocation reads.
const APP: &str = "so.tilo.photos";
/// The device that takes the turn.
const SERVER: [u8; 32] = [7; 32];

/// The workspace's app content on this device.
#[derive(Default)]
struct Media {
    content: Mutex<HashMap<String, Vec<u8>>>,
}

impl WorkspaceMedia for Media {
    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        assert_eq!((scope.workspace(), scope.app()), (WORKSPACE, APP));
        let kept = *blake3::hash(&bytes).as_bytes();
        self.content.lock_recover().insert(hex::encode(kept), bytes);
        Box::pin(async move { Ok(kept) })
    }
}

/// The workspace's other devices: what they answer, and what they were asked.
struct Devices {
    answer: Mutex<Option<Result<Taken, String>>>,
    asked: Mutex<Vec<(String, Job, InvocationId)>>,
}

impl Devices {
    fn answering(answer: Result<Taken, String>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Some(answer)),
            asked: Mutex::default(),
        })
    }
}

impl Takers for Devices {
    fn take(
        &self,
        workspace: &str,
        job: Job,
        invocation: InvocationId,
    ) -> Pending<Result<Taken, String>> {
        self.asked
            .lock_recover()
            .push((workspace.to_owned(), job, invocation));
        let answer = self.answer.lock_recover().take();
        // None: given up on, the completer is gone.
        answer.map_or_else(|| pending().1, Pending::ready)
    }
}

/// A thumbnail of the photo `input`, named twice, as the app asks for it.
fn request(input: &str) -> OffloadRequest {
    OffloadRequest {
        workspace: WORKSPACE.to_owned(),
        app_id: APP.to_owned(),
        address: ActionAddress::new("photos.library", "make-thumbnails"),
        offload: Offload {
            app: "so.tilo.thumbnails".to_owned(),
            entrypoint: "thumbnail-batch".to_owned(),
            gpu: false,
            cpu_time_ms: 5_000,
            memory_bytes: 512 * 1024 * 1024,
            output_bytes: 64 * 1024 * 1024,
        },
        inputs: vec![input.to_owned(), input.to_owned()],
    }
}

/// The invocation a person made of `request`.
fn invoked(request: &OffloadRequest) -> InvokeRequest {
    tilo_actions::remote::InvokeRequest {
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
        signed: None,
    }
}

/// What a device that took the turn answers: one thumbnail.
fn ran(thumbnail: &[u8]) -> Taken {
    Taken {
        device: SERVER,
        output: ActionOutput {
            value: None,
            resources: vec![
                tilo_actions::content_resource(&hex::encode(blake3::hash(thumbnail).as_bytes()))
                    .unwrap(),
            ],
        },
        outputs: vec![thumbnail.to_vec()],
    }
}

fn route(media: &Arc<Media>, devices: &Arc<Devices>) -> Arc<dyn ActionOffload> {
    turn_offload(
        Arc::clone(media) as Arc<dyn WorkspaceMedia>,
        Arc::clone(devices) as Arc<dyn Takers>,
    )
    .expect("offload is on")
}

/// The turn another device took runs there: the devices are asked for the
/// person's own invocation, with each input once, and the thumbnail it made is
/// kept here where the app reads it.
#[tokio::test]
async fn a_turn_another_device_takes_runs_there_and_its_output_is_kept_here() {
    let media = Arc::new(Media::default());
    let devices = Devices::answering(Ok(ran(b"thumbnail")));
    let request = request(&"ab".repeat(32));
    let invocation = invoked(&request);

    let placed = route(&media, &devices)
        .offload_invocation(request.clone(), invocation.clone())
        .await;

    let Offloaded::Ran { output, device, .. } = placed else {
        panic!("the turn ran on the device that took it: {placed:?}");
    };
    assert_eq!(device, hex::encode(SERVER));
    assert_eq!(output, ran(b"thumbnail").output);
    assert_eq!(
        media
            .content
            .lock_recover()
            .get(&hex::encode(blake3::hash(b"thumbnail").as_bytes())),
        Some(&b"thumbnail".to_vec()),
        "the output is kept where the app reads it"
    );
    let asked = devices.asked.lock_recover().clone();
    assert_eq!(asked.len(), 1);
    let (workspace, job, id) = &asked[0];
    assert_eq!(
        (workspace.as_str(), *id),
        (WORKSPACE, invocation.invocation)
    );
    assert_eq!(
        (job.app.as_str(), job.resource.as_str(), job.action.as_str()),
        (APP, "photos.library", "make-thumbnails")
    );
    assert_eq!(job.inputs, vec![[0xab; 32]], "each input is named once");
}

/// Whenever no device runs the turn to its end, it runs here, and the route
/// says why: none took it, the asks were given up on, or what came back is not
/// the content it names.
#[tokio::test]
async fn a_turn_no_device_runs_runs_here() {
    let input = "ab".repeat(32);
    let mut forged = ran(b"thumbnail");
    forged.outputs = vec![b"something else".to_vec()];
    for (answer, why) in [
        (
            Some(Err("this device offers no compute".to_owned())),
            "offers no compute",
        ),
        (None, "given up on"),
        (Some(Ok(forged)), "not the content it names"),
    ] {
        let media = Arc::new(Media::default());
        let devices = Arc::new(Devices {
            answer: Mutex::new(answer),
            asked: Mutex::default(),
        });
        let request = request(&input);
        let placed = route(&media, &devices)
            .offload_invocation(request.clone(), invoked(&request))
            .await;
        assert!(
            matches!(&placed, Offloaded::RunHere { reason } if reason.contains(why)),
            "{why}: {placed:?}"
        );
    }

    let media = Arc::new(Media::default());
    let devices = Devices::answering(Ok(ran(b"thumbnail")));
    let placed = route(&media, &devices)
        .offload(request("not a content hash"))
        .await;
    assert!(
        matches!(&placed, Offloaded::RunHere { reason } if reason.contains("not a content hash")),
        "{placed:?}"
    );
    assert!(devices.asked.lock_recover().is_empty(), "nothing is asked");
}

/// The receipt of a device that took the turn.
fn took() -> Pending<Result<InvocationReceipt, ActionCallError>> {
    Pending::ready(Ok(InvocationReceipt {
        invocation: InvocationId::new(),
        status: ReceiptStatus::Accepted,
    }))
}

/// Why a device did not take the turn.
fn declined(reason: &str) -> Pending<Result<InvocationReceipt, ActionCallError>> {
    Pending::ready(Err(ActionCallError::Refused {
        reason: reason.to_owned(),
    }))
}

/// The device whose claim held is followed, whatever the others answer; when
/// every device answers why not, the turn is no one's; one that does not
/// answer is waited for only until the deadline.
#[tokio::test(start_paused = true)]
async fn the_device_that_claims_the_turn_is_followed() {
    let (_held, silent) = pending::<Result<InvocationReceipt, ActionCallError>>();
    assert_eq!(
        first_to_take(
            vec![
                ("tower", declined("the turn is taken by 07070707")),
                ("phone", declined("this device offers no compute")),
                ("server", took()),
                ("laptop", silent),
            ],
            CLAIM_DEADLINE,
        )
        .await,
        Ok("server")
    );

    assert_eq!(
        first_to_take(
            vec![
                ("tower", declined("this device does not keep every input")),
                ("phone", declined("this device offers no compute")),
            ],
            CLAIM_DEADLINE,
        )
        .await,
        Err("this device does not keep every input; this device offers no compute".to_owned())
    );

    let (_held, silent) = pending::<Result<InvocationReceipt, ActionCallError>>();
    let started = tokio::time::Instant::now();
    let unanswered = first_to_take(
        vec![
            ("tower", declined("this device runs no jobs")),
            ("laptop", silent),
        ],
        CLAIM_DEADLINE,
    )
    .await;
    assert_eq!(
        unanswered,
        Err("no device took it within 2000 ms".to_owned())
    );
    assert_eq!(started.elapsed(), CLAIM_DEADLINE);
}

#[test]
fn offload_turns_off_by_its_variable() {
    for off in ["off", "0", "false", "No"] {
        assert!(disabled(Some(off)), "{off}");
    }
    for on in [None, Some("on"), Some("")] {
        assert!(!disabled(on), "{on:?}");
    }
}

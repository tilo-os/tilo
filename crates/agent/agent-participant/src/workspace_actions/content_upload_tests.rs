//! A file put by a page through its session, as the host answers it: the session's upload driver
//! against the host's own open uploads and a workspace store.
#![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

use std::cell::{Cell, RefCell};

use tilo_actions::remote::{
    ActionHostError, Caller, ContentPutAnswer, ContentPutStep, HostedContentPuts,
    MAX_CONTENT_FRAME_BYTES, Pending,
};
use tilo_content::ContentStore;
use tilo_session_core::remote::upload::{self, PutSteps, UploadError};

use super::StorePuts;

/// The host's side of one session: its open uploads over a workspace's store, the steps it was
/// asked, and the one append the way loses.
struct Host {
    puts: HostedContentPuts,
    store: ContentStore,
    caller: Caller,
    steps: RefCell<Vec<ContentPutStep>>,
    /// The append at this offset never reaches the host, once: its answer is that the workspace did
    /// not answer.
    lose_at: Cell<Option<u64>>,
}

impl Host {
    fn new() -> Self {
        Self {
            puts: HostedContentPuts::default(),
            store: ContentStore::memory(),
            caller: Caller::local_person("ada"),
            steps: RefCell::default(),
            lose_at: Cell::new(None),
        }
    }

    fn appends(&self) -> Vec<(u64, usize)> {
        self.steps
            .borrow()
            .iter()
            .filter_map(|step| match step {
                ContentPutStep::Append { offset, bytes, .. } => Some((*offset, bytes.len() / 2)),
                _ => None,
            })
            .collect()
    }
}

impl PutSteps for Host {
    fn put_step(&self, step: ContentPutStep) -> Pending<Result<ContentPutAnswer, ActionHostError>> {
        self.steps.borrow_mut().push(step.clone());
        if let ContentPutStep::Append { offset, .. } = &step
            && self.lose_at.get() == Some(*offset)
        {
            self.lose_at.set(None);
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "the device did not answer in time".into(),
            }));
        }
        Pending::ready(
            self.puts
                .step(&StorePuts(self.store.clone()), &self.caller, step),
        )
    }
}

/// A PDF of `size` bytes, its bytes past the header all different enough to tell pieces apart.
fn pdf(size: usize) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut state = 7_u32;
    while bytes.len() < size {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        bytes.push(state.to_be_bytes()[1]);
    }
    bytes
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

/// The bytes go up in pieces a frame carries, the person is told how far it got, and the
/// reference the host answers is the one the workspace's store gives the whole file.
#[test]
fn a_file_goes_up_in_pieces_and_is_named_as_the_store_names_it() {
    let host = Host::new();
    let bytes = pdf(3 * MAX_CONTENT_FRAME_BYTES + 1_000);
    let mut told = Vec::new();
    let content = run(upload::put(
        &host,
        bytes.as_slice(),
        &mut |at| told.push(at),
        &|| false,
    ))
    .unwrap();
    let whole = ContentStore::memory().put(&bytes).unwrap();
    assert_eq!(
        Some(content.clone()),
        tilo_actions::content_resource(&whole)
    );
    assert_eq!(
        host.store
            .read(tilo_actions::content_hash(&content).unwrap()),
        Some(bytes.clone()),
        "the workspace holds the file"
    );
    let appends = host.appends();
    assert_eq!(appends.len(), 4, "{appends:?}");
    assert!(
        appends
            .iter()
            .all(|(_, length)| *length <= MAX_CONTENT_FRAME_BYTES)
    );
    assert_eq!(told.last().copied(), Some(bytes.len() as u64));
    assert!(told.windows(2).all(|pair| pair[0] < pair[1]), "{told:?}");
    // What the bytes are lands the file: a PDF in PDF Viewer, whatever it is called.
    let landing =
        upload::landing_request("ws", &bytes[..64], "docs/2026/scan.txt", &content).unwrap();
    assert_eq!(landing.target.address.resource, "app.so.tilo.pdf-viewer");
}

/// A piece the way lost stops the ones sent behind it; the upload asks the host where it stands
/// and goes on from there, and the file arrives whole and once.
#[test]
fn a_lost_piece_resumes_where_the_host_stands() {
    let host = Host::new();
    let piece = MAX_CONTENT_FRAME_BYTES as u64;
    host.lose_at.set(Some(piece));
    let bytes = pdf(5 * MAX_CONTENT_FRAME_BYTES + 17);
    let content = run(upload::put(&host, bytes.as_slice(), &mut |_| {}, &|| false)).unwrap();
    assert_eq!(
        host.store
            .read(tilo_actions::content_hash(&content).unwrap()),
        Some(bytes),
        "the file is whole"
    );
    let standing = host
        .steps
        .borrow()
        .iter()
        .filter(|step| matches!(step, ContentPutStep::Standing { .. }))
        .count();
    assert_eq!(standing, 1, "the upload asked where it stands once");
    let offsets: Vec<u64> = host
        .appends()
        .into_iter()
        .map(|(offset, _)| offset)
        .collect();
    assert_eq!(
        offsets,
        vec![
            0,
            piece,
            2 * piece,
            3 * piece,
            4 * piece,
            piece,
            2 * piece,
            3 * piece,
            4 * piece,
            5 * piece
        ],
        "the lost piece and the ones behind it are sent again from where the host stood"
    );
}

/// A person who stops the upload stops it between pieces, and nothing lands.
#[test]
fn a_stopped_upload_stores_nothing() {
    let host = Host::new();
    let bytes = pdf(2 * MAX_CONTENT_FRAME_BYTES);
    let asked = Cell::new(0_u32);
    let stopped = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    assert_eq!(
        run(upload::put(&host, bytes.as_slice(), &mut |_| {}, &stopped)),
        Err(UploadError::Stopped)
    );
    assert!(
        !host
            .steps
            .borrow()
            .iter()
            .any(|step| matches!(step, ContentPutStep::Finish { .. }))
    );
}

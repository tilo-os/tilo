//! The service's threads stop with it: a request they are serving when it
//! stops finishes on a runtime that still runs, and the stop returns only
//! once no thread is left to outlive it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{Caller, RemoteActionMessage};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::{HostDevice, Shared, WorkspaceActions, WorkspaceDocumentsLink};
use crate::apps::AppCatalogue;
use crate::service::ServerEndpoint;

const WORKSPACE: &str = "5f0a3d1e-8c4b-4f2a-9d6e-1b7c0a2f4e51";
const PERSON: [u8; 32] = [7; 32];
const DEVICE: [u8; 32] = [8; 32];

/// A host whose content store for a workspace comes only once the service
/// stops taking requests: a request works there while the service is
/// stopped, as one mid-work is, and goes on to arm a timer on the runtime
/// the service works on.
struct HeldStore {
    shared: Weak<Shared>,
    device: HostDevice,
    /// Signalled once a request waits here.
    running: mpsc::Sender<()>,
    /// Signalled once the request finished its work.
    finished: mpsc::Sender<()>,
    /// Whether the request got to its work again after waiting, on the
    /// runtime: a timer arms there, and a runtime that already stopped
    /// refuses one.
    worked_on: AtomicBool,
}

impl WorkspaceDocumentsLink for HeldStore {
    fn user_public_key(&self) -> [u8; 32] {
        PERSON
    }

    fn own_device(&self) -> HostDevice {
        self.device.clone()
    }

    fn workspace_content(&self, _workspace: WorkspaceId) -> Option<tilo_content::ContentStore> {
        let _ = self.running.send(());
        // The service holds no queue once it stopped taking requests: the
        // request behind this one finds none either. So this request is what
        // the stop has in hand when it returns.
        while self
            .shared
            .upgrade()
            .is_some_and(|shared| shared.jobs.lock_recover().is_some())
        {
            std::thread::yield_now();
        }
        self.device
            .runtime
            .block_on(async { tokio::time::sleep(Duration::from_millis(1)).await });
        self.worked_on.store(true, Ordering::SeqCst);
        let _ = self.finished.send(());
        None
    }
}

/// The service's host certificate for `key`, as a device of `PERSON`.
fn device_cert(key: &SigningKey) -> DeviceCert {
    DeviceCert {
        user_public_key: PERSON,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "the service's host".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    }
}

/// The threads serving requests, as the process names them.
#[cfg(target_os = "linux")]
fn serving_threads() -> usize {
    let tasks = std::fs::read_dir("/proc/self/task").expect("this process's threads");
    tasks
        .filter_map(std::result::Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.starts_with("workspace-actions-"))
        })
        .count()
}

#[cfg(not(target_os = "linux"))]
const fn serving_threads() -> usize {
    0
}

#[test]
fn a_stop_ends_the_threads_after_the_request_in_flight_and_before_the_runtime_does() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (running, started) = mpsc::channel();
    let (finished, job_finished) = mpsc::channel();
    let key = SigningKey::from_bytes(&[9; 32]);
    let actions = WorkspaceActions::new(Arc::new(|| AppCatalogue::new(Vec::new())));
    let link = Arc::new(HeldStore {
        shared: Arc::downgrade(&actions.shared),
        device: HostDevice {
            endpoint: ServerEndpoint::iroh(
                iroh_base::EndpointAddr::new(
                    iroh_base::EndpointId::from_bytes(&key.verifying_key().to_bytes()).unwrap(),
                ),
                device_cert(&key),
            ),
            key: key.clone(),
            runtime: runtime.handle().clone(),
            keys: None,
        },
        running,
        finished,
        worked_on: AtomicBool::new(false),
    });
    actions.attach(link.clone()).unwrap();
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    let caller = Caller::authenticated(PERSON, DEVICE);
    let asked = actions.serve(
        workspace,
        &caller,
        RemoteActionMessage::ContentGetRequest {
            correlation: 1,
            content: "tilo.content:5f0a3d1e8c4b4f2a".into(),
            offset: 0,
            length: 0,
        },
    );
    assert!(asked.is_some(), "the request was taken");
    started
        .recv()
        .expect("a request reached the store it waits on");

    // The stop runs on a thread of its own, and says what stood the moment
    // it returned: whether the request in flight had finished, and whether
    // any thread of the service was still there.
    let stopped = std::thread::spawn(move || {
        actions.shutdown();
        (job_finished.try_recv().is_ok(), serving_threads())
    })
    .join()
    .unwrap();

    assert!(
        stopped.0,
        "the stop returned with a request still running: its thread outlives it, and arms a \
         timer on a runtime that is stopping"
    );
    assert_eq!(stopped.1, 0, "no thread of the service outlives the stop");
    assert!(
        link.worked_on.load(Ordering::SeqCst),
        "the request finished on the runtime the service works on"
    );
    // Every thread ended before this, so the shutdown has none to find.
    drop(runtime);
}

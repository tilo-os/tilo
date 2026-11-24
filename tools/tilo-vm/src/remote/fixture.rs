//! A device in this process for tests: a real action service on a loopback endpoint, serving its
//! steps from a closure and its content store from memory, so tilo-vm is tested through the same
//! frames, signatures and admission a device's daemon answers it with.
#![allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test fixture: a failed step is the test failing"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentPut,
    ContentPuts, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_actions::{ActionFailure, ActionOutput, InvocationId};
use tilo_device_protocol::RpcError;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use crate::config::DeviceRoute;
use crate::remote::Machine;

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

/// What a step answers: its output, or the failure the device gives it.
pub type Answer = Result<Value, RpcError>;

type Steps = dyn Fn(&str, &Value) -> Answer + Send + Sync;

/// One step the device ran, as it was asked.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub action: String,
    pub resource: String,
    pub argument: Value,
    /// The device key the request was signed with.
    pub by: [u8; 32],
}

#[derive(Clone)]
struct Provider {
    status: Value,
    steps: Arc<Steps>,
    asked: Arc<Mutex<Vec<Asked>>>,
    content: Arc<Store>,
}

#[derive(Default)]
pub struct Store(Mutex<BTreeMap<String, Vec<u8>>>);

impl Store {
    /// The blob stored under `hash`, if the device holds it.
    pub fn get(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.lock().unwrap().get(hash).cloned()
    }

    /// How many blobs the device holds.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

struct Upload(Arc<Store>, Vec<u8>);

impl ContentPut for Upload {
    fn size(&self) -> u64 {
        self.1.capacity() as u64
    }

    fn written(&self) -> u64 {
        self.1.len() as u64
    }

    fn append(&mut self, offset: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        if offset != self.written() {
            return Err(ActionCallError::InvalidRequest {
                reason: format!("an append at {offset} after {} bytes", self.written()),
            });
        }
        self.1.extend_from_slice(chunk);
        Ok(self.written())
    }

    fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
        let hash = blake3::hash(&self.1).to_hex().to_string();
        self.0.0.lock().unwrap().insert(hash.clone(), self.1);
        Ok(format!("tilo.content:{hash}"))
    }
}

struct Puts(Arc<Store>);

impl ContentPuts for Puts {
    fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        let size = usize::try_from(size).unwrap();
        Ok(Box::new(Upload(
            Arc::clone(&self.0),
            Vec::with_capacity(size),
        )))
    }

    fn have(&self, _: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        let held = self.0.0.lock().unwrap();
        Ok(hashes.iter().map(|hash| held.contains_key(hash)).collect())
    }
}

impl ActionProvider for Provider {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let InvokeArgument::Json(argument) = request.argument else {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "a step takes an object".into(),
            }));
        };
        let argument = Value::Object(argument);
        let action = request.target.address.action.clone();
        if action == "status" {
            return Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(self.status.clone()),
                        resources: vec![],
                    },
                },
            }));
        }
        self.asked.lock().unwrap().push(Asked {
            action: action.clone(),
            resource: request.target.address.resource.clone(),
            argument: argument.clone(),
            by: *caller.device_public_key().unwrap(),
        });
        let status = match (self.steps)(&action, &argument) {
            Ok(value) => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(value),
                    resources: vec![],
                },
            },
            Err(error) => ReceiptStatus::Failed {
                error: ActionFailure {
                    code: error.code.to_string(),
                    message: error.message.clone(),
                    details: Some(serde_json::to_value(&error).unwrap()),
                    retryable: false,
                },
            },
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(Puts(Arc::clone(&self.content))))
    }
}

/// A device of one person, and a machine of the same person that drives it.
pub struct Device {
    pub machine: Machine,
    pub route: DeviceRoute,
    asked: Arc<Mutex<Vec<Asked>>>,
    pub content: Arc<Store>,
    router: iroh::protocol::Router,
}

impl Device {
    /// A device of this machine's architecture with no accounts, answering each step other than
    /// `status` with `steps`.
    pub fn serving(steps: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static) -> Self {
        Self::of_arch(std::env::consts::ARCH, steps)
    }

    /// [`Self::serving`] on a device of architecture `arch`.
    pub fn of_arch(
        arch: &str,
        steps: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static,
    ) -> Self {
        Self::with_status(
            serde_json::json!({
                "daemon_version": "0.0.0-test",
                "arch": arch,
                "tools": { "cargo": true, "python3": true, "sccache": false, "podman": true },
                "accounts": [],
                "seat0": null,
                "storage": { "layers_bytes": 0, "targets_bytes": 0, "free_bytes": null, "total_bytes": null },
                "issues": [],
            }),
            steps,
        )
    }

    /// [`Self::serving`], answering `status` with `status`.
    pub fn with_status(
        status: Value,
        steps: impl Fn(&str, &Value) -> Answer + Send + Sync + 'static,
    ) -> Self {
        let root = SigningKey::from_bytes(&[1; 32]);
        let (device, device_cert) = certified(&root, 2);
        let (machine, machine_cert) = certified(&root, 3);
        let roster = [device_cert.clone(), machine_cert];
        let now = tilo_clock::wall();
        let mut peer = StatePeer::new(
            WORKSPACE,
            device_cert.clone(),
            roster.to_vec(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &device_cert, &roster).unwrap(),
                documents: Vec::new(),
            },
            &device,
            now,
        )
        .unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let content = Arc::new(Store::default());
        let provider = Provider {
            status,
            steps: Arc::new(steps),
            asked: Arc::clone(&asked),
            content: Arc::clone(&content),
        };
        let runtime = tilo_rt::app().handle();
        let _entered = runtime.enter();
        let endpoint = runtime.block_on(tilo_peer_state::loopback::endpoint(&device));
        let service = ActionService::new(device.clone(), Arc::new(provider));
        service.hold(Replica::new(peer));
        let accepting = iroh::protocol::Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, service)
            .spawn();
        let addr = tilo_peer_state::loopback::addr(&endpoint);
        Self {
            machine: Machine {
                key: machine,
                user: root.verifying_key().to_bytes(),
                workspace: WORKSPACE.into(),
            },
            route: DeviceRoute {
                device: hex::encode(device.verifying_key().to_bytes()),
                workspace: WORKSPACE.into(),
                addresses: addr.ip_addrs().map(ToString::to_string).collect(),
                relay: None,
            },
            asked,
            content,
            router: accepting,
        }
    }

    /// Every step the device ran, in order.
    pub fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap().clone()
    }

    /// The device key of the machine that drives it.
    pub fn machine_key(&self) -> [u8; 32] {
        self.machine.key.verifying_key().to_bytes()
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let router = self.router.clone();
        drop(tilo_rt::app().handle().block_on(router.shutdown()));
    }
}

fn certified(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

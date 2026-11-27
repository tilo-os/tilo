//! A program that drives another of the person's devices (`tilo-vm`) never dials as this device:
//! it asks this computer's daemon to carry its connection there, and the daemon asks the other
//! device as its own device, from the one endpoint it keeps under the device key. Steps, their
//! answers and content puts all go that way; a connection the daemon answers with the device's
//! own actions alone is told why it is carried nowhere.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_action_line::{Carry, LineClient};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ContentPut,
    ContentPuts, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use crate::language::host;

const WORKSPACE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

/// The other device's steps: `status` answers what it is, and every request is kept with the
/// device key it came from.
#[derive(Clone, Default)]
struct Steps {
    asked: Arc<Mutex<Vec<(String, [u8; 32])>>>,
    stored: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl ActionProvider for Steps {
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
        let address = &request.target.address;
        self.asked.lock().unwrap().push((
            format!("{}#{}", address.resource, address.action),
            *caller.device_public_key().unwrap(),
        ));
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(json!({ "arch": "riscv64" })),
                    resources: vec![],
                },
            },
        }))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn content_puts(&self) -> Option<Arc<dyn ContentPuts>> {
        Some(Arc::new(self.clone()))
    }
}

struct Upload(Steps, Vec<u8>);

impl ContentPut for Upload {
    fn size(&self) -> u64 {
        self.1.capacity() as u64
    }

    fn written(&self) -> u64 {
        self.1.len() as u64
    }

    fn append(&mut self, _: u64, chunk: &[u8]) -> Result<u64, ActionCallError> {
        self.1.extend_from_slice(chunk);
        Ok(self.written())
    }

    fn finish(self: Box<Self>) -> Result<String, ActionCallError> {
        let hash = blake3::hash(&self.1).to_hex().to_string();
        self.0.stored.lock().unwrap().insert(hash.clone(), self.1);
        Ok(format!("tilo.content:{hash}"))
    }
}

impl ContentPuts for Steps {
    fn begin(&self, _: &Caller, size: u64) -> Result<Box<dyn ContentPut>, ActionCallError> {
        Ok(Box::new(Upload(
            self.clone(),
            Vec::with_capacity(usize::try_from(size).unwrap()),
        )))
    }

    fn have(&self, _: &Caller, hashes: &[String]) -> Result<Vec<bool>, ActionCallError> {
        let stored = self.stored.lock().unwrap();
        Ok(hashes
            .iter()
            .map(|hash| stored.contains_key(hash))
            .collect())
    }
}

/// A step on `device`'s `vm` resource, as tilo-vm asks it.
fn step(device: &str, action: &str) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "so.tilo.devices".into(),
            address: ActionAddress::new(format!("devices/{device}/vm"), action),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

#[test]
fn the_daemon_carries_a_programs_steps_to_another_device_as_its_own() {
    host::in_own_process(|profile| {
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let paths = tilo_identity::provisioning::collaboration_identity_paths();
        let this = tilo_device_identity::persist::read_owner_cert(&paths)
            .unwrap()
            .unwrap();
        let root = tilo_identity::store::load_from_path(
            tilo_runtime::device::services::device_root_record().unwrap(),
        )
        .unwrap();

        // Another device of the same person, in a workspace both are in, serving its steps on
        // an endpoint of its own.
        let key = SigningKey::from_bytes(&[7; 32]);
        let now = tilo_clock::wall();
        let mut other = DeviceCert {
            user_public_key: this.user_public_key,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "build box".into(),
            capabilities: DeviceCapabilities::member(),
            // Issued a moment before the peer's clock reading, so it is valid at it.
            issued_at_ns: tilo_clock::unix_now_ns() - 1_000_000_000,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut other, &root).unwrap();
        let roster = [other.clone(), this.clone()];
        let mut peer = StatePeer::new(
            WORKSPACE,
            other.clone(),
            roster.to_vec(),
            RevocationList::new(),
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        peer.install_seed(
            SeedState {
                workspace_changes: seed_workspace(WORKSPACE, &other, &roster).unwrap(),
                documents: Vec::new(),
            },
            &key,
            now,
        )
        .unwrap();
        let steps = Steps::default();
        // The other device's endpoint runs on a runtime of the test's own, for the test's run.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("the other device's runtime");
        let entered = runtime.enter();
        let endpoint = runtime.block_on(tilo_peer_state::loopback::endpoint(&key));
        let service = ActionService::new(key.clone(), Arc::new(steps.clone()));
        service.hold(Replica::new(peer));
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, service)
            .spawn();
        drop(entered);
        let device = hex::encode(other.device_public_key);
        let carry = Carry {
            device: device.clone(),
            workspace: WORKSPACE.into(),
            addresses: tilo_peer_state::loopback::addr(&endpoint)
                .ip_addrs()
                .map(ToString::to_string)
                .collect(),
            relay: None,
        };

        // The native endpoint takes the person's connection once their services run.
        let client = LineClient::carried(running.native_socket(), &carry)
            .expect("the person's connection is carried");
        let caller = Caller::local_system().in_workspace(WORKSPACE);
        let receipt = client
            .invoke(&caller, step(&device, "status"))
            .wait_timeout(std::time::Duration::from_secs(30))
            .ok()
            .flatten()
            .expect("the other device answers")
            .unwrap();
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("the step completes: {:?}", receipt.status);
        };
        assert_eq!(output.value, Some(json!({ "arch": "riscv64" })));
        assert_eq!(
            *steps.asked.lock().unwrap(),
            [(
                format!("devices/{device}/vm#status"),
                this.device_public_key
            )],
            "asked as this computer's device, by its daemon"
        );

        // Bytes go into the other device's store through the same connection.
        let bytes: Vec<u8> = (0..200_000_u32)
            .map(|n| u8::try_from(n % 256).expect("a value below 256 is a byte"))
            .collect();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        assert_eq!(
            client.have(&caller, std::slice::from_ref(&hash)).unwrap(),
            [false]
        );
        let mut upload = client.begin(&caller, bytes.len() as u64).unwrap();
        upload.append(0, &bytes).unwrap();
        assert!(upload.finish().unwrap().ends_with(&hash));
        assert_eq!(steps.stored.lock().unwrap().get(&hash), Some(&bytes));
        assert_eq!(client.have(&caller, &[hash]).unwrap(), [true]);

        // Its own device is asked at the daemon, never carried.
        let own = Carry {
            device: hex::encode(this.device_public_key),
            ..carry
        };
        let refused = LineClient::carried(running.native_socket(), &own)
            .err()
            .expect("a carry to this device is refused");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            refused.to_string().contains("this computer's own device"),
            "{refused}"
        );

        drop(client);
        drop(runtime.block_on(router.shutdown()));
        running.stop().unwrap();
    });
}

/// A connection the daemon answers with the device's own actions alone (here, on a device no
/// person has claimed) is told why it is carried nowhere, and nothing is dialled.
#[test]
fn a_connection_the_device_alone_answers_is_carried_nowhere() {
    host::in_own_unclaimed_process(|profile| {
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let carry = Carry {
            device: "07".repeat(32),
            workspace: WORKSPACE.into(),
            addresses: vec!["127.0.0.1:9".into()],
            relay: None,
        };
        let refused = LineClient::carried(running.native_socket(), &carry)
            .err()
            .expect("the device's own actions carry nothing");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            refused.to_string().contains("carried to no other device"),
            "{refused}"
        );
        running.stop().unwrap();
    });
}

//! A person's devices in one process, for the tests of the fabric and of the services built on it:
//! each device's providers are mounted on one in-memory link ([`Link`]), whose calls are the same
//! action calls the device link carries, made with the caller the link would prove. The person's
//! store is one map every device reads ([`SharedFacts`]); the session on each device is a recorder
//! ([`RecordingSeat`]), and so are its magic packets.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test support: a fixture that cannot be built fails the calling test"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tilo_actions::remote::{ActionCallError, ActionProvider, Caller, InvokeRequest, Pending};
use tilo_lock::MutexRecover as _;
use tilo_seal::SecretKey;
use tilo_user_identity::{CertifiedFleet, DeviceCapabilities, DeviceCert, sign_device_cert};

use crate::actions::{completed_value, invoke_argument};
use crate::calls::{DeviceCall, Facts, Invoker, SeatCalls, cant_now, device_target};
use crate::interfaces::WakeInterfaces;
use crate::unlock_keys::{Ownership, PersonKeys};
use crate::wake::MagicPackets;
use crate::{DeviceAbout, DeviceId, Fabric, Ipv4Subnet, PROVIDER, Parts, UNLOCK_SCOPE, WakeRoute};

type Mounted = BTreeMap<String, Arc<dyn ActionProvider + Send + Sync>>;

/// The device link between a person's devices: the providers each device mounts, and which
/// devices it reaches now.
#[derive(Default)]
pub struct Link {
    person: [u8; 32],
    mounted: Mutex<BTreeMap<DeviceId, Mounted>>,
    uncertified: Mutex<BTreeMap<DeviceId, Mounted>>,
    down: Mutex<BTreeSet<DeviceId>>,
}

impl Link {
    /// Mount `provider` as `name` on `device`.
    pub fn mount(
        &self,
        device: &DeviceId,
        name: &str,
        provider: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.mounted
            .lock_recover()
            .entry(device.clone())
            .or_default()
            .insert(name.to_owned(), provider);
    }

    /// Mount `provider` as the scope `name` an uncertified key may call on `device`.
    pub fn mount_uncertified(
        &self,
        device: &DeviceId,
        name: &str,
        provider: Arc<dyn ActionProvider + Send + Sync>,
    ) {
        self.uncertified
            .lock_recover()
            .entry(device.clone())
            .or_default()
            .insert(name.to_owned(), provider);
    }

    /// `device` drops off the link (asleep, or out of reach), or comes back.
    pub fn set_down(&self, device: &DeviceId, down: bool) {
        let mut set = self.down.lock_recover();
        if down {
            set.insert(device.clone());
        } else {
            set.remove(device);
        }
    }

    /// Run `call` on the device it names, as `caller`: what the device link does with a call it
    /// carries.
    #[must_use]
    pub fn invoke(
        &self,
        caller: &Caller,
        call: DeviceCall,
        uncertified: bool,
    ) -> Pending<Result<Value, ActionCallError>> {
        if self.down.lock_recover().contains(&call.device) {
            return Pending::ready(Err(ActionCallError::Unavailable {
                reason: "the device is not reachable".to_owned(),
            }));
        }
        let table = if uncertified {
            &self.uncertified
        } else {
            &self.mounted
        };
        let provider = table
            .lock_recover()
            .get(&call.device)
            .and_then(|mounted| mounted.get(&call.target.provider))
            .cloned();
        let Some(provider) = provider else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let mut request = InvokeRequest::plain(call.target, serde_json::Map::new());
        request.argument = invoke_argument(call.input);
        provider
            .invoke(caller, request)
            .map(|receipt| receipt.and_then(completed_value))
    }
}

/// One device's calls on the link: made with the certificate it holds of the person's.
pub struct LinkInvoker {
    link: Arc<Link>,
    key: [u8; 32],
}

impl Invoker for LinkInvoker {
    fn call(&self, call: DeviceCall) -> Pending<Result<Value, ActionCallError>> {
        self.link.invoke(
            &Caller::authenticated(self.link.person, self.key),
            call,
            false,
        )
    }
}

/// The person's store as every device of theirs reads it: each device's facts by name.
#[derive(Default)]
pub struct SharedFacts {
    facts: Mutex<BTreeMap<(String, DeviceId), Vec<u8>>>,
}

/// One device's facts in the person's store.
pub struct DeviceFacts {
    store: Arc<SharedFacts>,
    device: DeviceId,
}

impl Facts for DeviceFacts {
    fn keep(&self, name: &str, value: Vec<u8>) -> Result<(), String> {
        self.store
            .facts
            .lock_recover()
            .insert((name.to_owned(), self.device.clone()), value);
        Ok(())
    }

    fn read(&self, name: &str) -> Vec<(DeviceId, Vec<u8>)> {
        self.store
            .facts
            .lock_recover()
            .iter()
            .filter(|((kept, _), _)| kept == name)
            .map(|((_, device), value)| (device.clone(), value.clone()))
            .collect()
    }
}

/// The session on a device, as a recorder: every call it got, answered with what the test set for
/// it, or refused like a device with no session once detached.
#[derive(Default)]
pub struct RecordingSeat {
    calls: Mutex<Vec<(String, String, Value)>>,
    answers: Mutex<BTreeMap<String, Value>>,
    detached: Mutex<bool>,
}

impl RecordingSeat {
    /// The calls it got, as `resource#action` with their input.
    #[must_use]
    pub fn calls(&self) -> Vec<(String, Value)> {
        self.calls
            .lock_recover()
            .iter()
            .map(|(resource, action, input)| (format!("{resource}#{action}"), input.clone()))
            .collect()
    }

    /// The inputs of the calls of `resource#action`.
    #[must_use]
    pub fn inputs(&self, address: &str) -> Vec<Value> {
        self.calls()
            .into_iter()
            .filter(|(called, _)| called == address)
            .map(|(_, input)| input)
            .collect()
    }

    /// Answer `resource#action` with `value` from now on.
    pub fn answer(&self, address: &str, value: Value) {
        self.answers
            .lock_recover()
            .insert(address.to_owned(), value);
    }

    /// No session is attached any more (or one is again).
    pub fn detach(&self, detached: bool) {
        *self.detached.lock_recover() = detached;
    }
}

impl SeatCalls for RecordingSeat {
    fn call(
        &self,
        resource: &str,
        action: &str,
        input: Value,
    ) -> Pending<Result<Value, ActionCallError>> {
        if *self.detached.lock_recover() {
            return Pending::ready(Err(cant_now()));
        }
        self.calls
            .lock_recover()
            .push((resource.to_owned(), action.to_owned(), input));
        let answer = self
            .answers
            .lock_recover()
            .get(&format!("{resource}#{action}"))
            .cloned()
            .unwrap_or(Value::Null);
        Pending::ready(Ok(answer))
    }
}

/// The magic packets a device sent, instead of a broadcast.
#[derive(Default)]
pub struct RecordingPackets {
    sent: Mutex<Vec<([u8; 6], Ipv4Subnet)>>,
}

impl RecordingPackets {
    #[must_use]
    pub fn sent(&self) -> Vec<([u8; 6], Ipv4Subnet)> {
        self.sent.lock_recover().clone()
    }
}

impl MagicPackets for RecordingPackets {
    fn send(&self, mac: [u8; 6], subnet: Ipv4Subnet) -> std::io::Result<()> {
        self.sent.lock_recover().push((mac, subnet));
        Ok(())
    }
}

/// A machine's network interfaces as the test sets them.
#[derive(Default)]
pub struct FixedInterfaces(Mutex<Vec<WakeRoute>>);

impl FixedInterfaces {
    pub fn set(&self, routes: Vec<WakeRoute>) {
        *self.0.lock_recover() = routes;
    }
}

impl WakeInterfaces for FixedInterfaces {
    fn routes(&self) -> Vec<WakeRoute> {
        self.0.lock_recover().clone()
    }
}

/// The person's identity: their root, the fleet it certified, and their secret-sync key.
pub struct Identity {
    pub root: SigningKey,
    pub fleet: Mutex<CertifiedFleet>,
    /// The person's devices, as their fleet lists them.
    pub devices: Mutex<Vec<[u8; 32]>>,
    pub secret_sync: SecretKey,
}

impl Identity {
    fn new(root: SigningKey) -> Self {
        let user = root.verifying_key().to_bytes();
        Self {
            root,
            fleet: Mutex::new(CertifiedFleet::new(user)),
            devices: Mutex::default(),
            secret_sync: SecretKey::from_bytes([12; 32]),
        }
    }

    /// The root's certificate for `key`, named `label`.
    #[must_use]
    pub fn certificate(
        &self,
        key: [u8; 32],
        label: &str,
        capabilities: DeviceCapabilities,
    ) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: key,
            device_label: label.to_owned(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &self.root).expect("the root signs a certificate");
        cert
    }
}

/// One device's view of the person's identity: whether it holds their root.
pub struct DeviceKeys {
    identity: Arc<Identity>,
    holds_root: bool,
}

impl PersonKeys for DeviceKeys {
    fn ownership(&self) -> Option<Ownership> {
        Some(Ownership {
            fleet: self.identity.fleet.lock_recover().clone(),
            key_holder: self.holds_root,
        })
    }

    fn secret_sync(&self) -> Option<SecretKey> {
        self.holds_root.then(|| self.identity.secret_sync.clone())
    }

    fn sign_root(&self, digest: &[u8; 32]) -> Result<[u8; 64], String> {
        use ed25519_dalek::Signer as _;
        if !self.holds_root {
            return Err("this device holds no root".to_owned());
        }
        Ok(self.identity.root.sign(digest).to_bytes())
    }

    fn person_devices(&self, _now_ns: u64) -> Vec<[u8; 32]> {
        self.identity.devices.lock_recover().clone()
    }
}

/// A person and their devices on one link.
pub struct Person {
    pub identity: Arc<Identity>,
    pub link: Arc<Link>,
    pub facts: Arc<SharedFacts>,
}

impl Default for Person {
    fn default() -> Self {
        Self::new()
    }
}

/// One of the person's devices: its fabric, its session, and what it sent.
pub struct TestDevice {
    pub key: SigningKey,
    pub id: DeviceId,
    pub label: String,
    pub fabric: Fabric,
    pub seat: Arc<RecordingSeat>,
    pub packets: Arc<RecordingPackets>,
    pub interfaces: Arc<FixedInterfaces>,
    pub facts: Arc<DeviceFacts>,
    pub link: Arc<Link>,
    person: [u8; 32],
}

impl Person {
    #[must_use]
    pub fn new() -> Self {
        let identity = Arc::new(Identity::new(SigningKey::from_bytes(&[1; 32])));
        let link = Arc::new(Link {
            person: identity.root.verifying_key().to_bytes(),
            ..Link::default()
        });
        Self {
            identity,
            link,
            facts: Arc::default(),
        }
    }

    /// The person's root key.
    #[must_use]
    pub fn key(&self) -> [u8; 32] {
        self.identity.root.verifying_key().to_bytes()
    }

    /// A new device of the person's, certified by their root, its fabric mounted on the link.
    /// `seed` tells its keys apart; `holds_root` says whether the person's root is on it.
    #[must_use]
    pub fn device(&self, seed: u8, label: &str, holds_root: bool) -> TestDevice {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let public = key.verifying_key().to_bytes();
        let id = DeviceId::of_key(&public);
        self.identity
            .fleet
            .lock_recover()
            .certify(
                self.identity
                    .certificate(public, label, DeviceCapabilities::member()),
            )
            .expect("the fleet takes the person's device");
        self.identity.devices.lock_recover().push(public);
        let seat = Arc::new(RecordingSeat::default());
        let packets = Arc::new(RecordingPackets::default());
        let interfaces = Arc::new(FixedInterfaces::default());
        let facts = Arc::new(DeviceFacts {
            store: self.facts.clone(),
            device: id.clone(),
        });
        let fabric = Fabric::new(Parts {
            key: public,
            person: self.key(),
            about: DeviceAbout::named(label),
            invoker: Arc::new(LinkInvoker {
                link: self.link.clone(),
                key: public,
            }),
            seat: seat.clone(),
            facts: facts.clone(),
            keys: Arc::new(DeviceKeys {
                identity: self.identity.clone(),
                holds_root,
            }),
            interfaces: interfaces.clone(),
            packets: packets.clone(),
        });
        self.link.mount(&id, PROVIDER, fabric.provider());
        // The unlock scope answers the person's certified devices too: one without the root asks
        // through it to enrol its boot key.
        let scope = fabric.unlock_scope();
        self.link.mount(&id, UNLOCK_SCOPE, scope.clone());
        self.link.mount_uncertified(&id, UNLOCK_SCOPE, scope);
        TestDevice {
            key,
            id,
            label: label.to_owned(),
            fabric,
            seat,
            packets,
            interfaces,
            facts,
            link: self.link.clone(),
            person: self.key(),
        }
    }

    /// Every pair of `devices` reaches each other: the link tells both sides.
    pub fn connect(devices: &[&TestDevice]) {
        for (index, one) in devices.iter().enumerate() {
            for other in devices.iter().skip(index.saturating_add(1)) {
                one.link.set_down(&other.id, false);
                other.link.set_down(&one.id, false);
                one.fabric.arrived(&other.id, &other.label);
                other.fabric.arrived(&one.id, &one.label);
            }
        }
    }
}

impl TestDevice {
    /// This device goes to sleep: the link no longer reaches it, and `others` see it leave.
    pub fn sleep(&self, others: &[&TestDevice]) {
        self.link.set_down(&self.id, true);
        for other in others {
            other.fabric.left(&self.id);
            self.fabric.left(&other.id);
        }
    }

    /// Run `provider`'s `resource#action` on this device as the person here.
    ///
    /// # Errors
    /// The action's refusal.
    pub fn here<O: DeserializeOwned>(
        &self,
        provider: &str,
        resource: &str,
        action: &str,
        input: impl Serialize,
    ) -> Result<O, ActionCallError> {
        let call = DeviceCall::new(&self.id, provider, resource, action, input);
        let value = answer(self.link.invoke(
            &Caller::authenticated(self.person, self.id_key()),
            call,
            false,
        ))?;
        serde_json::from_value(value).map_err(|error| ActionCallError::InvalidRequest {
            reason: error.to_string(),
        })
    }

    /// Run `provider`'s `resource#action` on `device` as this device, over the link.
    ///
    /// # Errors
    /// The action's refusal.
    pub fn ask<O: DeserializeOwned>(
        &self,
        device: &DeviceId,
        provider: &str,
        resource: &str,
        action: &str,
        input: impl Serialize,
    ) -> Result<O, ActionCallError> {
        let call = DeviceCall::new(device, provider, resource, action, input);
        let value = answer(self.link.invoke(
            &Caller::authenticated(self.person, self.id_key()),
            call,
            false,
        ))?;
        serde_json::from_value(value).map_err(|error| ActionCallError::InvalidRequest {
            reason: error.to_string(),
        })
    }

    fn id_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Read `provider`'s `resource` on this device as the person here.
    ///
    /// # Panics
    /// When it does not read.
    #[must_use]
    pub fn read<O: DeserializeOwned>(&self, provider: &str, resource: &str) -> O {
        self.here(provider, resource, crate::actions::READ, Value::Null)
            .unwrap_or_else(|error| panic!("{provider} {resource} reads: {error}"))
    }
}

/// Run `provider`'s `resource#action` on `device` as the key `key` alone, which holds no
/// certificate: what a booting device does.
///
/// # Errors
/// The action's refusal.
pub fn uncertified<O: DeserializeOwned>(
    link: &Link,
    key: [u8; 32],
    device: &DeviceId,
    provider: &str,
    resource: &str,
    action: &str,
    input: impl Serialize,
) -> Result<O, ActionCallError> {
    let call = DeviceCall {
        device: device.clone(),
        target: device_target(device, provider, resource, action),
        input: serde_json::to_value(input).unwrap_or(Value::Null),
    };
    let value = answer(link.invoke(&Caller::uncertified_device(key), call, true))?;
    serde_json::from_value(value).map_err(|error| ActionCallError::InvalidRequest {
        reason: error.to_string(),
    })
}

/// The answer of a call on the in-memory link, which answers at once.
fn answer(pending: Pending<Result<Value, ActionCallError>>) -> Result<Value, ActionCallError> {
    pending
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("a call on the in-memory link answers at once")
}

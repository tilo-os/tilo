//! The person's private messages: the texts exchanged with each person, and
//! the action that sends one.
//!
//! A private text travels between two people's devices on each session's own
//! collaboration endpoint (`tilo_network_actor::private_messages`): the
//! sender proves a certificate of the person it writes as, the receiver
//! answers with one of the person the text was addressed to, and a text
//! arrives only when both sides addressed the other's root. Nothing of it
//! goes through a room, a record or a relay: the two devices talk.
//!
//! What was exchanged is published as the `messages` resource — who this
//! device is for private texts, and the conversation with each person, the
//! texts sent and received — and sending is the resource's `send` action, so
//! a panel, an app and a script read the same conversations and write through
//! the same action.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_lock::MutexRecover as _;
use tilo_messaging::SendError;
use tilo_network_actor::private_messages;
use tilo_resource::Effect;
use tilo_service::{LoopWake, Service, Started, Wake, decode, offer, reading};

/// The resource the conversations are published as.
pub const MESSAGES: &str = "messages";
/// Its kind.
pub const MESSAGES_KIND: &str = "so.tilo.messages.private";

/// How often the person's certificate is read again while the loop turns: a
/// device revoked or relinked since the session started stops receiving.
const MATERIAL_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// Longest private text this device sends, as the wire bounds it.
const MAX_TEXT: usize = 16 * 1024;

/// The `messages` resource's actions: one way to say something to a person.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.messages.private")]
pub enum MessagesEffect {
    /// Send a private text to one of a person's devices.
    #[effect(
        label = "Send a private message",
        icon = "send",
        keywords = "message, private, send, schreiben"
    )]
    Send {
        #[param("The device it goes to, by its session's address")]
        peer: String,
        #[param("The person it is for, by their public key")]
        person: String,
        #[param("What to say")]
        text: String,
    },
}

/// One text of a conversation, as the resource shows it.
#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub id: String,
    pub at: u64,
    pub body: String,
    /// Whether this device sent it, rather than received it.
    pub mine: bool,
}

/// A conversation with one person, as the resource shows it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Conversation {
    /// The person, by their root public key.
    pub person: String,
    /// The texts, oldest first.
    pub texts: Vec<Line>,
}

/// What the resource reads: who this device is for private texts, and every
/// conversation this device keeps.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Reading {
    /// This device's address for private texts; empty until the endpoint is
    /// bound.
    pub address: String,
    /// The person this device writes as, by their root public key.
    pub person: String,
    /// Where this device is reached, as a contact dials it; empty until the
    /// endpoint is bound.
    pub at: serde_json::Value,
    pub conversations: Vec<Conversation>,
}

/// What the loop and the endpoint share.
#[derive(Default)]
struct Shared {
    transport: Option<private_messages::Transport>,
    /// The account the endpoint answers with while this device carries texts:
    /// the registry holds it weakly, so it lives here.
    account: Option<Arc<dyn private_messages::Account>>,
    /// This device's certificate and the revocations it knows: what a peer is
    /// answered with, and what a peer's certificate is checked against.
    material: Option<private_messages::Material>,
    address: String,
    conversations: BTreeMap<String, Vec<Line>>,
    /// The person's custody this device writes as, read again while the loop
    /// turns so a relinked or revoked device stops writing as the old one.
    identity: Option<tilo_identity_host::custody::HostIdentity>,
    refreshed: Option<std::time::Instant>,
    at: serde_json::Value,
    /// Moves whenever the reading a person sees changed.
    version: u64,
    /// The version the loop last took in.
    polled: u64,
}

impl Shared {
    fn reading(&self) -> Reading {
        Reading {
            address: self.address.clone(),
            person: self
                .material
                .as_ref()
                .map(|material| hex::encode(material.cert.user_public_key))
                .unwrap_or_default(),
            at: self.at.clone(),
            conversations: self
                .conversations
                .iter()
                .map(|(person, texts)| Conversation {
                    person: person.clone(),
                    texts: texts.clone(),
                })
                .collect(),
        }
    }

    fn keep(&mut self, person: String, line: Line) {
        self.conversations.entry(person).or_default().push(line);
        self.version = self.version.wrapping_add(1);
    }
}

/// The person's private messages on this session.
#[derive(Clone)]
pub struct Messages {
    shared: Arc<Mutex<Shared>>,
    wake: Wake,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for Messages {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Messages")
    }
}

/// The endpoint's side of a private text: it answers a peer with this
/// device's certificate, with the person's revocations checked against it,
/// and keeps a text that arrives.
struct Private {
    shared: Arc<Mutex<Shared>>,
    wake: Wake,
}

impl Private {
    /// Whether `certificate` is a device the person still lets write: its
    /// certificate has not run out and nothing the person revoked names it.
    fn acceptable(certificate: &private_messages::Material) -> bool {
        let now = tilo_clock::unix_now_ms();
        let cert = &certificate.cert;
        if cert.expires_at_ns.is_some_and(|end| end / 1_000_000 <= now) {
            return false;
        }
        let device = cert.device_public_key;
        !certificate.revocations.iter().any(|revocation| {
            revocation.user_public_key == cert.user_public_key
                && revocation.revoked_device_public_key == device
        })
    }
}

impl private_messages::Account for Private {
    fn certificate(&self) -> Result<private_messages::Material, SendError> {
        self.shared
            .lock_recover()
            .material
            .clone()
            .ok_or(SendError::Unavailable)
    }

    fn check_peer(&self, certificate: &private_messages::Material) -> Result<(), SendError> {
        if Self::acceptable(certificate) {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }

    fn receive<'a>(
        &'a self,
        certificate: &'a private_messages::Material,
        text: &'a private_messages::Text,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SendError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.shared.lock_recover().keep(
                hex::encode(certificate.cert.user_public_key),
                Line {
                    id: text.id.to_string(),
                    at: text.timestamp_ms,
                    body: text.body.clone(),
                    mine: false,
                },
            );
            (self.wake)();
            Ok(())
        })
    }
}

impl Messages {
    /// The person's private messages, published on the session's door.
    ///
    /// # Errors
    /// The runtime a send runs on cannot be made.
    pub fn new(wake: &LoopWake) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("tilo-messages")
            .enable_all()
            .build()?;
        Ok(Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            wake: wake.handle(),
            runtime: Arc::new(runtime),
        })
    }

    /// Wake the session's loop: what a text that landed asks of it.
    pub fn wake(&self) {
        (self.wake)();
    }

    /// This device's certificate as `material`, kept current by the loop
    /// ([`Self::see`]).
    pub fn see(&self, material: private_messages::Material, address: String) {
        let mut shared = self.shared.lock_recover();
        shared.material = Some(material);
        shared.address = address;
        shared.version = shared.version.wrapping_add(1);
    }

    /// Publish where this device is reached, as a contact dials it.
    pub fn reached_at(&self, at: serde_json::Value) {
        self.shared.lock_recover().at = at;
    }

    /// Write as `identity` from now on, with `address` naming this device's
    /// endpoint.
    pub fn act_as(&self, identity: tilo_identity_host::custody::HostIdentity, address: String) {
        if let Ok(material) = Self::read_material(&identity) {
            self.see(material, address);
        }
        self.shared.lock_recover().identity = Some(identity);
    }

    /// Read this device's certificate from `identity`, as the person it
    /// writes as: the device the session's endpoint runs on, with the
    /// revocations the person's other devices announced.
    ///
    /// # Errors
    /// The person's custody cannot be read now.
    pub fn read_material(
        identity: &tilo_identity_host::custody::HostIdentity,
    ) -> Result<private_messages::Material, String> {
        let (cert, revocations) = identity.current_person_material()?;
        Ok(private_messages::Material { cert, revocations })
    }

    /// Answer peers on `transport`, showing the certificate this device
    /// keeps now.
    ///
    /// # Errors
    /// The transport refuses a second account, or this device has no
    /// certificate yet.
    pub fn bind(&self, transport: private_messages::Transport) -> Result<(), SendError> {
        if self.shared.lock_recover().account.is_some() {
            return Ok(());
        }
        let account: Arc<dyn private_messages::Account> = Arc::new(Private {
            shared: Arc::clone(&self.shared),
            wake: Arc::clone(&self.wake),
        });
        transport.bind(&account)?;
        let mut shared = self.shared.lock_recover();
        shared.account = Some(account);
        shared.transport = Some(transport);
        drop(shared);
        Ok(())
    }

    /// Send `text` to the device at `peer`, for the person `person`: what the
    /// resource's action does, and what the messaging service's own sends go
    /// through.
    ///
    /// # Errors
    /// The addresses are not private addresses, the text is not one this
    /// device sends, or no endpoint carries it.
    pub fn send(&self, peer: &str, person: &str, text: &str) -> Result<(), DoorError> {
        let refused = |reason: &str| DoorError::InvalidArguments(reason.to_owned());
        let unavailable = |reason: &str| DoorError::Unavailable {
            code: "no-private-endpoint".into(),
            reason: reason.to_owned(),
        };
        if text.trim().is_empty() || text.len() > MAX_TEXT {
            return Err(refused(
                "a private message is a line, not empty and not long",
            ));
        }
        let mut peer_bytes = [0; 32];
        hex::decode_to_slice(peer, &mut peer_bytes).map_err(|_| refused("not a device address"))?;
        let peer = iroh::EndpointId::from_bytes(&peer_bytes)
            .map_err(|_| refused("not a device address"))?;
        let mut root = [0; 32];
        hex::decode_to_slice(person, &mut root).map_err(|_| refused("not a person's key"))?;
        let transport = self
            .shared
            .lock_recover()
            .transport
            .clone()
            .ok_or_else(|| unavailable("this device carries no private texts yet"))?;
        let shared = Arc::clone(&self.shared);
        let wake = Arc::clone(&self.wake);
        let line = Line {
            id: uuid::Uuid::new_v4().to_string(),
            at: tilo_clock::unix_now_ms(),
            body: text.to_owned(),
            mine: true,
        };
        let text = private_messages::Text {
            id: uuid::Uuid::parse_str(&line.id).map_err(|_| refused("no id"))?,
            timestamp_ms: line.at,
            body: line.body.clone(),
        };
        let person = hex::encode(root);
        self.runtime.spawn(async move {
            // Only a text the other person's device took is kept: a send
            // whose outcome is unknown is not shown as sent.
            let sent = transport.send(peer, root, text).await;
            if sent.is_ok() {
                shared.lock_recover().keep(person, line);
                (wake)();
            }
        });
        Ok(())
    }
}

impl Service for Messages {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<MessagesEffect>(ResourceSpec::new(
            MESSAGES,
            MESSAGES_KIND,
            "Private messages",
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            MESSAGES => Some(reading(&self.shared.lock_recover().reading())),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        if resource != MESSAGES {
            return Err(DoorError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        let published = self.resources();
        let effect = decode::<MessagesEffect>(&published, resource, action, arguments)?;
        match effect {
            MessagesEffect::Send { peer, person, text } => {
                self.send(&peer, &person, &text)?;
                // The line shows in the conversation once the other person's
                // device has taken it.
                Ok(Started::Accepted)
            }
        }
    }

    fn poll(&mut self) -> bool {
        let (mut changed, identity) = {
            let mut shared = self.shared.lock_recover();
            let changed = shared.version != shared.polled;
            shared.polled = shared.version;
            // The certificate a peer is answered with is read again now and
            // then: a device the person revoked since the session started stops
            // writing as itself, and a relinked one starts.
            let due = shared
                .refreshed
                .is_none_or(|at| at.elapsed() >= MATERIAL_EVERY);
            let identity = if due {
                shared.refreshed = Some(std::time::Instant::now());
                shared.identity.clone()
            } else {
                None
            };
            (changed, identity)
        };
        if let Some(identity) = identity {
            match Self::read_material(&identity) {
                Ok(material) => {
                    let mut shared = self.shared.lock_recover();
                    let relinked = shared.material.as_ref().is_some_and(|kept| {
                        kept.cert.device_public_key != material.cert.device_public_key
                    });
                    let first = shared.material.is_none();
                    shared.material = Some(material);
                    if relinked || first {
                        shared.version = shared.version.wrapping_add(1);
                        changed = true;
                    }
                    drop(shared);
                }
                Err(error) => {
                    tracing::warn!(%error, "the person's certificate for private texts is not read");
                }
            }
        }
        changed
    }
}

//! The person's messaging accounts run in `tilo-messaging`
//! (crate `tilo-messaging-service`), which the session starts here.
//!
//! The service enrolls and runs Signal, `WhatsApp`, SMS and Tilo accounts and
//! serves Messages over its own socket. It asks the session, over its
//! standard input and output (the door), only for what the session alone
//! holds: the person's accounts and contacts records, the protected network
//! prompt, and the device's endpoint for private Tilo texts. The session
//! tells it whether the person's session is usable. Dropping
//! [`MessagingService`] closes the door, which stops the service.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use accounts::person::PersonAccounts;
use tilo_messaging_service::door::{
    self, Call, DoorLink, Event, Material, Publication, PublicationKind, Reply, Text,
};
use tilo_messaging_service::link::{Handler, Responder};
use tilo_network_actor::private_messages;

type DoorResponder = Responder<Call, Reply, Event>;

/// How long the service has to answer the endpoint about a private text.
const PRIVATE: Duration = Duration::from_secs(10);

/// What the door handler shares with the session's loop.
#[derive(Default)]
struct Shared {
    /// Calls only the session's loop can answer: the person's records.
    records: Mutex<Option<mpsc::Sender<(Call, DoorResponder)>>>,
    prompts: Mutex<Option<crate::portal::PromptQueueHandle>>,
    transport: Mutex<Option<private_messages::Transport>>,
    peers: Mutex<Option<crate::network::PeerIdentityStore>>,
    /// The account the endpoint delivers private texts to; the endpoint
    /// keeps only a weak reference.
    private: Mutex<Option<Arc<dyn private_messages::Account>>>,
    wake: Mutex<Option<crate::session_runtime::Wake>>,
    /// The person's own workspace, which the network prompt names.
    workspace: u64,
}

impl Shared {
    fn wake(&self) {
        let wake = self
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

struct Door {
    shared: Arc<Shared>,
    link: std::sync::OnceLock<DoorLink>,
}

impl Handler<Call, Reply, Event> for Door {
    fn call(&self, call: Call, reply: DoorResponder) {
        match call {
            Call::Person | Call::Publish(_) => {
                let records = self
                    .shared
                    .records
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                match records {
                    Some(records) => {
                        if records.send((call, reply)).is_ok() {
                            self.shared.wake();
                        }
                    }
                    None => reply.send(Reply::Unavailable),
                }
            }
            Call::Consent { key, hosts } => self.consent(key, hosts, reply),
            Call::ConsentWithdrawn { key } => {
                if let Some(prompts) = self.prompts() {
                    prompts.cancel_enqueued_request(&key);
                }
                reply.send(Reply::Done);
            }
            Call::PrivateBind => reply.send(self.bind()),
            Call::PrivateSend { peer, root, text } => self.send(peer, root, text, reply),
            _ => reply.send(Reply::Refused),
        }
    }

    fn event(&self, _: Event) {}
}

impl Door {
    fn prompts(&self) -> Option<crate::portal::PromptQueueHandle> {
        self.shared
            .prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Ask the person, once, to let Messages reach `hosts`; never an Always
    /// grant.
    fn consent(&self, key: String, hosts: Vec<String>, reply: DoorResponder) {
        let (Some(prompts), Ok(runtime)) = (self.prompts(), crate::session_runtime::runtime())
        else {
            reply.send(Reply::Unavailable);
            return;
        };
        let answer = prompts.enqueue_for_request(
            crate::portal::PortalCaller::new(
                crate::shell::state::WorkspaceId(self.shared.workspace),
                tilo_messaging_service::MESSAGES_APP,
            ),
            crate::shell::state::Capability::Network {
                hosts: Some(hosts),
                scope: crate::shell::state::GRANT_ONCE,
            },
            key,
        );
        runtime.spawn(async move {
            reply.send(match answer.await {
                Ok(
                    crate::portal::PromptOutcome::AllowOnce
                    | crate::portal::PromptOutcome::AllowSession,
                ) => Reply::Done,
                _ => Reply::Refused,
            });
        });
    }

    fn bind(&self) -> Reply {
        let transport = self
            .shared
            .transport
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let peers = self
            .shared
            .peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (Some(transport), Some(peers), Some(link)) =
            (transport, peers, self.link.get().cloned())
        else {
            return Reply::Unavailable;
        };
        let account: Arc<dyn private_messages::Account> = Arc::new(PrivateAccount { link, peers });
        match transport.bind(&account) {
            Ok(()) => {
                *self
                    .shared
                    .private
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(account);
                Reply::Done
            }
            Err(_) => Reply::Refused,
        }
    }

    fn send(&self, peer: [u8; 32], root: [u8; 32], text: Text, reply: DoorResponder) {
        let transport = self
            .shared
            .transport
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (Some(transport), Ok(runtime)) = (transport, crate::session_runtime::runtime()) else {
            reply.send(Reply::Unavailable);
            return;
        };
        let (Ok(peer), Ok(id)) = (
            iroh::EndpointId::from_bytes(&peer),
            uuid::Uuid::parse_str(&text.id),
        ) else {
            reply.send(Reply::Refused);
            return;
        };
        runtime.spawn(async move {
            let sent = transport
                .send(
                    peer,
                    root,
                    private_messages::Text {
                        id,
                        timestamp_ms: text.timestamp_ms,
                        body: text.body.to_string(),
                    },
                )
                .await;
            reply.send(match sent {
                Ok(()) => Reply::Done,
                Err(tilo_messaging::SendError::Revoked) => Reply::Refused,
                Err(_) => Reply::Unavailable,
            });
        });
    }
}

/// The service's Tilo account, as the device's endpoint sees it. The
/// endpoint's own record of revoked devices is checked here; whether the
/// person's contacts route a peer to the account is the service's answer.
struct PrivateAccount {
    link: DoorLink,
    peers: crate::network::PeerIdentityStore,
}

fn material(material: &private_messages::Material) -> Material {
    Material {
        cert: material.cert.clone(),
        revocations: material.revocations.clone(),
    }
}

impl PrivateAccount {
    fn check_revocations(
        &self,
        material: &private_messages::Material,
    ) -> Result<(), tilo_messaging::SendError> {
        let certificate = &material.cert;
        let mut revocations = tilo_user_identity::RevocationList::new();
        for entry in &material.revocations {
            if entry.user_public_key != certificate.user_public_key {
                return Err(tilo_messaging::SendError::Revoked);
            }
            revocations
                .insert(entry.clone())
                .map_err(|_| tilo_messaging::SendError::Revoked)?;
        }
        self.peers
            .set_revocations(certificate.user_public_key, &revocations);
        if self.peers.is_revoked(
            &certificate.user_public_key,
            &certificate.device_public_key,
            tilo_clock::unix_now_ns(),
        ) {
            return Err(tilo_messaging::SendError::Revoked);
        }
        Ok(())
    }
}

impl private_messages::Account for PrivateAccount {
    fn certificate(&self) -> Result<private_messages::Material, tilo_messaging::SendError> {
        match self.link.call_blocking(Call::PrivateCertificate, PRIVATE) {
            Some(Reply::Material(material)) => Ok(private_messages::Material {
                cert: material.cert,
                revocations: material.revocations,
            }),
            _ => Err(tilo_messaging::SendError::Unavailable),
        }
    }

    fn check_peer(
        &self,
        peer: &private_messages::Material,
    ) -> Result<(), tilo_messaging::SendError> {
        self.check_revocations(peer)?;
        match self
            .link
            .call_blocking(Call::PrivateCheckPeer(material(peer)), PRIVATE)
        {
            Some(Reply::Done) => Ok(()),
            _ => Err(tilo_messaging::SendError::Revoked),
        }
    }

    fn receive<'a>(
        &'a self,
        peer: &'a private_messages::Material,
        text: &'a private_messages::Text,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), tilo_messaging::SendError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.check_revocations(peer)?;
            let answer = self.link.call(Call::PrivateReceive(
                material(peer),
                Text {
                    id: text.id.to_string(),
                    timestamp_ms: text.timestamp_ms,
                    body: zeroize::Zeroizing::new(text.body.clone()),
                },
            ));
            match tokio::time::timeout(PRIVATE, answer).await {
                Ok(Ok(Reply::Done)) => Ok(()),
                Ok(Ok(Reply::Refused)) => Err(tilo_messaging::SendError::Revoked),
                _ => Err(tilo_messaging::SendError::Unavailable),
            }
        })
    }
}

/// The session's handle on the running service, as the in-process apps
/// hold it.
///
/// They hold it as a trait object so that what runs the service (its door,
/// its process and the person's admitted session) stays out of the types
/// of the chrome state that holds them: the admitted session reaches the
/// session broker's state, down to the permits its queued requests hold,
/// and every borrow of the chrome state would otherwise count as holding
/// those permits.
pub(crate) trait Service {
    /// Ask the person through `prompts` before Messages reaches a host.
    fn bind_prompts(&self, prompts: crate::portal::PromptQueueHandle);
    /// Carry private Tilo texts on `network`'s endpoint.
    fn bind_network(&mut self, network: &crate::shell::network::ShellNetwork);
    /// On the session's loop: follow the person's session, take the
    /// endpoint once it is ready and answer what asks for their records.
    fn pump(&mut self, person: &crate::shell::settings_records::Holder);
}

/// Start the service for `author` on SDK device `device`, keeping its
/// accounts under `profile`. `socket` is the session broker whose admission
/// of the person the service follows. `None` when the service is not
/// installed here or did not start.
pub(crate) fn start(
    author: &str,
    device: &str,
    profile: &std::path::Path,
    socket: PathBuf,
    wake: crate::session_runtime::Wake,
) -> Option<Box<dyn Service>> {
    MessagingService::start(author, device, profile, socket, wake)
        .map(|service| Box::new(service) as Box<dyn Service>)
}

/// The running service and the session's side of its door.
struct MessagingService {
    child: Child,
    link: DoorLink,
    shared: Arc<Shared>,
    records: mpsc::Receiver<(Call, DoorResponder)>,
    network: Option<tokio::sync::oneshot::Receiver<private_messages::Transport>>,
    /// The person's admitted session the service's state follows.
    session: Option<Arc<crate::shell::collab_broker::NativePersonalSession>>,
    generation: u64,
    socket: PathBuf,
    author: String,
}

impl MessagingService {
    fn start(
        author: &str,
        device: &str,
        profile: &std::path::Path,
        socket: PathBuf,
        wake: crate::session_runtime::Wake,
    ) -> Option<Self> {
        let helper = helper()?;
        let mut person = [0; 32];
        hex::decode_to_slice(author, &mut person).ok()?;
        let room = collab_types::person_room(&person).to_string();
        let workspace = tilo_relay_protocol::room_workspace_id(&room).ok()?;
        let mut child = Command::new(&helper)
            .args([door::PRINCIPAL_ARG, author, door::DEVICE_ARG, device])
            .arg(door::PROFILE_ARG)
            .arg(profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .inspect_err(|error| {
                tracing::warn!(target: "tilo::messaging", %error, helper = %helper.display(), "the messaging service did not start");
            })
            .ok()?;
        let (stdin, stdout) = (child.stdin.take()?, child.stdout.take()?);
        let (records, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            records: Mutex::new(Some(records)),
            wake: Mutex::new(Some(wake)),
            workspace,
            ..Shared::default()
        });
        let door = Arc::new(Door {
            shared: shared.clone(),
            link: std::sync::OnceLock::new(),
        });
        let link = DoorLink::start("tilo-messaging-door", stdout, stdin, door.clone()).ok()?;
        let _ = door.link.set(link.clone());
        Some(Self {
            child,
            link,
            shared,
            records: receiver,
            network: None,
            session: None,
            generation: 0,
            socket,
            author: author.into(),
        })
    }
}

impl Service for MessagingService {
    fn bind_prompts(&self, prompts: crate::portal::PromptQueueHandle) {
        *self
            .shared
            .prompts
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(prompts);
    }

    fn bind_network(&mut self, network: &crate::shell::network::ShellNetwork) {
        if self.network.is_some()
            || self
                .shared
                .transport
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
        {
            return;
        }
        *self
            .shared
            .peers
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(network.private_message_identities());
        let (reply, receiver) = tokio::sync::oneshot::channel();
        if network
            .cmd_tx
            .try_send(tilo_network_actor::NetCmd::PrivateMessages(reply))
            .is_ok()
        {
            self.network = Some(receiver);
        }
    }

    fn pump(&mut self, person: &crate::shell::settings_records::Holder) {
        if let Some(receiver) = self.network.as_mut() {
            match receiver.try_recv() {
                Ok(transport) => {
                    *self
                        .shared
                        .transport
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(transport);
                    self.network = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => self.network = None,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        self.follow_session();
        for (call, reply) in self.records.try_iter().take(32) {
            reply.send(answer(person, call, self.session.is_some()));
        }
    }
}

impl MessagingService {
    fn follow_session(&mut self) {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.check().is_err())
        {
            self.session = None;
            self.link.event(Event::Session {
                active: false,
                generation: self.generation,
            });
        }
        if self.session.is_none()
            && let Ok(session) = crate::shell::collab_broker::NativePersonalSource::admit_for_socket(
                &self.socket,
                &self.author,
            )
        {
            self.session = Some(session);
            self.generation = self.generation.wrapping_add(1);
            self.link.event(Event::Session {
                active: true,
                generation: self.generation,
            });
        }
    }
}

/// Answer a call about the person's records, on the loop that holds them.
fn answer(person: &crate::shell::settings_records::Holder, call: Call, admitted: bool) -> Reply {
    match call {
        Call::Person => {
            let accounts = person.read::<PersonAccounts, _>(PersonAccounts::store);
            let directory = person
                .read::<tilo_contact_directory::ContactDirectory, _>(tilo_resource::Model::to_json);
            match (accounts, directory) {
                (Ok(accounts), Ok(directory)) => Reply::Person {
                    accounts,
                    directory,
                },
                _ => Reply::Unavailable,
            }
        }
        Call::Publish(publication) => {
            if !admitted {
                return Reply::Refused;
            }
            let Publication {
                account,
                name,
                generation,
                kind,
            } = publication;
            let invoke =
                match kind {
                    PublicationKind::Signal { aci } => PersonAccounts::PUBLISH_SIGNAL_ACCOUNT
                        .with((account, name, aci, generation)),
                    PublicationKind::Whatsapp { jid } => PersonAccounts::PUBLISH_WHATSAPP_ACCOUNT
                        .with((account, name, jid, generation)),
                    PublicationKind::Sms { scope } => {
                        PersonAccounts::PUBLISH_SMS_ACCOUNT.with((account, name, scope, generation))
                    }
                    PublicationKind::Tilo { scope } => PersonAccounts::PUBLISH_TILO_ACCOUNT
                        .with((account, name, scope, generation)),
                };
            match person.change_accounts(invoke, || Ok(())) {
                Ok(()) => Reply::Done,
                Err(_) => Reply::Refused,
            }
        }
        _ => Reply::Refused,
    }
}

impl Drop for MessagingService {
    fn drop(&mut self) {
        // Closing its door is the service's signal to stop.
        self.link.close();
        let _ = self.child.wait();
    }
}

impl std::fmt::Debug for MessagingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagingService")
            .field("pid", &self.child.id())
            .finish_non_exhaustive()
    }
}

/// The service: beside the running compositor, else where the image
/// installs it. Never in a unit test, which must not start one.
fn helper() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-messaging")));
    beside
        .into_iter()
        .chain([PathBuf::from("/usr/libexec/tilo-messaging")])
        .find(|path| path.exists())
}

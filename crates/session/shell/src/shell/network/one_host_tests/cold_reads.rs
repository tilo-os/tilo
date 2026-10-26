//! An app opened cold reads a sealed room's record in full: what was
//! written into the record before the app ran on this device, by this
//! device or another, comes from the room's host when this device holds
//! none of it, through the app's own committed read.

use std::cell::RefCell;
use std::os::fd::IntoRawFd as _;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use ed25519_dalek::Signer as _;
use tilo_user_identity::DeviceCapabilities;
use ui::application::{ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget};
use ui::collaboration::CertifiedEndpointIdentity;
use ui::permissions::{PermissionContext, Role, UserId};
use ui::record::{RecordCheckout, RecordId, RecordOwners};

use super::{Device, Person, RoomId, Wakes, hosted_workspace, scratch, until};

/// The app whose record these tests make and open cold.
pub(crate) const CHAT: &str = "so.tilo.chat";

impl Device {
    /// How an app this device's session launched in `room` presents the
    /// device to its broker: the device's certificate, and its proof of the
    /// key, for the room.
    pub(crate) fn endpoint_identity(&self, room: RoomId) -> CertifiedEndpointIdentity {
        let identity = self.keyless_endpoint_identity(room);
        // An app of a session that serves through its broker encrypts under
        // the keys that session hands it, as a launched app does.
        let keys = self.endpoint_keys.lock().expect("endpoint keys").clone();
        match keys {
            Some(keys) => identity.with_channel_keys(keys),
            None => identity,
        }
    }

    /// [`Self::endpoint_identity`] with no channel keys to encrypt under.
    pub(crate) fn keyless_endpoint_identity(&self, room: RoomId) -> CertifiedEndpointIdentity {
        let key = self.key();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room.to_string(),
            device_certificate: serde_json::to_vec(&self.cert()).expect("certificate JSON"),
            proof: key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("certified join encodes");
        CertifiedEndpointIdentity::new(presentation, key)
    }

    /// Serve this device's session through a collaboration broker bound at
    /// `socket`, as a desktop session does, admitting this device's apps.
    pub(crate) fn start_broker(
        &mut self,
        socket: &Path,
    ) -> crate::shell::collab_broker::BrokerHandle {
        self.serve_through_broker();
        let broker = crate::shell::collab_broker::spawn_native_owner_harness_with_reads(
            socket.to_path_buf(),
            self.tree.clone(),
            vec![self.cert().device_public_key],
            self.net(),
            None,
        )
        .expect("bind the collaboration service");
        *self.endpoint_keys.lock().expect("endpoint keys") = Some(broker.endpoint_channel_keys());
        broker
    }

    /// A connection of `app` in `room` to this device's broker at
    /// `socket`, made as the launcher makes an app's.
    pub(crate) fn connect_app(
        &self,
        socket: &Path,
        app: &str,
        room: RoomId,
    ) -> ui::collaboration::CollaborationManager {
        crate::shell::launcher::register_in_process_launch(app, &room.to_string());
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("connect to the broker");
        launcher::services::write_collab_handoff(&mut stream, app, &room.to_string())
            .expect("hand the connection over as the launcher does");
        ui::collaboration::CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app,
            room.to_string().parse().expect("a room is a UUID"),
            None,
            Some(hex::encode(self.cert().device_public_key)),
            self.endpoint_identity(room),
        )
        .expect("start the collaboration client")
    }

    /// How many sealed writes of `app` this device's own host retains in
    /// `room`.
    pub(crate) fn retained_writes(&self, room: RoomId, app: &str) -> usize {
        let net = self.net();
        let host = net.room_host().expect("the device runs its host");
        let workspace = tilo_relay::room_workspace_id(&room.to_string()).unwrap();
        let records = host.room_records();
        net.wait_on(async move { records.retained(workspace).await })
            .and_then(std::convert::identity)
            .map_or(0, |bodies| {
                bodies
                    .iter()
                    .filter(|body| {
                        matches!(
                            postcard::from_bytes::<crate::network::protocol::NeighbourMsg>(&body.body),
                            Ok(crate::network::protocol::NeighbourMsg::AppDocument { app_id, envelope, .. })
                                if app_id == app && envelope.encryption.is_some()
                        )
                    })
                    .count()
            })
    }
}

impl Device {
    /// Write `changes` (Automerge change bytes) into `app`'s `record` in
    /// `room`, as the app does on its sealed channel: sealed under the
    /// record's channel keys, which this device asks the room's steward for,
    /// signed by this device and published to the room's host. Before this
    /// device serves through a broker, whose channel service then holds the
    /// keys instead.
    pub(crate) fn write_sealed(&self, room: RoomId, app: &str, record: &str, changes: Vec<u8>) {
        let keys = until(&[self], "the record's channel keys", || {
            let (channels, _) = self.channels.as_ref()?;
            let route = collab_types::ChannelRoute::new(app, record);
            match channels.access_record(room, &route).0 {
                super::ChannelAccess::Ready(keys) => Some(keys),
                super::ChannelAccess::Waiting => None,
                other => panic!("{} cannot open {app}'s channel: {other:?}", self.label),
            }
        });
        let key = self.key();
        let room_id = room.to_string();
        let artifact = collab_types::document_artifact_id(app, &room_id, record);
        let frames = collab_types::DocumentMutation::Incremental(changes)
            .encode_frames()
            .unwrap();
        for frame in frames {
            let (metadata, sealed) = keys
                .seal_content(
                    artifact,
                    collab_types::ContentRetention::of_frame(&frame),
                    &frame,
                )
                .unwrap();
            let now = std::time::SystemTime::now();
            let mut op = collab_types::SignedOp::new_unsigned_with_encryption(
                sealed,
                artifact,
                collab_types::Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
                collab_types::document_attestation_id(app, &room_id, record),
                collab_types::OpKind::Change,
                now,
                collab_types::next_unordered_document_sequence(now),
                metadata,
            );
            collab_types::sign_op(&mut op, &key).unwrap();
            self.net()
                .publish_document(app.into(), room_id.clone(), record, op);
        }
    }
}

/// The chat app authoring the room's chat on its device's sealed channel,
/// before the device serves through its broker.
pub(crate) struct SealedChat {
    doc: automerge::AutoCommit,
}

impl SealedChat {
    pub(crate) fn new() -> Self {
        let mut doc = automerge::AutoCommit::new();
        {
            use tilo_resource::{Fields as _, Resource as _};
            chat::Chat::default()
                .write_fields(&mut doc, &automerge::ROOT)
                .expect("the chat's fields");
            tilo_resource::evolution::write_version(&mut doc, chat::Chat::VERSION)
                .expect("the chat's version");
            doc.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::FrameworkChange::Genesis.change_message()),
            );
        }
        Self { doc }
    }

    /// Change the chat with `change` on `device` in `room`, as `person`,
    /// and write it there: the chat's genesis with its first change.
    pub(crate) fn write(
        &mut self,
        device: &Device,
        room: RoomId,
        person: &str,
        change: impl FnOnce(&mut automerge::AutoCommit, &str),
    ) {
        let first = self.doc.get_changes(&[]).len() == 1;
        let heads = if first {
            Vec::new()
        } else {
            self.doc.get_heads()
        };
        change(&mut self.doc, person);
        let changes = self.doc.save_after(&heads);
        device.write_sealed(room, CHAT, "", changes);
    }
}

/// Wait until `device`'s broker took every sealed write of `app` that
/// reached its network: the broker's drain empties the network's inbound
/// documents as it admits them.
pub(crate) fn until_broker_took(devices: &[&Device], device: &Device, app: &str) {
    let inbound = device.net().inbound_documents_handle();
    until(devices, "the broker to take the record's writes", || {
        (!inbound
            .lock()
            .unwrap()
            .iter()
            .any(|document| document.app_id == app))
        .then_some(())
    });
}

/// An app of `definition` opening the package's record in `room` cold, in
/// a fresh profile at `profile`, over `collaboration`: nothing of the
/// record is on the device's disk for it. The app and the records it
/// opened.
pub(crate) fn open_cold(
    profile: &Path,
    definition: &'static ApplicationDefinition,
    room: RoomId,
    author: &str,
    collaboration: ui::collaboration::CollaborationManager,
) -> (ui::App, RecordOwners) {
    assert!(!profile.exists(), "the profile is fresh");
    std::fs::create_dir_all(profile).unwrap();
    let device_id = ui::application::NativeProfile::open(profile, None)
        .unwrap()
        .device_id()
        .to_owned();
    let store: Rc<dyn durable_store::DurableStore> =
        Rc::new(app_state::FsStore::new(profile.to_path_buf()));
    let owners = RecordOwners::with_store(Rc::clone(&store));
    let room = room.to_string();
    let app = {
        let mut owners = owners.clone();
        ui::App::from_prepared_component_with_collaboration(
            collaboration,
            || {
                let records = owners
                    .open(definition.app_id, &room)
                    .map_err(ui::launch::LaunchError::Record)?;
                let checkout = RecordCheckout::package_primary(definition.record_kind.clone());
                records
                    .try_open_record(&checkout)
                    .map_err(ui::launch::LaunchError::Record)?;
                let target = LaunchTarget::Record {
                    checkout,
                    reveal: None,
                };
                let views = Rc::new(
                    ui::application_view::InstanceViewStore::with_store(
                        Rc::clone(&store),
                        &device_id,
                        &room,
                    )
                    .map_err(ui::launch::LaunchError::Restore)?
                    .without_persistence(),
                );
                let context = HostContext::prepare(
                    definition,
                    HostContextInputs {
                        records,
                        views,
                        device_id: device_id.clone(),
                        instance_id: 1,
                        permissions: Rc::new(RefCell::new(PermissionContext::member(
                            author,
                            Role::Owner,
                        ))),
                        author: UserId(author.to_owned()),
                        action_context: tilo_actions::ActionContext::default(),
                    },
                    &target,
                )?;
                Ok(ui::application::PreparedComponentInputs {
                    context,
                    target,
                    restore: ui::application::RestoreInput::None,
                    runner_inputs: ui::application::ComponentRunnerInputs::from_native_process(),
                })
            },
            |context, target, restore| definition.instantiate(context, target, restore),
        )
        .expect("the app opens cold")
    };
    (app, owners)
}

/// The ids of the chat's channels in `room`, as the app holding `owners`
/// reads its record, once it has it.
pub(crate) fn chat_channels(owners: &RecordOwners, room: RoomId) -> Option<Vec<String>> {
    let mut owners = owners.clone();
    let records = owners.open(CHAT, &room.to_string()).ok()?;
    records.read(&RecordId::package_primary(), |document| {
        chat::projection::read(document)
            .channel_rows()
            .iter()
            .map(|channel| channel.id.clone())
            .collect::<Vec<_>>()
    })
}

#[test]
#[serial_test::serial]
fn a_record_written_on_one_device_opens_in_full_on_another_device_of_the_person() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = scratch();
    // The brokers bind only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let wakes = Arc::new(Wakes::default());
    let ada = Person::new(81);
    home.env().set("TILO_USER_ID", ada.id());
    let desktop = Device::start(
        &ada,
        "Ada's desktop",
        &dir.path().join("desktop"),
        0x0A51,
        DeviceCapabilities::primary(),
        &wakes,
    );
    let mut laptop = Device::start(
        &ada,
        "Ada's laptop",
        &dir.path().join("laptop"),
        0x0A52,
        DeviceCapabilities::member(),
        &wakes,
    );
    let (_, room) = hosted_workspace(&desktop, &[&laptop], "Projects");
    laptop.reach(&desktop);
    until(
        &[&desktop, &laptop],
        "the laptop to hold the workspace",
        || laptop.holds(room).then_some(()),
    );

    // The desktop's chat app makes the room's chat, in two writes, on its
    // sealed channel; the room's host, on the desktop, keeps both.
    desktop.open_app_first(room, CHAT);
    let person = hex::encode(ada.user());
    let mut chat = SealedChat::new();
    chat.write(&desktop, room, &person, |document, person| {
        chat::demo::start(document, person, "Ada", "A").unwrap();
        chat::demo::insert_category(document, "projects", "Projects").unwrap();
    });
    until(
        &[&desktop, &laptop],
        "the chat's first write on the host",
        || (desktop.retained_writes(room, CHAT) >= 1).then_some(()),
    );
    let first = desktop.retained_writes(room, CHAT);
    chat.write(&desktop, room, &person, |document, _| {
        chat::demo::insert_channel(
            document,
            "harbour",
            "projects",
            "Harbour",
            chat::model::ChannelKind::Text,
        )
        .unwrap();
    });
    until(
        &[&desktop, &laptop],
        "the chat's second write on the host",
        || (desktop.retained_writes(room, CHAT) > first).then_some(()),
    );

    // Only now does the chat app open on the laptop, in a fresh profile,
    // through the laptop's own broker, which never had a window of it.
    let laptop_socket = dir.path().join("laptop-collab.sock");
    let laptop_broker = laptop.start_broker(&laptop_socket);
    until_broker_took(&[&desktop, &laptop], &laptop, CHAT);
    let (mut app, owners) = open_cold(
        &dir.path().join("laptop-chat"),
        chat::definition(),
        room,
        &person,
        laptop.connect_app(&laptop_socket, CHAT, room),
    );
    let channels = until(
        &[&desktop, &laptop],
        "the chat in full in the laptop's cold app",
        || {
            app.pump_hosted_model();
            chat_channels(&owners, room)
                .filter(|channels| channels.iter().any(|id| id == "harbour"))
        },
    );
    assert!(
        channels.iter().any(|id| id == "harbour"),
        "the laptop's cold app shows the channel the desktop wrote last, found {channels:?}"
    );
    drop(app);
    drop(laptop_broker);
    drop((desktop, laptop));
}

//! The native palette/control caller, derived from protected host identity.
//! No agent transport or app request can enter this process-owned capability.
use super::state::{RoomId, WeakWorkspaceTreeHandle};
use tilo_action_broker::services::LocalPersonServices;
use tilo_user_identity::DeviceCert;

pub(crate) struct NativeLocalPerson {
    pub tree: WeakWorkspaceTreeHandle,
}

fn current_certificate(room: &str, person: &str) -> Result<DeviceCert, String> {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let certificate = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .map_err(|error| error.to_string())?;
    if tilo_action_broker::collab_broker_capnp::principal_id_for_root(certificate.user_public_key)
        != person
    {
        return Err("native caller does not match the device person".into());
    }
    // A selected alternate workspace identity cannot borrow this device's
    // primary person. The certificate's fleet revocation is checked afresh.
    tilo_identity_host::certificates::authorize(room, &certificate)?;
    let registry = crate::identity_registry::IdentityRegistryStore::from_env()
        .map_err(|error| error.to_string())?;
    if !tilo_identity_host::provisioning::active_identity_owns_device(
        registry.identity_dir(),
        &paths,
    )
    .map_err(|error| error.to_string())?
    {
        return Err("native device identity is not currently claimed".into());
    }
    Ok(certificate)
}

impl LocalPersonServices for NativeLocalPerson {
    fn certificate(&self, room: &str, person: &str) -> Result<DeviceCert, String> {
        let certificate = current_certificate(room, person)?;
        let room = RoomId::parse(room).ok_or("native caller room is invalid")?;
        let tree = self.tree.upgrade().ok_or("native caller owner retired")?;
        if !tree.read().requires_native_owner(room) {
            return super::launcher::action_authority::with_ordinary_room_current(
                &tree,
                room,
                |_| Ok(certificate),
            );
        }
        let owner = tree
            .read()
            .installed_native_owner()
            .ok_or("native caller owner unavailable")?;
        owner.with_current(room, |view| {
            view.peer()
                .permissions(certificate.device_public_key, std::time::SystemTime::now())?;
            Ok(certificate)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_host(
        tree: &crate::shell::state::WorkspaceTreeHandle,
        room: RoomId,
    ) -> (
        super::super::remote_actions::BrokerActionHost,
        tokio::sync::mpsc::UnboundedReceiver<
            tilo_action_broker::collab_broker_capnp::InvocationEnvelope,
        >,
    ) {
        let broker =
            crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_authority(
                std::sync::Arc::new(super::super::broker_services::NativeWorkspaceAuthority(
                    tree.clone(),
                )),
                tilo_action_broker::services::HostServices {
                    native_certificates: Some(std::sync::Arc::new(
                        super::super::broker_services::NativeCertificates::default(),
                    )),
                    local_person: Some(std::sync::Arc::new(NativeLocalPerson {
                        tree: tree.downgrade(),
                    })),
                    ..Default::default()
                },
            );
        broker.replace_published_surfaces_for_test(vec![
            crate::shell::collab_broker_capnp::PublishedSurface {
                peer_id: 7,
                app_id: "so.tilo.settings".into(),
                route: crate::shell::collab_broker::CrdtRoute::app(
                    "so.tilo.settings",
                    room.to_string(),
                ),
                actions: [
                    ("inspect", tilo_actions::ActionKind::Read),
                    ("edit", tilo_actions::ActionKind::Undoable),
                    ("apply", tilo_actions::ActionKind::Outside),
                ]
                .into_iter()
                .map(
                    |(id, kind)| crate::shell::collab_broker_capnp::PublishedAction {
                        resource: "settings.display".into(),
                        semantic_identity: None,
                        action: id.into(),
                        resource_kind: "settings".into(),
                        resource_label: "Displays".into(),
                        action_label: id.into(),
                        description: None,
                        operation_kind: tilo_actions::OperationKind::View,
                        params: vec![],
                        contract: tilo_actions::ActionContract {
                            kind,
                            ..tilo_actions::ActionContract::view()
                        },
                        input: crate::shell::collab_broker_capnp::PublishedInput::None,
                        view: None,
                    },
                )
                .collect(),
                live: true,
            },
        ]);
        let invocations = broker.capture_invocations_for_test(7);
        let host = super::super::remote_actions::BrokerActionHost::new(broker.clone());
        drop(broker);
        (host, invocations)
    }

    fn invoke_native(
        host: &super::super::remote_actions::BrokerActionHost,
        caller: &tilo_actions::remote::Caller,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<(u64, tilo_actions::remote::InvocationReceipt), tilo_actions::remote::ActionHostError>
    {
        host.invoke_local_person_job(caller, request)
            .wait_timeout(std::time::Duration::from_secs(1))
            .ok()
            .flatten()
            .expect("native route resolves")
    }

    fn request(action: &str) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: "7".into(),
                address: tilo_actions::ActionAddress::new("settings.display", action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    /// A write publishes its own policy on the state writer; let that land
    /// before the explicit publication so none arrives after it.
    fn publish_after_saves(tree: &crate::shell::state::WorkspaceTreeHandle) {
        crate::shell::persist::wait_for_saves();
        super::super::launcher::action_authority::Export::of(&tree.read()).publish();
    }

    #[test]
    fn native_person_comes_from_protected_identity_and_borrows_live_ordinary_custody() {
        let directory = tempfile::tempdir().unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_CONFIG_HOME", directory.path().join("config"));
        environment.set("TILO_IDENTITY_ROOT", directory.path().join("device"));
        environment.set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
        environment.remove("TILO_USER_ID");
        let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
        let identity = runtime_identity::identity_dir_from_env().unwrap();
        tilo_identity_host::provisioning::ensure_collaboration_identity(
            &identity,
            &paths,
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let certificate =
            tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        let person = tilo_action_broker::collab_broker_capnp::principal_id_for_root(
            certificate.user_public_key,
        );
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let room = tree.desktop.workspaces[0].room;
        tree.desktop.workspaces[0].set_owner(&person);
        tree.persist_path = Some(directory.path().join("state.json"));
        let tree = tree.shared();
        let owner = super::super::native_workspace_owner::open(tree.clone()).unwrap();
        super::super::launcher::action_authority::Export::of(&tree.read()).publish();
        let service = NativeLocalPerson {
            tree: tree.downgrade(),
        };
        assert_eq!(
            service.certificate(&room.to_string(), &person).unwrap(),
            certificate
        );
        // The exact maintained BrokerActionHost used by the palette/admin
        // socket acquires this protected identity, not a supplied certificate.
        let (host, mut invocations) = native_host(&tree, room);
        let caller = tilo_actions::remote::Caller::local_person(&person).in_room(room.to_string());
        let invoke =
            |caller: &tilo_actions::remote::Caller, request| invoke_native(&host, caller, &request);
        assert!(invoke(&caller, request("apply")).is_ok());
        assert_eq!(invocations.try_recv().unwrap().actor, person);
        let agent = tilo_actions::remote::Caller::local_agent("agent").in_room(room.to_string());
        assert!(invoke(&agent, request("apply")).is_err());
        let mut continuation = request("apply");
        continuation.on_behalf_of = Some(tilo_actions::InvocationId::new());
        assert!(invoke(&caller, continuation).is_err());
        {
            let mut current = tree.write();
            current.desktop.workspaces[0].set_owner("another-owner");
            current.desktop.workspaces[0].admit(person.clone(), tilo_action_policy::Role::Viewer);
        }
        publish_after_saves(&tree);
        assert!(
            invoke(&caller, request("inspect")).is_ok(),
            "native identity does not elevate a Viewer"
        );
        assert!(invoke(&caller, request("apply")).is_err());
        tree.write().desktop.workspaces[0].admit(person.clone(), tilo_action_policy::Role::Editor);
        publish_after_saves(&tree);
        assert!(
            invoke(&caller, request("edit")).is_ok(),
            "native identity preserves the Editor role"
        );
        assert!(invoke(&caller, request("apply")).is_err());
        tree.write().desktop.workspaces[0].set_owner(&person);
        publish_after_saves(&tree);
        assert!(
            service
                .certificate(&room.to_string(), "forged-person")
                .is_err()
        );
        assert!(
            service
                .certificate(&RoomId::new().to_string(), &person)
                .is_err()
        );
        crate::shell::persist::wait_for_saves();
        // Hold the state writer: this change stays unpublished behind the job.
        let (release, held) = std::sync::mpsc::channel::<()>();
        crate::shell::durable_writer::global().submit(
            directory.path().join("hold-the-writer"),
            move || {
                let _ = held.recv();
            },
        );
        tree.write().desktop.workspaces[0].policy.owner = None;
        assert!(
            service.certificate(&room.to_string(), &person).is_err(),
            "unpublished policy changes revoke the captured native origin"
        );
        release.send(()).unwrap();
        crate::shell::persist::wait_for_saves();
        tree.write().desktop.workspaces[0].set_owner(person.clone());
        crate::shell::persist::wait_for_saves();
        super::super::launcher::action_authority::Export::of(&tree.read()).publish();
        assert!(service.certificate(&room.to_string(), &person).is_ok());
        drop(owner);
        assert!(
            service.certificate(&room.to_string(), &person).is_err(),
            "queued native origin service does not keep the writer alive"
        );
    }
}

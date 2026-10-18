use super::{NativeOwnerCommitService, check_profile_credential};
use agent_provider_config::management::CredentialStatus;
use agent_provider_config::{EndpointLocation, ProviderProfile, ProviderProtocol};

#[test]
fn local_and_external_noauth_profiles_do_not_inspect_vault() {
    for location in EndpointLocation::ALL {
        let profile = ProviderProfile {
            location: *location,
            protocol: ProviderProtocol::OpenAiCompatible,
            credential: None,
            sign_in: None,
            ..Default::default()
        };
        check_profile_credential(&profile, |_| panic!("no credential was requested")).unwrap();
    }
}

#[test]
fn configured_credentials_are_provider_and_model_neutral() {
    for protocol in [
        ProviderProtocol::Anthropic,
        ProviderProtocol::OpenAiCompatible,
    ] {
        let mut profile = ProviderProfile::hosted(protocol);
        profile.default_model = Some("any-configured-model".into());
        check_profile_credential(&profile, |id| {
            assert_eq!(id, profile.id);
            Ok(CredentialStatus::Configured)
        })
        .unwrap();
        for status in [
            CredentialStatus::Missing,
            CredentialStatus::MigrationRequired,
        ] {
            assert!(check_profile_credential(&profile, |_| Ok(status)).is_err());
        }
        assert!(check_profile_credential(&profile, |_| Err("vault unavailable".into())).is_err());
    }
}

#[test]
fn anthropic_without_a_credential_is_not_reported_ready() {
    let profile = ProviderProfile {
        protocol: ProviderProtocol::Anthropic,
        credential: None,
        sign_in: None,
        ..Default::default()
    };
    assert!(check_profile_credential(&profile, |_| panic!("no reference")).is_err());
}
use crate::shell::{native_workspace_owner, state::WorkspaceTree};
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_task::{WorkspaceTaskAuthority, WorkspaceTaskCommand};

#[test]
fn first_use_start_refuses_non_selected_person_even_when_they_own_the_governor() {
    assert_foreign_owner_denied(false);
}

#[test]
fn first_use_start_refuses_selected_person_with_foreign_owner_custody() {
    assert_foreign_owner_denied(true);
}

fn assert_foreign_owner_denied(use_selected_caller: bool) {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let system = home.config().join("test-system");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1")
        .set(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-tilo-test-bus",
        )
        .remove("TILO_USER_IDENTITY")
        .remove("TILO_IDENTITY_BROKER_FD");
    let identity = runtime_identity::identity_dir_from_env().unwrap();
    crate::firstboot::ensure_throwaway_test_identity(
        &identity,
        &crate::firstboot::collaboration_identity_paths(),
    )
    .unwrap();
    let room = crate::shell::state::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let registry = tilo_identity_host::registry::IdentityRegistryStore::at(&identity);
    let selected = registry
        .load_reconciled()
        .unwrap()
        .default_identity
        .unwrap();
    registry
        .bind_workspace(&room.to_string(), &selected)
        .unwrap();
    let (_, selected) =
        tilo_identity_host::broker::trusted_current_device(&room.to_string()).unwrap();

    let mut initial = WorkspaceTree::bootstrap_default();
    assert!(initial.set_room(initial.desktop.active, room));
    let path = home.config().join("state.json");
    initial.persist_path = Some(path.clone());
    let tree = initial.shared();
    let owner = native_workspace_owner::open(tree.clone()).unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[81; 32]);
    let foreign = tilo_joined_room::testing::certificate(
        &ed25519_dalek::SigningKey::from_bytes(&[82; 32]),
        &key,
        "Foreign governor",
    );
    let _governor =
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &foreign).unwrap();
    owner
        .with_current(room, |view| {
            assert_eq!(view.device(), &foreign);
            assert_eq!(view.governor(), &foreign);
            assert_ne!(view.device(), &selected);
            assert_ne!(foreign.user_public_key, selected.user_public_key);
            Ok(())
        })
        .unwrap();
    let service = NativeOwnerCommitService::new(tree, None);
    let before = std::fs::read(&path).unwrap();
    let request = OwnerCommitRequest {
        room: room.to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: if use_selected_caller {
                    selected
                } else {
                    foreign
                },
            },
            // Identity checks must precede model lookup and task preparation:
            // denial must not depend on configured compute or an existing queue.
            command: WorkspaceTaskCommand::ApproveRun {
                task_id: "task:unprepared".into(),
                agent_id: "unprovisioned-agent".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        },
    };
    let error = match service.prepare_native_task_start(request) {
        Ok(_) => panic!("foreign custody must not produce a pending native Start"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        if use_selected_caller {
            "managed provisioning for a foreign workspace owner is unsupported"
        } else {
            "first-use Start requires the actual selected native person's device"
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

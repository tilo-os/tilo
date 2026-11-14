//! Bots the governor installs in a workspace it governs: their programs,
//! the devices they act as, and the names a viewer sees them by.

use super::*;

/// A one-step program posting to chat, as canonical text.
#[cfg(feature = "agents")]
pub(crate) fn bridge_program() -> String {
    let rule = tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::root()
            .any(tilo_action_policy::Level::Workspace)
            .app("so.tilo.chat")
            .kind("chat.workspace")
            .any(tilo_action_policy::Level::Resource)
            .action("post"),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        tilo_action_policy::Answer::Allow,
    );
    let shortcut = tilo_automation::Shortcut {
        id: "bridge".into(),
        name: "Bridge".into(),
        inputs: vec![],
        steps: vec![tilo_automation::Step::new(
            "post",
            tilo_automation::StepKind::Invoke {
                action: tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                resource: tilo_automation::Selector::FirstOfKind("chat.workspace".into()),
                args: BTreeMap::default(),
                appearance: None,
            },
        )],
        step_timeout_secs: None,
        output: None,
        grant: vec![rule],
        triggers: vec![],
        saved_by: None,
        saved_proof: None,
    };
    String::from_utf8(tilo_automation::program::canonical_bytes(&shortcut).unwrap()).unwrap()
}

/// A capability-free bot device of the fixture's root, and its installer's
/// delegation naming it installation `id` in `workspace`.
#[cfg(feature = "agents")]
fn bot_device(
    id: &str,
    workspace: &str,
    seed: u8,
) -> tilo_peer_state::workspace_membership::BotDevice {
    use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
    let installer = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: installer.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Bot: Bridge".into(),
        capabilities: DeviceCapabilities::nothing(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &installer).unwrap();
    let now_ns = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &installer,
        cert.device_public_key,
        Delegate::Bot {
            installation: id.into(),
        },
        "Bridge",
        [1, 2, 3],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(workspace).unwrap()],
            apps: vec![],
        },
        now_ns.saturating_sub(1_000_000_000),
        now_ns.saturating_add(3_600_000_000_000),
    )
    .unwrap();
    tilo_peer_state::workspace_membership::BotDevice { cert, delegation }
}

/// Install `program` as bot `id` with `appearance` in `peer`, as the
/// governor `cert`.
#[cfg(feature = "agents")]
fn install_bot(
    peer: &mut StatePeer,
    (key, cert): (&SigningKey, &DeviceCert),
    program: &str,
    (id, seed): (&str, u8),
    appearance: tilo_peer_state::AppearancePolicy,
) {
    let command = install_command(peer.workspace_id(), program, (id, seed), appearance);
    assert_eq!(
        peer.workspace_mut().apply_workspace_membership_command(
            cert,
            &command,
            key,
            SystemTime::now()
        ),
        Ok(true)
    );
}

/// The command installing `program` as bot `id` with `appearance` in
/// `workspace`, its device of seed `seed`, whose key the fixture derives from
/// that seed.
#[cfg(feature = "agents")]
pub(crate) fn install_command(
    workspace: &str,
    program: &str,
    (id, seed): (&str, u8),
    appearance: tilo_peer_state::AppearancePolicy,
) -> tilo_peer_state::workspace_membership::WorkspaceMembershipCommand {
    use tilo_peer_state::workspace_membership::{BotInstall, WorkspaceMembershipCommand};
    WorkspaceMembershipCommand::InstallBot(Box::new(BotInstall {
        installation: tilo_peer_state::Installation {
            id: tilo_peer_state::InstallationId::parse(id).unwrap(),
            program: tilo_automation::program::program_hash(program.as_bytes()),
            name: "Bridge".into(),
            icon: None,
            installed_by: hex::encode(SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes()),
            installed_at_ms: 1,
            appearance,
            source: None,
            armed_at: vec![],
            connector_cursors_from_ms: 1,
            inputs: BTreeMap::default(),
            paused: false,
        },
        program: program.into(),
        device: Some(bot_device(id, workspace, seed)),
    }))
}

/// A bot installed with personas shows other names; one installed to post
/// as itself, an unknown installation and a workspace not governed here do not.
#[cfg(feature = "agents")]
#[tokio::test]
async fn only_a_live_bot_installed_with_personas_shows_other_names() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Bridged").await.unwrap();
    let governed = workspaces.workspace(&created.workspace).await.unwrap();
    let mut peer = governed.lock().await.peer().clone();
    let program = bridge_program();
    let personas = "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    let own = "7f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6";
    install_bot(
        &mut peer,
        (&key, &cert),
        &program,
        (personas, 8),
        tilo_peer_state::AppearancePolicy::Personas,
    );
    install_bot(
        &mut peer,
        (&key, &cert),
        &program,
        (own, 9),
        tilo_peer_state::AppearancePolicy::Own,
    );
    let now = SystemTime::now();
    assert!(crate::workspaces::views::bot_shows_personas(
        &peer, personas, now
    ));
    assert!(!crate::workspaces::views::bot_shows_personas(
        &peer, own, now
    ));
    assert!(!crate::workspaces::views::bot_shows_personas(
        &peer,
        "8f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
        now
    ));
    assert!(
        !workspaces
            .bot_personas(created.workspace_id, personas)
            .await,
        "the governed copy has no such installation"
    );
    assert!(
        !workspaces
            .bot_personas(WorkspaceId::from_u128(0), personas)
            .await
    );
    drop(workspaces);
    drop(relay);
}

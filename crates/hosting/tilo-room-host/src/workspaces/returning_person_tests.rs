//! A person who comes back after every device of theirs the workspace listed
//! has gone: a page's certificate lasts an hour, and the workspace forgets the
//! row it left once it expires. Membership is the person's, by their root, so
//! each way back finds them a member with the part they hold: a browser that
//! forgot the site opening their root with a passkey or a recovery key, which
//! certifies a new device; a new device paired from another of theirs, which
//! the certification queue takes and their root answers; and their agent,
//! whose delegation is bounded by their membership.
#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use super::fixtures::{door_join_as, governor, test_host};
use super::*;
use tilo_relay_protocol::entering::{Entered, Entering, Entry, Through};
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// A device of the person whose root is `root`, certified at `issued_at_ns`
/// until `expires_at_ns`, as a page's root certifies its tab.
fn device_of(root: &SigningKey, issued_at_ns: u64, expires_at_ns: u64) -> (SigningKey, DeviceCert) {
    let device = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Browser".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: Some(expires_at_ns),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (device, cert)
}

#[tokio::test]
async fn a_member_whose_every_device_expired_is_still_a_member_on_each_way_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let workspace = created.workspace.clone();
    let root = SigningKey::from_bytes(&rand::random());
    let person = root.verifying_key().to_bytes();

    // The person's first tab, whose certificate ends in two seconds, as a
    // page's ends in an hour.
    let now = tilo_clock::unix_now_ns();
    let (tab, tab_cert) = device_of(
        &root,
        now.saturating_sub(1_000_000_000),
        now + 2_000_000_000,
    );
    door_join_as(
        workspaces,
        &cert,
        &workspace,
        (&tab, &tab_cert),
        31,
        WorkspaceRole::Moderator,
    )
    .await;
    let registry = &workspaces.inner.host.workspaces;
    assert!(
        crate::workspace_access::admits_person(registry, cert.user_public_key, person).await,
        "a member while their tab is listed"
    );

    // The tab's certificate expires and the workspace settles without it.
    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
    let governed = workspaces.workspace(&workspace).await.unwrap();
    {
        let mut governed = governed.lock().await;
        workspaces.settle(&mut governed).await.unwrap();
        let access = governed
            .peer()
            .governance()
            .governed_access(tilo_clock::wall())
            .unwrap();
        assert!(
            !access.member_devices.contains(&tab_cert.device_public_key),
            "the expired tab's row is gone"
        );
        assert!(
            access.member_user_roots.contains(&person),
            "the person is a member by their root, with no device listed"
        );
    }

    // One answer everywhere a person's membership is read: the relay-wide
    // handshake and the certification queue (which a device paired from
    // another of theirs, and their root approving it, go through) read the
    // workspace runtimes' roots; an agent's delegation is bounded by the same
    // roots; the person's own listing of workspaces reads the governed ones.
    assert!(
        crate::workspace_access::admits_person(registry, cert.user_public_key, person).await,
        "the certification queue still takes the person's devices"
    );
    let runtime = registry
        .lock()
        .await
        .get(&created.workspace_id)
        .cloned()
        .unwrap();
    assert!(
        runtime.lock().await.governed_user_roots.contains(&person),
        "the handshake and an agent's delegation still find the person a member"
    );
    assert!(
        workspaces.inner.memberships.holds(&person, &workspace),
        "the person's workspaces still list this one"
    );

    // A browser that forgot the site opens the person's root with a passkey or
    // a recovery key and certifies a new tab: the door lets it in as theirs,
    // at the part they hold, not as a newcomer at the door's default.
    let now = tilo_clock::unix_now_ns();
    let (_, returned) = device_of(
        &root,
        now.saturating_sub(1_000_000_000),
        now + 3_600_000_000_000,
    );
    let entered = crate::handshake::Doorway::enter(
        workspaces,
        &returned,
        Entry {
            entering: Entering {
                workspace: workspace.clone(),
                name: "Returning".into(),
                request_id: rand::random(),
                through: Through::Door,
            },
            access: None,
            proof: [0; 64],
        },
    )
    .await;
    let Entered::Admitted { role, .. } = entered else {
        panic!("the returning person is let in: {entered:?}");
    };
    assert_eq!(role, WorkspaceRole::Moderator, "with the part they hold");
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert_eq!(
        members
            .iter()
            .filter(|member| member.user_public_key == person)
            .count(),
        1,
        "one person, on whichever device they came back"
    );

    // A stranger is not a member by any of these readings.
    let stranger = SigningKey::from_bytes(&rand::random())
        .verifying_key()
        .to_bytes();
    assert!(
        !crate::workspace_access::admits_person(registry, cert.user_public_key, stranger).await
    );
    relay.shutdown().await;
}

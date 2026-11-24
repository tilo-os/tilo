//! A returning browser's device and the workspace's entry: what the workspace refuses
//! of a device it knows, and whom it admits instead.

use super::tests::{governor, signed_pre_admission, test_host};
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// How long a tab's certificate lasts: thirty days.
const TAB_CERT_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000;

/// Ask a pre-admitted join of `device_cert` with a fresh pre-admission for its
/// person, as a browser coming through the entry does.
async fn pre_admitted_join(
    workspaces: &Workspaces,
    cert: &DeviceCert,
    workspace: &str,
    (key, device_cert): (&SigningKey, DeviceCert),
) -> tilo_server_protocol::workspaces::JoinReply {
    let token: [u8; 8] = rand::random();
    let issued = workspaces
        .pre_admit(
            cert,
            PreAdmitRequest {
                request_id: rand::random(),
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    workspaces
        .admit_proven(
            issued.signed_invitation,
            (device_cert, key),
            rand::random(),
            "Returning",
            Some(token.as_slice()),
        )
        .await
}

/// A browser whose person issues its device key a certificate again joins on
/// it: a later certificate that grants no more than the one the workspace
/// holds is the same device and replaces it. One that would grant it more, or
/// one issued before the held one, is refused as a conflict, and the same
/// person on a new device key is admitted. The browser relies on it
/// (`workspaces.mjs` `joinViaHost`).
#[tokio::test]
async fn a_later_certificate_of_a_device_replaces_it_and_an_escalating_or_older_one_is_refused() {
    use tilo_server_protocol::workspaces::JoinReply;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let root = SigningKey::from_bytes(&rand::random());
    let certify = |device: &SigningKey, issued_at_ns: u64, capabilities: DeviceCapabilities| {
        let mut device_cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            capabilities,
            issued_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut device_cert, &root).unwrap();
        device_cert
    };
    let device = SigningKey::from_bytes(&rand::random());
    let first = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device, certify(&device, 1, DeviceCapabilities::member())),
    )
    .await;
    assert!(matches!(first, JoinReply::Admitted { .. }), "{first:?}");

    // The same key certified later with more than the workspace holds: the
    // device would become more than it was admitted as.
    let escalated = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device, certify(&device, 2, DeviceCapabilities::primary())),
    )
    .await;
    assert!(
        matches!(&escalated, JoinReply::Refused(reason) if reason.contains("certificate conflict")),
        "{escalated:?}"
    );

    // The same key certified again by the same person, granting no more.
    let again = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device, certify(&device, 3, DeviceCapabilities::member())),
    )
    .await;
    assert!(matches!(again, JoinReply::Admitted { .. }), "{again:?}");

    // A certificate older than the one the workspace now holds.
    let older = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        (&device, certify(&device, 2, DeviceCapabilities::member())),
    )
    .await;
    assert!(
        matches!(&older, JoinReply::Refused(reason) if reason.contains("certificate conflict")),
        "{older:?}"
    );

    let renewed = SigningKey::from_bytes(&rand::random());
    let fresh = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        (&renewed, certify(&renewed, 4, DeviceCapabilities::member())),
    )
    .await;
    assert!(matches!(fresh, JoinReply::Admitted { .. }), "{fresh:?}");
    let members = workspaces
        .summary(&created.workspace)
        .await
        .unwrap()
        .members;
    assert_eq!(
        members
            .iter()
            .filter(|member| member.root() == Some(root.verifying_key().to_bytes()))
            .count(),
        1,
        "one person, whatever devices they came with"
    );
    server.shutdown().await;
}

/// A tab's certificate that `signer` — one of the person's own devices, certified
/// by their root — signed: what a computer that is already the person's signs
/// their browser tab in with.
fn based_on(
    root: &SigningKey,
    (signer, signer_cert): (&SigningKey, &DeviceCert),
    label: &str,
    now: u64,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(TAB_CERT_NS)),
        signature: [0; 64],
        issuer: Some(Box::new(signer_cert.clone())),
    };
    sign_device_cert(&mut cert, signer).expect("a device of the person's signs their tab's");
    (key, cert)
}

/// A tab one of the person's devices signed enters the person's own workspace as
/// theirs, and the workspace lets it in again on a session of its own: what a
/// reloaded tab does.
#[tokio::test]
async fn a_tab_a_member_device_signed_enters_and_comes_back_as_the_person() {
    use crate::workspace_access::{DenyReason, ServerTrust, WorkspaceOperation, decide};
    use tilo_server_protocol::entering::{Entered, Entering, Entry, Through};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Home").await.unwrap();
    let workspace = created.workspace.clone();
    let root = SigningKey::from_bytes(&rand::random());
    let now = tilo_clock::unix_now_ns();
    let entry = |through: Through| Entry {
        entering: Entering {
            workspace: workspace.clone(),
            name: "Jane".into(),
            request_id: rand::random(),
            through,
            admission: Vec::new(),
        },
        access: None,
        proof: [0; 64],
    };

    // The person's computer, certified by their root, joins the workspace they
    // were invited into.
    let (computer_key, computer) =
        super::tests::device_cert(&root, "Laptop", DeviceCapabilities::primary());
    let joined = pre_admitted_join(
        workspaces,
        &cert,
        &workspace,
        (&computer_key, computer.clone()),
    )
    .await;
    assert!(
        matches!(
            joined,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{joined:?}"
    );

    // The tab their computer signed, as the sign-in makes it, comes in as theirs.
    let (tab_key, tab) = based_on(&root, (&computer_key, &computer), "Firefox on phone", now);
    let entered = workspaces
        .enter_signed((&tab_key, &tab), entry(Through::Entry))
        .await;
    assert!(
        matches!(entered, Entered::Admitted { .. }),
        "a tab one of the person's devices signed enters as theirs: {entered:?}"
    );

    // Reloaded, it holds a session of its own: the workspace's admission, with no entry.
    let runtime = workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&created.workspace_id)
        .cloned()
        .unwrap();
    let runtime = runtime.lock().await;
    let peer = crate::auth::AuthenticatedPeer {
        user_public_key: tab.user_public_key,
        device_public_key: tab.device_public_key,
        cert: tab.clone(),
    };
    let service = &workspaces.inner.host;
    let trust = ServerTrust {
        revocations: &service.revocations,
        policy: &service.trust_policy,
        blocked: &service.blocked,
        own_user: &service.cert.user_public_key,
        agents: &service.agents,
        environments: &service.environments,
    };
    assert_ne!(
        decide(&runtime, &trust, &peer, WorkspaceOperation::Subscribe),
        crate::workspace_access::Decision::Deny(DenyReason::NotAMember),
        "the workspace admits the tab again on its own session"
    );
    drop(runtime);
    server.shutdown().await;
}

/// A member's device whose certificate ran out comes back through the entry
/// under a later certificate of its own person's root: the workspace holds the
/// device, and takes the later certificate in the one it held's place.
#[tokio::test]
async fn a_member_device_whose_certificate_ran_out_comes_back_under_its_renewal() {
    use tilo_server_protocol::entering::{Entered, Entering, Entry, Through};

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Home").await.unwrap();
    let workspace = created.workspace.clone();
    let root = SigningKey::from_bytes(&rand::random());
    let now = tilo_clock::unix_now_ns();
    let entry = || Entry {
        entering: Entering {
            workspace: workspace.clone(),
            name: "Jane".into(),
            request_id: rand::random(),
            through: Through::Entry,
            admission: Vec::new(),
        },
        access: None,
        proof: [0; 64],
    };
    let certified = |key: &SigningKey, issued_at_ns: u64, expires_at_ns: Option<u64>| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    };

    // The certificate the workspace is given expires two seconds from now.
    let device = SigningKey::from_bytes(&rand::random());
    let short = certified(
        &device,
        now.saturating_sub(1_000_000_000),
        Some(now + 2_000_000_000),
    );
    let joined = pre_admitted_join(workspaces, &cert, &workspace, (&device, short)).await;
    assert!(
        matches!(
            joined,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{joined:?}"
    );
    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;

    // Its person's root certifies the same device again, and it comes back as
    // the device the workspace holds: no conflict.
    let renewed = certified(&device, tilo_clock::unix_now_ns(), None);
    let entered = workspaces.enter_signed((&device, &renewed), entry()).await;
    assert!(
        matches!(entered, Entered::Admitted { .. }),
        "a member's device comes back under its renewal: {entered:?}"
    );
    server.shutdown().await;
}

//! The person's workspace list on a host that keeps workspaces of people's
//! own: one of their devices keeps it, any key of theirs reads it back with
//! the workspaces this host keeps for them, an agent's grant reads only what
//! it reaches, and nobody keeps or reads another person's.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionCallError, ActionProvider as _, InvokeRequest, ReceiptStatus};
use tilo_server_protocol::workspaces::JoinReply;
use tilo_server_session::workspaces::{
    CREATE_OWN_WORKSPACE, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    KEEP_PERSONAL, LIST_PERSONAL, PersonalList,
};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::ServiceWorkspaceActions;
use super::super::tests::{caller, settled};
use crate::workspaces::OwnWorkspaceOffer;
use crate::workspaces::fixtures::test_host_holding;
use crate::workspaces::tests::{governor, test_host};
use std::sync::Arc;

/// Hold `person`'s own workspace on `workspaces`: the list and devices the
/// host serves for them live there.
pub(super) async fn hold_person_workspace(
    workspaces: &crate::workspaces::Workspaces,
    person: [u8; 32],
) {
    let id = tilo_server_protocol::servers::person_workspace_id(&person);
    workspaces
        .create_for(crate::workspaces::person_workspace::NAME, Some(&id), person)
        .await
        .expect("the person's own workspace");
}

fn host_request(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", action, argument);
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, action),
    };
    request
}

async fn invoke(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let receipt = settled(
        host.as_host(device)
            .invoke(&caller(device), host_request(action, argument)),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

async fn read(host: &ServiceWorkspaceActions, device: &DeviceCert) -> PersonalList {
    serde_json::from_value(
        invoke(host, device, LIST_PERSONAL, json!({}))
            .await
            .unwrap(),
    )
    .unwrap()
}

/// A key of the person `root`, with `capabilities`.
fn key_of(root: &SigningKey, label: &str, capabilities: DeviceCapabilities) -> DeviceCert {
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        // A grant holds for at most 90 days.
        expires_at_ns: Some(tilo_clock::unix_now_ns().saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

const SHELL: &str = "11111111-1111-4111-8111-111111111111";
const LAPTOP: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's list from its first keep through a restart"
)]
async fn a_persons_keys_read_the_list_their_devices_kept_with_the_workspaces_here() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    // One device key service across the restart, as a server keeps its keys.
    let keys = Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    // The site let the person in: an invitation into its home names them.
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let agent = key_of(
        &root,
        "Claude",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let pinned = key_of(
        &root,
        "one workspace",
        DeviceCapabilities::agent_grant(AgentScope::parse(SHELL).unwrap()),
    );

    // Nothing kept yet: a key reads only the workspaces this host keeps.
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &desktop,
            CREATE_OWN_WORKSPACE,
            json!({"name": "On the server"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let here = &created.workspace.workspace;
    let listed = read(&host, &agent).await;
    assert_eq!(listed.workspaces.len(), 1, "{listed:?}");
    assert_eq!(&listed.workspaces[0].workspace, here);
    assert_eq!(listed.workspaces[0].role.as_deref(), Some("owner"));
    let address = listed.workspaces[0].host.as_ref().unwrap();
    assert_eq!(address.name, "tilo.test");
    assert_eq!(
        address.hint, None,
        "a workspace this host keeps is reached where the list was asked"
    );

    // The desktop keeps the person's list: its own workspaces, hosted on it.
    let kept = json!({"workspaces": [
        {"workspace": SHELL, "name": "Shell"},
        {"workspace": LAPTOP, "name": "Laptop"},
        {"workspace": here, "name": "On the server"},
    ]});
    assert_eq!(
        invoke(&host, &desktop, KEEP_PERSONAL, json!({"list": kept}))
            .await
            .unwrap(),
        json!({"kept": 3})
    );
    let listed = read(&host, &agent).await;
    let names: Vec<_> = listed
        .workspaces
        .iter()
        .map(|one| {
            (
                one.workspace.as_str(),
                one.host.as_ref().map(|host| host.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            (SHELL, None),
            (LAPTOP, None),
            (here.as_str(), Some("tilo.test")),
        ],
        "the person's order, each workspace here as this host knows it"
    );

    // Asked for one workspace it keeps, the host signs where it is entered, and the entry's
    // address still names no other device: a hint there is a device that keeps it.
    let ask_one = |workspace: &str| {
        let workspace = workspace.to_owned();
        let host = &host;
        let desktop = &desktop;
        async move {
            serde_json::from_value::<PersonalList>(
                invoke(
                    host,
                    desktop,
                    LIST_PERSONAL,
                    json!({"workspace": workspace}),
                )
                .await
                .unwrap(),
            )
            .unwrap()
        }
    };
    let one = ask_one(here).await;
    assert_eq!(one.workspaces.len(), 1, "{one:?}");
    assert_eq!(one.workspaces[0].host.as_ref().unwrap().hint, None);
    let hint: tilo_peer_state::join::TransportHint =
        postcard::from_bytes(&hex::decode(one.workspaces[0].enter_at.as_ref().unwrap()).unwrap())
            .unwrap();
    let (signer, _) =
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
            &hint,
        )
        .unwrap();
    assert_eq!(signer.workspace_id.to_string(), *here);
    assert_eq!(
        signer.device_certificate.device_public_key,
        workspaces.serving_certificate().device_public_key,
        "it is this host's own address"
    );
    let elsewhere = ask_one(SHELL).await;
    assert_eq!(
        elsewhere.workspaces[0].enter_at, None,
        "a workspace another device keeps is entered there"
    );

    // A grant for one workspace reads that one alone.
    let listed = read(&host, &pinned).await;
    assert_eq!(
        listed
            .workspaces
            .iter()
            .map(|one| one.workspace.as_str())
            .collect::<Vec<_>>(),
        [SHELL]
    );

    // A grant keeps nothing: the list is what the person's own devices hold.
    assert!(
        invoke(
            &host,
            &agent,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": []}})
        )
        .await
        .is_err()
    );
    // A list that names something other than workspaces is refused.
    assert!(
        invoke(
            &host,
            &desktop,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": [{"workspace": "not a workspace", "name": "x"}]}})
        )
        .await
        .is_err()
    );

    // Someone else reads only theirs.
    let stranger = key_of(
        &SigningKey::from_bytes(&rand::random()),
        "stranger",
        DeviceCapabilities::member(),
    );
    assert!(read(&host, &stranger).await.workspaces.is_empty());
    // Nor keeps one: the site never let them in, and nothing here is theirs.
    assert!(
        invoke(
            &host,
            &stranger,
            KEEP_PERSONAL,
            json!({"list": {"workspaces": []}})
        )
        .await
        .is_err()
    );

    // The list stays across a restart.
    server.shutdown().await;
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    assert_eq!(read(&host, &agent).await.workspaces.len(), 3);
    server.shutdown().await;
}

/// The person's devices are one record here: what a desktop holding the root
/// sends with its list and what a pairing the page's root answered here
/// issued are added, never taken out; every device of theirs that asks is in
/// it; a revocation marks a device; an agent's grant reads none of it; and a
/// certificate another root signed is never taken in.
#[tokio::test]
async fn one_record_lists_every_device_the_persons_root_certified() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    let foreign = key_of(
        &SigningKey::from_bytes(&rand::random()),
        "not theirs",
        DeviceCapabilities::member(),
    );
    let agent = key_of(
        &root,
        "Claude",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );

    // The desktop holding the root sends its fleet with its list.
    let carried = |cert: &DeviceCert| json!({"cert": cert});
    invoke(
        &host,
        &desktop,
        KEEP_PERSONAL,
        json!({"list": {"workspaces": [], "devices": [carried(&laptop), carried(&foreign)]}}),
    )
    .await
    .unwrap();
    // A browser the page's root certified, carried through this host's pairing.
    let browser = key_of(&root, "Firefox", DeviceCapabilities::member());
    workspaces.keep_issued_device(&browser, "").await;

    let labels = |list: &PersonalList| {
        let mut labels: Vec<(String, bool)> = list
            .devices
            .iter()
            .map(|device| (device.cert.device_label.clone(), device.revoked))
            .collect();
        labels.sort();
        labels
    };
    let listed = read(&host, &desktop).await;
    assert_eq!(
        labels(&listed),
        [
            ("Firefox".to_owned(), false),
            ("laptop".to_owned(), false),
            ("studio".to_owned(), false),
        ],
        "every key their root certified, the asking device among them, and no other root's"
    );
    assert!(
        read(&host, &agent).await.devices.is_empty(),
        "an agent's grant reads no device of the person's"
    );

    // A later keep naming no devices takes none out.
    invoke(
        &host,
        &desktop,
        KEEP_PERSONAL,
        json!({"list": {"workspaces": []}}),
    )
    .await
    .unwrap();
    assert_eq!(labels(&read(&host, &desktop).await).len(), 3);

    // The root takes the laptop back: the record marks it.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: laptop.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Stolen,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    // As the server keeps a revocation it took in from one of their sessions.
    tilo_lock::RwLockRecover::write_recover(&*workspaces.inner.host.revocations)
        .insert(entry)
        .unwrap();
    assert!(
        labels(&read(&host, &desktop).await).contains(&("laptop".to_owned(), true)),
        "a revoked device stays in the record, marked"
    );
    server.shutdown().await;
}

/// A browser keeps one key per open tab. A page asking for the record names
/// its browser's tab keys: each its person's root certified for a device of
/// theirs joins the record and is listed as that browser's; another root's
/// key, an agent's grant and an agent asking are not taken in; and each page
/// naming them replaces what the browser named before.
#[tokio::test]
async fn a_pages_browser_names_its_tab_devices_and_the_list_groups_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let tabs: Vec<DeviceCert> = (0..3)
        .map(|_| key_of(&root, "Firefox on Linux", DeviceCapabilities::member()))
        .collect();
    let foreign = key_of(
        &SigningKey::from_bytes(&rand::random()),
        "not theirs",
        DeviceCapabilities::member(),
    );
    let agent = key_of(
        &root,
        "Claude",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let browser = "0123456789abcdef0123456789abcdef";
    let ask = |certs: Vec<&DeviceCert>| json!({"browser": {"id": browser, "certs": certs}});

    let mut named: Vec<&DeviceCert> = tabs.iter().collect();
    named.push(&foreign);
    named.push(&agent);
    let listed: PersonalList = serde_json::from_value(
        invoke(&host, &tabs[0], LIST_PERSONAL, ask(named))
            .await
            .unwrap(),
    )
    .unwrap();
    let mut grouped: Vec<[u8; 32]> = listed
        .devices
        .iter()
        .filter(|device| device.browser.as_deref() == Some(browser))
        .map(|device| device.cert.device_public_key)
        .collect();
    grouped.sort_unstable();
    let mut expected: Vec<[u8; 32]> = tabs.iter().map(|tab| tab.device_public_key).collect();
    expected.sort_unstable();
    assert_eq!(grouped, expected, "the three tabs, as one browser's");
    assert_eq!(
        listed.devices.len(),
        3,
        "no other root's key and no agent's grant: {:?}",
        listed.devices
    );

    // A later page names two of them: the third is no longer the browser's,
    // and stays in the record as any device of theirs does.
    invoke(
        &host,
        &tabs[1],
        LIST_PERSONAL,
        ask(vec![&tabs[1], &tabs[2]]),
    )
    .await
    .unwrap();
    let listed = read(&host, &tabs[1]).await;
    let of = |key: [u8; 32]| {
        listed
            .devices
            .iter()
            .find(|device| device.cert.device_public_key == key)
            .and_then(|device| device.browser.clone())
    };
    assert_eq!(of(tabs[0].device_public_key), None);
    assert_eq!(of(tabs[1].device_public_key).as_deref(), Some(browser));
    assert_eq!(of(tabs[2].device_public_key).as_deref(), Some(browser));

    // An agent naming a browser changes nothing; a malformed one still reads.
    let other = key_of(&root, "Chrome", DeviceCapabilities::member());
    invoke(
        &host,
        &agent,
        LIST_PERSONAL,
        json!({"browser": {"id": browser, "certs": [&other]}}),
    )
    .await
    .unwrap();
    assert!(
        invoke(
            &host,
            &tabs[1],
            LIST_PERSONAL,
            json!({"browser": {"id": "nope"}})
        )
        .await
        .is_ok()
    );
    let listed = read(&host, &tabs[1]).await;
    assert!(
        listed
            .devices
            .iter()
            .all(|device| device.cert.device_public_key != other.device_public_key),
        "an agent names no browser of the person's"
    );
    server.shutdown().await;
}

/// One key of the person's as their record lists it.
fn listed_device<'a>(
    list: &'a PersonalList,
    cert: &DeviceCert,
) -> &'a tilo_server_session::workspaces::PersonalDevice {
    list.devices
        .iter()
        .find(|device| device.cert.device_public_key == cert.device_public_key)
        .unwrap_or_else(|| panic!("{} is in the record", cert.device_label))
}

/// Whether a session of `device` is open on `workspaces` now, as their record says.
async fn is_online(workspaces: &crate::workspaces::Workspaces, device: &DeviceCert) -> bool {
    workspaces
        .personal(device, &[], &super::Listed::default())
        .await
        .devices
        .iter()
        .any(|listed| listed.cert.device_public_key == device.device_public_key && listed.online)
}

/// When a session of `device` was last on `workspaces`, as their record says.
async fn seen_at(workspaces: &crate::workspaces::Workspaces, device: &DeviceCert) -> Option<i64> {
    let key = device.device_public_key;
    workspaces
        .personal(device, &[], &super::Listed::default())
        .await
        .devices
        .iter()
        .find(|listed| listed.cert.device_public_key == key)
        .and_then(|listed| listed.last_active_ms)
}

/// A listed workspace says when it was made, from the first change its
/// governance holds, so two workspaces of one name are told apart by it. A
/// later change to the workspace — a rename — does not move it.
#[tokio::test]
async fn a_listed_workspace_says_when_it_was_made_and_a_rename_does_not_move_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let actions = ServiceWorkspaceActions::new();
    actions.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    // The site let the person in: an invitation into its home names them.
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let made = |list: &PersonalList, workspace: &str| {
        list.workspaces
            .iter()
            .find(|one| one.workspace == workspace)
            .and_then(|one| one.made_at_ms)
    };

    let before = tilo_clock::unix_now_ns() / 1_000_000;
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &actions,
            &desktop,
            CREATE_OWN_WORKSPACE,
            json!({"name": "Studio"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let after = tilo_clock::unix_now_ns() / 1_000_000;
    let listed = read(&actions, &desktop).await;
    let made_at = made(&listed, &created.workspace.workspace)
        .expect("a listed workspace says when it was made");
    let at = u64::try_from(made_at).expect("a time after the epoch");
    assert!(
        before / 1_000 <= at / 1_000 && at / 1_000 <= after / 1_000,
        "{made_at} lies between {before} and {after}"
    );

    workspaces
        .rename(&created.workspace.workspace, "Studio again")
        .await
        .unwrap();
    let renamed = read(&actions, &desktop).await;
    assert_eq!(
        made(&renamed, &created.workspace.workspace),
        Some(made_at),
        "a later change to the workspace does not move when it was made"
    );

    server.shutdown().await;
}

/// A session of a person's key with this host says in their record that the
/// key is here now, and when it was last: a session that ends says when it
/// left, and the record keeps it across the host opening its files again.
#[tokio::test]
async fn a_keys_sessions_say_when_it_was_here_and_whether_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    // One device key service across the restart, as a server keeps its keys.
    let keys = Arc::<crate::workspaces::fixtures::TestGroupKeys>::default();
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    // The record holds the key: a device of theirs kept their list here.
    workspaces.keep_issued_device(&laptop, "").await;

    workspaces.session_opened_at(&laptop, 1_000).await;
    let listed = workspaces
        .personal(&laptop, &[], &super::Listed::default())
        .await;
    let device = listed_device(&listed, &laptop);
    assert!(device.online, "a session of the key is open here now");
    assert_eq!(device.last_active_ms, Some(1_000));
    assert_eq!(device.agent, None);

    workspaces.session_ended_at(&laptop, 5_000).await;
    let listed = workspaces
        .personal(&laptop, &[], &super::Listed::default())
        .await;
    let device = listed_device(&listed, &laptop);
    assert!(!device.online, "no session of the key is open");
    assert_eq!(device.last_active_ms, Some(5_000));

    // The host opens its files again: the record still says when the key was
    // here last.
    server.shutdown().await;
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let listed = server
        .workspaces()
        .personal(&laptop, &[], &super::Listed::default())
        .await;
    assert_eq!(listed_device(&listed, &laptop).last_active_ms, Some(5_000));
    server.shutdown().await;
}

/// A session opening writes when a key was here at most once a minute, since a
/// host serves many sessions, and one ending always writes: a device that
/// came and left within the minute is listed as having left when it did, and a
/// key the person's record does not hold writes nothing at all.
#[tokio::test]
async fn an_opening_is_written_once_a_minute_and_an_ending_always() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    let stranger = key_of(&root, "never kept", DeviceCapabilities::member());
    workspaces.keep_issued_device(&laptop, "").await;
    workspaces.session_opened_at(&laptop, 1_000).await;
    assert_eq!(seen_at(workspaces, &laptop).await, Some(1_000));
    workspaces.session_opened_at(&laptop, 30_000).await;
    assert_eq!(
        seen_at(workspaces, &laptop).await,
        Some(1_000),
        "a second session within the minute does not write when it was here again"
    );

    workspaces.session_ended_at(&laptop, 31_000).await;
    assert_eq!(
        seen_at(workspaces, &laptop).await,
        Some(31_000),
        "the session that ended a moment after one opened is still kept"
    );

    workspaces.session_opened_at(&stranger, 40_000).await;
    assert!(
        workspaces
            .personal(&stranger, &[], &super::Listed::default())
            .await
            .devices
            .iter()
            .all(|listed| listed.cert.device_public_key != stranger.device_public_key),
        "a key the person kept no record of writes nothing"
    );
    server.shutdown().await;
}

/// A session of a member's device — a person this host is not the own of —
/// says so in their record as it starts, from the session itself and not from
/// the record's own entry.
#[tokio::test]
async fn a_members_session_marks_its_key_online_as_it_starts() {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let laptop_key = SigningKey::from_bytes(&rand::random());
    let mut laptop = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: laptop_key.verifying_key().to_bytes(),
        device_label: "Claude Code on laptop".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut laptop, &root).unwrap();
    workspaces.keep_issued_device(&laptop, "").await;
    assert!(
        !is_online(workspaces, &laptop).await,
        "no session of the key is open yet"
    );

    let hello = crate::auth::Hello::new(laptop.clone(), [2; 32]);
    let peer = crate::auth::accept_hello(
        &hello,
        &owner.user_public_key,
        &crate::policy::TrustPolicy::AllowAny,
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let registry: crate::peer::WorkspaceRegistry = Arc::new(AsyncMutex::new(HashMap::new()));
    let context = crate::peer::PeerContext {
        workspaces: registry,
        revocations: Arc::new(std::sync::RwLock::new(
            tilo_user_identity::RevocationList::new(),
        )),
        trust_policy: Arc::new(std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny)),
        own_user_public_key: owner.user_public_key,
        own_device_cert: owner.clone(),
        own_device_public_key: owner.device_public_key,
        app_serving: None,
        metrics: Arc::new(crate::metrics::Metrics::default()),
        admin: Some(Arc::new(workspaces.clone())),
        fetch: None,
        agents: crate::peer::AgentRegistry::default(),
        environments: crate::environment::EnvironmentRegistry::default(),
    };
    let (_client, end) = tokio::io::duplex(65_536);
    let (read, write) = tokio::io::split(end);
    let session = tokio::spawn(crate::peer::run_peer_until(
        peer,
        std::future::pending::<()>(),
        read,
        write,
        context,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !is_online(workspaces, &laptop).await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session says in the record that the key is here");
    assert!(
        listed_device(
            &workspaces
                .personal(&laptop, &[], &super::Listed::default())
                .await,
            &laptop
        )
        .last_active_ms
        .is_some(),
        "and when it was here"
    );

    session.abort();
    server.shutdown().await;
}

/// A computer the person logged in is one of their own devices, and its session is their agent's
/// run: it reads the person's workspace list (its folders are named after their workspaces),
/// and asks its person before it changes anything of their account on the host.
#[tokio::test]
async fn an_agents_run_on_a_persons_own_device_reads_their_list_and_asks_before_changing_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    // The site let the person in: an invitation into its home names them.
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &desktop,
            CREATE_OWN_WORKSPACE,
            json!({"name": "My workspace"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let laptop = key_of(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let run = tilo_actions::remote::Caller::authenticated(
        laptop.user_public_key,
        laptop.device_public_key,
    )
    .delegated_to("Claude Code");
    let ask = |action: &str, argument: Value| {
        settled(
            host.as_host(&laptop)
                .invoke(&run, host_request(action, argument)),
        )
    };

    let receipt = ask(LIST_PERSONAL, json!({})).await.unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the run reads its person's list: {receipt:?}");
    };
    let list: PersonalList = serde_json::from_value(output.value.unwrap()).unwrap();
    assert!(
        list.workspaces
            .iter()
            .any(|listed| listed.workspace == created.workspace.workspace
                && listed.name == "My workspace"),
        "{list:?}"
    );
    for (action, argument) in [
        (CREATE_OWN_WORKSPACE, json!({"name": "Another"})),
        (KEEP_PERSONAL, json!({"list": {"workspaces": []}})),
    ] {
        // Changing anything asks its person first, and with nobody to ask
        // nothing changes.
        let refused = ask(action, argument).await;
        assert_eq!(
            refused.unwrap_err(),
            ActionCallError::confirmation_required(),
            "{action}"
        );
    }
    server.shutdown().await;
}

/// The session of `device`: the watch that sends it what its person's devices
/// are sent, and where that arrives.
fn session_of(
    workspaces: &crate::workspaces::Workspaces,
    device: &DeviceCert,
) -> (u64, tokio::sync::mpsc::Receiver<crate::transport::Envelope>) {
    let (outbound, pushed) = tokio::sync::mpsc::channel(8);
    (workspaces.watch_personal(device, outbound), pushed)
}

/// The next list the host pushes to a device's session, waiting for one that
/// `ok` takes: a change lands as the host settles it, not in a fixed order.
async fn next_list(
    pushed: &mut tokio::sync::mpsc::Receiver<crate::transport::Envelope>,
    ok: impl Fn(&PersonalList) -> bool + Send + Sync,
) -> PersonalList {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let crate::transport::Envelope::PersonalList { list } =
                pushed.recv().await.expect("the device's session")
            else {
                panic!("the host sends the person's list");
            };
            let list: PersonalList = serde_json::from_slice(&list).expect("a list");
            if ok(&list) {
                return list;
            }
        }
    })
    .await
    .expect("the host sends the list as it changes")
}

/// A device of a person's is sent their workspace list as it changes — one
/// they made, one they were admitted to, one renamed — without asking for it.
#[tokio::test]
async fn a_devices_session_is_sent_the_list_as_its_person_workspaces_change() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    // The site let the person in: an invitation into its home names them.
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    // The session of the person's device: where their list is sent.
    let (_watch, mut pushed) = session_of(workspaces, &desktop);

    // A workspace the person makes is in the list at once.
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &desktop,
            CREATE_OWN_WORKSPACE,
            json!({"name": "Second"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    next_list(&mut pushed, |list| {
        list.workspaces
            .iter()
            .any(|one| one.workspace == created.workspace.workspace && one.name == "Second")
    })
    .await;

    // One they are admitted to is too, without the ask that admitted them
    // answering the list.
    let token = [9_u8; 8];
    let issued = workspaces
        .pre_admit(
            &owner,
            crate::workspaces::PreAdmitRequest {
                request_id: rand::random(),
                grant: crate::workspaces::tests::signed_pre_admission(&home, &token),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let admitted = workspaces
        .admit_proven(
            issued.signed_invitation,
            desktop.clone(),
            rand::random(),
            "Studio",
            Some(token.as_slice()),
        )
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
    {
        let state = workspaces.workspace(&home).await.unwrap();
        let mut governed = state.lock().await;
        workspaces.settle(&mut governed).await.unwrap();
    }
    next_list(&mut pushed, |list| {
        list.workspaces
            .iter()
            .any(|one| one.workspace == home && one.name == "Lounge")
    })
    .await;

    // A renamed one reads by its new name.
    workspaces
        .rename(&created.workspace.workspace, "Renamed")
        .await
        .unwrap();
    next_list(&mut pushed, |list| {
        list.workspaces
            .iter()
            .any(|one| one.workspace == created.workspace.workspace && one.name == "Renamed")
    })
    .await;

    server.shutdown().await;
}

/// A device of a person's is sent the list of their devices when another of
/// them is taken back, marked there: whatever shows that list on it shows the
/// device gone without asking again.
#[tokio::test]
async fn a_devices_session_is_sent_the_list_when_a_device_is_taken_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let holder = test_host(dir.path(), &key, &owner).await;
    let workspaces = holder.workspaces();
    // The person the fixture is set up for: the root it certifies its own
    // device with.
    let root = SigningKey::from_bytes(&[1; 32]);
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    workspaces.keep_issued_device(&desktop, "").await;
    // The session of the person's desktop: where their list is sent.
    let (_watch, mut pushed) = session_of(workspaces, &desktop);
    let listed = |list: &PersonalList, device: &DeviceCert| {
        list.devices
            .iter()
            .find(|listed| listed.cert.device_public_key == device.device_public_key)
            .map(|listed| listed.revoked)
    };

    // A laptop paired with them is in the list their sessions are sent.
    workspaces.keep_issued_device(&laptop, "").await;
    next_list(&mut pushed, |list| listed(list, &laptop) == Some(false)).await;

    // Their root takes the laptop back, and one of their sessions says so.
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: laptop.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    workspaces.take_revocations(vec![entry]).await.unwrap();
    let sent = next_list(&mut pushed, |list| listed(list, &laptop) == Some(true)).await;
    assert_eq!(
        listed(&sent, &desktop),
        Some(false),
        "their other devices stay theirs"
    );
    holder.shutdown().await;
}

/// A workspace this host governs is listed as its governance has the person, whatever a device of
/// theirs kept: one they are not in (they left it, were taken out, or a device claimed it as
/// theirs) is in no list of theirs here, so their devices let it go, while one another server
/// keeps is still listed as their devices kept it.
#[tokio::test]
async fn a_workspace_governed_here_is_listed_only_as_its_governance_has_the_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    hold_person_workspace(workspaces, root.verifying_key().to_bytes()).await;
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    workspaces
        .issue(
            &home,
            workspaces.serving_certificate(),
            crate::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Viewer,
                recipient_user_public_key: Some(root.verifying_key().to_bytes()),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let desktop = key_of(&root, "studio", DeviceCapabilities::member());
    // The host's own workspace, which the person is not in.
    let team = workspaces.create("Team").await.unwrap().workspace;

    // A device of theirs keeps a list naming it as theirs, beside one another device keeps.
    let kept = json!({"workspaces": [
        {"workspace": SHELL, "name": "Shell", "role": "owner"},
        {"workspace": team, "name": "Team", "role": "owner"},
    ]});
    assert_eq!(
        invoke(&host, &desktop, KEEP_PERSONAL, json!({"list": kept}))
            .await
            .unwrap(),
        json!({"kept": 2})
    );
    let listed: Vec<String> = read(&host, &desktop)
        .await
        .workspaces
        .into_iter()
        .map(|one| one.workspace)
        .collect();
    assert_eq!(
        listed,
        [SHELL.to_owned()],
        "the workspace governed here that the person is not in is not listed from what a device kept"
    );
    server.shutdown().await;
}

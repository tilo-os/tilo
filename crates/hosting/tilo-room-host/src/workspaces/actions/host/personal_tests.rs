//! The person's workspace list on a host that keeps workspaces of people's
//! own: one of their devices keeps it, any key of theirs reads it back with
//! the workspaces this host keeps for them, an agent's grant reads only what
//! it reaches, and nobody keeps or reads another person's.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay handle a test binds is kept until the test ends; dropping it earlier closes the host it drives"
)]

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{ActionHost as _, ActionHostError, InvokeRequest, ReceiptStatus};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, KEEP_PERSONAL,
    LIST_PERSONAL, PersonalList,
};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use crate::workspaces::OwnWorkspaceOffer;
use crate::workspaces::tests::{governor, test_host};

fn host_request(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", action, argument);
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, action),
    };
    request
}

async fn invoke(
    host: &HostWorkspaceActions,
    device: &DeviceCert,
    action: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
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

async fn read(host: &HostWorkspaceActions, device: &DeviceCert) -> PersonalList {
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

const DESK: &str = "11111111-1111-4111-8111-111111111111";
const LAPTOP: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one person's list from its first keep through a restart"
)]
async fn a_persons_keys_read_the_list_their_devices_kept_with_the_workspaces_here() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
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
        DeviceCapabilities::agent_grant(AgentScope::parse(DESK).unwrap()),
    );

    // Nothing kept yet: a key reads only the workspaces this host keeps.
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &desktop,
            CREATE_OWN_ROOM,
            json!({"name": "On the relay"}),
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
        {"workspace": DESK, "name": "Desk", "host": {"name": "studio", "hint": "00"}},
        {"workspace": LAPTOP, "name": "Laptop"},
        {"workspace": here, "name": "On the relay"},
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
            (DESK, Some("studio")),
            (LAPTOP, None),
            (here.as_str(), Some("tilo.test")),
        ],
        "the person's order, each workspace here as this host knows it"
    );

    // A grant for one workspace reads that one alone.
    let listed = read(&host, &pinned).await;
    assert_eq!(
        listed
            .workspaces
            .iter()
            .map(|one| one.workspace.as_str())
            .collect::<Vec<_>>(),
        [DESK]
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
    relay.shutdown().await;
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    assert_eq!(read(&host, &agent).await.workspaces.len(), 3);
    relay.shutdown().await;
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
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
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
    workspaces.keep_issued_device(&browser);

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
    // As the relay keeps a revocation it took in from one of their sessions.
    tilo_lock::RwLockRecover::write_recover(&*workspaces.inner.host.revocations)
        .insert(entry)
        .unwrap();
    assert!(
        labels(&read(&host, &desktop).await).contains(&("laptop".to_owned(), true)),
        "a revoked device stays in the record, marked"
    );
    relay.shutdown().await;
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
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
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
    relay.shutdown().await;
}

/// One key of the person's as their record lists it.
fn listed_device<'a>(
    list: &'a PersonalList,
    cert: &DeviceCert,
) -> &'a tilo_relay_session::workspaces::PersonalDevice {
    list.devices
        .iter()
        .find(|device| device.cert.device_public_key == cert.device_public_key)
        .unwrap_or_else(|| panic!("{} is in the record", cert.device_label))
}

/// A session of a person's key with this host says in their record that the
/// key is here now, and when it was last: a session that ends says when it
/// left, and the record keeps it across the host opening its files again.
#[tokio::test]
async fn a_keys_sessions_say_when_it_was_here_and_whether_it_is_now() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    // The record holds the key: a device of theirs kept their list here.
    workspaces.keep_issued_device(&laptop);

    workspaces.session_opened_at(&laptop, 1_000);
    let listed = workspaces.personal(&laptop, &[]);
    let device = listed_device(&listed, &laptop);
    assert!(device.online, "a session of the key is open here now");
    assert_eq!(device.last_active_ms, Some(1_000));
    assert_eq!(device.agent, None);

    workspaces.session_ended_at(&laptop, 5_000);
    let listed = workspaces.personal(&laptop, &[]);
    let device = listed_device(&listed, &laptop);
    assert!(!device.online, "no session of the key is open");
    assert_eq!(device.last_active_ms, Some(5_000));

    // The host opens its files again: the record still says when the key was
    // here last.
    relay.shutdown().await;
    let relay = test_host(dir.path(), &key, &owner).await;
    let listed = relay.workspaces().personal(&laptop, &[]);
    assert_eq!(listed_device(&listed, &laptop).last_active_ms, Some(5_000));
    relay.shutdown().await;
}

/// A session opening writes when a key was here at most once a minute, since a
/// host serves many sessions, and one ending always writes: a device that
/// came and left within the minute is listed as having left when it did, and a
/// key the person's record does not hold writes nothing at all.
#[tokio::test]
async fn an_opening_is_written_once_a_minute_and_an_ending_always() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    let laptop = key_of(&root, "laptop", DeviceCapabilities::member());
    let stranger = key_of(&root, "never kept", DeviceCapabilities::member());
    workspaces.keep_issued_device(&laptop);
    let record = dir.path().join("rooms").join("personal-lists");
    let kept_bytes = || {
        std::fs::read(record.join(format!(
            "{}.json",
            hex::encode(root.verifying_key().to_bytes())
        )))
        .unwrap()
    };

    workspaces.session_opened_at(&laptop, 1_000);
    let opened = kept_bytes();
    workspaces.session_opened_at(&laptop, 30_000);
    assert_eq!(
        kept_bytes(),
        opened,
        "a second session within the minute does not write the record again"
    );
    assert_eq!(
        workspaces
            .personal(&laptop, &[])
            .devices
            .iter()
            .find(|device| device.cert.device_public_key == laptop.device_public_key)
            .and_then(|device| device.last_active_ms),
        Some(1_000)
    );

    workspaces.session_ended_at(&laptop, 31_000);
    assert_eq!(
        workspaces
            .personal(&laptop, &[])
            .devices
            .iter()
            .find(|device| device.cert.device_public_key == laptop.device_public_key)
            .and_then(|device| device.last_active_ms),
        Some(31_000),
        "the session that ended a moment after one opened is still kept"
    );

    let before = kept_bytes();
    workspaces.session_opened_at(&stranger, 40_000);
    assert_eq!(
        kept_bytes(),
        before,
        "a key the person kept no record of writes nothing"
    );
    relay.shutdown().await;
}

/// A key whose session says it runs an agent is listed with the agent, for a
/// person this host is not the own of, and joins their record even when none
/// of their devices kept a list here: the root that certified it is on no
/// page of this host's.
#[tokio::test]
async fn a_keys_session_says_which_agent_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
    let computer = key_of(&root, "Claude Code on laptop", DeviceCapabilities::member());
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());

    workspaces.note_agent(&computer, "Claude Code", &stop);
    let listed = workspaces.personal(&computer, &[]);
    let device = listed_device(&listed, &computer);
    assert_eq!(device.agent.as_deref(), Some("Claude Code"));
    assert!(!device.online, "an agent's name is no session open now");

    // Another of their keys reads the same record, and the name stays.
    let phone = key_of(&root, "phone", DeviceCapabilities::member());
    workspaces.keep_issued_device(&phone);
    workspaces.note_agent(&computer, "Claude Code", &stop);
    let listed = workspaces.personal(&phone, &[]);
    assert_eq!(
        listed_device(&listed, &computer).agent.as_deref(),
        Some("Claude Code")
    );
    relay.shutdown().await;
}

/// A session of a member's device — a person this host is not the own of —
/// says so in their record as it starts, from the session itself and not from
/// the record's own door.
#[tokio::test]
async fn a_members_session_marks_its_key_online_as_it_starts() {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as AsyncMutex;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let root = SigningKey::from_bytes(&rand::random());
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
    workspaces.keep_issued_device(&laptop);
    let online = || listed_device(&workspaces.personal(&laptop, &[]), &laptop).online;
    assert!(!online(), "no session of the key is open yet");

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
        autobind: None,
        revocations: Arc::new(std::sync::RwLock::new(
            tilo_user_identity::RevocationList::new(),
        )),
        trust_policy: Arc::new(std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny)),
        own_user_public_key: owner.user_public_key,
        own_device_cert: owner.clone(),
        own_device_public_key: owner.device_public_key,
        app_serving: None,
        metrics: Arc::new(crate::metrics::Metrics::default()),
        carried_calls: None,
        admin: Some(Arc::new(workspaces.clone())),
        fetch: None,
        agents: crate::peer::AgentRegistry::default(),
        environments: crate::environment::EnvironmentRegistry::default(),
        positions: None,
        compactor: None,
        member_limits: None,
    };
    let (_client, server) = tokio::io::duplex(65_536);
    let (read, write) = tokio::io::split(server);
    let session = tokio::spawn(crate::peer::run_peer_until(
        peer,
        std::future::pending::<()>(),
        read,
        write,
        context,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        while !online() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the session says in the record that the key is here");
    assert!(
        listed_device(&workspaces.personal(&laptop, &[]), &laptop)
            .last_active_ms
            .is_some(),
        "and when it was here"
    );

    session.abort();
    relay.shutdown().await;
}

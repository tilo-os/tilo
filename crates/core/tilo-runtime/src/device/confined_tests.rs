//! A workspace's VM runs this daemon as a device placed for records of that workspace: it keeps
//! no copy of any workspace and holds no keys, it lists and marks nothing of its person's, and
//! every call made on it goes to its workspace's host over the link it dialled, which decides it
//! as the placed device and seals what it writes.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    RESOURCE_PROVIDER, ReadAnswer, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    ResourceReadRequest,
};
use tilo_app_host::services::HostedWorkspaces as _;
use tilo_desktop_model::WorkspaceId;
use tilo_identity::sources::{DeviceTrust as _, DevicesTrust};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, PlacedRecords};
use tilo_workspace_service::environment::{
    ENVIRONMENT_RECORD, SETTINGS, VM_APP, VM_RECORD, document_channel, vm_placed_records,
};
use tilo_workspace_service::workspaces::person_workspace;

use super::{ConfinedHost, forwarded, placed_cert, placed_cert_until, trust_own, workspace_of};
use crate::device::hosts::Hosts;
use crate::device::ticket_tests::{Studio, enrol, studio};
use crate::device::workspaces::ServiceWorkspaces;

/// The login the VM's device entered with: the device its ticket made, in `workspace`, through
/// the host `through` names.
fn vm_login(
    workspace: &str,
    vm: &DeviceCert,
    through: &tilo_device_identity::login::Device,
) -> tilo_device_identity::login::Device {
    tilo_device_identity::login::Device {
        site: through.site.clone(),
        host_origin: through.host_origin.clone(),
        workspace: workspace.to_owned(),
        workspace_name: Some("Browser".into()),
        agent: "Browser".into(),
        cert: vm.clone(),
        server_cert: through.server_cert.clone(),
        server_address: through.server_address.clone(),
    }
}

/// A hint naming a device that serves `workspace`: what a member copy would be held by.
fn hint_for(workspace: &str) -> tilo_peer_state::join::TransportHint {
    let (key, cert) = enrol(
        &SigningKey::from_bytes(&[3; 32]),
        "host",
        DeviceCapabilities::member(),
    );
    tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
        version: 1,
        holds_keys: true,
        runs_work: true,
        label: String::new(),
        cloud: false,
        origin: None,
        workspace_id: tilo_peer_state::join::WorkspaceId::parse(workspace).unwrap(),
        endpoint_addr: iroh::EndpointAddr::new(
            iroh::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
        ),
        user_public_key: cert.user_public_key,
        device_certificate: cert,
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(&key)
    .unwrap()
}

/// A picture an emoji keeps: a PNG's signature and a few bytes, base64.
const PNG: &str =
    "iVBORw0KGgoBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQ==";

/// An hour, in ns.
const HOUR: u64 = 3_600_000_000_000;

/// The setting a VM session keeps its wallpapers in, as every device's session names it.
const WALLPAPERS: &str = "so.tilo.session-wallpapers";

/// A VM's device, placed by the person whose root is `root` for `records`.
fn placed_vm(root: &SigningKey, records: PlacedRecords, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let cert = placed_cert(root, key.verifying_key().to_bytes(), records, label);
    (key, cert)
}

/// The VM's service opened under `dir` as `(key, cert)`, reaching its workspace through the host
/// `login` entered through, as a daemon's start does.
async fn vm_daemon(
    dir: &std::path::Path,
    (key, cert): (&SigningKey, &DeviceCert),
    login: &tilo_device_identity::login::Device,
    server: &tilo_server::daemon::Server,
) -> (Arc<ServiceWorkspaces>, Arc<Hosts>) {
    let vm = ServiceWorkspaces::open_for_test(
        dir,
        key,
        cert,
        &tilo_server_client::ServerAddress::at(server.node_addr()),
    )
    .await
    .expect("the VM's daemon opens its service");
    vm.confine_to(ConfinedHost::of_login(cert, login).expect("the login names the host"));
    let hosts = Hosts::placed(&vm);
    (vm, hosts)
}

/// A call of `action` on `resource` of `provider`, with `arguments`.
fn call(provider: &str, (resource, action): (&str, &str), arguments: Value) -> InvokeRequest {
    let Value::Object(arguments) = arguments else {
        panic!("a call's arguments are named");
    };
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: tilo_actions::ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: Some(20_000),
        appearance: None,
        signed: None,
    }
}

/// The session's `settings#set` of `name` to `value`, as any device's session asks it.
fn set_setting(name: &str, value: &Value) -> InvokeRequest {
    call(
        tilo_device_identity::login::PROVIDER,
        (tilo_actions::remote::DEVICE_SETTINGS, "set"),
        json!({ "name": name, "value": value }),
    )
}

/// The host's renewal of the VM's certificate, handing it `cert`.
fn renewal(cert: &DeviceCert) -> InvokeRequest {
    use tilo_action_services::environment_window as wire;
    let mut request = InvokeRequest::plain(wire::target(wire::RENEW), serde_json::Map::new());
    request.argument = wire::argument(&wire::Renewal {
        certificate: serde_json::to_string(cert).unwrap(),
    });
    request.wait_ms = Some(20_000);
    request
}

/// What `host` answers `request` with, sent there as the confined device's call.
async fn asked(
    host: &Arc<dyn ActionProvider + Send + Sync>,
    workspace: &str,
    request: InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
    tilo_test_env::within(
        "the host answers the VM's call",
        host.invoke(&Caller::local_system().in_workspace(workspace), request),
    )
    .await
    .expect("the call is answered")
}

/// The VM session's settings as `vm` reads its environment record, through its host.
async fn settings_read(vm: &ServiceWorkspaces, workspace: &str) -> Value {
    let read = ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: VM_APP.to_owned(),
        record: ENVIRONMENT_RECORD.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let reads = vm.copies().resource_reads();
    let answered = tilo_test_env::within(
        "the host reads the environment record for the VM",
        tilo_actions::remote::ResourceReads::answer_read(
            reads,
            &Caller::local_system().in_workspace(workspace),
            0,
            read,
        ),
    )
    .await;
    let RemoteActionMessage::ResourceReading { result, .. } = answered else {
        panic!("a reading answers a read: {answered:?}");
    };
    let readings: Value =
        serde_json::from_str(&result.expect("the VM reads its environment record")).unwrap();
    readings["records"][0]["value"][SETTINGS].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one VM's daemon, from its login to its settings read back after a restart, read top \
              to bottom"
)]
async fn a_workspaces_vm_keeps_nothing_and_its_host_decides_its_calls() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let state = tempfile::tempdir().unwrap();
    let Studio {
        server,
        hosted,
        workspace,
        laptop_key,
        laptop_cert,
        login: laptop_login,
        ..
    } = studio(&root, state.path()).await;
    let person = root.verifying_key().to_bytes();
    let own = person_workspace::found(&hosted)
        .await
        .expect("the person's own workspace is on their host");
    let workspace_id = WorkspaceId::parse(&workspace).unwrap();

    // The VM's device is placed for the workspace's Browser record and its environment record,
    // and is listed nowhere.
    let (vm_key, vm_cert) = placed_vm(&root, vm_placed_records(workspace_id).unwrap(), "The VM");
    let login = vm_login(&workspace, &vm_cert, &laptop_login);
    let (vm, hosts) = vm_daemon(
        &state.path().join("vm"),
        (&vm_key, &vm_cert),
        &login,
        &server,
    )
    .await;
    assert_eq!(vm.confined(), Some(workspace_id));
    assert_eq!(
        workspace_of(&laptop_cert),
        None,
        "a laptop is confined nowhere"
    );
    assert!(
        ConfinedHost::of_login(&laptop_cert, &laptop_login).is_none(),
        "a device confined nowhere reaches its workspaces as every device does"
    );
    assert!(
        ConfinedHost::of_login(&vm_cert, &vm_login(&own, &vm_cert, &laptop_login)).is_none(),
        "a login into another workspace names no host of the VM's"
    );
    assert_eq!(
        vm.confined_host().map(|host| host.device),
        Some(laptop_login.server_cert.device_public_key)
    );

    // It keeps no copy of its workspace, nor of any other, and makes none: each would hold keys.
    let refused = vm
        .copies()
        .hold_as_member(workspace_id, &hint_for(&workspace))
        .await
        .expect_err("a confined device takes no member copy of its workspace");
    assert_eq!(
        refused,
        tilo_workspace_service::workspaces::CONFINED_HOLDS_NONE
    );
    let own_id = WorkspaceId::parse(&own).unwrap();
    assert_eq!(
        vm.copies()
            .hold_as_member(own_id, &hint_for(&own))
            .await
            .expect_err("nor of its person's own"),
        tilo_workspace_service::workspaces::CONFINED_HOLDS_NONE
    );
    assert_eq!(
        vm.copies()
            .create("Elsewhere")
            .await
            .expect_err("a confined device makes no workspace"),
        tilo_workspace_service::workspaces::CONFINED_HOLDS_NONE
    );
    crate::device::first_workspace::hold_person(vm.copies(), &vm_cert)
        .await
        .expect("a confined device writes nothing of its person's, and is refused nothing");
    assert!(vm.held().is_empty(), "the VM holds no workspace");
    assert!(!vm.keeps(workspace_id).await);

    // Of its person's devices it knows itself alone, in its workspace alone: it reads none of
    // their records.
    let trust = DevicesTrust::new();
    trust_own(&trust, vm_cert.clone());
    assert_eq!(trust.reaches(&vm_cert, &workspace), Some(true));
    assert_eq!(
        trust.reaches(&vm_cert, &own),
        Some(false),
        "the VM's key reaches no other workspace"
    );
    assert_eq!(
        trust.enrolled(&laptop_cert),
        Some(false),
        "the person's other devices are none it admits"
    );

    // Every call made on it goes to its host, in its one workspace alone: there is nothing it
    // admits here.
    assert!(hosts.host_actions(&own).is_none());
    assert!(
        forwarded(&vm, Some(&own)).is_err(),
        "no other workspace is reached from it"
    );
    let host = forwarded(&vm, None).expect("its workspace is on its host");

    // The host decides a call as the placed device: its key, its person named for history, the
    // records its certificate names, and never the person.
    let probe = Arc::new(tilo_action_services::TestProvider::new());
    hosted
        .providers()
        .serve_placed(
            "so.tilo.probe",
            VM_APP,
            vm_cert.device_public_key,
            Arc::clone(&probe) as _,
        )
        .unwrap();
    let probed = asked(
        &host,
        &workspace,
        call("so.tilo.probe", ("probe", "touch"), json!({})),
    )
    .await;
    assert!(
        probed.is_ok(),
        "the provider placed for it answers: {probed:?}"
    );
    let caller = probe
        .callers()
        .into_iter()
        .next()
        .expect("the host asked the provider");
    let placed = caller
        .placed_records()
        .expect("the host decides the call as a placed device");
    assert_eq!(placed.person(), &person);
    assert!(placed.reaches(&document_channel(VM_APP, ENVIRONMENT_RECORD)));
    assert_eq!(caller.device_public_key(), Some(&vm_cert.device_public_key));
    assert_eq!(caller.user_public_key(), None, "{caller:?}");
    assert_eq!(
        caller.acting().person_id(),
        None,
        "never the person: {caller:?}"
    );

    // What the host lists to it is what it may call there: the provider placed for it, and
    // nothing of the workspace's apps or of the host's own actions.
    let catalogue = tilo_test_env::within(
        "the host lists what the VM may call",
        host.query(&Caller::local_system(), CatalogueQuery::default()),
    )
    .await
    .expect("the host answers")
    .expect("a placed device reads what it may call");
    assert_eq!(probe.callers().len(), 2, "the probe was asked for its rows");
    assert!(
        catalogue
            .rows
            .iter()
            .all(|row| row.provider == "so.tilo.probe"),
        "{catalogue:?}"
    );
    // The VM's own settings actions lead that listing on the VM, as it sends them to its host.
    let own_rows = crate::device::ticket::settings_catalogue(&CatalogueQuery {
        needle: "set".into(),
        ..CatalogueQuery::default()
    });
    assert!(
        own_rows
            .rows
            .iter()
            .filter_map(|row| row.action.as_ref())
            .any(|action| action.address
                == tilo_actions::ActionAddress::new(tilo_actions::remote::DEVICE_SETTINGS, "set")),
        "{own_rows:?}"
    );

    // A ticket for a new computer is no placed device's to ask.
    let issued = asked(
        &host,
        &workspace,
        call(
            tilo_device_identity::login::PROVIDER,
            ("devices", "issue"),
            json!({}),
        ),
    )
    .await;
    assert_eq!(issued.err(), Some(ActionCallError::NotPermitted));

    // A typed action of a record its certificate does not name is refused at the host.
    let emoji = "tilo.workspace.emoji";
    let record = collab_types::workspace_document_id(emoji);
    let added = asked(
        &host,
        &workspace,
        call(
            &format!("{RESOURCE_PROVIDER}:{}:{record}", tilo_app_id::apps::CHAT),
            (emoji, "add-emoji"),
            json!({ "name": "vm_party", "image": PNG }),
        ),
    )
    .await;
    assert_eq!(added.err(), Some(ActionCallError::NotPermitted));
    assert!(
        hosted
            .resource_document(&workspace, tilo_app_id::apps::CHAT, &record)
            .await
            .unwrap()
            .is_none(),
        "nothing of the chat's record was written"
    );

    // The session's `settings#set`, the one every device's session keeps its records with, lands
    // in the workspace's environment record, made and sealed by the host and naming the person.
    let wallpapers = json!({ "default": "dunes" });
    let kept = asked(&host, &workspace, set_setting(WALLPAPERS, &wallpapers))
        .await
        .expect("the host keeps the VM session's setting");
    assert!(
        matches!(kept.status, ReceiptStatus::Completed { .. }),
        "{kept:?}"
    );
    let document = hosted
        .resource_document(&workspace, VM_APP, ENVIRONMENT_RECORD)
        .await
        .unwrap()
        .expect("the host keeps the environment record");
    let mut document = automerge::AutoCommit::load(&document).unwrap();
    let held = {
        use automerge::ReadDoc as _;
        let field = |object: &automerge::ObjId, name: &str| {
            document
                .get(object, name)
                .unwrap()
                .unwrap_or_else(|| panic!("the record holds {name}"))
        };
        let (_, settings) = field(&automerge::ROOT, SETTINGS);
        let (_, kept) = field(&settings, WALLPAPERS);
        let (default, _) = field(&kept, "default");
        default.as_str().map(str::to_owned)
    };
    assert_eq!(held.as_deref(), Some("dunes"));
    let message = document
        .get_changes(&[])
        .last()
        .and_then(|change| change.message().map(str::to_owned))
        .expect("the change says what it did");
    let context = tilo_actions::ActionContext::from_change_message(&message)
        .expect("the change names its action");
    assert_eq!(
        context.invoker,
        Some(tilo_actions::ActionInvoker::person(
            tilo_action_policy::PersonId::from_root(person)
        )),
        "history names the person whose VM it is"
    );
    // The person's own settings are none of it.
    for space in [&own, &workspace] {
        assert!(
            hosted
                .resource_document(space, tilo_actions::remote::DEVICE_SETTINGS, WALLPAPERS)
                .await
                .unwrap()
                .is_none(),
            "the person's own settings are untouched"
        );
    }
    assert_eq!(settings_read(&vm, &workspace).await[WALLPAPERS], wallpapers);

    // A device placed for the Browser record alone keeps no setting there.
    let (window_key, window_cert) = placed_vm(
        &root,
        PlacedRecords::new(
            workspace_id.as_uuid().into_bytes(),
            [document_channel(VM_APP, VM_RECORD)],
        )
        .unwrap(),
        "A window",
    );
    let (window, _window_hosts) = vm_daemon(
        &state.path().join("window"),
        (&window_key, &window_cert),
        &vm_login(&workspace, &window_cert, &laptop_login),
        &server,
    )
    .await;
    let refused = asked(
        &forwarded(&window, None).expect("its workspace is on its host"),
        &workspace,
        set_setting(WALLPAPERS, &json!({ "default": "elsewhere" })),
    )
    .await;
    assert_eq!(refused.err(), Some(ActionCallError::NotPermitted));
    window.shutdown().await;

    // The host that placed it hands it the same device certified again, over the link the VM
    // dialled. The VM takes nothing else: another device's key, other records, or an end no
    // later than its own.
    let later = placed_cert_until(
        &root,
        vm_cert.device_public_key,
        vm_placed_records(workspace_id).unwrap(),
        "The VM",
        HOUR * 3 / 2,
    );
    let placing = hosted
        .action_service()
        .remote_held(vm_cert.device_public_key, &workspace)
        .expect("the host holds the link the VM dialled");
    let renew = |cert: DeviceCert| {
        let placing = placing.clone();
        let workspace = workspace.clone();
        async move {
            tilo_test_env::within(
                "the VM answers its host's renewal",
                placing.invoke(
                    &Caller::local_system().in_workspace(workspace),
                    renewal(&cert),
                ),
            )
            .await
            .expect("the VM answers")
            .map(|receipt| matches!(receipt.status, ReceiptStatus::Completed { .. }))
        }
    };
    let (other_key, _) = placed_vm(&root, vm_placed_records(workspace_id).unwrap(), "Another");
    let browser_only = tilo_user_identity::PlacedRecords::new(
        workspace_id.as_uuid().into_bytes(),
        [document_channel(VM_APP, VM_RECORD)],
    )
    .unwrap();
    for (refused, why) in [
        (
            placed_cert_until(
                &root,
                other_key.verifying_key().to_bytes(),
                vm_placed_records(workspace_id).unwrap(),
                "Another",
                HOUR * 7 / 4,
            ),
            "another device's certificate",
        ),
        (
            placed_cert_until(
                &root,
                vm_cert.device_public_key,
                browser_only,
                "The VM",
                HOUR * 7 / 4,
            ),
            "a certificate for other records",
        ),
        (
            placed_cert_until(
                &root,
                vm_cert.device_public_key,
                vm_placed_records(workspace_id).unwrap(),
                "The VM",
                HOUR / 2,
            ),
            "a certificate ending no later",
        ),
    ] {
        assert_ne!(renew(refused).await, Ok(true), "the VM takes {why}");
        assert_eq!(vm.device_cert(), vm_cert, "the VM keeps its own for {why}");
    }
    assert_eq!(
        renew(later.clone()).await,
        Ok(true),
        "the VM takes its renewal"
    );
    assert_eq!(
        vm.device_cert(),
        later,
        "the VM holds its renewed certificate"
    );
    let proven_cert = tilo_test_env::within("the VM proves itself again", placing.certificate())
        .await
        .expect("the VM answers its certificate");
    assert_eq!(
        proven_cert, later,
        "the next link proves the renewed certificate"
    );

    // A device other than the host that placed it is refused by name: the VM takes requests
    // from that host alone. Its endpoint answers the device actions as a daemon's does.
    let answering = iroh::protocol::Router::builder(vm.endpoint())
        .accept(
            tilo_peer_state::actions::ACTIONS_ALPN,
            vm.copies().action_service(),
        )
        .spawn();
    let stranger = ServiceWorkspaces::open_for_test(
        &state.path().join("stranger"),
        &laptop_key,
        &laptop_cert,
        &tilo_server_client::ServerAddress::at(server.node_addr()),
    )
    .await
    .expect("another device's service opens");
    let asked_vm = stranger.copies().action_service().remote(
        &stranger.endpoint(),
        &vm.endpoint().addr(),
        &workspace,
    );
    for asked in [
        call("so.tilo.probe", ("probe", "touch"), json!({})),
        renewal(&later),
    ] {
        let refused = tilo_test_env::within(
            "the VM answers another device",
            asked_vm.invoke(
                &Caller::local_system().in_workspace(workspace.clone()),
                asked,
            ),
        )
        .await
        .expect("the VM answers");
        let named = hex::encode(&laptop_cert.device_public_key[..8]);
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason }) if reason.contains(&named)),
            "the VM refuses another device, naming it: {refused:?}"
        );
    }
    assert_eq!(vm.device_cert(), later, "nobody else renews the VM");
    stranger.shutdown().await;
    answering.shutdown().await.unwrap();

    // After a restart the VM's session reads what it kept back from its host: its disk kept
    // nothing.
    vm.shutdown().await;
    drop(hosts);
    let (vm, _hosts) = vm_daemon(
        &state.path().join("vm"),
        (&vm_key, &vm_cert),
        &login,
        &server,
    )
    .await;
    assert_eq!(settings_read(&vm, &workspace).await[WALLPAPERS], wallpapers);
    vm.shutdown().await;

    // A member device that keeps a copy of the workspace applies a record's action to that copy
    // itself, its host unreachable: a change made offline waits for no one, and the copy carries
    // it to the host once it is reached.
    let laptop = ServiceWorkspaces::open_for_test(
        &state.path().join("laptop-copy"),
        &laptop_key,
        &laptop_cert,
        &tilo_api::daemon::enter::server_of(&laptop_login)
            .await
            .unwrap(),
    )
    .await
    .expect("the laptop's service opens");
    let entered = tilo_test_env::within(
        "the laptop enters its workspace",
        tilo_api::daemon::enter::connect(&laptop_key, &laptop_login, Some(laptop.lent())),
    )
    .await
    .expect("the laptop enters");
    tilo_test_env::within("the laptop keeps its copy", laptop.hold(&entered)).await;
    assert!(laptop.keeps(workspace_id).await, "the laptop keeps a copy");
    let _placed = Hosts::placed(&laptop);
    tilo_test_env::within("the laptop's copy holds the environment record", async {
        while !matches!(
            laptop
                .copies()
                .resource_document(&workspace, VM_APP, ENVIRONMENT_RECORD)
                .await,
            Ok(Some(_))
        ) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    server.shutdown().await;
    let receipt = tilo_test_env::within(
        "the laptop applies the action to its copy",
        laptop
            .copies()
            .providers()
            .invoke(
                &Caller::authenticated(laptop_cert.user_public_key, laptop_cert.device_public_key)
                    .in_workspace(workspace.clone()),
                call(
                    &format!("{RESOURCE_PROVIDER}:{}:{record}", tilo_app_id::apps::CHAT),
                    (emoji, "add-emoji"),
                    json!({ "name": "offline_party", "image": PNG }),
                ),
            )
            .expect("the laptop's dispatch takes it"),
    )
    .await
    .expect("the call is answered")
    .expect("the laptop takes the call");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the laptop's copy takes the change with its host gone: {receipt:?}"
    );
    laptop.shutdown().await;
}

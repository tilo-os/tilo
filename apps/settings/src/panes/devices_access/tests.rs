//! What Devices and access lists, and what the person does to it, through the
//! host calls the page makes.

use super::*;

use ui::prelude::TestApp;
use ui::tilo_actions::remote::mcp_server::Reach;
use ui::tilo_actions::remote::{
    ActionCallError, Caller, Completer, Confirmation, InvocationReceipt, InvokeRequest,
    ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake,
    device_tickets,
};

use crate::resource::SettingsShell;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn plain(text: &str) -> String {
    text.replace(['\u{2068}', '\u{2069}'], "")
}

fn plain_all(lines: Vec<String>) -> Vec<String> {
    lines.into_iter().map(|line| plain(&line)).collect()
}

fn sample() -> AccessSample {
    crate::scenarios::access_fixture(Some("devices_access")).expect("the access capture")
}

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

struct RecordingHost {
    asked: std::sync::Mutex<Vec<InvokeRequest>>,
    sheet: std::sync::Mutex<Sheet>,
    /// What the MCP server answers of the person's agents. None answers
    /// here until a test says what.
    served: std::sync::Mutex<Option<serde_json::Value>>,
    /// The actions this host refuses whoever asks, after noting the ask.
    refused: std::sync::Mutex<Vec<ui::tilo_actions::remote::RemoteActionTarget>>,
    /// The person's records, as the record API reads them.
    records: std::sync::Arc<PersonRecords>,
}

/// The records of the person's own workspace this host keeps, built from the
/// devices a test lists as the page shows them: each certified by the
/// person's root, as every device of theirs is.
struct PersonRecords {
    root: ed25519_dalek::SigningKey,
    /// The devices, each as a row of the page reads it.
    listed: std::sync::Mutex<serde_json::Value>,
    /// Who waits to hear the records change.
    waiting: std::sync::Mutex<Vec<ResourceWake>>,
    /// How often the page asked for the record of the person's devices.
    devices_asked: std::sync::atomic::AtomicUsize,
}

impl PersonRecords {
    fn person(&self) -> [u8; 32] {
        self.root.verifying_key().to_bytes()
    }

    /// The device the page is shown on: the one the list marks current.
    fn this_device(&self) -> [u8; 32] {
        let listed = self.listed.lock().unwrap();
        let this = listed
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["current"] == true)
            .expect("one device is this one");
        key_bytes(this["key_hex"].as_str().unwrap())
    }

    /// The person's devices record: each listed device's certificate, signed
    /// by their root, with the name, agent, mark and facts the list shows.
    fn devices(&self) -> serde_json::Value {
        let listed = self.listed.lock().unwrap();
        let mut devices = serde_json::Map::new();
        for (at, device) in listed.as_array().unwrap().iter().enumerate() {
            let key = device["key_hex"].as_str().unwrap();
            let ns = |field: &str| {
                device[field]
                    .as_i64()
                    .map(|ms| u64::try_from(ms.max(0)).unwrap() * 1_000_000)
            };
            let mut cert = tilo_user_identity::DeviceCert {
                user_public_key: self.person(),
                device_public_key: key_bytes(key),
                device_label: device["label"].as_str().unwrap().to_owned(),
                capabilities: tilo_user_identity::DeviceCapabilities::member(),
                issued_at_ns: ns("issued_ms").unwrap_or(0),
                expires_at_ns: ns("expires_ms"),
                signature: [0; 64],
                issuer: None,
            };
            tilo_user_identity::sign_device_cert(&mut cert, &self.root).unwrap();
            let mut facts = serde_json::Map::new();
            if let Some(seen) = device["last_active_ms"].as_i64() {
                facts.insert("seen".into(), hex::encode(seen.to_le_bytes()).into());
            }
            devices.insert(
                key.to_owned(),
                serde_json::json!({"k": format!("{at:04}"), "v": {
                    "certificate": hex::encode(postcard::to_allocvec(&cert).unwrap()),
                    "name": device["label"],
                    "agent": device["agent"],
                    "revoked": device["revoked"].as_bool().unwrap_or(false),
                    "facts": facts,
                }}),
            );
        }
        serde_json::json!({ "devices": devices })
    }

    /// Which browser each tab key is a tab of.
    fn tab_groups(&self) -> serde_json::Value {
        let listed = self.listed.lock().unwrap();
        listed
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|device| {
                Some((
                    format!("tab:{}", device["key_hex"].as_str()?),
                    device["browser"].as_str()?.into(),
                ))
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    }

    /// Who is in the person's workspace: the person, on each device the list
    /// shows online.
    fn here(&self) -> serde_json::Value {
        let listed = self.listed.lock().unwrap();
        let online: Vec<&serde_json::Value> = listed
            .as_array()
            .unwrap()
            .iter()
            .filter(|device| device["online"] == true)
            .map(|device| &device["key_hex"])
            .collect();
        serde_json::json!({ "here": [{"who": hex::encode(self.person()), "name": "Ada",
            "kind": "person", "devices": online}] })
    }

    /// Change the devices, and tell whoever reads the records.
    fn change(&self, change: impl FnOnce(&mut serde_json::Value)) {
        change(&mut self.listed.lock().unwrap());
        for wake in std::mem::take(&mut *self.waiting.lock().unwrap()) {
            wake.wake();
        }
    }
}

impl ResourceReads for PersonRecords {
    fn read(&self, _: &Caller, address: &ResourceAddress) -> Result<ResourceCopy, ActionCallError> {
        use tilo_server_protocol::devices::{DEVICES, TAB_GROUPS};
        if address.workspace != tilo_action_policy::roles::person_workspace_id(&self.person()) {
            return Err(ActionCallError::Refused {
                reason: "not the person's workspace".into(),
            });
        }
        let value = match (address.app.as_str(), address.record.as_str()) {
            read if read == DEVICES => self.devices(),
            read if read == TAB_GROUPS => self.tab_groups(),
            (ui::tilo_actions::remote::PRESENCE, "") => self.here(),
            _ => return Ok(ResourceCopy::Absent),
        };
        Ok(ResourceCopy::reading(value, None))
    }

    fn answer_read(
        self: std::sync::Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> ui::tilo_actions::remote::Pending<ui::tilo_actions::remote::RemoteActionMessage> {
        if (read.app.as_str(), read.record.as_str()) == tilo_server_protocol::devices::DEVICES {
            self.devices_asked
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        ui::tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        _: &Caller,
        _: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        self.waiting.lock().unwrap().push(wake.clone());
        Ok(())
    }
}

fn key_bytes(key: &str) -> [u8; 32] {
    let mut bytes = [0; 32];
    hex::decode_to_slice(key, &mut bytes).unwrap();
    bytes
}

/// How the session's shell answers an ask to open one of its sheets.
#[derive(Default)]
enum Sheet {
    /// At once and with nothing: the sheet on the screen is its answer.
    #[default]
    Opens,
    /// With why this caller may not have it.
    Refused,
    /// Not yet: the answers it still owes.
    Held(Vec<(ui::tilo_actions::InvocationId, Completer<Answer>)>),
}

type Answer = Result<InvocationReceipt, ActionCallError>;

fn opened(invocation: ui::tilo_actions::InvocationId) -> InvocationReceipt {
    InvocationReceipt {
        invocation,
        status: ui::tilo_actions::remote::ReceiptStatus::Completed {
            output: ui::tilo_actions::ActionOutput::default(),
        },
    }
}

const THIS_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const LAPTOP_AGENT: &str = "5555555555555555555555555555555555555555555555555555555555555555";
const BROWSER: &str = "0123456789abcdef0123456789abcdef";

impl RecordingHost {
    fn new(listed: serde_json::Value) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asked: std::sync::Mutex::default(),
            sheet: std::sync::Mutex::default(),
            served: std::sync::Mutex::default(),
            refused: std::sync::Mutex::default(),
            records: std::sync::Arc::new(PersonRecords {
                root: ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
                listed: std::sync::Mutex::new(listed),
                waiting: std::sync::Mutex::default(),
                devices_asked: std::sync::atomic::AtomicUsize::new(0),
            }),
        })
    }

    /// This device is the one the list marks current, of the person whose
    /// root certified every device listed: what the identity service tells
    /// an app.
    fn sign_in(&self) {
        ui::local_identity::install_source(Some(std::sync::Arc::new(
            ui::local_identity::FixedIdentity(ui::local_identity::LocalIdentity {
                group_public_key: self.records.person(),
                display_name: "Ada".into(),
                avatar_color: [0; 3],
                device_public_key: Some(self.records.this_device()),
            }),
        )));
    }

    /// Change the person's devices as their record has them, and tell
    /// whoever follows it.
    fn change(&self, change: impl FnOnce(&mut serde_json::Value)) {
        self.records.change(change);
    }

    fn shell(&self, sheet: Sheet) {
        *self.sheet.lock().unwrap() = sheet;
    }

    /// The shell opens every sheet it was asked for and held back.
    fn open_held(&self) {
        let held = match &mut *self.sheet.lock().unwrap() {
            Sheet::Held(held) => std::mem::take(held),
            Sheet::Opens | Sheet::Refused => Vec::new(),
        };
        for (invocation, answer) in held {
            answer.complete(Ok(opened(invocation)));
        }
    }

    fn asked(&self) -> Vec<InvokeRequest> {
        self.asked.lock().unwrap().clone()
    }

    fn asked_at(
        &self,
        target: &ui::tilo_actions::remote::RemoteActionTarget,
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.asked()
            .into_iter()
            .filter(|request| &request.target == target)
            .map(|request| match request.argument {
                InvokeArgument::Json(argument) => argument,
                _ => serde_json::Map::new(),
            })
            .collect()
    }
}

impl ui::tilo_actions::remote::ActionProvider for RecordingHost {
    fn query(
        &self,
        _: &Caller,
        _: ui::tilo_actions::remote::CatalogueQuery,
    ) -> ui::tilo_actions::remote::Pending<
        Result<ui::tilo_actions::remote::CataloguePage, ui::tilo_actions::remote::ActionCallError>,
    > {
        ui::tilo_actions::remote::Pending::ready(Err(
            ui::tilo_actions::remote::ActionCallError::NotPermitted,
        ))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> ui::tilo_actions::remote::Pending<
        Result<
            ui::tilo_actions::remote::InvocationReceipt,
            ui::tilo_actions::remote::ActionCallError,
        >,
    > {
        let invocation = request.invocation;
        if [
            session_actions::SHOW_PAIR_DEVICE,
            session_actions::SHOW_CONNECT,
        ]
        .iter()
        .any(|sheet| request.target == session_actions::target(sheet))
        {
            self.asked.lock().unwrap().push(request);
            return match &mut *self.sheet.lock().unwrap() {
                Sheet::Opens => ui::tilo_actions::remote::Pending::ready(Ok(opened(invocation))),
                Sheet::Refused => {
                    ui::tilo_actions::remote::Pending::ready(Err(ActionCallError::NotPermitted))
                }
                Sheet::Held(held) => {
                    let (answer, pending) = ui::tilo_actions::remote::pending();
                    held.push((invocation, answer));
                    pending
                }
            };
        }
        let value = if request.target == mcp_server::target(mcp_server::SERVER) {
            let answer = self.served.lock().unwrap().clone();
            let Some(answer) = answer else {
                return ui::tilo_actions::remote::Pending::ready(Err(
                    ActionCallError::UnknownAction,
                ));
            };
            answer
        } else {
            serde_json::json!({})
        };
        let refused = self.refused.lock().unwrap().contains(&request.target);
        self.asked.lock().unwrap().push(request);
        if refused {
            return ui::tilo_actions::remote::Pending::ready(Err(ActionCallError::NotPermitted));
        }
        ui::tilo_actions::remote::Pending::ready(Ok(ui::tilo_actions::remote::InvocationReceipt {
            invocation,
            status: ui::tilo_actions::remote::ReceiptStatus::Completed {
                output: ui::tilo_actions::ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
        }))
    }

    fn observe(
        &self,
        _: &Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionCallError>>
    {
        ui::tilo_actions::remote::Pending::ready(Ok(()))
    }

    fn resource_reads(&self) -> Option<std::sync::Arc<dyn ResourceReads>> {
        Some(self.records.clone())
    }
}

fn key(n: u8) -> String {
    hex::encode([n; 32])
}

fn record(now: i64) -> serde_json::Value {
    let tab = |n: u8| {
        serde_json::json!({"label": "Chrome on Linux", "key_hex": key(n),
            "fingerprint": format!("{n:02X} {n:02X} {n:02X} {n:02X} {n:02X} {n:02X}"),
            "current": false, "revoked": false, "browser": BROWSER,
            "issued_ms": now - 3 * DAY, "last_active_ms": now - 5 * MINUTE})
    };
    serde_json::json!([
        {"label": "Framework 16", "key_hex": THIS_DEVICE, "fingerprint": "11 11 11 11 11 11",
         "current": true, "revoked": false},
        {"label": "Claude Code on laptop", "key_hex": LAPTOP_AGENT,
         "fingerprint": "55 55 55 55 55 55", "current": false, "revoked": false,
         "kind": "agent", "agent": "Claude Code",
         "issued_ms": now - 2 * DAY, "expires_ms": now + 28 * DAY, "online": true},
        tab(0x31), tab(0x32),
    ])
}

fn model_on(host: &std::sync::Arc<RecordingHost>) -> (AccessModel, ui::RemoteActions) {
    host.sign_in();
    let remote = ui::RemoteActions::with_provider(host.clone(), Caller::local_person("owner"));
    let mut model = AccessModel::listed(&remote);
    tilo_test_env::wait_until("the person's devices to be read", || {
        model.poll(&remote);
        model.devices_read.answered
    });
    (model, remote)
}

tilo_test_env::scenario! {
    fn a_device_row_asks_for_the_name_the_person_types;

    phase the_row_binds_its_device_and_asks_for_the_name;
    {
        en();
        let now = i64::try_from(ui::time::unix_now_ms()).unwrap();
        let host = RecordingHost::new(record(now));
        let (model, _) = model_on(&host);
        let agent = &model.agents()[0];
        let row = agent_row(agent, "").into_resource();
        let open = row
            .actions
            .iter()
            .find(|spec| spec.id == "rename-device")
            .expect("the row offers naming its device");
        assert!(
            open.params().is_empty(),
            "the row carries the name it is published with; it asks for nothing"
        );
        // The typed name is the row's too: the app reads the name the row
        // carries, so a press after typing it sends what was typed.
        let typed = agent_row(agent, "Build server").into_resource();
        let open = typed
            .actions
            .iter()
            .find(|spec| spec.id == "rename-device")
            .expect("the row offers naming its device");
        let carried = open
            .target()
            .get::<serde_json::Map<String, serde_json::Value>>()
            .expect("the row is bound to the device and the name");
        assert_eq!(carried["name"], "Build server");
        assert_eq!(carried["device"], LAPTOP_AGENT);
    }
}

tilo_test_env::scenario! {
    fn a_device_is_named_through_the_host;

    phase the_name_the_person_typed_reaches_their_resource;
    {
        en();
        let now = i64::try_from(ui::time::unix_now_ms()).unwrap();
        let host = RecordingHost::new(record(now));
        let (mut model, remote) = model_on(&host);
        let rename = DevicesAccessEffect::RenameDevice {
            device: LAPTOP_AGENT.into(),
            name: "Build server".into(),
        };
        model.act(&remote, &rename, None).unwrap();
        model.poll(&remote);
        assert_eq!(
            host.asked_at(&device_tickets::target(device_tickets::RENAME)),
            [
                serde_json::json!({"device": LAPTOP_AGENT, "name": "Build server"})
                    .as_object()
                    .unwrap()
                    .clone()
            ]
        );
    }

    phase a_name_of_nothing_is_refused_before_it_is_asked;
    {
        en();
        let now = i64::try_from(ui::time::unix_now_ms()).unwrap();
        let host = RecordingHost::new(record(now));
        let (mut model, remote) = model_on(&host);
        let rename = DevicesAccessEffect::RenameDevice {
            device: LAPTOP_AGENT.into(),
            name: "  ".into(),
        };
        assert!(model.act(&remote, &rename, None).is_err());
        assert!(
            host.asked_at(&device_tickets::target(device_tickets::RENAME))
                .is_empty(),
            "nothing is asked for a device the person gave no name"
        );
    }
}

tilo_test_env::scenario! {
    fn the_access_list_is_read_and_changed_through_the_host;

    phase an_agent_signed_in_as_the_person_is_listed_with_what_it_may_do;
    {
        en();
        let now = i64::try_from(ui::time::unix_now_ms()).unwrap();
        let host = RecordingHost::new(record(now));
        let (model, _) = model_on(&host);
        let agents = model.agents();
        assert_eq!(agents.len(), 1, "the agent, and nothing else");
        let agent = &agents[0];
        assert_eq!(agent.label(), "Claude Code on laptop");
        assert_eq!(
            plain_all(agent_lines(agent, model.now())),
            ["In all your workspaces", "Active now"]
        );
        assert_eq!(
            agent.expires(),
            Some(now + 28 * DAY),
            "its Details say when it ends"
        );
        assert!(
            model.entries().iter().all(|entry| match entry {
                Entry::Device(device) => device.agent.is_none(),
                Entry::Browser(_) => true,
            }),
            "an agent's key is not a computer"
        );
        assert_eq!(model.list_note(), None, "the lists read");
    }

    phase removing_an_agent_revokes_its_key_and_its_row_follows_the_record;
    {
        en();
        let host = RecordingHost::new(record(0));
        let (mut model, remote) = model_on(&host);
        let removal = model.agents()[0].removal();
        assert_eq!(
            removal,
            DevicesAccessEffect::RevokeDevice {
                device: LAPTOP_AGENT.into(),
                reason: "retired".into(),
            }
        );
        model.act(&remote, &removal, None).unwrap();
        model.poll(&remote);
        let revoked = host.asked_at(&device_tickets::target(device_tickets::REVOKE));
        assert_eq!(
            revoked,
            [
                serde_json::json!({"device": LAPTOP_AGENT, "reason": "retired"})
                    .as_object()
                    .unwrap()
                    .clone()
            ]
        );
        let revoke = host
            .asked()
            .into_iter()
            .find(|request| request.target == device_tickets::target(device_tickets::REVOKE))
            .unwrap();
        assert_eq!(revoke.confirmation, Some(Confirmation::Confirmed));
        host.change(|listed| {
            for device in listed.as_array_mut().unwrap() {
                if device["key_hex"] == LAPTOP_AGENT {
                    device["revoked"] = true.into();
                }
            }
        });
        tilo_test_env::wait_until("the agent to leave the list", || {
            model.poll(&remote);
            model.agents().is_empty()
        });
    }

    phase a_browsers_tabs_are_one_entry_removed_together;
    {
        en();
        let now = i64::try_from(ui::time::unix_now_ms()).unwrap();
        let host = RecordingHost::new(record(now));
        let (mut model, remote) = model_on(&host);
        let entries = model.entries();
        assert_eq!(entries.len(), 2, "this computer, and the browser");
        assert!(
            matches!(&entries[0], Entry::Device(device) if device.current),
            "this computer comes first"
        );
        let Entry::Browser(browser) = &entries[1] else {
            panic!("the browser is one entry");
        };
        assert_eq!(browser.label(), "Chrome on Linux");
        assert_eq!(
            plain_all(browser_lines(browser, model.now())),
            ["2 tabs", "Last active 5 min ago"]
        );
        assert!(browser.revocable());
        let Entry::Device(this) = &entries[0] else {
            panic!("this computer comes first");
        };
        assert_eq!(
            plain_all(device_lines(this, model.now())),
            ["Framework 16", "Active now"]
        );
        drop(entries);

        model
            .act(
                &remote,
                &DevicesAccessEffect::RevokeBrowser {
                    browser: BROWSER.into(),
                    reason: "retired".into(),
                },
                None,
            )
            .unwrap();
        model.poll(&remote);
        let revoked: Vec<serde_json::Value> = host
            .asked_at(&device_tickets::target(device_tickets::REVOKE))
            .into_iter()
            .map(|argument| argument["device"].clone())
            .collect();
        assert_eq!(revoked, [key(0x31), key(0x32)], "every tab of it");
    }

    phase this_browser_comes_first_and_is_removed_from_elsewhere;
    {
        en();
        let mut listed = record(0);
        let devices = listed.as_array_mut().unwrap();
        devices[0]["current"] = false.into();
        devices[3]["current"] = true.into();
        let host = RecordingHost::new(listed);
        let (model, remote) = model_on(&host);
        let entries = model.entries();
        let Entry::Browser(browser) = &entries[0] else {
            panic!("this browser comes first");
        };
        assert_eq!(browser.label(), "This browser");
        assert_eq!(
            plain_all(browser_lines(browser, model.now())),
            ["Chrome on Linux with 2 tabs", "Active now"]
        );
        assert!(!browser.revocable());
        drop(entries);
        let mut model = model;
        assert!(
            model
                .act(
                    &remote,
                    &DevicesAccessEffect::RevokeBrowser {
                        browser: BROWSER.into(),
                        reason: "retired".into(),
                    },
                    None,
                )
                .is_err(),
            "nor by its effect"
        );
    }

    phase connecting_asks_the_shell_for_its_setup;
    {
        en();
        let host = RecordingHost::new(record(0));
        let (mut model, remote) = model_on(&host);
        model
            .act(&remote, &DevicesAccessEffect::ShowConnect, None)
            .unwrap();
        assert_eq!(
            host.asked_at(&session_actions::target(session_actions::SHOW_CONNECT))
                .len(),
            1
        );
    }
}

tilo_test_env::scenario! {
    fn the_access_rows_join_grants_and_offer_what_can_be_done;

    phase the_apis_grants_join_the_resources_agents;
    {
        en();
        let mut sample = sample();
        sample.api.grants.push(Grant {
            id: hex::encode([5u8; 32]),
            agent: "Claude Code".into(),
            reach: Reach::Account,
            granted_ms: 0,
            last_ms: None,
            sessions: Vec::new(),
            browser_pages: true,
        });
        let model = AccessModel::shown(sample);
        let agents = model.agents();
        let labels: Vec<String> = agents.iter().map(AgentEntry::label).collect();
        assert_eq!(
            labels,
            ["Nightly export", "Claude Code on laptop", "ChatGPT"],
            "active now first, the latest active first"
        );
        assert!(agents[1].grant.is_some(), "the API's grant joins its key");
        assert_eq!(
            agents[2].removal(),
            DevicesAccessEffect::RevokeDevice {
                device: "bb".repeat(32),
                reason: "retired".into(),
            },
            "an agent's key is taken back as every key of the person's is"
        );
        assert_eq!(plain(&agent_may(&agents[2])), "Only in Launch plan");
    }

    phase the_staged_rows_pass_the_hosts_check_and_offer_what_can_be_done;
    // Read from the window that publishes them, after the check the host
    // makes of a surface. A capture opens without that check, so it is made
    // here: rows counted beside the app carried one address four times.
    {
        en();
        let mut app = TestApp::<SettingsShell>::capture(Some("devices_access"))
            .expect("the staged page opens");
        let violations = app.harness().component_contract_violations();
        assert!(violations.is_empty(), "the host would refuse: {violations:?}");

        let phone = published_row(&mut app, &format!("settings.device:{}", key(2)))
            .expect("the phone is published");
        assert_eq!(
            verbs(&phone),
            ["rename-device", "revoke-device"],
            "naming it, and taking it back"
        );
        assert_eq!(asks(offered(&phone, "revoke-device")), [reason_asked()]);
        let browser = published_row(&mut app, &format!("settings.device-browser:{BROWSER}"))
            .expect("the browser is published");
        assert_eq!(verbs(&browser), ["revoke-browser"]);
        assert_eq!(asks(offered(&browser, "revoke-browser")), [reason_asked()]);
        assert!(
            published_row(&mut app, &format!("settings.device:{}", key(4))).is_none(),
            "the removed phone is gone"
        );
        assert!(
            published_row(&mut app, &format!("settings.device:{}", key(1))).is_none(),
            "this computer is taken back from another device, not from here"
        );

        let nightly = published_row(
            &mut app,
            &format!("settings.agent-grant:{}", "cc".repeat(32)),
        )
        .expect("the script's key is published");
        assert_eq!(
            verbs(&nightly),
            ["rename-device", "revoke-device", "stop-session"],
            "name it, take it back as any key, and stop its open session"
        );
        assert_eq!(
            asks(offered(&nightly, "stop-session")),
            [("session".to_owned(), vec!["Nightly export #12".to_owned()])]
        );
    }
}

/// This device and an agent's key, as the host lists them: rows that each
/// offer an action once. A browser's row and another device's offer taking
/// it back once per reason under one address, which the test app refuses to
/// open on, as the host refuses to publish it.
fn this_device_and_an_agent() -> serde_json::Value {
    let mut listed = record(0);
    listed.as_array_mut().unwrap().truncate(2);
    listed
}

/// Settings as a person has it open on Devices and access, with `host`
/// answering what the page asks of the session.
fn window_on(host: &std::sync::Arc<RecordingHost>) -> TestApp<SettingsShell> {
    en();
    host.sign_in();
    let served = host.clone();
    crate::area::replace_in_next_window(move |context| {
        DevicesAccessArea::new(
            context,
            ui::RemoteActions::with_provider(served, Caller::local_person("owner")),
        )
    });
    let mut app = TestApp::<SettingsShell>::new().expect("Settings opens");
    let pane = crate::categories::Category::DevicesAccess;
    app.effect_on(
        &crate::effects::pane_row_id(pane),
        &crate::effects::open_pane(pane),
    )
    .expect("Devices and access opens");
    app.settle();
    app
}

/// The two buttons that bring a device or an agent in: the button, what it
/// says, the sheet of the shell it asks for, and its effect.
fn sheets() -> [(
    &'static str,
    &'static str,
    &'static str,
    DevicesAccessEffect,
); 2] {
    [
        (
            "settings.access.add-device",
            "Add a device",
            session_actions::SHOW_PAIR_DEVICE,
            DevicesAccessEffect::ShowPairDevice,
        ),
        (
            "settings.access.connect",
            "Connect an agent or computer",
            session_actions::SHOW_CONNECT,
            DevicesAccessEffect::ShowConnect,
        ),
    ]
}

fn published(app: &mut TestApp<SettingsShell>, id: &str) -> ui::tilo_actions::ActionSpec {
    fn find(
        resources: &[ui::tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .cloned()
                .or_else(|| find(&resource.children, id))
        })
    }
    find(&app.harness().resources(), id).expect("Settings publishes the effect")
}

tilo_test_env::scenario! {
    fn adding_a_device_or_an_agent_asks_the_shell_for_its_sheet;

    phase a_press_on_each_button_is_carried_out_not_refused;
    // The press goes the way a person's does: the button's bound effect
    // through the app's dispatch, where the framework holds the handler to
    // what the effect declares.
    {
        for (button, _, sheet, _) in sheets() {
            let host = RecordingHost::new(this_device_and_an_agent());
            let mut app = window_on(&host);
            app.harness()
                .click_named(button)
                .expect("the button is on the page");
            app.settle();
            assert_eq!(app.harness().refusal_said(), None, "{button}");
            assert!(
                app.harness().control_failures().is_empty(),
                "{button}: {:?}",
                app.harness().control_failures()
            );
            assert_eq!(
                host.asked_at(&session_actions::target(sheet)).len(),
                1,
                "{button} asks the shell for its sheet once"
            );
        }
    }

    phase a_second_press_in_the_same_page_asks_the_shell_again;
    // A person who closed the sheet, or added one device and wants to add
    // another, presses the same button again without loading the page anew:
    // on a page whose lists are followed.
    {
        for (button, _, sheet, _) in sheets() {
            {
                let shell = a_full_list(now());
                let mut app = window_on_a_full_list(&shell);
                for press in 1..=3 {
                    app.harness()
                        .click_named(button)
                        .expect("the button is on the page");
                    app.settle();
                    assert_eq!(
                        app.harness().refusal_said(),
                        None,
                        "{button}, press {press}"
                    );
                    assert!(
                        app.harness().control_failures().is_empty(),
                        "{button}, press {press}: {:?}",
                        app.harness().control_failures()
                    );
                    assert_eq!(
                        shell.asked_at(&session_actions::target(sheet)).len(),
                        press,
                        "{button}: press {press} asks for its sheet"
                    );
                }
            }
        }
    }

    phase the_caller_hears_once_the_shell_has_answered;
    {
        for (_, _, sheet, effect) in sheets() {
            let host = RecordingHost::new(this_device_and_an_agent());
            host.shell(Sheet::Held(Vec::new()));
            let mut app = window_on(&host);
            app.start_effect(&SettingsEffect::from(effect))
                .expect("it answers later, as the job it declares");
            app.settle();
            assert_eq!(host.asked_at(&session_actions::target(sheet)).len(), 1);
            assert!(
                !app.effect_progress()
                    .iter()
                    .any(ui::tilo_actions::InvocationUpdate::is_terminal),
                "nothing is answered before the shell has"
            );
            host.open_held();
            tilo_test_env::wait_until("the shell's answer to reach the caller", || {
                app.settle();
                app.effect_progress().iter().any(|update| {
                    matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. })
                })
            });
        }
    }

    phase the_shells_refusal_reaches_whoever_asked;
    {
        for (_, _, sheet, effect) in sheets() {
            let host = RecordingHost::new(this_device_and_an_agent());
            host.shell(Sheet::Refused);
            let mut app = window_on(&host);
            let refused = app
                .effect(&SettingsEffect::from(effect))
                .expect_err("the shell refused");
            assert_eq!(host.asked_at(&session_actions::target(sheet)).len(), 1);
            assert!(
                matches!(
                    &refused,
                    ui::tilo_actions::ApiError::Failed(why)
                        if *why == ActionCallError::NotPermitted.to_string()
                ),
                "{refused:?}"
            );
        }
    }

    phase a_caller_that_gives_up_is_told_so_and_a_late_sheet_changes_nothing;
    {
        for (_, _, _, effect) in sheets() {
            let host = RecordingHost::new(this_device_and_an_agent());
            host.shell(Sheet::Held(Vec::new()));
            let mut app = window_on(&host);
            let invocation = app
                .start_effect(&SettingsEffect::from(effect))
                .expect("it answers later, as the job it declares");
            app.settle();
            assert!(
                matches!(
                    app.cancel_effect(invocation),
                    Some(ui::tilo_actions::InvocationUpdate::Cancelled { .. })
                ),
                "the caller that gave up is told it was cancelled"
            );
            host.open_held();
            app.settle();
            assert!(
                !app.effect_progress().iter().any(|update| {
                    matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. })
                }),
                "an answer nobody waits for is told to nobody"
            );
        }
    }

    phase who_may_run_them_and_what_they_read_as_are_a_views;
    // Each was a view: offered to the person at this device alone, needing
    // no more of a workspace than reading does, asking no confirmation,
    // harmless to repeat. Answering later changes none of that.
    {
        let host = RecordingHost::new(this_device_and_an_agent());
        let mut app = window_on(&host);
        for (_, label, _, effect) in sheets() {
            let spec = published(&mut app, effect.id());
            assert_eq!(plain(&spec.label), label);
            assert!(
                spec.contract.runs_on_invokers_device(),
                "{label}: only the person at this device, and those acting for them"
            );
            assert!(
                matches!(spec.contract.need().kind, ui::tilo_actions::ActionKind::Read),
                "{label}: a workspace's grants have nothing more to allow than reading"
            );
            assert!(
                matches!(
                    spec.contract.confirmation,
                    ui::tilo_actions::ConfirmationPolicy::None
                ),
                "{label}"
            );
            assert!(
                matches!(spec.contract.retry, ui::tilo_actions::RetryPolicy::Idempotent),
                "{label}: asking again shows the same sheet"
            );
            assert!(
                matches!(
                    spec.contract.execution,
                    ui::tilo_actions::ExecutionMode::Background { .. }
                ),
                "{label}: the shell answers later"
            );
        }
    }
}

const OTHER_COMPUTER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const FIRST_SESSION: &str = "Claude Code #1";
const SECOND_SESSION: &str = "Claude Code #2";

/// What a person with more than one of everything has listed: this computer
/// and another, a browser with two tabs, and an agent with two sessions open.
fn a_full_list(now: i64) -> std::sync::Arc<RecordingHost> {
    let mut listed = record(now);
    listed.as_array_mut().unwrap().push(serde_json::json!({
        "label": "MacBook Air", "key_hex": OTHER_COMPUTER,
        "fingerprint": "22 22 22 22 22 22", "current": false, "revoked": false,
        "issued_ms": now - 9 * DAY, "last_active_ms": now - 2 * DAY}));
    let host = RecordingHost::new(listed);
    *host.served.lock().unwrap() = Some(serde_json::json!({
        "endpoint": "https://mcp.tilo.sh",
        "grants": [{"id": LAPTOP_AGENT, "agent": "Claude Code", "device": "laptop",
            "granted_ms": now - 2 * DAY, "sessions": [
                {"name": FIRST_SESSION, "started_ms": now - 9 * MINUTE},
                {"name": SECOND_SESSION, "started_ms": now - 2 * MINUTE},
            ]}],
    }));
    host
}

fn computer_row_id() -> String {
    format!("settings.device:{OTHER_COMPUTER}")
}

fn browser_row_id() -> String {
    format!("settings.device-browser:{BROWSER}")
}

fn agent_row_id() -> String {
    format!("settings.agent-grant:{LAPTOP_AGENT}")
}

fn revoke() -> ui::tilo_actions::remote::RemoteActionTarget {
    device_tickets::target(device_tickets::REVOKE)
}

fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(object) => object,
        other => panic!("an object, not {other}"),
    }
}

/// The row `id` as the window publishes it now, in the tree a host reads.
fn published_row(
    app: &mut TestApp<SettingsShell>,
    id: &str,
) -> Option<ui::tilo_actions::ResourceSpec> {
    fn find(
        resources: &[ui::tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<ui::tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(&app.harness().resources(), id)
}

/// The verbs `row` offers, in order, each under an address of its own: one
/// address offered twice is what the host refuses.
fn verbs(row: &ui::tilo_actions::ResourceSpec) -> Vec<&str> {
    let verbs: Vec<&str> = row.actions.iter().map(|spec| spec.id.as_str()).collect();
    for verb in &verbs {
        assert_eq!(
            verbs.iter().filter(|other| other == &verb).count(),
            1,
            "{} offers `{verb}` once",
            row.id.as_str()
        );
    }
    verbs
}

fn offered<'a>(
    row: &'a ui::tilo_actions::ResourceSpec,
    verb: &str,
) -> &'a ui::tilo_actions::ActionSpec {
    row.actions
        .iter()
        .find(|spec| spec.id == verb)
        .unwrap_or_else(|| panic!("{} offers `{verb}`", row.id.as_str()))
}

/// What `spec` asks its caller for: each parameter, with the values it takes
/// when they are a list. Each is required: the row binds everything else.
fn asks(spec: &ui::tilo_actions::ActionSpec) -> Vec<(String, Vec<String>)> {
    spec.params()
        .iter()
        .map(|param| {
            assert!(param.required, "`{}` asks for `{}`", spec.id, param.name);
            (
                param.name.to_string(),
                param.choices.iter().map(ToString::to_string).collect(),
            )
        })
        .collect()
}

/// Why a device or a browser is taken back, as its one action asks.
fn reason_asked() -> (String, Vec<String>) {
    (
        "reason".to_owned(),
        REVOKE_REASONS.iter().map(ToString::to_string).collect(),
    )
}

/// Settings open on Devices and access with everything of [`a_full_list`]
/// read. Opening is itself the check: the test app does not open on a
/// surface the host would refuse.
fn window_on_a_full_list(host: &std::sync::Arc<RecordingHost>) -> TestApp<SettingsShell> {
    let mut app = window_on(host);
    tilo_test_env::wait_until("every list of the page to be read", || {
        app.settle();
        [computer_row_id(), browser_row_id()]
            .iter()
            .all(|id| published_row(&mut app, id).is_some())
            && published_row(&mut app, &agent_row_id())
                .is_some_and(|row| row.actions.iter().any(|spec| spec.id == "stop-session"))
    });
    app
}

/// A person's click on the control named `name`, and the frames after it.
fn press(app: &mut TestApp<SettingsShell>, name: &str) {
    app.harness()
        .click_named(name)
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    app.settle();
}

/// What the host was asked at `target`, once it was asked anything there.
fn asked_at_last(
    app: &mut TestApp<SettingsShell>,
    host: &RecordingHost,
    target: &ui::tilo_actions::remote::RemoteActionTarget,
) -> Vec<serde_json::Map<String, serde_json::Value>> {
    tilo_test_env::wait_until("the host to be asked", || {
        app.settle();
        !host.asked_at(target).is_empty()
    });
    host.asked_at(target)
}

/// The person agrees to what the host asks before something is taken back.
/// A control bound to such an action never confirms it itself: the host
/// asks about that row's action, and until the person agreed the host that
/// keeps their devices has heard nothing.
fn agree(app: &mut TestApp<SettingsShell>, host: &RecordingHost, row: &str, verb: &str) {
    let (asked, _) = app.asking().expect("taking it back asks first");
    assert_eq!(
        (asked.resource.as_str(), asked.action.as_str()),
        (row, verb)
    );
    assert!(
        host.asked_at(&revoke()).is_empty(),
        "nothing is taken back before the person agreed"
    );
    assert_eq!(app.confirm_asked(), Some(asked));
}

fn now() -> i64 {
    i64::try_from(ui::time::unix_now_ms()).unwrap()
}

tilo_test_env::scenario! {
    fn the_rows_of_devices_and_access_are_a_surface_the_host_accepts;

    phase settings_opens_on_a_person_with_more_than_one_of_everything;
    // Another computer, a browser with two tabs, and an agent with two
    // sessions open: each put one address on its row
    // several times, and the host withdrew every action of Settings for it.
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        let violations = app.harness().component_contract_violations();
        assert!(violations.is_empty(), "the host would refuse: {violations:?}");
    }

    phase each_row_offers_each_verb_once_and_asks_for_what_differed;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);

        let computer = published_row(&mut app, &computer_row_id()).unwrap();
        assert_eq!(verbs(&computer), ["rename-device", "revoke-device"]);
        assert_eq!(asks(offered(&computer, "revoke-device")), [reason_asked()]);
        assert!(
            asks(offered(&computer, "rename-device")).is_empty(),
            "the name is the row's, as the person types it"
        );

        let browser = published_row(&mut app, &browser_row_id()).unwrap();
        assert_eq!(verbs(&browser), ["revoke-browser"]);
        assert_eq!(asks(offered(&browser, "revoke-browser")), [reason_asked()]);

        let agent = published_row(&mut app, &agent_row_id()).unwrap();
        assert_eq!(
            verbs(&agent),
            ["rename-device", "revoke-device", "stop-session"],
            "a row shows its type's own verbs first, then the ones Settings declares for it"
        );
        assert_eq!(
            asks(offered(&agent, "stop-session")),
            [(
                "session".to_owned(),
                vec![FIRST_SESSION.to_owned(), SECOND_SESSION.to_owned()]
            )],
            "the sessions open now, by the names History gives them"
        );
    }

    phase taking_back_keeps_the_contract_it_was_declared_with;
    // What a caller needs and what it is asked first are the declaration's,
    // whether the row binds the reason or asks for it.
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        for (row, effect) in [
            (
                computer_row_id(),
                DevicesAccessEffect::RevokeDevice {
                    device: OTHER_COMPUTER.into(),
                    reason: REMOVED.into(),
                },
            ),
            (
                browser_row_id(),
                DevicesAccessEffect::RevokeBrowser {
                    browser: BROWSER.into(),
                    reason: REMOVED.into(),
                },
            ),
        ] {
            let declared = SettingsEffect::from(effect).bound();
            let row = published_row(&mut app, &row).unwrap();
            let spec = offered(&row, declared.id.as_str());
            assert_eq!(spec.contract, declared.contract, "{}", spec.id);
            assert_eq!(
                spec.contract.confirmation,
                ui::tilo_actions::ConfirmationPolicy::Confirm,
                "{}: it asks first",
                spec.id
            );
            assert_eq!(
                spec.contract.risk,
                ui::tilo_actions::RiskLevel::Destructive,
                "{}",
                spec.id
            );
        }
    }
}

tilo_test_env::scenario! {
    fn a_press_on_the_page_reaches_the_rows_one_action_with_its_argument;

    phase stopping_one_of_two_sessions_stops_that_one;
    {
        for session in [SECOND_SESSION, FIRST_SESSION] {
            let host = a_full_list(now());
            let mut app = window_on_a_full_list(&host);
            // The sessions are in the row's details.
            press(&mut app, &format!("settings.access.agent.{LAPTOP_AGENT}"));
            press(
                &mut app,
                &format!("settings.access.agent.{LAPTOP_AGENT}.stop.{session}"),
            );
            assert_eq!(
                asked_at_last(&mut app, &host, &mcp_server::target(mcp_server::STOP)),
                [object(serde_json::json!({"grant": LAPTOP_AGENT, "session": session}))],
                "{session}"
            );
        }
    }

    phase removing_another_computer_takes_it_back_as_retired_once_confirmed;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.ask-remove"),
        );
        assert!(
            host.asked_at(&revoke()).is_empty(),
            "the first press only says what removing does"
        );
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.remove"),
        );
        agree(&mut app, &host, &computer_row_id(), "revoke-device");
        assert_eq!(
            asked_at_last(&mut app, &host, &revoke()),
            [object(serde_json::json!({"device": OTHER_COMPUTER, "reason": "retired"}))]
        );
        let sent = host
            .asked()
            .into_iter()
            .find(|request| request.target == revoke())
            .unwrap();
        assert_eq!(sent.confirmation, Some(Confirmation::Confirmed));
    }

    phase removing_a_browser_takes_every_tab_of_it_back;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        press(&mut app, &format!("settings.access.browser.{BROWSER}.ask-remove"));
        press(&mut app, &format!("settings.access.browser.{BROWSER}.remove"));
        agree(&mut app, &host, &browser_row_id(), "revoke-browser");
        tilo_test_env::wait_until("both tabs to be taken back", || {
            app.settle();
            host.asked_at(&revoke()).len() == 2
        });
        assert_eq!(
            host.asked_at(&revoke()),
            [
                object(serde_json::json!({"device": key(0x31), "reason": "retired"})),
                object(serde_json::json!({"device": key(0x32), "reason": "retired"})),
            ]
        );
    }
}

tilo_test_env::scenario! {
    fn a_caller_names_the_argument_and_a_denial_stays_a_denial;

    phase the_search_or_an_agent_says_why_a_device_is_taken_back;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        app.call_confirmed_on(
            &computer_row_id(),
            "revoke-device",
            serde_json::json!({"reason": "stolen"}),
        )
        .expect("a reason of the choices, confirmed, is carried out");
        assert_eq!(
            asked_at_last(&mut app, &host, &revoke()),
            [object(serde_json::json!({"device": OTHER_COMPUTER, "reason": "stolen"}))]
        );
    }

    phase an_argument_the_action_does_not_take_is_refused_before_the_host_hears;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        for arguments in [
            // Not one of the reasons.
            serde_json::json!({"reason": "bored"}),
            // No reason at all: the row holds none for its caller.
            serde_json::json!({}),
            // The device is the row's, never its caller's.
            serde_json::json!({"reason": "lost", "device": THIS_DEVICE}),
        ] {
            let refused = app.call_confirmed_on(
                &computer_row_id(),
                "revoke-device",
                arguments.clone(),
            );
            assert!(refused.is_err(), "{arguments}: {refused:?}");
        }
        // A session that is not one of the agent's open ones.
        let stranger = app.call_on(
            &agent_row_id(),
            "stop-session",
            serde_json::json!({"session": "Codex #7"}),
        );
        assert!(stranger.is_err(), "{stranger:?}");
        app.settle();
        for target in [revoke(), mcp_server::target(mcp_server::STOP)] {
            assert!(host.asked_at(&target).is_empty(), "{target:?}");
        }
    }

    phase without_its_confirmation_nothing_is_taken_back;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        for (row, verb) in [
            (computer_row_id(), "revoke-device"),
            (browser_row_id(), "revoke-browser"),
        ] {
            let refused = app.call_on(&row, verb, serde_json::json!({"reason": "lost"}));
            assert_eq!(
                refused,
                Err(ui::tilo_actions::ApiError::ConfirmationRequired),
                "{verb}"
            );
        }
        app.settle();
        assert!(host.asked_at(&revoke()).is_empty());
    }

    phase a_member_who_may_only_read_takes_nothing_back;
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        app.set_role(ui::permissions::Role::Viewer);
        app.settle();
        for (row, verb) in [
            (computer_row_id(), "revoke-device"),
            (browser_row_id(), "revoke-browser"),
        ] {
            let refused =
                app.call_confirmed_on(&row, verb, serde_json::json!({"reason": "lost"}));
            assert!(refused.is_err(), "{verb}: {refused:?}");
        }
        app.settle();
        assert!(
            host.asked_at(&revoke()).is_empty(),
            "what a member may not do reaches no host"
        );
    }

    phase the_hosts_refusal_reaches_the_person_who_pressed;
    {
        let host = a_full_list(now());
        host.refused.lock().unwrap().push(revoke());
        let mut app = window_on_a_full_list(&host);
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.ask-remove"),
        );
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.remove"),
        );
        agree(&mut app, &host, &computer_row_id(), "revoke-device");
        tilo_test_env::wait_until("the host's refusal to be told", || {
            app.settle();
            !app.harness().control_failures().is_empty()
                || app.harness().refusal_said().is_some()
        });
        let failed = format!("{:?}", app.harness().control_failures());
        let said = format!("{:?}", app.harness().refusal_said());
        let told = format!("{failed} {said}");
        assert!(
            told.contains(&ActionCallError::NotPermitted.to_string()),
            "{told}"
        );
        assert_eq!(
            host.asked_at(&revoke()).len(),
            1,
            "asked once, and not again after the refusal"
        );
        assert!(
            published_row(&mut app, &computer_row_id()).is_some(),
            "the computer is still one of the person's"
        );
    }
}

/// How often the page asked for the record of the person's devices.
fn times_listed(host: &RecordingHost) -> usize {
    host.records
        .devices_asked
        .load(std::sync::atomic::Ordering::SeqCst)
}

/// The controls the page offers for the entry `key` now, by name.
fn offered_for(app: &mut TestApp<SettingsShell>, key: &str) -> Vec<String> {
    let mut offered: Vec<String> = app
        .harness()
        .targets()
        .into_keys()
        .filter(|name| name.contains(key))
        .collect();
    offered.sort();
    offered
}

/// Whether a row of the page says it is being removed.
fn says_removing(app: &mut TestApp<SettingsShell>) -> bool {
    plain_all(app.texts())
        .iter()
        .any(|text| text.contains("Removing…"))
}

/// The person removes the other computer: Remove on its row, Remove again
/// where the row says what that does, and yes to what the host asks. The
/// host has taken the removal when this returns.
fn remove_the_other_computer(app: &mut TestApp<SettingsShell>, host: &RecordingHost) {
    press(
        app,
        &format!("settings.access.device.{OTHER_COMPUTER}.ask-remove"),
    );
    press(
        app,
        &format!("settings.access.device.{OTHER_COMPUTER}.remove"),
    );
    agree(app, host, &computer_row_id(), "revoke-device");
    assert_eq!(asked_at_last(app, host, &revoke()).len(), 1);
    app.settle();
}

/// The host lists the other computer as taken back from now on.
fn the_other_computer_is_revoked(listed: &mut serde_json::Value) {
    for device in listed.as_array_mut().unwrap() {
        if device["key_hex"] == OTHER_COMPUTER {
            device["revoked"] = true.into();
        }
    }
}

tilo_test_env::scenario! {
    fn the_page_shows_the_persons_devices_as_they_are;

    phase a_device_approved_while_the_page_is_open_is_listed_and_the_host_is_asked_once;
    // The list was read when the page opened and after what the person did
    // on it: a device approved on the shell's own card, or on another
    // device, was listed only by a page loaded again.
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        let phone = format!("settings.device:{}", key(0x77));
        assert!(published_row(&mut app, &phone).is_none());
        host.change(|listed| {
            listed.as_array_mut().unwrap().push(serde_json::json!({
                "label": "Browser on iPhone", "key_hex": key(0x77),
                "fingerprint": "77 77 77 77 77 77", "current": false, "revoked": false,
                "issued_ms": now(), "online": true}));
        });
        tilo_test_env::wait_until("the new device to be listed", || {
            app.settle();
            published_row(&mut app, &phone).is_some()
        });
        let shown = plain_all(app.texts());
        assert!(
            shown.iter().any(|text| text.contains("Browser on iPhone")),
            "{shown:?}"
        );
        assert_eq!(
            times_listed(&host),
            1,
            "read once and followed, never asked for again"
        );
    }

    phase the_followed_record_is_asked_for_on_no_timer;
    {
        en();
        let host = a_full_list(0);
        let (model, _remote) = model_on(&host);
        assert_eq!(model.wakes_in(), None, "nothing wakes the page to ask again");
    }

    phase a_removed_device_says_so_until_the_list_shows_it_gone;
    // The host took the removal and its list named the device a moment
    // longer: the row stayed, still offering Cancel and Remove, so the
    // removal read as one that failed.
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        remove_the_other_computer(&mut app, &host);
        let offered = offered_for(&mut app, OTHER_COMPUTER);
        assert!(
            !offered.iter().any(|name| {
                name.ends_with(".cancel")
                    || name.ends_with(".remove")
                    || name.ends_with(".ask-remove")
            }),
            "a row being removed offers no removal: {offered:?}"
        );
        assert!(says_removing(&mut app));
        assert!(
            published_row(&mut app, &computer_row_id()).is_some(),
            "the host's list still names it"
        );

        host.change(the_other_computer_is_revoked);
        tilo_test_env::wait_until("the removed computer to leave the list", || {
            app.settle();
            published_row(&mut app, &computer_row_id()).is_none()
        });
        assert!(!says_removing(&mut app));
        assert!(offered_for(&mut app, OTHER_COMPUTER).is_empty());
        assert_eq!(times_listed(&host), 1, "the record said so itself");
    }

    phase a_removal_the_host_refuses_leaves_the_row_as_it_was;
    {
        let host = a_full_list(now());
        host.refused.lock().unwrap().push(revoke());
        let mut app = window_on_a_full_list(&host);
        remove_the_other_computer(&mut app, &host);
        tilo_test_env::wait_until("the host's refusal to be told", || {
            app.settle();
            !app.harness().control_failures().is_empty()
                || app.harness().refusal_said().is_some()
        });
        app.settle();
        assert!(!says_removing(&mut app), "nothing is being removed");
        let offered = offered_for(&mut app, OTHER_COMPUTER);
        assert!(
            offered.iter().any(|name| name.ends_with(".ask-remove")),
            "the row offers Remove again: {offered:?}"
        );
        assert!(published_row(&mut app, &computer_row_id()).is_some());
    }

    phase the_question_before_a_removal_speaks_to_the_person;
    // The sheet that asks first read what the action says of itself to
    // whoever looks it up: "End every tab's access of one browser of the
    // person's, for good."
    {
        let host = a_full_list(now());
        let mut app = window_on_a_full_list(&host);
        for (row, verb, named, told) in [
            (
                computer_row_id(),
                "revoke-device",
                "Remove device",
                "It's signed out and can't act as you anymore.",
            ),
            (
                browser_row_id(),
                "revoke-browser",
                "Remove browser",
                "It's signed out and can't act as you anymore.",
            ),
            (
                agent_row_id(),
                "revoke-device",
                "Remove device",
                "It stops acting as you right away.",
            ),
        ] {
            let row = published_row(&mut app, &row).unwrap();
            let spec = offered(&row, verb);
            assert_eq!(plain(&spec.label), named, "{verb}");
            assert_eq!(
                spec.description.as_deref().map(plain),
                Some(told.to_owned()),
                "{verb}"
            );
        }
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.ask-remove"),
        );
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.remove"),
        );
        let (_, question) = app.asking().expect("removing asks first");
        assert!(
            plain(&question).contains("Remove device"),
            "the sheet names what the row's button does: {question}"
        );
    }

    phase a_row_that_asks_before_removing_keeps_its_name;
    // Cancel and Remove filled the row from its leading edge: the row's
    // name and what removing does were gone under them, and the person was
    // asked to remove something the row no longer named.
    {
        let mut app = window_on_a_full_list(&a_full_list(now()));
        press(
            &mut app,
            &format!("settings.access.device.{OTHER_COMPUTER}.ask-remove"),
        );
        let targets = app.harness().targets();
        let placed = |name: String| {
            *targets
                .get(&name)
                .unwrap_or_else(|| panic!("the page shows `{name}`"))
        };
        let row = placed(format!("settings.access.device.{OTHER_COMPUTER}"));
        let cancel = placed(format!("settings.access.device.{OTHER_COMPUTER}.cancel"));
        let remove = placed(format!("settings.access.device.{OTHER_COMPUTER}.remove"));
        assert!(
            cancel.x0 > row.x0 + row.width() / 2.0,
            "the row's name keeps its leading half: Cancel at {cancel:?} in the row at {row:?}"
        );
        assert!(
            cancel.x1 <= remove.x0,
            "Cancel, then Remove: {cancel:?}, {remove:?}"
        );
        assert!(
            remove.x1 <= row.x1,
            "both at the row's trailing edge: {remove:?} in the row at {row:?}"
        );
        let shown = plain_all(app.texts());
        assert!(
            shown.iter().any(|text| {
                text.contains("MacBook Air")
                    && text.contains("It's signed out and can't act as you anymore.")
            }),
            "the row still names the computer and says what removing does: {shown:?}"
        );
    }

    phase a_removal_the_host_took_and_never_showed_gone_stops_saying_so;
    {
        en();
        let host = a_full_list(0);
        let (mut model, remote) = model_on(&host);
        model.now_ms = Some(1_000);
        model
            .act(
                &remote,
                &DevicesAccessEffect::RevokeDevice {
                    device: OTHER_COMPUTER.into(),
                    reason: REMOVED.into(),
                },
                None,
            )
            .unwrap();
        assert!(model.leaves(OTHER_COMPUTER));
        model.poll(&remote);
        assert!(
            model.leaves(OTHER_COMPUTER),
            "the host took it: until its list shows it gone"
        );
        assert_eq!(model.wakes_in(), Some(10_000));
        model.now_ms = Some(11_000);
        assert!(model.poll(&remote), "the page is built again");
        assert!(
            !model.leaves(OTHER_COMPUTER),
            "the row shows what the list says"
        );
        assert_eq!(times_listed(&host), 1, "and the record is not asked for again");
    }
}

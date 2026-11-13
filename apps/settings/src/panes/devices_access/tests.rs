//! What Devices and access lists, and what the person does to it, through the
//! host calls the page makes.

use super::*;

use ui::tilo_actions::remote::{Caller, InvokeRequest, identity_admin};

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

#[derive(Default)]
struct RecordingHost {
    asked: std::sync::Mutex<Vec<InvokeRequest>>,
    listed: std::sync::Mutex<serde_json::Value>,
    asking: std::sync::Mutex<serde_json::Value>,
}

const THIS_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const LAPTOP_AGENT: &str = "5555555555555555555555555555555555555555555555555555555555555555";
const BROWSER: &str = "0123456789abcdef0123456789abcdef";

impl RecordingHost {
    fn new(listed: serde_json::Value) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            asked: std::sync::Mutex::default(),
            listed: std::sync::Mutex::new(listed),
            asking: std::sync::Mutex::new(serde_json::json!({"asking": []})),
        })
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

impl ui::tilo_actions::remote::ActionHost for RecordingHost {
    fn query(
        &self,
        _: &Caller,
        _: ui::tilo_actions::remote::CatalogueQuery,
    ) -> ui::tilo_actions::remote::Pending<
        Result<ui::tilo_actions::remote::CataloguePage, ui::tilo_actions::remote::ActionHostError>,
    > {
        ui::tilo_actions::remote::Pending::ready(Err(
            ui::tilo_actions::remote::ActionHostError::NotPermitted,
        ))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> ui::tilo_actions::remote::Pending<
        Result<
            ui::tilo_actions::remote::InvocationReceipt,
            ui::tilo_actions::remote::ActionHostError,
        >,
    > {
        let invocation = request.invocation;
        let value = if request.target == identity_admin::target(identity_admin::DEVICES) {
            self.listed.lock().unwrap().clone()
        } else if request.target == device_pairing::target(device_pairing::ASKING) {
            self.asking.lock().unwrap().clone()
        } else if request.target == agent_door::target(agent_door::DOOR) {
            return ui::tilo_actions::remote::Pending::ready(Err(
                ui::tilo_actions::remote::ActionHostError::UnknownAction,
            ));
        } else {
            serde_json::json!({})
        };
        self.asked.lock().unwrap().push(request);
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
    ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionHostError>>
    {
        ui::tilo_actions::remote::Pending::ready(Ok(()))
    }
}

fn key(n: u8) -> String {
    hex::encode([n; 32])
}

fn record(now: i64) -> serde_json::Value {
    let tab = |n: u8| {
        serde_json::json!({"label": "Chrome on Linux", "key_hex": key(n),
            "fingerprint": format!("{n:02X} {n:02X} {n:02X} {n:02X} {n:02X} {n:02X}"),
            "current": false, "revoked": false, "can_host": false, "browser": BROWSER,
            "issued_ms": now - 3 * DAY, "last_active_ms": now - 5 * MINUTE})
    };
    serde_json::json!([
        {"label": "Framework 16", "key_hex": THIS_DEVICE, "fingerprint": "11 11 11 11 11 11",
         "current": true, "revoked": false, "can_host": true},
        {"label": "Claude Code on laptop", "key_hex": LAPTOP_AGENT,
         "fingerprint": "55 55 55 55 55 55", "current": false, "revoked": false,
         "can_host": false, "kind": "agent", "agent": "Claude Code",
         "issued_ms": now - 2 * DAY, "expires_ms": now + 28 * DAY, "online": true},
        tab(0x31), tab(0x32),
    ])
}

fn model_on(host: &std::sync::Arc<RecordingHost>) -> (AccessModel, ui::RemoteActions) {
    let remote = ui::RemoteActions::with_host(host.clone(), Caller::local_person("owner"));
    let mut model = AccessModel::asking(&remote);
    model.poll(&remote);
    (model, remote)
}

#[test]
fn an_agent_signed_in_as_the_person_is_listed_with_what_it_may_do() {
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

#[test]
fn removing_an_agent_revokes_its_key_and_reads_the_list_again() {
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
    let listed_before = host
        .asked_at(&identity_admin::target(identity_admin::DEVICES))
        .len();
    model.poll(&remote);
    let revoked = host.asked_at(&identity_admin::target(identity_admin::REVOKE));
    assert_eq!(
        revoked,
        [
            serde_json::json!({"device": LAPTOP_AGENT, "reason": "retired", "confirm": true})
                .as_object()
                .unwrap()
                .clone()
        ]
    );
    let revoke = host
        .asked()
        .into_iter()
        .find(|request| request.target == identity_admin::target(identity_admin::REVOKE))
        .unwrap();
    assert_eq!(revoke.confirmation, Some(Confirmation::Confirmed));
    assert!(
        host.asked_at(&identity_admin::target(identity_admin::DEVICES))
            .len()
            > listed_before,
        "the list is read again after the removal"
    );
}

#[test]
fn a_browsers_tabs_are_one_entry_removed_together() {
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
        .asked_at(&identity_admin::target(identity_admin::REVOKE))
        .into_iter()
        .map(|argument| argument["device"].clone())
        .collect();
    assert_eq!(revoked, [key(0x31), key(0x32)], "every tab of it");
}

#[test]
fn this_browser_comes_first_and_is_removed_from_elsewhere() {
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

#[test]
fn a_request_appears_and_is_approved() {
    en();
    let host = RecordingHost::new(record(0));
    *host.asking.lock().unwrap() = serde_json::json!({"asking": [
        {"ask": "dd", "label": "Safari on iPad", "code": "482 913", "via": "Pixel 9"},
    ]});
    let (mut model, remote) = model_on(&host);
    assert_eq!(model.asking.len(), 1);
    assert_eq!(
        plain_all(asking_lines(&model.asking[0])),
        ["Shows the code 482 913", "Asked on Pixel 9"]
    );
    model
        .act(
            &remote,
            &DevicesAccessEffect::AnswerDevice {
                ask: "dd".into(),
                label: "Safari on iPad".into(),
                approve: true,
            },
            None,
        )
        .unwrap();
    *host.asking.lock().unwrap() = serde_json::json!({"asking": []});
    model.poll(&remote);
    let answered = host.asked_at(&device_pairing::target(device_pairing::ANSWER));
    assert_eq!(
        answered,
        [
            serde_json::json!({"ask": "dd", "label": "Safari on iPad", "approve": true,
            "confirm": true})
            .as_object()
            .unwrap()
            .clone()
        ]
    );
    model.poll(&remote);
    assert!(
        model.asking.is_empty(),
        "an answered request no longer waits"
    );
}

#[test]
fn connecting_asks_the_desk_for_its_setup() {
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

#[test]
fn the_doors_grants_join_the_records_agents() {
    en();
    let mut sample = sample();
    sample.door.grants.push(Grant {
        id: hex::encode([5u8; 32]),
        agent: "Claude Code".into(),
        reach: Reach::Account,
        granted_ms: 0,
        last_ms: None,
        runs: Vec::new(),
        runs_on: "laptop".into(),
        expires_ms: None,
        browser_pages: true,
        script_key: false,
    });
    let model = AccessModel::shown(sample);
    let agents = model.agents();
    let labels: Vec<String> = agents.iter().map(AgentEntry::label).collect();
    assert_eq!(
        labels,
        ["Nightly export", "Claude Code on laptop", "ChatGPT"],
        "active now first, the latest active first"
    );
    assert!(agents[1].grant.is_some(), "the door's grant joins its key");
    assert_eq!(
        agents[2].removal(),
        DevicesAccessEffect::RevokeAgent {
            grant: "bb".repeat(32)
        }
    );
    assert_eq!(plain(&agent_may(&agents[2])), "Only in Launch plan");
}

#[test]
fn the_published_rows_offer_what_can_be_done() {
    en();
    let model = AccessModel::shown(sample());
    let devices: Vec<LiveRow> = model
        .entries()
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Device(device) if !device.current => Some(device_row(device)),
            Entry::Browser(browser) if browser.revocable() => Some(browser_row(&browser)),
            _ => None,
        })
        .collect();
    assert_eq!(
        devices.len(),
        2,
        "the phone and the browser; the removed phone is gone"
    );
    assert_eq!(
        devices[0].clone().into_resource().actions.len(),
        REVOKE_REASONS.len()
    );
    let agents: Vec<_> = model
        .agents()
        .iter()
        .map(|agent| agent_row(agent).into_resource())
        .collect();
    assert_eq!(agents.len(), 3);
    let nightly = agents
        .iter()
        .find(|row| row.id.as_str().ends_with(&"cc".repeat(32)))
        .unwrap();
    assert_eq!(nightly.actions.len(), 2, "remove, and stop its open run");
    let asking = asking_row(&model.asking[0]).into_resource();
    assert_eq!(asking.actions.len(), 2, "approve or decline");
}

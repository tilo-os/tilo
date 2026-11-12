//! What Devices and access lists, and which keys it lets the person act on.

use super::*;

fn en() {
    ui::i18n::I18nManager::init("en-US");
}

fn sample() -> AccessSample {
    crate::scenarios::access_fixture(Some("devices_access")).expect("the access capture")
}

/// Agents are grouped by the app they came in through, and a script's key
/// is grouped with the other script keys, whatever the person named it.
#[test]
fn agents_group_by_their_app_and_script_keys_together() {
    en();
    let door = sample().door;
    let apps: Vec<String> = door.grants.iter().map(app_of).collect();
    assert_eq!(apps, ["Claude Code", "ChatGPT", "Script keys"]);
}

/// A grant says where it runs, what it reaches, until when, whether it may
/// operate web pages in Browser, when it last acted and the runs open now.
#[test]
fn a_grant_says_what_it_reaches_until_when_and_what_runs() {
    en();
    let door = sample().door;
    let claude = &door.grants[0];
    assert_eq!(
        grant_lines(claude),
        [
            "On Framework 16",
            "All your workspaces, until Jan 31, 2027",
            "Operates web pages in Browser",
            "Running now: Claude Code #2",
        ]
    );
    assert_eq!(reach_line(claude), "All your workspaces");
    assert!(claude.browser_pages);
    assert_eq!(claude.runs.len(), 1);
    let chatgpt = &door.grants[1];
    assert_eq!(reach_line(chatgpt), "Only Launch plan");
    assert!(!chatgpt.browser_pages);
    let script = &door.grants[2];
    assert_eq!(script.expires_ms, None);
    assert!(script.last_ms.is_none());
}

/// Only a device of the person's whose access has not ended can be
/// revoked; a key the list does not hold, or one already revoked, is not.
#[test]
fn only_a_listed_device_still_in_can_be_revoked() {
    en();
    let model = AccessModel::shown(sample());
    assert_eq!(model.device(&hex::encode([2u8; 32])), Ok([2u8; 32]));
    assert!(
        model.device(&hex::encode([4u8; 32])).is_err(),
        "the lost phone is out already"
    );
    assert!(
        model.device(&hex::encode([9u8; 32])).is_err(),
        "not the person's device"
    );
    assert!(model.grant(&"aa".repeat(32)).is_ok());
    assert!(model.grant("unknown").is_err());
}

/// The rows the page publishes offer each device but this one and those
/// already out, each agent, and each device asking in: one row each, with
/// what can be done to it.
#[test]
fn the_published_rows_offer_what_can_be_done() {
    en();
    let sample = sample();
    let devices: Vec<LiveRow> = sample
        .devices
        .iter()
        .filter(|device| !device.current && !device.revoked)
        .map(device_row)
        .collect();
    assert_eq!(devices.len(), 2);
    let revoke = devices[0].clone().into_resource();
    assert_eq!(
        revoke.actions.len(),
        REVOKE_REASONS.len(),
        "a device is revoked as no longer used, lost or stolen"
    );
    let claude = grant_row(&sample.door.grants[0]).into_resource();
    assert_eq!(claude.actions.len(), 2, "revoke, and stop its open run");
    let idle = grant_row(&sample.door.grants[1]).into_resource();
    assert_eq!(
        idle.actions.len(),
        1,
        "an agent with no open run can only be revoked"
    );
    let asking = asking_row(&sample.asking[0]).into_resource();
    assert_eq!(asking.actions.len(), 2, "approve or decline");
}

/// A host that answers the identity and pairing actions as a desktop's
/// session does, and records what it was asked.
#[derive(Default)]
struct RecordingHost {
    asked: std::sync::Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
    /// The devices it lists, when not this laptop and a phone.
    listed: Option<serde_json::Value>,
}

const THIS_DEVICE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const PHONE: &str = "2222222222222222222222222222222222222222222222222222222222222222";

impl ui::tilo_actions::remote::ActionHost for RecordingHost {
    fn query(
        &self,
        _: &ui::tilo_actions::remote::Caller,
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
        _: &ui::tilo_actions::remote::Caller,
        request: ui::tilo_actions::remote::InvokeRequest,
    ) -> ui::tilo_actions::remote::Pending<
        Result<
            ui::tilo_actions::remote::InvocationReceipt,
            ui::tilo_actions::remote::ActionHostError,
        >,
    > {
        use ui::tilo_actions::remote::identity_admin;
        let invocation = request.invocation;
        let value = match request.target.address.action.as_str() {
            identity_admin::DEVICES if self.listed.is_some() => self.listed.clone().unwrap(),
            identity_admin::DEVICES => serde_json::json!([
                {"label": "This laptop", "key_hex": THIS_DEVICE, "fingerprint": "11 11 11 11 11 11",
                 "current": true, "revoked": false, "can_host": true},
                {"label": "Phone", "key_hex": PHONE, "fingerprint": "22 22 22 22 22 22",
                 "current": false, "revoked": false, "can_host": false},
            ]),
            device_pairing::ASKING => serde_json::json!({"asking": []}),
            agent_door::DOOR => {
                serde_json::json!({"endpoint": "https://mcp.tilo.sh", "grants": []})
            }
            _ => serde_json::json!({"device": PHONE, "revoked": true}),
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
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionHostError>>
    {
        ui::tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// The devices and the root key are the host's: the page lists the devices
/// through the host's identity actions and revokes one there, as lost, with
/// the person's confirmation, then reads the list again. This device is
/// revoked from another one, never from itself.
#[test]
fn a_device_is_revoked_on_the_host_and_the_list_read_again() {
    use ui::tilo_actions::remote::{Caller, identity_admin};
    en();
    let host = std::sync::Arc::new(RecordingHost::default());
    let remote = ui::RemoteActions::with_host(host.clone(), Caller::local_person("owner"));
    let mut model = AccessModel::asking(&remote);
    model.poll(&remote);
    assert_eq!(
        model
            .devices
            .iter()
            .map(|device| (device.label.as_str(), device.current))
            .collect::<Vec<_>>(),
        [("This laptop", true), ("Phone", false)]
    );
    assert!(
        model
            .act(
                &remote,
                &DevicesAccessEffect::RevokeDevice {
                    device: PHONE.into(),
                    reason: "misplaced".into(),
                },
                None,
            )
            .is_err(),
        "a device's access ends for one of the reasons a revocation records"
    );
    model
        .act(
            &remote,
            &DevicesAccessEffect::RevokeDevice {
                device: PHONE.into(),
                reason: "lost".into(),
            },
            None,
        )
        .unwrap();
    let before = host.asked.lock().unwrap().len();
    model.poll(&remote);
    let asked = host.asked.lock().unwrap().clone();
    let revoke = asked
        .iter()
        .find(|request| request.target == identity_admin::target(identity_admin::REVOKE))
        .expect("the revocation was asked of the host");
    assert_eq!(revoke.confirmation, Some(Confirmation::Confirmed));
    assert_eq!(
        revoke.argument,
        InvokeArgument::Json(
            serde_json::json!({"device": PHONE, "reason": "lost", "confirm": true})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    assert!(
        asked[before..]
            .iter()
            .any(|request| request.target == identity_admin::target(identity_admin::DEVICES)),
        "the list is read again after the revocation"
    );
}

/// A key acting for the person (an agent's grant, a script's key) is no
/// device: Devices leaves it out, and where no door answers for the agents,
/// Agents lists it from the person's record, unless its access ended.
#[test]
fn an_agents_key_is_listed_with_the_agents_not_the_devices() {
    en();
    let mut sample = sample();
    let key = |n: u8, label: &str, revoked| DeviceView {
        label: label.into(),
        key: [n; 32],
        fingerprint: String::new(),
        current: false,
        revoked,
        can_host: false,
        kind: KeyKind::Agent,
        browser: None,
    };
    sample.devices.push(key(7, "Claude Code on job", false));
    sample.devices.push(key(8, "Old export", true));
    let model = AccessModel::shown(sample);
    assert!(
        model
            .listed_devices()
            .iter()
            .all(|device| device.kind == KeyKind::Device),
        "Devices lists no agent's key"
    );
    let keys: Vec<&str> = model.agent_keys().map(|key| key.label.as_str()).collect();
    assert_eq!(keys, ["Claude Code on job"]);
}

const BROWSER: &str = "0123456789abcdef0123456789abcdef";

/// One browser's tab device `n`, as the list names it.
fn tab(n: u8, current: bool) -> DeviceView {
    DeviceView {
        label: "Firefox on Linux".into(),
        key: [n; 32],
        fingerprint: format!("{n:02X} {n:02X} {n:02X} {n:02X} {n:02X} {n:02X}"),
        current,
        revoked: false,
        can_host: false,
        kind: KeyKind::Device,
        browser: Some(BROWSER.into()),
    }
}

/// A browser keeps a key per tab: its three tab devices are one entry,
/// saying "3 tabs" and their keys. Opened in one of those tabs, it is
/// "This browser", which can't revoke itself: no revoke, and no row.
#[test]
fn a_browsers_tab_devices_are_one_entry() {
    en();
    let mut sample = sample();
    sample.devices.retain(|device| !device.current);
    sample
        .devices
        .extend([tab(0x31, false), tab(0x32, true), tab(0x33, false)]);
    let model = AccessModel::shown(sample.clone());
    let entries = model.entries();
    let browsers: Vec<&BrowserEntry<'_>> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Browser(browser) => Some(browser),
            Entry::Device(_) => None,
        })
        .collect();
    assert_eq!(browsers.len(), 1, "one entry for the browser");
    let browser = browsers[0];
    assert!(
        matches!(entries[0], Entry::Browser(_)),
        "this browser comes first"
    );
    assert_eq!(browser.label(), "This browser");
    let lines: Vec<String> = browser_lines(browser)
        .into_iter()
        .map(|line| line.replace(['\u{2068}', '\u{2069}'], ""))
        .collect();
    assert_eq!(lines, ["3 tabs", "Keys 31 31 31, 32 32 32, 33 33 33"]);
    assert!(!browser.revocable(), "a tab can't revoke its own browser");
    assert!(
        model.browser_keys(BROWSER).is_err(),
        "nor revoke it by its effect"
    );

    // Seen from another device, it is named by its newest key and revoked.
    for device in &mut sample.devices {
        device.current = false;
    }
    sample.devices[0].current = true;
    let model = AccessModel::shown(sample);
    let entries = model.entries();
    let Some(Entry::Browser(browser)) = entries
        .iter()
        .find(|entry| matches!(entry, Entry::Browser(_)))
    else {
        panic!("the browser is listed");
    };
    assert_eq!(browser.label(), "Firefox on Linux");
    assert!(browser.revocable());
    let row = browser_row(browser).into_resource();
    assert_eq!(row.actions.len(), REVOKE_REASONS.len());
}

/// From another device, revoking the browser revokes every tab's key on the
/// host, and the list is read again once all of them settled.
#[test]
fn revoking_a_browser_revokes_each_of_its_tabs() {
    use ui::tilo_actions::remote::{Caller, identity_admin};
    en();
    let key = |n: u8| hex::encode([n; 32]);
    let tab = |n: u8, revoked: bool| {
        serde_json::json!({"label": "Firefox on Linux", "key_hex": key(n),
            "fingerprint": "", "current": false, "revoked": revoked, "can_host": false,
            "browser": BROWSER})
    };
    let host = std::sync::Arc::new(RecordingHost {
        listed: Some(serde_json::json!([
            {"label": "This laptop", "key_hex": THIS_DEVICE, "fingerprint": "",
             "current": true, "revoked": false, "can_host": true},
            tab(0x31, false), tab(0x32, false), tab(0x33, false), tab(0x34, true),
        ])),
        ..RecordingHost::default()
    });
    let remote = ui::RemoteActions::with_host(host.clone(), Caller::local_person("owner"));
    let mut model = AccessModel::asking(&remote);
    model.poll(&remote);
    assert_eq!(model.entries().len(), 2, "this laptop, and the browser");
    model
        .act(
            &remote,
            &DevicesAccessEffect::RevokeBrowser {
                browser: BROWSER.into(),
                reason: "stolen".into(),
            },
            None,
        )
        .unwrap();
    let before = host.asked.lock().unwrap().len();
    model.poll(&remote);
    let asked = host.asked.lock().unwrap().clone();
    let revoked: Vec<serde_json::Value> = asked
        .iter()
        .filter(|request| request.target == identity_admin::target(identity_admin::REVOKE))
        .map(|request| match &request.argument {
            InvokeArgument::Json(argument) => argument["device"].clone(),
            other => panic!("a revoke names its device: {other:?}"),
        })
        .collect();
    assert_eq!(
        revoked,
        [key(0x31), key(0x32), key(0x33)],
        "every tab whose access has not ended"
    );
    assert!(
        asked[before..]
            .iter()
            .any(|request| request.target == identity_admin::target(identity_admin::DEVICES)),
        "the list is read again after the revocations"
    );
}

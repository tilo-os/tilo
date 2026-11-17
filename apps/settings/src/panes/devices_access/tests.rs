//! What Devices and access lists, and what the person does to it, through the
//! host calls the page makes.

use super::*;

use ui::prelude::TestApp;
use ui::tilo_actions::remote::{
    ActionHostError, Caller, Completer, InvocationReceipt, InvokeRequest, identity_admin,
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

#[derive(Default)]
struct RecordingHost {
    asked: std::sync::Mutex<Vec<InvokeRequest>>,
    listed: std::sync::Mutex<serde_json::Value>,
    asking: std::sync::Mutex<serde_json::Value>,
    sheet: std::sync::Mutex<Sheet>,
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

type Answer = Result<InvocationReceipt, ActionHostError>;

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
            listed: std::sync::Mutex::new(listed),
            asking: std::sync::Mutex::new(serde_json::json!({"asking": []})),
            sheet: std::sync::Mutex::default(),
        })
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
                    ui::tilo_actions::remote::Pending::ready(Err(ActionHostError::NotPermitted))
                }
                Sheet::Held(held) => {
                    let (answer, pending) = ui::tilo_actions::remote::pending();
                    held.push((invocation, answer));
                    pending
                }
            };
        }
        let value = if request.target == identity_admin::target(identity_admin::DEVICES) {
            self.listed.lock().unwrap().clone()
        } else if request.target == device_pairing::target(device_pairing::ASKING) {
            self.asking.lock().unwrap().clone()
        } else if request.target == mcp_server::target(mcp_server::SERVER) {
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
    let remote = ui::RemoteActions::with_host(host.clone(), Caller::local_person("owner"));
    let mut model = AccessModel::asking(&remote);
    model.poll(&remote);
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

    phase the_name_the_person_typed_reaches_their_record;
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
            host.asked_at(&identity_admin::target(identity_admin::RENAME)),
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
            host.asked_at(&identity_admin::target(identity_admin::RENAME))
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

    phase removing_an_agent_revokes_its_key_and_reads_the_list_again;
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
            .asked_at(&identity_admin::target(identity_admin::REVOKE))
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

    phase a_request_appears_and_is_approved;
    {
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

    phase the_apis_grants_join_the_records_agents;
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
            device: "laptop".into(),
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
        assert!(agents[1].grant.is_some(), "the API's grant joins its key");
        assert_eq!(
            agents[2].removal(),
            DevicesAccessEffect::RevokeAgent {
                grant: "bb".repeat(32)
            }
        );
        assert_eq!(plain(&agent_may(&agents[2])), "Only in Launch plan");
    }

    phase the_published_rows_offer_what_can_be_done;
    {
        en();
        let model = AccessModel::shown(sample());
        let devices: Vec<LiveRow> = model
            .entries()
            .into_iter()
            .filter_map(|entry| match entry {
                Entry::Device(device) if !device.current => Some(device_row(device, "")),
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
            REVOKE_REASONS.len() + 1,
            "every way to take it back, and naming it"
        );
        let agents: Vec<_> = model
            .agents()
            .iter()
            .map(|agent| agent_row(agent, "").into_resource())
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
    let served = host.clone();
    crate::area::replace_in_next_window(move |context| {
        DevicesAccessArea::new(
            context,
            ui::RemoteActions::with_host(served, Caller::local_person("owner")),
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
                        if *why == ActionHostError::NotPermitted.to_string()
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

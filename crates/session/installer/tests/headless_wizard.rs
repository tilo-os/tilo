//! The whole installer wizard, driven on the host with no window, no GPU and
//! no disk: the real widget tree under `ui::headless::FrameHarness`, a recording
//! install backend, and the installer's published actions reached three
//! ways — saved `--headless-drive` scenario steps, a wizard control clicked
//! by name, and the console protocol the live medium serves.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the wizard is walked in one ordered test"
)]

use std::sync::{Arc, Mutex};

use installer::InstallerApp;
use installer::backend::InstallBackend;
use installer::disks::Disk;
use installer::install::{InstallOptions, PowerAction};
use installer::installer_app::CONSOLE_WAKE;
use serde_json::Value;

const KEY: &str = "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
const SECRET: &str = "correct horse battery staple";

#[derive(Default)]
struct Fake {
    installs: Mutex<Vec<(String, String, String, InstallOptions)>>,
    power: Mutex<Vec<PowerAction>>,
}

fn disk(name: &str, serial: &str, major_minor: &str, system: bool) -> Disk {
    Disk {
        name: name.into(),
        size: "40G".into(),
        model: "QEMU HARDDISK".into(),
        size_bytes: 40 * 1024 * 1024 * 1024,
        partition_table: String::new(),
        partition_table_uuid: String::new(),
        logical_sector_size: 512,
        major_minor: major_minor.into(),
        wwn: String::new(),
        serial: serial.into(),
        removable: false,
        read_only: false,
        is_system: system,
        partitions: Vec::new(),
        live_system_nodes: Vec::new(),
        internal_plan: None,
    }
}

impl InstallBackend for Fake {
    fn read_disks(&self) -> Vec<Disk> {
        vec![
            disk("/dev/vda", "tilo-installer-medium", "252:0", true),
            disk("/dev/vdb", "tilo-install-target", "252:16", false),
        ]
    }

    fn run_install(
        &self,
        disk: &str,
        user: &str,
        password: &str,
        opts: &InstallOptions,
        on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        self.installs.lock().unwrap().push((
            disk.into(),
            user.into(),
            password.into(),
            opts.clone(),
        ));
        on_line("==> Copying the running Tilo image into local container storage".into());
        on_line(format!("::tilo-recovery-key::{KEY}"));
        on_line("==> Preparing the 'ada' account on the installed system".into());
        Ok(true)
    }

    fn run_alongside_preflight(
        &self,
        _disk: &str,
        _opts: &InstallOptions,
        _on_line: &mut dyn FnMut(String),
    ) -> std::io::Result<bool> {
        Ok(true)
    }

    fn run_power(&self, action: PowerAction) -> std::io::Result<bool> {
        self.power.lock().unwrap().push(action);
        Ok(true)
    }
}

/// Ask the console, the way a live socket request arrives: queued, then
/// answered on the UI thread when the wake-up is delivered.
fn ask(
    driver: &mut ui::headless::FrameHarness,
    console: &console_control::ConsoleServer,
    request: &str,
) -> Value {
    let answer = console.submit(request);
    driver.deliver_external(CONSOLE_WAKE);
    serde_json::from_str(&answer.recv().unwrap()).unwrap()
}

#[test]
fn online_accounts_can_be_skipped_without_private_input() {
    let console = console_control::ConsoleServer::new();
    let app = InstallerApp::with_disks(Vec::new())
        .with_backend(Arc::new(Fake::default()))
        .with_console(console.clone());
    let mut driver = ui::headless::FrameHarness::new(
        ui::State::new(),
        Box::new(ui::widget::resource_effects::host(app)),
        (1280, 800),
    );
    let spec = ui::headless::parse_spec(
        r#"{"actions":[
          {"invoke":{"resource":"installer","action":"next"}},
          {"invoke":{"resource":"installer","action":"next"}}
        ]}"#,
    )
    .unwrap();
    for action in &spec.actions {
        driver.perform(action).unwrap();
    }
    let accounts = ask(&mut driver, &console, r#"{"op":"state_dump"}"#);
    assert_eq!(accounts["state"]["step"], "accounts");
    let skipped = ask(
        &mut driver,
        &console,
        r#"{"op":"actions_invoke","provider":"installer","id":"skip-offline@installer.accounts"}"#,
    );
    assert!(skipped.get("answer").is_some(), "{skipped}");
    let storage = ask(&mut driver, &console, r#"{"op":"state_dump"}"#);
    assert_eq!(storage["state"]["step"], "disk");
    let off_step = driver.invoke_action(
        &tilo_actions::ActionAddress::new("installer.accounts", "skip-offline"),
        &serde_json::Map::new(),
        false,
        None,
    );
    assert_eq!(
        off_step.err().map(|error| error.code()),
        Some("action_unavailable")
    );
}

#[test]
fn the_whole_wizard_runs_headless_through_published_actions() {
    let backend = Arc::new(Fake::default());
    let console = console_control::ConsoleServer::new();
    let app = InstallerApp::with_disks(Vec::new())
        .with_backend(backend.clone())
        .with_console(console.clone());
    let mut driver = ui::headless::FrameHarness::new(
        ui::State::new(),
        Box::new(ui::widget::resource_effects::host(app)),
        (1280, 800),
    );

    // Saved scenario steps in the `--headless-drive` grammar: every step
    // names an action by address, and every refusal is asserted by code.
    let steps = format!(
        r#"{{"actions":[
          {{"invoke":{{"resource":"installer.keyboard","action":"set-layout","args":{{"layout":"English (US)"}}}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer.accounts","action":"skip-offline"}}}},
          {{"invoke":{{"resource":"installer","action":"next","expect_error":"action_failed"}}}},
          {{"invoke":{{"resource":"installer.disk:/dev/vda","action":"select","expect_error":"action_unavailable"}}}},
          {{"invoke":{{"resource":"installer.storage","action":"set-method","args":{{"method":"erase"}}}}}},
          {{"invoke":{{"resource":"installer.disk:/dev/vdb","action":"select"}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer.account","action":"set-identity","args":{{"username":"ada","full_name":"Ada Lovelace"}}}}}},
          {{"invoke":{{"resource":"installer.account","action":"set-passphrase","args":{{"passphrase":"{SECRET}","confirmation":"{SECRET}"}}}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer.options","action":"set-hostname","args":{{"hostname":"lab-1"}}}}}},
          {{"invoke":{{"resource":"installer.options","action":"set-filesystem","args":{{"filesystem":"xfs"}}}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer","action":"next"}}}},
          {{"invoke":{{"resource":"installer.review","action":"begin","confirmation_phrase":"/dev/vdb","expect_error":"action_unavailable"}}}}
        ]}}"#
    );
    let spec = ui::headless::parse_spec(&steps).unwrap();
    for (index, action) in spec.actions.iter().enumerate() {
        driver
            .perform(action)
            .unwrap_or_else(|error| panic!("step {}: {error}", index + 1));
    }

    // The wizard's own checkbox, toggled the way a screen reader does it,
    // runs the same `set-recovery-promise` action.
    let toggle = ui::headless::parse_spec(
        r#"{"actions":[{"perform":{"target":"recovery_ack","action":"toggle_check"}}]}"#,
    )
    .unwrap();
    driver.perform(&toggle.actions[0]).unwrap();
    let review = ask(&mut driver, &console, r#"{"op":"state_dump"}"#);
    assert_eq!(review["state"]["step"], "review");
    assert_eq!(review["state"]["review"]["recovery_promise"], true);
    assert_eq!(
        review["state"]["storage"]["confirmation_target"],
        "/dev/vdb"
    );

    // The destructive action needs its exact phrase, whichever door asks.
    for wrong in [None, Some("/dev/vda")] {
        let refused = driver.invoke_action(
            &tilo_actions::ActionAddress::new("installer.review", "begin"),
            &serde_json::Map::new(),
            true,
            wrong,
        );
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some("action_confirmation_required")
        );
    }
    let refused = ask(
        &mut driver,
        &console,
        r#"{"op":"actions_invoke","provider":"installer","id":"begin@installer.review","args":{"confirmation_phrase":"/dev/sda"}}"#,
    );
    assert_eq!(refused["err"]["code"], "action_confirmation_required");
    assert!(backend.installs.lock().unwrap().is_empty());

    driver
        .invoke_action(
            &tilo_actions::ActionAddress::new("installer.review", "begin"),
            &serde_json::Map::new(),
            false,
            Some("/dev/vdb"),
        )
        .unwrap();
    let installs = backend.installs.lock().unwrap().clone();
    assert_eq!(installs.len(), 1);
    let (target, user, password, options) = &installs[0];
    assert_eq!((target.as_str(), user.as_str()), ("/dev/vdb", "ada"));
    assert_eq!(password, SECRET);
    assert_eq!(
        (options.filesystem.as_str(), options.hostname.as_str()),
        ("xfs", "lab-1")
    );
    assert_eq!(options.full_name, "Ada Lovelace");
    assert_eq!(options.disk_serial, "tilo-install-target");

    // Progress and completion as a caller that cannot see the screen reads
    // them. The recovery key exists and never leaves the installer.
    let finished = ask(&mut driver, &console, r#"{"op":"state_dump"}"#);
    let state = &finished["state"];
    assert_eq!(state["install"]["state"], "done");
    assert_eq!(state["recovery_key"]["available"], true);
    assert!(
        state["install"]["stages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|stage| stage == "Tilo is ready")
    );
    let listed = ask(&mut driver, &console, r#"{"op":"actions_enumerate"}"#);
    let everything = format!("{finished}{listed}{}", driver.dump_json());
    assert!(
        !everything.contains(KEY),
        "the recovery key left the installer"
    );
    assert!(
        !everything.contains(SECRET),
        "the passphrase left the installer"
    );

    // Power waits for the saved acknowledgement, then goes to the backend.
    let early = ask(
        &mut driver,
        &console,
        r#"{"op":"actions_invoke","provider":"installer","id":"power-off@installer.power"}"#,
    );
    assert_eq!(early["err"]["code"], "action_unavailable");
    assert!(backend.power.lock().unwrap().is_empty());
    for request in [
        r#"{"op":"actions_invoke","provider":"installer","id":"confirm-saved@installer.recovery","args":{"saved":true}}"#,
        r#"{"op":"actions_invoke","provider":"installer","id":"power-off@installer.power"}"#,
    ] {
        let reply = ask(&mut driver, &console, request);
        assert!(reply.get("answer").is_some(), "{reply}");
    }
    assert_eq!(*backend.power.lock().unwrap(), [PowerAction::PowerOff]);
}

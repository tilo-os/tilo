//! A device's migrations through its actions, as any caller invokes them: a Thunderbird profile
//! reviewed, chosen and brought over by `start`, a second run adding nothing, and a cancelled run
//! stopping before anything lands, which `start` resumes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvocationState, InvokeArgument, InvokeRequest, Pending,
    RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::{Host, Migrations, PROVIDER};
use crate::engine::state::Place;
use crate::engine::tests::test_env::Recorded;
use crate::engine::writer::{Answer, Invocation, Workspace};

const DEVICE: [u8; 32] = [7; 32];

/// A device whose home holds a Thunderbird profile, and a workspace that records what it is
/// asked, across every time the migration reaches it.
struct Device {
    dir: tempfile::TempDir,
    recorded: Arc<Mutex<Recorded>>,
    /// Closed, reaching the workspace waits until it opens.
    gate: (Mutex<bool>, Condvar),
}

impl Host for Device {
    fn place(&self) -> Place {
        Place::at(&self.dir.path().join("state"))
    }

    fn workspace(&self) -> String {
        "ws".into()
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn folders(&self) -> BTreeMap<String, PathBuf> {
        BTreeMap::new()
    }

    fn scratch(&self) -> PathBuf {
        self.dir.path().join("scratch")
    }

    fn open(&self, _: &Caller) -> Result<Box<dyn Workspace>> {
        let (closed, opened) = &self.gate;
        drop(
            opened
                .wait_while(closed.lock().expect("gate"), |closed| *closed)
                .expect("gate"),
        );
        Ok(Box::new(Shared(Arc::clone(&self.recorded))))
    }
}

/// The one recorded workspace, reached again.
struct Shared(Arc<Mutex<Recorded>>);

impl Workspace for Shared {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        self.0.lock().expect("recorded").act(run, action, args)
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        self.0.lock().expect("recorded").ask(action, args)
    }

    fn read(&mut self, path: &str) -> Result<Value> {
        self.0.lock().expect("recorded").read(path)
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        self.0.lock().expect("recorded").person(path, action, args)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.0.lock().expect("recorded").put(bytes)
    }

    fn put_files(&mut self, files: &[(PathBuf, String)]) -> Result<Vec<String>> {
        self.0.lock().expect("recorded").put_files(files)
    }
}

fn device() -> (Arc<Device>, Migrations) {
    let dir = tempfile::tempdir().expect("scratch");
    crate::thunderbird::fixture::write_sample_profile(&dir.path().join("home"));
    let device = Arc::new(Device {
        dir,
        recorded: Arc::default(),
        gate: (Mutex::new(false), Condvar::new()),
    });
    let migrations = Migrations::new(super::resource(&DEVICE), Arc::clone(&device) as _);
    (device, migrations)
}

fn caller() -> Caller {
    Caller::authenticated([1; 32], [2; 32])
}

fn request(action: &str, args: Value) -> InvokeRequest {
    let Value::Object(args) = args else {
        panic!("arguments are an object")
    };
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(super::resource(&DEVICE), action),
        },
        argument: InvokeArgument::Json(args),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// What `pending` answers, soon.
fn answered<T>(pending: Pending<T>) -> T {
    pending
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("answered")
}

/// Invoke `action` and follow it to its end.
fn run(migrations: &Migrations, action: &str, args: Value) -> InvocationUpdate {
    let request = request(action, args);
    let id = request.invocation;
    answered(migrations.invoke(&caller(), request)).expect("admitted");
    follow(migrations, id)
}

/// How the invocation `id` ended.
fn follow(migrations: &Migrations, id: InvocationId) -> InvocationUpdate {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let state: InvocationState = migrations.observe(&caller(), &id).expect("known");
        if state.is_terminal() {
            return state.update;
        }
        assert!(Instant::now() < until, "{id:?} never ended");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn value(update: &InvocationUpdate) -> &Value {
    match update {
        InvocationUpdate::Succeeded { output, .. } => output.value.as_ref().expect("a value"),
        other => panic!("not done: {other:?}"),
    }
}

/// The key of the profile's address book, as the review lists it.
fn address_book(review: &Value) -> String {
    review["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == "contacts")
        .and_then(|item| item["key"].as_str())
        .expect("the profile has an address book")
        .to_owned()
}

#[test]
fn a_profile_comes_over_through_the_devices_actions() {
    let (device, migrations) = device();
    let reviewed = run(&migrations, "review", json!({"source": "thunderbird"}));
    let book = address_book(value(&reviewed));
    let chosen = run(
        &migrations,
        "choose",
        json!({"source": "thunderbird", "items": [book], "only": true}),
    );
    assert_eq!(value(&chosen)["chosen"], json!([book]));

    let started = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&started)["state"], "complete", "{started:?}");
    assert_eq!(value(&started)["brought"]["contacts"], 2, "{started:?}");
    let landed = device.recorded.lock().expect("recorded").actions.len();
    assert!(
        landed > 0,
        "the contacts landed through the workspace's actions"
    );

    // A second run finds everything there and adds nothing.
    let again = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&again)["already_there"], 2, "{again:?}");
    assert_eq!(
        device.recorded.lock().expect("recorded").actions.len(),
        landed
    );

    let status = run(&migrations, "status", json!({"source": "thunderbird"}));
    assert_eq!(value(&status)["state"], "complete");
}

#[test]
fn cancelling_a_run_stops_it_and_start_resumes() {
    let (device, migrations) = device();
    let reviewed = run(&migrations, "review", json!({"source": "thunderbird"}));
    let book = address_book(value(&reviewed));
    run(
        &migrations,
        "choose",
        json!({"source": "thunderbird", "items": [book], "only": true}),
    );

    // Held while it reaches the workspace, the run is cancelled before its first item.
    let (closed, opened) = &device.gate;
    *closed.lock().expect("gate") = true;
    let request = request("start", json!({"source": "thunderbird"}));
    let id = request.invocation;
    answered(migrations.invoke(&caller(), request)).expect("admitted");
    answered(migrations.cancel(&caller(), &id)).expect("cancelled");
    *closed.lock().expect("gate") = false;
    opened.notify_all();
    let stopped = follow(&migrations, id);
    assert!(
        matches!(stopped, InvocationUpdate::Cancelled { .. }),
        "{stopped:?}"
    );
    assert!(device.recorded.lock().expect("recorded").actions.is_empty());

    let resumed = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&resumed)["brought"]["contacts"], 2, "{resumed:?}");
}

#[test]
fn a_step_of_another_device_or_provider_is_unknown() {
    let (_, migrations) = device();
    let mut other = request("sources", json!({}));
    other.target.address = tilo_actions::ActionAddress::new(super::resource(&[8; 32]), "sources");
    let refused = answered(migrations.invoke(&caller(), other));
    assert!(refused.is_err(), "{refused:?}");
}

/// The key of the item named `name` on the page `page` answered.
fn key_of(page: &Value, name: &str) -> String {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["name"] == name)
        .and_then(|item| item["key"].as_str())
        .unwrap_or_else(|| panic!("{name} is not listed: {page}"))
        .to_owned()
}

#[test]
fn a_drive_is_paged_chosen_at_any_depth_and_given_a_destination_through_the_actions() {
    use cloud_source_dav::fake::{FakeDav, PASSWORD, USER};
    let server = FakeDav::nextcloud();
    for (path, body) in [
        ("Notes/todo.md", "- [ ] move"),
        ("Photos/2026/beach.jpg", "jpeg"),
        ("Photos/2026/hill.jpg", "jpeg too"),
        ("Photos/2026/sea.jpg", "jpeg three"),
    ] {
        server.put(path, body.as_bytes());
    }
    let mut account = accounts::Account::new("nc-provider", accounts::cloud_types::NEXTCLOUD);
    account.config = server.account_config();
    account.config["use_files"] = true.into();
    let drive = crate::engine::clouds::open_with(
        &account,
        Arc::new(cloud_source::StaticCredential(
            cloud_source::Credential::Basic {
                username: USER.into(),
                password: zeroize::Zeroizing::new(PASSWORD.into()),
            },
        )),
    )
    .expect("the fake drive opens");
    crate::engine::clouds::FAKES
        .lock()
        .expect("fakes")
        .insert("nc-provider".into(), drive);
    let (_, migrations) = device();

    let top = run(
        &migrations,
        "review",
        json!({"source": "nextcloud", "account": "nc-provider"}),
    );
    let top = value(&top);
    let photos = key_of(top, "Photos");
    let notes = key_of(top, "Notes");
    let year = run(
        &migrations,
        "review",
        json!({"source": "nextcloud", "within": photos}),
    );
    let year = key_of(value(&year), "2026");
    let first = run(
        &migrations,
        "review",
        json!({"source": "nextcloud", "within": year, "limit": 2}),
    );
    let first = value(&first);
    assert_eq!(first["items"].as_array().expect("items").len(), 2);
    assert_eq!(first["path"][1]["key"], json!(photos), "{first}");
    let next = first["next"].as_str().expect("a second page").to_owned();
    let second = run(
        &migrations,
        "review",
        json!({"source": "nextcloud", "within": year, "cursor": next, "limit": 2}),
    );
    let sea = key_of(value(&second), "sea.jpg");
    assert!(value(&second)["next"].is_null());

    let chosen = run(
        &migrations,
        "choose",
        json!({"source": "nextcloud", "keys": [notes, sea], "only": true}),
    );
    assert_eq!(value(&chosen)["chosen"], json!([notes, sea]));

    let refused = run(
        &migrations,
        "start",
        json!({"source": "nextcloud", "destination": "../elsewhere"}),
    );
    assert!(
        matches!(refused, InvocationUpdate::Failed { .. }),
        "a destination outside the workspace is refused: {refused:?}"
    );
    let status = run(&migrations, "status", json!({"source": "nextcloud"}));
    let status = value(&status);
    assert_eq!(status["chosen"], json!([notes, sea]));
    assert_eq!(status["failed"], json!([]));
    assert_eq!(status["left_out"], json!([]));
}

#[test]
fn stopping_a_source_cancels_its_run() {
    let (device, migrations) = device();
    let reviewed = run(&migrations, "review", json!({"source": "thunderbird"}));
    let book = address_book(value(&reviewed));
    run(
        &migrations,
        "choose",
        json!({"source": "thunderbird", "items": [book], "only": true}),
    );

    // Paused while it reaches the workspace, the run is stopped by its source, by whoever asks.
    let (closed, opened) = &device.gate;
    *closed.lock().expect("gate") = true;
    let request = request("start", json!({"source": "thunderbird"}));
    let id = request.invocation;
    answered(migrations.invoke(&caller(), request)).expect("admitted");
    let waited = migrations
        .next_change(&caller(), &id)
        .expect("a step is followed by what moves it");
    // The run is going once its thread took it up.
    let until = Instant::now() + Duration::from_secs(30);
    let stopped = loop {
        let stopped = run(&migrations, "stop", json!({"source": "thunderbird"}));
        if matches!(stopped, InvocationUpdate::Succeeded { .. }) || Instant::now() > until {
            break stopped;
        }
    };
    assert_eq!(
        value(&stopped)["run"],
        json!(id.0.to_string()),
        "{stopped:?}"
    );
    *closed.lock().expect("gate") = false;
    opened.notify_all();
    assert!(
        matches!(follow(&migrations, id), InvocationUpdate::Cancelled { .. }),
        "stopping it is cancelling it"
    );
    // Its follower was told.
    answered(waited);

    let nothing = run(&migrations, "stop", json!({"source": "thunderbird"}));
    assert!(
        matches!(nothing, InvocationUpdate::Failed { .. }),
        "nothing of it is going any more: {nothing:?}"
    );
}

/// What code compiles its calls of the engine against (`apps/tl/migrate-engine`) is what the
/// device registers with it.
#[test]
fn the_committed_package_is_what_the_engine_registers() {
    let (_, migrations) = device();
    let registered = tilo_actions::remote::ProvidedPackage::of(&migrations.published())
        .expect("the engine publishes a package");
    let manifest: toml::Table = toml::from_str(include_str!(
        "../../../../../apps/tl/migrate-engine/tilo-app.toml"
    ))
    .expect("the manifest reads");
    assert_eq!(manifest["package"]["id"].as_str(), Some(super::PACKAGE));
    assert_eq!(registered.app, super::PACKAGE);
    assert_eq!(
        manifest["package"]["name"].as_str(),
        Some(registered.title.as_str())
    );
    let committed: toml::Table = toml::from_str(include_str!(
        "../../../../../apps/tl/migrate-engine/surface.toml"
    ))
    .expect("the description reads");
    let committed: Vec<tilo_actions::surface::Surface> =
        serde_json::from_value(serde_json::to_value(&committed["resources"]).expect("as JSON"))
            .expect("the description is surfaces");
    let published: Vec<_> = registered
        .resources
        .into_iter()
        .map(|resource| {
            assert_eq!(resource.address, super::resource(&DEVICE));
            resource.surface
        })
        .collect();
    assert_eq!(committed, published);
}

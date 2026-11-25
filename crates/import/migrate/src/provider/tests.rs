//! A device's migrations through its actions, as any caller invokes them: a Thunderbird profile
//! reviewed, chosen and brought over by `start`, a second run adding nothing, and a cancelled run
//! stopping before anything lands, which `start` resumes. The person and their agent drive one
//! migration together: what one chooses the other reads, a run one stops the other resumes as the
//! same run, and taking that run back takes all of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationState, InvokeArgument, InvokeRequest,
    Pending, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};

use super::{Host, Migrations, PROVIDER};
use crate::engine::state::Place;
use crate::engine::tests::test_env::Recorded;
use crate::engine::writer::{Answer, Invocation, Workspace};

const DEVICE: [u8; 32] = [7; 32];

/// Where a run is held: once the workspace has been asked `at` actions, the next waits while it
/// is `held`.
#[derive(Default)]
struct Pause {
    at: Option<usize>,
    held: bool,
}

/// What the workspace keeps across every time the migration reaches it: what it was asked,
/// the runs History took back, and where a run is held.
#[derive(Default)]
struct Kept {
    recorded: Mutex<Recorded>,
    /// What the Migrations app was told of the migration, in order: the action and its
    /// arguments.
    told: Mutex<Vec<(String, Value)>>,
    undone: Mutex<BTreeSet<String>>,
    pause: (Mutex<Pause>, Condvar),
    /// The workspace each reach of it was in, in order.
    reached: Mutex<Vec<String>>,
}

/// A device whose home holds a Thunderbird profile, and a workspace that records what it is
/// asked, across every time the migration reaches it.
struct Device {
    dir: tempfile::TempDir,
    kept: Arc<Kept>,
    /// Closed, reaching the workspace waits until it opens.
    gate: (Mutex<bool>, Condvar),
}

impl Device {
    /// Every action the workspace was asked, as the run it was a step of and the action.
    fn actions(&self) -> Vec<(String, String)> {
        self.kept
            .recorded
            .lock()
            .expect("recorded")
            .actions
            .iter()
            .map(|(run, action, _)| (run.clone(), action.clone()))
            .collect()
    }

    /// What the Migrations app's record of the migration was told, as its actions.
    fn told(&self) -> Vec<String> {
        self.kept
            .told
            .lock()
            .expect("told")
            .iter()
            .map(|(action, _)| {
                action
                    .rsplit_once('#')
                    .map_or_else(|| action.clone(), |(_, verb)| verb.to_owned())
            })
            .collect()
    }

    /// What the Migrations app's record of the migration was told: each action and its
    /// arguments.
    fn told_whole(&self) -> Vec<(String, Value)> {
        self.kept.told.lock().expect("told").clone()
    }
}

/// The workspace the device is in, and another it is a member of.
const OWN: &str = "ws";
const LAB: &str = "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b";

impl Host for Device {
    fn workspace(&self) -> String {
        OWN.into()
    }

    fn member(&self, workspace: &str) -> bool {
        workspace == LAB
    }

    fn place(&self, workspace: &str) -> Place {
        Place::at(&self.dir.path().join("state").join(workspace))
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn folders(&self) -> BTreeMap<String, PathBuf> {
        BTreeMap::new()
    }

    fn scratch(&self, workspace: &str) -> PathBuf {
        self.dir.path().join("scratch").join(workspace)
    }

    fn open(&self, _: &Caller, workspace: &str) -> Result<Box<dyn Workspace>> {
        let (closed, opened) = &self.gate;
        drop(
            opened
                .wait_while(closed.lock().expect("gate"), |closed| *closed)
                .expect("gate"),
        );
        self.kept
            .reached
            .lock()
            .expect("reached")
            .push(workspace.to_owned());
        Ok(Box::new(Shared(Arc::clone(&self.kept))))
    }
}

/// The one recorded workspace, reached again. History answers as each record keeps it: every
/// step names the run it was a step of, and `undo` takes a run's steps back.
struct Shared(Arc<Kept>);

impl Workspace for Shared {
    fn act(&mut self, run: &Invocation, action: &str, args: Value) -> Result<Answer> {
        let answer = self
            .0
            .recorded
            .lock()
            .expect("recorded")
            .act(run, action, args);
        let asked = self.0.recorded.lock().expect("recorded").actions.len();
        let (pause, moved) = &self.0.pause;
        let mut pause = pause.lock().expect("pause");
        if pause.at == Some(asked) {
            pause.held = true;
            moved.notify_all();
            drop(moved.wait_while(pause, |pause| pause.held).expect("pause"));
        } else {
            drop(pause);
        }
        answer
    }

    fn ask(&mut self, action: &str, args: Value) -> Result<Answer> {
        if action.starts_with(super::records::APP) {
            let made = action.ends_with("#reviewed").then(|| json!({"id": "m-1"}));
            self.0
                .told
                .lock()
                .expect("told")
                .push((action.to_owned(), args));
            return Ok(Answer::of(made));
        }
        if action.ends_with("#history") {
            let undone = self.0.undone.lock().expect("undone");
            let versions: Vec<Value> = self
                .0
                .recorded
                .lock()
                .expect("recorded")
                .actions
                .iter()
                .enumerate()
                .filter(|(_, (run, _, _))| !run.is_empty())
                .map(|(n, (run, _, _))| {
                    json!({
                        "change": format!("{run}/{n}"),
                        "actor": {"declared": run},
                        "undone": undone.contains(run),
                    })
                })
                .collect();
            return Ok(Answer::of(Some(json!({ "versions": versions }))));
        }
        if action.ends_with("#undo") {
            let step = args["step"].as_str().expect("a step");
            let run = step.rsplit_once('/').expect("run/step").0.to_owned();
            self.0.undone.lock().expect("undone").insert(run);
            return Ok(Answer::of(None));
        }
        self.0.recorded.lock().expect("recorded").ask(action, args)
    }

    fn read(&mut self, path: &str) -> Result<Value> {
        self.0.recorded.lock().expect("recorded").read(path)
    }

    fn person(&mut self, path: &str, action: &str, args: Value) -> Result<Value> {
        self.0
            .recorded
            .lock()
            .expect("recorded")
            .person(path, action, args)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<String> {
        self.0.recorded.lock().expect("recorded").put(bytes)
    }

    fn put_files(&mut self, files: &[(PathBuf, String)]) -> Result<Vec<String>> {
        self.0.recorded.lock().expect("recorded").put_files(files)
    }
}

fn device() -> (Arc<Device>, Migrations) {
    let dir = tempfile::tempdir().expect("scratch");
    crate::thunderbird::fixture::write_sample_profile(&dir.path().join("home"));
    let device = Arc::new(Device {
        dir,
        kept: Arc::default(),
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
    run_as(migrations, &caller(), action, args)
}

/// Invoke `action` as `caller` and follow it to its end.
fn run_as(migrations: &Migrations, caller: &Caller, action: &str, args: Value) -> InvocationUpdate {
    let request = request(action, args);
    let id = request.invocation;
    answered(migrations.invoke(caller, request)).expect("admitted");
    follow_as(migrations, caller, id)
}

/// When a step taking longer than it ever should is given up on.
fn soon() -> Instant {
    Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("a time in reach")
}

/// How the invocation `id` ended.
fn follow(migrations: &Migrations, id: InvocationId) -> InvocationUpdate {
    follow_as(migrations, &caller(), id)
}

/// How the invocation `id` of `caller`'s ended.
fn follow_as(migrations: &Migrations, caller: &Caller, id: InvocationId) -> InvocationUpdate {
    let until = soon();
    loop {
        let state: InvocationState = migrations.observe(caller, &id).expect("known");
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
    let landed = device.actions().len();
    assert!(
        landed > 0,
        "the contacts landed through the workspace's actions"
    );

    // A second run finds everything there and adds nothing.
    let again = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&again)["already_there"], 2, "{again:?}");
    assert_eq!(device.actions().len(), landed);

    let status = run(&migrations, "status", json!({"source": "thunderbird"}));
    assert_eq!(value(&status)["state"], "complete");

    // The Migrations app's record followed every step, in the order they went.
    let told = device.told();
    let steps: Vec<&str> = told
        .iter()
        .map(String::as_str)
        .filter(|verb| *verb != "review_item")
        .collect();
    assert_eq!(
        steps,
        [
            "reviewed",
            "forget_items",
            "choose_keys",
            "begun",
            "landed",
            "ended",
            "begun",
            "landed",
            "ended"
        ],
        "{told:?}"
    );
    let kept = device.told_whole();
    let (path, reviewed) = &kept[0];
    assert_eq!(path, "so.tilo.migrate#reviewed");
    assert_eq!(reviewed["source"], "thunderbird");
    assert_eq!(reviewed["device"], hex::encode(DEVICE));
    let items = kept
        .iter()
        .filter(|(action, _)| action.ends_with("#review_item"));
    assert!(
        items
            .clone()
            .all(|(action, _)| action == "so.tilo.migrate/tl:so.tilo.migrate:m-1#review_item"),
        "each item is the migration's own"
    );
    assert_eq!(
        items
            .filter(|(_, item)| item["chosen"] == true)
            .map(|(_, item)| item["key"].clone())
            .collect::<Vec<_>>(),
        Vec::<Value>::new(),
        "a fresh review chooses nothing"
    );
    let chose = kept
        .iter()
        .find(|(action, _)| action.ends_with("#choose_keys"))
        .expect("chosen");
    assert_eq!(chose.1["keys"], json!([book]));
    let ended: Vec<&Value> = kept
        .iter()
        .filter(|(action, _)| action.ends_with("#ended"))
        .map(|(_, args)| &args["state"])
        .collect();
    assert_eq!(ended, [&json!("complete"), &json!("complete")]);
    let landed: Vec<(Value, Value)> = kept
        .iter()
        .filter(|(action, _)| action.ends_with("#landed"))
        .map(|(_, args)| (args["count"].clone(), args["already"].clone()))
        .collect();
    assert_eq!(landed, [(json!(2), json!(0)), (json!(0), json!(2))]);
}

/// A step that names another workspace the device is a member of lands there, kept apart from
/// the device's own; one naming a workspace the device is not in is refused, naming the command
/// that joins it.
#[test]
fn a_step_naming_a_workspace_lands_there_only_when_the_device_is_a_member() {
    let (device, migrations) = device();
    let refused = answered(migrations.invoke(
        &caller(),
        request(
            "review",
            json!({"source": "thunderbird", "workspace": "elsewhere"}),
        ),
    ));
    let Err(ActionCallError::Refused { reason }) = refused else {
        panic!("a workspace the device is not in is refused: {refused:?}")
    };
    assert!(reason.contains("tilo login"), "{reason}");
    assert!(reason.contains("elsewhere"), "{reason}");
    assert!(device.kept.reached.lock().expect("reached").is_empty());

    let into = json!({"source": "thunderbird", "workspace": LAB});
    let reviewed = run(&migrations, "review", into.clone());
    let book = address_book(value(&reviewed));
    let mut chose = into.clone();
    chose["items"] = json!([book]);
    chose["only"] = json!(true);
    run(&migrations, "choose", chose);
    let started = run(&migrations, "start", into);
    assert_eq!(value(&started)["state"], "complete", "{started:?}");
    let reached = device.kept.reached.lock().expect("reached").clone();
    assert!(
        !reached.is_empty() && reached.iter().all(|workspace| workspace == LAB),
        "every reach is the lab's: {reached:?}"
    );
    // The device's own workspace has no migration of thunderbird; the lab's is complete.
    let own = run(&migrations, "status", json!({"source": "thunderbird"}));
    let InvocationUpdate::Failed { error, .. } = &own else {
        panic!("nothing of thunderbird was reviewed for the device's own workspace: {own:?}")
    };
    assert!(error.message.contains("reviewed"), "{error:?}");
    let lab = run(
        &migrations,
        "status",
        json!({"source": "thunderbird", "workspace": LAB}),
    );
    assert_eq!(value(&lab)["state"], "complete", "{lab:?}");
}

/// `tilo migrate -w` steps the device's migrations into the workspace it names: its own by its
/// name, another it is a member of by its id, where every step that lands goes; a workspace it is
/// not in is refused by the device, naming the command that joins it.
#[test]
fn the_terminals_steps_land_in_the_workspace_it_names() {
    use super::caller::{Answering, Asks, Steps};

    let (device, migrations) = device();
    let migrations: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync> =
        Arc::new(migrations);
    let steps_in = |named: &str| {
        let mut steps = Steps::new(
            Arc::clone(&migrations),
            caller(),
            &DEVICE,
            OWN.to_owned(),
            Some("Studio".to_owned()),
        );
        steps.in_workspace(named).map(|()| steps)
    };
    let step = |steps: &Steps, action: &str, args: Value| {
        let Value::Object(args) = args else {
            panic!("arguments are an object")
        };
        tilo_rt::block_on(steps.step(
            action,
            args,
            &mut Answering {
                asks: Asks::Leave,
                prompt: &mut |_| panic!("nothing is asked"),
                say: &mut |_| {},
            },
        ))
        .expect("the runtime runs the step")
    };

    let into_lab = steps_in(LAB).expect("a workspace id is taken");
    step(&into_lab, "sources", json!({}))
        .expect("sources reaches the device")
        .expect("sources takes no workspace");
    device.kept.reached.lock().expect("reached").clear();
    step(&into_lab, "review", json!({"source": "thunderbird"}))
        .expect("review reaches the device")
        .expect("the review runs in the lab");
    let reached = device.kept.reached.lock().expect("reached").clone();
    assert!(
        !reached.is_empty() && reached.iter().all(|workspace| workspace == LAB),
        "{reached:?}"
    );

    let own = steps_in("studio").expect("the device's own workspace, by its name");
    let refused =
        step(&own, "status", json!({"source": "thunderbird"})).expect("status reaches the device");
    assert!(
        refused.is_err(),
        "the device's own workspace reviewed nothing: {refused:?}"
    );

    let elsewhere = "00000000-0000-4000-8000-000000000000";
    let refusal = format!(
        "{:#}",
        step(
            &steps_in(elsewhere).expect("a workspace id is taken"),
            "review",
            json!({"source": "thunderbird"}),
        )
        .expect_err("the device is in no such workspace")
    );
    assert!(refusal.contains("tilo login"), "{refusal}");
    assert!(refusal.contains(elsewhere), "{refusal}");
    let refusal = format!(
        "{:#}",
        steps_in("Lab")
            .err()
            .expect("no workspace the device knows is named Lab")
    );
    assert!(refusal.contains("tilo login"), "{refusal}");
}

#[test]
fn a_refused_start_is_what_the_migrations_record_says() {
    let (device, migrations) = device();
    run(&migrations, "review", json!({"source": "thunderbird"}));
    let refused = run(&migrations, "start", json!({"source": "thunderbird"}));
    let InvocationUpdate::Failed { error, .. } = &refused else {
        panic!("nothing is chosen: {refused:?}")
    };
    assert!(
        error.message.contains("Nothing of thunderbird is chosen"),
        "{error:?}"
    );
    let kept = device.told_whole();
    let (action, why) = kept.last().expect("told");
    assert_eq!(action, "so.tilo.migrate/tl:so.tilo.migrate:m-1#refused");
    assert!(
        why["why"]
            .as_str()
            .is_some_and(|why| why.contains("Nothing of thunderbird is chosen")),
        "{why}"
    );
    // No source has no record to say it in.
    let before = device.told().len();
    let unknown = run(&migrations, "start", json!({"source": "no such source"}));
    assert!(
        matches!(unknown, InvocationUpdate::Failed { .. }),
        "{unknown:?}"
    );
    assert_eq!(device.told().len(), before);
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
    assert!(device.actions().is_empty());
    // The migration's record says the run began and was stopped.
    let told = |verb: &str| -> Vec<Value> {
        device
            .kept
            .told
            .lock()
            .expect("told")
            .iter()
            .filter(|(action, _)| action.ends_with(&format!("#{verb}")))
            .map(|(_, args)| args.clone())
            .collect()
    };
    assert_eq!(told("ended")[0]["state"], "stopped", "{:?}", device.told());
    let run_id = told("begun")[0]["run"].clone();

    let resumed = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&resumed)["brought"]["contacts"], 2, "{resumed:?}");
    // Resumed, it is the same run, and the record follows it to the end.
    assert_eq!(told("begun")[1]["run"], run_id);
    assert_eq!(told("landed")[0]["run"], run_id);
    assert_eq!(told("ended")[1]["state"], "complete");
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
    let until = soon();
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

/// The key of the item of `kind` a review lists.
fn item_of(review: &Value, kind: &str) -> String {
    review["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["kind"] == kind)
        .and_then(|item| item["key"].as_str())
        .unwrap_or_else(|| panic!("no {kind} in {review:#}"))
        .to_owned()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one migration's flow, each step reading what the one before it left"
)]
fn the_person_and_their_agent_drive_one_migration_and_a_stopped_run_resumes_as_itself() {
    let (device, migrations) = device();
    let person = Caller::local_person("ada");
    let agent = Caller::local_agent("agent:ada-helper", "ada");

    // The agent reviews Thunderbird and chooses its address book only.
    let reviewed = run_as(
        &migrations,
        &agent,
        "review",
        json!({"source": "thunderbird"}),
    );
    let book = item_of(value(&reviewed), "contacts");
    let calendar = item_of(value(&reviewed), "calendar");
    run_as(
        &migrations,
        &agent,
        "choose",
        json!({"source": "thunderbird", "items": [book], "only": true}),
    );

    // The person reads the agent's choice and adds the calendar; the agent reads both chosen.
    let seen = run_as(
        &migrations,
        &person,
        "status",
        json!({"source": "thunderbird"}),
    );
    assert_eq!(value(&seen)["chosen"], json!([book]), "{seen:?}");
    run_as(
        &migrations,
        &person,
        "choose",
        json!({"source": "thunderbird", "items": [calendar]}),
    );
    let both = run_as(
        &migrations,
        &agent,
        "status",
        json!({"source": "thunderbird"}),
    );
    assert_eq!(value(&both)["chosen"], json!([book, calendar]), "{both:?}");

    // The person starts the run, and it is held once the address book is in.
    let (pause, moved) = &device.kept.pause;
    pause.lock().expect("pause").at = Some(2);
    let start = request("start", json!({"source": "thunderbird"}));
    let started = start.invocation;
    answered(migrations.invoke(&person, start)).expect("admitted");
    let (held, _) = moved
        .wait_timeout_while(
            pause.lock().expect("pause"),
            Duration::from_secs(30),
            |pause| !pause.held,
        )
        .expect("pause");
    assert!(held.held, "the run reached the workspace");
    drop(held);

    let going = run_as(
        &migrations,
        &agent,
        "status",
        json!({"source": "thunderbird"}),
    );
    assert_eq!(value(&going)["state"], "running", "{going:?}");

    // The agent cannot stop the person's run: their invocation is not its own.
    let refused = answered(migrations.cancel(&agent, &started));
    assert!(
        matches!(refused, Err(ActionCallError::UnknownInvocation)),
        "{refused:?}"
    );

    // The person stops it: it stops after the item it is on, keeping the address book.
    answered(migrations.cancel(&person, &started)).expect("cancelled");
    {
        let mut held = pause.lock().expect("pause");
        held.held = false;
        held.at = None;
    }
    moved.notify_all();
    let stopped = follow_as(&migrations, &person, started);
    assert!(
        matches!(stopped, InvocationUpdate::Cancelled { .. }),
        "{stopped:?}"
    );
    let after = run_as(
        &migrations,
        &agent,
        "status",
        json!({"source": "thunderbird"}),
    );
    let after = value(&after).clone();
    assert_eq!(after["state"], "stopped", "{after:#}");
    let run_id = after["run_id"].as_str().expect("a run").to_owned();
    assert!(
        device.actions().iter().all(|(run, _)| *run == run_id),
        "{:?}",
        device.actions()
    );
    assert!(
        !device
            .actions()
            .iter()
            .any(|(_, action)| action.starts_with("calendar")),
        "nothing of the calendar came before the stop: {:?}",
        device.actions()
    );

    // The agent starts it again: it is the same run, and it brings the rest.
    let resumed = run_as(
        &migrations,
        &agent,
        "start",
        json!({"source": "thunderbird"}),
    );
    let resumed = value(&resumed);
    assert_eq!(resumed["state"], "complete", "{resumed:#}");
    assert_eq!(
        resumed["run_id"],
        json!(run_id),
        "a resumed run is the run that stopped, so its undo takes all of it back"
    );
    assert!(
        resumed["brought"]["contacts"].as_u64() > Some(0),
        "{resumed:#}"
    );
    assert!(
        resumed["brought"]["events"].as_u64() > Some(0),
        "{resumed:#}"
    );
    assert!(
        device
            .actions()
            .iter()
            .any(|(run, action)| *run == run_id && action.starts_with("calendar")),
        "{:?}",
        device.actions()
    );

    // The person takes the run back: History undoes all of it, and the review stands again.
    let undone = run_as(&migrations, &person, "undo", json!({"run": run_id}));
    assert_eq!(value(&undone)["run_id"], json!(run_id), "{undone:?}");
    assert!(device.kept.undone.lock().expect("undone").contains(&run_id));
    let back = run_as(
        &migrations,
        &agent,
        "status",
        json!({"source": "thunderbird"}),
    );
    assert_eq!(value(&back)["state"], "reviewed", "{back:?}");
    // The migration's record forgets the run with it.
    let kept = device.told_whole();
    let (action, forgot) = kept.last().expect("told");
    assert!(action.ends_with("#forgot_run"), "{action}");
    assert_eq!(forgot["run"], json!(run_id));
}

#[test]
fn a_profile_handed_over_by_its_path_is_reviewed_and_brought_from_there() {
    let (device, migrations) = device();
    let elsewhere = device.dir.path().join("backup");
    crate::thunderbird::fixture::write_sample_profile(&elsewhere);
    let profile = std::fs::read_dir(elsewhere.join(".thunderbird"))
        .expect("the profiles")
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("prefs.js").is_file())
        .expect("a profile folder");
    // The home's own profile goes, so only the handed-over one can come over.
    std::fs::remove_dir_all(device.home().join(".thunderbird")).expect("the home's profile goes");

    let handed = profile.join("prefs.js");
    let reviewed = run(
        &migrations,
        "review",
        json!({"source": "thunderbird", "profile": handed.display().to_string()}),
    );
    let book = address_book(value(&reviewed));
    run(
        &migrations,
        "choose",
        json!({"source": "thunderbird", "items": [book], "only": true}),
    );
    let started = run(&migrations, "start", json!({"source": "thunderbird"}));
    assert_eq!(value(&started)["state"], "complete", "{started:?}");
    assert!(
        device
            .actions()
            .iter()
            .any(|(_, action)| action.starts_with("contacts")),
        "{:?}",
        device.actions()
    );
}

/// A key is never typed into the device: a `connect` not given one fails saying what it asks
/// for, so its caller asks the person and invokes it again with the key as its `secret`.
#[test]
fn a_connect_without_its_key_says_what_it_asks_for() {
    let (_, migrations) = device();
    let asked = run(&migrations, "connect", json!({"source": "linear"}));
    let InvocationUpdate::Failed { error, .. } = &asked else {
        panic!("a connect with no key does not connect: {asked:?}")
    };
    assert_eq!(error.code, super::SECRET_NEEDED, "{error:?}");
    let asks = error
        .details
        .as_ref()
        .and_then(|details| details["asks"].as_str())
        .unwrap_or_default();
    assert!(asks.starts_with("Linear API key"), "{error:?}");
    assert!(error.message.contains("Linear API key"), "{error:?}");
}

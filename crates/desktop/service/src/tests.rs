use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::{ApiError, ResourceSpec};
use tilo_resource::Effect;

use crate::lines::{self, Event, Remote, Request};
use crate::{Service, Services, Started, Worker, decode, offer, reading, unavailable};

/// A lamp's switch: the smallest service there is.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.lamp", catalogue = "tests/locales")]
enum Lamp {
    /// Turn the lamp on or off.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    SetOn {
        #[param("Whether it is on")]
        on: bool,
    },
    /// Dim the lamp.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Dim,
    /// Smash the lamp.
    #[effect(kind = "outside")]
    #[effect(own_device, destructive)]
    Smash,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct LampState {
    on: bool,
    dimmable: bool,
}

/// The lamp's device: records what it was told.
#[derive(Default)]
struct Bulb {
    told: Mutex<Vec<bool>>,
}

fn publish(readings: &BTreeMap<String, Value>) -> Vec<ResourceSpec> {
    let state: LampState = readings
        .get("lamp")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    vec![published(&state)]
}

fn published(state: &LampState) -> ResourceSpec {
    let mut resource = offer::<Lamp>(ResourceSpec::new("lamp", "test.lamp", "Lamp"));
    if !state.dimmable {
        unavailable(
            &mut resource,
            "dim",
            "not_dimmable",
            "This lamp does not dim",
        );
    }
    resource
}

struct LampService {
    state: LampState,
    worker: Worker<Bulb, bool>,
    bulb: Arc<Bulb>,
}

impl LampService {
    fn new() -> Self {
        let bulb = Arc::new(Bulb::default());
        Self {
            state: LampState::default(),
            worker: Worker::spawn("test-lamp", Arc::clone(&bulb), None).expect("worker"),
            bulb,
        }
    }
}

impl Service for LampService {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![published(&self.state)]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == "lamp").then(|| reading(&self.state))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        match decode::<Lamp>(&self.resources(), resource, action, arguments)? {
            Lamp::SetOn { on } => {
                self.worker.run(move |bulb: &Bulb| {
                    bulb.told.lock().expect("told").push(on);
                    on
                });
                Ok(Started::Accepted)
            }
            Lamp::Dim | Lamp::Smash => Ok(Started::Done(tilo_actions::ActionOutput::default())),
        }
    }

    fn poll(&mut self) -> bool {
        let landed = self.worker.landed();
        let changed = !landed.is_empty();
        if let Some(on) = landed.last() {
            self.state.on = *on;
        }
        changed
    }

    fn settle(&mut self) {
        if let Some(on) = self.worker.settle().last() {
            self.state.on = *on;
        }
    }
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// An action runs on the worker and its result lands in the reading the
/// service publishes; the action API checks the arguments before anything
/// runs, a device's refusal is published with why, and a destructive action
/// waits for the person.
#[test]
fn an_action_runs_and_is_refused_before_anything_happens() {
    let mut services = Services::default();
    let lamp = LampService::new();
    let bulb = Arc::clone(&lamp.bulb);
    services.add(Box::new(lamp));
    assert!(
        services
            .resources()
            .iter()
            .any(|resource| resource.actions.iter().any(|spec| spec.id == "set-on"))
    );
    let started = services
        .invoke("lamp", "set-on", &arguments(json!({"on": true})))
        .expect("the lamp publishes it")
        .expect("taken");
    assert_eq!(started, Started::Accepted);
    services.settle();
    assert_eq!(
        services.read("lamp"),
        Some(json!({"on": true, "dimmable": false}))
    );
    assert_eq!(*bulb.told.lock().expect("told"), [true]);

    let refused = services
        .invoke("lamp", "set-on", &arguments(json!({"on": "bright"})))
        .expect("the lamp publishes it");
    assert!(
        matches!(refused, Err(ApiError::InvalidArguments(_))),
        "{refused:?}"
    );
    assert!(services.invoke("lamp", "explode", &Map::new()).is_none());
    assert_eq!(
        bulb.told.lock().expect("told").len(),
        1,
        "nothing invalid ran"
    );

    let refused = services
        .invoke("lamp", "dim", &Map::new())
        .expect("the lamp publishes it");
    assert!(
        matches!(&refused, Err(ApiError::Unavailable { code, .. }) if code == "not_dimmable"),
        "{refused:?}"
    );

    assert_eq!(
        services.invoke("lamp", "smash", &Map::new()),
        Some(Err(ApiError::ConfirmationRequired))
    );
}

/// A service in another process is the same service: its readings arrive
/// as lines, the session checks invocations against specs built from them,
/// and what it writes is carried out there.
#[test]
fn a_service_over_lines_is_the_same_service() {
    let (sender, incoming, _wake) = lines::incoming();
    let mut service = LampService::new();
    let bulb = Arc::clone(&service.bulb);
    let requests = Arc::new(Mutex::new(Vec::<u8>::new()));

    // The session's first view: what the service wrote on start.
    let mut first = Vec::new();
    sender.send(lines::Incoming::Closed).expect("send");
    lines::serve(&mut service, &incoming, &mut first).expect("serve");
    let (events, received) = std::sync::mpsc::channel();
    for line in String::from_utf8(first).expect("utf-8").lines() {
        events
            .send(serde_json::from_str::<Event>(line).expect("an event"))
            .expect("send");
    }
    let mut remote = Remote::new(publish, received, Box::new(Shared(Arc::clone(&requests))));
    remote.wait_for_readings();
    assert_eq!(
        remote.read("lamp"),
        Some(json!({"on": false, "dimmable": false}))
    );

    assert!(
        remote.invoke("lamp", "dim", &Map::new()).is_err(),
        "unavailable here too"
    );
    assert_eq!(
        remote.invoke("lamp", "set-on", &arguments(json!({"on": true}))),
        Ok(Started::Accepted)
    );
    let written = String::from_utf8(requests.lock().expect("requests").clone()).expect("utf-8");
    assert_eq!(
        written.lines().count(),
        1,
        "only the valid invocation went out"
    );
    let request: Request = serde_json::from_str(written.trim()).expect("a request");

    // The service carries it out and writes the reading that changed.
    let (sender, incoming, _wake) = lines::incoming();
    sender
        .send(lines::Incoming::Line(
            serde_json::to_string(&request).expect("line"),
        ))
        .expect("send");
    sender.send(lines::Incoming::Closed).expect("send");
    service.settle();
    let mut out = Vec::new();
    lines::serve(&mut service, &incoming, &mut out).expect("serve");
    service.settle();
    assert_eq!(*bulb.told.lock().expect("told"), [true]);
}

struct Shared(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("shared").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A service in a child process: started, read once it wrote its first
/// readings, and stopped by closing its input when dropped.
#[test]
fn a_child_process_is_a_service() {
    let mut command = std::process::Command::new("sh");
    command.args([
        "-c",
        r#"echo '{"reading":{"resource":"lamp","value":{"on":true,"dimmable":true}}}'; cat > /dev/null"#,
    ]);
    let mut process = lines::Process::spawn(command, publish, None).expect("spawned");
    assert_eq!(
        process.read("lamp"),
        Some(json!({"on": true, "dimmable": true}))
    );
    assert_eq!(
        process.invoke("lamp", "dim", &Map::new()),
        Ok(Started::Accepted)
    );
    drop(process);
}

/// A service whose one child row says what it may do now.
struct RowService;

impl Service for RowService {
    fn resources(&self) -> Vec<ResourceSpec> {
        let mut root = offer::<Lamp>(ResourceSpec::new("lamp", "test.lamp", "Lamp"));
        root.children.push(offer::<Lamp>(ResourceSpec::new(
            "lamp:child",
            "test.lamp",
            "Child",
        )));
        vec![root]
    }

    fn read(&self, _resource: &str) -> Option<Value> {
        None
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
            resource, action,
        )))
    }

    fn row_status(&self, row: &str) -> Vec<tilo_actions::RowStatus> {
        (row == "lamp:child")
            .then(|| tilo_actions::RowStatus {
                action: "dim".to_owned(),
                availability: tilo_actions::Availability::Unavailable {
                    code: "foreign".to_owned(),
                    reason: "this one is not yours to dim".to_owned(),
                },
            })
            .into_iter()
            .collect()
    }
}

/// What a service says of one row reaches the catalogue the palette, agents
/// and scripts read, without it building a refused spec itself.
#[test]
fn a_services_row_status_reaches_its_published_tree() {
    let mut services = Services::default();
    services.add(Box::new(RowService));
    let published = services.resources();
    let child = published[0]
        .children
        .iter()
        .find(|child| child.id.as_str() == "lamp:child")
        .expect("the child is published");
    let dim = child
        .actions
        .iter()
        .find(|action| action.id == "dim")
        .expect("the child offers dim");
    assert_eq!(
        dim.contract.availability,
        tilo_actions::Availability::Unavailable {
            code: "foreign".to_owned(),
            reason: "this one is not yours to dim".to_owned(),
        }
    );
    let root = &published[0];
    assert!(
        root.actions
            .iter()
            .all(|action| action.contract.availability.is_available()),
        "a row's status never reaches the resource above it"
    );
}

/// A service that counts how often it is listed, and reports a change on its next poll once
/// told to.
struct Counted {
    id: &'static str,
    listed: Arc<std::sync::atomic::AtomicUsize>,
    moves: Arc<std::sync::atomic::AtomicBool>,
}

impl Service for Counted {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.listed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec![ResourceSpec::new(self.id, "test.counted", self.id)]
    }

    fn read(&self, _resource: &str) -> Option<Value> {
        None
    }

    fn invoke(
        &mut self,
        _resource: &str,
        _action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Ok(Started::Done(tilo_actions::ActionOutput::default()))
    }

    fn poll(&mut self) -> bool {
        self.moves.swap(false, std::sync::atomic::Ordering::Relaxed)
    }
}

/// A reading that lands on one service moves the catalogue's revision and lists that service
/// again, and no other: the session reads the catalogue whenever its revision moves, and one busy
/// service must not cost every other service's list each time.
#[test]
fn a_reading_lists_again_only_the_service_it_landed_on() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let (busy, still) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let moves = Arc::new(AtomicBool::new(false));
    let mut services = Services::default();
    services.add(Box::new(Counted {
        id: "busy",
        listed: Arc::clone(&busy),
        moves: Arc::clone(&moves),
    }));
    services.add(Box::new(Counted {
        id: "still",
        listed: Arc::clone(&still),
        moves: Arc::new(AtomicBool::new(false)),
    }));
    // What is published, by the resources' ids and labels.
    let published = |services: &Services| -> Vec<(String, String)> {
        services
            .resources()
            .into_iter()
            .map(|resource| (resource.id.0, resource.label))
            .collect()
    };
    let first = published(&services);
    assert_eq!(
        (busy.load(Ordering::Relaxed), still.load(Ordering::Relaxed)),
        (1, 1),
        "the first read lists each service once"
    );

    let before = services.revision();
    assert!(!services.poll(), "nothing landed");
    assert_eq!(
        services.revision(),
        before,
        "a poll that took nothing in moves nothing"
    );
    assert_eq!(published(&services), first, "what is published stands");
    assert_eq!(
        still.load(Ordering::Relaxed),
        1,
        "an unmoved catalogue is read from what was listed"
    );

    moves.store(true, Ordering::Relaxed);
    assert!(services.poll(), "the busy service took a reading in");
    assert_ne!(
        services.revision(),
        before,
        "a reading moves the catalogue's revision"
    );
    assert_eq!(
        published(&services),
        first,
        "the same resources are published"
    );
    assert_eq!(
        (busy.load(Ordering::Relaxed), still.load(Ordering::Relaxed)),
        (2, 1),
        "only the service the reading landed on is listed again"
    );

    services.settle();
    let _ = services.resources();
    assert_eq!(
        (busy.load(Ordering::Relaxed), still.load(Ordering::Relaxed)),
        (3, 2),
        "a settle, which may change every service, lists each again"
    );
}

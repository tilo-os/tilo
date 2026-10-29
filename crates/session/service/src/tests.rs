use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::{DoorError, ResourceSpec};
use tilo_resource::Effect;

use crate::lines::{self, Event, Remote, Request};
use crate::{Service, Services, Started, Worker, decode, offer, reading, unavailable};

/// A lamp's switch: the smallest service there is.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.lamp")]
enum Lamp {
    /// Turn the lamp on or off.
    #[effect(own_device, idempotent)]
    SetOn {
        #[param("Whether it is on")]
        on: bool,
    },
    /// Dim the lamp.
    #[effect(own_device, idempotent)]
    Dim,
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
    ) -> Result<Started, DoorError> {
        match decode::<Lamp>(&self.resources(), resource, action, arguments)? {
            Lamp::SetOn { on } => {
                self.worker.run(move |bulb: &Bulb| {
                    bulb.told.lock().expect("told").push(on);
                    on
                });
                Ok(Started::Accepted)
            }
            Lamp::Dim => Ok(Started::Done(tilo_actions::ActionOutput::default())),
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
/// service publishes; the door checks the arguments before anything runs.
#[test]
fn an_action_lands_in_the_reading() {
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
        matches!(refused, Err(DoorError::InvalidArguments(_))),
        "{refused:?}"
    );
    assert!(services.invoke("lamp", "explode", &Map::new()).is_none());
    assert_eq!(
        bulb.told.lock().expect("told").len(),
        1,
        "nothing invalid ran"
    );
}

/// An action the device cannot do now is published unavailable, and
/// invoking it is refused with why.
#[test]
fn an_unavailable_action_is_refused() {
    let mut services = Services::default();
    services.add(Box::new(LampService::new()));
    let refused = services
        .invoke("lamp", "dim", &Map::new())
        .expect("the lamp publishes it");
    assert!(
        matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "not_dimmable"),
        "{refused:?}"
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

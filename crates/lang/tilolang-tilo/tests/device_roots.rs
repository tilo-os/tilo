//! `device root readings: Readings`: one root for each device, kept under that device's resources
//! (`devices/<device>/<app>/root-<type id>`). What a device read of its own stays that device's:
//! another device of the same person, and every other member, keeps its own and reads none of it.
//! What people share stays in an ordinary root.

use tilolang_runtime::{Actor, ResourceRef, Value};

use crate::fixtures::{Replica, Workspace, action_id, type_id};

const APP: &str = "so.tilo.test.station";

const STATION: (&str, &str) = (
    r#"app so.tilo.test.station "Station"

type Place {
    name: Text = ""
}

/// One thing this device read, and when it did.
type Sample {
    name: Text = ""
    fetched: Text = ""
}

/// What this device read of the places it follows.
type Readings {
    readings: Collection<Sample>
}

/// The places people follow together.
type Places {
    places: Collection<Place>
}

root places: Places
device root readings: Readings

default view places_window for Places

action follow(p: Places, name: Text) {
    p.places.insert(Place(name: name))
}

/// Reads the places here, on this device: what it read stays this device's.
action read_here(p: Places, at: Text) {
    let mine = readings
    for place in p.places {
        let who = place.name
        mine.readings.insert(Sample(name: who, fetched: at))
    }
}

action read_names(p: Places) -> Text {
    let mine = readings
    let out = ""
    for reading in mine.readings {
        out = out + reading.name + ","
    }
    return out
}

view places_window(p: Places) {
    ui.Text("Places")
}
"#,
    "",
);

fn person(name: &str) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(name))
}

/// The reference to the device `device`'s readings: the document the root stands at there.
fn readings_of(device: &str, meta: &tilolang_runtime::meta::Meta) -> ResourceRef {
    ResourceRef::root(
        format!("devices/{device}/{APP}/root-{}", type_id(meta, "Readings")),
        type_id(meta, "Readings"),
    )
}

fn invoke(
    replica: &Replica,
    meta: &tilolang_runtime::meta::Meta,
    action: &str,
    args: Vec<Value>,
) -> Value {
    let out = replica
        .host
        .runtime()
        .invoke_action(
            &person(&replica.name),
            (action_id(meta, action).as_str(), args),
        )
        .unwrap();
    match out {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("`{action}` answers {other:?}"),
    }
}

#[test]
fn each_device_keeps_its_own_readings_and_people_share_the_places() {
    let (_, probe) = Replica::new("probe", Workspace::new(), &[STATION]);
    let meta = probe[0].meta.clone();
    let packages = [(STATION.0, probe[0].ids.as_str())];
    let base = Workspace::new();
    let (first, _) = Replica::on_device("ada", "ada-1", base.fork(), &packages);
    let (second, _) = Replica::on_device("ada", "ada-2", base.fork(), &packages);

    let places = first.root(APP);
    second.root(APP);
    first.commit();
    assert!(
        first.host.store().contains(&readings_of("ada-1", &meta)),
        "opening the app makes this device's readings"
    );
    assert_eq!(
        places.doc,
        format!("root-{}", type_id(&meta, "Places")),
        "what people share stays one root of the workspace"
    );

    // A place one device follows is the workspace's, once both have seen it.
    invoke(
        &first,
        &meta,
        "follow",
        vec![Value::resource(&places), Value::text("Berlin")],
    );
    first.commit();

    // What one device reads stays on that device.
    invoke(
        &first,
        &meta,
        "read_here",
        vec![Value::resource(&places), Value::text("nine")],
    );
    first.commit();
    assert_eq!(
        invoke(&first, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text("Berlin,"),
        "the device reads what it read"
    );
    assert_eq!(
        invoke(&second, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text(""),
        "and the other device reads nothing of it"
    );
    assert_eq!(
        second.host.store().contains(&readings_of("ada-1", &meta)),
        false,
        "the other device holds no copy of this one's readings"
    );

    // Once it has seen the first device's, it reads the shared place itself, and each keeps its
    // own answer.
    first.sync_with(&second);
    assert_eq!(
        invoke(&second, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text(""),
        "a copy of the other device's readings is not this device's"
    );
    invoke(
        &second,
        &meta,
        "read_here",
        vec![Value::resource(&places), Value::text("four")],
    );
    second.commit();
    assert_eq!(
        invoke(&second, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text("Berlin,"),
        "the second device reads its own reading"
    );
    assert_eq!(
        read_when(&first, "ada-1", &meta),
        "nine",
        "and the first device's reading is still the one it made"
    );
    assert_eq!(
        read_when(&second, "ada-2", &meta),
        "four",
        "each device's reading says when that device read it"
    );
    assert_ne!(
        readings_of("ada-1", &meta).id_string(),
        readings_of("ada-2", &meta).id_string()
    );
}

/// When the device `device` read the place it follows, as its own reading says.
fn read_when(replica: &Replica, device: &str, meta: &tilolang_runtime::meta::Meta) -> String {
    let mine = readings_of(device, meta);
    let Value::List(readings) = replica.read(&mine, "readings") else {
        panic!("readings are a list")
    };
    let Some(Value::Ref(held)) = readings.first() else {
        return String::new();
    };
    match replica.read(&held.target, "fetched") {
        Value::Text(text) => text,
        other => panic!("fetched is {other:?}"),
    }
}

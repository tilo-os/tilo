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
type Reading {
    name: Text = ""
    fetched: Text = ""
}

/// What this device read of the places it follows.
type Readings {
    readings: Collection<Reading>
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
        mine.readings.insert(Reading(name: place.name, fetched: at))
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
    assert_eq!(
        places.doc, "root-Places",
        "what people share stays one root of the workspace"
    );
    invoke(
        &first,
        &meta,
        "follow",
        vec![Value::resource(&places), Value::text("Berlin")],
    );
    first.commit();
    assert_eq!(
        invoke(&second, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text(""),
        "the second device read nothing, though it follows the same place"
    );

    let mine = readings_of("ada-1", &meta);
    assert_eq!(
        first.read(&mine, "readings").kind_name(),
        "list",
        "the device's readings are a document of their own, under the device"
    );
    assert_eq!(
        second.host.store().contains(&mine),
        false,
        "the other device holds no readings of this one"
    );

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

    invoke(
        &second,
        &meta,
        "read_here",
        vec![Value::resource(&places), Value::text("four")],
    );
    second.commit();
    first.sync_with(&second);
    assert_eq!(
        invoke(&second, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text("Berlin,"),
        "the second device's own reading, made of the plants, sits beside the first's"
    );
    assert_eq!(
        invoke(&first, &meta, "read_names", vec![Value::resource(&places)]),
        Value::text("Berlin,"),
        "the first device still reads its own"
    );
    assert_ne!(
        readings_of("ada-1", &meta).id_string(),
        readings_of("ada-2", &meta).id_string()
    );
}

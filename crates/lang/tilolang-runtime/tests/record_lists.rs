//! Records inside records: a field whose type is a `List` of a record type holds record values,
//! which a record under construction stores with it, that JSON text reads, and that a find over
//! the collection an action just wrote into leaves alone.

#[cfg(test)]
mod cases {
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, ResourceRef, Runtime, Value};

    const SOURCE: &str = r#"app so.tilo.trips "Trips"

type Step {
    instruction: Text
    distance: Float = 0.0
}

type Trip {
    from_name: Text = ""
    steps: List<Step>
}

type Board {
    trips: Collection<Trip>
}

root Board

default view board for Board

view board(b: Board) {
    text("{b.trips.len()}")
}

action plan(b: Board, from: Text) {
    b.trips.insert(
        Trip(
            from_name: from,
            steps: [
                Step(instruction: "Go straight", distance: 1.5),
                Step(instruction: "Arrive"),
            ],
        ),
    )
}

action land(b: Board, text: Text) -> Text {
    match json.decode<Trip>(text) {
        .ok(trip) => {
            b.trips.insert(trip)
            ""
        }
        .err(why) => why
    }
}

action take_in(b: Board, names: List<Text>) -> Int {
    for name in names {
        if let known = b.trips.find(t -> t.from_name == name) {
            known.from_name = "{name} again"
        } else {
            b.trips.insert(Trip(from_name: name))
        }
    }
    b.trips.len()
}
"#;

    struct Trips {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Trips {
        fn new() -> Self {
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            let app = App::compile(SOURCE, &[]);
            app.load(&rt);
            let board = rt.create_root("Board").unwrap();
            Self { rt, app, board }
        }

        fn call(&self, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|error| panic!("{action}: {error:?}"))
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt.read(at, field).unwrap()
        }

        fn trips(&self) -> Vec<ResourceRef> {
            match self.read(&self.board, "trips") {
                Value::List(items) => items
                    .into_iter()
                    .map(|item| match item {
                        Value::Ref(held) => Rc::unwrap_or_clone(held.target),
                        other => panic!("a trip reads {other:?}"),
                    })
                    .collect(),
                other => panic!("trips reads {other:?}"),
            }
        }

        /// The record values a trip's `steps` holds, by the instruction each says.
        fn steps(&self, trip: &ResourceRef) -> Vec<String> {
            let Value::List(items) = self.read(trip, "steps") else {
                panic!("steps is no list");
            };
            let instruction = self.field_id("Step", "instruction");
            items
                .into_iter()
                .map(|item| match item {
                    Value::Rec(record) => match record.fields.get(&instruction) {
                        Some(Value::Text(said)) => said.clone(),
                        other => panic!("a step's instruction reads {other:?}"),
                    },
                    other => panic!("a step reads {other:?}"),
                })
                .collect()
        }

        fn field_id(&self, ty: &str, field: &str) -> String {
            let meta = self.app.meta();
            meta.types
                .iter()
                .find(|declared| declared.name == ty)
                .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
                .map_or_else(|| panic!("{ty} has no field {field}"), |f| f.id.clone())
        }

        fn names(&self) -> Vec<String> {
            self.trips()
                .iter()
                .map(|trip| match self.read(trip, "from_name") {
                    Value::Text(name) => name,
                    other => panic!("from_name reads {other:?}"),
                })
                .collect()
        }
    }

    #[test]
    fn a_record_holding_a_list_of_records_is_stored_and_read_back() {
        let t = Trips::new();
        t.call("plan", vec![Value::resource(&t.board), Value::text("here")]);
        let trips = t.trips();
        assert_eq!(trips.len(), 1);
        assert_eq!(t.names(), ["here"]);
        assert_eq!(
            t.steps(&trips[0]),
            ["Go straight", "Arrive"],
            "the steps the trip was written with come back as the records they are"
        );
    }

    #[test]
    fn json_text_reads_a_record_holding_a_list_of_records() {
        let t = Trips::new();
        let text = r#"{"from_name":"there","steps":[{"instruction":"Turn left"},{"instruction":"Arrive"}]}"#;
        let out = t.call("land", vec![Value::resource(&t.board), Value::text(text)]);
        assert_eq!(out, InvokeOutcome::Done(Value::text("")), "{out:?}");
        let trips = t.trips();
        assert_eq!(trips.len(), 1);
        assert_eq!(t.names(), ["there"]);
        assert_eq!(t.steps(&trips[0]), ["Turn left", "Arrive"]);
    }

    #[test]
    fn a_find_leaves_a_record_the_same_action_just_inserted_alone() {
        let t = Trips::new();
        let out = t.call(
            "take_in",
            vec![
                Value::resource(&t.board),
                Value::List(vec![Value::text("one"), Value::text("two")]),
            ],
        );
        assert_eq!(out, InvokeOutcome::Done(Value::Int(2)), "{out:?}");
        assert_eq!(
            t.names(),
            ["one", "two"],
            "the second name is a trip of its own, not the first one written over"
        );
        let out = t.call(
            "take_in",
            vec![
                Value::resource(&t.board),
                Value::List(vec![Value::text("one")]),
            ],
        );
        assert_eq!(out, InvokeOutcome::Done(Value::Int(2)), "nothing new");
        assert_eq!(t.names(), ["one again", "two"], "the known trip is changed");
    }
}

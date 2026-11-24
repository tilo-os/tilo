//! What a view hands a call: a record as a view's argument, a number a map keeps and gives back,
//! and an action whose files the person's own chooser fills.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, Runtime, SimpleAuthority, Value};

    /// The package `source` compiled and its view `view` opened over its root, as a window does:
    /// whether the view ran.
    fn opens(source: &str, view: &str) -> Result<(), String> {
        let app = App::compile(source, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).expect("a runtime");
        app.load(&rt);
        let root = rt.create_root("T").expect("a root");
        rt.open_view(&me(), &app.view(view), vec![Value::resource(&root)])
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// A map keeps a number the way the handle imports carry it, and a read gives the number
    /// back: `??` on a read is the entry, or the fallback where the key holds none.
    #[test]
    fn a_map_read_of_a_number_gives_the_number_back() {
        let source = r#"app so.tilo.t "T"

type T {
    n: Float
}

root T

default view t for T

fn store() -> Map<Float> {
    let m = Map<Float>()
    m.put("a", 1.5)
    m
}

fn entry() -> Float {
    let m = store()
    m.get("a") ?? 0.0
}

fn absent() -> Float {
    let m = store()
    m.get("b") ?? 2.5
}

view t(r: T) {
    ui.Text("{entry()} {absent()}")
}
"#;
        opens(source, "t").expect("a map's entries read as the values they hold");
    }

    /// A view called with a record made in the caller hands the record on: its arguments travel
    /// as the node's `arg.<name>` properties, which the runtime gives the view itself.
    #[test]
    fn a_view_takes_a_record_made_in_the_caller() {
        let source = r#"app so.tilo.t "T"

type T {
    n: Int
}

type Count {
    n: Int
}

root T

default view t for T

view t(r: T) {
    let c = Count(n: 2)
    shown(c)
}

view shown(c: Count) {
    ui.Text("{c.n}")
}
"#;
        opens(source, "t").expect("the record reaches the view it is passed to");
    }

    /// A loop passes a record from around it to its rows' view: a row is a view too, and the
    /// record reaches it as the loop's `arg.<name>` property.
    #[test]
    fn a_loop_hands_its_rows_a_record_from_around_it() {
        let source = r#"app so.tilo.t "T"

type T {
    things: Collection<Thing>
}

type Thing {
    n: Int
}

type Count {
    n: Int
}

root T

default view t for T

default view entry for Thing

view entry(t: Thing) {
    ui.Text("{t.n}")
}

view t(r: T) {
    let c = Count(n: 2)
    std.list(r.things) { (item, at) ->
        ui.ListItem(item: item, presence: at) {
            ui.Text("{item.n} of {c.n}")
        }
    }
}
"#;
        opens(source, "t").expect("the record reaches the rows' view");
    }

    /// An action whose `List<File>` the person's chooser fills runs from a button with the other
    /// arguments bound: the reference is the action's, and no call fills the files.
    #[test]
    fn an_action_whose_files_a_chooser_fills_takes_the_rest() {
        let source = r#"app so.tilo.t "T"

type T {
    n: Int
}

root T

default view t for T

action import_all(t: T, files: List<File>) (did: "imported pictures") {
}

view t(r: T) {
    ui.Button("Import") {
        on press: file.choose_folder(import_all(r), accept: ["image/png", "image/jpeg"])
    }
}
"#;
        opens(source, "t").expect("the chooser fills the files, and the button binds the rest");
    }
}

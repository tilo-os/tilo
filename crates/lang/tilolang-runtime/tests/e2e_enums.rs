//! Enums, unions and view state end to end: enum values and union references in fields, through sync
//! and in views, and view state bound to controls.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        Actor, AmStore, EditOutcome, EnumValue, ResourceRef, Runtime, SimpleAuthority, Value,
    };

    const SOURCE: &str = r#"app so.tilo.shapes "Shapes"

enum Kind {
    dot
    line
    big
}

type Line {
    len: Int
}

type Box {
    w: Float
    label: Text
}

type Shape = Line | Box

type Canvas {
    kind: Kind
    last: Option<Kind>
    shapes: Collection<Shape>
    shape: Option<Shape> = .none
}

root Canvas

default view drawing for Canvas

action draw(c: Canvas, len: Int) {
    c.last = .some(c.kind)
    c.kind = if len > 5 { .big } else { .line }
    if len > 5 {
        c.shape = c.shapes.insert(Box(w: 1.5, label: "big"))
    } else {
        c.shape = c.shapes.insert(Line(len: len))
    }
}

fn describe(s: Shape) -> Text {
    match s {
        l: Line => "a line of {l.len}",
        b: Box => "{b.label} {b.w}",
    }
}

view drawing(c: Canvas) {
    state query: Text
    state zoom: Float = 1.0
    page {
        if let s = c.shape {
            text(describe(s))
        } else {
            text("a dot")
        }
        if c.last == .some(Kind.dot) {
            text("was a dot")
        }
        input(bind: query, placeholder: "Find")
        ui.NumberInput(bind: zoom)
        text("{query} at {zoom}")
    }
}
"#;

    fn setup(store: &AmStore) -> (Runtime, App, ResourceRef) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(store.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let canvas = rt.create("Canvas").unwrap();
        (rt, app, canvas)
    }

    fn draw(rt: &Runtime, app: &App, canvas: &ResourceRef, len: i64) {
        let id = app.action("draw");
        rt.invoke_action(
            &me(),
            (id.as_str(), vec![Value::resource(canvas), Value::Int(len)]),
        )
        .unwrap();
    }

    fn texts(v: &tilolang_runtime::ViewInstance) -> Vec<String> {
        of_kind(v.tree(), "ui.Text")
            .into_iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    // An enum field holds one case and merges as a register; a union field holds a reference
    // to a resource of any of its types, and a collection of a union owns items of each type.
    #[test]
    fn enum_and_union_fields_hold_whole_values_and_sync_as_registers() {
        let (mut store_a, mut store_b) = (AmStore::new(), AmStore::new());
        let (rt_a, app, canvas) = setup(&store_a);
        let ids = &app.compiled.ids;
        let id = |name: &str| ids.get(name).unwrap().to_owned();
        assert_eq!(
            rt_a.read(&canvas, "kind").unwrap(),
            Value::Enum(EnumValue {
                ty: id("Kind"),
                variant: id("Kind.dot"),
                fields: std::collections::BTreeMap::default(),
            }),
            "an enum field starts as its first case"
        );
        draw(&rt_a, &app, &canvas, 3);
        let Value::Opt(Some(line)) = rt_a.read(&canvas, "shape").unwrap() else {
            panic!("a shape");
        };
        let Value::Ref(line) = *line else {
            panic!("a reference")
        };
        assert_eq!(line.target.ty, id("Line"), "a reference to a Line");
        let rt_b = Runtime::new(store_b.clone(), SimpleAuthority::new()).unwrap();
        app.load(&rt_b);
        store_a.sync_with(&mut store_b).unwrap();
        draw(&rt_a, &app, &canvas, 9);
        draw(&rt_b, &app, &canvas, 2);
        store_a.sync_with(&mut store_b).unwrap();
        let (on_a, on_b) = (
            rt_a.read(&canvas, "shape").unwrap(),
            rt_b.read(&canvas, "shape").unwrap(),
        );
        assert_eq!(
            on_a, on_b,
            "concurrent writes converge on one whole reference"
        );
        let Value::List(items) = rt_a.read(&canvas, "shapes").unwrap() else {
            panic!("a list");
        };
        let mut types: Vec<String> = items
            .iter()
            .map(|v| match v {
                Value::Ref(r) => r.target.ty.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        types.sort();
        let mut want = vec![id("Line"), id("Line"), id("Box")];
        want.sort();
        assert_eq!(types, want, "each item is read as its own type");
    }

    #[test]
    fn views_match_on_enum_values_and_compare_options_of_them() {
        let store = AmStore::new();
        let (rt, app, canvas) = setup(&store);
        let mut v = rt
            .open_view(&me(), &app.view("drawing"), vec![Value::resource(&canvas)])
            .unwrap();
        assert_eq!(texts(&v)[0], "a dot");
        draw(&rt, &app, &canvas, 9);
        v.poll_patches().unwrap();
        assert_eq!(texts(&v)[..2], ["big 1.5", "was a dot"]);
        draw(&rt, &app, &canvas, 4);
        v.poll_patches().unwrap();
        assert_eq!(texts(&v)[0], "a line of 4");
        assert!(!texts(&v).contains(&"was a dot".to_owned()));
    }

    tilo_test_env::scenario! {
        fn bound_state_and_drafts_follow_each_press_and_keystroke;

        phase a_bound_state_value_is_each_persons_own;
        {
            let store = AmStore::new();
            let (rt, app, canvas) = setup(&store);
            let (ada, bo) = (person("ada"), person("bo"));
            let open = |who: &Actor| {
                rt.open_view(who, &app.view("drawing"), vec![Value::resource(&canvas)])
                    .unwrap()
            };
            let (mut va, mut vb) = (open(&ada), open(&bo));
            let before = rt.history(&canvas.doc).unwrap().len();
            let inputs = |v: &tilolang_runtime::ViewInstance| -> Vec<String> {
                of_kind(v.tree(), "ui.TextInput")
                    .into_iter()
                    .map(|i| i.key.clone())
                    .collect()
            };
            let numbers = |v: &tilolang_runtime::ViewInstance| -> Vec<String> {
                of_kind(v.tree(), "ui.NumberInput")
                    .into_iter()
                    .map(|i| i.key.clone())
                    .collect()
            };
            let query = inputs(&va)[0].clone();
            let zoom = numbers(&va)[0].clone();
            assert_eq!(va.bound_value(&query), Some(Value::text("")));
            assert_eq!(
                va.bound_value(&zoom),
                Some(Value::Float(1.0)),
                "the default shows"
            );

            assert_eq!(
                va.edit(&query, &Value::text("red")).unwrap(),
                EditOutcome::Applied
            );
            assert_eq!(
                va.edit(&zoom, &Value::text("3")).unwrap(),
                EditOutcome::Applied,
                "a number typed as text"
            );
            assert!(va.edit(&zoom, &Value::text("big")).is_err());
            va.poll_patches().unwrap();
            vb.poll_patches().unwrap();
            assert_eq!(texts(&va).last().unwrap(), "red at 3");
            assert_eq!(
                texts(&vb).last().unwrap(),
                " at 1",
                "Bo's window shows his own zoom, and his own view state"
            );
            assert_eq!(vb.bound_value(&inputs(&vb)[0]), Some(Value::text("")));
            assert_eq!(
                vb.bound_value(&numbers(&vb)[0]),
                Some(Value::Float(1.0)),
                "Bo reads the window value's default, not Ada's"
            );
            assert_eq!(
                rt.history(&canvas.doc).unwrap().len(),
                before,
                "view state is not resource state"
            );
        }

        phase a_toggle_pressed_twice_before_the_view_draws_again_toggles_twice;
        // An argument computed from view state is evaluated when the control is pressed, on the
        // view state as it is then: a second press before the view draws again sees the first.
        {
            let (_rt, _app, _talk, mut view) = pressed();
            let (toggle, more) = (button(&view, "Toggle"), button(&view, "More"));
            assert_eq!(texts(&view)[..2], ["shut", "0 more"]);
            view.press(&toggle).unwrap();
            view.poll_patches().unwrap();
            assert_eq!(texts(&view)[0], "open");
            // Both presses arrive in one frame: the view has not drawn between them.
            view.press(&toggle).unwrap();
            view.press(&toggle).unwrap();
            view.press(&more).unwrap();
            view.press(&more).unwrap();
            view.poll_patches().unwrap();
            assert_eq!(texts(&view)[..2], ["open", "2 more"]);
            view.press(&toggle).unwrap();
            view.poll_patches().unwrap();
            assert_eq!(texts(&view)[0], "shut");
        }

        phase an_argument_computed_from_a_draft_sends_the_last_keystroke;
        {
            let (rt, _app, talk, mut view) = pressed();
            let draft = of_kind(view.tree(), "ui.TextInput")[0].key.clone();
            let send = button(&view, "Send");
            view.edit(&draft, &Value::text("on my wa")).unwrap();
            view.poll_patches().unwrap();
            // The last keystroke and the press arrive together: the view has not drawn since.
            view.edit(&draft, &Value::text("on my way")).unwrap();
            view.press(&send).unwrap();
            assert_eq!(rt.read(&talk, "said").unwrap(), Value::text("on my way!"));
        }
    }

    const PRESSED: &str = r#"app so.tilo.pressed "Pressed"

type Talk {
    said: Text
}

root Talk

default view talk for Talk

action set_said(t: Talk, s: Text) {
    t.said = s
}

view talk(t: Talk) {
    state open: Bool = false
    state draft: Text
    state n: Int = 0
    page {
        text(if open { "open" } else { "shut" })
        text("{n} more")
        button("Toggle", on: set(open, not open))
        button("More", on: set(n, n + 1))
        input(bind: draft)
        button("Send", on: set_said(t, "{draft}!"))
    }
}
"#;

    fn pressed() -> (Runtime, App, ResourceRef, tilolang_runtime::ViewInstance) {
        let app = App::compile(PRESSED, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let talk = rt.create("Talk").unwrap();
        let view = rt
            .open_view(&me(), &app.view("talk"), vec![Value::resource(&talk)])
            .unwrap();
        (rt, app, talk, view)
    }

    fn button(view: &tilolang_runtime::ViewInstance, label: &str) -> String {
        of_kind(view.tree(), "ui.Button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text(label)))
            .unwrap_or_else(|| panic!("no button `{label}` in\n{}", dump(view.tree())))
            .key
            .clone()
    }

    // A text field the new item was not given starts as the words its type declares, as a case
    // field starts as its declared case: a control bound to it shows them, never an empty field.
    #[test]
    fn a_new_items_text_field_starts_as_its_declared_words() {
        const ROOMS: &str = r#"app so.tilo.rooms "Rooms"

type Booking {
    room: Text = "Studio"
    note: Text
}

type Calendar {
    bookings: Collection<Booking>
}

root Calendar

default view calendar for Calendar

view calendar(d: Calendar) {
    text("Bookings")
}

action book(d: Calendar) {
    d.bookings.insert(Booking(note: "Bring the key"))
}
"#;
        let store = AmStore::new();
        let app = App::compile(ROOMS, &[]);
        let rt = Runtime::new(store, SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let calendar = rt.create("Calendar").unwrap();
        rt.invoke_action(
            &me(),
            (
                app.action("book").as_str(),
                vec![Value::resource(&calendar)],
            ),
        )
        .unwrap();
        let Value::List(bookings) = rt.read(&calendar, "bookings").unwrap() else {
            panic!("a list");
        };
        let Some(Value::Ref(booking)) = bookings.first() else {
            panic!("the booking");
        };
        assert_eq!(
            rt.read(&booking.target, "room").unwrap(),
            Value::text("Studio"),
            "the room it was not given is the one its type declares"
        );
        assert_eq!(
            rt.read(&booking.target, "note").unwrap(),
            Value::text("Bring the key"),
            "the words it was given stand"
        );
    }

    // A root is a record of its type too: what its fields start as is the words their type
    // declares, so a control bound to one shows them from the window's first frame.
    #[test]
    fn a_new_roots_text_field_starts_as_its_declared_words() {
        const FOLDERS: &str = r#"app so.tilo.folders "Folders"

type Folder {
    label: Text = "Studio"
    note: Text
}

root Folder

default view folders for Folder

view folders(f: Folder) {
    text(f.label)
    text(f.note)
    ui.Dropdown(bind: f.label, options: ["Studio", "Annex"], placeholder: "Choose one")
}
"#;
        let app = App::compile(FOLDERS, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let folder = rt.create("Folder").unwrap();
        let mut view = rt
            .open_view(&me(), &app.view("folders"), vec![Value::resource(&folder)])
            .unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            texts(&view),
            ["Studio", ""],
            "the label the root was not given is the one its type declares, and a text whose type declares nothing starts empty: {}",
            dump(view.tree())
        );
        assert_eq!(
            rt.read(&folder, "label").unwrap(),
            Value::text("Studio"),
            "the root holds what its type declares"
        );
        assert_eq!(
            rt.read(&folder, "note").unwrap(),
            Value::text(""),
            "a text whose type declares nothing starts empty"
        );
    }
    /// The cases of an enum are elements of a set and of a map's sets, and values of a map:
    /// added, removed and read back as the cases they are, so an alarm keeps the days it rings.
    #[test]
    fn enum_cases_are_kept_in_sets_and_maps() {
        const ALARMS: &str = r#"app so.tilo.alarms "Alarms"

enum Weekday {
    mon
    tue
    wed
}

type Alarm {
    days: Set<Weekday>
    by_person: Map<Set<Weekday>>
    first: Map<Weekday>
}

root Alarm

default view alarm for Alarm

action ring_on(a: Alarm, d: Weekday) {
    a.days.add(d)
    a.by_person.add("ada", d)
    a.first.put("ada", d)
}

action quiet_on(a: Alarm, d: Weekday) {
    a.days.remove(d)
    a.by_person.remove("ada", d)
}

fn said(d: Weekday) -> Text {
    match d {
        .mon => "Mon",
        .tue => "Tue",
        .wed => "Wed",
    }
}

view alarm(a: Alarm) {
    text(a.days.map(d -> said(d)).join(" "))
    text((a.by_person.get("ada") ?? []).map(d -> said(d)).join(" "))
    text(if let d = a.first.get("ada") { said(d) } else { "-" })
    text(if a.days.contains(Weekday.tue) { "rings Tuesday" } else { "quiet Tuesday" })
}
"#;
        let app = App::compile(ALARMS, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let alarm = rt.create("Alarm").unwrap();
        let ids = &app.compiled.ids;
        let day = |case: &str| {
            Value::Enum(EnumValue {
                ty: ids.get("Weekday").unwrap().to_owned(),
                variant: ids.get(&format!("Weekday.{case}")).unwrap().to_owned(),
                fields: std::collections::BTreeMap::default(),
            })
        };
        let run = |action: &str, case: &str| {
            rt.invoke_action(
                &me(),
                (
                    app.action(action).as_str(),
                    vec![Value::resource(&alarm), day(case)],
                ),
            )
            .unwrap_or_else(|e| panic!("{action}({case}): {e}"));
        };
        run("ring_on", "mon");
        run("ring_on", "tue");
        run("ring_on", "wed");
        run("quiet_on", "mon");
        let mut view = rt
            .open_view(&me(), &app.view("alarm"), vec![Value::resource(&alarm)])
            .unwrap();
        view.poll_patches().unwrap();
        let mut shown = texts(&view);
        // A set keeps no order of its own: compare the days whatever order they come in.
        for line in shown.iter_mut().take(2) {
            let mut days: Vec<&str> = line.split(' ').collect();
            days.sort_unstable();
            *line = days.join(" ");
        }
        assert_eq!(
            shown,
            ["Tue Wed", "Tue Wed", "Wed", "rings Tuesday"],
            "{}",
            dump(view.tree())
        );
        let Value::List(days) = rt.read(&alarm, "days").unwrap() else {
            panic!("a set reads as a list");
        };
        assert!(
            days.contains(&day("tue")) && days.contains(&day("wed")) && days.len() == 2,
            "the set holds the cases themselves: {days:?}"
        );
    }
}

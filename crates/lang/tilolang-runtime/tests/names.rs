//! What a view shows of what only the window knows — a participant's name, how the person writes
//! a day or a time of day, a word in the language their window speaks — is a read like any
//! other: the boundary that showed it runs again when the person's environment changes, and no
//! other boundary does.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::{
        AmStore, NameHost, Principal, Runtime, ShownHost, SimpleAuthority, Value, ViewInstance,
        Words,
    };

    const SOURCE: &str = r#"app so.tilo.named "Named"

type Note {
    title: Text
}

type Meeting {
    at: Time = Time("2026-10-29T14:05:00Z")
}

action retitle(n: Note, title: Text) {
    n.title = title
}

/// A note: what it says, and who has the window. The two calls are boundaries of their own, and
/// only the second shows a name.
view note(n: Note) {
    stack {
        title_of(n)
        who_has_it()
    }
}

/// What the note says: it reads the record and no name.
view title_of(n: Note) {
    text(n.title, role: .title)
}

/// Who has the window: the name the window gives them, and nothing of the record.
view who_has_it() {
    text(workspace.me.name(), role: .label)
}

/// A note's title alone: a view that shows no name.
view just_title(n: Note) {
    text(n.title)
}

/// The word the window speaks beside it: no name, and nothing of the record.
view greeting() {
    text("Hello", role: .label)
}

/// The note's title and the window's word, as two boundaries of their own.
view titled_and_greeted(n: Note) {
    stack {
        title_of(n)
        greeting()
    }
}

/// When a meeting is, as the person reads a clock.
view when(m: Meeting) {
    text(m.at.clock())
}
"#;

    /// What a window calls each participant, by id: the names it told the runtime.
    type Said = Rc<RefCell<HashMap<String, String>>>;

    fn ada() -> Principal {
        Principal::new("ada")
    }

    /// What the window calls the workspace's participants: the names it told the runtime, and
    /// each participant's own id until it says otherwise.
    struct Window(Said);

    impl NameHost for Window {
        fn name(&self, who: &Principal) -> String {
            self.0
                .borrow()
                .get(&who.0)
                .cloned()
                .unwrap_or_else(|| who.0.clone())
        }
    }

    /// How the person writes a clock: a 24-hour one until they choose the other.
    struct Style(Rc<RefCell<bool>>);

    impl ShownHost for Style {
        fn shown(&self, _kind: &str, value: &str) -> String {
            value.to_owned()
        }

        fn clock(&self, hour: u32, minute: u32) -> String {
            if *self.0.borrow() {
                let half = if hour < 12 { "AM" } else { "PM" };
                let hour = match hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                format!("{hour}:{minute:02} {half}")
            } else {
                format!("{hour:02}:{minute:02}")
            }
        }
    }

    /// The language the person's window speaks: every word it is asked for reads in it.
    struct Speech(Rc<RefCell<&'static str>>);

    impl Words for Speech {
        fn add(&self, _: &[(String, String)]) {}
        fn add_used(&self, _: &[(String, String)]) {}
        fn word(&self, _key: &str, _args: &[(String, String)], _source: &str) -> String {
            (*self.0.borrow()).to_owned()
        }
    }

    fn texts(v: &ViewInstance) -> Vec<String> {
        of_kind(v.tree(), "ui.Text")
            .into_iter()
            .map(|node| text_prop(node, "value"))
            .collect()
    }

    /// A runtime with the package loaded, `title` a note it holds, and `names` what its window
    /// says it calls `ada`.
    fn setup(title: &str) -> (Runtime, App, ViewInstance, Said) {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let names = Rc::new(RefCell::new(HashMap::new()));
        rt.set_name_host(Window(Rc::clone(&names)));
        let note = rt.create("Note").unwrap();
        let retitle = app.action("retitle");
        rt.invoke_action(
            &ada(),
            (
                retitle.as_str(),
                vec![Value::resource(&note), Value::text(title)],
            ),
        )
        .unwrap();
        let view = rt
            .open_view(&ada(), &app.view("note"), vec![Value::resource(&note)])
            .unwrap();
        (rt, app, view, names)
    }

    #[test]
    fn a_name_from_the_window_runs_again_only_the_boundary_that_shows_it() {
        let (_rt, _app, mut v, names) = setup("a list of what to bring");
        assert_eq!(
            texts(&v),
            ["a list of what to bring", "ada"],
            "until the window names her, the person looking is her id"
        );
        let before = v.stats();
        assert!(!v.is_stale(), "nothing the view read has changed");

        // The window says what it calls her.
        names
            .borrow_mut()
            .insert("ada".to_owned(), "Ada Lovelace".to_owned());
        v.person_moved();
        assert!(v.is_stale(), "the boundary that shows a name runs again");
        assert!(!v.poll_patches().unwrap().is_empty());
        assert_eq!(
            texts(&v),
            ["a list of what to bring", "Ada Lovelace"],
            "the name the window gave her is the name the window shows"
        );
        let after = v.stats();
        assert_eq!(
            after.root_runs, before.root_runs,
            "the whole view does not run again for a name: before {before:?}, after {after:?}"
        );
        assert_eq!(
            after.renders,
            before.renders + 1,
            "the boundary that shows a name, and no other: before {before:?}, after {after:?}, \
             texts {:?}, tree {}",
            texts(&v),
            dump(v.tree())
        );

        // What the sibling showed is the record's, not a name: it is still the same fragment.
        assert!(dump(v.tree()).contains("a list of what to bring"));
    }

    #[test]
    fn a_view_that_shows_no_name_does_not_run_for_one() {
        let (rt, app, _, names) = setup("a list of what to bring");
        let note = rt.create("Note").unwrap();
        let mut v = rt
            .open_view(
                &ada(),
                &app.view("just_title"),
                vec![Value::resource(&note)],
            )
            .unwrap();
        let before = v.stats();
        names
            .borrow_mut()
            .insert("ada".to_owned(), "Ada Lovelace".to_owned());
        v.person_moved();
        assert!(!v.is_stale(), "a view that shows no name reads none");
        assert!(v.poll_patches().unwrap().is_empty());
        assert_eq!(v.stats().renders, before.renders);
    }

    #[test]
    fn another_way_of_writing_a_clock_runs_again_only_the_boundary_that_writes_one() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let twelve = Rc::new(RefCell::new(false));
        rt.set_shown_host(Style(Rc::clone(&twelve)));
        let meeting = rt.create("Meeting").unwrap();
        let mut v = rt
            .open_view(&ada(), &app.view("when"), vec![Value::resource(&meeting)])
            .unwrap();
        assert_eq!(texts(&v), ["14:05"]);
        let before = v.stats();

        // The person chose the other clock in Settings.
        *twelve.borrow_mut() = true;
        v.person_moved();
        assert!(
            v.is_stale(),
            "the boundary that writes a time of day runs again"
        );
        assert!(!v.poll_patches().unwrap().is_empty());
        assert_eq!(texts(&v), ["2:05 PM"]);
        assert_eq!(v.stats().renders, before.renders + 1);
        assert_eq!(v.stats().root_runs, before.root_runs + 1);
    }

    #[test]
    fn another_language_runs_again_only_the_boundary_that_shows_a_word() {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        app.load(&rt);
        let language = Rc::new(RefCell::new("Hello"));
        rt.set_words(Speech(Rc::clone(&language)));
        let note = rt.create("Note").unwrap();
        let retitle = app.action("retitle");
        rt.invoke_action(
            &ada(),
            (
                retitle.as_str(),
                vec![
                    Value::resource(&note),
                    Value::text("a list of what to bring"),
                ],
            ),
        )
        .unwrap();
        let mut v = rt
            .open_view(
                &ada(),
                &app.view("titled_and_greeted"),
                vec![Value::resource(&note)],
            )
            .unwrap();
        assert_eq!(texts(&v), ["a list of what to bring", "Hello"]);
        let before = v.stats();

        // The person reads another language, and the window says its words in it.
        *language.borrow_mut() = "Hallo";
        v.person_moved();
        assert!(v.is_stale(), "the boundary that shows a word runs again");
        assert!(!v.poll_patches().unwrap().is_empty());
        assert_eq!(
            texts(&v),
            ["a list of what to bring", "Hallo"],
            "what the word boundary shows reads in the language the window speaks"
        );
        assert_eq!(
            v.stats().renders,
            before.renders + 1,
            "the boundary that shows a word, and no other: before {before:?}, after {:?}",
            v.stats()
        );

        // A view that shows no word is not run for a language.
        let mut titled = rt
            .open_view(
                &ada(),
                &app.view("just_title"),
                vec![Value::resource(&note)],
            )
            .unwrap();
        titled.poll_patches().unwrap();
        let before = titled.stats();
        *language.borrow_mut() = "Bonjour";
        titled.person_moved();
        assert!(!titled.is_stale(), "a view that shows no word reads none");
        assert_eq!(titled.stats().renders, before.renders);
    }
}

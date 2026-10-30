//! Views that read the time (`today` in a query or a named filter, `-7d`) run again exactly
//! when what they read of it changes: the runtime records, beside a view's reads, when the time
//! it read next reads differently, the host wakes the view then, and the poll runs again only
//! the boundaries that read it. A view that read no time never asks to be woken.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::common::*;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, UiNode, Value};

    const SOURCE: &str = r#"app so.tilo.days "Days"

type Task {
    title: Text (title)
    done: Bool = false
    due: Option<Date> = .none

    search {
        filter "Today" = due <= today and not done
        filter due
    }
}

type Message {
    subject: Text (title)
    received: Time = Time("2026-01-01T00:00:00Z")

    search {
        filter received
    }
}

type Board {
    tasks: Collection<Task>
    messages: Collection<Message>
}

action seed(b: Board) {
    b.tasks.insert(Task(title: "due today", due: Date("2026-10-29")))
    b.tasks.insert(Task(title: "due tomorrow", due: Date("2026-10-30")))
    b.tasks.insert(Task(title: "undated"))
    b.messages.insert(Message(subject: "an hour ago", received: Time("2026-10-29T20:59:00Z")))
}

view plain(b: Board) {
    for t in b.tasks {
        text(t.title)
    }
}

view today(b: Board) {
    for t in b.tasks.matching("is:today") {
        text(t.title)
    }
}

view both(b: Board) {
    stack {
        plain(b)
        today(b)
    }
}

view recent(b: Board) {
    for m in b.messages.matching("received:-1h") {
        text(m.subject)
    }
}
"#;

    /// The person's clock is two hours ahead of UTC: their midnight is 22:00 UTC.
    const ZONE: i64 = 2 * 3600;
    /// 23:59 on 29 October where the person is.
    const BEFORE_MIDNIGHT: &str = "2026-10-29T21:59:00Z";
    /// Their midnight, in seconds since 1970.
    const MIDNIGHT: i64 = 1_793_311_200;
    /// Half a minute into 30 October where the person is.
    const AFTER_MIDNIGHT: &str = "2026-10-29T22:00:30Z";

    struct Days {
        rt: Runtime,
        app: App,
        board: ResourceRef,
        clock: Rc<RefCell<String>>,
    }

    impl Days {
        fn at(now: &str) -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            let clock = Rc::new(RefCell::new(now.to_owned()));
            let reading = Rc::clone(&clock);
            rt.set_clock(move || reading.borrow().clone());
            rt.set_zone(|| ZONE);
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            let id = app.action("seed");
            rt.invoke_action(&me(), (id.as_str(), vec![Value::resource(&board)]))
                .unwrap();
            Self {
                rt,
                app,
                board,
                clock,
            }
        }

        fn view(&self, name: &str) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view(name),
                    vec![Value::resource(&self.board)],
                )
                .unwrap()
        }

        fn set_clock(&self, now: &str) {
            now.clone_into(&mut self.clock.borrow_mut());
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    fn seconds(time: &str) -> i64 {
        tilolang::calendar::instant(time).unwrap()
    }

    #[test]
    fn a_view_on_today_runs_again_at_the_persons_midnight_and_no_other_view_does() {
        assert_eq!(MIDNIGHT, seconds("2026-10-29T22:00:00Z"));
        let days = Days::at(BEFORE_MIDNIGHT);
        let mut today = days.view("today");
        let mut plain = days.view("plain");
        let mut both = days.view("both");
        assert_eq!(texts(today.tree()), ["due today"]);
        assert_eq!(
            texts(both.tree()),
            ["due today", "due tomorrow", "undated", "due today"]
        );
        // What read `today` waits for the person's midnight; what read no time waits for nothing.
        assert_eq!(today.wakes_at(), Some(MIDNIGHT));
        assert_eq!(both.wakes_at(), Some(MIDNIGHT));
        assert_eq!(
            plain.wakes_at(),
            None,
            "a view that read no time is never woken"
        );

        // Before midnight nothing is due: a poll runs nothing.
        days.set_clock("2026-10-29T21:59:59Z");
        assert!(!today.is_stale());
        let renders = (today.stats().renders, both.stats().renders);
        assert!(today.poll_patches().unwrap().is_empty());
        assert!(both.poll_patches().unwrap().is_empty());
        assert_eq!((today.stats().renders, both.stats().renders), renders);

        days.set_clock(AFTER_MIDNIGHT);
        assert!(today.is_stale(), "the day it read has turned");
        assert!(!plain.is_stale(), "a view that read no time is not stale");
        let plain_renders = plain.stats().renders;
        let both_renders = both.stats().renders;
        let today_renders = today.stats().renders;
        assert!(!today.poll_patches().unwrap().is_empty());
        assert_eq!(texts(today.tree()), ["due today", "due tomorrow"]);
        // The view ran once, and the row that came in rendered: its boundary of its own.
        assert_eq!(today.stats().renders, today_renders + 2);
        assert!(plain.poll_patches().unwrap().is_empty());
        assert_eq!(plain.stats().renders, plain_renders);
        // In one tree, only the boundary that read the time ran again (with the row that came
        // in): not its parent, not its sibling or their rows.
        both.poll_patches().unwrap();
        assert_eq!(
            texts(both.tree()),
            [
                "due today",
                "due tomorrow",
                "undated",
                "due today",
                "due tomorrow"
            ]
        );
        assert_eq!(both.stats().renders, both_renders + 2);
        // Each now waits for the next midnight.
        assert_eq!(today.wakes_at(), Some(MIDNIGHT + 86_400));
        assert_eq!(both.wakes_at(), Some(MIDNIGHT + 86_400));
    }

    #[test]
    fn a_view_on_the_running_clock_runs_again_at_the_next_minute() {
        let days = Days::at("2026-10-29T21:58:30Z");
        let mut recent = days.view("recent");
        assert_eq!(texts(recent.tree()), ["an hour ago"]);
        // A query reads its clock to the minute: what it read moves on at the next one.
        assert_eq!(recent.wakes_at(), Some(seconds("2026-10-29T21:59:00Z")));
        days.set_clock("2026-10-29T22:00:00Z");
        recent.poll_patches().unwrap();
        assert_eq!(
            texts(recent.tree()),
            Vec::<String>::new(),
            "a message received more than an hour ago leaves"
        );
        assert_eq!(recent.wakes_at(), Some(seconds("2026-10-29T22:01:00Z")));
    }

    #[test]
    fn a_view_that_reads_no_time_never_reads_the_clock_to_poll() {
        let days = Days::at(BEFORE_MIDNIGHT);
        let mut plain = days.view("plain");
        let renders = plain.stats().renders;
        for now in [
            AFTER_MIDNIGHT,
            "2026-11-02T08:00:00Z",
            "2027-01-01T00:00:00Z",
        ] {
            days.set_clock(now);
            assert_eq!(plain.wakes_at(), None);
            assert!(!plain.is_stale());
            assert!(plain.poll_patches().unwrap().is_empty());
        }
        assert_eq!(plain.stats().renders, renders, "nothing ran again");
    }
}

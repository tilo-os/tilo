//! `ranked(items)`, run: a program's records in the order the person looking chose them, most
//! often and most lately first, read from each record's history rather than a count the app
//! keeps.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tilolang_runtime::ids::MemberId;
    use tilolang_runtime::{Actor, AmStore, ResourceRef, Runtime, UiNode, Value, ViewInstance};

    const SOURCE: &str = r#"app so.tilo.plays "Plays"

type Song (title: title) {
    title: Text
    heard: Int merge counter
}

type Library {
    songs: Collection<Song>
}

root Library

default view by_use for Library

action add(l: Library, title: Text) -> Song {
    l.songs.insert(Song(title: title))
}

action play(s: Song) {
    s.heard += 1
}

view by_use(l: Library) {
    stack {
        for s in ranked(l.songs.filter(x -> true)) {
            ui.Text(s.title)
        }
    }
}
"#;

    const DAY: i64 = 86_400;

    struct Plays {
        rt: Runtime,
        app: App,
        library: ResourceRef,
        clock: Rc<RefCell<String>>,
    }

    fn plays() -> Plays {
        let app = App::compile(SOURCE, &[]);
        let authority = TestAuthority::new();
        let rt = Runtime::new(AmStore::new(), authority.clone()).unwrap();
        authority.trust(&app.load(&rt));
        let clock = Rc::new(RefCell::new(String::new()));
        let reading = Rc::clone(&clock);
        rt.set_clock(move || reading.borrow().clone());
        let library = rt.create_root("Library").unwrap();
        let p = Plays {
            rt,
            app,
            library,
            clock,
        };
        p.at(0);
        p
    }

    impl Plays {
        /// Sets the clock `day` days after the first.
        fn at(&self, day: i64) {
            let at = jiff::Timestamp::from_second(1_790_000_000 + day * DAY).unwrap();
            *self.clock.borrow_mut() = at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string();
        }

        fn add(&self, title: &str) -> ResourceRef {
            let action = self.app.action("add");
            let args = vec![Value::resource(&self.library), Value::text(title)];
            returned_ref(
                self.rt
                    .invoke_action(&me(), (action.as_str(), args))
                    .unwrap(),
            )
        }

        fn play(&self, who: &Actor, song: &ResourceRef, times: usize) {
            let action = self.app.action("play");
            for _ in 0..times {
                self.rt
                    .invoke_action(who, (action.as_str(), vec![Value::resource(song)]))
                    .unwrap();
            }
        }

        fn view(&self, who: &Actor) -> ViewInstance {
            self.rt
                .open_view(
                    who,
                    &self.app.view("by_use"),
                    vec![Value::resource(&self.library)],
                )
                .unwrap()
        }
    }

    fn order(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    tilo_test_env::scenario! {
        fn records_rank_by_how_often_and_how_lately_their_person_chose_them;

        phase what_nobody_chose_since_it_was_made_keeps_its_order;
        {
            let p = plays();
            for title in ["Aria", "Bolero", "Canon"] {
                p.add(title);
            }
            assert_eq!(order(p.view(&me()).tree()), ["Aria", "Bolero", "Canon"]);
        }

        phase often_outranks_lately_and_a_choice_fades_with_age;
        {
            let p = plays();
            let aria = p.add("Aria");
            let bolero = p.add("Bolero");
            let canon = p.add("Canon");
            let mut v = p.view(&me());
            // Bolero five more times ten days in, Canon once today: the most chosen leads, the
            // one chosen today follows, and Aria, chosen only when it was added two months ago,
            // falls behind both.
            p.at(50);
            p.play(&me(), &bolero, 5);
            p.at(60);
            p.play(&me(), &canon, 1);
            v.poll_patches().unwrap();
            assert_eq!(
                order(v.tree()),
                ["Bolero", "Canon", "Aria"],
                "the open view ranks again as the choices are made"
            );
            // Not the app's own counter: the history is what ranks. A month on, Aria played
            // today pulls past choices that have faded.
            p.at(90);
            p.play(&me(), &aria, 2);
            v.poll_patches().unwrap();
            assert_eq!(order(v.tree()), ["Aria", "Bolero", "Canon"]);
        }

        phase another_persons_choices_rank_for_them_and_not_for_me;
        {
            let p = plays();
            p.add("Aria");
            let bolero = p.add("Bolero");
            let bo = Actor::from(MemberId::person("bo"));
            p.at(1);
            p.play(&bo, &bolero, 5);
            assert_eq!(
                order(p.view(&me()).tree()),
                ["Aria", "Bolero"],
                "what someone else played is not what I play"
            );
            assert_eq!(order(p.view(&bo).tree()), ["Bolero", "Aria"]);
        }
    }
}

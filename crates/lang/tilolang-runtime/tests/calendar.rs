//! Calendar's capabilities, each through its own actions and views: an event made, moved and
//! repeated, its dates skipped, cancelled and moved apart, its guests and their invitation, its
//! reminder, its zone; the tasks and projects beside it; the week, day, month and agenda windows
//! over what repeats; and the iCalendar file in and out.

#[cfg(test)]
mod cases {
    use std::collections::BTreeMap;

    use crate::common::*;
    use tilolang_runtime::{
        ActionInfo, Actor, AmStore, Authority, Decision, EnumValue, InvokeOutcome, MemorySink,
        ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    /// The moment every test reads as now: a Tuesday, late morning, in UTC.
    const NOW: &str = "2026-11-24T09:40:00Z";
    const TODAY: &str = "2026-11-24";

    /// The authority a test at the workspace runs under: everything it asks for is allowed, so
    /// an effect to a server is delivered rather than held for a person's yes.
    #[derive(Clone, Default)]
    struct Allow;

    impl Authority for Allow {
        fn decide(
            &self,
            _: &Actor,
            _: &[tilolang_runtime::ids::ModuleHash],
            _: &ActionInfo,
        ) -> Decision {
            Decision::Allow
        }
    }

    struct Sheet {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Files,
        raised: Raised,
        /// What the app asked of a server, as every outside effect it delivered.
        sink: MemorySink,
    }

    fn sheet() -> Sheet {
        let device = Files::default();
        let raised = Raised::default();
        let rt = Runtime::new(AmStore::new(), Allow).unwrap();
        rt.set_clock(|| NOW.to_owned());
        rt.set_zone(|| 0);
        rt.set_week_start(|| 0);
        rt.set_blobs(device.clone());
        rt.set_notices(raised.clone());
        let sink = MemorySink::new();
        rt.set_sink(sink.clone());
        stage_members(
            &rt,
            vec![tilolang_runtime::members::Member::person(
                "me",
                tilo_actions::policy::Role::Owner,
            )],
            None,
        );
        // Mail's own package runs beside Calendar's, so the invitation it hands over has a root
        // to reach.
        let mut packages = built("calendar");
        let app = packages.pop().expect("Calendar is built last");
        for used in &packages {
            used.load(&rt);
        }
        app.load(&rt);
        let root = rt.create_root("Calendars").unwrap();
        rt.create_root("Mail").unwrap();
        Sheet {
            rt,
            app,
            root,
            device,
            raised,
            sink,
        }
    }

    impl Sheet {
        fn call(&self, action: &str, given: &[(&str, Value)]) {
            let _ = self.invoke(action, given);
        }

        /// What `action` was refused with: the words the view shows when a press cannot go
        /// through.
        fn refusal(&self, action: &str, given: &[(&str, Value)]) -> String {
            let args = self.app.args(&self.rt, action, given);
            let id = self.app.action(action);
            match self.rt.invoke_action(&me(), (id.as_str(), args)) {
                Ok(outcome) => panic!("{action} was not refused: {outcome:?}"),
                Err(why) => why.to_string(),
            }
        }

        fn invoke(&self, action: &str, given: &[(&str, Value)]) -> InvokeOutcome {
            let args = self.app.args(&self.rt, action, given);
            let id = self.app.action(action);
            self.rt
                .invoke_action(&me(), (id.as_str(), args))
                .unwrap_or_else(|e| panic!("{action}: {e}"))
        }

        fn call_ref(&self, action: &str, given: &[(&str, Value)]) -> ResourceRef {
            returned_ref(self.invoke(action, given))
        }

        fn read(&self, at: &ResourceRef, field: &str) -> Value {
            self.rt
                .read(at, field)
                .unwrap_or_else(|e| panic!("{field}: {e}"))
        }

        fn text(&self, at: &ResourceRef, field: &str) -> String {
            match self.read(at, field) {
                Value::Text(t) => t,
                other => panic!("{field} is no text: {other:?}"),
            }
        }

        fn optional_text(&self, at: &ResourceRef, field: &str) -> Option<String> {
            match self.read(at, field) {
                Value::Opt(Some(v)) => v.as_text().map(str::to_owned),
                Value::Opt(None) => None,
                other => panic!("{field} is no moment: {other:?}"),
            }
        }

        fn texts_of_set(&self, at: &ResourceRef, field: &str) -> Vec<String> {
            let value = self.read(at, field);
            let items = match value {
                Value::List(items) => items,
                other => panic!("{field} is no set: {other:?}"),
            };
            items
                .into_iter()
                .filter_map(|v| v.as_text().map(str::to_owned))
                .collect()
        }

        /// The items of the collection `field`, each as the projection reads it:
        /// `{type, id, label, fields}`.
        fn projected(&self, at: &ResourceRef, field: &str) -> Vec<serde_json::Value> {
            let value = self.read(at, field);
            let shown = self.rt.project(&value);
            let Some(items) = shown.get("items").and_then(|i| i.as_array()) else {
                panic!("{field} projects no items: {shown}");
            };
            items.clone()
        }

        /// The case `name` (`Priority.high`) of the enum `ty` (`Priority`).
        fn case(&self, ty: &str, name: &str) -> Value {
            let ids = &self.app.compiled.ids;
            Value::Enum(EnumValue {
                ty: ids.get(ty).unwrap().to_owned(),
                variant: ids.get(name).unwrap().to_owned(),
                fields: BTreeMap::default(),
            })
        }

        fn window(&self, view: &str, args: Vec<Value>) -> ViewInstance {
            let mut window = self
                .rt
                .open_view(&me(), &self.app.view(view), args)
                .unwrap_or_else(|e| panic!("{view}: {e}"));
            window.set_lazy_rows(false).unwrap();
            window
        }

        /// The app's own window, over its root.
        fn root_window(&self) -> ViewInstance {
            self.window("window", vec![Value::resource(&self.root)])
        }

        /// Opens the calendar as a person does: the window stands on this week.
        fn opened(&self) {
            self.call("land", &[("c", Value::resource(&self.root))]);
        }

        /// The window showing one of its places: the timeline over the week, one day or the
        /// month, the agenda, or the tasks.
        fn place_view(&self, place: &str) -> ViewInstance {
            let mut w = self.root_window();
            let item = w
                .tree()
                .of_kind("ui.NavItem")
                .into_iter()
                .find(|n| n.props.get("key") == Some(&Value::text(place)))
                .unwrap_or_else(|| panic!("the window offers the place {place}"))
                .key
                .clone();
            w.fire(&item, "select").unwrap();
            w.poll_patches().unwrap();
            w
        }

        /// The timeline over the week, one day or the month, as the window shows it.
        fn timeline(&self, place: &str) -> ViewInstance {
            self.place_view(place)
        }

        fn place(&self, view: &str) -> ViewInstance {
            self.place_view(view)
        }

        fn make(&self, title: &str, day: &str) -> ResourceRef {
            self.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&self.root)),
                    ("title", Value::text(title)),
                    ("day", Value::some(Value::text(day))),
                    ("at", Value::some(Value::text(format!("{day}T09:00:00Z")))),
                ],
            )
        }

        fn make_task(&self, title: &str) -> ResourceRef {
            self.call_ref(
                "new_task",
                &[
                    ("c", Value::resource(&self.root)),
                    ("title", Value::text(title)),
                ],
            )
        }

        fn make_calendar(&self, name: &str) -> ResourceRef {
            let cal = self.call_ref("new_calendar", &[("c", Value::resource(&self.root))]);
            self.call(
                "rename_calendar",
                &[("k", Value::resource(&cal)), ("name", Value::text(name))],
            );
            cal
        }

        fn repeat(&self, e: &ResourceRef, case: &str) {
            self.call(
                "repeat",
                &[
                    ("e", Value::resource(e)),
                    ("how", self.case("Repeat", case)),
                ],
            );
        }
    }

    /// The events a timeline draws, by their titles.
    fn drawn(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.TimelineItem")
            .iter()
            .filter_map(|item| text_prop_opt(item, "title"))
            .collect()
    }

    /// Chooses the window's place `key`, as a person pressing its row does.
    fn press_nav(w: &mut ViewInstance, key: &str) {
        let item = w
            .tree()
            .of_kind("ui.NavItem")
            .into_iter()
            .find(|n| n.props.get("key") == Some(&Value::text(key)))
            .unwrap_or_else(|| panic!("the window offers no place {key}"))
            .key
            .clone();
        w.fire(&item, "select").unwrap();
        w.poll_patches().unwrap();
    }

    fn text_prop_opt(node: &UiNode, name: &str) -> Option<String> {
        match node.props.get(name) {
            Some(Value::Text(t)) => Some(t.clone()),
            Some(Value::Opt(Some(v))) => v.as_text().map(str::to_owned),
            _ => None,
        }
    }

    fn labels(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.ListRow")
            .iter()
            .filter_map(|node| text_prop_opt(node, "title"))
            .collect()
    }

    /// The words a view shows, its texts in order.
    fn shown(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .filter_map(|node| text_prop_opt(node, "value"))
            .collect()
    }

    /// The key of the control of `kind` whose `prop` says `text`.
    fn control_in(view: &ViewInstance, kind: &str, prop: &str, text: &str) -> String {
        of_kind(view.tree(), kind)
            .into_iter()
            .find(|node| text_prop_opt(node, prop).as_deref() == Some(text))
            .unwrap_or_else(|| panic!("no {kind} with {prop} `{text}`"))
            .key
            .clone()
    }

    tilo_test_env::scenario! {
        fn a_calendar_makes_moves_and_repeats_what_it_holds;

        phase an_event_starts_at_nine_for_an_hour_on_the_day_it_is_made;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            assert_eq!(s.text(&e, "title"), "Standup");
            assert_eq!(s.text(&e, "start"), format!("{TODAY}T09:00:00"));
            assert_eq!(s.text(&e, "end"), format!("{TODAY}T10:00:00"));
            assert_eq!(s.text(&e, "timezone"), "floating");
            assert_eq!(s.text(&e, "day"), TODAY);
            assert_eq!(s.text(&e, "end_day"), TODAY);
            assert_eq!(s.projected(&s.root, "events").len(), 1);
            // The workspace's own calendar was made with the root, and holds it.
            assert!(matches!(s.read(&e, "calendar"), Value::Opt(Some(_))));
        }

        phase a_quick_add_line_is_read_as_an_event;
        {
            let s = sheet();
            let quick = |line: &str| {
                s.call_ref(
                    "quick_add",
                    &[
                        ("c", Value::resource(&s.root)),
                        ("line", Value::text(line)),
                    ],
                )
            };
            // A weekday and a clock are read, and the words that said when leave the title.
            let lunch = quick("Lunch with Sam friday 1pm");
            assert_eq!(s.text(&lunch, "title"), "Lunch with Sam");
            assert_eq!(s.text(&lunch, "start"), "2026-11-27T13:00:00");
            assert_eq!(s.text(&lunch, "end"), "2026-11-27T14:00:00");
            assert_eq!(s.text(&lunch, "day"), "2026-11-27");
            // No day or clock: the next quarter hour, an hour long, on the day the window shows.
            let plumber = quick("Call the plumber");
            assert_eq!(s.text(&plumber, "title"), "Call the plumber");
            assert_eq!(s.text(&plumber, "start"), "2026-11-24T09:45:00");
            assert_eq!(s.text(&plumber, "end"), "2026-11-24T10:45:00");
            // Today, tonight and tomorrow, and a length that sets the end.
            assert_eq!(s.text(&quick("Coffee today 9am"), "start"), "2026-11-24T09:00:00");
            assert_eq!(s.text(&quick("Dinner tonight"), "start"), "2026-11-24T20:00:00");
            let workshop = quick("Workshop tomorrow at 10 for 90 min");
            assert_eq!(s.text(&workshop, "title"), "Workshop");
            assert_eq!(s.text(&workshop, "start"), "2026-11-25T10:00:00");
            assert_eq!(s.text(&workshop, "end"), "2026-11-25T11:30:00");
            // `next friday` is the one after this week, never the day the person is on.
            assert_eq!(s.text(&quick("Sync next friday 10am"), "start"), "2026-12-04T10:00:00");
            // A day as written is read however it is introduced.
            let trip = quick("Trip on 2026-11-03 at 8:00");
            assert_eq!(s.text(&trip, "title"), "Trip");
            assert_eq!(s.text(&trip, "start"), "2026-11-03T08:00:00");
            // Clocks as people write them; a bare hour after `at` is taken in the afternoon when
            // it is one to six, and an impossible one stays in the title.
            assert_eq!(s.text(&quick("Call at 3pm"), "start"), "2026-11-24T15:00:00");
            assert_eq!(s.text(&quick("Call at 3:30pm"), "start"), "2026-11-24T15:30:00");
            assert_eq!(s.text(&quick("Call at 15:30"), "start"), "2026-11-24T15:30:00");
            assert_eq!(s.text(&quick("Call at 3"), "start"), "2026-11-24T15:00:00");
            assert_eq!(s.text(&quick("Call at 21"), "start"), "2026-11-24T21:00:00");
            let noon = quick("Call at noon");
            assert_eq!(s.text(&noon, "start"), "2026-11-24T12:00:00");
            // Words that are no clock keep their place in the title.
            let joes = quick("Dinner at Joe's at 7pm");
            assert_eq!(s.text(&joes, "title"), "Dinner at Joe's");
            assert_eq!(s.text(&joes, "start"), "2026-11-24T19:00:00");
            let score = quick("1:1 with Sam about 5 things");
            assert_eq!(s.text(&score, "title"), "1:1 with Sam about 5 things");
            let party = quick("Party at 25:99");
            assert_eq!(s.text(&party, "title"), "Party at 25:99");
            assert_eq!(s.text(&party, "start"), "2026-11-24T09:45:00");
        }

        phase the_quick_add_sheet_says_what_the_line_will_write;
        {
            let s = sheet();
            let mut window = s.root_window();
            let open = control_in(&window, "ui.Button", "label", "Quick add");
            window.fire(&open, "press").unwrap();
            window.poll_patches().unwrap();
            let field = control_in(
                &window,
                "ui.TextInput",
                "placeholder",
                "Lunch with Sam friday 1pm",
            );
            window
                .edit(&field, &Value::text("Lunch with Sam friday 1pm"))
                .unwrap();
            window.poll_patches().unwrap();
            assert!(
                shown(window.tree())
                    .iter()
                    .any(|line| line.contains("Lunch with Sam, ") && line.contains("13:00")),
                "the sheet says the day and hours: {:?}",
                shown(window.tree())
            );
        }

        phase the_window_answers_its_keys_and_shows_them;
        {
            let s = sheet();
            let mut window = s.root_window();
            let button = |window: &ViewInstance, prop: &str, text: &str| {
                of_kind(window.tree(), "ui.Button")
                    .into_iter()
                    .find(|node| text_prop_opt(node, prop).as_deref() == Some(text))
                    .unwrap_or_else(|| panic!("no button whose {prop} is {text}"))
                    .clone()
            };
            let new = button(&window, "label", "New event");
            assert_eq!(new.props.get("key"), Some(&Value::text("N")), "N makes an event");
            window.fire(&new.key, "press").unwrap();
            window.poll_patches().unwrap();
            assert_eq!(s.projected(&s.root, "events").len(), 1, "N made the event");
            // The reference opens from ? and names the keys the window answers.
            let help = button(&window, "accessible_name", "Keyboard shortcuts");
            assert_eq!(
                help.props.get("keys"),
                Some(&Value::List(vec![Value::text("?")])),
                "? shows the reference"
            );
            window.fire(&help.key, "press").unwrap();
            window.poll_patches().unwrap();
            let hints = of_kind(window.tree(), "ui.KeyHint").len();
            assert!(hints >= 6, "the reference lists the keys: {hints}");
            // Arrows walk the days and weeks, and a jump menu walks the weeks anywhere.
            let arrows: Vec<String> = of_kind(window.tree(), "ui.Button")
                .into_iter()
                .flat_map(|node| match node.props.get("keys") {
                    Some(Value::List(items)) => items
                        .iter()
                        .filter_map(|v| v.as_text().map(str::to_owned))
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect();
            assert!(arrows.iter().any(|k| k == "ArrowLeft"), "{arrows:?}");
            let jump: Vec<String> = of_kind(window.tree(), "ui.MenuItem")
                .into_iter()
                .filter_map(|node| node.props.get("key").and_then(Value::as_text).map(str::to_owned))
                .collect();
            assert!(jump.iter().any(|k| k == "ArrowUp"), "{jump:?}");
        }

        phase an_event_moves_to_another_day_with_the_clock_and_reminder_it_kept;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call("remind_at_start", &[("e", Value::resource(&e))]);
            s.call(
                "move_to_day",
                &[("e", Value::resource(&e)), ("day", Value::text("2026-11-26"))],
            );
            assert_eq!(s.text(&e, "start"), "2026-11-26T09:00:00");
            assert_eq!(s.text(&e, "day"), "2026-11-26");
            assert_eq!(s.text(&e, "end_day"), "2026-11-26");
            assert_eq!(s.text(&e, "remind"), "2026-11-26T09:00:00Z", "the reminder moved with it");
        }

        phase an_event_keeps_how_long_it_lasts_when_it_starts_elsewhere;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call(
                "change_start",
                &[("e", Value::resource(&e)), ("at", Value::text("2026-11-24T14:30:00Z"))],
            );
            assert_eq!(s.text(&e, "end"), "2026-11-24T15:30:00");
            s.call(
                "change_end",
                &[("e", Value::resource(&e)), ("at", Value::text("2026-11-25T09:00:00Z"))],
            );
            assert_eq!(s.text(&e, "end_day"), "2026-11-25");
        }

        phase the_rule_names_the_unit_its_interval_its_end_and_its_days;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call(
                "repeat_rule",
                &[
                    ("e", Value::resource(&e)),
                    ("how", s.case("Repeat", "Repeat.weekdays")),
                    ("every", Value::Int(2)),
                    ("weekdays", Value::List(vec![Value::text("MO"), Value::text("WE")])),
                    ("until", Value::some(Value::text("2026-12-31"))),
                ],
            );
            assert_eq!(
                s.text(&e, "recurrence"),
                "FREQ=WEEKLY;BYDAY=MO,WE",
                "the days chosen are the rule's own"
            );
            assert!(matches!(s.read(&e, "recurrence_every"), Value::Int(2)));
            s.call("repeat_off", &[("e", Value::resource(&e))]);
            assert_eq!(s.text(&e, "recurrence"), "");
        }

        phase a_skipped_date_is_left_out_and_given_back;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.repeat(&e, "Repeat.weekly");
            s.call(
                "skip_date",
                &[("e", Value::resource(&e)), ("day", Value::text("2026-12-01"))],
            );
            assert_eq!(s.texts_of_set(&e, "exdates"), ["2026-12-01"]);
            s.call(
                "restore_date",
                &[("e", Value::resource(&e)), ("day", Value::text("2026-12-01"))],
            );
            assert!(s.texts_of_set(&e, "exdates").is_empty());
        }

        phase a_cancelled_date_stands_apart_and_is_restored;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.repeat(&e, "Repeat.weekly");
            s.call(
                "cancel_occurrence",
                &[("e", Value::resource(&e)), ("at", Value::text("2026-12-01T09:00:00Z"))],
            );
            let events = s.projected(&s.root, "events");
            let apart = events
                .iter()
                .find(|x| x["fields"]["original_start"] == "2026-12-01T09:00:00")
                .expect("the cancelled date stands apart");
            assert_eq!(apart["fields"]["cancelled"], true);
            s.call(
                "restore_occurrence",
                &[("e", Value::resource(&e)), ("at", Value::text("2026-12-01T09:00:00Z"))],
            );
            assert!(
                !s.projected(&s.root, "events")
                    .iter()
                    .any(|x| x["fields"]["original_start"] == "2026-12-01T09:00:00"),
                "the date is the series' again"
            );
        }

        phase one_date_moves_apart_and_the_series_stays;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.repeat(&e, "Repeat.weekly");
            s.call(
                "move_occurrence",
                &[
                    ("e", Value::resource(&e)),
                    ("at", Value::text("2026-12-01T09:00:00Z")),
                    ("start", Value::text("2026-12-01T11:00:00Z")),
                    ("end", Value::text("2026-12-01T12:00:00Z")),
                ],
            );
            assert_eq!(s.text(&e, "start"), "2026-11-24T09:00:00", "the series stays");
            let apart = s
                .projected(&s.root, "events")
                .into_iter()
                .find(|x| x["fields"]["original_start"] == "2026-12-01T09:00:00")
                .expect("the date stands apart");
            assert_eq!(apart["fields"]["start"], "2026-12-01T11:00:00");
            assert_eq!(apart["fields"]["day"], "2026-12-01");
        }

        phase a_series_moves_from_a_date_and_ends_the_day_before;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.repeat(&e, "Repeat.weekly");
            s.call(
                "move_following",
                &[
                    ("e", Value::resource(&e)),
                    ("at", Value::text("2026-12-01T09:00:00Z")),
                    ("start", Value::text("2026-12-01T11:00:00Z")),
                    ("end", Value::text("2026-12-01T12:00:00Z")),
                ],
            );
            assert_eq!(
                s.optional_text(&e, "recurrence_until").as_deref(),
                Some("2026-11-30")
            );
            let tail = s
                .projected(&s.root, "events")
                .into_iter()
                .find(|x| x["fields"]["start"] == "2026-12-01T11:00:00")
                .expect("the tail is a series of its own");
            assert_eq!(tail["fields"]["recurrence"], "FREQ=WEEKLY");
        }

        phase a_guest_is_invited_once_and_removed;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            let g = s.call_ref(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Ada")),
                    ("email", Value::text("ada@example.org")),
                ],
            );
            assert_eq!(s.text(&g, "name"), "Ada");
            assert_eq!(s.text(&g, "status"), "needs-action");
            assert_eq!(s.text(&g, "role"), "required");
            assert_eq!(s.projected(&e, "attendees").len(), 1);
            s.call("uninvite", &[("e", Value::resource(&e)), ("g", Value::resource(&g))]);
            assert!(s.projected(&e, "attendees").is_empty());
        }

        phase an_invitation_by_mail_waits_for_a_mailbox;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            let g = s.call_ref(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Ada")),
                    ("email", Value::text("ada@example.org")),
                ],
            );
            // Mail holds no account here: the invitation is not prepared, and the person is
            // told what to add. What a prepared invitation is, and the calendar part it
            // carries, Mail's own test holds (apps/tl/mail's hosted test).
            let said = s.refusal(
                "invite_by_mail",
                &[("e", Value::resource(&e)), ("g", Value::resource(&g))],
            );
            assert!(
                said.contains("Add a mail account"),
                "the person is told what to add: {said}"
            );
        }

        phase an_event_is_filed_under_a_project_and_under_a_calendar;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            let p = s.call_ref("new_project", &[("c", Value::resource(&s.root))]);
            s.call(
                "file_event",
                &[("e", Value::resource(&e)), ("p", Value::some(Value::resource(&p)))],
            );
            assert!(matches!(s.read(&e, "filed_under"), Value::Opt(Some(_))));
            let k = s.make_calendar("Team");
            s.call("file_in", &[("e", Value::resource(&e)), ("k", Value::resource(&k))]);
            assert!(matches!(s.read(&e, "calendar"), Value::Opt(Some(_))));
        }

        phase categories_the_organiser_and_the_zone_are_set_and_taken_off;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call(
                "add_category",
                &[("e", Value::resource(&e)), ("category", Value::text("team"))],
            );
            assert_eq!(s.texts_of_set(&e, "categories"), ["team"]);
            s.call(
                "remove_category",
                &[("e", Value::resource(&e)), ("category", Value::text("team"))],
            );
            assert!(s.texts_of_set(&e, "categories").is_empty());
            s.call(
                "organize",
                &[("e", Value::resource(&e)), ("who", Value::text("bo@example.org"))],
            );
            assert_eq!(s.text(&e, "organizer"), "bo@example.org");
            s.call(
                "set_zone",
                &[("e", Value::resource(&e)), ("zone", Value::text("Europe/Berlin"))],
            );
            assert_eq!(s.text(&e, "timezone"), "Europe/Berlin");
        }

        phase reminders_sit_at_the_presets_and_clear;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call("remind_before", &[("e", Value::resource(&e)), ("minutes", Value::Int(30))]);
            assert_eq!(s.text(&e, "remind"), "2026-11-24T08:30:00Z");
            s.call("remind_at_start", &[("e", Value::resource(&e))]);
            assert_eq!(s.text(&e, "remind"), "2026-11-24T09:00:00Z");
            s.call("remind_off", &[("e", Value::resource(&e))]);
            assert_eq!(s.text(&e, "remind"), "1970-01-01T00:00:00Z");
        }

        phase a_task_is_made_dated_prioritised_and_filed;
        {
            let s = sheet();
            let t = s.make_task("Write notes");
            s.call("due_task_tomorrow", &[("t", Value::resource(&t))]);
            assert_eq!(s.optional_text(&t, "due").as_deref(), Some("2026-11-25"));
            s.call("due_task_today", &[("t", Value::resource(&t))]);
            assert_eq!(s.optional_text(&t, "due").as_deref(), Some(TODAY));
            s.call(
                "set_task_priority",
                &[("t", Value::resource(&t)), ("priority", s.case("Priority", "Priority.high"))],
            );
            let p = s.call_ref("new_project", &[("c", Value::resource(&s.root))]);
            s.call(
                "file_task",
                &[("t", Value::resource(&t)), ("p", Value::some(Value::resource(&p)))],
            );
            assert!(matches!(s.read(&t, "project"), Value::Opt(Some(_))));
            s.call("check_task", &[("t", Value::resource(&t))]);
            assert!(matches!(s.read(&t, "done"), Value::Bool(true)));
            s.call("due_task_clear", &[("t", Value::resource(&t))]);
            assert!(matches!(s.read(&t, "due"), Value::Opt(None)));
        }

        phase a_step_stands_under_its_task_and_goes_with_it;
        {
            let s = sheet();
            let t = s.make_task("Ship");
            let step = s.call_ref(
                "add_step",
                &[
                    ("c", Value::resource(&s.root)),
                    ("t", Value::resource(&t)),
                    ("title", Value::text("Write notes")),
                ],
            );
            assert!(matches!(s.read(&step, "parent"), Value::Opt(Some(_))));
            s.call(
                "remove_task",
                &[("c", Value::resource(&s.root)), ("t", Value::resource(&t))],
            );
            assert!(
                s.projected(&s.root, "tasks").is_empty(),
                "the step went with its task"
            );
        }

        phase an_event_is_deleted;
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call(
                "remove",
                &[("c", Value::resource(&s.root)), ("e", Value::resource(&e))],
            );
            assert!(s.projected(&s.root, "events").is_empty());
        }

        phase the_period_moves_a_week_a_month_and_a_day;
        {
            let s = sheet();
            let root = Value::resource(&s.root);
            let anchor = |s: &Sheet| s.text(&s.root, "anchor_at");
            // Opening the calendar stands it on today, at its midnight.
            s.call("land", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-24T00:00:00Z");
            s.call("previous_week", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-17T00:00:00Z");
            s.call("next_week", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-24T00:00:00Z");
            s.call("next_month", &[("c", root.clone())]);
            assert_eq!(
                anchor(&s),
                "2026-12-01T00:00:00Z",
                "a month on from the first of the month"
            );
            s.call("previous_month", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-01T00:00:00Z", "back on the first");
            s.call("this_week", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-23T00:00:00Z", "the week's Monday");
            s.call("this_day", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-24T00:00:00Z");
            s.call("previous_day", &[("c", root.clone())]);
            assert_eq!(anchor(&s), "2026-11-23T00:00:00Z");
            s.call("next_day", &[("c", root)]);
            assert_eq!(anchor(&s), "2026-11-24T00:00:00Z");
        }

        phase a_project_deletes_and_its_events_and_tasks_wait_in_none;
        {
            let s = sheet();
            let p = s.call_ref("new_project", &[("c", Value::resource(&s.root))]);
            let e = s.make("Standup", TODAY);
            let t = s.make_task("Write notes");
            s.call(
                "file_event",
                &[("e", Value::resource(&e)), ("p", Value::some(Value::resource(&p)))],
            );
            s.call(
                "file_task",
                &[("t", Value::resource(&t)), ("p", Value::some(Value::resource(&p)))],
            );
            s.call(
                "remove_project",
                &[("c", Value::resource(&s.root)), ("p", Value::resource(&p))],
            );
            assert!(matches!(s.read(&e, "filed_under"), Value::Opt(None)));
            assert!(matches!(s.read(&t, "project"), Value::Opt(None)));
        }
    }

    /// The place the window's sidebar has chosen.
    fn chosen_place(w: &ViewInstance) -> Value {
        w.tree()
            .first_of_kind("ui.NavList")
            .and_then(|list| list.props.get("selected"))
            .cloned()
            .expect("the sidebar says which place it shows")
    }

    tilo_test_env::scenario! {
        fn calendar_1;

        phase the_day_draws_what_is_all_day_over_the_events_at_their_times;
        // The day window draws what is all day and what is timed.
        {
            let s = sheet();
            s.opened();
            s.make("Standup", TODAY);
            s.make("Offsite", TODAY);
            let w = s.timeline("day");
            let shown = drawn(w.tree());
            assert!(
                shown.iter().any(|t| t == "Standup") && shown.iter().any(|t| t == "Offsite"),
                "the day draws both events: {shown:?}"
            );
        }

        phase the_week_draws_an_event_on_every_day_it_runs_through;
        // The week draws an event on every day it runs through, not only the day it starts.
        {
            let s = sheet();
            s.opened();
            let e = s.make("Offsite", TODAY);
            s.call(
                "change_end",
                &[
                    ("e", Value::resource(&e)),
                    ("at", Value::text("2026-11-26T10:00:00Z")),
                ],
            );
            let w = s.timeline("week");
            let offsite: Vec<&UiNode> = of_kind(w.tree(), "ui.TimelineItem")
                .into_iter()
                .filter(|item| text_prop_opt(item, "title").as_deref() == Some("Offsite"))
                .collect();
            assert_eq!(
                offsite.len(),
                1,
                "one event, drawn over the days it runs through"
            );
            let end = text_prop_opt(offsite[0], "end").expect("it has an end");
            assert!(
                end.starts_with("2026-11-26"),
                "it runs to Thursday, the day it ends on: {end}"
            );
        }

        phase the_agenda_bands_the_days_in_the_event_zone;
        // The agenda bands its rows by the event's own zone, one list carrying a repeating
        // meeting between the one-off events.
        {
            let s = sheet();
            s.opened();
            let e = s.make("Late call", TODAY);
            s.call(
                "set_zone",
                &[
                    ("e", Value::resource(&e)),
                    ("zone", Value::text("Asia/Tokyo")),
                ],
            );
            s.call(
                "change_start",
                &[
                    ("e", Value::resource(&e)),
                    ("at", Value::text("2026-11-24T23:30:00Z")),
                ],
            );
            s.repeat(&e, "Repeat.weekly");
            let w = s.place("agenda");
            let bands: Vec<String> = of_kind(w.tree(), "ui.Section")
                .iter()
                .filter_map(|section| text_prop_opt(section, "title"))
                .collect();
            // Tokyo is nine hours on, so the late evening of the 24th is the 25th there.
            assert!(
                bands.contains(&"2026-11-25".to_owned()),
                "the band is the event's own day: {bands:?}"
            );
        }

        phase the_month_shows_a_series_in_every_week_it_meets;
        // The month is the timeline's month: a series stands in every week it meets.
        {
            let s = sheet();
            s.opened();
            let e = s.make("Standup", "2026-11-03");
            s.repeat(&e, "Repeat.weekly");
            let w = s.timeline("month");
            let items = of_kind(w.tree(), "ui.TimelineItem")
                .iter()
                .filter(|item| text_prop_opt(item, "title").as_deref() == Some("Standup"))
                .count();
            assert!(items >= 4, "four Tuesdays of that month hold the meeting");
        }

        phase a_series_every_other_week_or_twice_stands_on_those_dates_alone;
        // A series that repeats every other week, or a set number of times, stands on those dates
        // alone: the rule the event keeps is one the shared expander reads.
        {
            let standups = |every: i64, count: Option<i64>| {
                let s = sheet();
                s.opened();
                let e = s.make("Standup", "2026-11-03");
                let mut given = vec![
                    ("e", Value::resource(&e)),
                    ("how", s.case("Repeat", "Repeat.weekly")),
                    ("every", Value::Int(every)),
                ];
                if let Some(n) = count {
                    given.push(("count", Value::some(Value::Int(n))));
                }
                s.call("repeat_rule", &given);
                let w = s.timeline("month");
                of_kind(w.tree(), "ui.TimelineItem")
                    .iter()
                    .filter(|item| text_prop_opt(item, "title").as_deref() == Some("Standup"))
                    .count()
            };
            let weekly = standups(1, None);
            let fortnightly = standups(2, None);
            assert!(weekly >= 4, "every Tuesday of the month holds it: {weekly}");
            assert!(
                fortnightly >= 2 && fortnightly < weekly,
                "every other Tuesday holds it, not every one: {fortnightly} of {weekly}"
            );
            assert_eq!(standups(1, Some(2)), 2, "twice is two dates");
        }

        phase the_tasks_window_lists_the_list_chosen;
        // The tasks window lists the list the person chose.
        {
            let s = sheet();
            s.opened();
            let t = s.make_task("Write notes");
            s.call("due_task_today", &[("t", Value::resource(&t))]);
            let w = s.place_view("tasks");
            assert!(labels(w.tree()).iter().any(|x| x == "Write notes"));
        }

        phase a_caller_opens_the_window_at_a_place;
        // A caller opens the window at a place by its name, as the launcher, the palette, an agent
        // or a script do: the week, the tasks, one of their lists, a project's. The window goes there
        // for the person at it and no one else, and the calendar itself does not change.
        {
            let s = sheet();
            s.opened();
            let today = s.make_task("Write notes");
            s.call("due_task_today", &[("t", Value::resource(&today))]);
            s.make_task("Buy milk");
            let done = s.make_task("File taxes");
            s.call("check_task", &[("t", Value::resource(&done))]);
            let garden = s.call_ref("new_project", &[("c", Value::resource(&s.root))]);
            s.call(
                "rename_project",
                &[
                    ("p", Value::resource(&garden)),
                    ("name", Value::text("Garden")),
                ],
            );
            let tulips = s.make_task("Plant tulips");
            s.call(
                "file_task",
                &[
                    ("t", Value::resource(&tulips)),
                    ("p", Value::some(Value::resource(&garden))),
                ],
            );
            let standup = s.make("Standup", TODAY);
            s.make("Lunch", TODAY);
            let team = s.make_calendar("Team");
            s.call(
                "file_in",
                &[
                    ("e", Value::resource(&standup)),
                    ("k", Value::resource(&team)),
                ],
            );
            let anchor = s.read(&s.root, "anchor_at");

            // Bo is a member too, and looks at the same window.
            stage_members(
                &s.rt,
                vec![
                    tilolang_runtime::members::Member::person("me", tilo_actions::policy::Role::Owner),
                    tilolang_runtime::members::Member::person("bo", tilo_actions::policy::Role::Editor),
                ],
                None,
            );
            let open = |who: &Actor| {
                let mut window =
                    s.rt.open_window(
                        who,
                        "window-1",
                        &s.app.view("window"),
                        vec![Value::resource(&s.root)],
                    )
                    .unwrap();
                window.set_lazy_rows(false).unwrap();
                window
            };
            let mut w = open(&me());
            let mut other = open(&person("bo"));
            let shown = |w: &mut ViewInstance, place: &str| {
                assert!(
                    w.show_place(place).unwrap(),
                    "the window has the place {place}"
                );
                w.poll_patches().unwrap();
                labels(w.tree())
            };
            let tasks = shown(&mut w, "tasks");
            assert_eq!(chosen_place(&w), Value::text("tasks"));
            assert!(tasks.iter().any(|x| x == "Write notes"), "{tasks:?}");
            let inbox = shown(&mut w, "inbox");
            assert!(inbox.iter().any(|x| x == "Buy milk"), "{inbox:?}");
            assert!(!inbox.iter().any(|x| x == "Write notes"), "{inbox:?}");
            let finished = shown(&mut w, "done");
            assert!(finished.iter().any(|x| x == "File taxes"), "{finished:?}");
            assert!(!finished.iter().any(|x| x == "Buy milk"), "{finished:?}");
            // A project is a resource: showing it lists its tasks, as choosing it in the sidebar does.
            assert!(w.show(&garden).unwrap(), "the window answers for a project");
            w.poll_patches().unwrap();
            let planted = labels(w.tree());
            assert!(planted.iter().any(|x| x == "Plant tulips"), "{planted:?}");
            assert!(!planted.iter().any(|x| x == "Buy milk"), "{planted:?}");
            shown(&mut w, "week");
            assert_eq!(chosen_place(&w), Value::text("week"));
            // A calendar is a resource too: showing it narrows the week to its events, and all the
            // calendars are a place to come back to.
            assert!(w.show(&team).unwrap(), "the window answers for a calendar");
            w.poll_patches().unwrap();
            let narrowed = drawn(w.tree());
            assert!(narrowed.iter().any(|x| x == "Standup"), "{narrowed:?}");
            assert!(!narrowed.iter().any(|x| x == "Lunch"), "{narrowed:?}");
            let every = {
                assert!(w.show_place("calendars").unwrap());
                w.poll_patches().unwrap();
                drawn(w.tree())
            };
            assert!(every.iter().any(|x| x == "Lunch"), "{every:?}");
            assert!(
                !w.show_place("nowhere").unwrap(),
                "a place it does not declare"
            );

            // The place is this window's, kept for its person: another window stands where it stood,
            // and the calendar's own day is where it was.
            other.poll_patches().unwrap();
            assert_eq!(chosen_place(&other), Value::text("month"));
            assert_eq!(s.read(&s.root, "anchor_at"), anchor);
        }

        phase a_calendar_kept_out_is_left_out_and_comes_back;
        // A calendar the person kept out is left out of the week, and shown again it returns.
        {
            let s = sheet();
            s.opened();
            let e = s.make("Standup", TODAY);
            let k = s.make_calendar("Team");
            s.call(
                "file_in",
                &[("e", Value::resource(&e)), ("k", Value::resource(&k))],
            );
            let mut w = s.timeline("week");
            assert!(drawn(w.tree()).iter().any(|l| l == "Standup"));
            let hide = format!("{}-hide", k.id_string());
            press_nav(&mut w, &hide);
            assert!(
                !drawn(w.tree()).iter().any(|l| l == "Standup"),
                "the kept-out calendar's events are not drawn"
            );
            let show = format!("{}-show", k.id_string());
            press_nav(&mut w, &show);
            assert!(
                drawn(w.tree()).iter().any(|l| l == "Standup"),
                "shown again, its events come back"
            );
        }

        phase an_export_carries_events_tasks_dates_and_completion;
        // An exported calendar carries its events and tasks, an event's moment, and a finished
        // task as completed.
        {
            let s = sheet();
            s.make("Standup", TODAY);
            let t = s.make_task("Write notes");
            s.call("check_task", &[("t", Value::resource(&t))]);
            let e = s.make("Offsite", TODAY);
            s.call(
                "set_event_color",
                &[
                    ("e", Value::resource(&e)),
                    ("color", s.case("Color", "Color.violet")),
                ],
            );
            s.call(
                "organize",
                &[
                    ("e", Value::resource(&e)),
                    ("who", Value::text("bo@example.org")),
                ],
            );
            s.call(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Ada")),
                    ("email", Value::text("ada@example.org")),
                ],
            );
            s.call(
                "remind_before",
                &[("e", Value::resource(&e)), ("minutes", Value::Int(30))],
            );
            s.repeat(&e, "Repeat.weekly");
            s.call(
                "skip_date",
                &[
                    ("e", Value::resource(&e)),
                    ("day", Value::text("2026-12-01")),
                ],
            );
            let out = s.invoke("export_calendar", &[("c", Value::resource(&s.root))]);
            let InvokeOutcome::Done(Value::File(file)) = out else {
                panic!("an export answers a file: {out:?}");
            };
            let text = String::from_utf8(s.device.bytes_of(&file)).unwrap();
            assert!(text.contains("BEGIN:VCALENDAR"));
            assert!(text.contains("SUMMARY:Standup"));
            assert!(text.contains("DTSTART:20261124T090000"));
            assert!(text.contains("X-TILO-COLOR:violet"));
            assert!(text.contains("ORGANIZER:mailto:bo@example.org"));
            assert!(text.contains("ATTENDEE;CN=Ada;ROLE=required;PARTSTAT=NEEDS-ACTION"));
            assert!(text.contains("RRULE:FREQ=WEEKLY"));
            assert!(text.contains("EXDATE;VALUE=DATE:20261201"));
            assert!(text.contains("BEGIN:VALARM") && text.contains("TRIGGER:-PT30M"));
            assert!(text.contains("BEGIN:VTODO") && text.contains("STATUS:COMPLETED"));
        }

        phase an_import_reads_dates_times_repeats_and_tasks;
        // An imported file makes events and tasks: a zoned moment, a day, a repeat, a skipped
        // date and a finished task.
        {
            let s = sheet();
            let ics = "BEGIN:VCALENDAR\r\n\
                   VERSION:2.0\r\n\
                   BEGIN:VEVENT\r\n\
                   SUMMARY:Weekly sync\r\n\
                   DTSTART;TZID=Europe/Berlin:20261123T090000\r\n\
                   DTEND;TZID=Europe/Berlin:20261123T100000\r\n\
                   LOCATION:Room 4\r\n\
                   RRULE:FREQ=WEEKLY\r\n\
                   EXDATE:20261207T090000Z\r\n\
                   ORGANIZER:mailto:bo@example.org\r\n\
                   ATTENDEE;CN=Ada;ROLE=optional;PARTSTAT=ACCEPTED:mailto:ada@example.org\r\n\
                   X-TILO-COLOR:violet\r\n\
                   BEGIN:VALARM\r\n\
                   ACTION:DISPLAY\r\n\
                   TRIGGER:-PT30M\r\n\
                   END:VALARM\r\n\
                   END:VEVENT\r\n\
                   BEGIN:VTODO\r\n\
                   SUMMARY:Write notes\r\n\
                   DUE;VALUE=DATE:20261126\r\n\
                   STATUS:COMPLETED\r\n\
                   PRIORITY:1\r\n\
                   END:VTODO\r\n\
                   END:VCALENDAR\r\n";
            let file = s
                .device
                .keeping("calendar.ics", "text/calendar", ics.as_bytes());
            s.call(
                "import_ics",
                &[("c", Value::resource(&s.root)), ("file", Value::file(file))],
            );
            let events = s.projected(&s.root, "events");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["fields"]["title"], "Weekly sync");
            // The wall time and the zone it is read in are kept as the file held them.
            assert_eq!(events[0]["fields"]["start"], "2026-11-23T09:00:00");
            assert_eq!(events[0]["fields"]["end"], "2026-11-23T10:00:00");
            assert_eq!(events[0]["fields"]["timezone"], "Europe/Berlin");
            assert_eq!(events[0]["fields"]["location"], "Room 4");
            assert_eq!(events[0]["fields"]["recurrence"], "FREQ=WEEKLY");
            assert_eq!(events[0]["fields"]["color"], "violet");
            assert_eq!(events[0]["fields"]["organizer"], "bo@example.org");
            assert_eq!(events[0]["fields"]["attendees"]["count"], 1);
            // The alarm rings half an hour before it starts.
            assert_eq!(events[0]["fields"]["remind"], "2026-11-23T07:30:00Z");
            let tasks = s.projected(&s.root, "tasks");
            assert_eq!(tasks.len(), 1);
            assert_eq!(tasks[0]["fields"]["title"], "Write notes");
            assert_eq!(tasks[0]["fields"]["done"], true);
            assert_eq!(tasks[0]["fields"]["due"], "2026-11-26");
        }

        phase a_calendar_imported_again_keeps_one_of_each_event;
        // Another computer's mail program kept this calendar: its zone behind Thunderbird's own
        // prefix, a long line folded and its commas escaped. Importing it twice keeps one event,
        // the one its UID names.
        {
            let s = sheet();
            let ics = "BEGIN:VCALENDAR\r\n\
                   VERSION:2.0\r\n\
                   BEGIN:VEVENT\r\n\
                   UID:review-1@example.org\r\n\
                   SUMMARY:Design review\\, with the whole team and every\r\n  guest\r\n\
                   DTSTART;TZID=/mozilla.org/20050126_1/America/New_York:20261105T090000\r\n\
                   DTEND;TZID=/mozilla.org/20050126_1/America/New_York:20261105T100000\r\n\
                   END:VEVENT\r\n\
                   END:VCALENDAR\r\n";
            for _ in 0..2 {
                let file = s
                    .device
                    .keeping("calendar.ics", "text/calendar", ics.as_bytes());
                s.call(
                    "import_ics",
                    &[("c", Value::resource(&s.root)), ("file", Value::file(file))],
                );
            }
            let events = s.projected(&s.root, "events");
            assert_eq!(events.len(), 1, "{events:#?}");
            let fields = &events[0]["fields"];
            assert_eq!(
                fields["title"],
                "Design review, with the whole team and every guest"
            );
            assert_eq!(fields["key"], "review-1@example.org");
            assert_eq!(fields["timezone"], "America/New_York");
            assert_eq!(fields["start"], "2026-11-05T09:00:00");
        }

        phase searching_the_window_narrows_what_a_place_lists;
        // The window's search narrows what a place lists.
        {
            let s = sheet();
            s.opened();
            s.make("Standup", TODAY);
            s.make("Offsite", TODAY);
            let mut w = s.root_window();
            // The agenda lists what is coming; the bar's search narrows it.
            let agenda = w
                .tree()
                .of_kind("ui.NavItem")
                .into_iter()
                .find(|n| n.props.get("key") == Some(&Value::text("agenda")))
                .expect("the window offers the agenda")
                .key
                .clone();
            w.fire(&agenda, "select").unwrap();
            w.poll_patches().unwrap();
            let shell = w.tree().of_kind("ui.NavShell")[0].key.clone();
            w.fire_with(&shell, "search", Some(&Value::text("Stand")))
                .unwrap();
            w.poll_patches().unwrap();
            let shown = labels(w.tree());
            assert!(shown.iter().any(|t| t == "Standup"), "{shown:?}");
            assert!(
                !shown.iter().any(|t| t == "Offsite"),
                "the search narrows it: {shown:?}"
            );
        }
    }

    tilo_test_env::scenario! {
        fn calendar_2;

        phase a_reminder_tells_the_people_coming;
        // A reminder's moment tells the people coming: the run raises one notice, addressed to the
        // person, naming the event, and clears the reminder so it is told once.
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call("remind_at_start", &[("e", Value::resource(&e))]);
            s.call("remind_now", &[("e", Value::resource(&e))]);
            let raised = s.raised.notices();
            assert_eq!(raised.len(), 1, "one notice, to the one person coming");
            assert_eq!(raised[0].title, "Standup");
            assert_eq!(raised[0].to, me());
            assert_eq!(raised[0].body, "Starts 2026-11-24T09:00:00");
            assert_eq!(s.text(&e, "remind"), "1970-01-01T00:00:00Z");
            // The reminder is gone, so the same action refuses rather than telling them twice.
            let _ = s.rt.invoke_action(
                &me(),
                (
                    s.app.action("remind_now").as_str(),
                    s.app
                        .args(&s.rt, "remind_now", &[("e", Value::resource(&e))]),
                ),
            );
            assert_eq!(s.raised.notices().len(), 1, "nothing is told twice");
        }

        phase a_line_that_names_only_a_day_makes_an_all_day_event;
        // A quick-add line that names only a day makes an event of the whole day; one that names
        // a time is timed, and a day's own entry names the hour it starts at, so it is timed too.
        {
            let s = sheet();
            let whole = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Offsite")),
                    ("day", Value::some(Value::text(TODAY))),
                ],
            );
            assert!(matches!(s.read(&whole, "all_day"), Value::Bool(true)));
            assert_eq!(s.text(&whole, "start"), "2026-11-24T00:00:00");
            assert_eq!(s.text(&whole, "end"), "2026-11-25T00:00:00");
            // Its end is the next midnight, so it is on one day.
            assert_eq!(s.text(&whole, "end_day"), "2026-11-24");

            // A time the line names makes it timed.
            let timed = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Lunch")),
                    ("day", Value::some(Value::text(TODAY))),
                    ("at", Value::some(Value::text("2026-11-24T13:00:00Z"))),
                ],
            );
            assert!(matches!(s.read(&timed, "all_day"), Value::Bool(false)));
            assert_eq!(s.text(&timed, "start"), "2026-11-24T13:00:00");
            assert_eq!(s.text(&timed, "end"), "2026-11-24T14:00:00");

            // A line that names a time makes it timed: the same rule, an hour long.
            let nine = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Standup")),
                    ("day", Value::some(Value::text(TODAY))),
                    ("at", Value::some(Value::text("2026-11-24T09:00:00Z"))),
                ],
            );
            assert!(matches!(s.read(&nine, "all_day"), Value::Bool(false)));
            assert_eq!(s.text(&nine, "start"), "2026-11-24T09:00:00");
            assert_eq!(s.text(&nine, "end"), "2026-11-24T10:00:00");
        }

        phase a_floating_event_made_in_berlin_reads_nine_in_los_angeles;
        // A floating event made by a member in Berlin reads nine for a member in Los Angeles: what
        // is kept is the wall time, and each reader resolves it where they are.
        {
            let s = sheet();
            s.rt.set_zone(|| 3600);
            s.rt.set_zone_name(|| Some("Europe/Berlin".to_owned()));
            let e = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Standup")),
                    ("day", Value::some(Value::text(TODAY))),
                    ("at", Value::some(Value::text("2026-11-24T08:00:00Z"))),
                ],
            );
            assert_eq!(s.text(&e, "start"), "2026-11-24T09:00:00");
            assert_eq!(s.text(&e, "timezone"), "floating");
            // Read in Los Angeles, the same event starts at nine there: 17:00 UTC in November.
            s.rt.set_zone(|| -8 * 3600);
            s.rt.set_zone_name(|| Some("America/Los_Angeles".to_owned()));
            s.call("remind_at_start", &[("e", Value::resource(&e))]);
            assert_eq!(
                s.text(&e, "remind"),
                "2026-11-24T17:00:00Z",
                "nine in Los Angeles is seventeen UTC"
            );
        }

        phase a_title_typed_in_one_burst_makes_one_event_with_the_whole_title;
        // A title typed in one burst and saved at once makes one event carrying the whole title:
        // the title is the event's bound field, and creating it is one action that reads the
        // finished field — nothing runs per keystroke and nothing takes a partial value.
        {
            let s = sheet();
            let e = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Dentist appointment")),
                    ("day", Value::some(Value::text(TODAY))),
                    ("at", Value::some(Value::text("2026-11-24T10:00:00Z"))),
                ],
            );
            assert_eq!(s.text(&e, "title"), "Dentist appointment");
            let events = s.projected(&s.root, "events");
            assert_eq!(events.len(), 1, "one event, not two");
            assert_eq!(events[0]["fields"]["title"], "Dentist appointment");
        }

        phase the_sidebar_picks_a_day;
        // The sidebar's own calendar stands the window on the day it picks.
        {
            let s = sheet();
            s.opened();
            let mut w = s.root_window();
            let picker = w
                .tree()
                .of_kind("ui.DatePicker")
                .first()
                .expect("the sidebar offers a calendar")
                .key
                .clone();
            w.fire_with(
                &picker,
                "change",
                Some(&Value::text("2026-12-01T00:00:00Z")),
            )
            .unwrap();
            w.poll_patches().unwrap();
            assert_eq!(s.text(&s.root, "anchor_at"), "2026-12-01T00:00:00Z");
        }

        phase the_surface_is_what_the_package_declares;
        // What the package declares is what it publishes, under its own id.
        {
            let s = sheet();
            let meta = s.app.meta();
            assert_eq!(meta.app, "so.tilo.calendar");
            for name in [
                "new_event",
                "new_task",
                "repeat",
                "repeat_rule",
                "skip_date",
                "cancel_occurrence",
                "move_occurrence",
                "move_following",
                "invite",
                "invite_by_mail",
                "export_calendar",
                "import_ics",
                "set_event_color",
                "set_calendar_color",
            ] {
                assert!(
                    meta.actions.iter().any(|a| a.name == name),
                    "the package publishes `{name}`"
                );
            }
        }

        phase the_window_names_the_period_it_shows;
        // The window names the period it shows.
        {
            let s = sheet();
            s.call("land", &[("c", Value::resource(&s.root))]);
            let w = s.window("window", vec![Value::resource(&s.root)]);
            let shells = of_kind(w.tree(), "ui.NavShell");
            assert_eq!(shells.len(), 1);
            assert_eq!(
                text_prop_opt(shells[0], "title"),
                Some("2026-11-01 – 2026-11-30".to_owned()),
                "the month of the 24th"
            );
        }

        phase a_calendar_account_that_cannot_be_reached_is_said_over_the_window;
        // An account's own record is what the window says of its server: one that cannot be
        // reached names itself and the server's words, with the action that asks it again.
        {
            let s = sheet();
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "accounts"),
                &type_id(&s.app, "Account"),
                vec![
                    ("key", Value::text("acct")),
                    ("address", Value::text("me@example.test")),
                    ("name", Value::text("Work")),
                    ("server", Value::text("https://cal.example.test")),
                    ("read_only", Value::Bool(false)),
                    ("trouble", Value::text("connection refused")),
                    ("online", Value::Bool(false)),
                    ("checking", Value::Bool(false)),
                ],
            );
            let w = s.root_window();
            let banner = of_kind(w.tree(), "ui.Banner")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window says why: {}", dump(w.tree())));
            assert_eq!(
                text_prop_opt(banner, "title").as_deref(),
                Some("Can't reach Work")
            );
            assert!(
                text_prop_opt(banner, "message")
                    .unwrap_or_default()
                    .contains("Connection refused"),
                "the server's own reason is said"
            );
            assert_eq!(
                text_prop_opt(banner, "action").as_deref(),
                Some("Sync again")
            );
        }

        phase an_account_being_asked_right_now_says_so;
        // An account asking its server right now says so, and takes no room once the answer came.
        {
            let s = sheet();
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "accounts"),
                &type_id(&s.app, "Account"),
                vec![
                    ("key", Value::text("acct")),
                    ("address", Value::text("me@example.test")),
                    ("name", Value::text("Work")),
                    ("online", Value::Bool(true)),
                    ("checking", Value::Bool(true)),
                ],
            );
            let w = s.root_window();
            let spinner = of_kind(w.tree(), "ui.Spinner")
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("the window says it is asking: {}", dump(w.tree())));
            assert_eq!(
                text_prop_opt(spinner, "label").as_deref(),
                Some("Syncing Work…")
            );
        }

        phase answering_your_own_invitation_tells_the_organiser_through_the_server;
        // The person's own invitation is answered at the account's server, so the organiser hears
        // it; another guest's answer stays the organiser's record and tells nobody.
        {
            let s = sheet();
            let account = seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "accounts"),
                &type_id(&s.app, "Account"),
                vec![
                    ("key", Value::text("acct")),
                    ("address", Value::text("me@example.test")),
                    ("name", Value::text("Work")),
                    ("online", Value::Bool(true)),
                ],
            );
            let _ = account;
            let calendar = seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "calendars"),
                &type_id(&s.app, "Calendar"),
                vec![
                    ("key", Value::text("work")),
                    ("account", Value::text("acct")),
                    ("name", Value::text("Work")),
                    ("color", Value::text("")),
                    ("writable", Value::Bool(true)),
                    ("sync_token", Value::text("")),
                ],
            );
            let e = s.make("Planning", TODAY);
            s.call(
                "file_in",
                &[
                    ("e", Value::resource(&e)),
                    ("k", Value::resource(&calendar)),
                ],
            );
            assert_eq!(
                s.read(&e, "calendar"),
                Value::some(Value::resource(&calendar)),
                "the event stands in the account's calendar"
            );
            let me = s.call_ref(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Me")),
                    ("email", Value::text("me@example.test")),
                ],
            );
            s.call(
                "answer_and_tell",
                &[
                    ("e", Value::resource(&e)),
                    ("g", Value::resource(&me)),
                    ("said", Value::text("accepted")),
                ],
            );
            // An outside effect is marked in one settle and handed to the server in the next.
            s.rt.settle().unwrap();
            s.rt.settle().unwrap();
            let delivered = s.sink.delivered();
            let (_capability, payload) = delivered
                .iter()
                .find(|(capability, _)| capability == "caldav.answer")
                .unwrap_or_else(|| panic!("the organiser is told: {delivered:?}"));
            assert_eq!(payload[0], "acct");
            assert_eq!(payload[2], "me@example.test");
            assert_eq!(payload[3], "accepted");
            assert_eq!(s.text(&me, "status"), "accepted");

            // Another guest's answer is kept here and tells the server nothing.
            let bo = s.call_ref(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Bo")),
                    ("email", Value::text("bo@example.test")),
                ],
            );
            s.call(
                "answer",
                &[
                    ("e", Value::resource(&e)),
                    ("g", Value::resource(&bo)),
                    ("status", Value::text("declined")),
                ],
            );
            s.rt.settle().unwrap();
            assert_eq!(
                s.sink
                    .delivered()
                    .iter()
                    .filter(|(capability, _)| capability == "caldav.answer")
                    .count(),
                1,
                "nobody else's answer is written at the server"
            );
            assert_eq!(s.text(&bo, "status"), "declined");
        }

        phase another_guests_invitation_is_not_answered_at_the_server;
        // A guest who is not the account's own address is refused an answer at the server.
        {
            let s = sheet();
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "accounts"),
                &type_id(&s.app, "Account"),
                vec![
                    ("key", Value::text("acct")),
                    ("address", Value::text("me@example.test")),
                    ("name", Value::text("Work")),
                    ("online", Value::Bool(true)),
                ],
            );
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "calendars"),
                &type_id(&s.app, "Calendar"),
                vec![
                    ("key", Value::text("work")),
                    ("account", Value::text("acct")),
                    ("name", Value::text("Work")),
                    ("writable", Value::Bool(true)),
                ],
            );
            let e = s.make("Planning", TODAY);
            let bo = s.call_ref(
                "invite",
                &[
                    ("e", Value::resource(&e)),
                    ("name", Value::text("Bo")),
                    ("email", Value::text("bo@example.test")),
                ],
            );
            let why = s.refusal(
                "answer_and_tell",
                &[
                    ("e", Value::resource(&e)),
                    ("g", Value::resource(&bo)),
                    ("said", Value::text("accepted")),
                ],
            );
            assert!(why.contains("your own invitation"), "{why}");
        }
    }

    tilo_test_env::scenario! {
        fn calendar_3;

        phase an_invitation_taken_in_becomes_its_event;
        // An invitation taken in with no account to write it makes the event: what it says, its
        // organiser and the people it names, and the answer it waits for.
        {
            let s = sheet();
            let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
                    UID:invite@example.test\r\nSUMMARY:Dinner\r\nDTSTART:20261127T190000Z\r\n\
                    DTEND:20261127T200000Z\r\nORGANIZER:mailto:bo@example.test\r\n\
                    ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:me@example.test\r\n\
                    END:VEVENT\r\nEND:VCALENDAR\r\n";
            s.call(
                "take_invitation",
                &[("c", Value::resource(&s.root)), ("text", Value::text(text))],
            );
            let events = s.projected(&s.root, "events");
            assert_eq!(events.len(), 1, "the invitation made its event");
            assert_eq!(events[0]["fields"]["title"], "Dinner");
            assert_eq!(events[0]["fields"]["organizer"], "bo@example.test");
            let made =
                s.rt.read(&s.root, &field_id(&s.app, "Calendars", "events"))
                    .unwrap();
            let Value::List(items) = made else {
                panic!("the events read as a list")
            };
            let event = match &items[0] {
                Value::Ref(r) => (*r.target).clone(),
                other => panic!("an event is a resource: {other:?}"),
            };
            let guests = s.projected(&event, "attendees");
            assert_eq!(guests.len(), 1);
            assert_eq!(guests[0]["fields"]["email"], "me@example.test");
            assert_eq!(guests[0]["fields"]["status"], "needs-action");
        }

        phase an_invitation_is_written_to_the_accounts_server_when_one_can_take_it;
        // An invitation an account can write is handed to its server, so the organiser hears the
        // answer from where the event is kept.
        {
            let s = sheet();
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "calendars"),
                &type_id(&s.app, "Calendar"),
                vec![
                    ("key", Value::text("work")),
                    ("account", Value::text("acct")),
                    ("name", Value::text("Work")),
                    ("writable", Value::Bool(true)),
                ],
            );
            let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\n\
                    UID:invite@example.test\r\nSUMMARY:Dinner\r\nDTSTART:20261127T190000Z\r\n\
                    DTEND:20261127T200000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            s.call(
                "take_invitation",
                &[("c", Value::resource(&s.root)), ("text", Value::text(text))],
            );
            // An outside effect is marked in one settle and handed to the server in the next.
            s.rt.settle().unwrap();
            s.rt.settle().unwrap();
            s.rt.settle().unwrap();
            let delivered = s.sink.delivered();
            let (_, payload) = delivered
                .iter()
                .find(|(capability, _)| capability == "caldav.import")
                .unwrap_or_else(|| panic!("the server takes it: {delivered:?}"));
            assert_eq!(payload[0], "acct");
            assert_eq!(payload[1], "work");
            assert!(
                payload[2]
                    .as_str()
                    .unwrap_or_default()
                    .contains("invite@example.test")
            );
        }

        phase a_line_typed_over_the_week_becomes_an_event;
        // A line typed over the week becomes an event where it says: "Lunch with Sam friday 1pm"
        // names its day and its hour, and the words left are its title.
        {
            let s = sheet();
            s.opened();
            let mut w = s.place_view("week");
            let entry = of_kind(w.tree(), "ui.TextInput")
                .into_iter()
                .find(|field| matches!(field.props.get("entry"), Some(Value::Bool(true))))
                .unwrap_or_else(|| panic!("the week ends in its entry: {}", dump(w.tree())))
                .key
                .clone();
            w.fire_with(
                &entry,
                "submit",
                Some(&Value::text("Lunch with Sam friday 1pm")),
            )
            .unwrap();
            w.poll_patches().unwrap();
            let events = s.projected(&s.root, "events");
            assert_eq!(events.len(), 1, "one event: {}", dump(w.tree()));
            assert_eq!(events[0]["fields"]["title"], "Lunch with Sam");
            assert_eq!(
                events[0]["fields"]["day"], "2026-11-27",
                "the friday it named"
            );
            assert_eq!(events[0]["fields"]["start"], "2026-11-27T13:00:00");
        }

        phase a_weekly_repeat_chooses_its_weekdays;
        // A weekly repeat comes on the days its person chooses: a day is added and taken off the
        // rule, and the last one is kept rather than left a repeat that never comes.
        {
            let s = sheet();
            let e = s.make("Standup", TODAY);
            s.call(
                "repeat_weekdays",
                &[
                    ("e", Value::resource(&e)),
                    (
                        "days",
                        Value::List(vec![Value::text("MO"), Value::text("WE")]),
                    ),
                ],
            );
            assert_eq!(
                s.text(&e, "recurrence"),
                "FREQ=WEEKLY;BYDAY=MO,WE",
                "the rule names the days it comes on"
            );
            assert_eq!(s.texts_of_set(&e, "recurrence_weekdays"), ["MO", "WE"]);
            s.call(
                "toggle_weekday",
                &[("e", Value::resource(&e)), ("day", Value::text("FR"))],
            );
            assert_eq!(
                s.texts_of_set(&e, "recurrence_weekdays"),
                ["FR", "MO", "WE"],
                "a day is added"
            );
            s.call(
                "toggle_weekday",
                &[("e", Value::resource(&e)), ("day", Value::text("MO"))],
            );
            assert_eq!(s.texts_of_set(&e, "recurrence_weekdays"), ["FR", "WE"]);
            s.call(
                "toggle_weekday",
                &[("e", Value::resource(&e)), ("day", Value::text("FR"))],
            );
            let why = s.refusal(
                "toggle_weekday",
                &[("e", Value::resource(&e)), ("day", Value::text("WE"))],
            );
            assert!(why.contains("at least one day"), "{why}");
            assert_eq!(s.texts_of_set(&e, "recurrence_weekdays"), ["WE"]);
        }

        phase a_calendar_colour_is_drawn_until_an_event_names_its_own;
        // An event is drawn in its calendar's colour until it names one of its own: two events of
        // the same emerald calendar are drawn alike, and an event's own colour wins over it.
        {
            let s = sheet();
            s.opened();
            let home = s.make_calendar("Home");
            let work = s.make_calendar("Work");
            s.call(
                "set_calendar_color",
                &[
                    ("k", Value::resource(&home)),
                    ("color", s.case("Color", "Color.emerald")),
                ],
            );
            let a = s.make("Standup", TODAY);
            s.call(
                "file_in",
                &[("e", Value::resource(&a)), ("k", Value::resource(&home))],
            );
            let b = s.call_ref(
                "new_event",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Review")),
                    ("at", Value::some(Value::text(format!("{TODAY}T11:00:00Z")))),
                ],
            );
            s.call(
                "file_in",
                &[("e", Value::resource(&b)), ("k", Value::resource(&work))],
            );
            s.call(
                "set_event_color",
                &[
                    ("e", Value::resource(&b)),
                    ("color", s.case("Color", "Color.emerald")),
                ],
            );
            assert_eq!(
                s.text(&home, "color"),
                "emerald",
                "the calendar keeps its colour"
            );
            let held = match s.read(&a, "calendar") {
                Value::Opt(Some(held)) => match *held {
                    Value::Ref(r) => (*r.target).clone(),
                    other => panic!("the calendar is a resource: {other:?}"),
                },
                other => panic!("the event names a calendar: {other:?}"),
            };
            assert_eq!(held, home, "the event stands in Home");
            let w = s.place_view("week");
            let items = of_kind(w.tree(), "ui.TimelineItem");
            let tone_of = |title: &str| {
                items
                    .iter()
                    .find(|n| text_prop_opt(n, "title").as_deref() == Some(title))
                    .and_then(|n| n.props.get("tone").cloned())
                    .unwrap_or_else(|| panic!("{title} is drawn: {}", dump(w.tree())))
            };
            assert_eq!(
                tone_of("Standup"),
                tone_of("Review"),
                "the calendar's emerald is drawn for the event that names none"
            );
            assert_eq!(s.text(&a, "color"), "", "the event itself names no colour");
        }

        phase the_agenda_shows_the_days_tasks_and_walks_a_week;
        // The agenda shows what each day holds: its events and the tasks due on it, under the
        // controls that walk it a week at a time.
        {
            let s = sheet();
            s.opened();
            s.make("Standup", TODAY);
            s.call_ref(
                "new_task",
                &[
                    ("c", Value::resource(&s.root)),
                    ("title", Value::text("Write notes")),
                    ("due", Value::some(Value::text(TODAY))),
                ],
            );
            let mut w = s.place_view("agenda");
            let shown = labels(w.tree());
            assert!(shown.iter().any(|t| t == "Standup"), "{shown:?}");
            assert!(shown.iter().any(|t| t == "Write notes"), "{shown:?}");
            let controls: Vec<String> = of_kind(w.tree(), "ui.Button")
                .iter()
                .filter_map(|b| {
                    text_prop_opt(b, "label")
                        .filter(|t| !t.is_empty())
                        .or_else(|| text_prop_opt(b, "accessible_name"))
                })
                .collect();
            for control in ["Previous week", "Today", "Next week"] {
                assert!(
                    controls.iter().any(|c| c == control),
                    "the agenda walks its days: {controls:?}"
                );
            }
            let next = of_kind(w.tree(), "ui.Button")
                .into_iter()
                .find(|b| text_prop_opt(b, "accessible_name").as_deref() == Some("Next week"))
                .expect("the button that walks on")
                .key
                .clone();
            w.fire(&next, "press").unwrap();
            w.poll_patches().unwrap();
            assert_eq!(
                s.text(&s.root, "anchor_at"),
                "2026-12-01T00:00:00Z",
                "a week on from the day it stood on"
            );
        }

        phase a_pristine_event_is_discarded_and_one_with_a_word_is_not;
        // An event nothing was written in yet is thrown away; one with a word in it stays.
        {
            let s = sheet();
            let blank = s.call_ref("new_event", &[("c", Value::resource(&s.root))]);
            s.call(
                "discard_draft",
                &[
                    ("c", Value::resource(&s.root)),
                    ("e", Value::resource(&blank)),
                ],
            );
            assert!(s.projected(&s.root, "events").is_empty(), "nothing is left");
            let written = s.make("Standup", TODAY);
            let why = s.refusal(
                "discard_draft",
                &[
                    ("c", Value::resource(&s.root)),
                    ("e", Value::resource(&written)),
                ],
            );
            assert!(why.contains("something in it"), "{why}");
            assert_eq!(s.projected(&s.root, "events").len(), 1);
        }

        phase a_guest_is_invited_from_the_address_book;
        // The address book's people are offered to invite: choosing one invites them by the name
        // and address the book keeps.
        {
            let s = sheet();
            let e = s.make("Planning", TODAY);
            // The book is another app's root: the calendar reads it through its own window.
            let book_app = built("calendar")
                .into_iter()
                .find(|used| used.meta().app == "so.tilo.contacts")
                .expect("the address book runs beside the calendar");
            let book = s.rt.create_root("Book").unwrap();
            let contact = seed_origin(
                &s.rt,
                "vcard",
                &book,
                &field_id(&book_app, "Book", "contacts"),
                &type_id(&book_app, "Contact"),
                vec![("name", Value::text("Sam Rivera"))],
            );
            seed_origin(
                &s.rt,
                "vcard",
                &contact,
                &field_id(&book_app, "Contact", "emails"),
                &type_id(&book_app, "Labeled"),
                vec![
                    ("label", Value::text("work")),
                    ("value", Value::text("sam@example.test")),
                ],
            );
            let mut w = s.window("event_page", vec![Value::resource(&e)]);
            let invite = of_kind(w.tree(), "ui.MenuItem")
                .into_iter()
                .find(|item| text_prop_opt(item, "label").as_deref() == Some("Sam Rivera"))
                .unwrap_or_else(|| panic!("the book's people are offered: {}", dump(w.tree())))
                .key
                .clone();
            w.fire(&invite, "choose").unwrap();
            w.poll_patches().unwrap();
            let guests = s.projected(&e, "attendees");
            assert_eq!(guests.len(), 1);
            assert_eq!(guests[0]["fields"]["name"], "Sam Rivera");
            assert_eq!(guests[0]["fields"]["email"], "sam@example.test");
        }

        phase a_calendar_whose_account_is_gone_says_so;
        // A calendar whose account the workspace no longer configures says so, rather than
        // standing silent as if its server had nothing to say.
        {
            let s = sheet();
            seed_origin(
                &s.rt,
                "caldav",
                &s.root,
                &field_id(&s.app, "Calendars", "calendars"),
                &type_id(&s.app, "Calendar"),
                vec![
                    ("key", Value::text("work")),
                    ("account", Value::text("acct")),
                    ("name", Value::text("Work")),
                    ("writable", Value::Bool(true)),
                ],
            );
            let w = s.root_window();
            let said: Vec<String> = of_kind(w.tree(), "ui.Banner")
                .iter()
                .filter_map(|banner| text_prop_opt(banner, "title"))
                .collect();
            assert!(
                said.iter().any(|title| title == "Work is no longer kept"),
                "{said:?}"
            );
        }
    }
}

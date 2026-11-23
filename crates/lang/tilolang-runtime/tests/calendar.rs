//! Calendar's capabilities, each through its own actions and views: an event made, moved and
//! repeated, its dates skipped, cancelled and moved apart, its guests and their invitation, its
//! reminder, its zone; the tasks and projects beside it; the week, day, month and agenda windows
//! over what repeats; and the iCalendar file in and out.

#[cfg(test)]
mod cases {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::rc::Rc;

    use crate::common::*;
    use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
    use tilolang_runtime::{
        AmStore, EnumValue, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
        ViewInstance,
    };

    const CALENDAR: &str = include_str!("../../../../apps/tl/calendar/app.tl");
    const MAIL: &str = include_str!("../../../../apps/tl/mail/app.tl");

    /// The moment every test reads as now: a Tuesday, late morning, in UTC.
    const NOW: &str = "2026-11-24T09:40:00Z";
    const TODAY: &str = "2026-11-24";

    /// The device's content store: the files a test keeps and the bytes a saved file holds.
    #[derive(Clone, Default)]
    struct Device {
        content: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    }

    impl Device {
        fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
            self.keep(name, media, bytes).expect("the bytes are kept")
        }

        fn bytes_of(&self, file: &FileValue) -> Vec<u8> {
            self.content
                .borrow()
                .get(&file.hash)
                .cloned()
                .unwrap_or_else(|| panic!("{} has no bytes", file.name))
        }
    }

    impl BlobHost for Device {
        fn read(&self, hash: &str) -> Option<Vec<u8>> {
            self.content.borrow().get(hash).cloned()
        }

        fn choose(&self, _request: &FileRequest) -> Result<Option<FileValue>, String> {
            Ok(None)
        }

        fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
            Err("this test writes no files out".to_owned())
        }

        fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
            let hash = blake3::hash(bytes).to_hex().to_string();
            self.content
                .borrow_mut()
                .insert(hash.clone(), bytes.to_vec());
            Ok(FileValue::new(
                name,
                media,
                i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                hash,
            ))
        }
    }

    struct Sheet {
        rt: Runtime,
        app: App,
        root: ResourceRef,
        device: Device,
        raised: Raised,
    }

    fn sheet() -> Sheet {
        let device = Device::default();
        let raised = Raised::default();
        let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
        rt.set_clock(|| NOW.to_owned());
        rt.set_zone(|| 0);
        rt.set_week_start(|| 0);
        rt.set_blobs(device.clone());
        rt.set_notices(raised.clone());
        rt.set_members(
            vec![tilolang_runtime::members::Member::person(
                "me",
                tilo_actions::policy::Role::Owner,
            )],
            None,
        );
        // Mail's own package runs beside Calendar's, so the invitation it hands over has a root
        // to reach.
        let mail = App::compile(MAIL, &[]);
        mail.load(&rt);
        let app = App::compile(CALENDAR, &[mail.meta().clone()]);
        app.load(&rt);
        let root = rt.create_root("Calendars").unwrap();
        rt.create_root("Mail").unwrap();
        Sheet {
            rt,
            app,
            root,
            device,
            raised,
        }
    }

    impl Sheet {
        fn call(&self, action: &str, given: &[(&str, Value)]) {
            let _ = self.invoke(action, given);
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
            assert_eq!(s.text(&e, "recurrence"), "FREQ=WEEKLY");
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
            // Mail holds no account here: the invitation is not prepared, and Mail says what to
            // add. What a prepared invitation is, and the calendar part it carries, Mail's own
            // test holds (apps/tilolang/tests/hosted/mail.rs).
            let out = s.invoke(
                "invite_by_mail",
                &[("e", Value::resource(&e)), ("g", Value::resource(&g))],
            );
            assert!(
                matches!(out, InvokeOutcome::Suggested(_)),
                "Mail says what to add: {out:?}"
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

    /// The day window draws what is all day and what is timed.
    #[test]
    fn the_day_draws_what_is_all_day_over_the_events_at_their_times() {
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

    /// The week draws an event on every day it runs through, not only the day it starts.
    #[test]
    fn the_week_draws_an_event_on_every_day_it_runs_through() {
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

    /// The agenda bands its rows by the event's own zone, one list carrying a repeating
    /// meeting between the one-off events.
    #[test]
    fn the_agenda_bands_the_days_in_the_event_zone() {
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
        let bands: Vec<String> = of_kind(w.tree(), "ui.ListItem")
            .iter()
            .filter_map(|item| text_prop_opt(item, "group"))
            .collect();
        // Tokyo is nine hours on, so the late evening of the 24th is the 25th there.
        assert!(
            bands.contains(&"2026-11-25".to_owned()),
            "the band is the event's own day: {bands:?}"
        );
    }

    /// The month is the timeline's month: a series stands in every week it meets.
    #[test]
    fn the_month_shows_a_series_in_every_week_it_meets() {
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

    /// The tasks window lists the list the person chose.
    #[test]
    fn the_tasks_window_lists_the_list_chosen() {
        let s = sheet();
        s.opened();
        let t = s.make_task("Write notes");
        s.call("due_task_today", &[("t", Value::resource(&t))]);
        let w = s.place_view("tasks");
        assert!(labels(w.tree()).iter().any(|x| x == "Write notes"));
    }

    /// A calendar the person kept out is left out of the week, and shown again it returns.
    #[test]
    fn a_calendar_kept_out_is_left_out_and_comes_back() {
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

    /// An exported calendar carries its events and tasks, an event's moment, and a finished
    /// task as completed.
    #[test]
    fn an_export_carries_events_tasks_dates_and_completion() {
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

    /// An imported file makes events and tasks: a zoned moment, a day, a repeat, a skipped
    /// date and a finished task.
    #[test]
    fn an_import_reads_dates_times_repeats_and_tasks() {
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

    /// The window's search narrows what a place lists.
    #[test]
    fn searching_the_window_narrows_what_a_place_lists() {
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

    /// A reminder's moment tells the people coming: the run raises one notice, addressed to the
    /// person, naming the event, and clears the reminder so it is told once.
    #[test]
    fn a_reminder_tells_the_people_coming() {
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

    /// A quick-add line that names only a day makes an event of the whole day; one that names
    /// a time is timed, and a day's own entry names the hour it starts at, so it is timed too.
    #[test]
    fn a_line_that_names_only_a_day_makes_an_all_day_event() {
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

    /// A floating event made by a member in Berlin reads nine for a member in Los Angeles: what
    /// is kept is the wall time, and each reader resolves it where they are.
    #[test]
    fn a_floating_event_made_in_berlin_reads_nine_in_los_angeles() {
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

    /// A title typed in one burst and saved at once makes one event carrying the whole title:
    /// the title is the event's bound field, and creating it is one action that reads the
    /// finished field — nothing runs per keystroke and nothing takes a partial value.
    #[test]
    fn a_title_typed_in_one_burst_makes_one_event_with_the_whole_title() {
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

    /// The sidebar's own calendar stands the window on the day it picks.
    #[test]
    fn the_sidebar_picks_a_day() {
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

    /// What the package declares is what it publishes, under its own id.
    #[test]
    fn the_surface_is_what_the_package_declares() {
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

    /// The window names the period it shows.
    #[test]
    fn the_window_names_the_period_it_shows() {
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
}

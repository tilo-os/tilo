//! Tasks in a hosted window: the palette and the form that make a task, the person's today, and
//! Today over Tasks' own record on one broker, its cards reading that record and what it changes
//! of it running at Tasks' door.

use super::*;

/// The node of the form's tree at `key`.
fn form_node<'a>(window: &'a Window, key: &str) -> Option<&'a patch::Node> {
    fn at<'a>(node: &'a patch::Node, key: &str) -> Option<&'a patch::Node> {
        if node.key == key {
            return Some(node);
        }
        node.children.iter().find_map(|child| at(child, key))
    }
    at(&window.form.as_ref()?.tree, key)
}

/// The choices a `choice` node offers, as the value a pick holds and the words it reads as.
fn choices_of(node: &patch::Node) -> Vec<(String, String)> {
    let Some(patch::Value::List(cases)) = node.props.get("cases") else {
        panic!("a choice with cases: {node:?}");
    };
    cases
        .iter()
        .map(|case| {
            let patch::Value::List(parts) = case else {
                panic!("a case: {case:?}");
            };
            match parts.as_slice() {
                [patch::Value::Text(value), patch::Value::Text(label), ..] => {
                    (value.clone(), label.clone())
                }
                other => panic!("a case's value and label: {other:?}"),
            }
        })
        .collect()
}

/// The palette runs "New task", which asks for nothing: a task needs no title to be made, so no
/// form stands for it and the press makes one there and then. Returns the task the press made.
fn a_task_added_from_the_palette(host: &mut TilolangHost, window: &mut Window) -> Value {
    let new_task = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "New task")
        .expect("the tasks publish the action that makes one")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: new_task,
        argument: None,
    }]);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "every argument it takes has a default, so nothing is asked: {:?}",
        window.form.as_ref().map(|form| form.title.clone())
    );
    let task = tasks_of(host.instances().next().expect("the instance runs"))
        .first()
        .cloned()
        .expect("the press made a task");
    let Value::Ref(made) = &task else {
        panic!("a task is a resource: {task:?}")
    };
    // The person has not said what it does yet, and the task is still named: a name every surface
    // that names it shows, so it is not blank in a window, a menu or a screen reader.
    let named = host
        .instances()
        .next()
        .expect("the instance runs")
        .instance()
        .host()
        .expect("the runtime")
        .runtime()
        .title(&made.target)
        .expect("the task is named");
    assert_eq!(
        named, "Task",
        "an untitled task is named by what it is until the person says what it does"
    );
    task
}

/// "Remind me…" asks when, the moments a person names offered to pick from, and one way into the
/// rest: the entry they type a moment into with the calendar at its end.
fn a_reminder_asks_when(host: &mut TilolangHost, window: &mut Window) {
    let remind = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.action_label == "Remind me…")
        .expect("the task offers its reminder")
        .address
        .clone();
    window.send(&[ToApp::Invoke {
        address: remind,
        argument: None,
    }]);
    settle(host, window);
    assert_eq!(
        window.form.as_ref().map(|form| form.title.as_str()),
        Some("Remind me…"),
        "the reminder's form asks when"
    );
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(""),
        "it starts with no moment, the task having no reminder"
    );
    assert!(
        !window.form.as_ref().expect("the form").ready,
        "a moment is required: nothing reads yet, so submit waits"
    );
    assert!(
        !form_node(window, "form~when~field")
            .expect("the moment's field")
            .props
            .contains_key("label"),
        "a form of one question says what it asks in its title"
    );
    let choices = choices_of(form_node(window, "form~when~quick").expect("the moments offered"));
    assert!(
        choices.iter().all(|(_, label)| label != "None"),
        "a required moment offers no way to leave it unanswered: {choices:?}"
    );
    assert_eq!(
        form_node(window, "form~when~quick")
            .expect("the moments offered")
            .props
            .get("case:as"),
        Some(&patch::Value::Text("time".to_owned())),
        "every moment says when it is, at the choice's trailing edge"
    );
    let picker = form_node(window, "form~when~pick").expect("one way into the rest");
    assert_eq!(
        picker
            .children
            .iter()
            .map(|child| (child.kind.as_str(), child.key.as_str()))
            .collect::<Vec<_>>(),
        [
            ("ui.TextInput", "form~when~text"),
            ("ui.DateField", "form~when"),
        ],
        "a moment is typed into the entry, or picked from the calendar at its end"
    );
    assert_eq!(
        form_node(window, "form~when")
            .expect("the calendar")
            .props
            .get("compact"),
        Some(&patch::Value::Bool(true)),
        "the calendar is a chip beside the entry, not a field of its own"
    );
    assert!(
        !window.form.as_ref().expect("the form").confirm,
        "the moments answer the form on one pick: its footer stands as Cancel alone"
    );
    assert_eq!(
        window.form.as_ref().expect("the form").answers,
        [form_node(window, "form~when~quick")
            .expect("its choices")
            .key
            .clone()],
        "picking a moment answers the form by itself"
    );
}

/// The person picks a moment from the ones the form offers, and that pick answers the form: the
/// reminder is set without a second press.
fn the_moment_picked_answers_the_reminder(
    host: &mut TilolangHost,
    window: &mut Window,
    task: &Value,
) {
    let choices = choices_of(form_node(window, "form~when~quick").expect("the moments offered"));
    let (moment, _) = choices
        .iter()
        .find(|(_, label)| label == "Tomorrow morning")
        .unwrap_or_else(|| panic!("tomorrow morning is offered: {choices:?}"));
    type_into(window, "form~when~quick", moment);
    settle(host, window);
    assert_eq!(
        form_text(window, "form~when").as_deref(),
        Some(moment.as_str()),
        "the field reads as the moment the pick names"
    );
    submit_form(window);
    settle(host, window);
    assert!(
        window.form.is_none(),
        "the form closes once the action runs: {:?}",
        window.form.as_ref().map(|form| form.error.clone())
    );
    assert_eq!(
        &reminder_of(host.instances().next().expect("the instance runs"), task),
        moment,
        "the task is reminded at the moment picked"
    );
}

/// The palette runs an action whose arguments it does not give: "New task" takes a title whose
/// default is empty, so the task is made on the press and reads as its type names an untitled one,
/// and "Remind me…" asks its moment in a form, the person picking it in one press
/// (forms-and-diagrams.md §1).
#[test]
fn the_palette_makes_a_task_and_asks_the_form_for_a_moment() {
    let (app, bundle) = shipped(&shipped_dir().join("tasks"));
    assert_eq!(app, TASKS);
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
    settle(&mut host, &mut window);
    let task = a_task_added_from_the_palette(&mut host, &mut window);
    a_reminder_asks_when(&mut host, &mut window);
    the_moment_picked_answers_the_reminder(&mut host, &mut window, &task);
}

/// The day the task `task` is due, as its record holds it.
fn due_of(hosted: &Hosted, task: &Value) -> Option<String> {
    let Value::Ref(task) = task else {
        panic!("a task is a resource: {task:?}")
    };
    let instance = hosted.instance();
    let host = instance.host().expect("the runtime");
    match host.runtime().read(&task.target, "due").expect("its day") {
        Value::Opt(Some(day)) => match *day {
            Value::Text(day) => Some(day),
            other => panic!("a day: {other:?}"),
        },
        Value::Opt(None) => None,
        other => panic!("a day: {other:?}"),
    }
}

/// A task typed into Today's entry is due the person's today, and so stands under the day their
/// window heads Today: nine in the evening where they are is already the next day in UTC, and
/// their day, not UTC's, is the one their list is of.
#[test]
fn a_task_typed_into_todays_entry_is_due_the_persons_today() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_task_typed_into_todays_entry_is_due_the_persons_today",
        || {
            // 11 November, nine in the evening in Los Angeles: the 12th in UTC.
            let mut zone = tilo_test_env::ScopedEnv::new();
            zone.set("TZ", "America/Los_Angeles");
            let _clock = tilo_clock::testing::hold_wall_everywhere(
                tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process(),
                tilo_clock::UNIX_EPOCH + tilo_clock::Duration::from_secs(1_794_459_600),
            );
            let (app, bundle) = shipped(&shipped_dir().join("tasks"));
            assert_eq!(app, TASKS);
            let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", &app, bundle);
            settle(&mut host, &mut window);
            let entry = node_with(
                window.root.as_ref().expect("the window shows"),
                "placeholder",
                "New task for today",
            )
            .expect("Today's entry stands at the list's foot")
            .clone();
            let event = entry
                .events
                .get_key_value("submit")
                .expect("the entry submits what is typed into it")
                .0
                .clone();
            window.send(&[ToApp::Fire {
                lane: Lane::Root,
                key: entry.key,
                event,
                value: Some(patch::Value::text("File the report")),
            }]);
            settle(&mut host, &mut window);
            let held = host.instances().next().expect("the instance runs");
            let tasks = tasks_of(held);
            let [task] = tasks.as_slice() else {
                panic!("the entry made one task: {tasks:?}");
            };
            let due = due_of(held, task);
            assert_eq!(
                due.as_deref(),
                Some("2026-11-11"),
                "a task typed in their evening is due on their today, not UTC's tomorrow"
            );
            // The day a row is grouped under is the day the window heads by its own clock
            // (`ui::clock::today()`): the day it is due. They are one day, so the row heads
            // Today and stands in Today's list.
            let headed_by =
                chrono::DateTime::<chrono::Local>::from(tilo_clock::to_std(tilo_clock::wall()))
                    .date_naive()
                    .to_string();
            assert_eq!(
                due.as_deref(),
                Some(headed_by.as_str()),
                "and it stands under the day their window heads Today"
            );
        },
    );
}

/// An instance of `bundle` for `attributes` in `host` on `route`, over an empty record: its
/// window, what its connection sent, what the broker delivers to it and the other routes it
/// joined.
fn joined_instance(
    host: &mut TilolangHost,
    attributes: Spec,
    bundle: Bundle,
    route: &RouteSpec,
) -> (Window, Sent, Inbox, Rc<RefCell<Vec<RouteSpec>>>) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (sent, inbox): (Sent, Inbox) = (Rc::default(), Rc::default());
    let routes: Rc<RefCell<Vec<RouteSpec>>> = Rc::default();
    let mut manager = CollaborationManager::with_peer_transport(Broker {
        route: route.clone(),
        sent: Rc::clone(&sent),
        inbox: Rc::clone(&inbox),
        joined: None,
        directory: Rc::default(),
        actions: Rc::default(),
        routes: Rc::clone(&routes),
    });
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: None,
        record_kind: None,
        handle: Some(attributes.person.clone()),
    }));
    let _ = manager.poll();
    let record = InstanceRecord::new(
        automerge::AutoCommit::new(),
        manager,
        route.clone(),
        &attributes.person,
    );
    let kept = host.kept(&attributes.app).unwrap();
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::with_record(
        attributes,
        record,
        Arc::new(bundle),
        &kept,
        Box::new(ChannelTransport::new(ours).unwrap()),
        &Rc::new(Phrases::new("en-US")),
        host.formats(),
        wake(),
    );
    host.add(hosted, Some(watched));
    (
        Window::over(ChannelTransport::new(theirs).unwrap()),
        sent,
        inbox,
        routes,
    )
}

/// Today and Tasks on one broker, each over its own record, as their windows run in a session.
struct Desk {
    host: TilolangHost,
    tasks: Window,
    today: Window,
    tasks_inbox: Inbox,
    today_sent: Sent,
    today_inbox: Inbox,
    tasks_route: RouteSpec,
    /// How many of Today's requests the broker took.
    taken: usize,
    /// The invocations the broker delivered.
    delivered: u64,
}

impl Desk {
    fn new() -> (Self, Rc<RefCell<Vec<RouteSpec>>>) {
        let workspace = uuid::Uuid::new_v4();
        let tasks_route = RouteSpec::app(TASKS, workspace);
        let today_route = RouteSpec::app("so.tilo.lang.today", workspace);
        let mut host = TilolangHost::new(None, wake());
        let (tasks, _, tasks_inbox, _) = joined_instance(
            &mut host,
            spec(1, "ada", &tasks_route),
            shipped(&shipped_dir().join("tasks")).1,
            &tasks_route,
        );
        // Notes, Tasks and Chat are installed as apps of their own; Discussion is not, and stays
        // part of the apps that use it.
        let installed = ["so.tilo.lang.notes", TASKS, "so.tilo.lang.chat"]
            .into_iter()
            .map(|app| (app.to_owned(), app.to_owned()))
            .collect();
        let (today, today_sent, today_inbox, joined) = joined_instance(
            &mut host,
            Spec {
                installed,
                ..spec(2, "ada", &today_route)
            },
            shipped(&shipped_dir().join("today")).1,
            &today_route,
        );
        (
            Self {
                host,
                tasks,
                today,
                tasks_inbox,
                today_sent,
                today_inbox,
                tasks_route,
                taken: 0,
                delivered: 0,
            },
            joined,
        )
    }

    fn tasks_instance(&self) -> &Hosted {
        self.host
            .instances()
            .find(|hosted| hosted.spec().app == TASKS)
            .unwrap()
    }

    /// Run the host until it settles, the broker carrying between the two: Tasks' record to
    /// Today, which joined it to read, and what Today asks of Tasks' door to Tasks.
    fn settle(&mut self) {
        let mut quiet_since = std::time::Instant::now();
        let mut handed = Vec::new();
        loop {
            let woken = WOKEN.swap(false, std::sync::atomic::Ordering::SeqCst);
            let mut changed = self.host.turn();
            self.tasks.take();
            self.today.take();
            let heads = self.tasks_instance().record().doc().clone().get_heads();
            if heads != handed {
                handed = heads;
                let whole = self.tasks_instance().record().doc().clone().save();
                self.today_inbox
                    .borrow_mut()
                    .push_back(CollabEvent::CommittedDocument {
                        route: self.tasks_route.clone(),
                        read: collaboration::CommittedDocument::for_test(whole, None),
                    });
                changed = true;
            }
            changed |= self.carry_asks();
            if changed || woken {
                quiet_since = std::time::Instant::now();
            } else if quiet_since.elapsed() > std::time::Duration::from_millis(300) {
                return;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    /// Deliver to Tasks what Today asked of its door since the last call, as the person's
    /// invocation. Whether there was any.
    fn carry_asks(&mut self) -> bool {
        let asks: Vec<tilo_actions::remote::InvokeRequest> = self
            .today_sent
            .borrow()
            .get(self.taken..)
            .unwrap_or_default()
            .iter()
            .filter_map(|request| match request {
                CollabRequest::RemoteAction {
                    message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
                } if invoke.target.provider.ends_with(&format!(":{TASKS}")) => Some(invoke.clone()),
                _ => None,
            })
            .collect();
        self.taken = self.today_sent.borrow().len();
        for invoke in &asks {
            self.deliver(&invoke.target.address, invoke.argument.to_slot());
        }
        !asks.is_empty()
    }

    /// Deliver the action `address` with `argument` to Tasks, as ada's invocation.
    fn deliver(&mut self, address: &tilo_actions::ActionAddress, argument: Option<String>) {
        self.delivered = self.delivered.saturating_add(1);
        self.tasks_inbox
            .borrow_mut()
            .push_back(CollabEvent::Invocation {
                id: self.delivered,
                invocation: tilo_actions::InvocationId::new(),
                resource: address.resource.clone(),
                action: address.action.clone(),
                argument,
                actor: "ada".to_owned(),
                context: None,
            });
    }

    /// The address of the action Tasks publishes under `label` on a resource `pick` takes.
    fn tasks_action(
        &self,
        label: &str,
        pick: impl Fn(&str) -> bool,
    ) -> tilo_actions::ActionAddress {
        self.tasks_instance()
            .published()
            .iter()
            .find(|action| action.action_label == label && pick(&action.address.resource))
            .unwrap_or_else(|| panic!("Tasks publishes {label}"))
            .address
            .clone()
    }

    fn today_shows(&self, text: &str) -> bool {
        format!("{:?}", self.today.root).contains(text)
    }

    /// The titles of the tasks Tasks' own record holds.
    fn titles_in_tasks(&self) -> Vec<String> {
        let host = self.tasks_instance().instance().host().unwrap();
        let root = host.root_of(TASKS).unwrap();
        let tasks = host.package(TASKS).unwrap().meta;
        let items = tasks
            .types
            .iter()
            .find(|ty| ty.name == "Tasks")
            .and_then(|ty| ty.fields.iter().find(|field| field.name == "items"))
            .unwrap()
            .id
            .clone();
        let title = tasks
            .types
            .iter()
            .find(|ty| ty.name == "Task")
            .and_then(|ty| ty.fields.iter().find(|field| field.name == "title"))
            .unwrap()
            .id
            .clone();
        let Value::List(items) = host.runtime().read(&root, &items).unwrap() else {
            panic!("the tasks are a list");
        };
        items
            .iter()
            .filter_map(|item| match item {
                Value::Ref(task) => match host.runtime().read(&task.target, &title) {
                    Ok(Value::Text(title)) => Some(title),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

/// Today's cards read Tasks' own record, joined read-only on Today's connection: a task due today
/// that Tasks makes shows in Today, one checked off in Tasks leaves it, and Today's Morning
/// cleanup clears the finished tasks at Tasks' door, in Tasks' record. Today's record holds none
/// of Tasks' resources.
#[test]
fn today_reads_tasks_from_its_record_and_clears_it_at_its_door() {
    let (mut desk, joined) = Desk::new();
    assert!(
        joined.borrow().contains(&desk.tasks_route),
        "Today joined Tasks' record: {:?}",
        joined.borrow()
    );
    assert!(
        !joined
            .borrow()
            .iter()
            .any(|route| route.app_id == "so.tilo.lang.discussion"),
        "Discussion is no app here: its records are the apps' that use it"
    );
    desk.settle();

    let new_task = desk.tasks_action("New task", |resource| !resource.contains('/'));
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), "Water the plants".into());
    // Due today, the person's: what Today's place holds.
    let today = chrono::DateTime::<chrono::Local>::from(tilo_clock::to_std(tilo_clock::wall()))
        .date_naive()
        .to_string();
    arguments.insert("due".to_owned(), today.into());
    desk.deliver(
        &new_task,
        tilo_actions::remote::InvokeArgument::Json(arguments).to_slot(),
    );
    desk.settle();
    assert_eq!(desk.titles_in_tasks(), ["Water the plants"]);
    assert!(
        desk.today_shows("Water the plants"),
        "the task Tasks made shows in Today: {:?}",
        desk.today.root
    );

    let complete = desk.tasks_action("Complete", |resource| resource.contains('/'));
    desk.deliver(&complete, None);
    desk.settle();
    assert!(
        !desk.today_shows("Water the plants"),
        "a task checked off in Tasks leaves Today's place"
    );

    let button = node_with(
        desk.today.root.as_ref().expect("Today is shown"),
        "label",
        "Morning cleanup",
    )
    .expect("Today offers its morning cleanup")
    .clone();
    let event = button
        .events
        .keys()
        .next()
        .expect("the button fires")
        .clone();
    desk.today.send(&[ToApp::Fire {
        lane: Lane::Root,
        key: button.key,
        event,
        value: None,
    }]);
    desk.settle();
    assert!(
        desk.titles_in_tasks().is_empty(),
        "the finished task went from Tasks' own record"
    );
    let filed = desk.today_sent.borrow().iter().any(|request| {
        matches!(request, CollabRequest::RemoteAction {
            message: tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
        } if invoke.target.provider.ends_with(":so.tilo.lang.notes")
            && format!("{:?}", invoke.argument).contains("Morning: 1 done"))
    });
    assert!(filed, "the morning's note is asked of Notes' door");
    let today = desk
        .host
        .instances()
        .find(|hosted| hosted.spec().app == "so.tilo.lang.today")
        .unwrap();
    assert!(
        today.instance().host().unwrap().root_of(TASKS).is_err(),
        "Today's record holds no root of Tasks"
    );
}

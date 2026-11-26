//! A task's session: the engine's turn on it runs Tasks' own actions over a device's store, as
//! a person's device does with the app open, and what the session is told and leaves behind.
mod session_tests {
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use collab_types::store::wire::{Command, Reply};
    use serde_json::{Map, Value, json};
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
        InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, ResourceReads,
    };
    use tilo_actions::{ActionAddress, ActionInvoker, ActionOutput};
    use tilo_app_host::agents::task_work::{Task, TaskWork, Work, made_by, task, with_place};
    use tilo_app_host::agents::triggered;
    use tilo_app_host::agents::{EngineActions, Outcome, WorkspaceActionProvider};
    use tilo_server_protocol::WorkspaceId;
    use tilo_workspace_service::agent_shortcuts::TASKS;
    use tilo_workspace_service::workspaces::Workspaces;
    use tilo_workspace_service::workspaces::fixtures::{
        TestGroupKeys, TestHost, hosting_device, test_host_holding,
    };

    use super::super::mount;

    /// The packages the device runs, each with its ids lock: Tasks and what it builds on.
    const PACKAGES: [(&str, &str); 4] = [
        (
            include_str!("../../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../../apps/tl/discussion/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../../apps/tl/notes/app.ids"),
        ),
        (
            concat!(
                include_str!("../../../../../apps/tl/contacts/app.tl"),
                "\n",
                include_str!("../../../../../apps/tl/contacts/identities.tl")
            ),
            include_str!("../../../../../apps/tl/contacts/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/tasks/app.tl"),
            include_str!("../../../../../apps/tl/tasks/app.ids"),
        ),
    ];

    /// The documents a device's store changed, each whole: what sync carries to the workspace.
    type Documents = Vec<(String, Vec<u8>)>;

    /// One invocation of a package's action on the device, and where its answer goes.
    struct Ask {
        invoker: ActionInvoker,
        resource: String,
        action: String,
        arguments: Map<String, Value>,
        answer: mpsc::Sender<(Result<ActionOutput, String>, Documents)>,
    }

    /// The person's device: its record opened over a store of its own, on a thread of its own
    /// (a host is held by one thread), each ask run as the record's published action.
    fn device(person: String, asks: &mpsc::Receiver<Ask>, opened: &mpsc::Sender<Documents>) {
        use tilolang_workspace::records::Port;
        use tilolang_workspace::records::{LocalPort, Opened, Packages, ResourceStore};

        let port = Rc::new(LocalPort::in_memory());
        let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn Port>);
        let viewer = tilo_actions::Actor::Member(tilo_actions::MemberId::person(person));
        let packages = Packages::compile(&PACKAGES).unwrap();
        let mut record = Opened::open(store, &packages, viewer, None).unwrap();
        // The roots of the device's apps (its Tasks, which a new task is added to, its
        // Notebook, which a task's note is made in): made as an app makes its root, by the
        // id each is published under.
        let roots: BTreeMap<String, String> = [TASKS, "so.tilo.notes"]
            .into_iter()
            .map(|app| {
                let root = record.host().root_of(app).unwrap();
                (
                    format!("tl:{app}"),
                    tilolang_workspace::publish::resource_id(app, &root.id_string()),
                )
            })
            .collect();
        let mut sent: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut moved = || {
            let mut documents = Vec::new();
            for doc in port.documents() {
                let Some(bytes) = port.save(&doc) else {
                    continue;
                };
                if sent.get(&doc) != Some(&bytes) {
                    sent.insert(doc.clone(), bytes.clone());
                    documents.push((doc, bytes));
                }
            }
            documents
        };
        opened.send(moved()).unwrap();
        while let Ok(ask) = asks.recv() {
            // An app's own actions are asked of the app (`tl:<app>`): its root, as the
            // device publishes it.
            let resource = roots.get(&ask.resource).unwrap_or(&ask.resource);
            let done = record
                .invoke_published(
                    &ActionAddress::new(resource.as_str(), ask.action.as_str()),
                    &ask.arguments,
                    &ask.invoker,
                    &tilo_actions::ActionContext::default(),
                )
                .map_err(|error| error.to_string());
            let _ = ask.answer.send((done, moved()));
        }
    }

    /// The device's broker as a turn's calls reach it: it decides each call (`refuses` names
    /// the actions it denies the turn, with its words) and keeps who asked what. A call it lets
    /// through to a package runs there, Tasks' own rules deciding it; any other (Files') is
    /// answered as done.
    struct Broker {
        workspaces: Workspaces,
        workspace: String,
        refuses: BTreeMap<&'static str, &'static str>,
        device: Mutex<mpsc::Sender<Ask>>,
        asked: Mutex<Vec<(Caller, String, String, Map<String, Value>)>>,
    }

    /// Run `action` on `resource` on the device as `invoker`, and carry what it wrote to the
    /// workspace's store: what the action answered, or the words it was refused with.
    fn on_device(
        device: &Mutex<mpsc::Sender<Ask>>,
        workspaces: &Workspaces,
        workspace: &str,
        invoker: ActionInvoker,
        resource: &str,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<ActionOutput, String> {
        let (answer, answered) = mpsc::channel();
        device
            .lock()
            .unwrap()
            .send(Ask {
                invoker,
                resource: resource.to_owned(),
                action: action.to_owned(),
                arguments,
                answer,
            })
            .unwrap();
        let (done, documents) = answered.recv().unwrap();
        tokio::runtime::Handle::current().block_on(workspaces.take_documents(workspace, documents));
        done
    }

    struct Provider(Arc<Broker>);

    impl ActionProvider for Provider {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }

        fn invoke(
            &self,
            caller: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            let broker = &self.0;
            let resource = request.target.address.resource.clone();
            let action = request.target.address.action.clone();
            let arguments = match &request.argument {
                InvokeArgument::Json(arguments) => arguments.clone(),
                _ => Map::new(),
            };
            broker.asked.lock().unwrap().push((
                caller.clone(),
                resource.clone(),
                action.clone(),
                arguments.clone(),
            ));
            if let Some(words) = broker.refuses.get(action.as_str()) {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: (*words).to_owned(),
                }));
            }
            let output = if resource.starts_with("tl:") {
                let ran = on_device(
                    &broker.device,
                    &broker.workspaces,
                    &broker.workspace,
                    ActionInvoker::admitted(caller.acting()),
                    &resource,
                    &action,
                    arguments,
                );
                match ran {
                    Ok(output) => output,
                    Err(reason) => {
                        return Pending::ready(Err(ActionCallError::Refused { reason }));
                    }
                }
            } else {
                ActionOutput::default()
            };
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed { output },
            }))
        }

        fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
            None
        }

        fn next_change(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<Pending<()>> {
            None
        }

        fn cancel(
            &self,
            _: &Caller,
            _: &tilo_actions::InvocationId,
        ) -> Pending<Result<(), ActionCallError>> {
            Pending::ready(Ok(()))
        }

        fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
            None
        }
    }

    struct Keys;

    impl tilo_app_host::agents::agent_keys::AgentKeys for Keys {
        fn grant(&self, _: WorkspaceId, _: &str, _: &str, _: [u8; 32]) -> Result<[u8; 32], String> {
            Ok([7; 32])
        }

        fn sign(&self, _: WorkspaceId, _: &[u8; 32], _: &mut InvokeRequest) -> Result<(), String> {
            Ok(())
        }

        fn made(
            &self,
            _key: &[u8; 32],
        ) -> Option<(tilo_user_identity::DeviceCert, ed25519_dalek::SigningKey)> {
            None
        }
    }

    /// A workspace of a test host, its person, and Tasks on the person's device.
    struct Host {
        daemon: TestHost,
        _dir: tempfile::TempDir,
        workspaces: Workspaces,
        workspace: String,
        /// The person, by their root in hex.
        person: String,
        /// The person's root.
        root: [u8; 32],
        device: Mutex<mpsc::Sender<Ask>>,
    }

    impl Host {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (key, cert) = hosting_device();
            let host =
                test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default()))
                    .await;
            let workspaces = host.workspaces().clone();
            mount(&workspaces);
            workspaces.set_member_device(Arc::new(
                tilo_workspace_service::workspaces::MemberDevice {
                    cert: cert.clone(),
                    signer: Arc::new(key.clone()),
                },
            ));
            let workspace = workspaces.create("Tasks").await.unwrap().workspace;
            let route = super::super::record_route(&workspace, TASKS);
            let _ = workspaces.own_route_keys(&route, true).await.unwrap();
            let person = hex::encode(cert.user_public_key);
            let (device, asks) = mpsc::channel();
            let (opened, documents) = mpsc::channel();
            std::thread::Builder::new()
                .name("tasks-device".into())
                .spawn({
                    let person = person.clone();
                    move || self::device(person, &asks, &opened)
                })
                .unwrap();
            let documents = tokio::task::spawn_blocking(move || documents.recv().unwrap())
                .await
                .unwrap();
            workspaces.take_documents(&workspace, documents).await;
            Self {
                daemon: host,
                _dir: dir,
                workspaces,
                workspace,
                person,
                root: cert.user_public_key,
                device: Mutex::new(device),
            }
        }

        /// Run `action` on `resource` as the person, on their device: the resources it made.
        ///
        /// # Panics
        /// Tasks refused it.
        async fn act(&self, resource: &str, action: &str, arguments: Value) -> ActionOutput {
            let Value::Object(arguments) = arguments else {
                panic!("arguments by name: {arguments}")
            };
            let device = self.device.lock().unwrap().clone();
            let (workspaces, workspace) = (self.workspaces.clone(), self.workspace.clone());
            let person = self.person.clone();
            let (resource, action) = (resource.to_owned(), action.to_owned());
            let named = action.clone();
            tokio::task::spawn_blocking(move || {
                on_device(
                    &Mutex::new(device),
                    &workspaces,
                    &workspace,
                    ActionInvoker::person(person),
                    &resource,
                    &action,
                    arguments,
                )
            })
            .await
            .unwrap()
            .unwrap_or_else(|refused| panic!("{named}: {refused}"))
        }

        /// A new task of Tasks titled `title`, with `more` of what a new task is given: its id.
        async fn new_task(&self, title: &str, more: Value) -> String {
            let mut arguments = json!({ "title": title });
            if let (Value::Object(all), Value::Object(more)) = (&mut arguments, more) {
                all.extend(more);
            }
            let made = self
                .act(&format!("tl:{TASKS}"), "new_task", arguments)
                .await;
            made_by(made.value.as_ref().unwrap(), TASKS)
                .unwrap()
                .to_owned()
        }

        /// The task `id` as the device holds it, its work read where Tasks keeps it.
        async fn task(&self, id: &str) -> Task {
            // Its number and title as the store's row holds them, its work as its content
            // keeps it.
            let held = task(&self.workspaces, &self.workspace, id)
                .await
                .unwrap_or_else(|| panic!("the task {id} is held"));
            let content = self.content(id).await;
            Task {
                id: id.to_owned(),
                number: held.number,
                title: held.title,
                work: Work::where_kept(&self.workspaces, &self.workspace, content.get("work"))
                    .await,
            }
        }

        /// The task `id` as the store keeps it.
        async fn content(&self, id: &str) -> Map<String, Value> {
            let command = Command::Content {
                kind: tilo_workspace_service::agent_shortcuts::TASK.to_owned(),
                id: id.to_owned(),
            };
            let reply = self
                .workspaces
                .read_store_as(&self.workspace, &self.person, &command)
                .await
                .unwrap();
            let Reply::Content(Some(Value::Object(content))) = reply else {
                panic!("the task {id} is not kept: {reply:?}")
            };
            content
        }

        fn broker(&self, refuses: &[(&'static str, &'static str)]) -> Arc<Broker> {
            Arc::new(Broker {
                workspaces: self.workspaces.clone(),
                workspace: self.workspace.clone(),
                refuses: refuses.iter().copied().collect(),
                device: Mutex::new(self.device.lock().unwrap().clone()),
                asked: Mutex::default(),
            })
        }

        /// The engine's actions through `broker`.
        fn service(&self, broker: &Arc<Broker>) -> WorkspaceActionProvider {
            let provider: Arc<dyn ActionProvider + Send + Sync> =
                Arc::new(Provider(Arc::clone(broker)));
            WorkspaceActionProvider::new(
                EngineActions::new(provider, Arc::new(Keys)),
                WorkspaceId::parse(&self.workspace).unwrap(),
            )
        }

        /// The turn's agent, `scout`, acting for the person, as its calls name it.
        fn caller(&self) -> Caller {
            Caller::authenticated(self.root, [7; 32])
                .in_workspace(self.workspace.clone())
                .delegated_to("scout")
        }

        async fn end(self) {
            self.daemon.shutdown().await;
        }
    }

    /// A workspace whose Tasks holds the task "Book the venue", its work as its person's
    /// `before` actions on it left it, and a turn of the agent on it through a broker that
    /// denies `refuses`.
    async fn worked(
        before: &[(&str, Value)],
        refuses: &[(&'static str, &'static str)],
    ) -> (Host, TaskWork, Arc<Broker>) {
        let host = Host::new().await;
        let id = host
            .new_task(
                "Book the venue",
                json!({"assignee": {"agent": {"id": "scout", "person": host.person}}}),
            )
            .await;
        for (action, arguments) in before {
            host.act(&triggered::published(TASKS, &id), action, arguments.clone())
                .await;
        }
        let broker = host.broker(refuses);
        let service = host.service(&broker);
        let task = host.task(&id).await;
        let work = TaskWork {
            service,
            caller: host.caller(),
            workspace: host.workspace.clone(),
            task,
        };
        (host, work, broker)
    }

    /// Which actions were asked, in order.
    fn actions(broker: &Broker) -> Vec<String> {
        broker
            .asked
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, action, _)| action.clone())
            .collect()
    }

    /// A session Tasks refuses (the task's budget is spent) does not begin: the turn ends with
    /// Tasks' words, which stop the task's work as spent, and no place is made for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_tasks_refuses_ends_with_its_words_and_makes_nothing() {
        const SPENT: &str = "I stopped: this task's budget is spent. Raise it to let me go on.";
        let (host, work, broker) = worked(
            &[
                ("set_budget", json!({"micros": 10})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 10})),
            ],
            &[("session_began", SPENT)],
        )
        .await;
        assert_eq!(
            work.task.work.money_left(),
            Some(0),
            "the budget is spent: {:?}",
            host.content(&work.task.id).await.get("work")
        );
        let refused = work.begin().await.unwrap_err();
        assert!(refused.contains(SPENT), "{refused}");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        let stop = broker.asked.lock().unwrap()[1].3.clone();
        assert_eq!(stop["why"], "budget");
        assert!(stop["words"].as_str().unwrap().contains(SPENT));
        host.end().await;
    }

    /// A session that begins is told that sessions came before, what is left of its budget and
    /// where its material is, made for it through Tasks, Notes and Files as the agent; one that
    /// runs out of the task's money stops the task's work as spent, and what it cost is counted
    /// where Tasks keeps it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_is_told_its_tasks_work_and_counted_when_it_ends() {
        let (host, work, broker) = worked(
            &[
                ("set_budget", json!({"micros": 1_000_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
                ("session_began", json!({})),
                ("session_ended", json!({"spent_micros": 125_000})),
            ],
            &[],
        )
        .await;
        assert_eq!(work.task.work.sessions, 2);
        assert_eq!(work.task.work.money_left(), Some(750_000));
        work.begin().await.unwrap();
        let place = work.place().await;
        let request = with_place("Book the venue", &work.task.work, &place);
        assert!(request.contains("not your first session"), "{request}");
        assert!(request.contains("$0.75 of $1.00 is left"), "{request}");
        assert!(request.contains("`tl:so.tilo.notes:"), "{request}");
        assert!(
            request.contains("`Tasks/1 Book the venue`"),
            "{request}\nthe task as the store keeps it: {:?}",
            host.content(&work.task.id).await
        );
        assert_eq!(
            actions(&broker),
            [
                "session_began",
                "open",
                "new_note",
                "set_place",
                "new-folder"
            ]
        );
        let ended = work
            .ended_as(
                Outcome::Stopped("the model ran out".into()),
                750_000,
                Duration::from_secs(3),
            )
            .await;
        assert!(
            matches!(&ended, Outcome::Stopped(words) if words.contains("budget of $1.00 is spent")),
            "{ended:?}"
        );
        let asked = broker.asked.lock().unwrap().clone();
        let (_, counted, stop) = (&asked[5], &asked[5].3, &asked[6].3);
        assert_eq!(asked[5].2, "session_ended");
        assert_eq!(counted["spent_micros"], 750_000);
        assert_eq!(counted["worked_ms"], 3_000);
        assert_eq!(asked[6].2, "stop");
        assert_eq!(stop["why"], "budget");
        let after = host.task(&work.task.id).await;
        assert_eq!(after.work.spent_micros, 1_000_000);
        assert_eq!(after.work.money_left(), Some(0));
        assert!(!after.work.interrupted);
        // Every write is the agent's own, on the task it works or the place made for it.
        for (caller, asked_on, action, _) in &asked {
            assert_eq!(caller, &work.caller, "{action}");
            if !matches!(action.as_str(), "open" | "new_note" | "new-folder") {
                assert_eq!(asked_on, &work.task.address(), "{action}");
            }
        }
        host.end().await;
    }

    /// An agent the broker lets write nothing on the task begins no session on it, and nothing
    /// is asked as anyone else: the turn ends with the broker's refusal.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_without_a_grant_on_the_task_writes_nothing_to_it() {
        const DENIED: &str = "the agent may not change this task";
        let (host, work, broker) = worked(
            &[],
            &[
                ("session_began", DENIED),
                ("stop", DENIED),
                ("set_place", DENIED),
                ("new_note", DENIED),
            ],
        )
        .await;
        let refused = work.begin().await.unwrap_err();
        assert!(refused.contains(DENIED), "{refused}");
        assert_eq!(actions(&broker), ["session_began", "stop"]);
        assert!(
            broker
                .asked
                .lock()
                .unwrap()
                .iter()
                .all(|(caller, ..)| caller == &work.caller)
        );
        host.end().await;
    }
}

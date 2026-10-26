// The legacy invoke doors let the caller name its target itself, so a
// connection may only reach what is its own. Another application's action is
// refused there and has to go through `remoteAction`, where the application
// door applies the manifest ceiling, the workspace's grant to that application
// and the person's consent.
//
// The door answers the same refusal either way, so each case contrasts a
// target that is refused with a live control the identical call does admit.
mod legacy_invoke_door {
    use super::signed_worker::{Connection, OriginalRoom, directory_state, presentation, rpc_pair};
    use super::*;
    use crate::test_support::KindSet;
    use capnp::capability::Promise;
    use capnp_rpc::pry;
    use tilo_actions::{ActionContract, ActionKind, ExecutionMode, OperationKind, ResultSpec};

    const CONTROL: &str = "so.tilo.ebook-reader";
    const FOREIGN: &str = "so.tilo.files";
    /// The peer the target application's own connection holds.
    const PEER: u64 = 903;

    /// How long one step of the case may take before the case names it.
    const STEP: std::time::Duration = std::time::Duration::from_secs(5);

    /// A real signed room with a private host worker publishing an eager
    /// action for its own application, so both doors run their whole path:
    /// admission, directory resolution and delivery over the socket.
    struct Environment {
        room: ProvisionedRoom,
        directories: Rc<LocalActionDirectoryRegistry>,
    }

    struct ProvisionedRoom {
        environment: OriginalRoom,
        _identity_env: tilo_test_env::ScopedEnv,
    }

    impl std::ops::Deref for ProvisionedRoom {
        type Target = OriginalRoom;
        fn deref(&self) -> &OriginalRoom {
            &self.environment
        }
    }

    impl std::ops::DerefMut for ProvisionedRoom {
        fn deref_mut(&mut self) -> &mut OriginalRoom {
            &mut self.environment
        }
    }

    impl Environment {
        fn new() -> Self {
            // The room's own device is an ordinary person device: an
            // application joins by its certificate, which a host may not do.
            let room =
                OriginalRoom::new(tilo_user_identity::DeviceCapabilities::member(), None, None);
            let directories = room.directories.clone();
            Self {
                room: ProvisionedRoom {
                    environment: room,
                    _identity_env: tilo_test_env::ScopedEnv::new(),
                },
                directories,
            }
        }

        /// Join another connection in the same room as `app`, optionally as
        /// the agent it acts for.
        async fn join_as(&self, peer: u64, app: &str, agent: Option<&str>) -> Connection {
            let (server, client) = std::os::unix::net::UnixStream::pair().unwrap();
            let (rpc, server, client) = rpc_pair(
                self.room.state.clone(),
                peer,
                server,
                client,
                None,
                self.directories.clone(),
            );
            let route = CrdtRoute::record(app, &self.room.route.cluster_id, "document-1");
            let handle = rpc
                .join_channel_as_kind(
                    &route.channel_key(),
                    &presentation(&route.cluster_id, &self.room.cert, &self.room.key),
                    agent.unwrap_or(""),
                    if agent.is_some() { "agent" } else { "person" },
                    "",
                )
                .await
                .unwrap();
            Connection {
                handle,
                server,
                client,
            }
        }
    }

    /// The private host worker publishing a live action: peer `903`, the
    /// room's own word processor, and the directory that resolves its
    /// surface.
    struct Target {
        worker: Connection,
        stream: runtime_collab::InvocationStream,
    }

    impl Target {
        async fn new(env: &Environment) -> Self {
            let (worker, _) = env.room.join_worker(903).await;
            let stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            worker
                .handle
                .publish_actions(&env.room.route.app_id, &[live_descriptor()])
                .await
                .unwrap();
            worker
                .handle
                .publish_action_directory(
                    &env.room.route.app_id,
                    capnp_rpc::new_client(WorkerDirectory {
                        state: directory_state("original-r1"),
                    }),
                )
                .await
                .unwrap();
            Self { worker, stream }
        }

        /// Nothing reached the target since the last check.
        async fn assert_nothing_reached(&self) {
            let next =
                tokio::time::timeout(std::time::Duration::from_millis(200), self.stream.read())
                    .await;
            assert!(
                next.is_err(),
                "a refused invocation must not reach the target"
            );
        }

        /// Exactly one invocation arrived since the last check: the admitted
        /// one, and nothing from a refused call.
        async fn assert_delivered_one(&self) {
            tokio::time::timeout(STEP, self.stream.read())
                .await
                .expect("the admitted invocation reaches the target")
                .expect("the admitted invocation reaches the target");
            let next =
                tokio::time::timeout(std::time::Duration::from_millis(200), self.stream.read())
                    .await;
            assert!(
                next.is_err(),
                "a refused invocation must not reach the target"
            );
        }

        /// Ask the target, on the application's own connection, for its own
        /// action.
        async fn self_invoke(&self, door: &str) -> Result<u64, String> {
            if door == "eager" {
                let (id, _) = self
                    .worker
                    .handle
                    .invoke_published_action(
                        PEER,
                        "workspace:atlas",
                        "open",
                        Some("argument"),
                        "{}",
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(id)
            } else {
                let (id, _) = self
                    .worker
                    .handle
                    .invoke_resolved_published_action(
                        PEER,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                        Some("argument"),
                        "{}",
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(id)
            }
        }

        /// Ask `from` to invoke target `peer` through one of the two doors.
        async fn invoke(&self, from: &Connection, door: &str, peer: u64) -> Result<u64, String> {
            let (id, _) = if door == "eager" {
                from.handle
                    .invoke_published_action(
                        peer,
                        "workspace:atlas",
                        "open",
                        Some("argument"),
                        "{}",
                    )
                    .await
                    .map_err(|error| error.to_string())?
            } else {
                from.handle
                    .invoke_resolved_published_action(
                        peer,
                        "original-r1",
                        "workspace:atlas",
                        "open",
                        Some("argument"),
                        "{}",
                    )
                    .await
                    .map_err(|error| error.to_string())?
            };
            Ok(id)
        }

        async fn stop(mut self) {
            self.worker.server.abort();
            self.worker.client.abort();
            let _ = (&mut self.worker.server).await;
            let _ = (&mut self.worker.client).await;
        }
    }

    /// A connection that publishes `CONTROL`'s live action and reads the
    /// invocations the doors hand to it: the caller's own application.
    struct Own {
        connection: Connection,
        stream: runtime_collab::InvocationStream,
    }

    impl Own {
        async fn join(env: &Environment) -> Self {
            let connection = env.join_as(921, CONTROL, None).await;
            let stream = connection
                .handle
                .subscribe_stable_invocations()
                .await
                .unwrap();
            connection
                .handle
                .publish_actions(CONTROL, &[live_descriptor()])
                .await
                .unwrap();
            Self { connection, stream }
        }

        /// Exactly one invocation arrived since the last check: the admitted
        /// one, and nothing from a refused call.
        async fn assert_delivered_one(&self) {
            let invocation = tokio::time::timeout(STEP, self.stream.read())
                .await
                .expect("the admitted invocation reaches the caller's own application")
                .unwrap();
            assert_eq!(invocation.resource, "workspace:atlas");
            let next =
                tokio::time::timeout(std::time::Duration::from_millis(200), self.stream.read())
                    .await;
            assert!(
                next.is_err(),
                "a refused invocation must not reach an application"
            );
        }
    }

    /// An eager, non-cancellable action: the doors admit it without a
    /// confirmation token, so only their own rules decide a case.
    fn live_descriptor() -> PublishedAction {
        PublishedAction {
            semantic_identity: None,
            resource: "workspace:atlas".into(),
            action: "open".into(),
            resource_kind: "workspace".into(),
            resource_label: "Workspace".into(),
            action_label: "Open".into(),
            description: None,
            operation_kind: OperationKind::Effect,
            params: Vec::new(),
            contract: ActionContract {
                kind: ActionKind::Read,
                execution: ExecutionMode::Immediate,
                result: ResultSpec::None,
                confirmation: tilo_actions::ConfirmationPolicy::None,
                ..ActionContract::default()
            },
            input: PublishedInput::None,
            view: None,
        }
    }

    /// The host worker's found-mode directory at `original-r1`.
    struct WorkerDirectory {
        state: Rc<RefCell<TestActionDirectory>>,
    }

    impl action_directory::Server for WorkerDirectory {
        fn resolve(
            self: Rc<Self>,
            params: action_directory::ResolveParams,
            mut results: action_directory::ResolveResults,
        ) -> Promise<(), capnp::Error> {
            let request = pry!(params.get());
            let revision = pry!(request.get_revision());
            let revision = pry!(revision.to_str());
            let mut state = self.state.borrow_mut();
            state.resolve_count = state.resolve_count.saturating_add(1);
            let mut out = results.get().init_resolution();
            if revision != state.revision {
                out.set_stale_revision(());
                return Promise::ok(());
            }
            pry!(runtime_collab::write_action_directory_row(
                out.reborrow().init_found(),
                &state.row(),
            ));
            out.set_resolved_revision(state.revision.as_str());
            Promise::ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn eager_and_resolved_doors_refuse_another_applications_actions() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let env = Environment::new();
                let target = Target::new(&env).await;
                let own = Own::join(&env).await;
                // The person may reach the caller's own application too, so a
                // refusal is the door's own rule rather than the workspace's
                // grant.
                let person = principal_id_for_root(env.room.cert.user_public_key);
                {
                    let mut tree = env.room.membership.write();
                    let workspace = &mut tree.workspaces[0];
                    for app in [CONTROL, FOREIGN] {
                        workspace.allow_app(
                            tilo_action_policy::Subject::Person(person.clone()),
                            app,
                            KindSet::ALL,
                        );
                    }
                    workspace.admit_agent("scribe", KindSet::ALL);
                    workspace.allow_app(
                        tilo_action_policy::Subject::agent("scribe"),
                        FOREIGN,
                        KindSet::ALL,
                    );
                }
                let agent = env.join_as(922, FOREIGN, Some("scribe")).await;

                // The caller's own application, asked for its own action: the
                // eager door admits it and hands the invocation there.
                tokio::time::timeout(STEP, target.invoke(&own.connection, "eager", 921))
                    .await
                    .expect("the caller's own application is admitted")
                    .expect("the caller's own application is admitted");
                own.assert_delivered_one().await;

                // The resolved door dispatches to a private host worker on the
                // invoking connection, so the application that resolves its
                // own revision is the one running there.
                tokio::time::timeout(STEP, target.self_invoke("resolved"))
                    .await
                    .expect("the application resolves its own revision")
                    .expect("the application resolves its own revision");
                target.assert_delivered_one().await;

                // Another application's action, named by a connection that is
                // not it and is not acting as anyone: refused through either
                // door, and nothing reaches the target.
                for door in ["eager", "resolved"] {
                    let refusal = target.invoke(&own.connection, door, PEER).await;
                    let refusal = refusal.expect_err(&format!("{door}: an application"));
                    assert!(
                        refusal.contains("not allowed"),
                        "{door}: an application may only reach its own: {refusal}"
                    );
                }
                target.assert_nothing_reached().await;

                // An agent channel is its own principal, as it is for remote
                // actions, so the doors keep working for the agents they are
                // for.
                tokio::time::timeout(STEP, target.invoke(&agent, "eager", PEER))
                    .await
                    .expect("an agent reaches the target the room grants it")
                    .expect("an agent reaches the target the room grants it");
                target.assert_delivered_one().await;

                target.stop().await;
                drop(agent);
                drop(own);
            })
            .await;
    }
}

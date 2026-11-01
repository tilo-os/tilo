/// `invocation`'s latest state as `handle`'s caller sees it: `None` for an
/// invocation that is not theirs.
async fn observe(
    handle: &runtime_collab::ChannelHandle,
    invocation: tilo_actions::InvocationId,
) -> Result<Option<tilo_actions::remote::InvocationState>, String> {
    use tilo_actions::remote::RemoteActionMessage;
    match super::signed_worker::ask(handle, RemoteActionMessage::observe(invocation)).await {
        Ok(RemoteActionMessage::Update { state, .. }) => Ok(state),
        Ok(other) => Err(format!("an observe answered with {other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

fn next_wire_id(rig: &Rig) -> u64 {
    rig.state
        .journal
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .next_wire_id()
}

fn effect_count(env: &Environment) -> Option<String> {
    std::fs::read_to_string(env.root.path().join("effect-count")).ok()
}

/// The person's agent `agent`, working in the files application on
/// `cert`'s device.
async fn device_caller(
    rig: &Rig,
    env: &Environment,
    peer: u64,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    agent: &str,
) -> Connection {
    let (server, socket) = std::os::unix::net::UnixStream::pair().unwrap();
    let (rpc, server, client) = rpc_pair(
        rig.state.clone(),
        peer,
        server,
        socket,
        None,
        rig.directories.clone(),
    );
    let route = CrdtRoute::app("so.tilo.files", env.route.cluster_id.clone());
    let handle = rpc
        .join_channel_as_kind(
            &route.channel_key(),
            &presentation(&route.cluster_id, cert, key),
            agent,
            "agent",
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

/// Admit the person's agent `scribe` to the rig's application.
fn admit_scribe(env: &Environment) {
    use crate::test_support::{Grants as _, KindSet};
    let mut tree = env.membership.write();
    let workspace = tree.workspaces.first_mut().unwrap();
    workspace.admit_agent("scribe", KindSet::ALL);
    workspace.allow_app(
        tilo_action_policy::Subject::agent("scribe"),
        &env.route.app_id,
        KindSet::ALL,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_callers_invocation_id_admits_once_and_keeps_its_retained_outcome() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use tilo_actions::remote::{InvokeArgument, ReceiptStatus};
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let handle = &rig.worker.handle;
            let invocation = id(0xabcd_ef00_0000_0000_0000_0000_0000_0001);
            let mut request = durable_request(invocation);
            request.argument = InvokeArgument::Text("original".into());
            let (first, duplicate) = tokio::join!(
                invoke_with(handle, request.clone()),
                invoke_with(handle, request.clone())
            );
            let first = first.expect("the caller's own id reaches durable dispatch");
            assert_eq!(first.invocation, invocation);
            assert!(matches!(first.status, ReceiptStatus::Accepted));
            // A duplicate racing the admission is told to ask again, and is
            // then answered with the same invocation.
            match duplicate {
                Ok(duplicate) => assert_eq!(duplicate, first),
                Err(error) => assert!(error.contains("still being admitted"), "{error}"),
            }
            let wire = rig.delivered_effect(&env, invocation).await;
            assert_eq!(invoke_with(handle, request.clone()).await.unwrap(), first);
            super::signed_worker::cancel(handle, invocation)
                .await
                .unwrap();
            super::signed_worker::cancel(handle, invocation)
                .await
                .expect("a repeated cancel is the same request");
            assert!(handle.invocation_cancellation(wire).await.unwrap());
            let completed = tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            handle
                .update_invocation(wire, &serde_json::to_string(&completed).unwrap())
                .await
                .unwrap();
            let observed = observe(handle, invocation).await.unwrap().unwrap();
            assert_eq!(observed.update, completed);

            // The retained id answers before the live target is looked up.
            rig.directories.remove(903);
            let retained = invoke_with(handle, request.clone()).await.unwrap();
            assert!(
                matches!(retained.status, ReceiptStatus::Completed { .. }),
                "{retained:?}"
            );
            for change in ["argument", "none", "revision", "resource", "action"] {
                let mut changed = request.clone();
                match change {
                    "argument" => changed.argument = InvokeArgument::Text("changed".into()),
                    "none" => changed.argument = InvokeArgument::None,
                    "revision" => changed.revision = Some("changed-r".into()),
                    "resource" => {
                        changed.target.address =
                            tilo_actions::ActionAddress::new("changed-resource", "open");
                    }
                    "action" => {
                        changed.target.address =
                            tilo_actions::ActionAddress::new("workspace:atlas", "changed-action");
                    }
                    _ => unreachable!(),
                }
                assert!(
                    invoke_with(handle, changed).await.is_err(),
                    "a retained id cannot carry a changed {change}"
                );
            }
            assert_eq!(effect_count(&env).as_deref(), Some("1"));
            assert_eq!(next_wire_id(&rig), wire + 1, "the duplicate admits nothing");
            no_delivery(&rig).await;
            env.control.set_wall_ns(10_000_000_000);
            env.control.advance_monotonic(Duration::from_secs(80));
            assert!(
                !matches!(observe(handle, invocation).await, Ok(Some(_))),
                "a terminal outcome still needs the caller's current permission"
            );
            env.origin.invalidate();
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn durable_admission_survives_restart_and_journal_barrier_failures() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::collab_app_broker::invocation_journal::BarrierFailure;
            use tilo_actions::remote::Confirmation;
            for barrier in [
                None,
                Some(BarrierFailure::File),
                Some(BarrierFailure::Directory),
                Some(BarrierFailure::AfterAdmission),
                Some(BarrierFailure::Terminal),
            ] {
                let env = Environment::new();
                let rig = Rig::new(&env).await;
                let invocation = id(0xa000 + barrier.map_or(0, |b| b as u128 + 1));
                let label = barrier.map(|failure| failure as u8);
                let fail_at = |rig: &Rig, failure| {
                    rig.state
                        .journal
                        .lock()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .fail_at = Some(failure);
                };
                if let Some(failure) = barrier.filter(|failure| *failure != BarrierFailure::Terminal)
                {
                    fail_at(&rig, failure);
                }
                let admission = invoke(&rig.worker.handle, invocation).await;
                let ran = barrier.is_none() || barrier == Some(BarrierFailure::Terminal);
                if ran {
                    admission.unwrap();
                    let wire = rig.delivered_effect(&env, invocation).await;
                    if barrier == Some(BarrierFailure::Terminal) {
                        fail_at(&rig, BarrierFailure::File);
                    }
                    let update = tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    };
                    let result = rig
                        .worker
                        .handle
                        .update_invocation(wire, &serde_json::to_string(&update).unwrap())
                        .await;
                    assert_eq!(result.is_ok(), barrier.is_none(), "{label:?}");
                } else {
                    assert!(admission.is_err(), "{label:?}");
                    no_delivery(&rig).await;
                    assert!(effect_count(&env).is_none());
                }
                let restarted = rig.restarted(&env).await;
                let handle = &restarted.worker.handle;
                if barrier == Some(BarrierFailure::File) {
                    restarted.directories.remove(903);
                    assert!(
                        invoke(handle, invocation).await.is_err(),
                        "an absent id reaches no target that is gone"
                    );
                } else {
                    // The journal keeps the outcome across the restart.
                    let key = crate::collab_app_broker::invocation_journal::InvocationKey {
                        room: env.route.cluster_id.clone(),
                        host: restarted.state.invocation_host().unwrap(),
                        owner: crate::collab_app_broker::invocation_journal::InvocationOwner::AuthenticatedLocal {
                            root: env.cert.user_public_key,
                            device: env.cert.device_public_key,
                            agent: None,
                        },
                        invocation,
                    };
                    let row = retained(&restarted, &key);
                    assert_eq!(row.uncertain, barrier.is_some(), "{label:?}");
                    if barrier.is_some() {
                        assert!(
                            matches!(&row.update,
                                tilo_actions::InvocationUpdate::Failed { error, .. }
                                    if error.code == "action_outcome_unknown" && !error.retryable),
                            "{label:?}: {:?}",
                            row.update
                        );
                    } else {
                        assert!(matches!(
                            row.update,
                            tilo_actions::InvocationUpdate::Succeeded { .. }
                        ));
                    }
                    // Asking again with the retained id answers with what
                    // the journal keeps: nothing runs again, nothing is
                    // admitted anew, and the caller recovers the answer it
                    // lost across the restart.
                    let retained_receipt = tilo_actions::remote::InvocationState::new(
                        invocation,
                        row.update.clone(),
                    )
                    .receipt();
                    assert_eq!(
                        invoke(handle, invocation).await.unwrap(),
                        retained_receipt,
                        "{label:?}"
                    );
                    assert_eq!(
                        observe(handle, invocation).await.unwrap().unwrap().update,
                        row.update,
                        "{label:?}"
                    );
                    // A confirmation is evidence for a new admission, not a
                    // new intent for a retained one.
                    let mut confirmed = durable_request(invocation);
                    confirmed.confirmation =
                        Some(Confirmation::Phrase("changed retry evidence".into()));
                    assert_eq!(
                        invoke_with(handle, confirmed).await.unwrap(),
                        retained_receipt,
                        "{label:?}"
                    );
                }
                no_delivery(&restarted).await;
                assert_eq!(effect_count(&env), ran.then(|| "1".into()), "{label:?}");
                restarted.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_agents_durable_invocation_is_its_own_and_follows_current_policy() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::test_support::{Grants as _, KindSet};
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let agent_grant =
                env.membership.write().workspaces[0].admit_agent("scribe", KindSet::READ);
            env.membership.write().workspaces[0].allow_app(
                tilo_action_policy::Subject::agent("scribe"),
                &env.route.app_id,
                KindSet::ALL,
            );
            let person = rig.caller(&env, 904, None).await;
            let agent = rig
                .app_caller(&env, 905, "so.tilo.files", Some("scribe"))
                .await;
            let invocation = id(0xb001);
            let admitted = invoke(&agent.handle, invocation).await.unwrap();
            assert!(observe(&person.handle, invocation).await.unwrap().is_none());
            assert!(
                super::signed_worker::cancel(&person.handle, invocation)
                    .await
                    .is_err(),
                "the person cannot cancel their agent's invocation"
            );
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, "agent:scribe");
            let request = ui::invocation_scope::PublishedInvocationRequest {
                wire_id: delivered.id,
                invocation,
                address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
                argument: delivered.argument,
                actor: delivered.actor,
                participant: None,
            };
            let lease = (rig.authorize)(&request).unwrap();
            lease.check().unwrap();
            assert!(observe(&agent.handle, invocation).await.unwrap().is_some());
            assert!(
                rig.state.invocation_statuses("agent:scribe").is_empty(),
                "an actor name alone discloses no authenticated invocation"
            );
            env.membership.write().workspaces[0].revoke_grant(&agent_grant);
            assert!(!matches!(
                observe(&agent.handle, invocation).await,
                Ok(Some(_))
            ));
            assert!(lease.check().is_err());
            assert!(invoke(&agent.handle, invocation).await.is_err());
            assert!(matches!(
                admitted.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ));
            no_delivery(&rig).await;
            drop(person);
            drop(agent);
            rig.stop().await;
        })
        .await;
}

/// An application connection reaches another application's action only
/// with the workspace's leave; an agent working in that application is
/// its own principal and reaches it under its own grants.
#[tokio::test(flavor = "current_thread")]
async fn an_application_reaches_another_applications_action_only_as_a_granted_principal() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::test_support::{Grants as _, KindSet};
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let agent_grant =
                env.membership.write().workspaces[0].admit_agent("scribe", KindSet::ALL);
            env.membership.write().workspaces[0].allow_app(
                tilo_action_policy::Subject::agent("scribe"),
                &env.route.app_id,
                KindSet::ALL,
            );
            let application = rig.app_caller(&env, 904, "so.tilo.files", None).await;
            let invocation = id(0xb101);
            let refused = invoke(&application.handle, invocation).await.unwrap_err();
            assert!(refused.contains("NotPermitted"), "{refused}");
            no_delivery(&rig).await;
            assert!(
                observe(&application.handle, invocation)
                    .await
                    .unwrap()
                    .is_none()
            );
            let agent = rig
                .app_caller(&env, 905, "so.tilo.files", Some("scribe"))
                .await;
            invoke(&agent.handle, invocation).await.unwrap();
            rig.delivered_effect(&env, invocation).await;
            env.membership.write().workspaces[0].revoke_grant(&agent_grant);
            drop(application);
            drop(agent);
            rig.stop().await;
        })
        .await;
}

/// An edit a durable, certified invocation's effect submits is admitted
/// as that invocation's caller: under their grants, and refused beyond
/// them; a certified caller whose certificate no longer verifies
/// continues nothing.
#[tokio::test(flavor = "current_thread")]
async fn a_certified_invocations_edits_are_admitted_for_its_caller() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let room = env.route.cluster_id.clone();
            {
                use crate::test_support::{Grants as _, KindSet, Subject, TreePath};
                let workspace = &mut env.membership.write().workspaces[0];
                workspace.admit_agent("scribe", KindSet::READ);
                workspace.allow_app(Subject::agent("scribe"), &env.route.app_id, KindSet::ALL);
                workspace.deny(
                    Subject::agent("scribe"),
                    &TreePath::root()
                        .app(env.route.app_id.clone())
                        .any(tilo_action_policy::Level::Kind)
                        .resource("document-1:body")
                        .action("erase"),
                    KindSet::ALL,
                );
            }
            let agent = rig
                .app_caller(&env, 905, "so.tilo.files", Some("scribe"))
                .await;
            let invocation = id(0xc001);
            invoke(&agent.handle, invocation).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, "agent:scribe");

            let continued = rig
                .state
                .continuing_caller(903, invocation)
                .expect("the durable invocation's caller");
            assert_eq!(
                continued,
                tilo_actions::remote::Caller::local_agent("scribe").in_room(room.clone())
            );
            assert!(rig.state.continuing_caller(904, invocation).is_none());

            // Its edits are admitted as the agent: what the grants allow,
            // and not what one denies.
            let action = |action: &str| PublishedAction {
                semantic_identity: None,
                resource: "document-1:body".into(),
                action: action.into(),
                resource_kind: "document".into(),
                resource_label: "Document".into(),
                action_label: action.into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                state: tilo_actions::ActionState::default(),
                operation_kind: tilo_actions::OperationKind::Effect,
                params: Vec::new(),
                contract: tilo_actions::ActionContract::default(),
                input: PublishedInput::None,
                view: None,
            };
            rig.state
                .replace_published_surfaces_for_test(vec![PublishedSurface {
                    peer_id: 903,
                    app_id: env.route.app_id.clone(),
                    route: env.route.clone(),
                    actions: vec![action("edit"), action("erase")],
                    live: true,
                }]);
            let mut app = rig.state.capture_invocations_for_test(903);
            let edit = |action: &str| InvokeRequest {
                on_behalf_of: Some(invocation),
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider: tilo_actions::remote::OWN_PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new("document-1:body", action),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            };
            assert!(matches!(
                rig.state
                    .action_invoke(&continued, Some(903), None, &edit("edit"))
                    .await,
                Some(Ok(_))
            ));
            assert_eq!(app.try_recv().unwrap().actor, "agent:scribe");
            assert!(!matches!(
                rig.state
                    .action_invoke(&continued, Some(903), None, &edit("erase"))
                    .await,
                Some(Ok(_))
            ));
            assert!(app.try_recv().is_err(), "the denied edit reaches no app");

            // A remote device's certified caller is checked again each
            // time: revoked, it continues nothing.
            let certified = rig
                .state
                .verify_action_caller(&room, &presentation(&room, &env.cert, &env.key))
                .unwrap();
            let certified = crate::collab_app_broker::ContinuingCaller::Certified {
                caller: Box::new(certified),
                foreign_restriction: false,
                direct: None,
            };
            assert_eq!(
                certified.current(&rig.state),
                Some(
                    tilo_actions::remote::Caller::authenticated(
                        env.cert.user_public_key,
                        env.cert.device_public_key
                    )
                    .in_room(room.clone())
                )
            );
            *env.authority.certificate.lock().unwrap() = None;
            assert!(certified.current(&rig.state).is_none());
            drop(agent);
            rig.stop().await;
        })
        .await;
}

/// A caller that lost the answer to its invoke finds the same invocation
/// again by its id from a new connection: one effect, the retained outcome.
#[tokio::test(flavor = "current_thread")]
async fn a_lost_answer_is_recovered_by_invocation_id_on_a_new_connection() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            admit_scribe(&env);
            let caller = rig
                .app_caller(&env, 911, "so.tilo.files", Some("scribe"))
                .await;
            let invocation = id(0xc101);
            let handle = caller.handle.clone();
            let pending =
                tokio::task::spawn_local(async move { invoke(&handle, invocation).await });
            let wire = tilo_test_env::within(
                "the invocation reaches the application",
                rig.delivered_effect(&env, invocation),
            )
            .await;
            // The caller goes away without reading its answer.
            pending.abort();
            drop(caller);
            let terminal = tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            rig.worker
                .handle
                .update_invocation(wire, &serde_json::to_string(&terminal).unwrap())
                .await
                .unwrap();
            let recovered = rig
                .app_caller(&env, 912, "so.tilo.files", Some("scribe"))
                .await;
            rig.directories.remove(903);
            let found = observe(&recovered.handle, invocation)
                .await
                .unwrap()
                .expect("the same agent finds its invocation");
            assert_eq!(found.update, terminal);
            let retained = invoke(&recovered.handle, invocation).await.unwrap();
            assert!(
                matches!(
                    retained.status,
                    tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{retained:?}"
            );
            assert_eq!(effect_count(&env).as_deref(), Some("1"));
            no_delivery(&rig).await;
            drop(recovered);
            rig.stop().await;
        })
        .await;
}

/// The application's directory answering for the fixture action, which
/// holds its answer once `block` is set until `release`.
struct GatedDirectory {
    block: Rc<std::cell::Cell<bool>>,
    entered: tokio::sync::mpsc::UnboundedSender<()>,
    release: Rc<tokio::sync::Notify>,
    row: runtime_collab::ActionDirectoryRow,
}
impl wire::DirectorySource for GatedDirectory {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
        Box::pin(std::future::ready(Err(
            "this directory only resolves".into()
        )))
    }

    fn resolve(
        &self,
        _: (&str, &str, &str),
        _local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        let block = self.block.get();
        let entered = self.entered.clone();
        let release = self.release.clone();
        let row = self.row.clone();
        Box::pin(async move {
            if block {
                entered.send(()).unwrap();
                release.notified().await;
            }
            Ok(runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(row),
                revision: None,
            })
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn durable_admission_rechecks_original_custody_after_the_directory_answers() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for mode in ["worker", "stream", "clock", "revocation", "retired"] {
                let env = Environment::new();
                let mut rig = Rig::new(&env).await;
                let block = Rc::new(std::cell::Cell::new(true));
                let release = Rc::new(tokio::sync::Notify::new());
                let (entered, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
                rig.worker
                    .handle
                    .publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(
                        GatedDirectory {
                            block: block.clone(),
                            entered,
                            release: release.clone(),
                            row: directory_state("original-r1").borrow().row(),
                        },
                    )))
                    .await
                    .unwrap();
                admit_scribe(&env);
                let caller = rig
                    .app_caller(&env, 914, "so.tilo.files", Some("scribe"))
                    .await;
                let handle = caller.handle.clone();
                let pending =
                    tokio::task::spawn_local(async move { invoke(&handle, id(0xe001)).await });
                tilo_test_env::within("the directory is asked", entered_rx.recv())
                    .await
                    .unwrap();
                match mode {
                    "worker" => {
                        rig.worker.server.abort();
                        rig.worker.client.abort();
                        let _ = (&mut rig.worker.server).await;
                        let _ = (&mut rig.worker.client).await;
                        rig.worker = join_worker(
                            &rig.state,
                            &rig.directories,
                            &env.route,
                            &env.cert,
                            &env.key,
                            903,
                        )
                        .await
                        .0;
                    }
                    "stream" => {
                        drop(rig.stream.take());
                        tilo_test_env::within("the application's stream closes", async {
                            while rig
                                .state
                                .invocation_subscribers
                                .lock()
                                .unwrap()
                                .get(&903)
                                .is_some_and(|sender| !sender.is_closed())
                            {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await;
                    }
                    "clock" => {
                        env.control.set_wall_ns(1);
                        env.control.advance_monotonic(Duration::from_secs(80));
                    }
                    "revocation" => {
                        *env.authority.certificate.lock().unwrap() = None;
                    }
                    "retired" => {
                        env.origin.invalidate();
                    }
                    _ => unreachable!(),
                }
                release.notify_one();
                assert!(pending.await.unwrap().is_err(), "{mode}");
                assert_eq!(
                    next_wire_id(&rig),
                    1,
                    "{mode} must fail before durable admission"
                );
                assert!(effect_count(&env).is_none());
                drop(caller);
                rig.stop().await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_agent_on_another_device_neither_sees_the_persons_invocation_nor_outlives_its_certificate()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let original = id(0xf001);
            invoke(&rig.worker.handle, original).await.unwrap();
            rig.stream.as_ref().unwrap().read().await.unwrap();
            let device = SigningKey::from_bytes(&[83; 32]);
            let mut certificate = env.cert.clone();
            certificate.device_public_key = device.verifying_key().to_bytes();
            certificate.device_label = "Temporary second device".into();
            certificate.capabilities = tilo_user_identity::DeviceCapabilities::member();
            certificate.expires_at_ns = Some(certificate.issued_at_ns + 6_000_000_000);
            tilo_user_identity::sign_device_cert(&mut certificate, &env.person).unwrap();
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .push(certificate.clone());
            admit_scribe(&env);
            let second = device_caller(&rig, &env, 918, &certificate, &device, "scribe").await;
            assert!(observe(&second.handle, original).await.unwrap().is_none());
            assert!(
                super::signed_worker::cancel(&second.handle, original)
                    .await
                    .is_err()
            );
            let invocation = id(0xf002);
            invoke(&second.handle, invocation).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let request = ui::invocation_scope::PublishedInvocationRequest {
                wire_id: delivered.id,
                invocation,
                address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
                argument: delivered.argument,
                actor: delivered.actor,
                participant: None,
            };
            let lease = (rig.authorize)(&request).unwrap();
            lease.check().unwrap();
            env.control.set_wall_ns(1);
            env.control.advance_monotonic(Duration::from_secs(7));
            assert!(!matches!(
                observe(&second.handle, invocation).await,
                Ok(Some(_))
            ));
            assert!(lease.check().is_err());
            assert!(
                observe(&rig.worker.handle, original)
                    .await
                    .unwrap()
                    .is_some(),
                "only the temporary device expired, not the host or its grants"
            );
            no_delivery(&rig).await;
            drop(second);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn host_execution_retirement_refuses_actual_retained_worker_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let descriptor = directory_state("original-r1")
                .borrow()
                .row()
                .descriptor
                .unwrap();
            rig.worker
                .handle
                .publish_actions(&[descriptor])
                .await
                .unwrap();
            let admitted = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .only(903)
                .unwrap()
                .clone();
            let caller = admitted.caller(&env.route.cluster_id).in_new_execution();
            let origin = crate::collab_app_broker::published_invocation::InvocationCaller::Local {
                route: env.route.clone(),
                admitted: Box::new(admitted),
                time: rig.state.registered_policy_time().unwrap(),
            };
            let invocation = tilo_actions::InvocationId::new();
            let mut request = durable_request(invocation);
            request.revision = None;
            local_host_answer(rig.state.action_invoke_with_origin(
                &caller,
                None,
                None,
                Some(&origin),
                &request,
            ))
            .await
            .expect("the broker admits the queued original caller");
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            let request = ui::invocation_scope::PublishedInvocationRequest {
                wire_id: delivered.id,
                invocation,
                address: tilo_actions::ActionAddress::new(delivered.resource, delivered.action),
                argument: delivered.argument,
                actor: delivered.actor,
                participant: Some(tilo_actions::ParticipantId::for_execution(
                    caller.execution_id().unwrap(),
                )),
            };
            let lease = (rig.authorize)(&request).unwrap();
            lease.check().unwrap();
            caller.retire_execution();
            assert!(
                lease.check().is_err(),
                "retained checked effects cannot outlive their execution"
            );
            assert!(
                (rig.authorize)(&request).is_err(),
                "the same delivered request cannot mint custody again"
            );
            rig.stop().await;
        })
        .await;
}

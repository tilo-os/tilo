// Regression: completing a semantic open does not retire its selected view.
struct SelectedViewDirectory;
impl runtime_collab::raw::action_directory::Server for SelectedViewDirectory {
    fn resolve(
        self: Rc<Self>,
        _params: runtime_collab::raw::action_directory::ResolveParams,
        mut results: runtime_collab::raw::action_directory::ResolveResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let mut row = directory_state("selected-r1").borrow().row();
        row.action = Some("open-direct-conversation".into());
        row.descriptor.as_mut().unwrap().action = "open-direct-conversation".into();
        row.visibility.as_mut().unwrap().action = "open-direct-conversation".into();
        capnp::capability::Promise::from(runtime_collab::write_action_directory_row(
            results.get().init_resolution().init_found(),
            &row,
        ))
    }
}

struct SelectedViewOwner {
    fail_open: std::sync::atomic::AtomicBool,
    oversized: std::sync::atomic::AtomicBool,
    changes: tokio::sync::watch::Sender<u64>,
    certificate: tilo_user_identity::DeviceCert,
}
impl crate::services::person_chat::PersonChatServices for SelectedViewOwner {
    fn command(
        &self,
        _room: &str,
        caller: &tilo_user_identity::DeviceCert,
        command: &[u8],
    ) -> Result<Vec<u8>, String> {
        assert_eq!(caller, &self.certificate);
        if self.fail_open.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("owner refused open".into());
        }
        let command: crate::services::person_chat::PersonChatCommand =
            serde_json::from_slice(command).unwrap();
        assert!(
            matches!(
                &command,
                crate::services::person_chat::PersonChatCommand::Open { .. }
                    | crate::services::person_chat::PersonChatCommand::Read { .. }
            ),
            "selected view must not broaden its command authority"
        );
        match command {
            crate::services::person_chat::PersonChatCommand::Open { public_key }
            | crate::services::person_chat::PersonChatCommand::Read { public_key } => {
                assert_eq!(public_key, "a".repeat(64));
            }
            _ => return Err("selected view must not broaden its command authority".into()),
        }
        if self.oversized.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(vec![0; 1024 * 1024 + 1]);
        }
        Ok(br#"{"peer":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","messages":[]}"#.to_vec())
    }
    fn subscribe(
        &self,
        _room: &str,
        caller: &tilo_user_identity::DeviceCert,
        _peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        assert_eq!(caller, &self.certificate);
        Ok(self.changes.subscribe())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_person_open_retains_selected_events_and_reads_not_original_commands() {
    tokio::task::LocalSet::new().run_until(async {
        let mut env = Environment::new();
        env.route.app_id = "so.tilo.chat".into();
        env.authority = CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
        let owner = Arc::new(SelectedViewOwner {
            fail_open: std::sync::atomic::AtomicBool::new(true),
            oversized: std::sync::atomic::AtomicBool::new(false),
            changes: tokio::sync::watch::channel(0).0,
            certificate: env.cert.clone(),
        });
        let mut services = env.membership.services();
        services.person_chat = Some(owner.clone());
        let rig = Rig::with_services(&env, services).await;
        let handle = &rig.worker.handle;
        handle.publish_action_directory("so.tilo.chat", capnp_rpc::new_client(SelectedViewDirectory)).await.unwrap();
        let (_, binding) = handle.resolve_published_action_for_invocation(
            903, "selected-r1", "workspace:atlas", "open-direct-conversation",
        ).await.unwrap();
        let peer = "a".repeat(64);
        let argument = serde_json::json!({"public_key":peer}).to_string();
        let command = serde_json::json!({"operation":"open","public_key":peer}).to_string();
        let original = id(0x005e_1ec7);
        handle.invoke_resolved_published_action_durable(
            original.0.as_bytes(), &binding, "selected-r1", "workspace:atlas",
            "open-direct-conversation", Some(&argument), "{}",
        ).await.unwrap();
        let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
        assert!(handle.person_chat(command.as_bytes(), original.0.as_bytes()).await.is_err());
        assert!(handle.subscribe_person_chat(&peer, original.0.as_bytes()).await.is_err(), "failed owner open cannot mint authority");

        // Even an otherwise valid original cannot enter through task ownership.
        let old_owner = {
            let mut pending = rig.state.pending_invocations.lock().unwrap();
            let entry = pending.get_mut(&delivered.id).unwrap();
            std::mem::replace(&mut entry.owner, InvocationOwner::NativeTask {
                task: "task:private".into(), agent: "scribe".into(), epoch: 1,
            })
        };
        owner.fail_open.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(handle.person_chat(command.as_bytes(), original.0.as_bytes()).await.is_err());
        assert!(handle.subscribe_person_chat(&peer, original.0.as_bytes()).await.is_err());
        rig.state.pending_invocations.lock().unwrap().get_mut(&delivered.id).unwrap().owner = old_owner;

        handle.person_chat(command.as_bytes(), original.0.as_bytes()).await.unwrap();
        let completed = tilo_actions::InvocationUpdate::Succeeded {
            invocation: original, output: tilo_actions::ActionOutput::default(),
        };
        handle.update_invocation(delivered.id, &serde_json::to_string(&completed).unwrap()).await.unwrap();
        // Subscription starts after the semantic handler has already completed.
        let stream = handle.subscribe_person_chat(&peer, original.0.as_bytes()).await.unwrap();
        assert_eq!(stream.next().await.unwrap(), 0);
        let snapshot = stream.read().await.unwrap();
        let snapshot: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        assert_eq!(snapshot["peer"], peer);
        assert_eq!(snapshot["messages"], serde_json::json!([]));
        let read = serde_json::json!({"operation":"read","public_key":peer}).to_string();
        assert!(handle.person_chat(read.as_bytes(), original.0.as_bytes()).await.is_err(), "a retained terminal original is not command authority");
        rig.state.pending_invocations.lock().unwrap().remove(&delivered.id);
        owner.changes.send_replace(1);
        assert_eq!(stream.next().await.unwrap(), 1);
        assert!(stream.read().await.is_ok(), "selected reads survive pending-original removal");
        owner.oversized.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(stream.read().await.is_err(), "bound before copying a service reply");
        owner.oversized.store(false, std::sync::atomic::Ordering::SeqCst);
        owner.fail_open.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(stream.read().await.is_err(), "each fetch must reauthorize at the owner");
        owner.fail_open.store(false, std::sync::atomic::Ordering::SeqCst);
        for operation in ["read", "send"] {
            let bytes = if operation == "read" {
                serde_json::json!({"operation":operation,"public_key":peer})
            } else {
                serde_json::json!({"operation":operation,"public_key":peer,"text":"no","invocation":original.0})
            }.to_string();
            assert!(handle.person_chat(bytes.as_bytes(), original.0.as_bytes()).await.is_err());
        }
        assert!(handle.subscribe_person_chat(&"b".repeat(64), original.0.as_bytes()).await.is_err());

        use crate::test_support::{Grants as _, KindSet};
        env.membership.write().workspaces[0].admit_agent("scribe", KindSet::READ);
        env.membership.write().workspaces[0].allow_app(
            tilo_action_policy::Subject::agent("scribe"), &env.route.app_id, KindSet::ALL,
        );
        let agent = rig.caller(&env, 905, Some("scribe")).await;
        let model_original = id(0x005e_1ec8);
        agent.handle.invoke_resolved_published_action_durable(
            model_original.0.as_bytes(), &binding, "selected-r1", "workspace:atlas",
            "open-direct-conversation", Some(&argument), "{}",
        ).await.unwrap();
        rig.stream.as_ref().unwrap().read().await.unwrap();
        assert!(handle.person_chat(command.as_bytes(), model_original.0.as_bytes()).await.is_err());
        assert!(handle.subscribe_person_chat(&peer, model_original.0.as_bytes()).await.is_err());

        // A waiting stream grants no authority after actual certificate loss.
        *env.authority.certificate.lock().unwrap() = None;
        owner.changes.send_replace(2);
        assert!(stream.next().await.is_err());
        assert!(stream.read().await.is_err(), "selected reads cannot survive worker certificate loss");
        drop(stream);
        rig.stop().await;
    }).await;
}

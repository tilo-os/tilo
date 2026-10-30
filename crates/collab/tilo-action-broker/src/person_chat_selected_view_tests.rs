// Completing a person's open of a conversation does not retire the view it
// selected, and grants no further commands.
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
async fn a_persons_open_conversation_keeps_its_selected_view_but_no_command_authority() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let owner = Arc::new(SelectedViewOwner {
                fail_open: std::sync::atomic::AtomicBool::new(true),
                oversized: std::sync::atomic::AtomicBool::new(false),
                changes: tokio::sync::watch::channel(0).0,
                certificate: env.cert.clone(),
            });
            let mut services = env.membership.services();
            services.person_chat = Some(owner.clone());
            services.local_person = Some(Arc::new(super::signed_worker::NativePerson(
                env.authority.clone(),
            )));
            let rig = Rig::with_services(&env, services).await;
            let caller = super::signed_worker::native_person(&env.route, &env.cert);
            let handle = &rig.worker.handle;
            let peer = "a".repeat(64);
            let open = || NamedActionDirectory {
                revision: "selected-r1",
                action: "open-direct-conversation",
            };
            let command = serde_json::json!({"operation":"open","public_key":peer}).to_string();
            let (original, delivered) =
                invoke_as_person(&rig, &caller, open(), serde_json::json!({"public_key": peer}))
                    .await;
            assert!(
                handle
                    .person_chat(command.as_bytes(), original.0.as_bytes())
                    .await
                    .is_err()
            );
            assert!(
                selected(handle, &peer, original).await.is_err(),
                "a failed owner open mints no authority"
            );

            // Even a valid original cannot enter through task ownership.
            owner
                .fail_open
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let person_owner = std::mem::replace(
                &mut rig
                    .state
                    .pending_invocations
                    .lock()
                    .unwrap()
                    .get_mut(&delivered.id)
                    .unwrap()
                    .owner,
                InvocationOwner::NativeTask {
                    task: "task:private".into(),
                    agent: "scribe".into(),
                    epoch: 1,
                },
            );
            assert!(
                handle
                    .person_chat(command.as_bytes(), original.0.as_bytes())
                    .await
                    .is_err()
            );
            assert!(selected(handle, &peer, original).await.is_err());
            rig.state
                .pending_invocations
                .lock()
                .unwrap()
                .get_mut(&delivered.id)
                .unwrap()
                .owner = person_owner;

            handle
                .person_chat(command.as_bytes(), original.0.as_bytes())
                .await
                .unwrap();
            let completed = tilo_actions::InvocationUpdate::Succeeded {
                invocation: original,
                output: tilo_actions::ActionOutput::default(),
            };
            handle
                .update_invocation(delivered.id, &serde_json::to_string(&completed).unwrap())
                .await
                .unwrap();
            // The view is selected after the open already completed.
            let (stream, (generation, snapshot)) =
                selected(handle, &peer, original).await.unwrap();
            assert_eq!(generation, 0);
            let snapshot: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
            assert_eq!(snapshot["peer"], peer);
            assert_eq!(snapshot["messages"], serde_json::json!([]));
            for command in [
                serde_json::json!({"operation":"read","public_key":peer}),
                serde_json::json!({"operation":"send","public_key":peer,"text":"no","invocation":original.0}),
            ] {
                assert!(
                    handle
                        .person_chat(command.to_string().as_bytes(), original.0.as_bytes())
                        .await
                        .is_err(),
                    "a retained terminal original is no command authority"
                );
            }
            assert!(
                selected(handle, &"b".repeat(64), original).await.is_err(),
                "the original selects only its own conversation"
            );
            rig.state
                .pending_invocations
                .lock()
                .unwrap()
                .remove(&delivered.id);
            owner.changes.send_replace(1);
            assert_eq!(
                stream.read().await.unwrap().0,
                1,
                "selected reads survive the original's removal"
            );

            use crate::test_support::{Grants as _, KindSet};
            env.membership.write().workspaces[0].admit_agent("scribe", KindSet::READ);
            env.membership.write().workspaces[0].allow_app(
                tilo_action_policy::Subject::agent("scribe"),
                &env.route.app_id,
                KindSet::ALL,
            );
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let request =
                named_action_request(&rig, open(), serde_json::json!({"public_key": peer})).await;
            super::signed_worker::receipt(
                super::signed_worker::ask(
                    &agent.handle,
                    tilo_actions::remote::RemoteActionMessage::Invoke(request.clone()),
                )
                .await
                .unwrap(),
            );
            rig.stream.as_ref().unwrap().read().await.unwrap();
            assert!(
                handle
                    .person_chat(command.as_bytes(), request.invocation.0.as_bytes())
                    .await
                    .is_err(),
                "an agent's open is not the person's"
            );
            assert!(selected(handle, &peer, request.invocation).await.is_err());

            // A selected view ends with the worker's certificate.
            *env.authority.certificate.lock().unwrap() = None;
            owner.changes.send_replace(2);
            assert!(
                stream.read().await.is_err(),
                "selected reads cannot survive worker certificate loss"
            );
            drop(stream);
            rig.stop().await;
        })
        .await;
}

/// The conversation with `peer` that `original` selects, and its first read.
async fn selected(
    handle: &runtime_collab::ChannelHandle,
    peer: &str,
    original: tilo_actions::InvocationId,
) -> Result<(runtime_collab::PersonChatStream, (u64, Vec<u8>)), runtime_portal::NotAvailable> {
    let stream = handle
        .subscribe_person_chat(peer, original.0.as_bytes())
        .await?;
    let read = stream.read().await?;
    Ok((stream, read))
}

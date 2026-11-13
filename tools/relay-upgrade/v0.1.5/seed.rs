// The Lounge a v0.1.5 relay keeps, written by v0.1.5's own relay and driven
// with v0.1.5's own test harness. `tools/relay-upgrade/run.sh` appends this to
// that release's `crates/hosting/tilo-relay/tests/persistent_room.rs`, so it
// uses the helpers there (`start_with`, `join_through_door`, `page`, ...), and
// runs it with `TILO_UPGRADE_DIR` naming where the state goes. The relay of
// this checkout then starts on that state
// (`persistent_room::release_upgrade`), and run.sh starts the release's relay
// on what it left afterwards, for a rollback's sake.
//
// Every person is a key this file derives from a seed byte, so the checkout's
// side makes the same devices without being told their keys.

mod release_seed {
    use super::*;
    use sha2::Digest as _;

    /// The people of the seeded Lounge: (seed byte, name). The checkout's
    /// side reads them from `seed.json`.
    const OWNER_DECLARED: (u8, &str) = (0x31, "Olu");
    const MODERATOR_DECLARED: (u8, &str) = (0x35, "Mo");
    const MEMBERS: [(u8, &str); 2] = [(0x41, "Ana"), (0x51, "Ben")];
    const STRANGER: (u8, &str) = (0x61, "Zed");
    /// The workspace of her own Ana makes and lists.
    const OWN_WORKSPACE: &str = "Ana's studio";
    const OWN_DESCRIPTION: &str = "Sketches, open to look at";
    /// The address someone left on the Lounge's wait list.
    const WAITING: &str = "ada@example.org";
    /// What Ana writes into the chat, which the upgrade must keep.
    const CHAT_TEXT: &str = "kept across the upgrade";

    /// Say which step the seed is at, so a hang names its step.
    fn step(what: &str) {
        eprintln!("release seed: {what}");
    }

    /// `future`, or a panic naming `what` once the hang bound passes.
    fn bounded<T>(
        runtime: &tokio::runtime::Runtime,
        what: &str,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        runtime
            .block_on(async { tokio::time::timeout(tilo_test_env::hang_bound(), future).await })
            .unwrap_or_else(|_| panic!("{what} did not answer in time"))
    }

    fn dir() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("TILO_UPGRADE_DIR").expect("TILO_UPGRADE_DIR names the state's place"),
        )
    }

    fn root_hex(seed: u8) -> String {
        hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        )
    }

    /// The Lounge as eu1's entrypoint starts it at v0.1.5, on `state`, with
    /// the door `door` (`open` or `invite`).
    fn start_release_lounge(
        dir: &std::path::Path,
        plain: std::net::SocketAddr,
        door: &[&str],
    ) -> std::io::Result<Workspace> {
        let state = dir.join("lounge");
        let web_plain = plain.to_string();
        let avatars = state.join("avatars");
        let roles = dir.join("roles.json");
        let mut args = vec![
            "--workspace-name",
            "Lounge",
            "--public-avatars",
            avatars.to_str().unwrap(),
            "--public-avatars-url",
            "https://lounge.example/avatars",
            "--listing-description",
            "The Tilo community: chat, notes and a shared desktop",
            "--default-role",
            "editor",
            "--inactive-after-days",
            "30",
            "--chat-channel",
            "general",
            "--chat-channel",
            "feedback",
            "--feedback-channel",
            "feedback",
            "--roles-file",
            roles.to_str().unwrap(),
            "--web-plain",
            &web_plain,
        ];
        args.extend_from_slice(door);
        start_with(&state, &dir.join("root"), &state.join("secrets"), &args)
    }

    /// A page of `who`'s device on the Lounge, joined to it.
    fn joined_page(
        plain: std::net::SocketAddr,
        workspace: &str,
        workspace_id: u64,
        runtime: &tokio::runtime::Runtime,
        (key, cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    ) -> (
        tilo_session_host::PeerHandle,
        tilo_user_identity::DeviceCert,
        tilo_peer_state::document_frame::DocumentRoute,
    ) {
        let (host, host_cert, route) = runtime.block_on(chat_route(plain, workspace));
        // A page's session runs on the runtime it is started in.
        let _entered = runtime.enter();
        let handle = page(host, key, cert, &host_cert);
        handle.join(workspace_id).unwrap();
        (handle, host_cert, route)
    }

    /// `who`'s page writes a real change into the chat, and the room commits
    /// it.
    fn write_chat(
        runtime: &tokio::runtime::Runtime,
        plain: std::net::SocketAddr,
        workspace: &str,
        workspace_id: u64,
        who: (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
        text: &str,
    ) {
        use automerge::transaction::Transactable as _;
        use collab_types::DocumentMutation;
        use tilo_session_host::PeerEvent;

        let (writer, _, route) = joined_page(plain, workspace, workspace_id, runtime, who);
        writer.open_route(route.clone()).unwrap();
        let mut doc = runtime.block_on(assert_page_reads_the_chat(
            "Ana",
            &writer,
            workspace_id,
            &route,
            &["general"],
        ));
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "edited", text).unwrap();
        doc.commit();
        writer
            .publish(
                route,
                &DocumentMutation::Incremental(doc.save_after(&before)),
            )
            .unwrap();
        let failures = runtime.block_on(wait_for(
            "Ana's chat edit to be committed",
            || {
                writer
                    .drain()
                    .iter()
                    .any(|event| matches!(event, PeerEvent::Committed { .. }))
                    .then(|| writer.status().failures)
            },
            || format!("{:?}", writer.status()),
        ));
        assert!(failures.is_empty(), "Ana's edit was refused: {failures:?}");
        writer.close();
    }

    /// A page of `who`'s device in the Lounge, once the host let it in.
    fn ready_page(
        runtime: &tokio::runtime::Runtime,
        plain: std::net::SocketAddr,
        workspace: &str,
        workspace_id: u64,
        who: (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    ) -> (tilo_session_host::PeerHandle, [u8; 32]) {
        let (handle, host_cert, _) = joined_page(plain, workspace, workspace_id, runtime, who);
        wait_ready(runtime, &handle, workspace_id);
        (handle, host_cert.device_public_key)
    }

    fn wait_ready(
        runtime: &tokio::runtime::Runtime,
        handle: &tilo_session_host::PeerHandle,
        workspace_id: u64,
    ) {
        use tilo_session_host::PeerEvent;
        runtime.block_on(wait_for(
            "the page to be let in",
            || {
                handle
                    .drain()
                    .iter()
                    .any(|event| {
                        matches!(event, PeerEvent::Ready { workspace_id: at } if *at == workspace_id)
                    })
                    .then_some(())
            },
            || format!("{:?}", handle.status()),
        ));
    }

    /// What the workspace's governance answers `verb` with `argument`, asked
    /// by `handle`'s device.
    fn govern(
        runtime: &tokio::runtime::Runtime,
        page: (&tilo_session_host::PeerHandle, [u8; 32]),
        workspace: &str,
        workspace_id: u64,
        verb: &str,
        argument: serde_json::Value,
    ) -> serde_json::Value {
        ask_host(
            runtime,
            page,
            workspace_id,
            tilo_actions::remote::RemoteActionTarget::workspace_governance(workspace, verb),
            argument,
        )
    }

    /// What the host answers the action `target` with `argument`, asked by
    /// `handle`'s device in the workspace `workspace_id`.
    fn ask_host(
        runtime: &tokio::runtime::Runtime,
        (handle, host): (&tilo_session_host::PeerHandle, [u8; 32]),
        workspace_id: u64,
        target: tilo_actions::remote::RemoteActionTarget,
        argument: serde_json::Value,
    ) -> serde_json::Value {
        use tilo_actions::InvocationId;
        use tilo_actions::remote::{
            InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
        };
        use tilo_session_host::PeerEvent;

        let serde_json::Value::Object(argument) = argument else {
            panic!("arguments are an object");
        };
        let message = RemoteActionMessage::Invoke(InvokeRequest {
            invocation: InvocationId::new(),
            target: target.clone(),
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        });
        let frame = RemoteActionFrame::new(message).encode().unwrap();
        handle
            .send_remote_action(workspace_id, host, frame)
            .unwrap();
        let verb = target.address.action.clone();
        let answer = runtime.block_on(wait_for(
            &format!("the answer to {verb}"),
            || {
                handle.drain().into_iter().find_map(|event| match event {
                    PeerEvent::RemoteAction { from, frame, .. } if from == host => {
                        Some(RemoteActionFrame::decode(&frame).unwrap().message)
                    }
                    PeerEvent::RemoteActionUndeliverable { reason, .. } => {
                        panic!("{verb} was undeliverable: {reason}")
                    }
                    _ => None,
                })
            },
            || format!("{:?}", handle.status()),
        ));
        let RemoteActionMessage::Receipt(receipt) = answer else {
            panic!("{verb} was answered with no receipt: {answer:?}");
        };
        let ReceiptStatus::Completed { output } = receipt.status else {
            panic!("{verb} was refused: {:?}", receipt.status);
        };
        output.value.unwrap_or_default()
    }

    /// The Lounge a v0.1.5 relay keeps: listed, its door open at first and
    /// then taking people by invitation with a wait list; an owner and a
    /// moderator the operator declared, two members who came in through the
    /// door, a chat one of them wrote in, a pending invitation the owner made,
    /// a workspace of her own one member made and listed, a stranger the
    /// invitation-only door turned away, and an address left for the wait
    /// list, which this release keeps only for a members-only workspace.
    #[test]
    #[ignore = "writes the state tools/relay-upgrade/run.sh hands the checkout's relay"]
    fn a_lounge_the_release_keeps() {
        let dir = dir();
        for sub in ["lounge/relay", "lounge/avatars", "lounge/secrets"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("root"), "11".repeat(32)).unwrap();
        std::fs::write(
            dir.join("roles.json"),
            serde_json::json!({
                "owners": [root_hex(OWNER_DECLARED.0)],
                "moderators": [root_hex(MODERATOR_DECLARED.0)],
            })
            .to_string(),
        )
        .unwrap();
        let plain = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();

        // The Lounge with its door open: people come in at its default role.
        step("the Lounge starts with its door open");
        let room = start_release_lounge(&dir, plain, &[]).unwrap();
        let (workspace, workspace_id) = announced(&room);
        let mut people = vec![OWNER_DECLARED, MODERATOR_DECLARED];
        people.extend(MEMBERS);
        for (seed, name) in &people {
            step(&format!("{name} comes in through the door"));
            join_through_door(&runtime, plain, &workspace, own_device(*seed, name), name);
        }
        step("Ana writes in the chat");
        write_chat(
            &runtime,
            plain,
            &workspace,
            workspace_id,
            own_device(MEMBERS[0].0, MEMBERS[0].1),
            CHAT_TEXT,
        );
        // The owner the operator declared invites someone, and leaves the
        // invitation open.
        let (owner, host) = ready_page(
            &runtime,
            plain,
            &workspace,
            workspace_id,
            own_device(OWNER_DECLARED.0, OWNER_DECLARED.1),
        );
        step("the declared owner invites someone");
        let invitation = govern(
            &runtime,
            (&owner, host),
            &workspace,
            workspace_id,
            "invite",
            serde_json::json!({"role": "editor", "invitee_name": "Cy"}),
        )["invite"]
            .as_str()
            .expect("the invitation's id")
            .to_owned();
        owner.close();

        // A member makes a workspace of their own beside the Lounge, and
        // lists it in the directory.
        let (ana, host) = ready_page(
            &runtime,
            plain,
            &workspace,
            workspace_id,
            own_device(MEMBERS[0].0, MEMBERS[0].1),
        );
        step("Ana makes a workspace of her own");
        let created = ask_host(
            &runtime,
            (&ana, host),
            workspace_id,
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_relay::workspaces::HOST_ACTIONS_PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(
                    tilo_relay::workspaces::HOST_ROOMS_RESOURCE,
                    tilo_relay_session::workspaces::CREATE_OWN_ROOM,
                ),
            },
            serde_json::json!({"name": OWN_WORKSPACE}),
        );
        let created: tilo_relay_session::workspaces::CreatedWorkspace =
            serde_json::from_value(created.clone())
                .unwrap_or_else(|error| panic!("{error}: {created}"));
        let studio = created.workspace;
        let into_studio = hex::decode(
            created
                .invitation
                .expect("an invitation into the workspace of her own")
                .invitation,
        )
        .unwrap();
        let (key, cert) = own_device(MEMBERS[0].0, MEMBERS[0].1);
        step("Ana redeems the invitation into it");
        match bounded(
            &runtime,
            "Ana's redemption",
            tilo_relay_client::invite::ask_to_join(
                &Plain(plain),
                &into_studio,
                &cert,
                &key,
                [7; 32],
                MEMBERS[0].1,
            ),
        ) {
            Ok(tilo_relay_protocol::workspaces::JoinReply::Admitted { .. }) => {}
            other => panic!("Ana did not get into her own workspace: {other:?}"),
        }
        step("Ana enters her workspace");
        ana.join(studio.workspace_id).unwrap();
        wait_ready(&runtime, &ana, studio.workspace_id);
        step("Ana lists her workspace");
        govern(
            &runtime,
            (&ana, host),
            &studio.workspace,
            studio.workspace_id,
            "publish",
            serde_json::json!({"default_role": "viewer", "description": OWN_DESCRIPTION}),
        );
        ana.close();
        step("the directory lists both");
        let listed = bounded(&runtime, "the directory", directory_page(plain));
        let members = listed
            .workspaces
            .iter()
            .find(|listed| listed.workspace == workspace)
            .expect("the Lounge is listed")
            .members;
        assert!(
            listed
                .workspaces
                .iter()
                .any(|listed| listed.workspace == studio.workspace),
            "Ana's workspace is listed: {listed:?}"
        );
        room.stop();

        // The same Lounge taking people by invitation and keeping a wait list,
        // as eu1 runs it now: a stranger is turned away, and an address left
        // at the door is kept.
        step("the Lounge starts again taking people by invitation");
        let mut room = start_release_lounge(
            &dir,
            plain,
            &["--invitation-only", "--wait-list-max", "500"],
        )
        .unwrap();
        let (again, _) = announced(&room);
        assert_eq!(again, workspace);
        let (_, stranger) = own_device(STRANGER.0, STRANGER.1);
        let (status, body) = bounded(
            &runtime,
            "the door",
            ask_door_raw(plain, &workspace, &stranger, STRANGER.1),
        );
        let body = String::from_utf8_lossy(&body);
        assert!(
            status != 200 || !body.contains("admission_token"),
            "the invitation-only door let a stranger in: {body}"
        );
        step("an address is left on the wait list");
        let removal = hex::encode(sha2::Sha256::digest(b"the visitor's removal secret"));
        let line = format!(
            "{}\n",
            serde_json::json!({
                "op": "wait_list",
                "request": "aa",
                "contact": WAITING,
                "removal_sha256": removal,
                "language": "en",
            })
        );
        let stdin = room.stdin.as_mut().expect("the operator's input");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.flush().unwrap();
        // A listed Lounge of this release refuses it ("the workspace keeps no
        // wait list": its wait list is a members-only workspace's), so the
        // state eu1 keeps holds none; said in seed.json either way.
        let wait_list = match room.next(|event| {
            matches!(
                event,
                OperatorEvent::WaitListed { .. } | OperatorEvent::WaitListRefused { .. }
            )
        }) {
            OperatorEvent::WaitListed { added: true, .. } => {
                serde_json::json!({"contact": WAITING})
            }
            OperatorEvent::WaitListRefused { reason, .. } => {
                serde_json::json!({"contact": null, "refused": reason})
            }
            other => panic!("the wait list answered {other:?}"),
        };
        room.stop();

        let seed = serde_json::json!({
            "release": "v0.1.5",
            "workspace": workspace,
            "workspace_id": workspace_id,
            "owner_root": "11".repeat(32),
            "declared_owner": {"seed": OWNER_DECLARED.0, "name": OWNER_DECLARED.1},
            "declared_moderator": {"seed": MODERATOR_DECLARED.0, "name": MODERATOR_DECLARED.1},
            "members": MEMBERS.iter().map(|(seed, name)| serde_json::json!({"seed": seed, "name": name})).collect::<Vec<_>>(),
            "stranger": {"seed": STRANGER.0, "name": STRANGER.1},
            "listed_members": members,
            "invitation": invitation,
            "own_workspace": {
                "workspace": studio.workspace,
                "workspace_id": studio.workspace_id,
                "name": OWN_WORKSPACE,
                "description": OWN_DESCRIPTION,
                "owner": {"seed": MEMBERS[0].0, "name": MEMBERS[0].1},
            },
            "chat_text": CHAT_TEXT,
            "wait_list": wait_list,
        });
        std::fs::write(
            dir.join("seed.json"),
            serde_json::to_vec_pretty(&seed).unwrap(),
        )
        .unwrap();
    }
}

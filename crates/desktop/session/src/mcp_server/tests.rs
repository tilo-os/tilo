use super::*;
use tilo_test_env::SpawnUnderEnvLock as _;

/// An agent's `open` and `place` naming nothing to stand beside stand beside the conversation
/// of its turn; one that names its own anchor, and every other action, keep their arguments.
#[test]
fn a_turns_open_and_place_stand_beside_its_conversation() {
    let conversation = "chat.workspace/channels/c1";
    let address = |action: &str| {
        tilo_actions::ActionAddress::new(tilo_session_core::layout::RESOURCE, action)
    };
    let resource = serde_json::json!({"resource": "tl:so.tilo.tasks:root-t"});
    let resource = resource.as_object().expect("an object");
    for action in ["open", "place"] {
        let filled = with_turn_beside(&address(action), resource, conversation);
        assert_eq!(filled["beside"], conversation, "{action}");
    }
    let own = serde_json::json!({"resource": "r", "beside": "shell:place:3"});
    let own = own.as_object().expect("an object");
    assert_eq!(
        with_turn_beside(&address("place"), own, conversation)["beside"],
        "shell:place:3"
    );
    assert!(
        !with_turn_beside(&address("place-found"), resource, conversation).contains_key("beside")
    );
}

/// What the person remembers is the program, and every connection it admits is an instance of
/// its own: revoking the one takes back the answer of the other, and no other program's.
#[test]
fn an_answer_remembered_for_a_program_covers_each_instance_of_it() {
    assert!(same_program("claude-code", "claude-code@host#1"));
    assert!(same_program("claude-code@host", "claude-code@host#2"));
    assert!(same_program("claude-code", "claude-code@laptop-3f2a#1"));
    assert!(same_program("claude-code@host#1", "claude-code@host#1"));
    assert!(!same_program("claude-code", "codex@host#2"));
    assert!(
        !same_program("claude-code", "claude-codex@host#1"),
        "an id that only begins with the same letters is another program"
    );
}

/// The login's shell runtime, hidden from the session, is told from a sandboxed program by its
/// proof that it holds the device identity: made with the device's key, over the statement of the
/// very process that connected, for the request it makes. A proof made with another device's key,
/// for another request, by another process or by one that has ended, or none at all, is not it;
/// nor is any proof when the session itself holds no device identity to check it with.
#[test]
fn the_shell_runtime_proves_it_holds_the_device_identity() {
    use runtime_portal::holder_proof::statement;
    let device = CallerKey::for_session(&[7; 32]).unwrap();
    let other_device = CallerKey::for_session(&[8; 32]).unwrap();
    let store = CallerKey::derive(&[7; 32]).unwrap();
    let mut runtime = std::process::Command::new("sleep")
        .arg("30")
        .spawn_locked()
        .unwrap();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn_locked()
        .unwrap();
    let pid = runtime.id();
    let op = runtime_portal::workspace_terminal::OP;
    let proof = |key: &CallerKey, of: u32, op: &str| {
        hex::encode(key.prove(&statement(of).unwrap(), op, b"").unwrap())
    };
    let made = proof(&device, pid, op);
    assert!(holds_device_identity(Some(&device), pid, op, Some(&made)));
    assert!(
        !holds_device_identity(Some(&device), pid, op, None),
        "no proof"
    );
    assert!(
        !holds_device_identity(None, pid, op, Some(&made)),
        "a session holding no device identity"
    );
    assert!(
        !holds_device_identity(Some(&device), pid, op, Some(&proof(&other_device, pid, op))),
        "another device's key"
    );
    assert!(
        !holds_device_identity(Some(&device), pid, op, Some(&proof(&store, pid, op))),
        "a proof made for the Secret Service"
    );
    assert!(
        !holds_device_identity(
            Some(&device),
            pid,
            runtime_portal::environment_socket::OP,
            Some(&made)
        ),
        "a proof made for another request"
    );
    assert!(
        !holds_device_identity(Some(&device), other.id(), op, Some(&made)),
        "the proof of another process"
    );
    assert!(
        !holds_device_identity(Some(&device), pid, op, Some("not hex")),
        "a garbled proof"
    );
    let _ = runtime.kill();
    let _ = runtime.wait();
    assert!(
        !holds_device_identity(Some(&device), pid, op, Some(&made)),
        "a process that has ended"
    );
    let _ = other.kill();
    let _ = other.wait();
}

/// A process that loads the device key makes itself non-dumpable, and the kernel then hides its
/// namespaces from the session: such a peer is hidden, which is not a peer sharing the session's
/// namespaces, and a process the session can look at is not hidden.
#[test]
fn a_process_holding_the_device_key_is_hidden_from_the_session() {
    let mut shown = std::process::Command::new("sleep")
        .arg("30")
        .spawn_locked()
        .unwrap();
    assert_eq!(shares_security_namespaces(shown.id()), Some(true));
    let _ = shown.kill();
    let _ = shown.wait();

    let mut ready = [0_i32; 2];
    // SAFETY: a pipe into a buffer of two descriptors.
    assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
    // SAFETY: the child only makes itself non-dumpable, says so on the pipe and waits to be
    // killed: async-signal-safe calls, no allocation and no lock after the fork.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        // SAFETY: as above.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            libc::write(ready[1], b"r".as_ptr().cast(), 1);
            loop {
                libc::pause();
            }
        }
    }
    let mut byte = [0_u8; 1];
    // SAFETY: one byte into a buffer of one, from the pipe this test made.
    assert_eq!(
        unsafe { libc::read(ready[0], byte.as_mut_ptr().cast(), 1) },
        1
    );
    let hidden = shares_security_namespaces(u32::try_from(pid).unwrap());
    // SAFETY: the child this test forked, and the pipe it made.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(ready[0]);
        libc::close(ready[1]);
    }
    assert_eq!(hidden, None);
}

/// An agent works in one workspace: the shell's windows of another, and windows the shell no
/// longer knows, are not its; the windows of its own are, and so is anything not a window.
#[test]
fn a_window_of_another_workspace_is_not_an_agents() {
    let services = SessionServices::new(Arc::new(|| {}), "person");
    services
        .tabs
        .lock_recover()
        .extend([(7, "studio".to_owned()), (9, "garden".to_owned())]);
    assert!(
        !services.window_elsewhere("desktop:tab:7", "studio"),
        "its own"
    );
    assert!(
        services.window_elsewhere("desktop:tab:9", "studio"),
        "another's"
    );
    assert!(
        services.window_elsewhere("desktop:tab:11", "studio"),
        "one no longer on the shell"
    );
    assert!(
        !services.window_elsewhere("notes.notebook", "studio"),
        "a resource that is no window"
    );
}

/// A workspace app's record is read without waiting on the session's pass, which the loop
/// answers only between frames: only the apps the session's services keep are asked there.
#[test]
fn a_workspace_apps_record_is_read_without_the_sessions_pass() {
    use tilo_actions::remote::{ResourceAddress, ResourceReads as _};
    // A session that never answers, and no broker: a read asking the session waits it out.
    let records = ServiceRecords {
        state: None,
        session: Arc::new(SessionServices::new(Arc::new(|| {}), "person")),
        agent_session: None,
    };
    let started = std::time::Instant::now();
    let read = records.read(
        &Caller::local_person("person"),
        &ResourceAddress {
            workspace: "workspace".to_owned(),
            app: "so.tilo.chat".to_owned(),
            record: String::new(),
        },
    );
    assert!(
        matches!(&read, Err(ActionCallError::Unavailable { reason })
            if reason == "this desktop keeps no records of its own"),
        "answered from the broker, which this shell has none of: {read:?}"
    );
    assert!(started.elapsed() < WAIT, "nothing waited on the session");
    assert!(records.session.asks.lock_recover().is_empty());
}

/// The login's shell runtime comes in by the entrance that asks only for its environments'
/// sockets and its workspaces' terminals, and proves there that it holds the device identity: a
/// hidden process of this account is let in, a process the session can look at in another mount
/// namespace is a sandboxed program and is refused as one, and another account's process is
/// refused before anything of it is considered.
#[test]
fn a_hidden_process_of_this_account_comes_in_to_prove_its_device_identity() {
    let own = rustix::process::geteuid().as_raw();
    let peer = |uid, shares_namespaces, hidden| Peer {
        uid,
        shares_namespaces,
        hidden,
    };
    assert!(
        matches!(
            entrance(7, &peer(own, false, true)),
            Ok(From::ShellRuntime(7))
        ),
        "hidden, and in the login's account: let in to prove it"
    );
    assert!(
        matches!(
            entrance(7, &peer(own, false, false)),
            Err(PeerRefusal::Sandboxed)
        ),
        "a program in another mount namespace, which the session can look at, is sandboxed"
    );
    assert!(
        matches!(
            entrance(7, &peer(own.saturating_add(1), false, true)),
            Err(PeerRefusal::OtherAccount)
        ),
        "another account's process, hidden or not, is not let in"
    );
    assert!(
        matches!(entrance(7, &peer(own, true, false)), Ok(From::Login(7))),
        "a program of this login sharing its namespaces is an ordinary one"
    );
}

#[test]
fn the_peer_must_be_this_accounts_own_process_in_its_namespaces() {
    let own = rustix::process::geteuid().as_raw();
    let shares = |uid, namespaces| {
        peer_decision(&Peer {
            uid,
            shares_namespaces: namespaces,
            hidden: false,
        })
    };
    assert_eq!(shares(own, true), Ok(()));
    assert_eq!(
        shares(own.saturating_add(1), true),
        Err(PeerRefusal::OtherAccount),
        "another account"
    );
    assert_eq!(
        shares(own, false),
        Err(PeerRefusal::Sandboxed),
        "a program inside a sandbox"
    );
}

/// A service publishing the one style the person may switch to, as the device's does.
struct Styles {
    offered: serde_json::Value,
}

impl tilo_service::Service for Styles {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        Vec::new()
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == crate::device_lists::STYLES_RESOURCE).then(|| self.offered.clone())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        Err(tilo_actions::ApiError::Unknown(
            tilo_actions::ActionAddress::new(resource, action),
        ))
    }
}

/// A program's read of a resource the session publishes (the device's styles, as this session
/// shows them) is answered from the session's own services on the loop's pass, where they are:
/// one `tilo` call reads `styles.installed.offered` with no record and no app, as the shell's own
/// parts read it.
#[test]
fn a_program_reads_the_reading_a_session_service_publishes() {
    let offered = serde_json::json!({
        "offered": [{"style": "so.tilo.shell:strips", "package": "so.tilo.shell",
                     "name": "strips", "line": "The shell's strips", "loads": true, "reason": ""}],
    });
    let mut services = tilo_service::Services::default();
    services.add(Box::new(Styles {
        offered: offered.clone(),
    }));
    assert_eq!(
        published_reading(
            &services,
            tilolang_tilo::platform_apps::STYLES_APP,
            crate::device_lists::STYLES_RESOURCE
        ),
        Some(offered),
        "the reading its service publishes"
    );
    // A device resource no service of this device serves is published empty, as the shell's
    // parts are handed it: a program reads its fields as absent and goes on.
    assert_eq!(
        published_reading(&services, tilolang_tilo::platform_apps::DEVICE_APP, "power"),
        Some(serde_json::json!({})),
        "a resource no service serves is published empty"
    );
    // The session's own roots (`device.windows`) are published as its own app's.
    assert_eq!(
        published_reading(
            &services,
            tilolang_tilo::platform_apps::DEVICE_APP,
            "session:windows",
        ),
        Some(serde_json::json!({})),
        "a resource of the session's own, read as it stands here"
    );
}

/// A resource the session does not publish itself is no reading of its, whatever app names it:
/// an app's record is read through the record API, never from a service.
#[test]
fn what_the_session_does_not_publish_is_no_reading_of_its() {
    let mut services = tilo_service::Services::default();
    services.add(Box::new(Styles {
        offered: serde_json::json!({"offered": []}),
    }));
    // An app whose records are read, by the id its own record's resource has, is no reading the
    // session publishes: none of `platform_roots` names it.
    assert_eq!(
        published_reading(&services, "so.tilo.notes", "notes.notebook"),
        None,
        "no reading of an app's record"
    );
    // Nor is a session resource asked of an app that does not publish it.
    assert_eq!(
        published_reading(
            &services,
            tilolang_tilo::platform_apps::SETTINGS_APP,
            crate::device_lists::STYLES_RESOURCE,
        ),
        None,
        "the device publishes the styles, not Settings"
    );
}

/// A follow of a reading is woken by the pass that moves what the services publish, once for a
/// revision: a pass that published nothing new wakes none, and the follow reads again by itself.
#[test]
fn a_pass_that_moves_the_readings_wakes_the_follows() {
    use tilo_actions::remote::ResourceWake;
    let services = SessionServices::new(Arc::new(|| {}), "person");
    let (wake, woken) = ResourceWake::new();
    services.watch_reading(&wake);
    services.readings_moved(1);
    assert!(
        woken.wait_timeout(WAIT).is_ok(),
        "what the services publish moved, so the follow woke"
    );
    let (wake, woken) = ResourceWake::new();
    services.watch_reading(&wake);
    services.readings_moved(1);
    let woken = match woken.wait_timeout(std::time::Duration::from_millis(50)) {
        Err(woken) => woken,
        Ok(_) => panic!("a pass at the revision the follow already saw wakes none"),
    };
    services.readings_moved(2);
    assert!(
        woken.wait_timeout(WAIT).is_ok(),
        "the next revision wakes the follow that was still watching"
    );
}

/// A device list the session publishes changes on its own pass, and the record read a program
/// follows shows the new reading: the pass that moved what the services publish wakes the follow,
/// which reads again, rather than a timer asking.
#[test]
fn a_live_reading_that_changes_reaches_its_follower() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_actions::remote::{ResourceAddress, ResourceAsk, ResourceCopy, read_resources};

    /// A device list whose reading the test moves, as its device would on its own pass.
    struct LiveStyles {
        offered: Arc<std::sync::Mutex<serde_json::Value>>,
        moved: Arc<AtomicBool>,
    }

    impl tilo_service::Service for LiveStyles {
        fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
            vec![tilo_actions::ResourceSpec::new(
                crate::device_lists::STYLES_RESOURCE,
                "session.styles",
                "Styles",
            )]
        }

        fn read(&self, resource: &str) -> Option<serde_json::Value> {
            (resource == crate::device_lists::STYLES_RESOURCE)
                .then(|| self.offered.lock().unwrap().clone())
        }

        fn invoke(
            &mut self,
            resource: &str,
            action: &str,
            _: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
            Err(tilo_actions::ApiError::Unknown(
                tilo_actions::ActionAddress::new(resource, action),
            ))
        }

        fn poll(&mut self) -> bool {
            self.moved.swap(false, Ordering::SeqCst)
        }
    }

    /// One of the loop's passes, where the services are: what moved wakes the follows waiting on
    /// it, and every reading a connection asks for is answered from the services as it stands.
    fn pass(services: &mut tilo_service::Services, session: &SessionServices) {
        if services.poll() {
            session.readings_moved(services.revision());
        }
        let asked = std::mem::take(&mut *session.asks.lock_recover());
        for ask in asked {
            if let ApiAsk::Reading {
                app,
                resource,
                done,
            } = ask
            {
                done.complete(published_reading(services, &app, &resource).map(|reading| {
                    Published {
                        kind: published_kind(services, &resource),
                        reading,
                    }
                }));
            }
        }
    }

    let offered = Arc::new(std::sync::Mutex::new(serde_json::json!({"offered": []})));
    let moved = Arc::new(AtomicBool::new(false));
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    let mut services = tilo_service::Services::default();
    services.add(Box::new(LiveStyles {
        offered: Arc::clone(&offered),
        moved: Arc::clone(&moved),
    }));
    let records = Arc::new(super::records::ServiceRecords {
        state: None,
        session: Arc::clone(&session),
        agent_session: None,
    });
    let caller = Caller::local_person("person");
    let asks = [ResourceAsk {
        address: ResourceAddress {
            workspace: "studio".to_owned(),
            app: tilolang_tilo::platform_apps::STYLES_APP.to_owned(),
            record: crate::device_lists::STYLES_RESOURCE.to_owned(),
        },
        joined: false,
    }];
    let first = {
        let records = Arc::clone(&records);
        let caller = caller.clone();
        let asks = asks.clone();
        let reading = std::thread::spawn(move || {
            read_resources(&*records, &caller, &asks, None, tilo_clock::now())
        });
        let deadline = std::time::Instant::now() + WAIT;
        while !reading.is_finished() {
            pass(&mut services, &session);
            assert!(
                std::time::Instant::now() < deadline,
                "the first read did not answer"
            );
            std::thread::yield_now();
        }
        reading
            .join()
            .expect("the reader")
            .expect("the reading the device publishes")
    };
    assert_eq!(
        first.records.first().and_then(|(_, copy)| copy.clone()),
        Some(ResourceCopy::reading(
            serde_json::json!({"offered": []}),
            None
        )),
        "a program reads the device's styles"
    );
    // Only the follow's watch is woken from here on, never the first read's.
    session.readings_woken.lock_recover().clear();
    let follower = {
        let records = Arc::clone(&records);
        let caller = caller.clone();
        let asks = asks.clone();
        let after = first.revision.clone();
        std::thread::spawn(move || {
            read_resources(
                &*records,
                &caller,
                &asks,
                Some(&after),
                tilo_clock::now().checked_add(WAIT).expect("a deadline"),
            )
        })
    };
    // The loop's pass, where the services are: what moved wakes the follows waiting on it, and
    // every reading a connection asks for is answered from the services as this pass reads them.
    // The device moves its list once the follow watches, and the pass that takes the move in
    // wakes it.
    let deadline = std::time::Instant::now() + WAIT;
    let mut moved_device = false;
    while !follower.is_finished() {
        pass(&mut services, &session);
        if !moved_device
            && session
                .readings_woken
                .lock_recover()
                .iter()
                .any(tilo_actions::remote::ResourceWake::waiting)
        {
            *offered.lock().unwrap() = serde_json::json!({"offered": ["warm"]});
            moved.store(true, Ordering::SeqCst);
            moved_device = true;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the follow did not read the changed device"
        );
        std::thread::yield_now();
    }
    assert!(moved_device, "the follow watched its reading");
    let changed = follower
        .join()
        .expect("the follower")
        .expect("the changed reading");
    assert_eq!(
        changed.records.first().and_then(|(_, copy)| copy.clone()),
        Some(ResourceCopy::reading(
            serde_json::json!({"offered": ["warm"]}),
            None
        )),
        "the changed reading reached the follower"
    );
    assert_ne!(changed.revision, first.revision);
}

/// A program's reading of a resource the session publishes is decided for its caller, as every
/// read of a record is: the person reads their own device, and an agent reads it only where the
/// broker lets it read the resource. With no broker to decide it, an agent reads none of it.
#[test]
fn a_reading_is_the_callers_only_where_the_read_is_theirs() {
    use tilo_actions::remote::ResourceReads as _;
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    let records = super::records::ServiceRecords {
        state: None,
        session: Arc::clone(&session),
        agent_session: None,
    };
    let offered = serde_json::json!({"offered": []});
    // The loop's pass, answering the reading asked of it with the styles, published under the
    // kind `kind` (`None`: under none).
    let answer_once = |session: &SessionServices, kind: Option<&str>| {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            let asked = std::mem::take(&mut *session.asks.lock_recover());
            if let Some(ApiAsk::Reading { done, .. }) = asked.into_iter().next() {
                done.complete(Some(Published {
                    kind: kind.map(str::to_owned),
                    reading: offered.clone(),
                }));
                return;
            }
            assert!(std::time::Instant::now() < deadline, "no reading was asked");
            std::thread::yield_now();
        }
    };
    let address = tilo_actions::remote::ResourceAddress {
        workspace: "studio".to_owned(),
        app: tilolang_tilo::platform_apps::STYLES_APP.to_owned(),
        record: crate::device_lists::STYLES_RESOURCE.to_owned(),
    };
    let read_as = |caller: &Caller, kind: Option<&str>| {
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| records.read(caller, &address));
            answer_once(&session, kind);
            reader.join().expect("the reader")
        })
    };
    // A reading published under no kind has no node to decide it at: nobody reads it, the
    // person included, rather than anyone reading it unchecked.
    for caller in [
        Caller::local_person("person"),
        Caller::local_agent("helper").in_workspace("studio"),
    ] {
        assert_eq!(
            read_as(&caller, None),
            Ok(tilo_actions::remote::ResourceCopy::Absent),
            "no kind, no reading"
        );
    }
    for (caller, theirs) in [
        (Caller::local_person("person"), true),
        (Caller::local_agent("helper").in_workspace("studio"), false),
    ] {
        let read = read_as(&caller, Some("session.styles"));
        if theirs {
            assert_eq!(
                read,
                Ok(tilo_actions::remote::ResourceCopy::reading(
                    offered.clone(),
                    None
                )),
                "the person's own device"
            );
        } else {
            assert!(
                matches!(
                    read,
                    Err(ActionCallError::NotPermitted | ActionCallError::Refused { .. })
                ),
                "an agent no broker lets read it reads none of it: {read:?}"
            );
        }
    }
}

use super::*;
use tilo_actions::remote::ResourceCopy;

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
    };
    let started = std::time::Instant::now();
    let read = tilo_rt::block_on(records.read(
        &Caller::local_person("person"),
        &ResourceAddress {
            workspace: "workspace".to_owned(),
            app: "so.tilo.chat".to_owned(),
            record: String::new(),
        },
    ))
    .expect("a runtime to read on");
    assert!(
        matches!(&read, Err(ActionCallError::Unavailable { reason })
            if reason == tilo_actions::remote::RESOURCE_READS_UNAVAILABLE),
        "answered by the device's daemon, which this session reaches none of: {read:?}"
    );
    assert!(started.elapsed() < WAIT, "nothing waited on the session");
    assert!(records.session.asks.lock_recover().is_empty());
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

/// `read`, run on a thread of its own while `pass`, the loop's pass where the services are,
/// answers what it asks of them: the one reader of what the session's services publish.
fn answered<T: Send>(
    mut pass: impl FnMut(),
    read: impl std::future::Future<Output = T> + Send,
) -> T {
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| tilo_rt::block_on(read).expect("a runtime to read on"));
        let deadline = std::time::Instant::now()
            .checked_add(WAIT)
            .expect("a deadline");
        while !reader.is_finished() {
            pass();
            assert!(
                std::time::Instant::now() < deadline,
                "the read was not answered"
            );
            std::thread::yield_now();
        }
        reader.join().expect("the reader")
    })
}

/// `record` of the app `app` as the person reads it from what `services` publish.
fn published_reading(
    services: &mut tilo_service::Services,
    app: &str,
    record: &str,
) -> Option<Result<ResourceCopy, ActionCallError>> {
    let link = crate::app_wire::Link::new(Arc::new(|| {}));
    let address = tilo_actions::remote::ResourceAddress {
        workspace: "studio".to_owned(),
        app: app.to_owned(),
        record: record.to_owned(),
    };
    let person = Caller::local_person("person");
    answered(
        || link.serve(services),
        super::records::published_read(&*link, &person, &address),
    )
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
            &mut services,
            tilolang_tilo::platform::STYLES_APP,
            crate::device_lists::STYLES_RESOURCE
        ),
        Some(Ok(ResourceCopy::reading(offered, None))),
        "the reading its service publishes"
    );
    // A device resource no service of this device serves is published empty, as the shell's
    // parts are handed it: a program reads its fields as absent and goes on.
    assert_eq!(
        published_reading(&mut services, tilolang_tilo::platform::DEVICE_APP, "power"),
        Some(Ok(ResourceCopy::reading(serde_json::json!({}), None))),
        "a resource no service serves is published empty"
    );
    // The session's own roots (`device.windows`) are published as its own app's.
    assert_eq!(
        published_reading(
            &mut services,
            tilolang_tilo::platform::DEVICE_APP,
            "session:windows",
        ),
        Some(Ok(ResourceCopy::reading(serde_json::json!({}), None))),
        "a resource of the session's own, read as it stands here"
    );
    // A view names the device's own settings by the device's key, read at this device's.
    let section = "devices/{device}/settings/appearance";
    assert_eq!(
        super::records::published_here(tilo_actions::remote::settings_resources::APP, section),
        Some(tilolang_tilo::platform::on_device(
            section,
            &tilo_app::framework::settings_device()
        )),
        "the device's key is this device's"
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
        published_reading(&mut services, "so.tilo.notes", "notes.notebook"),
        None,
        "no reading of an app's record"
    );
    // Nor is a session resource asked of an app that does not publish it.
    assert_eq!(
        published_reading(
            &mut services,
            tilo_actions::remote::settings_resources::APP,
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
    let Err(woken) = woken.wait_timeout(std::time::Duration::from_millis(50)) else {
        panic!("a pass at the revision the follow already saw wakes none");
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
#[expect(
    clippy::too_many_lines,
    reason = "one test's whole flow, from its setup to what it proves"
)]
fn a_live_reading_that_changes_reaches_its_follower() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tilo_action_calls::read_resources;
    use tilo_actions::remote::{ResourceAddress, ResourceAsk, ResourceWake};

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
    fn pass(
        services: &mut tilo_service::Services,
        session: &SessionServices,
        link: &crate::app_wire::Link,
    ) {
        if services.poll() {
            session.readings_moved(services.revision());
        }
        link.serve(services);
    }

    let offered = Arc::new(std::sync::Mutex::new(serde_json::json!({"offered": []})));
    let moved = Arc::new(AtomicBool::new(false));
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    let link = crate::app_wire::Link::new(Arc::new(|| {}));
    session.keep_published(Arc::clone(&link) as _);
    let mut services = tilo_service::Services::default();
    services.add(Box::new(LiveStyles {
        offered: Arc::clone(&offered),
        moved: Arc::clone(&moved),
    }));
    let records = super::records::ServiceRecords {
        state: None,
        session: Arc::clone(&session),
    };
    let caller = Caller::local_person("person");
    let asks = [ResourceAsk {
        address: ResourceAddress {
            workspace: "studio".to_owned(),
            app: tilolang_tilo::platform::STYLES_APP.to_owned(),
            record: crate::device_lists::STYLES_RESOURCE.to_owned(),
        },
        joined: false,
    }];
    let first = answered(
        || pass(&mut services, &session, &link),
        read_resources(&records, &caller, &asks, None, tilo_clock::now()),
    )
    .expect("the reading the device publishes");
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
    // The loop's pass, where the services are: what moved wakes the follows waiting on it, and
    // every reading a connection asks for is answered from the services as this pass reads them.
    // The device moves its list once the follow watches, and the pass that takes the move in
    // wakes it.
    let mut moved_device = false;
    let changed = answered(
        || {
            pass(&mut services, &session, &link);
            let woken = session
                .readings_woken
                .lock_recover()
                .iter()
                .any(ResourceWake::waiting);
            if !moved_device && woken {
                *offered.lock().unwrap() = serde_json::json!({"offered": ["warm"]});
                moved.store(true, Ordering::SeqCst);
                moved_device = true;
            }
        },
        read_resources(
            &records,
            &caller,
            &asks,
            Some(&first.revision),
            tilo_clock::now().checked_add(WAIT).expect("a deadline"),
        ),
    )
    .expect("the changed reading");
    assert!(moved_device, "the follow watched its reading");
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

/// A program's reading of a resource the session publishes is the session's to answer only for the
/// person at this device, who reads their own: an agent's read goes to the device's daemon, which
/// decides it as the agent, and never to the session's copy. With no daemon to reach, the agent
/// reads none of it, and the session is never asked.
#[test]
fn a_reading_is_the_callers_only_where_the_read_is_theirs() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tilo_actions::remote::ResourceReads as _;
    let asked = Arc::new(AtomicUsize::new(0));
    let link = crate::app_wire::Link::new({
        let asked = Arc::clone(&asked);
        Arc::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
        })
    });
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    session.keep_published(Arc::clone(&link) as _);
    let records = super::records::ServiceRecords {
        state: None,
        session: Arc::clone(&session),
    };
    let offered = serde_json::json!({"offered": []});
    let mut services = tilo_service::Services::default();
    services.add(Box::new(Styles {
        offered: offered.clone(),
    }));
    let address = |app: &str, record: &str| tilo_actions::remote::ResourceAddress {
        workspace: "studio".to_owned(),
        app: app.to_owned(),
        record: record.to_owned(),
    };
    let styles = address(
        tilolang_tilo::platform::STYLES_APP,
        crate::device_lists::STYLES_RESOURCE,
    );
    let person = Caller::local_person("person");
    assert_eq!(
        answered(|| link.serve(&mut services), records.read(&person, &styles)),
        Ok(ResourceCopy::reading(offered, None)),
        "the person's own device"
    );
    // A root no service of this device serves reads empty for the person, as the shell's views
    // are handed it, rather than absent.
    let power = address(tilolang_tilo::platform::DEVICE_APP, "power");
    assert_eq!(
        answered(|| link.serve(&mut services), records.read(&person, &power)),
        Ok(ResourceCopy::reading(serde_json::json!({}), None)),
        "no service, an empty reading"
    );
    let before = asked.load(Ordering::SeqCst);
    let read = tilo_rt::block_on(records.read(
        &Caller::local_agent("helper", "person").in_workspace("studio"),
        &styles,
    ))
    .expect("a runtime to read on");
    assert!(
        matches!(&read, Err(ActionCallError::Unavailable { reason })
            if reason == tilo_actions::remote::RESOURCE_READS_UNAVAILABLE),
        "an agent's read goes to the device's daemon, which this session reaches none of: {read:?}"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        before,
        "the session's copy is never asked for an agent"
    );
}

/// A catalogue is asked as a workspace: what it lists is decided by that workspace's grants. A
/// workspace this session is founding on its own service answers nothing yet, so the ask waits for
/// the founding and is delivered once it lands — never refused for a workspace this device is
/// making. A person's very first session asks for it while their first workspace is still being
/// founded, and a refusal there leaves them with no actions at all.
#[test]
fn a_catalogue_asked_as_a_workspace_being_founded_waits_for_it() {
    let session = Arc::new(SessionServices::new(Arc::new(|| {}), "person"));
    let host = Host::new(None, Arc::clone(&session));
    let workspace = "6e86cad3-c941-404a-9a19-8ac0ffbe908d";
    session.keep_founding(std::collections::BTreeSet::from([workspace.to_owned()]));
    let caller = Caller::local_person("person").in_workspace(workspace);
    let mut page = host.query(&caller, CatalogueQuery::default());
    assert!(
        matches!(
            page.poll_ready(),
            tilo_actions::remote::PendingPoll::Waiting
        ),
        "an ask of a workspace this session is founding waits"
    );
    assert_eq!(
        session.held_catalogues.lock_recover().len(),
        1,
        "the catalogue is held, not refused"
    );
    // The founding landed: the ask is delivered, and answered by what this session reaches.
    session.keep_founding(std::collections::BTreeSet::new());
    session.release_founding();
    assert!(
        session.held_catalogues.lock_recover().is_empty(),
        "the held catalogue is delivered once the workspace is the service's"
    );
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        match page.poll_ready() {
            tilo_actions::remote::PendingPoll::Waiting => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the delivered catalogue was never answered"
                );
                std::thread::yield_now();
            }
            tilo_actions::remote::PendingPoll::Ready(answer) => {
                assert!(
                    matches!(answer, Err(ActionCallError::Unavailable { .. })),
                    "a session reaching no dispatch answers no apps' catalogue: {answer:?}"
                );
                return;
            }
            tilo_actions::remote::PendingPoll::Abandoned => {
                panic!("the held catalogue was dropped, not answered")
            }
        }
    }
}

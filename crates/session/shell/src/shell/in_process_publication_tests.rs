// Native publication in the session's workspace room, with the person's own
// records (Browser's history and address book) in the person's own room: the
// identity host signs the launch room, and the person room only for the
// person records the launched app's surface declares.

fn authorized_person_session(
    profile: &std::path::Path,
) -> (crate::runtime_host::Session, InProcessApps, String) {
    std::fs::create_dir_all(profile).unwrap();
    let session = crate::runtime_host::Session::start(profile).unwrap();
    let room = session.room.clone();
    let socket = session.broker.capnp_socket().to_path_buf();
    let identity = session.identity_socket().to_path_buf();
    let apps = InProcessApps::at(&profile.join("apps"), &session.actor)
        .unwrap()
        .reaching_broker(Rc::new(move |app, room| {
            session_connector(socket.clone(), identity.clone(), app, room)
        }));
    (session, apps, room)
}

fn wait_native_directory(
    shown: &mut Shown,
    broker: &crate::shell::collab_broker::AppActionBroker,
    app_id: &str,
) -> String {
    let mut provider = None;
    until(
        &mut [shown],
        "the app publishes its native directory",
        || {
            provider = broker
                .state()
                .action_directory_root_page(None, 16)
                .roots
                .into_iter()
                .find(|root| root.app_id == app_id)
                .map(|root| root.peer_id.to_string());
            provider.is_some()
        },
    );
    provider.unwrap()
}

fn native_directory_query(
    shown: &mut Shown,
    host: &crate::shell::remote_actions::BrokerActionHost,
    caller: &tilo_actions::remote::Caller,
    provider: &str,
    needle: &str,
) -> tilo_actions::remote::CataloguePage {
    let mut pending = tilo_actions::remote::ActionHost::query(
        host,
        caller,
        tilo_actions::remote::CatalogueQuery {
            provider: Some(provider.to_owned()),
            needle: needle.into(),
            ..Default::default()
        },
    );
    let mut answer = None;
    until(
        &mut [shown],
        "the native directory answers",
        || match pending.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(result) => {
                answer = Some(result.unwrap());
                true
            }
            tilo_actions::remote::PendingPoll::Waiting => false,
            tilo_actions::remote::PendingPoll::Abandoned => {
                panic!("native directory query abandoned")
            }
        },
    );
    answer.unwrap()
}

fn has_native_operation(page: &tilo_actions::remote::CataloguePage, operation: &str) -> bool {
    page.rows.iter().any(|row| {
        row.action
            .as_ref()
            .is_some_and(|action| action.address.action == operation)
    })
}

#[test]
fn authorized_browser_directory_waits_for_primary_and_person_documents() {
    const TEST: &str = "shell::in_process::tests::authorized_browser_directory_waits_for_primary_and_person_documents";
    let Some(profile) = isolated_session_root(TEST) else {
        return;
    };
    let (session, mut apps, room) = authorized_person_session(&profile);
    let mut shown = Shown::mount(&mut apps, browser::definition(), &room, 93, |_| {});
    let embedded = shown
        .tile
        .as_any_mut()
        .unwrap()
        .downcast_mut::<ui::app::EmbeddedApp>()
        .unwrap();
    assert_eq!(
        embedded.component_root_mut().unwrap().record_owners().len(),
        3,
        "the original Browser must retain its history, address-book and imported-bookmarks owners"
    );
    let provider = wait_native_directory(
        &mut shown,
        &session.broker.app_action_broker().unwrap(),
        browser::definition().app_id,
    );
    let host = crate::shell::remote_actions::BrokerActionHost::new(
        session.broker.app_action_broker().unwrap(),
    );
    let caller = tilo_actions::remote::Caller::local_person(&session.actor).in_room(&room);
    let exact = native_directory_query(&mut shown, &host, &caller, &provider, "read-page");
    let app_name = native_directory_query(&mut shown, &host, &caller, &provider, "Browser");
    drop(host);
    assert!(has_native_operation(&exact, "read-page"), "{exact:?}");
    // Named-directory matching differs from the eager machine catalogue.
    // Keep the original diagnostic assertion; both queries share one caller.
    assert!(
        !has_native_operation(&app_name, "read-page"),
        "{app_name:?}"
    );
}

#[test]
fn authorized_store_directory_publishes_inspect_catalogue() {
    const TEST: &str =
        "shell::in_process::tests::authorized_store_directory_publishes_inspect_catalogue";
    let Some(profile) = isolated_session_root(TEST) else {
        return;
    };
    let (session, mut apps, room) = authorized_person_session(&profile);
    let definition = tilo_app::framework::definition::<store::StoreShell>();
    let mut shown = Shown::mount(&mut apps, definition, &room, 94, |_| {});
    let provider = wait_native_directory(
        &mut shown,
        &session.broker.app_action_broker().unwrap(),
        definition.app_id,
    );
    let host = crate::shell::remote_actions::BrokerActionHost::new(
        session.broker.app_action_broker().unwrap(),
    );
    let caller = tilo_actions::remote::Caller::local_person(&session.actor).in_room(&room);
    let exact = native_directory_query(&mut shown, &host, &caller, &provider, "inspect-catalogue");
    drop(host);
    assert!(
        has_native_operation(&exact, "inspect-catalogue"),
        "{exact:?}"
    );
}

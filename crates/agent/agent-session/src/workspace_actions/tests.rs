use super::{GaveUp, Needs, sealed_for};
use std::collections::{BTreeSet, HashSet};
use tilo_peer_state::document_frame::DocumentRoute;

const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
const OTHER_WORKSPACE: &str = "0b7f1d2e-5a6c-4d3b-8e9f-a0b1c2d3e4f5";

fn route(workspace: &str, app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: workspace.into(),
        document: record.into(),
    }
}

/// A failure the caller may ask again is the invocation's answer as it stands, so whoever
/// follows the invocation learns to ask again; one that settles its answer, and the same id
/// keeps saying that.
#[test]
fn a_retryable_failure_leaves_the_id_free_to_run_again() {
    let failure = |retryable| super::ActionFailure {
        code: "commit_failed".into(),
        message: "the host did not save the change: the workspace refused the change: \
                  workspace membership changed"
            .into(),
        details: None,
        retryable,
    };
    assert!(
        !super::outcome::settles(&failure(true)),
        "asking again runs it again, over the session the workspace holds now"
    );
    assert!(
        super::outcome::settles(&failure(false)),
        "a failure of its own is its answer"
    );
    let invocation = tilo_actions::InvocationId::new();
    let remembered = tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: failure(true),
    };
    assert!(remembered.asks_again(), "a follower is told to ask again");
    let answered = tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: failure(false),
    };
    assert!(!answered.asks_again(), "a failure of its own is the answer");
}

/// A read is no invocation of the workspace: a listing leaves it out, while
/// a write that changed nothing is still one the log remembers.
#[test]
fn a_read_is_no_invocation_of_the_workspace() {
    use tilo_actions::InvocationId;
    use tilo_actions::api::ActionAddress;
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    let caller = tilo_actions::remote::Caller::local_person("ada");
    let workspace = super::WorkspaceId::from_u128(7);
    let request = |action: &str| InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: format!(
                "{}:so.tilo.notes:notes.page",
                tilo_actions::remote::RESOURCE_PROVIDER
            ),
            address: ActionAddress::new("notes.page", action),
        },
        argument: InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let read = request("read");
    let write = request("append-paragraph");
    let elsewhere = request("append-paragraph");
    let mut log = super::InvocationLog::default();
    for asked in [&read, &write, &elsewhere] {
        assert!(log.insert(&caller, asked.clone()));
    }
    for (asked, at, reads) in [
        (&read, workspace, true),
        (&write, workspace, false),
        (&elsewhere, super::WorkspaceId::from_u128(8), false),
    ] {
        if let Some(entry) = log.entries.get_mut(&asked.invocation) {
            entry.workspace = Some(at);
            entry.reads = reads;
        }
    }
    let listed = super::invocations::listed_calls(&mut log, workspace);
    assert_eq!(
        listed.len(),
        1,
        "the read and another workspace's call are not its invocations"
    );
    assert!(
        listed[0].0.is_none(),
        "a call that changed nothing keeps the step it computed"
    );
}

/// A full log makes room by forgetting what settled, never an invocation
/// still to run or running: forgotten, it would never run, or leave its
/// caller no answer, and a retry under its id would run it twice.
#[test]
fn a_full_log_forgets_only_settled_invocations() {
    use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
    use tilo_actions::{InvocationId, InvocationUpdate};
    let caller = tilo_actions::remote::Caller::local_person("ada");
    let request = || InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "tilo.notes:page".into(),
            address: tilo_actions::api::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let mut log = super::InvocationLog::default();
    let pending = request().invocation;
    let first = InvokeRequest {
        invocation: pending,
        ..request()
    };
    assert!(log.insert(&caller, first));
    for _ in 1..super::invocation_log::REMEMBERED {
        assert!(log.insert(&caller, request()));
    }
    assert!(
        !log.insert(&caller, request()),
        "with every invocation unsettled, a new one is refused, not one of them dropped"
    );
    assert!(
        log.entries.contains_key(&pending),
        "the pending write is kept"
    );

    let settled = log.order[1];
    if let Some(entry) = log.entries.get_mut(&settled) {
        entry.update = InvocationUpdate::Succeeded {
            invocation: settled,
            output: tilo_actions::ActionOutput::default(),
        };
    }
    assert!(log.insert(&caller, request()), "a settled one makes room");
    assert!(
        !log.entries.contains_key(&settled),
        "the settled one is forgotten"
    );
    assert!(
        log.entries.contains_key(&pending),
        "the oldest, still pending, is kept"
    );
}

#[test]
fn a_request_waits_only_for_the_keys_of_what_it_reads() {
    let workspace = tilo_server_protocol::WorkspaceId::parse(WORKSPACE).unwrap();
    let b = route(WORKSPACE, "so.tilo.sheets", "b");
    let elsewhere = route(OTHER_WORKSPACE, "so.tilo.sheets", "a");
    // `a` opened; `b` here and the record elsewhere wait for their keys.
    let routes = BTreeSet::from([b.clone(), elsewhere]);
    let act = |record| Needs::Act {
        app_id: "so.tilo.sheets",
        record,
    };
    let read = |record| Needs::Read {
        app_id: "so.tilo.sheets",
        record,
    };
    let fresh = GaveUp::default();

    assert!(
        sealed_for(&routes, workspace, act("a"), &fresh).is_empty(),
        "another record's missing keys never hold back this one"
    );
    assert!(sealed_for(&routes, workspace, read("a"), &fresh).is_empty());
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert_eq!(
        sealed_for(&routes, workspace, read("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert!(
        sealed_for(
            &routes,
            workspace,
            Needs::Act {
                app_id: "so.tilo.slides",
                record: "b"
            },
            &fresh
        )
        .is_empty(),
        "a record is named by its app and id together"
    );
    assert_eq!(
        sealed_for(&routes, workspace, Needs::Workspace, &fresh),
        std::slice::from_ref(&b),
        "the first listing waits for this workspace's sealed records only"
    );
    assert!(sealed_for(&routes, workspace, Needs::Joined, &fresh).is_empty());

    let waited = GaveUp {
        listed: true,
        records: HashSet::from([b.clone()]),
    };
    assert!(
        sealed_for(&routes, workspace, Needs::Workspace, &waited).is_empty(),
        "later listings wait for nothing"
    );
    assert!(
        sealed_for(&routes, workspace, read("b"), &waited).is_empty(),
        "a listing waits for the same missing keys once"
    );
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &waited),
        [b],
        "acting on a record always waits for its keys"
    );
}

/// A write answered pending ends as its save does: done once the host holds
/// it, failed with the host's reason when it refuses it, and failed when the
/// session ends before the host held it, and failed (not to be asked again
/// blindly: it may still be saved) when the host says nothing in time, so
/// whoever follows it always hears how it went. Until then it cannot be
/// cancelled: it ran.
#[test]
fn a_pending_write_settles_as_its_save_does() {
    use std::sync::{Arc, Mutex, mpsc};
    use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
    use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
    let caller = tilo_actions::remote::Caller::local_person("ada");
    let request = || InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "tilo.notes:page".into(),
            address: tilo_actions::api::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let log = Arc::new(Mutex::new(super::InvocationLog::default()));
    // Each write: its save's outcome, sent, dropped (`None`) or never said
    // (`Some(None)` holds the answer back), and how it settled.
    let mut kept = Vec::new();
    let mut settle = |outcome: Option<Option<Result<(), String>>>| {
        let asked = request();
        let invocation = asked.invocation;
        {
            let mut held = log.lock().unwrap();
            assert!(held.insert(&caller, asked));
            held.entries.get_mut(&invocation).unwrap().saving = true;
        }
        let (answer, saved) = tilo_session_client::answer();
        let (settled, heard) = mpsc::channel();
        let shared = Arc::clone(&log);
        super::publish::Unsaved::waiting(saved).settle_within(
            std::time::Duration::from_millis(200),
            move |saved| {
                super::publish::settle_saved(&shared, invocation, ActionOutput::default(), saved);
                let _ = settled.send(());
            },
        );
        match outcome {
            Some(Some(outcome)) => {
                let _ = answer.send(outcome);
            }
            Some(None) => kept.push(answer),
            None => drop(answer),
        }
        heard
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the save's outcome settles the write");
        let (saving, update) = log
            .lock()
            .unwrap()
            .entries
            .get(&invocation)
            .map(|entry| (entry.saving, entry.update.clone()))
            .unwrap();
        assert!(!saving, "a settled write is saving no more");
        update
    };

    assert!(
        matches!(
            settle(Some(Some(Ok(())))),
            InvocationUpdate::Succeeded { .. }
        ),
        "a write the host holds is done"
    );
    let refused = settle(Some(Some(Err("workspace membership changed".into()))));
    assert!(
        matches!(&refused, InvocationUpdate::Failed { error, .. }
            if error.message.contains("workspace membership changed") && !error.retryable),
        "a refused write fails for good, with its reason: {refused:?}"
    );
    let ended = settle(None);
    assert!(
        matches!(&ended, InvocationUpdate::Failed { error, .. }
            if error.message.contains("session ended") && error.retryable),
        "a write whose session ended unsaved fails: {ended:?}"
    );
    let silent = settle(Some(None));
    assert!(
        matches!(&silent, InvocationUpdate::Failed { error, .. }
            if !error.retryable && error.message.contains("read the record")),
        "a write the host never confirms fails in time, not to be made twice: {silent:?}"
    );
    assert_eq!(
        kept.len(),
        1,
        "the silent save's answer stayed open throughout"
    );
}

/// A provider names a record only as `record:<app>:<record>`: anything else
/// names none, so a request under it acts on no record. The workspace is the
/// one the caller is in, never one the provider could name.
#[test]
fn a_provider_names_a_record_only_in_the_record_form() {
    let rows: [(&str, Option<(&str, &str)>); 8] = [
        ("record:so.tilo.notes:n1", Some(("so.tilo.notes", "n1"))),
        // App ids have no colon; a record id may.
        (
            "record:so.tilo.sheets:uuid:6f1c",
            Some(("so.tilo.sheets", "uuid:6f1c")),
        ),
        ("record:so.tilo.notes", None),
        ("records:so.tilo.notes:n1", None),
        ("Record:so.tilo.notes:n1", None),
        ("so.tilo.notes:n1", None),
        ("workspace:6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11", None),
        ("", None),
    ];
    for (provider, expected) in rows {
        let expected = expected.map(|(app, record)| route(WORKSPACE, app, record));
        assert_eq!(
            super::resource_of(provider, WORKSPACE),
            expected,
            "{provider:?}"
        );
    }
    let named = route(WORKSPACE, "so.tilo.sheets", "uuid:6f1c");
    let provider = super::record_provider(&named);
    assert_eq!(provider, "record:so.tilo.sheets:uuid:6f1c");
    assert_eq!(super::resource_of(&provider, WORKSPACE), Some(named));
    assert_eq!(
        super::resource_of(&provider, OTHER_WORKSPACE),
        Some(route(OTHER_WORKSPACE, "so.tilo.sheets", "uuid:6f1c")),
        "the workspace is the caller's, whatever record the provider names"
    );
}

/// What a request waits for follows from what it names: running an operation
/// of a record waits for that record's keys as an act, a listing of one as a
/// read, and a request naming no record never waits for one.
#[test]
fn a_request_waits_for_the_record_its_provider_names() {
    use super::keys::{Keys, catalogue_keys, invoke_keys};
    use tilo_actions::remote::{
        CatalogueQuery, InvokeRequest, RemoteActionTarget, ViewResourceQuery,
    };
    let invoke = |provider: &str| InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address: tilo_actions::api::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let listing = |provider: Option<&str>| CatalogueQuery {
        provider: provider.map(str::to_owned),
        ..CatalogueQuery::default()
    };
    let record = |act| Keys::Record {
        app_id: "so.tilo.notes".into(),
        record: "uuid:n1".into(),
        act,
    };
    let named = "record:so.tilo.notes:uuid:n1";
    let no_document = "record:so.tilo.notes";

    assert_eq!(invoke_keys(&invoke(named)), record(true));
    assert_eq!(invoke_keys(&invoke(no_document)), Keys::Joined);
    assert_eq!(
        invoke_keys(&invoke(&format!("workspace:{WORKSPACE}"))),
        Keys::Joined
    );
    assert_eq!(catalogue_keys(&listing(Some(named))), record(false));
    assert_eq!(catalogue_keys(&listing(Some(no_document))), Keys::Workspace);
    assert_eq!(catalogue_keys(&listing(None)), Keys::Workspace);
    let view = CatalogueQuery {
        view_resource: Some(ViewResourceQuery {
            id: "notes.page/p1".into(),
            kind: "notes.page".into(),
        }),
        ..listing(Some(named))
    };
    assert_eq!(
        catalogue_keys(&view),
        Keys::Joined,
        "a view lookup reads through the host's records link"
    );
}

/// A record's keys are waited for as an act when it is acted on, however the
/// request named it, and as a read when it is only listed; a request that
/// names no record waits for none either way.
#[test]
fn a_records_keys_are_waited_for_as_an_act_or_a_read() {
    use super::keys::Keys;
    let record = |act| Keys::Record {
        app_id: "so.tilo.notes".into(),
        record: "uuid:n1".into(),
        act,
    };
    let (act, read) = (record(true), record(false));
    assert!(
        matches!(
            act.needs(),
            Needs::Act {
                app_id: "so.tilo.notes",
                record: "uuid:n1"
            }
        ),
        "{:?}",
        act.needs()
    );
    assert!(
        matches!(
            read.needs(),
            Needs::Read {
                app_id: "so.tilo.notes",
                record: "uuid:n1"
            }
        ),
        "{:?}",
        read.needs()
    );
    assert!(
        matches!(
            read.act(),
            Needs::Act {
                app_id: "so.tilo.notes",
                record: "uuid:n1"
            }
        ),
        "acting on a record that was only listed waits for its keys as an act: {:?}",
        read.act()
    );
    assert!(
        matches!(
            act.read(),
            Needs::Read {
                app_id: "so.tilo.notes",
                record: "uuid:n1"
            }
        ),
        "{:?}",
        act.read()
    );
    assert!(matches!(Keys::Joined.act(), Needs::Joined));
    assert!(matches!(Keys::Workspace.read(), Needs::Workspace));
}

#[test]
fn files_opens_its_workspace_record_and_lists_no_tree_beside_it() {
    // The page's Files, which is what a workspace mounts for the app: the
    // computer's file manager reads the device's disk instead.
    let files = file_explorer::page_definition();
    assert!(
        super::opens_as_its_own(files, ""),
        "the workspace's own Files record"
    );
    let tree = collab_types::person_document_id(fs_protocol::tree::TREE_KIND);
    assert!(
        !super::opens_as_its_own(files, &tree),
        "the tree of the workspace's folder is read where it is kept, never mounted as Files'"
    );
    for chunk in [1, 2] {
        let record = fs_protocol::tree::chunk_document_id(chunk);
        assert!(
            !super::opens_as_its_own(files, &record),
            "chunk {chunk} of the tree is the folder too, read record by record where it is kept"
        );
    }
}

#[test]
fn a_chain_beside_the_workspace_record_is_the_apps_own() {
    let chat = chat::definition();
    assert!(
        super::opens_as_its_own(chat, ""),
        "the workspace's own Chat record"
    );
    let part = chat::parts::part(0).route().to_owned();
    assert!(
        super::opens_as_its_own(chat, &part),
        "a part of the chat's messages is the chat's own record: {part}"
    );
}

#[test]
fn an_apps_index_is_not_one_of_its_records() {
    // An app that keeps no record per workspace keeps its index of the workspace's named
    // records at the empty route, where Files keeps its own record.
    fn per_window(app: &'static str) -> ui::application::ApplicationDefinition {
        fn no_component(
            _: ui::application::ProcessContext,
            _: ui::application::LaunchTarget,
            _: ui::application::RestoreInput,
        ) -> Result<ui::application::AppComponent, ui::launch::LaunchError> {
            Err(ui::launch::LaunchError::Operation(
                "the factory must not run".into(),
            ))
        }
        ui::application::ApplicationDefinition {
            app_id: app.into(),
            title: "Index probe".into(),
            icon: ui::widget::IconDescriptor::Glyph("P"),
            resource_type: ui::resource::ResourceType::new("probe"),
            required: vec![],
            optional: vec![],
            opens: &[],
            workspace_resource: false,
            opens_before_resource: false,
            create: no_component,
        }
    }
    let app = per_window("so.tilo.test");
    assert!(
        !super::opens_as_its_own(&app, ""),
        "the index of the workspace's records is read where it is kept, never mounted as the app's"
    );
    let named = chat::parts::part(0).route().to_owned();
    assert!(
        super::opens_as_its_own(&app, &named),
        "a named record is the app's own: {named}"
    );
}

/// A caller's replica set aside while its write is saved comes back only
/// once the host holds the write: the watch hears saved when the host saves
/// it, and not saved when the host refuses it or nobody settles it.
#[test]
fn a_set_aside_replica_waits_for_its_write_to_be_saved() {
    let wait = std::time::Duration::from_secs(10);
    let watched = |outcome: Option<Result<(), String>>| {
        let (answer, saved) = tilo_session_client::answer();
        let mut unsaved = super::publish::Unsaved::waiting(saved);
        let watch = unsaved.watched();
        unsaved.settle(|_| {});
        match outcome {
            Some(outcome) => {
                let _ = answer.send(outcome);
            }
            None => drop(answer),
        }
        watch.wait(wait)
    };
    assert!(watched(Some(Ok(()))), "saved");
    assert!(!watched(Some(Err("refused".into()))), "refused");
    assert!(!watched(None), "the session ended first");
    let (_answer, saved) = tilo_session_client::answer();
    let mut unsaved = super::publish::Unsaved::waiting(saved);
    let watch = unsaved.watched();
    drop(unsaved);
    assert!(!watch.wait(wait), "never settled counts as not saved");
}

/// A caller's replica whose write outlived the host's grace is kept while
/// the write is saved and handed back to the caller's next action once it
/// is: the same replica, never one opened again over a record that grows
/// with every change it holds. Before the host holds the write, or when it
/// never does, nothing is handed back: nothing unsaved is acted on.
#[test]
fn a_replica_kept_for_a_slow_save_comes_back_to_the_caller_never_reopened() {
    use std::sync::Arc;
    use std::time::Duration;

    let wait = Duration::from_millis(50);
    let key = (
        route(WORKSPACE, "so.tilo.tasks", ""),
        "ada".into(),
        None::<String>,
        None,
    );
    // The replica the caller acted through, identified by the one token
    // inside it: one opened again would carry another.
    let replica = Arc::new(());
    let mut parked: super::bounded::Parked<Arc<()>> = super::bounded::Parked::default();
    let watch = super::publish::SaveWatch::new();
    parked.park(key.clone(), Arc::clone(&replica), Arc::clone(&watch));
    assert!(
        parked.unpark(&key, wait).is_none(),
        "not handed back before the host holds what it wrote"
    );

    // The caller asks again while the save is still in flight: the replica
    // waits, and is taken back once the host holds the write.
    let watch = super::publish::SaveWatch::new();
    parked.park(key.clone(), Arc::clone(&replica), Arc::clone(&watch));
    let tell = {
        let watch = Arc::clone(&watch);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            watch.set(true);
        })
    };
    let reused = parked
        .unpark(&key, Duration::from_secs(10))
        .expect("the replica is handed back once the write is saved");
    assert!(
        Arc::ptr_eq(&replica, &reused),
        "the very replica the caller acted through, never opened again"
    );
    tell.join().expect("the save was told");

    // A write the host refuses, or never confirms, is never acted on again:
    // the replica is let go and the next action opens what the workspace
    // holds.
    let rejecting = super::publish::SaveWatch::new();
    parked.park(key.clone(), Arc::clone(&replica), Arc::clone(&rejecting));
    rejecting.set(false);
    assert!(
        parked.unpark(&key, wait).is_none(),
        "a refused write is let go"
    );
    let unconfirmed = super::publish::SaveWatch::new();
    parked.park(key.clone(), Arc::clone(&replica), Arc::clone(&unconfirmed));
    assert!(
        parked.unpark(&key, wait).is_none(),
        "a write nobody confirmed is let go"
    );

    // Another caller's replica is never handed this caller's.
    let other = (
        route(WORKSPACE, "so.tilo.tasks", ""),
        "grace".into(),
        None::<String>,
        None,
    );
    let saved = super::publish::SaveWatch::new();
    saved.set(true);
    parked.park(key.clone(), Arc::clone(&replica), Arc::clone(&saved));
    assert!(parked.unpark(&other, wait).is_none());
    let again = parked
        .unpark(&key, wait)
        .expect("still there for the caller it was set aside for");
    assert!(Arc::ptr_eq(&replica, &again));
}

/// A typed call made through generated types asks the app service for the
/// surface the types were written for. A service that mounts the workspace's
/// apps itself answers from the signed build it carries, before the app's
/// first window: the workspace's PDF Viewer has no window and no member
/// holding its bytes, and the types are still checked against what it
/// publishes.
#[test]
fn a_surface_request_is_answered_from_the_app_the_service_mounts() {
    use std::sync::Arc;
    use std::time::Duration;
    use tilo_actions::remote::{Caller, RemoteActionMessage};

    let catalogue: super::CatalogueFactory = Arc::new(super::AppCatalogue::native);
    let service = super::WorkspaceActions::new(catalogue);
    let caller = Caller::local_person("ada");
    let asked = |app: &str| {
        let answer = service
            .serve(
                super::WorkspaceId::from_u128(1),
                &caller,
                RemoteActionMessage::SurfaceRequest {
                    correlation: 7,
                    app: app.to_owned(),
                },
            )
            .expect("the service answers a surface request");
        answer
            .wait_timeout(Duration::from_secs(1))
            .ok()
            .flatten()
            .expect("the answer is ready")
    };
    let surface = match asked("so.tilo.pdf-viewer") {
        RemoteActionMessage::SurfaceResponse {
            correlation: 7,
            result: Ok(Some(surface)),
        } => surface,
        other => panic!("the app's surface is answered: {other:?}"),
    };
    let (_, kept) = collab_types::surface::SurfaceFile::kept(include_bytes!(
        "../../../../../apps/pdf-viewer/surface.toml"
    ))
    .expect("the app's committed surface parses");
    assert_eq!(
        surface, kept,
        "the surface is the app's signed build's, in the form it is kept by"
    );
    // An app the service mounts none of has no surface to decide a change
    // against: it answers none rather than another app's.
    match asked("so.tilo.no-such-app") {
        RemoteActionMessage::SurfaceResponse {
            result: Ok(None), ..
        } => {}
        other => panic!("an app the service mounts none of has no surface: {other:?}"),
    }
}

/// A follow of an invocation that has not moved is held and answered once it
/// moves, or when its wait ends — not answered at once with what the caller
/// already saw, which would have it ask again and again.
#[test]
fn a_follow_is_held_until_the_invocation_moves() {
    use std::sync::Arc;
    use std::time::Duration;
    use tilo_actions::remote::{
        Caller, InvokeRequest, RemoteActionMessage, RemoteActionTarget, UpdateMark,
    };
    use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

    let catalogue: super::CatalogueFactory = Arc::new(super::AppCatalogue::native);
    let service = super::WorkspaceActions::new(catalogue);
    let caller = Caller::local_person("ada");
    let admitted = |invocation| {
        let request = InvokeRequest {
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: "tilo.notes:page".into(),
                address: tilo_actions::api::ActionAddress::new("notes.page", "append"),
            },
            argument: tilo_actions::remote::InvokeArgument::default(),
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        assert!(service.log().insert(&caller, request));
    };
    let follow = |invocation, seen: UpdateMark, wait_ms: u32| {
        service
            .serve(
                super::WorkspaceId::from_u128(1),
                &caller,
                RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: Some(wait_ms),
                    seen: Some(seen),
                },
            )
            .expect("the service answers an observe")
    };
    let state = |answer: RemoteActionMessage| match answer {
        RemoteActionMessage::Update {
            state: Some(state), ..
        } => state,
        other => panic!("a follow answers the invocation's state: {other:?}"),
    };

    // Already past what the caller saw: answered now.
    let invocation = InvocationId::new();
    admitted(invocation);
    let answered = follow(invocation, UpdateMark::Finished, 5_000)
        .wait_timeout(Duration::from_secs(1))
        .ok()
        .flatten()
        .expect("a follow already moved past answers at once");
    assert_eq!(
        UpdateMark::of(&state(answered).update),
        UpdateMark::Accepted
    );

    // Where the caller already is: held, not answered with what they saw.
    let held = follow(invocation, UpdateMark::Accepted, 5_000);
    let Err(held) = held.wait_timeout(Duration::from_millis(20)) else {
        panic!("a follow that has not moved answers at once");
    };

    // The invocation moves: the very follow that was waiting answers with
    // where it moved to.
    service.log().answered(
        invocation,
        &InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput::default(),
        },
        None,
    );
    let answered = held
        .wait_timeout(Duration::from_secs(1))
        .ok()
        .flatten()
        .expect("a follow answers once the invocation moved");
    assert!(state(answered).is_terminal());

    // Nothing moves: the follow answers by its own wait, with what it has.
    let quiet = InvocationId::new();
    admitted(quiet);
    let answered = follow(quiet, UpdateMark::Accepted, 10)
        .wait_timeout(Duration::from_secs(2))
        .ok()
        .flatten()
        .expect("a follow answers once its wait ends");
    assert_eq!(
        UpdateMark::of(&state(answered).update),
        UpdateMark::Accepted
    );
}

/// A write whose save outlives the thread's wait is answered pending — its
/// receipt says accepted, never done — and is done only once the server holds
/// it: nothing reads it as done before it is durable.
#[test]
fn a_write_answered_pending_is_done_only_once_it_is_saved() {
    use std::sync::{Arc, Barrier, Mutex};
    use tilo_actions::remote::{
        Caller, InvocationState, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    };
    use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

    let caller = Caller::local_person("ada");
    let invocation = InvocationId::new();
    let asking = |invocation| InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: "tilo.notes:page".into(),
            address: tilo_actions::api::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let log = Arc::new(Mutex::new(super::InvocationLog::default()));
    {
        let mut held = log.lock().unwrap();
        assert!(held.insert(&caller, asking(invocation)));
        held.entries.get_mut(&invocation).unwrap().saving = true;
    }
    let state = || {
        let held = log.lock().unwrap();
        InvocationState::new(
            invocation,
            held.entries.get(&invocation).unwrap().update.clone(),
        )
    };

    // The receipt the thread answers with when its wait ran out: accepted,
    // not done, and the invocation still open.
    let pending = state().receipt();
    assert!(
        matches!(pending.status, ReceiptStatus::Accepted),
        "a write still being saved is accepted, not done: {:?}",
        pending.status
    );
    assert!(!state().is_terminal(), "an unsaved write is not done");

    // The server's answer comes only once it holds the write; then, and not
    // before, the invocation is done.
    let (answer, saved) = tilo_session_client::answer();
    let unsaved = super::publish::Unsaved::waiting(saved);
    let settled = Arc::new(Barrier::new(2));
    let answered = Arc::clone(&settled);
    let shared = Arc::clone(&log);
    unsaved.settle(move |saved| {
        super::publish::settle_saved(&shared, invocation, ActionOutput::default(), saved);
        answered.wait();
    });
    assert!(!state().is_terminal(), "nothing said it is saved yet");
    let _ = answer.send(Ok(()));
    settled.wait();
    assert!(
        matches!(state().update, InvocationUpdate::Succeeded { .. }),
        "saved: the write is done"
    );

    // A save the server refuses is answered failed, and is never done.
    let refused = InvocationId::new();
    {
        let mut held = log.lock().unwrap();
        assert!(held.insert(&caller, asking(refused)));
        held.entries.get_mut(&refused).unwrap().saving = true;
    }
    let (answer, saved) = tilo_session_client::answer();
    let unsaved = super::publish::Unsaved::waiting(saved);
    let settled = Arc::new(Barrier::new(2));
    let answered = Arc::clone(&settled);
    let shared = Arc::clone(&log);
    unsaved.settle(move |saved| {
        super::publish::settle_saved(&shared, refused, ActionOutput::default(), saved);
        answered.wait();
    });
    let _ = answer.send(Err("workspace membership changed".into()));
    settled.wait();
    assert!(
        matches!(
            &log.lock().unwrap().entries.get(&refused).unwrap().update,
            InvocationUpdate::Failed { .. }
        ),
        "a refused save is failed, never done"
    );
}

/// A write the session never confirms is answered failed once its watch passed, with what the
/// session says it keeps and waits for: whoever asked reads why, not only that it is not saved.
#[test]
fn a_write_not_saved_within_its_watch_fails_with_what_the_session_keeps() {
    let (_answer, saved) = tilo_session_client::answer();
    let (settled, heard) = tilo_session_client::answer();
    super::publish::Unsaved::waiting(saved)
        .asked_by(|| {
            Some((
                "so.tilo.chat/agents, which waits for the document it changes, for 61 s".to_owned(),
                true,
            ))
        })
        .settle_within(std::time::Duration::from_millis(50), move |saved| {
            let _ = settled.send(saved);
        });
    let failure = heard
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the watch settles the write")
        .expect_err("a write nobody confirmed is not saved");
    assert_eq!(failure.code, "save_unconfirmed");
    assert!(!failure.retryable, "it may still be saved");
    assert!(
        failure
            .message
            .ends_with(": the session keeps so.tilo.chat/agents, which waits for the document it changes, for 61 s"),
        "{failure:?}"
    );
}

/// What a write carries is named for its log and History at a small fraction of what applying
/// it costs: a whole part a list of issues starts by its stored heads, never by loading it, and
/// a change by its chunk's header, never by decoding it. Loading the part to name its heads,
/// twice for every write, cost a list of 200 issues a third of a second on a hosted workspace.
#[test]
fn naming_a_write_costs_a_fraction_of_applying_it() {
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use collab_types::DocumentMutation;
    use std::time::{Duration, Instant};

    const PARA: &str = "The page should load in a second whatever the tracker holds, and a \
        click should answer within a frame. This paragraph stands in for what a real issue says. ";
    let list = |doc: &mut automerge::AutoCommit, first: usize| {
        let issues = match doc.get(automerge::ROOT, "issues").unwrap() {
            Some((_, id)) => id,
            None => doc
                .put_object(automerge::ROOT, "issues", automerge::ObjType::Map)
                .unwrap(),
        };
        for at in first..first + 200 {
            let issue = doc
                .put_object(&issues, format!("issue-{at}"), automerge::ObjType::Map)
                .unwrap();
            doc.put(&issue, "title", format!("ENG-{at} Fix the list"))
                .unwrap();
            let body = doc
                .put_object(&issue, "body", automerge::ObjType::Text)
                .unwrap();
            doc.splice_text(&body, 0, 0, &PARA.repeat(2 + at % 7))
                .unwrap();
            doc.commit();
        }
    };
    // The fastest of a few tries: what the work costs, not what the machine did meanwhile.
    let fastest =
        |work: &mut dyn FnMut() -> Duration| (0..3).map(|_| work()).min().unwrap_or(Duration::ZERO);
    let timed = |work: &mut dyn FnMut()| {
        let began = Instant::now();
        work();
        began.elapsed()
    };

    let mut doc = automerge::AutoCommit::new();
    list(&mut doc, 0);
    let whole = doc.save();
    let heads = doc.get_heads();
    list(&mut doc, 200);
    let change = doc.save_after(&heads);

    let short =
        |hash: &automerge::ChangeHash| hash.to_string().chars().take(12).collect::<String>();
    let snapshot = DocumentMutation::Snapshot(whole.clone());
    assert_eq!(
        super::publish::changes_of(&snapshot),
        heads.iter().map(short).collect::<Vec<_>>(),
        "a whole part is named by its heads"
    );
    let loading = fastest(&mut || {
        timed(&mut || {
            std::hint::black_box(automerge::AutoCommit::load(&whole).unwrap());
        })
    });
    let naming = fastest(&mut || {
        timed(&mut || {
            std::hint::black_box(super::publish::changes_of(&snapshot));
        })
    });
    assert!(
        naming * 10 <= loading,
        "naming a whole part took {naming:?}, loading it {loading:?}"
    );

    let incremental = DocumentMutation::Incremental(change.clone());
    let named = super::publish::changes_of(&incremental);
    assert_eq!(
        named.len(),
        200,
        "a list's change is named change by change"
    );
    assert_eq!(
        named.last(),
        doc.get_heads().first().map(short).as_ref(),
        "the last change named is the one the list ends at"
    );
    let applying = fastest(&mut || {
        let mut base = automerge::AutoCommit::load(&whole).unwrap();
        timed(&mut || {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test's own change, applied as the baseline naming it is measured against"
            )]
            base.load_incremental(&change).unwrap();
        })
    });
    let naming = fastest(&mut || {
        timed(&mut || {
            std::hint::black_box(super::publish::changes_of(&incremental));
        })
    });
    assert!(
        naming * 10 <= applying,
        "naming a list's change took {naming:?}, applying it {applying:?}"
    );
}

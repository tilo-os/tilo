//! A host without a screen acts through the app's own view: mounted over the
//! record, never drawn, every operation through the API a click takes, and
//! the change it made flowing back to the person's window.

use tilo_app::collaboration::DocumentMutation;
use tilo_app::framework::harness_device::HarnessDevice;
use tilo_app::framework::testing::TEST_WORKSPACE;
use tilo_app::framework::{AttachedView, definition};
use tilo_app::permissions::{KindSet, LOCAL_USER, TreePath};
use tilo_app::prelude::*;
use tilo_app::resource::DocumentCheckout;
use tilo_app::tilo_resource::profiling::observe;
use tilo_app_test_host::TestApp;

use crate::kinds::{Tally, TallyApp};

const AGENT: &str = "scribe";

fn writer() -> HarnessDevice {
    granted(&TreePath::root().app(TallyApp::ID), KindSet::READ_UNDOABLE)
}

/// The store's answer to the asked record a call was kept as, as its resend carries it: these
/// tests stand in for the device that answers.
fn approval() -> tilo_actions::remote::asked::Approval {
    tilo_actions::remote::asked::Approval::new(
        tilo_actions::remote::asked::RecordRef {
            workspace: TEST_WORKSPACE.to_owned(),
            record: format!("invocations/{}", tilo_actions::InvocationId::new().0),
            device: None,
        },
        None,
    )
}

fn attached(app: &TestApp<TallyApp>, device: HarnessDevice) -> AttachedView {
    let definition = definition::<TallyApp>();
    AttachedView::open(
        definition,
        TEST_WORKSPACE,
        DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
        app.read_document(|doc| doc.clone().save()),
        &MemberId::person(AGENT),
        device,
        false,
    )
    .unwrap()
}

/// The device of the workspace's owner acting through the agent, granted exactly `kinds` at
/// `node`.
fn granted(node: &TreePath, kinds: KindSet) -> HarnessDevice {
    HarnessDevice::agent_granted(
        TEST_WORKSPACE,
        &MemberId::person(LOCAL_USER),
        AGENT,
        std::slice::from_ref(node),
        kinds,
    )
}

tilo_test_env::scenario! {
    fn an_agent_acts_through_the_view_and_is_recorded_as_itself;

    phase an_agent_acts_through_the_view_and_the_window_sees_it;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        let mut view = attached(&app, writer());
        assert!(
            view.take_changes().unwrap().is_none(),
            "mounting writes nothing"
        );

        let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
        assert!(
            view.operations()
                .iter()
                .any(|operation| operation.address == bump),
            "the view offers what a person's window offers"
        );
        view.invoke(&bump, &serde_json::Map::new(), None).unwrap();
        assert!(view.landed().is_some(), "the action ran in the view");
        let changes = view
            .take_changes()
            .unwrap()
            .expect("the agent's own change");
        assert!(
            view.take_changes().unwrap().is_none(),
            "nothing is taken twice"
        );

        assert!(
            matches!(changes, DocumentMutation::Incremental(_)),
            "an existing record answers with increments"
        );
        let (DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) = changes;
        let mut peer = app.peer_document();
        tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
        app.merge_peer(&mut peer).unwrap();
        assert_eq!(app.state().count, 1);
        let version = app.versions().into_iter().last().unwrap();
        assert_eq!(version.context.unwrap().invoker.unwrap().acting.by, MemberId::person(AGENT).into());
    }

    phase history_names_the_agent_its_app_and_the_run_it_acts_in;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        let mut view = attached(&app, writer());
        view.set_context(Some(tilo_actions::ActionContext::default().with_group(
            "run-1",
            "Count",
            "agent-session",
        )));
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();
        view.set_context(None);
        assert_eq!(count(&view), 1);

        merge(&mut app, &mut view);
        assert_eq!(app.state().count, 1);
        let version = app.versions().into_iter().last().unwrap();
        let context = version.context.unwrap();
        let Some(tilo_actions::Did::Verb(verb)) = &context.what else {
            panic!("the agent's edit records its verb: {context:?}");
        };
        assert_eq!(verb.action.action, "bump");
        assert_eq!(context.invoker.unwrap().acting.by, MemberId::person(AGENT).into());
        assert_eq!(context.app.unwrap().id, TallyApp::ID);
        assert_eq!(context.group.unwrap().id, "run-1");
    }

    phase an_agent_acting_for_a_person_is_recorded_as_such;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let definition = definition::<TallyApp>();
        let agent = tilo_actions::ActionInvoker::agent(AGENT, "ada")
            .acting_for("ada");
        let mut view = AttachedView::open_for(
            definition,
            TEST_WORKSPACE,
            DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
            app.read_document(|doc| doc.clone().save()),
            &agent,
            writer(),
            false,
        )
        .unwrap();
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();
        merge(&mut app, &mut view);
        let invoker = app
            .versions()
            .into_iter()
            .last()
            .and_then(|version| version.context)
            .and_then(|context| context.invoker)
            .unwrap();
        assert_eq!(invoker, agent, "the agent, for the person who asked");
    }

    phase several_typed_edits_apply_as_one_step;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        let before = app.versions().len();
        view.invoke_typed(
            &Invoke::group([Tally::RENAME.with("guests"), Tally::RENAME.with("members")]),
            None,
        )
        .unwrap();
        merge(&mut app, &mut view);
        assert_eq!(app.state().name, "members");
        assert_eq!(app.versions().len(), before + 1, "a group is one version");
    }

    phase a_run_is_recorded_in_the_coordinating_record_by_the_agent;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();
        let last = view
            .read(|doc| tilo_resource::history::versions(&mut doc.clone()))
            .unwrap()
            .into_iter()
            .last()
            .unwrap();
        let end = tilo_app::history_source::entry_id_from_hash(&last.hash.to_string());
        let steps = [tilo_app::InvocationStep {
            app: TallyApp::ID.into(),
            record: String::new(),
            range: tilo_app::ChangeRange::single(end),
            what: vec![tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new(TallyApp::ID, "document", "bump"),
                [],
            ))],
        }];
        assert!(view.record_invocation("Count", None, None, &steps));
        assert!(!view.record_invocation("Nothing", None, None, &[]));

        merge(&mut app, &mut view);
        let runs = app.read_document(tilo_app::read_invocations);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs.first().unwrap().label, "Count");
    }
}

tilo_test_env::scenario! {
    fn the_attached_view_refuses_what_its_member_may_not_do;

    phase the_view_refuses_what_the_member_may_not_do;
    {
        let app = TestApp::<TallyApp>::new().unwrap();
        let reader =
            granted(&TreePath::root().app(TallyApp::ID), KindSet::READ);
        let mut view = attached(&app, reader);
        let bump = tilo_actions::ActionAddress::new("test.tally", "bump");
        assert!(view.invoke(&bump, &serde_json::Map::new(), None).is_err());
        assert!(view.take_changes().unwrap().is_none());
    }

    phase a_reader_is_offered_no_edit;
    {
        let app = TestApp::<TallyApp>::new().unwrap();
        let reader =
            granted(&TreePath::root().app(TallyApp::ID), KindSet::READ);
        let mut view = attached(&app, reader);
        assert!(view.operations().is_empty());
        // Refused as an edit the agent may not make, never as one the app
        // does not have: an agent told that looks for another way.
        let refused = view.invoke_typed(&Tally::BUMP.invoke(), None).map(drop);
        assert!(
            matches!(
                &refused,
                Err(tilo_actions::api::ApiError::Unavailable { code, .. })
                    if code == "permission_denied"
            ),
            "{refused:?}"
        );
        assert_eq!(count(&view), 0);
    }

    phase a_grant_of_one_action_reaches_that_action_only;
    {
        use tilo_permissions::policy::Level;
        let app = TestApp::<TallyApp>::new().unwrap();
        let only_bump = granted(&TreePath::root()
                .app(TallyApp::ID)
                .any(Level::Kind)
                .resource("test.tally")
                .action("bump"), KindSet::READ_UNDOABLE);
        let mut view = attached(&app, only_bump);
        let actions: Vec<String> = view
            .operations()
            .into_iter()
            .map(|operation| operation.address.action)
            .collect();
        // The named action, and the ways any value of it is invoked: a typed edit, a group of
        // them, a field edit. Each admits only the values the grant reaches.
        assert_eq!(actions, ["bump", "apply", "group", "field-edit"]);
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();
        assert_eq!(count(&view), 1);
        assert!(
            view.invoke_typed(&Tally::RENAME.with("guests"), None)
                .is_err()
        );
        assert_eq!(count(&view), 1);
    }

    phase a_view_that_writes_as_it_opens_a_joined_record_is_refused;
    // Only a record's creator writes its initial content: a view that writes
    // as it opens a record its member joined is refused, naming the app,
    // instead of keeping a change no peer would ever be sent.
    {
        let app = TestApp::<BenchApp>::new().unwrap();
        let definition = definition::<BenchApp>();
        let open = |created| {
            AttachedView::open(
                definition,
                TEST_WORKSPACE,
                DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
                app.read_document(|doc| doc.clone().save()),
                &MemberId::person(AGENT),
                HarnessDevice::member_of(TEST_WORKSPACE, &MemberId::person(AGENT), tilo_app::Role::Owner),
                created,
            )
        };
        let Err(refusal) = open(false) else {
            panic!("a joined bench opened although its view sat the agent down");
        };
        assert!(refusal.contains(BenchApp::ID), "{refusal}");
        assert!(refusal.contains("`sit`"), "{refusal}");
        assert!(refusal.contains("joined"), "{refusal}");

        // The record's creator writes what it starts with.
        assert!(open(true).is_ok());

        // A view that writes nothing on open joins as before.
        let tally = TestApp::<TallyApp>::new().unwrap();
        assert!(attached(&tally, writer()).take_changes().unwrap().is_none());
    }
}

tilo_test_env::scenario! {
    fn a_member_changes_through_the_view_only_what_its_grant_names;

    phase a_member_who_may_edit_is_answered_in_every_form;
    // The forms the phases below ask in, first for a member who may edit:
    // each is carried out, so where one is refused below it is the grant
    // that refuses it and not the way it was asked.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        let mut view = attached(&app, writer());
        let mut answers = asked(&mut view, "rename", &Tally::RENAME.with("guests"));
        answers.extend(asked(&mut view, "bump", &Tally::BUMP.invoke()));
        for (form, answer) in answers {
            assert!(
                answer.is_ok(),
                "{form} is carried out for a member who may edit: {answer:?}"
            );
        }
        assert_eq!(name(&view), "guests");
        assert_eq!(count(&view), 5, "one for each form");
        merge(&mut app, &mut view);
        assert_eq!(app.state().name, "guests");
        assert_eq!(app.state().count, 5);
    }

    phase an_agent_granted_read_is_refused_every_edit_and_changes_nothing;
    // What a view lists to its member and what it carries out for it are
    // decided apart. An agent that may only read asks for each edit in every
    // form: each is refused, a confirmation turns no refusal into a yes, the
    // tally is as it was and no change leaves the view.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        let reader =
            granted(&TreePath::root().app(TallyApp::ID), KindSet::READ);
        let mut view = attached(&app, reader);
        let mut answers = asked(&mut view, "rename", &Tally::RENAME.with("guests"));
        answers.extend(asked(&mut view, "bump", &Tally::BUMP.invoke()));
        all_refused(answers, "an agent that may only read");
        assert_eq!(name(&view), "visitors");
        assert_eq!(count(&view), 0);
        assert!(
            view.take_changes().unwrap().is_none(),
            "no change leaves the view"
        );
        merge(&mut app, &mut view);
        assert_eq!(app.state().name, "visitors");
        assert_eq!(app.state().count, 0);
    }

    phase an_agent_granted_one_action_does_that_one_and_no_other;
    {
        use tilo_permissions::policy::Level;
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        let only_bump = granted(&TreePath::root()
                .app(TallyApp::ID)
                .any(Level::Kind)
                .resource("test.tally")
                .action("bump"), KindSet::READ_UNDOABLE);
        let mut view = attached(&app, only_bump);
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();
        all_refused(
            asked(&mut view, "rename", &Tally::RENAME.with("guests")),
            "an agent granted bump alone",
        );
        assert_eq!(name(&view), "visitors");
        assert_eq!(count(&view), 1, "the one action it was granted, once");
        merge(&mut app, &mut view);
        assert_eq!(app.state().name, "visitors");
        assert_eq!(app.state().count, 1);
    }

    phase a_person_who_only_views_is_listed_no_edit_and_refused_every_one;
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        app.invoke(Tally::RENAME.with("visitors")).unwrap();
        // The view's member as a person of the workspace who only views.
        let viewer =
            HarnessDevice::member_of(TEST_WORKSPACE, &MemberId::person(AGENT), tilo_app::Role::Viewer);
        let mut view = attached(&app, viewer);
        assert!(
            !view
                .operations()
                .iter()
                .any(|operation| matches!(operation.address.action.as_str(), "rename" | "bump")),
            "a person's view lists only what they may do"
        );
        let mut answers = asked(&mut view, "rename", &Tally::RENAME.with("guests"));
        answers.extend(asked(&mut view, "bump", &Tally::BUMP.invoke()));
        all_refused(answers, "a person who only views");
        assert_eq!(name(&view), "visitors");
        assert_eq!(count(&view), 0);
        merge(&mut app, &mut view);
        assert_eq!(app.state().name, "visitors");
        assert_eq!(app.state().count, 0);
    }
}

tilo_test_env::scenario! {
    fn an_attached_host_collects_only_its_own_landed_changes;

    phase a_change_that_cannot_be_collected_fails_and_goes_with_the_next_call;
    // A host answers a write by what it collects of it: a change that could not
    // be collected is a failure, never "nothing changed", and it is not lost.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        view.invoke_typed(&Tally::BUMP.invoke(), None).unwrap();

        let taken = view.while_resources_busy(AttachedView::take_changes).unwrap();
        let error = taken.expect_err("a change that could not be collected is not 'nothing changed'");
        assert!(error.contains("could not be collected"), "{error}");

        merge(&mut app, &mut view);
        assert_eq!(app.state().count, 1, "the change goes with the next call");
    }

    phase a_peers_change_is_refused_while_own_changes_cannot_be_set_aside;
    // A peer's change that arrives while this member's own cannot be set
    // aside is refused, leaving the record as it was: its own change still goes
    // out, the peer's merges once asked again.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        view.invoke_typed(&Tally::RENAME.with("ours"), None)
            .unwrap();
        app.invoke(Tally::BUMP.invoke()).unwrap();
        let theirs = DocumentMutation::Snapshot(app.read_document(|doc| doc.clone().save()));

        let merged = view.while_resources_busy(|view| view.merge(&theirs)).unwrap();
        assert!(merged.is_err(), "the peer's change waits");
        assert_eq!(count(&view), 0, "nothing of the peer's was merged");

        assert!(view.merge(&theirs).unwrap(), "merged once asked again");
        assert_eq!(count(&view), 1);
        merge(&mut app, &mut view);
        assert_eq!(
            app.state().name,
            "ours",
            "the member's own change still goes"
        );
    }

    phase changes_of_another_record_are_left_alone;
    {
        use tilo_app::automerge::transaction::Transactable as _;
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        let mut other = tilo_resource::Doc::new();
        other
            .put(tilo_app::automerge::ROOT, "name", "elsewhere")
            .unwrap();
        other.commit();
        let first = other.get_heads();
        other
            .put(tilo_app::automerge::ROOT, "count", 9_i64)
            .unwrap();
        other.commit();
        let unrelated = other.save_after(&first);
        assert!(
            !view
                .merge(&DocumentMutation::Incremental(unrelated))
                .unwrap()
        );
        assert_eq!(count(&view), 0);

        app.invoke(Tally::BUMP.invoke()).unwrap();
        let ours = app.read_document(|doc| doc.clone().save());
        assert!(view.merge(&DocumentMutation::Snapshot(ours)).unwrap());
        assert_eq!(count(&view), 1);
    }

    phase an_action_answers_without_waiting_for_the_view;
    // An action answers once its own change is made, never after a frame of
    // the view: what the view makes of it (a reply it asks a worker for in
    // turn) is made when the view is next read, whoever reads it, and that
    // frame waits for the reply it asked for, so the reader sees it all.
    {
        let app = TestApp::<LogApp>::new().unwrap();
        let definition = definition::<LogApp>();
        let mut view = AttachedView::open(
            definition,
            TEST_WORKSPACE,
            DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
            app.read_document(|doc| doc.clone().save()),
            &MemberId::person(AGENT),
            HarnessDevice::member_of(TEST_WORKSPACE, &MemberId::person(AGENT), tilo_app::Role::Owner),
            false,
        )
        .unwrap();
        let answer = |view: &AttachedView| {
            view.read(|doc| {
                <Log as tilo_resource::Resource>::read(doc)
                    .unwrap()
                    .answer
            })
            .unwrap()
        };
        let mut before = String::new();
        for entry in ["lamps", "chairs"] {
            let (_, acting) = observe(|| view.invoke_typed(&Log::ASK.with(entry), None).unwrap());
            assert_eq!(frames(&acting), 0, "the action lays nothing out");
            assert_eq!(answer(&view), before, "reading the record lays nothing out");
            assert!(view.take_changes().unwrap().is_some(), "the ask leaves alone");
            let _ = view.resources();
            assert_eq!(answer(&view), format!("{entry}: found"));
            assert!(view.take_changes().unwrap().is_some(), "then its answer");
            assert!(view.take_changes().unwrap().is_none());
            before = format!("{entry}: found");
        }
        let did: Vec<String> = view
            .read(|doc| tilo_resource::history::versions(&mut doc.clone()))
            .unwrap()
            .into_iter()
            .filter_map(|version| match version.context?.what? {
                tilo_actions::Did::Verb(verb) => Some(verb.action.action),
                _ => None,
            })
            .collect();
        assert_eq!(did, ["ask", "answer", "ask", "answer"]);
    }

    phase two_pages_of_one_profile_write_one_record_without_sharing_a_sequence;
    // Two pages of one profile open one record: each is a writer of its own, so
    // their changes never share a writer's sequence. Two changes under one
    // writer and sequence cannot both be kept — the record holds the one that
    // reached it first and builds on it alone — so a record two pages wrote has
    // to merge from both, with every change its own.
    {
        let app = TestApp::<TallyApp>::new().unwrap();
        let saved = app.read_document(|doc| doc.clone().save());
        let definition = definition::<TallyApp>();
        let open = || {
            AttachedView::open(
                definition,
                TEST_WORKSPACE,
                DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
                saved.clone(),
                &MemberId::person(AGENT),
                writer(),
                false,
            )
            .unwrap()
        };
        let mut first = open();
        let mut second = open();
        let mut written = Vec::new();
        for (page, name) in [
            (&mut first, "from the first page"),
            (&mut second, "from the second page"),
        ] {
            page.invoke_typed(&Tally::RENAME.with(name), None).unwrap();
            let Some(DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) =
                page.take_changes().unwrap()
            else {
                panic!("a page that wrote takes its change");
            };
            written.push(bytes);
        }
        // What the workspace does with what both pages sent: one history.
        let mut record = tilo_app::automerge::AutoCommit::load(&saved).unwrap();
        for bytes in &written {
            tilo_automerge::ApplyIncoming::load_incoming(&mut record, bytes).unwrap();
        }
        let changes = record.get_changes(&[]);
        let writers: std::collections::HashSet<_> = changes
            .iter()
            .map(|change| (change.actor_id().to_bytes(), change.seq()))
            .collect();
        assert_eq!(
            writers.len(),
            changes.len(),
            "one change per writer and sequence, whichever page wrote it"
        );
        assert_eq!(
            changes.len(),
            5,
            "the record's founding, its step to its version, its first values and both pages' \
             changes are in it"
        );
        // Both writes are in what the workspace keeps: neither page's change is
        // the one a record holding two of a sequence leaves out.
        let name = <Tally as tilo_resource::Resource>::read(&record)
            .unwrap()
            .name;
        for written in ["from the first page", "from the second page"] {
            assert!(
                name.contains(written),
                "both pages' writes are in the record: {name:?}"
            );
        }
    }
}

tilo_test_env::scenario! {
    fn an_attached_host_does_the_work_a_read_needs_once;

    phase merged_changes_are_laid_out_once_when_the_view_is_next_read;
    // A host merges every change its record takes, a write's own echo among
    // them, and reads the view far less often: the view lays them out once,
    // when it is read, and a change it already held moves nothing.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        let mut peers = Vec::new();
        for _ in 0..3 {
            app.invoke(Tally::BUMP.invoke()).unwrap();
            peers.push(DocumentMutation::Snapshot(
                app.read_document(|doc| doc.clone().save()),
            ));
        }
        let (merged, merging) = observe(|| {
            peers
                .iter()
                .map(|peer| view.merge(peer).unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(merged, [true, true, true]);
        assert_eq!(frames(&merging), 0, "merging lays nothing out");

        let (_, reading) = observe(|| view.resources());
        assert_eq!(frames(&reading), 1, "the read lays all three out at once");
        assert_eq!(count(&view), 3, "the record holds every change");
        let (_, again) = observe(|| view.resources());
        assert_eq!(frames(&again), 0, "and only once");

        let echo = peers.last().unwrap();
        let (merged, merging) = observe(|| view.merge(echo).unwrap());
        assert!(!merged, "a change the record holds moves nothing");
        let (_, reading) = observe(|| view.resources());
        assert_eq!(frames(&merging), 0, "nothing to lay out");
        assert_eq!(frames(&reading), 0, "nor to read");
    }

    phase one_tree_answers_every_read_until_something_moves;
    // A call resolves a path a level at a time, finds the chains the view
    // keeps and checks what it offers: each reads the tree the view
    // publishes, which is built once until the view or its record moves.
    {
        let mut app = TestApp::<TallyApp>::new().unwrap();
        let mut view = attached(&app, writer());
        let (first, building) = observe(|| {
            (tree(&view), view.chains().len(), view.operations().len())
        });
        assert!(
            publications(&building) <= 1,
            "the tree is built once for all three: {building:?}"
        );
        let (again, reading) = observe(|| tree(&view));
        assert_eq!(publications(&reading), 0, "and read again as it was");
        assert_eq!(again, first.0);

        view.invoke_typed(&Tally::RENAME.with("visitors"), None)
            .unwrap();
        let renamed = tree(&view);
        assert_ne!(renamed, first.0, "what the view did is published");
        merge(&mut app, &mut view);
        app.invoke(Tally::RENAME.with("guests")).unwrap();
        let theirs = DocumentMutation::Snapshot(app.read_document(|doc| doc.clone().save()));
        view.merge(&theirs).unwrap();
        assert_ne!(tree(&view), renamed, "a peer's change is published");
    }
}

/// The tree `view` publishes, each resource by id, label and actions, to
/// compare.
fn tree(view: &AttachedView) -> Vec<String> {
    fn walk(resource: &tilo_actions::ResourceSpec, into: &mut Vec<String>) {
        let actions: Vec<&str> = resource
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        into.push(format!(
            "{} {} {}",
            resource.id.as_str(),
            resource.label,
            actions.join(",")
        ));
        for child in &resource.children {
            walk(child, into);
        }
    }
    let mut into = Vec::new();
    for resource in view.resources() {
        walk(&resource, &mut into);
    }
    into
}

fn frames(phases: &[tilo_app::tilo_resource::profiling::Sample]) -> usize {
    phases
        .iter()
        .filter(|phase| phase.phase == "ui.frame_sync")
        .map(|phase| phase.calls)
        .sum()
}

fn publications(phases: &[tilo_app::tilo_resource::profiling::Sample]) -> usize {
    phases
        .iter()
        .filter(|phase| phase.phase == "framework.publication")
        .map(|phase| phase.calls)
        .sum()
}

/// What the view did and has not sent yet, merged into the person's window.
fn merge(app: &mut TestApp<TallyApp>, view: &mut AttachedView) {
    let Some(DocumentMutation::Incremental(bytes) | DocumentMutation::Snapshot(bytes)) =
        view.take_changes().unwrap()
    else {
        return;
    };
    let mut peer = app.peer_document();
    tilo_automerge::ApplyIncoming::load_incoming(&mut peer, &bytes).unwrap();
    app.merge_peer(&mut peer).unwrap();
}

fn count(view: &AttachedView) -> i64 {
    view.read(|doc| <Tally as tilo_resource::Resource>::read(doc).unwrap().count)
        .unwrap()
}

/// The tally's name as `view` holds it.
fn name(view: &AttachedView) -> String {
    view.read(|doc| <Tally as tilo_resource::Resource>::read(doc).unwrap().name)
        .unwrap()
}

/// What the member is answered when it asks through `view` for `action`, the
/// edit `typed` is: by its address, confirmed and not, by its typed
/// reference, as a group of one and as an applied action.
fn asked(
    view: &mut AttachedView,
    action: &str,
    typed: &Invoke<Tally>,
) -> Vec<(String, Result<(), tilo_actions::api::ApiError>)> {
    let address = tilo_actions::ActionAddress::new("test.tally", action);
    let group = Invoke::group([typed.clone()]);
    let applied = Invoke::<Tally>::apply(&typed.build(0).unwrap()).unwrap();
    vec![
        (
            format!("{action} by its address"),
            view.invoke(&address, &typed.arguments, None).map(drop),
        ),
        (
            format!("{action} by its address, confirmed"),
            view.invoke(&address, &typed.arguments, Some(&approval()))
                .map(drop),
        ),
        (
            format!("{action} by its typed reference"),
            view.invoke_typed(typed, Some(&approval())).map(drop),
        ),
        (
            format!("{action} in a group"),
            view.invoke_typed(&group, Some(&approval())).map(drop),
        ),
        (
            format!("{action} as an applied action"),
            view.invoke_typed(&applied, Some(&approval())).map(drop),
        ),
    ]
}

/// Every answer of `answers` is a refusal, and none an ask that a
/// confirmation would turn into a yes.
fn all_refused(answers: Vec<(String, Result<(), tilo_actions::api::ApiError>)>, who: &str) {
    for (form, answer) in answers {
        assert!(
            !matches!(
                answer,
                Ok(()) | Err(tilo_actions::api::ApiError::ConfirmationRequired)
            ),
            "{form} is refused to {who}: {answer:?}"
        );
    }
}

/// A bench whose view sits whoever opens it down: the mistake the attached
/// view refuses in a record its member joined.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bench {
    pub seated: i64,
}

#[resource(
    kind = "test.bench",
    app = "so.tilo.test-bench",
    catalogue = "tests/locales"
)]
impl Bench {
    /// Sit down.
    #[action]
    pub fn sit(&mut self) {
        self.seated = self.seated.saturating_add(1);
    }
}

pub struct BenchApp;

impl ResourceApp for BenchApp {
    type Resource = Bench;
    const ID: &'static str = "so.tilo.test-bench";
    const TITLE: &'static str = "Bench";

    fn view(cx: &mut ViewCx<Bench>) -> Element {
        cx.dispatcher().submit(Bench::SIT.invoke());
        Box::new(cx.button("Sit", Bench::SIT.invoke()))
    }

    fn label(_: &Bench) -> String {
        "Bench".into()
    }
}

/// A log whose view looks up what is asked for off the UI thread, as a
/// viewer reads the markup a file already carries, and records the answer
/// once it lands.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub asked: String,
    pub answer: String,
}

#[resource(
    kind = "test.entries",
    app = "so.tilo.test-entries",
    catalogue = "tests/locales"
)]
impl Log {
    /// Ask for an entry.
    #[action]
    pub fn ask(&mut self, entry: String) {
        self.asked = entry;
    }

    /// Record what looking an entry up answered.
    #[action]
    pub fn answer(&mut self, answer: String) {
        self.answer = answer;
    }
}

/// The log's view: asks a worker for each entry asked for, owing the
/// window its reply, and records the reply in the frame after it lands.
struct Lookup {
    dispatcher: tilo_app::framework::Dispatcher<Log>,
    asked: String,
    replies: Option<std::sync::mpsc::Receiver<String>>,
    rect: Rect,
}

impl Widget for Lookup {
    fn sync(&mut self, state: &tilo_app::widget::AppModel, path: &tilo_app::widget::ModelPath) {
        // An answer is taken up before anything is asked, so the answer to
        // what this frame asks lands after it, in a frame of its own.
        if let Some(answer) = self
            .replies
            .as_ref()
            .and_then(|replies| replies.try_recv().ok())
        {
            self.replies = None;
            self.dispatcher.submit(Log::ANSWER.with(answer));
        }
        let (doc, _) = tilo_app::doc::document_ref(state, path);
        let Ok(log) = <Log as tilo_resource::Resource>::read(doc) else {
            return;
        };
        if log.asked == self.asked {
            return;
        }
        self.asked.clone_from(&log.asked);
        let (reply, replies) = tilo_app::wake::waking_channel();
        let owed = tilo_app::wake::WakeHandle::new().owe();
        std::thread::spawn(move || {
            let _ = reply.send(format!("{}: found", log.asked));
            drop(owed);
        });
        self.replies = Some(replies);
    }

    fn build_layout(
        &mut self,
        taffy: &mut tilo_app::advanced::TaffyTree<()>,
        _: &Theme,
        _: &mut std::collections::HashMap<tilo_app::advanced::NodeId, *mut dyn Widget>,
    ) -> tilo_app::advanced::NodeId {
        taffy.new_leaf(tilo_app::taffy::Style::default()).unwrap()
    }

    fn apply_layout(
        &mut self,
        _: &tilo_app::advanced::TaffyTree<()>,
        _: tilo_app::advanced::NodeId,
        origin: Point,
    ) {
        self.rect = Rect::from_origin_size(origin, Size::ZERO);
    }

    fn name(&self) -> &'static str {
        "Lookup"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

pub struct LogApp;

impl ResourceApp for LogApp {
    type Resource = Log;
    const ID: &'static str = "so.tilo.test-entries";
    const TITLE: &'static str = "Log";

    fn view(cx: &mut ViewCx<Log>) -> Element {
        Box::new(Lookup {
            dispatcher: cx.dispatcher(),
            asked: String::new(),
            replies: None,
            rect: Rect::ZERO,
        })
    }

    fn label(_: &Log) -> String {
        "Log".into()
    }
}

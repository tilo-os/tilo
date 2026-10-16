//! The session on its own (verbs, field edits, merging, the published
//! places) and as the app hosts it.

use super::*;
use crate::model::Resolution;
use ui::framework::testing::TestApp;
use ui::tilo_actions::Resource as Published;
use ui::tilo_resource::{Edit, Effect as _, Replica, Resource as _};

fn edit(session: &mut Replica<Session>, edit: &Edit<Session>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    session
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn conflict_merge() -> Invoke<Session> {
    Session::OPEN_MERGE.with(session::OpenMerge {
        base_label: "base".into(),
        base: "a\nb\nc\n".into(),
        ours_label: "ours".into(),
        ours: "a\nOURS\nc\n".into(),
        theirs_label: "theirs".into(),
        theirs: "a\nTHEIRS\nc\n".into(),
    })
}

/// A merge with exactly one real conflict, and a comparison, opened through
/// their verbs.
fn opened_merge() -> Replica<Session> {
    let mut session = Replica::<Session>::new();
    session.apply(conflict_merge()).unwrap();
    session
        .apply(Session::OPEN_COMPARISON.with(session::OpenComparison {
            left_label: "before.rs".into(),
            left: "a\nb\nc\n".into(),
            right_label: "after.rs".into(),
            right: "a\nB\nc\nnew\n".into(),
        }))
        .unwrap();
    session
}

fn conflict(session: &Replica<Session>) -> u64 {
    let index = session
        .state()
        .unwrap()
        .merge_model()
        .hunks
        .iter()
        .position(|h| h.resolution.is_conflict())
        .expect("one conflict");
    u64::try_from(index).unwrap()
}

fn children_of_kind<'a>(tree: &'a Published, kind: &str) -> Vec<&'a Published> {
    tree.children
        .iter()
        .filter(|child| child.kind == kind)
        .collect()
}

/// Opening a merge writes its inputs and a merged result with no side taken;
/// taking a side records it and recomposes the result; a hand edit stops
/// sides from being taken, and an unknown change is refused.
#[test]
fn opening_and_resolving_a_merge_goes_through_the_record() {
    let mut session = opened_merge();
    let hunk = conflict(&session);
    assert!(session.state().unwrap().merged.contains("OURS"));

    session
        .apply(Session::TAKE_THEIRS.with(session::TakeTheirs { hunk }))
        .unwrap();
    let state = session.state().unwrap();
    assert!(
        state.merged.contains("THEIRS") && !state.merged.contains("OURS"),
        "{:?}",
        state.merged
    );
    let index = usize::try_from(hunk).unwrap();
    assert_eq!(state.resolutions(), vec![(index, HunkChoice::Theirs)]);
    assert!(state.composed(&state.merge_model()));
    assert_eq!(
        state.merge_model().hunks[index].resolution,
        Resolution::TookTheirs
    );

    assert!(
        session
            .apply(Session::TAKE_OURS.with(session::TakeOurs { hunk: 9999 }))
            .is_err()
    );

    edit(
        &mut session,
        &Session::paths().merged.replace("hand made\n"),
    )
    .unwrap();
    assert!(
        session
            .apply(Session::TAKE_OURS.with(session::TakeOurs { hunk }))
            .is_err(),
        "a hand edit is never overwritten"
    );
    assert_eq!(session.state().unwrap().merged, "hand made\n");
    let tree = session.published().unwrap();
    assert!(
        children_of_kind(&tree, "diffmerge.session.change").is_empty(),
        "no side is offered once the result was edited"
    );
    assert!(
        edit(
            &mut session,
            &Session::paths().resolutions.key("0").set(HunkChoice::Both)
        )
        .is_err(),
        "only taking a side records one"
    );

    session.apply(conflict_merge()).unwrap();
    let state = session.state().unwrap();
    assert!(
        state.resolutions().is_empty(),
        "a new merge starts unresolved"
    );
    assert!(state.composed(&state.merge_model()));
}

/// A custom resolution replaces exactly its change; taking both stacks the
/// sides; each is one undoable change.
#[test]
fn a_change_takes_both_sides_or_custom_text() {
    let mut session = opened_merge();
    let hunk = conflict(&session);
    session
        .apply(Session::TAKE_BOTH.with(session::TakeBoth { hunk }))
        .unwrap();
    assert_eq!(session.state().unwrap().merged, "a\nOURS\nTHEIRS\nc\n");
    session.apply(conflict_merge()).unwrap();
    session
        .apply(Session::RESOLVE.with(session::Resolve {
            hunk,
            text: "CUSTOM".into(),
        }))
        .unwrap();
    assert_eq!(session.state().unwrap().merged, "a\nCUSTOM\nc\n");
}

/// Two people typing in the merged result converge character by
/// character; inputs opened at once end as one of the two, never a mix.
#[test]
fn concurrent_edits_converge() {
    let mut ours = opened_merge();
    let mut theirs = ours.fork();
    let merged = Session::paths().merged;
    edit(&mut ours, &merged.splice(0, 0, "// ours\n")).unwrap();
    edit(&mut theirs, &merged.splice(2, 0, "!")).unwrap();
    theirs
        .apply(Session::OPEN_COMPARISON.with(session::OpenComparison {
            left_label: "x".into(),
            left: "1\n".into(),
            right_label: "y".into(),
            right: "2\n".into(),
        }))
        .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let state = ours.state().unwrap();
    assert_eq!(state, theirs.state().unwrap());
    assert_eq!(state.merged, "// ours\na\n!OURS\nc\n");
    let comparison = state.comparison.unwrap();
    assert_eq!(
        (comparison.left_label.0.as_str(), comparison.left.0.as_str()),
        ("x", "1\n")
    );
}

/// The session publishes the merge's changes with content-based ids and
/// their sides bound, and the comparison's change blocks as bounded view
/// actions.
#[test]
fn the_session_publishes_changes_and_change_blocks() {
    let mut session = opened_merge();
    let tree = session.published().unwrap();
    let actions: Vec<&str> = tree.actions.iter().map(|a| a.id.as_str()).collect();
    for id in ["open-comparison", "open-merge", "field-edit"] {
        assert!(actions.contains(&id), "{id}: {actions:?}");
    }
    let changes = children_of_kind(&tree, "diffmerge.session.change");
    let [change] = changes.as_slice() else {
        panic!("one change, not {}", changes.len());
    };
    assert!(
        change.id.as_str().contains("conflict:"),
        "{}",
        change.id.as_str()
    );
    for id in ["take-ours", "take-theirs", "take-both", "resolve"] {
        let spec = change
            .actions
            .iter()
            .find(|a| a.id == id)
            .unwrap_or_else(|| panic!("{id} on the change"));
        assert!(
            spec.params().iter().all(|p| p.name != "hunk"),
            "{id} is bound to its change"
        );
    }

    let state = session.state().unwrap();
    let model = state.comparison.as_ref().unwrap().model();
    let comparison = children_of_kind(&tree, "diffmerge.session.comparison");
    let [comparison] = comparison.as_slice() else {
        panic!("the comparison is published");
    };
    assert_eq!(comparison.children.len(), model.blocks.len());
    for (index, child) in comparison.children.iter().enumerate() {
        let show = child.actions.iter().find(|a| a.id == "show").unwrap();
        assert_eq!(show.kind, ui::tilo_actions::OperationKind::View);
        let encoded = ui::testing::action_contract::spec_encoding(show, None);
        let fingerprint = diff_block_fingerprint(&model, index).unwrap();
        assert_eq!(
            DiffmergeEffect::from_payload(&encoded.payload).unwrap(),
            DiffmergeEffect::Show {
                block: u64::try_from(index).unwrap(),
                fingerprint: fingerprint.clone(),
            }
        );
        assert!(child.id.0.contains(&fingerprint));
    }

    // A change's bound side applies to that change.
    let take = change
        .actions
        .iter()
        .find(|a| a.id == "take-theirs")
        .unwrap();
    let encoded = take.build_json(&serde_json::Map::new()).unwrap();
    let action: <Session as ui::tilo_resource::Resource>::Action =
        serde_json::from_slice(&encoded.payload).unwrap();
    session.apply_action(&action).unwrap();
    assert!(session.state().unwrap().merged.contains("THEIRS"));
}

/// The sample merge has a conflict to resolve through the palette.
#[test]
fn the_sample_merge_exposes_a_conflict() {
    let mut session = Replica::<Session>::new();
    for invoke in sample_invocations() {
        session.apply(invoke).unwrap();
    }
    let tree = session.published().unwrap();
    assert!(
        children_of_kind(&tree, "diffmerge.session.change")
            .iter()
            .any(|change| change.label.starts_with("Conflict"))
    );
}

/// An agent opens, resolves and edits through the one published tree: each
/// change one call.
#[test]
fn an_agent_merges_one_call_each() {
    let mut app = TestApp::<DiffmergeShell>::new().unwrap();
    app.call(
        "open-merge",
        serde_json::json!({
            "base_label": "base", "base": "a\nb\nc\n",
            "ours_label": "ours", "ours": "a\nOURS\nc\n",
            "theirs_label": "theirs", "theirs": "a\nTHEIRS\nc\n",
        }),
    )
    .unwrap();
    app.settle();
    let surface = app.agent_surface();
    let change = surface
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["resource_kind"] == "diffmerge.session.change" && row["action"] == "take-both"
        })
        .and_then(|row| row["resource"].as_str())
        .expect("the change is published")
        .to_owned();
    app.call_on(&change, "take-both", serde_json::json!({}))
        .unwrap();
    assert_eq!(app.state().merged, "a\nOURS\nTHEIRS\nc\n");
    let edit = Session::paths().merged.replace("done\n");
    app.call("field-edit", serde_json::json!({ "edit": edit }))
        .unwrap();
    assert_eq!(app.state().merged, "done\n");
    assert!(app.undo(), "the edit is one undo step");
    assert_eq!(app.state().merged, "a\nOURS\nTHEIRS\nc\n");
}

/// What an agent does for a person is history the person reads: the app's
/// sentence for each field and verb.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<DiffmergeShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    app.call_by(
        agent.clone(),
        Session::KIND,
        "open-comparison",
        serde_json::json!({
            "left_label": "before.rs", "left": "a\n",
            "right_label": "after.rs", "right": "b\n",
        }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        Session::KIND,
        "open-merge",
        serde_json::json!({
            "base_label": "base.rs", "base": "a\nb\nc\n",
            "ours_label": "main.rs", "ours": "a\nOURS\nc\n",
            "theirs_label": "topic.rs", "theirs": "a\nTHEIRS\nc\n",
        }),
    )
    .unwrap();
    for (verb, args) in [
        ("take-ours", serde_json::json!({ "hunk": 1 })),
        ("take-theirs", serde_json::json!({ "hunk": 1 })),
        ("take-both", serde_json::json!({ "hunk": 1 })),
        ("resolve", serde_json::json!({ "hunk": 1, "text": "mine" })),
    ] {
        app.call_by(agent.clone(), Session::KIND, verb, args)
            .unwrap();
    }
    app.call_by(
        agent,
        Session::KIND,
        "field-edit",
        serde_json::json!({ "edit": Session::paths().merged.splice(0, 0, "x") }),
    )
    .unwrap();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Compared before.rs with after.rs",
            "Opened a merge of main.rs and topic.rs",
            "Took ours for a change",
            "Took theirs for a change",
            "Took both sides for a change",
            "Resolved a change by hand",
            "Edited the merged result",
        ]
    );
}

#[test]
fn twenty_megabyte_sources_open_through_the_actual_resource_and_stay_scrollable() {
    use std::time::Instant;
    let (left, right) = crate::diff_view::performance_tests::unique_sources();
    let bytes = left.len();
    let mut app = ui::framework::testing::HeadlessApp::<DiffmergeShell>::new().unwrap();
    let open = Session::OPEN_COMPARISON.with(session::OpenComparison {
        left_label: "large-before.rs".into(),
        left,
        right_label: "large-after.rs".into(),
        right,
    });
    let started = Instant::now();
    app.invoke(open.clone()).unwrap();
    let opened = started.elapsed();
    let state = app.state();
    assert_eq!(state.comparison.as_ref().unwrap().left.0.len(), bytes);
    let started = Instant::now();
    app.effect(&DiffmergeEffect::NextChange).unwrap();
    let jump = started.elapsed();
    let started = Instant::now();
    app.effect(&DiffmergeEffect::ShowMerge).unwrap();
    app.effect(&DiffmergeEffect::ShowDiff).unwrap();
    let switch = started.elapsed();
    let started = Instant::now();
    app.invoke(open).unwrap();
    let reopened = started.elapsed();
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(480.0, 400.0));
    let digests = crate::model::digest_passes();
    let started = Instant::now();
    for _ in 0..8 {
        app.harness().scroll(0.0, -3.0);
    }
    let scroll = started.elapsed();
    assert_eq!(
        crate::model::digest_passes(),
        digests,
        "ordinary wheel frames do not hash the unchanged 40 MB comparison"
    );
    assert_eq!(app.state().comparison.as_ref().unwrap().left.0.len(), bytes);
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "the real large comparison publishes its controls: {audit:#?}"
    );
    eprintln!(
        "hosted large comparison bytes={bytes} canonical_open_and_frame={opened:?} next_change_and_frame={jump:?} mode_round_trip={switch:?} repeated_open={reopened:?} eight_wheel_frames={scroll:?}"
    );
}

/// The header's controls are the shared widgets, reached the way a person
/// reaches them: the shell's header holds the mode switch, change
/// navigation and file actions, and pressing the mode switch changes
/// what the comparison shows.
#[test]
fn the_header_controls_drive_the_comparison_through_the_shell() {
    use ui::framework::testing::HeadlessApp;
    use ui::headless::{Control, ControlInput};

    let mut app = HeadlessApp::<DiffmergeShell>::capture(Some("diff")).expect("the diff stages");
    let find = |app: &mut HeadlessApp<DiffmergeShell>, name: &str| -> Option<Control> {
        app.harness()
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref().is_some_and(|n| n.starts_with(name)))
    };
    for name in [
        "Diff",
        "3-way merge",
        "Previous change",
        "Next change",
        "Load sample",
        "Open files",
    ] {
        assert!(find(&mut app, name).is_some(), "the header offers {name}");
    }
    assert!(
        find(&mut app, "Take ours for change").is_none(),
        "the diff is showing, not the merge"
    );

    let merge = find(&mut app, "3-way merge").expect("the mode switch");
    app.harness()
        .operate_control(&merge.id, &ControlInput::Press)
        .expect("the mode switch answers a press");
    app.harness().relayout();
    assert!(
        find(&mut app, "Take ours for change").is_some(),
        "choosing the 3-way merge shows its resolutions"
    );
    assert!(find(&mut app, "Diff between").is_none());
}

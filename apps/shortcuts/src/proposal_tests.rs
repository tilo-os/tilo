use serde_json::json;
use tilo_automation::Selector;
use ui::prelude::Replica;

use super::*;
use crate::model::Workbench;

/// One action the person took: `action` on `notes.folder`, mostly on Inbox.
fn taken(action: &str, resource: &str) -> Occurrence {
    Occurrence {
        app: "so.tilo.notes".into(),
        kind: "notes.folder".into(),
        action: action.into(),
        resource: Some(resource.into()),
        label: None,
    }
}

fn inbox(action: &str) -> Occurrence {
    taken(action, "notes.folder:inbox")
}

/// The same run of `steps` taken `times` in a row.
fn repeated(steps: &[&str], times: usize) -> Vec<Occurrence> {
    (0..times)
        .flat_map(|_| steps.iter().map(|action| inbox(action)))
        .collect()
}

fn log(lines: &str) -> crate::Observed {
    crate::Observed::from_log(lines)
}

#[test]
fn a_log_reads_into_the_actions_it_names() {
    let seen = log(concat!(
        "so.tilo.notes/notes.folder/list on notes.folder:inbox as List notes\n",
        "so.tilo.notes/notes.folder/create on notes.folder:inbox\n",
        "not an action\n",
    ));
    let invocations = seen.invocations();
    assert_eq!(
        invocations.len(),
        2,
        "the line that does not read is skipped"
    );
    assert_eq!(invocations[0].action, "list");
    assert_eq!(invocations[0].label.as_deref(), Some("List notes"));
    assert_eq!(
        invocations[1].resource.as_deref(),
        Some("notes.folder:inbox")
    );
    assert_eq!(invocations[1].label, None);
}

#[test]
fn work_taken_three_times_in_a_row_is_offered() {
    let proposal = detect(&repeated(&["list", "create"], 3)).expect("offered");
    assert_eq!(proposal.steps.len(), 2);
    assert_eq!(proposal.times, 3);
    assert_eq!(
        proposal.key(),
        "so.tilo.notes/notes.folder/list > so.tilo.notes/notes.folder/create"
    );
    assert_eq!(
        proposal.evidence(),
        "You did these 2 steps 3 times in a row.",
        "the evidence is what was noticed, in the person's words"
    );
}

#[test]
fn work_taken_twice_is_not_offered() {
    assert!(
        detect(&repeated(&["list", "create"], 2)).is_none(),
        "one repetition could be a coincidence"
    );
}

#[test]
fn a_person_who_stops_halfway_through_is_not_repeating_work() {
    // list, create, then list, create, then only list again: the run of the
    // last two is `create, list`, which was not taken before.
    let observed = vec![
        inbox("list"),
        inbox("create"),
        inbox("list"),
        inbox("create"),
        inbox("list"),
    ];
    assert!(detect(&observed).is_none());
}

#[test]
fn the_longest_run_is_offered_when_it_is_the_one_taken_most() {
    // list, create three times back to back: the whole pair is the work.
    let proposal = detect(&repeated(&["list", "create", "rename"], 3)).expect("offered");
    assert_eq!(
        proposal.steps.len(),
        3,
        "the whole sequence, not just its last step"
    );
}

#[test]
fn the_shortcut_names_its_steps_and_wants_the_first_resource_of_the_kind() {
    let catalog = crate::samples::catalog();
    let proposal = detect(&repeated(&["list", "create"], 3)).expect("offered");
    let name = proposal.name(&catalog);
    assert_eq!(name, "List notes, then New note", "the catalogue's words");
    let program = proposal.program();
    assert_eq!(program.steps.len(), 2);
    let StepKind::Invoke { resource, args, .. } = &program.steps[0].kind else {
        panic!("an invocation");
    };
    assert!(
        matches!(resource, Selector::FirstOfKind(kind) if kind == "notes.folder"),
        "the work, not the folder it happened in: {resource:?}"
    );
    assert!(args.is_empty(), "the person fills them in");
}

#[test]
fn similar_resources_repeating_the_same_work_are_offered() {
    // Tidying Inbox, then Archive, then Inbox again: two folders, one habit.
    let observed = vec![
        taken("list", "notes.folder:inbox"),
        taken("create", "notes.folder:inbox"),
        taken("list", "notes.folder:archive"),
        taken("create", "notes.folder:archive"),
        taken("list", "notes.folder:inbox"),
        taken("create", "notes.folder:inbox"),
    ];
    let proposal = detect(&observed).expect("offered");
    assert_eq!(proposal.steps.len(), 2, "the run, not one of its steps");
    assert_eq!(proposal.times, 3);
    assert!(
        proposal.program().steps.iter().all(|step| matches!(
            &step.kind,
            StepKind::Invoke {
                resource: Selector::FirstOfKind(_),
                ..
            }
        )),
        "and it is offered for the kind, not for one of the folders"
    );
}

#[test]
fn a_draft_is_opened_with_a_name_the_checker_accepts() {
    let catalog = crate::samples::catalog();
    let proposal = detect(&repeated(&["list"], 3)).expect("offered");
    let mut replica = Replica::<Workbench>::new();
    let mut program = proposal.program();
    program.name = proposal.name(&catalog);
    replica
        .apply(Workbench::DRAFT_PROPOSAL.with(json!(program)))
        .unwrap();
    let state = replica.state().unwrap();
    let (_, draft) = state.drafts.iter().next().expect("a draft");
    assert_eq!(draft.name, "List notes");
    assert_eq!(draft.program().steps.len(), 1);
}

#[test]
fn the_same_proposal_is_not_opened_twice() {
    let catalog = crate::samples::catalog();
    let proposal = detect(&repeated(&["list"], 3)).expect("offered");
    let mut replica = Replica::<Workbench>::new();
    let mut program = proposal.program();
    program.name = proposal.name(&catalog);
    let offered = json!(program);
    replica
        .apply(Workbench::DRAFT_PROPOSAL.with(offered.clone()))
        .unwrap();
    let refused = replica.apply(Workbench::DRAFT_PROPOSAL.with(offered));
    assert!(refused.is_err(), "a second draft of the same name");
    assert_eq!(replica.state().unwrap().drafts.len(), 1);
}

#[test]
fn turning_a_proposal_down_is_remembered_so_it_is_not_offered_again() {
    let proposal = detect(&repeated(&["list"], 3)).expect("offered");
    let mut replica = Replica::<Workbench>::new();
    replica
        .apply(Workbench::IGNORE_PROPOSAL.with(proposal.key()))
        .unwrap();
    assert_eq!(replica.state().unwrap().ignored, vec![proposal.key()]);
    replica
        .apply(Workbench::ALLOW_PROPOSAL.with(proposal.key()))
        .unwrap();
    assert!(
        replica.state().unwrap().ignored.is_empty(),
        "offering it again is how a person changes their mind"
    );
}

#[test]
fn a_refinement_keeps_the_shortcut_and_says_what_changed() {
    let catalog = crate::samples::catalog();
    let before = crate::drafting::adopt(
        json!({
            "name": "Tidy notes",
            "steps": [
                {"id": "list", "step": "invoke",
                 "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
                 "resource": {"fixed": "notes.folder:inbox"}},
                {"id": "note", "step": "invoke",
                 "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
                 "resource": {"fixed": "notes.folder:inbox"},
                 "args": {"title": {"literal": "Digest"}, "pinned": {"literal": false}}}
            ]
        }),
        &[],
        &catalog,
    )
    .expect("a shortcut");
    let answer = json!({
        "name": "Tidy notes",
        "steps": [
            {"id": "list", "step": "invoke",
             "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}},
            {"id": "note", "step": "invoke",
             "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
             "resource": {"fixed": "notes.folder:inbox"},
             "args": {"title": {"literal": "Skipped the archived ones"}, "pinned": {"literal": false}}}
        ]
    });
    let refined = refine(answer, &before, &[], &catalog).expect("a refinement");
    assert_eq!(
        refined.shortcut.id, before.shortcut.id,
        "a refinement changes one shortcut, it does not make another"
    );
    let changes = diff(&before.shortcut.steps, &refined.shortcut.steps, &catalog);
    assert!(
        !changes.is_empty(),
        "the person reviews what changed: {changes:?}"
    );
    assert!(
        changes.iter().any(|line| line.contains('2')),
        "the step that changed is named: {changes:?}"
    );
}

#[test]
fn a_refinement_that_does_not_check_comes_back_with_what_is_wrong() {
    let catalog = crate::samples::catalog();
    let before = crate::drafting::adopt(json!({"name": "List", "steps": []}), &[], &catalog)
        .expect("a shortcut");
    let answer = json!({
        "name": "List",
        "steps": [
            {"id": "note", "step": "invoke",
             "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
             "resource": {"fixed": "notes.folder:inbox"},
             "args": {"title": {"literal": "Digest"}}}
        ]
    });
    let refined = refine(answer, &before, &[], &catalog).expect("still a shortcut");
    assert!(
        refined
            .problems
            .iter()
            .any(|problem| problem.contains("Pin it")),
        "the checker's complaint is carried to the person: {:?}",
        refined.problems
    );
}

#[test]
fn the_comment_on_a_run_is_part_of_what_the_agent_is_asked() {
    let catalog = crate::samples::catalog();
    let before: Shortcut = serde_json::from_value(json!({
        "id": "tidy",
        "name": "Tidy notes",
        "steps": [
            {"id": "list", "step": "invoke",
             "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
             "resource": {"fixed": "notes.folder:inbox"}}
        ]
    }))
    .unwrap();
    let arguments = arguments(&before, "skip the archived ones", &catalog);
    let prompt = arguments
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        prompt.contains("skip the archived ones"),
        "what the person said reaches the agent: {prompt}"
    );
    assert!(
        prompt.contains("\"id\":\"list\""),
        "and so does the program it is about: {prompt}"
    );
}

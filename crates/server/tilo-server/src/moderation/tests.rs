#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_action_policy::{
    Answer, Grant, Grantee, KindSet, MemberAccess, Role, TreePath, WorkspacePolicy,
};
use tilo_actions::MemberId;
use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};

use super::*;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const OWNER: &str = "owner";
const ADA: &str = "ada";
const MALLORY: &str = "mallory";

fn reports_ready(page: &mut Replica) {
    for kind in [
        "tilo.workspace.emoji",
        chat::skills::Skills::KIND,
        Moderation::KIND,
    ] {
        let id = DocumentId::parse(&collab_types::workspace_document_id(kind)).unwrap();
        page.workspace_answered(id, collaboration::resource::WorkspaceAnswer::HoldsNone);
    }
}

fn report_state(replica: &Replica) -> Moderation {
    let id = DocumentId::parse(chat::MODERATION_DOCUMENT).unwrap();
    replica
        .read_resource(&id, |doc| Moderation::read(doc).unwrap())
        .unwrap()
}

#[test]
fn a_member_reports_but_cannot_edit_the_report_or_decide_it() {
    use serde_json::json;
    use tilo_resource::{Edit, EditOp, Segment};

    let member_actor = |policy| ReplicaActor::Member {
        access: MemberAccess::person(policy, ADA),
        sandbox: Vec::new(),
        unix_time: 2000,
        agent_name: None,
        device: None,
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, OWNER);
    policy.members.insert(ADA.into(), Role::Editor);
    let mut page = Replica::open(
        &AppCatalogue::native(),
        tilo_app_id::apps::CHAT,
        WORKSPACE,
        &MemberId::person(ADA),
        member_actor(policy.clone()),
        Seed::New,
    )
    .unwrap();
    reports_ready(&mut page);
    let invoke = |page: &mut Replica, verb: &str, args: serde_json::Value| {
        page.invoke_with(
            None,
            None,
            &ActionAddress::new(Moderation::KIND, verb),
            args.as_object().unwrap(),
            true,
        )
    };
    invoke(
        &mut page,
        "report-person",
        json!({"person": {"person": MALLORY}, "reason": "spam"}),
    )
    .unwrap();
    let before = report_state(&page);
    let report = before.open_reports().next().unwrap();
    assert_eq!(report.reporter, MemberId::person(ADA));
    let decision = json!({
        "id": "d1",
        "person": {"person": MALLORY},
        "reports": [&report.id],
        "ruling": {"outcome": "no-action", "reasoning": "Keep the post."},
    });
    let error = invoke(&mut page, "decide-reports", decision.clone()).unwrap_err();
    assert_eq!(
        error,
        tilo_actions::api::ApiError::Unknown(ActionAddress::new(
            Moderation::KIND,
            "decide-reports"
        )),
    );

    let edit = Edit::<Moderation>::new(
        vec![
            Segment::Field("reports".into()),
            Segment::Item(tilo_resource::Id::from_raw(report.id.clone())),
            Segment::Field("decision".into()),
        ],
        EditOp::Set { value: json!("d1") },
    );
    invoke(
        &mut page,
        tilo_resource::resource::FIELD_EDIT,
        json!({"edit": edit}),
    )
    .unwrap_err();
    assert_eq!(report_state(&page), before);

    // The same action address and arguments settle the existing report when
    // the workspace authorizes the caller as a Moderator.
    policy.members.insert(ADA.into(), Role::Moderator);
    let original_node = TreePath::workspace(WORKSPACE)
        .app(tilo_app_id::apps::CHAT)
        .kind("chat.workspace")
        .resource("")
        .field(tilo_action_policy::roles::MODERATION_FIELD)
        .action("decide-reports");
    policy.grants.push(Grant::new(
        "deny-chat-decisions",
        Grantee::person(ADA),
        original_node,
        KindSet::UNDOABLE,
        Answer::Deny,
        Grantee::person(OWNER),
    ));
    page.renew(member_actor(policy.clone())).unwrap();
    invoke(&mut page, "decide-reports", decision.clone()).unwrap_err();
    assert_eq!(report_state(&page), before);
    policy.grants.pop();
    page.renew(member_actor(policy)).unwrap();
    invoke(&mut page, "decide-reports", decision).unwrap();
    let after = report_state(&page);
    assert_eq!(after.open_reports().count(), 0);
    assert_eq!(
        after.decisions.iter().next().unwrap().reports.as_slice(),
        std::slice::from_ref(&report.id)
    );
}

#[test]
fn only_a_report_wakes_the_agent() {
    let report = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "report-person"),
        [],
    ));
    assert!(files_a_report(&report));
    let post = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
        [],
    ));
    assert!(!files_a_report(&post));
}

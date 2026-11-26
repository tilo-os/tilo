//! `std.may_read(who, r)` answers what a read of `r` by `who` would face: the workspace's own
//! policy, as the host gives it to the runtime for `members(r)`. A member the grants let read
//! reads, one they keep out does not, and someone who is no member of the workspace never does.

use tilo_action_policy::{
    Answer, Grant, Grantee, KindSet, MemberAccess, PersonId, Role, TreePath, WorkspacePolicy,
};
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, action_id, type_id};

const READS: &str = r#"app test.reads "Reads"

type Board {
  readable: Bool
}

type Note {
  title: Text
}

root Board

default view board for Board

view board(b: Board) {
  std.page(b) {
    ui.Text("{b.readable}")
  }
}

action check(b: Board, who: Actor, r: host.Resource) {
  b.readable = std.may_read(who, r)
}
"#;

const APP: &str = "test.reads";
const WORKSPACE: &str = "workspace";

/// The node a grant on the note `note` names.
fn node_of(kind: &str, note: &ResourceRef) -> TreePath {
    TreePath::workspace(WORKSPACE)
        .app(APP)
        .kind(kind)
        .resource(note.id_string())
}

/// Ada's workspace: Cy views it but may not read `secret`, and the agent Scout is admitted
/// with a read of `open` alone, when `scout_reads`.
fn access(kind: &str, open: &ResourceRef, secret: &ResourceRef, scout_reads: bool) -> MemberAccess {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("cy".to_owned(), Role::Viewer);
    policy.grants.push(Grant::new(
        "hide-secret",
        Grantee::person("cy"),
        node_of(kind, secret),
        KindSet::ALL,
        Answer::Deny,
        Grantee::person("ada"),
    ));
    if scout_reads {
        policy.grants.push(Grant::new(
            "agent-member:scout",
            Grantee::agent("scout", "ada"),
            node_of(kind, open),
            KindSet::READ,
            Answer::Allow,
            Grantee::person("ada"),
        ));
    }
    // An answer Ada gave an agent working for her lets it read, but admits it to nothing.
    policy.grants.push(Grant::new(
        "answer:helper",
        Grantee::agent("helper", "ada"),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    MemberAccess::person(policy, "ada")
}

/// The members and their reads by `access`, as the window's instance gives them to the runtime.
fn follow(ada: &Replica, access: &MemberAccess) {
    let me = PersonId::from("ada");
    crate::fixtures::stage_members(
        &ada.host,
        tilolang_tilo::members::members_of(access, &me),
        Some(tilolang_tilo::members::reads_of(access, &me, &ada.host)),
    );
}

/// What `std.may_read(who, note)` answers, run by Ada's action: what the action wrote.
fn may_read(
    ada: &Replica,
    check: &str,
    board: &ResourceRef,
    who: MemberId,
    note: &ResourceRef,
) -> bool {
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(MemberId::person("ada")),
            (
                check,
                vec![
                    Value::resource(board),
                    Value::Actor(Actor::from(who)),
                    Value::resource(note),
                ],
            ),
        )
        .unwrap();
    match ada.host.runtime().read(board, "readable").unwrap() {
        Value::Bool(readable) => readable,
        other => panic!("the board's answer is a Bool, got {other:?}"),
    }
}

#[test]
fn may_read_answers_what_the_workspace_grants() {
    let (ada, packages) = Replica::new("ada", crate::fixtures::Workspace::new(), &[(READS, "")]);
    let meta = packages[0].meta.clone();
    let board = ada.host.root_of(APP).unwrap();
    let note_ty = type_id(&meta, "Note");
    let open = ada.host.runtime().create(&note_ty).unwrap();
    let secret = ada.host.runtime().create(&note_ty).unwrap();
    ada.commit();
    let kind = meta.kind_of("Note");
    let check = action_id(&meta, "check");
    let ask =
        |ada: &Replica, who: MemberId, note: &ResourceRef| may_read(ada, &check, &board, who, note);

    let granted = access(&kind, &open, &secret, true);
    follow(&ada, &granted);
    // The members the grants let read, read.
    assert!(
        ask(&ada, MemberId::person("ada"), &secret),
        "the owner reads"
    );
    assert!(ask(&ada, MemberId::person("cy"), &open), "a viewer reads");
    assert!(
        ask(&ada, MemberId::agent("scout", "cy"), &open),
        "the agent's grant lets it read"
    );
    // A member the grants keep out does not.
    assert!(
        !ask(&ada, MemberId::person("cy"), &secret),
        "Cy's denial holds"
    );
    assert!(
        !ask(&ada, MemberId::agent("scout", "cy"), &secret),
        "no grant lets the agent read the secret"
    );
    // Nor does anyone who is no member, whatever a grant says of them.
    assert!(
        !ask(&ada, MemberId::person("stranger"), &open),
        "a stranger reads nothing"
    );
    assert!(
        !ask(&ada, MemberId::agent("helper", "stranger"), &open),
        "an agent an answer lets read is no member"
    );

    // The agent's grant revoked, it reads nothing more.
    follow(&ada, &access(&kind, &open, &secret, false));
    assert!(
        !ask(&ada, MemberId::agent("scout", "cy"), &open),
        "a revoked grant reads false"
    );
    assert!(
        ask(&ada, MemberId::person("cy"), &open),
        "the others read as before"
    );

    // A runtime the host gave no members answers no for everyone.
    let (lone, packages) = Replica::new("ada", crate::fixtures::Workspace::new(), &[(READS, "")]);
    let meta = packages[0].meta.clone();
    let board = lone.host.root_of(APP).unwrap();
    let note = lone.host.runtime().create(&type_id(&meta, "Note")).unwrap();
    lone.commit();
    assert!(
        !may_read(
            &lone,
            &action_id(&meta, "check"),
            &board,
            MemberId::person("ada"),
            &note
        ),
        "who the host named no member of reads nothing"
    );
}

#[test]
fn a_read_refuses_what_the_reader_may_not_read() {
    let (ada, packages) = Replica::new("ada", crate::fixtures::Workspace::new(), &[(READS, "")]);
    let meta = packages[0].meta.clone();
    ada.host.root_of(APP).unwrap();
    let note_ty = type_id(&meta, "Note");
    let open = ada.host.runtime().create(&note_ty).unwrap();
    let secret = ada.host.runtime().create(&note_ty).unwrap();
    ada.commit();
    follow(&ada, &access(&meta.kind_of("Note"), &open, &secret, true));
    let read = |who: MemberId, note: &ResourceRef| {
        let reader = Actor::from(who);
        ada.host.read_resource(
            &tilolang_tilo::ReadAsk::of(Some(&tilolang_tilo::naming::resource_id(
                APP,
                &note.id_string(),
            ))),
            &ada.host.reads_for(&reader),
        )
    };
    assert!(
        read(MemberId::person("ada"), &secret).is_ok(),
        "the owner reads it"
    );
    assert!(
        read(MemberId::person("cy"), &open).is_ok(),
        "a viewer reads the open note"
    );
    let refused = read(MemberId::person("cy"), &secret).unwrap_err();
    assert!(
        refused.starts_with("no resource"),
        "it reads as not there: {refused}"
    );
    assert!(
        read(MemberId::agent("scout", "stranger"), &secret).is_err(),
        "no grant lets the agent read it"
    );
    assert!(
        read(MemberId::person("stranger"), &open).is_err(),
        "a stranger reads nothing"
    );
}

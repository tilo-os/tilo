//! Who a conversation's composer offers to mention: the workspace's people, named as the member
//! list the instance's connection heard names them, and of them the members its policy names.

use super::*;
use crate::interaction::{composer, send_button};
use tilolang_process::testing::Bus;
use ui::headless::Driver;
use ui::permissions::policy::{self, Grantee, Role};
use ui::tilo_actions::MemberId;

const INTERACTION: &str = "so.tilo.lang.interaction";

/// Ada at a window of the conversation the `interaction` package stages, in a workspace whose
/// member list names `people` (member, name); `policy` makes the workspace's policy, given its id.
fn in_a_conversation(
    people: &[(MemberId, &str)],
    policy: impl FnOnce(&str) -> Option<policy::WorkspacePolicy>,
) -> Session {
    let bus = Rc::new(Bus::default());
    bus.member(MemberId::person("ada"), "Ada");
    for (id, name) in people {
        bus.member(id.clone(), name);
    }
    let route = RouteSpec::app(INTERACTION, uuid::Uuid::new_v4());
    let workspace = route.cluster_id.to_string();
    let mut spec = spec_of(&route, "ada");
    if let Some(policy) = policy(&workspace) {
        spec = spec.access(policy::MemberAccess::person(policy, "ada"));
    }
    let mut session = Session::on_bus(&bus, &route, spec);
    session.instance_mut().stage("a conversation");
    session.settle();
    session
}

/// The open completion drawer's rows, as assistive technology reads them.
fn drawer_rows(window: &Driver) -> Vec<String> {
    fn find(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        if node.role == ui::a11y::Role::ListBox {
            out.extend(node.children.iter().filter_map(|row| row.name.clone()));
            return;
        }
        for child in &node.children {
            find(child, out);
        }
    }
    let snapshot = window.inspection();
    let mut out = Vec::new();
    for node in snapshot.root.nodes() {
        if node.widget == "RichInput"
            && let Some(access) = &node.semantics
        {
            find(access, &mut out);
        }
    }
    out
}

/// Everything `access` and what is under it say, in order.
fn texts(access: &ui::a11y::AccessNode, out: &mut String) {
    for part in [&access.name, &access.value].into_iter().flatten() {
        out.push_str(part);
        out.push(' ');
    }
    for child in &access.children {
        texts(child, out);
    }
}

/// What the composer holds.
fn draft(window: &Driver) -> String {
    let snapshot = window.inspection();
    snapshot
        .root
        .nodes()
        .iter()
        .rev()
        .find(|node| node.widget == "RichInput")
        .and_then(|node| node.semantics.as_ref())
        .and_then(|access| access.value.clone())
        .unwrap_or_default()
}

/// What the last rich text in the window says: the message sent last.
fn sent(window: &Driver) -> String {
    let snapshot = window.inspection();
    let access = snapshot
        .root
        .nodes()
        .iter()
        .rev()
        .find(|node| node.widget == "BlockEditor")
        .and_then(|node| node.semantics.clone())
        .expect("the sent message shows its rich text");
    let mut said = String::new();
    texts(&access, &mut said);
    said
}

/// Ada types `text` into the composer, the drawer laid out after.
fn type_in_composer(session: &mut Session, text: &str) {
    let (x, y) = composer(session.window.harness());
    session.window.click_at(x, y);
    session.window.type_text(text);
    session.window.harness().relayout();
}

#[test]
fn the_composers_drawer_offers_the_members_of_the_conversation_by_the_names_the_member_list_gives()
{
    let _settled = ui::motion::testing::settled();
    // Ada owns the workspace, Mia edits, Leo views, and the agent `planner` was admitted. Zoe is
    // on the member list the broker hands out, but the policy holds no role of hers.
    let mut session = in_a_conversation(
        &[
            (MemberId::person("u_mia"), "Mia"),
            (MemberId::person("u_leo"), "Leo"),
            (MemberId::person("u_zoe"), "Zoe"),
            (MemberId::agent("planner", "u_zoe"), "Planner"),
        ],
        |workspace| {
            let mut policy = policy::WorkspacePolicy::new(workspace, "ada");
            policy.members.insert("u_mia".to_owned(), Role::Editor);
            policy.members.insert("u_leo".to_owned(), Role::Viewer);
            policy.grants.push(policy::Grant::new(
                "agent-member:planner",
                Grantee::agent("planner", "ada"),
                policy::TreePath::workspace(workspace),
                policy::KindSet::READ_UNDOABLE,
                policy::Answer::Allow,
                Grantee::person("ada"),
            ));
            Some(policy)
        },
    );
    type_in_composer(&mut session, "@");
    let people = drawer_rows(session.window.harness());
    // Who spoke in the conversation first (its scenario's people), then its members.
    let members = people.iter().position(|row| row == "Members").map_or_else(
        || panic!("the members have their header: {people:?}"),
        |at| &people[at + 1..],
    );
    assert!(
        people
            .first()
            .is_some_and(|row| row == "In this conversation"),
        "{people:?}"
    );
    for member in ["Mia", "Leo"] {
        assert!(
            members.iter().any(|row| row.contains(member)),
            "{member} is a member and is offered by name: {people:?}"
        );
    }
    assert!(
        !people.iter().any(|row| row.contains("Zoe")),
        "who is on the list but no member is not offered: {people:?}"
    );
    assert!(
        !people.iter().any(|row| row.contains("Unknown")),
        "nobody offered is unnamed: {people:?}"
    );
    // The drawer shows its first rows; typing finds the rest.
    session.window.type_text("pla");
    session.window.harness().relayout();
    let found = drawer_rows(session.window.harness());
    assert!(
        found.len() == 1 && found[0].contains("Planner"),
        "the admitted agent is a member too: {found:?}"
    );
    session.window.harness().key("Enter");
    session.settle();
    // An agent's mention reads as one: `@Planner · Agent`.
    let draft = draft(session.window.harness());
    assert!(draft.starts_with("@Planner"), "{draft:?}");
}

#[test]
fn the_composers_drawer_offers_the_workspaces_people_and_writes_a_mention_that_is_sent() {
    let _settled = ui::motion::testing::settled();
    let mut session = in_a_conversation(
        &[
            (MemberId::person("u_mia"), "Mia"),
            (MemberId::person("u_leo"), "Leo"),
        ],
        |_| None,
    );
    type_in_composer(&mut session, "thanks @");
    let people = drawer_rows(session.window.harness());
    assert!(
        people.iter().any(|row| row == "Mia") && people.iter().any(|row| row == "Leo"),
        "the trigger opens the workspace's people, with nothing declared by the view: {people:?}"
    );
    session.window.type_text("mi");
    // No settling: the drawer narrows in the window.
    session.window.harness().relayout();
    assert_eq!(drawer_rows(session.window.harness()), ["Mia"]);
    session.window.harness().key("Enter");
    session.settle();
    assert_eq!(draft(session.window.harness()), "thanks @Mia ");
    assert!(
        drawer_rows(session.window.harness()).is_empty(),
        "the pick closed the drawer"
    );
    session.window.type_text("for the build");
    session.settle();
    let (x, y) = send_button(session.window.harness());
    session.window.click_at(x, y);
    session.settle();
    let said = sent(session.window.harness());
    assert!(
        said.contains("Mia") && said.contains("for the build"),
        "the sent message holds its mention, read as the person: {said:?}"
    );
    assert_eq!(draft(session.window.harness()), "", "and the draft cleared");
}

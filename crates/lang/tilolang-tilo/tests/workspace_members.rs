//! Who is in the workspace is its members' resource, read through the host's own store: a view
//! of `workspace.members` follows the workspace's add-member and remove-member actions, on the
//! device that carried them out and on one that synced them in, and a workspace of ten thousand
//! people is read once, never a member at a time.

use std::cell::Cell;
use std::rc::Rc;

use collab_types::store::wire::{Command, Reply};
use tilolang_runtime::{Actor, UiNode};
use tilolang_tilo::host::Host;
use tilolang_tilo::personal::actor;
use tilolang_tilo::resources::ResourceStore;

use crate::fixtures::{Replica, Workspace};

const PEOPLE: &str = r#"app so.tilo.lang.people "People"

use tilo.workspace as workspace

type People {
    title: Text
}

root People

default view people for People

view people(r: People) {
    page {
        text("{workspace.members.people.len()} people")
        for p in workspace.members.people.take(20) {
            text("in {p.name()}")
        }
    }
}
"#;
const APP: &str = "so.tilo.lang.people";

fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .filter(|node| node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("value") {
            Some(tilolang_runtime::Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn shown(view: &tilolang_runtime::ViewInstance) -> Vec<String> {
    texts(view.tree())
}

#[test]
fn a_view_of_the_workspaces_members_follows_its_member_actions_here_and_on_a_synced_device() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(PEOPLE, "")]);
    ada.host.root_of(APP).unwrap();
    ada.commit();
    let mut view = ada.view(APP);
    assert_eq!(
        shown(&view),
        ["0 people"],
        "a workspace no one was added to"
    );

    ada.host.add_workspace_member(&actor("ada")).unwrap();
    ada.host.add_workspace_member(&actor("mia")).unwrap();
    view.poll_patches().unwrap();
    assert_eq!(shown(&view), ["2 people", "in ada", "in mia"]);

    // Ben's device takes the workspace in: the members it reads are the ones Ada's actions wrote.
    ada.commit();
    let ben = ada.fork("ben", &[(PEOPLE, "")]);
    let mut his = ben.view(APP);
    assert_eq!(shown(&his), ["2 people", "in ada", "in mia"]);

    ada.host.remove_workspace_member(&actor("mia")).unwrap();
    view.poll_patches().unwrap();
    assert_eq!(
        shown(&view),
        ["1 people", "in ada"],
        "a removal shows at once"
    );
    ada.commit();
    ada.send_to(&ben);
    assert!(
        ben.host.follow_members(),
        "the synced removal moves Ben's members"
    );
    his.poll_patches().unwrap();
    assert_eq!(shown(&his), ["1 people", "in ada"]);
    assert!(
        !ben.host.follow_members(),
        "nothing moved since: the members are not read again"
    );
}

/// A store held in this process that counts its reads of the workspace's members' resource and
/// every command it carries out.
struct Counted {
    port: tilo_resource_store::LocalPort,
    members: Cell<usize>,
    commands: Cell<usize>,
}

impl tilo_resource_store::Port for Counted {
    fn command(&self, command: Command) -> Result<Reply, String> {
        self.commands.set(self.commands.get().saturating_add(1));
        if let Command::Content { id, .. } = &command
            && id == tilo_resource_store::WORKSPACE_MEMBERS_ID
        {
            self.members.set(self.members.get().saturating_add(1));
        }
        tilo_resource_store::Port::command(&self.port, command)
    }

    fn declare(&self, schema: tilo_resource_store::Schema) {
        tilo_resource_store::Port::declare(&self.port, schema);
    }

    fn set_record_reader(&self, reader: std::sync::Arc<dyn tilo_resource_store::RecordReader>) {
        tilo_resource_store::Port::set_record_reader(&self.port, reader);
    }
}

#[test]
fn ten_thousand_members_are_read_once_and_a_view_works_only_for_the_page_it_shows() {
    let port = Rc::new(Counted {
        port: tilo_resource_store::LocalPort::in_memory(),
        members: Cell::new(0),
        commands: Cell::new(0),
    });
    let store = ResourceStore::new(port.clone());
    let me = actor("ada");
    // The workspace was staged with ten thousand people: one write of its members' resource.
    let people: Vec<serde_json::Value> = (0..10_000)
        .map(|n| {
            tilolang_runtime::value::to_json(&tilolang_runtime::Value::Actor(actor(&format!(
                "p{n:05}"
            ))))
            .unwrap()
        })
        .collect();
    store
        .change_workspace_members(
            tilo_action_policy::roles::governance::ADD_MEMBER,
            &me,
            serde_json::Value::Array(people),
        )
        .unwrap();
    // Staging read whether the workspace had a members' resource yet: what follows is the host's.
    port.members.set(0);
    let host = Host::open(store, me, None, || None).unwrap();
    host.load_builtin(PEOPLE, "").unwrap();
    host.root_of(APP).unwrap();
    assert_eq!(
        port.members.get(),
        1,
        "the host read the members once as it opened"
    );

    let mut view = host.open_main(APP).unwrap();
    let shown = shown(&view);
    assert_eq!(shown.first().map(String::as_str), Some("10000 people"));
    assert_eq!(
        shown.len(),
        21,
        "the page shows twenty of them: {}",
        shown.len()
    );
    assert_eq!(
        port.members.get(),
        1,
        "opening a view of them read nothing again"
    );

    // A window follows the members every frame: with nothing moved it reads nothing, and the
    // view has nothing to run again for.
    let commands = port.commands.get();
    for _ in 0..3 {
        assert!(!host.follow_members());
        view.poll_patches().unwrap();
        assert!(!view.woken());
    }
    assert_eq!(port.members.get(), 1);
    assert_eq!(
        port.commands.get(),
        commands,
        "a frame with nothing moved asks the store nothing"
    );

    // One more person: the members are read once more, and the view runs again for them.
    host.add_workspace_member(&Actor::from(tilolang_runtime::ids::MemberId::person("zed")))
        .unwrap();
    view.poll_patches().unwrap();
    assert_eq!(
        texts(view.tree()).first().map(String::as_str),
        Some("10001 people")
    );
}

/// The shell keeps its record in the person's own workspace, whose members are the person
/// alone: the workspace in front's people reach it as its embedder's reading of that
/// workspace's members, and its views name them and wear their colours from it.
#[test]
fn a_host_its_embedder_publishes_members_to_names_them_from_the_reading() {
    let store = ResourceStore::new(Rc::new(tilo_resource_store::LocalPort::in_memory()));
    let me = actor("ada");
    let host = Host::open(store, me, None, || None).unwrap();
    host.load_builtin(PEOPLE, "").unwrap();
    host.root_of(APP).unwrap();
    // The host's own store holds no members' record: nothing is read from it, and the people
    // are the rows the session publishes, each with what the workspace says of them.
    host.let_members_be_read_elsewhere();
    assert!(host.reads_members_elsewhere());
    let rows: Vec<serde_json::Value> = ["ada", "bo"]
        .iter()
        .map(|name| {
            let who = tilo_member_types::Actor::Member(tilo_member_types::MemberId::person(*name));
            serde_json::json!({
                "actor": serde_json::to_value(&who).unwrap(),
                "name": name,
            })
        })
        .collect();
    assert!(
        host.serve_member_reading(&serde_json::json!({ "people": rows })),
        "the workspace's people are read as the session publishes them"
    );
    let mut view = host.open_main(APP).unwrap();
    assert_eq!(shown(&view), ["2 people", "in ada", "in bo"]);
    assert_eq!(
        host.member_rows().len(),
        2,
        "the rows name every member the reading named"
    );
    assert!(
        host.member_rows().values().any(|row| row.name == "bo"),
        "a member is named by the name the workspace keeps"
    );
    // A host that takes its members as a reading reads no record of them, and a frame with
    // nothing served runs no view again.
    assert!(!host.read_workspace_members());
    view.poll_patches().ok();
    assert!(!host.follow_members(), "nothing moved since the reading");
}

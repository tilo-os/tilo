//! What a resource starts with (`initial action`) and how an earlier version's resources are
//! upgraded (`migration action`): a new resource starts filled in the change that makes it, which
//! is nobody's and catches nobody up; a resource that exists never gets its type's initial
//! content, and a migration upgrades only what an earlier version wrote.

use crate::fixtures::Workspace;
use tilolang_runtime::Actor;
use tilolang_runtime::value::Value;

use crate::fixtures::{Replica, target};

/// A gallery that starts with two groups, each group starting with a note, and a migration
/// that has nothing to do on a gallery this version makes.
const GALLERY: &str = r#"app so.tilo.gallery "Gallery"

type Note {
  text: Text
}

type Group {
  title: Text
  notes: Collection<Note>
}

type Gallery {
  name: Text = "Our gallery"
  groups: Collection<Group>
}

initial action fill(g: Gallery) {
  g.groups.insert(Group(title: "Everyday parts"))
  g.groups.insert(Group(title: "Collections"))
}

initial action start(g: Group) {
  g.notes.insert(Note(text: "What this group holds"))
}

action add(g: Gallery, title: Text) -> Group {
  g.groups.insert(Group(title: title))
}

migration action rename_the_old(g: Gallery) {
  for group in g.groups {
    if group.title == "Old" {
      group.title = "Renamed"
    }
  }
}

root Gallery

default view board for Gallery

view board(g: Gallery) {
  page {
    for group in g.groups {
      text(group.title)
      for note in group.notes {
        text(note.text)
      }
    }
  }
}
"#;

/// The same gallery before it declared what a new one starts with or any migration.
const EARLIER: &str = r#"app so.tilo.gallery "Gallery"

type Note {
  text: Text
}

type Group {
  title: Text
  notes: Collection<Note>
}

type Gallery {
  groups: Collection<Group>
}

action add(g: Gallery, title: Text) -> Group {
  g.groups.insert(Group(title: title))
}

root Gallery

default view board for Gallery

view board(g: Gallery) {
  page {
    for group in g.groups {
      text(group.title)
      for note in group.notes {
        text(note.text)
      }
    }
  }
}
"#;

const APP: &str = "so.tilo.gallery";

/// The words the window shows.
fn shown(replica: &Replica) -> Vec<String> {
    let view = replica.view(APP);
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .filter(|node| node.kind == "text" || node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn person(replica: &Replica) -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person(&replica.name))
}

/// Invoke the package's action `action` as the replica's person.
fn call(replica: &Replica, action: &str, args: Vec<Value>) -> Value {
    replica.call(APP, action, args)
}

/// Who made each version of `at`, by which action.
fn versions(replica: &Replica, at: &tilolang_runtime::ResourceRef) -> Vec<(Option<Actor>, String)> {
    replica
        .host
        .runtime()
        .versions(at)
        .unwrap()
        .into_iter()
        .map(|version| (version.who, version.action))
        .collect()
}

#[test]
fn a_fresh_root_starts_filled_in_its_making_and_nobody_is_caught_up_on_it() {
    let (replica, packages) = Replica::new("ada", Workspace::new(), &[(GALLERY, "")]);
    // The first frame already shows what the gallery starts with, and what each group it made
    // starts with too.
    let first = vec![
        "Everyday parts".to_owned(),
        "What this group holds".to_owned(),
        "Collections".to_owned(),
        "What this group holds".to_owned(),
    ];
    assert_eq!(
        shown(&replica),
        first,
        "the root as its window first draws it"
    );
    assert_eq!(
        replica.commit(),
        1,
        "the root and all it starts with are one change"
    );

    let root = replica.host.root_of(APP).unwrap();
    assert_eq!(
        replica.host.runtime().read(&root, "name").unwrap(),
        Value::Text("Our gallery".into()),
        "a text field starts at the words its type declares"
    );
    let history = versions(&replica, &root);
    assert!(
        !history.is_empty()
            && history
                .iter()
                .all(|(_, action)| action == tilolang_runtime::store::CREATE_ACTION),
        "its history is its making alone: {history:?}"
    );
    assert!(
        replica
            .host
            .runtime()
            .unseen(&person(&replica), &root)
            .unwrap()
            .is_empty(),
        "nothing about a making is anyone's to catch up on"
    );
    assert_eq!(
        replica.host.migrate(),
        0,
        "a gallery this version made has no migration to do"
    );
    assert_eq!(replica.commit(), 0, "and opening it again changes nothing");

    let ids = packages[0].ids.clone();
    let peer = Replica::new("peer", replica.doc.fork(), &[(GALLERY, ids.as_str())]).0;
    assert_eq!(peer.host.migrate(), 0, "a peer opening it changes nothing");
    assert_eq!(peer.commit(), 0);
    assert_eq!(shown(&peer), first, "a peer reads what the making wrote");
}

#[test]
fn an_item_inserted_later_starts_with_its_types_initial_content_in_its_own_change() {
    let (replica, _) = Replica::new("ada", Workspace::new(), &[(GALLERY, "")]);
    let root = replica.host.root_of(APP).unwrap();
    replica.commit();
    let added = target(&call(
        &replica,
        "add",
        vec![Value::resource(&root), Value::Text("Media".into())],
    ));
    assert_eq!(
        replica.commit(),
        1,
        "the group and its note are the change that added it"
    );
    let shown = shown(&replica);
    assert_eq!(
        shown[shown.len() - 2..],
        ["Media".to_owned(), "What this group holds".to_owned()],
        "the new group starts with its note"
    );
    let add = versions(&replica, &added);
    assert_eq!(add.len(), 1, "one version, the person's: {add:?}");
    assert_eq!(add[0].0, Some(person(&replica)));
}

#[test]
fn a_root_that_exists_never_gets_initial_content_and_a_migration_upgrades_it() {
    let (earlier, packages) = Replica::new("ada", Workspace::new(), &[(EARLIER, "")]);
    let root = earlier.host.root_of(APP).unwrap();
    call(
        &earlier,
        "add",
        vec![Value::resource(&root), Value::Text("Old".into())],
    );
    earlier.commit();
    assert_eq!(shown(&earlier), vec!["Old".to_owned()]);

    // The next version declares what a new gallery starts with, and a migration of the old ones.
    let ids = packages[0].ids.clone();
    let upgraded = earlier.fork("ada", &[(GALLERY, ids.as_str())]);
    assert_eq!(
        shown(&upgraded),
        vec!["Old".to_owned()],
        "the gallery that exists keeps what it had"
    );
    assert_eq!(upgraded.host.migrate(), 1, "the migration upgrades it");
    assert_eq!(upgraded.commit(), 1);
    assert_eq!(shown(&upgraded), vec!["Renamed".to_owned()]);
    assert_eq!(upgraded.host.migrate(), 0, "once");
}

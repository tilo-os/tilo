//! Packages compiled from source, and replicas of one room that run them.

use automerge::AutoCommit;
use tilo_actions::{ActionContext, ActionEnvelope, ActionInvoker};
use tilo_automerge::ApplyIncoming as _;
use tilolang_runtime::meta::Meta;
use tilolang_runtime::{Decision, Principal, ViewInstance};
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::{Host, Package};
use tilolang_tilo::record::{DynRecord, DynValue};

/// The counter of the language spec: the first package of the host.
pub const COUNTER: &str = include_str!("../../../../apps/tl/counter/app.tl");

/// A list with text, a collection and a set, for the merge kinds the counter does not use.
pub const TODO: &str = r#"app so.tilo.todo "Todo"

type Item {
  text: Text merge text
  done: Bool
}

type TodoList {
  title: Text merge text
  items: Collection<Item>
  tags: Set<Text>
}

action add(l: TodoList, text: Text) { l.items.insert(Item(text: text, done: false)) }

action move_first(l: TodoList, i: Item) { l.items.move(i, 0) }

action tag(l: TodoList, t: Text) { l.tags.add(t) }

root TodoList

default view main for TodoList

view main(l: TodoList) {
  page {
    input(bind: l.title, placeholder: "Title")
    for i in l.items {
      text(i.text)
    }
  }
}
"#;

pub const APP: &str = "so.tilo.lang.counter";

/// One collaborator's machine: a copy of the room record and a host over it.
pub struct Replica {
    pub name: String,
    pub doc: AutoCommit,
    pub host: Host,
    /// The envelope of every action committed here, for a peer to replay.
    pub log: Vec<ActionEnvelope>,
}

impl Replica {
    /// A replica of `doc` for `name`, with `packages` (source and ids lock) loaded.
    pub fn new(name: &str, doc: AutoCommit, packages: &[(&str, &str)]) -> (Self, Vec<Package>) {
        let host = Host::new(&doc, Principal::new(name), |_, _| Decision::Allow).unwrap();
        let loaded = packages
            .iter()
            .map(|(source, ids)| host.load_builtin(source, ids).unwrap())
            .collect();
        (
            Self {
                name: name.to_owned(),
                doc,
                host,
                log: Vec::new(),
            },
            loaded,
        )
    }

    /// A second replica of this one's room, for `name`, running the same packages.
    pub fn fork(&mut self, name: &str, packages: &[(&str, &str)]) -> Self {
        Self::new(name, self.doc.fork(), packages).0
    }

    /// Commit one planned action through the executor, as the host does for a window: it is
    /// the change's recorded action, attributed to this replica's person.
    pub fn apply(&mut self, action: &DynAction) {
        let context = ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("room"),
            &action.kind,
            &action.kind,
        )
        .with_app("so.tilo.lang", "tilolang")
        .with_invoker(ActionInvoker::actor(self.name.clone()));
        let envelope = tilo_actions::apply_local_with_context(
            &mut self.doc,
            action,
            action.encode().unwrap(),
            Some(self.name.as_bytes()),
            context,
        )
        .unwrap();
        self.log.push(envelope);
        self.doc.commit();
        self.host.sync_from(&self.doc);
    }

    /// Commit what the runtime committed on its own (a press, a created root).
    pub fn commit(&mut self) {
        for committed in self.host.take_committed() {
            self.apply(&committed.action);
        }
    }

    /// Exchange every change with `other`, as sync does.
    pub fn sync_with(&mut self, other: &mut Self) {
        self.doc.merge_incoming(&mut other.doc).unwrap();
        other.doc.merge_incoming(&mut self.doc).unwrap();
        self.host.sync_from(&self.doc);
        other.host.sync_from(&other.doc);
    }

    /// Open the `main` view of `app`.
    pub fn view(&self, app: &str) -> ViewInstance {
        self.host.open_main(app).unwrap()
    }

    /// The root resource of `app` as the runtime reads it now.
    pub fn root(&self, app: &str) -> DynRecord {
        let root = self.host.root_of(app).unwrap();
        self.host.store().record(&root.doc).unwrap()
    }
}

/// The id of the field `field` of the type `ty` of `meta`.
pub fn field_id(meta: &Meta, ty: &str, field: &str) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
        .map(|f| f.id.clone())
        .unwrap()
}

/// The counter's value in a root record.
pub fn count(record: &DynRecord, meta: &Meta) -> i64 {
    match record.get(&field_id(meta, "Counter", "value")) {
        Some(DynValue::Counter(counter)) => counter.get(),
        other => panic!("not a counter: {other:?}"),
    }
}

/// The key of the button that adds: the counter's `Increment`, else the first button of a view.
pub fn button_key(view: &ViewInstance) -> String {
    fn plus(node: &tilolang_runtime::UiNode) -> Option<String> {
        if node.kind == "button"
            && node.props.get("label") == Some(&tilolang_runtime::Value::text("Increment"))
        {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(plus)
    }
    fn find(node: &tilolang_runtime::UiNode) -> Option<String> {
        if node.kind == "button" {
            return Some(node.key.clone());
        }
        node.children.iter().find_map(find)
    }
    plus(view.tree())
        .or_else(|| find(view.tree()))
        .expect("the view has a button")
}

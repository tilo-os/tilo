//! A view reads how far a record's own writes have got (`reach(r).state`): pending until another
//! device that keeps the workspace takes them in, then reached, or refused when one refuses them.
//! The view draws again as that moves, as it does when the record itself moves.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use tilo_resource_store::wire::{Command, Reach, Reply};
use tilolang_runtime::{Actor, UiNode, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::resources::{LocalPort, Port, ResourceStore};

const APP: &str = "t.reach";

const SOURCE: &str = r#"app t.reach "Reach"

type Note {
    text: Text merge text
}

root Note

default view note for Note

view note(n: Note) {
    page {
        text(n.text)
        text(reach(n).state)
    }
}
"#;

/// A store on this device whose answer to how far a record's writes have got is whatever the
/// test last set, as the workspace's sync answers it.
struct Reaching {
    store: LocalPort,
    reach: RefCell<Reach>,
}

impl Port for Reaching {
    fn command(&self, command: Command) -> Result<Reply, String> {
        match command {
            Command::Reach { .. } => Ok(Reply::Reach(self.reach.borrow().clone())),
            other => self.store.command(other),
        }
    }
}

/// Every text the view shows, in order.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .filter(|node| node.kind == "text" || node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_view_reads_each_step_of_a_records_writes_reaching_another_device() {
    let port = Rc::new(Reaching {
        store: LocalPort::in_memory(),
        reach: RefCell::new(Reach {
            pending: vec!["00".repeat(32)],
            refused: Vec::new(),
        }),
    });
    let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn Port>);
    let host = Host::open(
        store,
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        None,
        || None,
    )
    .unwrap();
    host.load_builtin(SOURCE, "").unwrap();
    let root = host.root_of(APP).unwrap();
    let mut view = host.open_resource(&root, None).unwrap();
    view.poll_patches().unwrap();
    assert!(
        texts(view.tree()).contains(&"pending".to_owned()),
        "nobody else has the record's write yet: {:?}",
        texts(view.tree())
    );

    // Another device keeping the workspace takes the write in: the view draws it reached.
    *port.reach.borrow_mut() = Reach::default();
    host.store().moved_docs(&BTreeSet::from([root.doc.clone()]));
    view.poll_patches().unwrap();
    assert!(
        texts(view.tree()).contains(&"reached".to_owned()),
        "{:?}",
        texts(view.tree())
    );

    // A later write that device refuses: the view draws it refused.
    *port.reach.borrow_mut() = Reach {
        pending: Vec::new(),
        refused: vec!["11".repeat(32)],
    };
    host.store().moved_docs(&BTreeSet::from([root.doc.clone()]));
    view.poll_patches().unwrap();
    assert!(
        texts(view.tree()).contains(&"refused".to_owned()),
        "{:?}",
        texts(view.tree())
    );
}

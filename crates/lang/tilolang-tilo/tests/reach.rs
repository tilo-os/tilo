//! A view reads how far a record's own writes have got (`reach(r).state`): pending until another
//! device that keeps the workspace takes them in, then reached, or refused when one refuses them.
//! The view draws again as that moves, as it does when the record itself moves.

use std::cell::RefCell;
use std::rc::Rc;

use crate::fixtures::text_words as texts;
use collab_types::store::MovedRecord;
use collab_types::store::wire::{Command, Reach, Reply};
use tilolang_runtime::Actor;
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
    host.store()
        .moved_records(&[MovedRecord::whole(root.doc.clone())]);
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
    host.store().moved_records(&[MovedRecord::whole(root.doc)]);
    view.poll_patches().unwrap();
    assert!(
        texts(view.tree()).contains(&"refused".to_owned()),
        "{:?}",
        texts(view.tree())
    );
}

/// A store that counts the queries of the outside effects it is asked.
struct CountingEffects {
    store: LocalPort,
    asked: std::cell::Cell<usize>,
}

impl Port for CountingEffects {
    fn command(&self, command: Command) -> Result<Reply, String> {
        if matches!(&command, Command::Query(query) if query.kind == tilolang_tilo::resources::EFFECT)
        {
            self.asked.set(self.asked.get().saturating_add(1));
        }
        self.store.command(command)
    }
}

/// What effects wait to go out is asked of the store once, and again only once a record moved:
/// a loop that asks it on every turn sends nothing while nothing changed.
#[test]
fn the_effects_waiting_are_asked_again_only_once_a_record_moved() {
    use tilolang_runtime::store::Store as _;
    let port = Rc::new(CountingEffects {
        store: LocalPort::in_memory(),
        asked: std::cell::Cell::new(0),
    });
    let store = ResourceStore::new(Rc::clone(&port) as Rc<dyn Port>);
    for _ in 0..3 {
        assert!(store.pending_effects().is_empty());
        let _ = store.next_held_effect();
        let _ = store.effect_states();
    }
    let asked = port.asked.get();
    assert!(asked > 0, "the first look asks the store");
    for _ in 0..3 {
        let _ = store.pending_effects();
        let _ = store.effect_states();
    }
    assert_eq!(
        port.asked.get(),
        asked,
        "nothing moved: nothing is asked again"
    );
    store.moved_records(&[MovedRecord::whole("n1")]);
    let _ = store.pending_effects();
    assert!(
        port.asked.get() > asked,
        "a record moved: what waits is asked again"
    );
}

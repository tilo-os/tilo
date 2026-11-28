//! The Browser as it ships: a private window's page is its author's alone, and goes with its
//! window. Another member, and an agent acting for them, lists, counts and reads none of it, while
//! the author's other device has it as long as the window is open; once the window closes,
//! nothing of the page is left on any device. An ordinary page is every member's, as before.

use std::rc::Rc;
use std::sync::Arc;

use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Actor, InvokeOutcome};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::runtime::NameHost;
use tilolang_tilo::runtime::ids::MemberId;

use crate::fixtures::{Replica, Workspace, action_id, person};

const APP: &str = "so.tilo.browser";
const BROWSER: (&str, &str) = (
    include_str!("../../../../apps/tl/browser/app.tl"),
    include_str!("../../../../apps/tl/browser/app.ids"),
);
const FILES: &str = include_str!("../../tilolang/tests/fixtures/file_explorer.tl");
const PRINT_CENTRE: &str = include_str!("../../tilolang/tests/fixtures/print_centre.tl");

/// What Ada's window loads: the packages the Browser uses, then the Browser, each under the ids
/// every device runs it with.
struct Browsing(Vec<(String, String)>);

impl Platform for Browsing {
    fn load(&self, host: &Host) -> u64 {
        for (source, ids) in &self.0 {
            host.load_builtin(source, ids).unwrap();
        }
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-28T12:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// What `replica`'s store answers of the page `page`: how many pages it lists and counts, and
/// whether it reads this one.
fn found(replica: &Replica, kind: &str, page: &ResourceRef) -> (usize, usize, bool) {
    use collab_types::store::Query;
    use collab_types::store::wire::{Command, Reply};
    let kind = kind.to_owned();
    let listed = match replica.doc.command(Command::Query(Query::of(&kind, 10))) {
        Reply::Page(page) => page.resources.len(),
        other => panic!("a query answered {other:?}"),
    };
    let counted = match replica.doc.command(Command::Count {
        kind: kind.clone(),
        filters: Vec::new(),
    }) {
        Reply::Count(count) => count,
        other => panic!("a count answered {other:?}"),
    };
    let read = match replica.doc.command(Command::Get {
        kind,
        id: page.id_string(),
    }) {
        Reply::Resource(found) => found.is_some(),
        other => panic!("a read answered {other:?}"),
    };
    (listed, counted, read)
}

/// The pages the Browser's root lists on `replica`.
fn pages(replica: &Replica) -> usize {
    match replica.read(&replica.root(APP), "pages") {
        Value::List(pages) => pages.len(),
        other => panic!("a browser's pages are a list, got {other:?}"),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one walk of a private window across three devices"
)]
fn a_private_window_leaves_nothing_anyone_else_reads_and_nothing_once_it_closes() {
    // Ben's device and Ada's laptop run the same build of every package.
    let (ben, packages) = Replica::new(
        "ben",
        Workspace::new(),
        &[(FILES, ""), (PRINT_CENTRE, ""), BROWSER],
    );
    let sources = [FILES, PRINT_CENTRE, BROWSER.0];
    let loaded: Vec<(String, String)> = sources
        .iter()
        .zip(&packages)
        .map(|(source, package)| ((*source).to_owned(), package.ids.clone()))
        .collect();
    let as_loaded: Vec<(&str, &str)> = loaded
        .iter()
        .map(|(source, ids)| (source.as_str(), ids.as_str()))
        .collect();
    let laptop = Replica::new("ada", Workspace::new(), &as_loaded).0;
    let meta = packages[2].meta.clone();

    // Ada's window of the Browser, on her desktop.
    let desktop = Workspace::new();
    let (app_end, _ui) = pair();
    let mut window = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: desktop.store(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Browsing(loaded.clone())),
        Rc::new(Alone),
        Box::new(app_end),
    );
    window.sync();
    let host = window.host().expect("the window opened");
    let root = host.root_of(APP).unwrap();
    let mut make = |action: &str| {
        let made = host
            .runtime()
            .invoke_action(
                &person("ada"),
                (
                    action_id(&meta, action).as_str(),
                    vec![Value::resource(&root)],
                ),
            )
            .unwrap();
        window.sync();
        let InvokeOutcome::Done(Value::Ref(made)) = made else {
            panic!("{action} opened a page, got {made:?}");
        };
        (*made.target).clone()
    };
    let ordinary = make("new_page");
    let private = make("new_private_page");
    let kind = meta.kind_of("Page");
    // Sync carries what Ada's desktop holds to every device of the workspace.
    let carry = |to: &Replica| {
        if to.doc.take_in_as(&desktop, Some(&to.name), None) {
            to.host.store().moved();
        }
    };
    carry(&ben);
    carry(&laptop);

    // Ben's device has the ordinary page, and none of the private one.
    assert!(
        found(&ben, &kind, &ordinary).2,
        "Ben reads an ordinary page"
    );
    assert_eq!(
        found(&ben, &kind, &private),
        (2, 2, false),
        "Ben lists and counts the home page and the ordinary one, and reads none of Ada's \
         private page"
    );
    assert_eq!(
        pages(&ben),
        2,
        "the Browser opens on a home page, and the ordinary one"
    );
    // Ada's laptop has her private page while its window is open.
    assert_eq!(found(&laptop, &kind, &private), (3, 3, true));

    // No member but Ada, and no agent acting for another member, may read the private page.
    crate::fixtures::stage_members(
        &host,
        vec![
            tilolang_runtime::members::Member::person("ada", tilo_actions::policy::Role::Owner),
            tilolang_runtime::members::Member::person("ben", tilo_actions::policy::Role::Editor),
            tilolang_runtime::members::Member::agent("helper", "ben"),
        ],
        None,
    );
    let bens_agent = Actor::from(MemberId::agent("helper", "ben"));
    assert!(host.runtime().may_read(&person("ben"), &ordinary));
    assert!(host.runtime().may_read(&bens_agent, &ordinary));
    assert!(host.runtime().may_read(&person("ada"), &private));
    assert!(!host.runtime().may_read(&person("ben"), &private));
    assert!(
        !host.runtime().may_read(&bens_agent, &private),
        "an agent acting for Ben reads none of Ada's private page"
    );

    // What the private page's window opens joins no history.
    host.runtime()
        .invoke_action(
            &person("ada"),
            (
                action_id(&meta, "went_to").as_str(),
                vec![
                    Value::resource(&private),
                    Value::text("https://example.org/private"),
                ],
            ),
        )
        .unwrap();
    window.sync();
    assert_eq!(
        host.runtime().read(&root, "visits").unwrap(),
        Value::List(Vec::new()),
        "a private page's addresses join no history"
    );

    // Ada's window shows the private page, then closes: the page goes with it, everywhere.
    window.open_requests().borrow_mut().push(OpenEffect::Open {
        anchor: private.id_string(),
    });
    window.update();
    window.sync();
    window.window_closed();
    window.sync();
    carry(&ben);
    carry(&laptop);
    assert_eq!(
        found(&laptop, &kind, &private),
        (2, 2, false),
        "nothing of the private page outlives its window on Ada's other device"
    );
    assert_eq!(pages(&laptop), 2);
    assert_eq!(found(&ben, &kind, &private), (2, 2, false));
    assert!(
        found(&ben, &kind, &ordinary).2,
        "an ordinary page stays when a window closes"
    );
}

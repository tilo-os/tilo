//! `cargo run -p tilolang-process --example components -- <package dir>`: the components a tilolang
//! app's first window shows, as its instance in a host builds them over an empty workspace store,
//! one JSON object per line: `key` (the widget's test id in the window), `kind` and `label`.
//!
//! What a test that presses a control by its test id reads, so it names a button by what it says
//! rather than by a key the compiler derived.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    reason = "a test tool: a failure ends it, and its output is its stdout"
)]

use std::rc::Rc;
use std::sync::Arc;

use collaboration::{CollabRequest, CollaborationManager, RouteSpec};
use tilo_render_protocol::channel::Channel;
use tilo_render_protocol::patch::unix::ChannelTransport;
use tilo_render_protocol::patch::{self, Level, ToUi, Transport as _};
use tilolang_process::TilolangProcess;
use tilolang_process::connection::Connection;
use tilolang_process::hosted::{Hosted, Spec};
use tilolang_process::phrases::Phrases;
use tilolang_process::testing::{Broker, Bus};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::resources::ResourceStore;

fn print(node: &patch::Node) {
    let label = ["label", "title", "text"]
        .iter()
        .find_map(|prop| match node.props.get(*prop) {
            Some(patch::Value::Text(text)) => Some(text.clone()),
            _ => None,
        });
    println!(
        "{}",
        serde_json::json!({"key": node.key, "kind": node.kind, "label": label})
    );
    for child in &node.children {
        print(child);
    }
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("a package directory"));
    let source = std::fs::read_to_string(dir.join("app.tl")).unwrap();
    let lock =
        tilolang::IdsLock::parse(&std::fs::read_to_string(dir.join("app.ids")).unwrap()).unwrap();
    let compiled = tilolang::compile_package(&source, &lock, &[])
        .unwrap_or_else(|_| panic!("{} does not compile alone", dir.display()));
    let bundle = Bundle::new(&compiled, Vec::new());
    let app = bundle.meta.app.clone();
    let route = RouteSpec::app(app.clone(), uuid::Uuid::new_v4());
    let bus = Rc::new(Bus::default());
    let mut manager =
        CollaborationManager::with_peer_transport(Broker::on(&bus, route.clone(), "person"));
    manager.send(CollabRequest::Hello {
        app_id: app.clone(),
        cluster_id: route.cluster_id,
        record: None,
        resource_type: None,
        handle: Some("person".to_owned()),
    });
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let store = ResourceStore::new(Rc::new(connection.port(None)));
    let wake: tilolang_tilo::background::Waker = Arc::new(|| {});
    let mut host = TilolangProcess::new(Arc::clone(&wake));
    let (ours, theirs) = Channel::pair().unwrap();
    let hosted = Hosted::new(
        Spec::new(app, route.cluster_id.to_string(), 1, "person"),
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(bundle),
        &tilolang_process::hosted::Kept::default(),
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake,
    );
    host.add(hosted, None);
    let mut window = ChannelTransport::new(theirs).unwrap();
    let mut root: Option<patch::Node> = None;
    // Turn until the window's tree stops changing: the document's publication settles off the frame.
    let mut quiet: u32 = 0;
    while quiet < 40 {
        host.turn();
        let mut changed = false;
        while let Some(batch) = window.recv().unwrap() {
            for message in patch::decode_batch::<ToUi>(&batch).unwrap() {
                match message {
                    ToUi::Open {
                        level: Level::Root,
                        tree,
                        ..
                    } => root = Some(tree),
                    ToUi::Patch {
                        level: Level::Root,
                        patches,
                    } => {
                        let root = root.as_mut().expect("a patch after the open");
                        for change in patches {
                            change.apply(root).unwrap();
                        }
                    }
                    _ => continue,
                }
                changed = true;
            }
        }
        quiet = if changed || root.is_none() {
            0
        } else {
            quiet.saturating_add(1)
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    print(&root.expect("the window shows a tree"));
}

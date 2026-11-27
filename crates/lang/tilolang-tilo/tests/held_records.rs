//! A record a field holds, through the workspace's store: `Clocks` made without a `Stopwatch`
//! holds one of its own, built from the stopwatch's defaults, as the one item under its field.
//! Two devices that each make the root hold one stopwatch between them, and a member who reads
//! the clocks reads it too.

use tilolang_runtime::{Actor, Value};

use crate::fixtures::{Replica, Workspace, action_id};

const CLOCKS: (&str, &str) = (
    r#"app so.tilo.test.clocks "Clocks"

type Stopwatch {
    elapsed: Int = 0
    laps: List<Int>
}

type Clocks {
    stopwatch: Stopwatch
    timers: Collection<Stopwatch>
}

root Clocks

default view clocks for Clocks

action lap(c: Clocks, at: Int) {
    c.stopwatch.elapsed = at
}

view clocks(c: Clocks) {
    ui.Text("{c.stopwatch.elapsed}")
}
"#,
    "",
);
const CLOCKS_APP: &str = "so.tilo.test.clocks";
const WORKSPACE: &str = "workspace";

/// `who`'s access to a workspace ada owns, where bo reads and nothing more.
fn access(who: &str) -> tilo_action_policy::MemberAccess {
    use tilo_action_policy::{MemberAccess, Role, WorkspacePolicy};
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("bo".to_owned(), Role::Viewer.into());
    MemberAccess::person(policy, who)
}

fn stopwatch(
    replica: &Replica,
    clocks: &tilolang_runtime::ResourceRef,
) -> tilolang_runtime::ResourceRef {
    match replica.read(clocks, "stopwatch") {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("the clocks hold a stopwatch, not {other:?}"),
    }
}

#[test]
fn two_devices_make_the_clocks_and_hold_one_stopwatch_a_reader_sees() {
    let (_, probe) = Replica::new("probe", Workspace::new(), &[CLOCKS]);
    let meta = probe[0].meta.clone();
    let ids = probe[0].ids.clone();
    let packages = [(CLOCKS.0, ids.as_str())];
    let base = Workspace::new();
    let (ada, _) = Replica::access("ada", base.fork(), &packages, Some(access("ada")));
    let (bo, _) = Replica::access("bo", base.fork(), &packages, Some(access("bo")));

    // Each device makes the root on its own, before it heard from the other.
    let on_ada = ada.root(CLOCKS_APP);
    ada.commit();
    let on_bo = bo.root(CLOCKS_APP);
    bo.commit();
    assert_eq!(on_ada, on_bo, "one root");
    let watch = stopwatch(&ada, &on_ada);
    assert_eq!(
        ada.read(&watch, "elapsed"),
        Value::Int(0),
        "it starts at its default"
    );
    assert_eq!(ada.read(&watch, "laps"), Value::List(Vec::new()));
    assert_eq!(
        ada.read(&on_ada, "timers"),
        Value::List(Vec::new()),
        "the held stopwatch is none of the clocks' timers"
    );

    ada.sync_with(&bo);
    assert_eq!(stopwatch(&bo, &on_bo), watch, "one stopwatch between them");
    assert_eq!(
        bo.read(&watch, "elapsed"),
        Value::Int(0),
        "the reader reads it"
    );

    // What ada writes to it reaches bo's device.
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (
                action_id(&meta, "lap").as_str(),
                vec![Value::resource(&on_ada), Value::Int(12)],
            ),
        )
        .unwrap();
    ada.commit();
    ada.sync_with(&bo);
    assert_eq!(bo.read(&watch, "elapsed"), Value::Int(12));
    let view = bo.view(CLOCKS_APP);
    assert!(
        format!("{:?}", view.tree()).contains("\"12\""),
        "bo's window shows it: {:?}",
        view.tree()
    );
}

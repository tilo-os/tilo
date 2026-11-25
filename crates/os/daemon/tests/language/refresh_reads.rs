//! What a read of one record costs in documents: the record it names, never the
//! records around it in its workspace.
//!
//! The work is counted, not timed: a shared machine's times vary with the load,
//! while the documents one read opens are the same wherever it executes. The
//! count is the daemon's own ([`Host::daemon`]'s metrics), so this measures the
//! same reads a person's app or an agent makes through the device's dispatch.

use serde_json::{Value, json};
use tilo_actions::remote::{ActionProvider as _, ResourceAddress, ResourceAsk, read_resources};

use crate::language::host;

/// The workspace sizes a read is counted in: one where a per-record cost is a
/// handful of reads, and one where it is hundreds.
const SIZES: [usize; 2] = [10, 1_000];

/// The issues one `new_tasks` makes at a time.
const LIST: usize = 100;

/// The most documents one read of one record may open: the record it names, and
/// the parts a chain of it is kept in, never the rest of the workspace.
const CEILING: u64 = 4;

/// `action` of `resource` with `args`, through `key`'s session.
async fn act(key: &host::Key, base: &str, resource: &str, action: &str, args: &Value) -> Value {
    let (status, reply) = key
        .post(
            &format!("{base}/resources/{resource}/actions/{action}"),
            args,
        )
        .await;
    assert_eq!(status, 200, "{resource}#{action}: {reply}");
    assert_eq!(reply["status"], "ok", "{resource}#{action}: {reply}");
    reply["result"].clone()
}

/// The workspace's tracker, filled with the issues numbered `from..from + size`.
async fn fill(key: &host::Key, base: &str, from: usize, size: usize) {
    act(key, base, "so.tilo.tasks", "open", &json!({})).await;
    for start in (from..from + size).step_by(LIST) {
        let titles: Vec<Value> = (start..(start + LIST).min(from + size))
            .map(|at| json!(format!("Issue {at}")))
            .collect();
        act(
            key,
            base,
            "so.tilo.tasks",
            "new_tasks",
            &json!({ "titles": titles }),
        )
        .await;
    }
}

/// One record of `app` in the workspace.
async fn a_record(host: &host::Host, workspace: &str, app: &str) -> ResourceAddress {
    let records = host
        .daemon()
        .workspaces()
        .records(workspace)
        .await
        .unwrap_or_default();
    let (app, record) = records
        .into_iter()
        .find(|(of, record)| of == app && !record.is_empty())
        .unwrap_or_else(|| panic!("the workspace has a record of {app}"));
    ResourceAddress {
        workspace: workspace.to_owned(),
        app,
        record,
    }
}

/// The documents reading `address` through the daemon's dispatch opens, the
/// least of a few reads, so a settling workspace beside them does not decide it.
async fn reads_of_one_record(host: &host::Host, address: ResourceAddress) -> u64 {
    let dispatch = host.dispatch();
    let reads = dispatch
        .resource_reads()
        .expect("the device's dispatch reads records");
    let caller = host.caller();
    let mut took = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..3 {
        let ask = [ResourceAsk {
            address: address.clone(),
            joined: false,
        }];
        let before = host.daemon().metrics().documents_read;
        let since = after.take();
        let readings = read_resources(
            reads.as_ref(),
            &caller,
            &ask,
            since.as_deref(),
            tilo_clock::now(),
        )
        .await
        .expect("the record reads");
        took.push(
            host.daemon()
                .metrics()
                .documents_read
                .saturating_sub(before),
        );
        after = Some(readings.revision);
    }
    took.into_iter().min().unwrap_or_default()
}

/// A refresh reads the record it names and what changed since, so a read at a
/// thousand records opens what one at ten does.
#[test]
fn a_read_of_one_record_opens_what_it_names_however_full_the_workspace() {
    host::run(|host| async move {
        let key = host.key("Refresh");
        let workspace = host.workspace().to_owned();
        let base = format!("/v1/workspaces/{workspace}");
        let mut counted = Vec::new();
        let mut filled = 0;
        for size in SIZES {
            fill(&key, &base, filled, size - filled).await;
            filled = size;
            let address = a_record(&host, &workspace, "so.tilo.tasks").await;
            let reads = reads_of_one_record(&host, address).await;
            eprintln!("{size} records: a read of one opened {reads} documents");
            counted.push((size, reads));
        }
        for (size, reads) in &counted {
            assert!(
                *reads >= 1,
                "{size} records: a read of one opened no document at all: it did not read the \
                 record it named"
            );
            assert!(
                *reads <= CEILING,
                "{size} records: one read opened {reads} documents, more than the {CEILING} the \
                 record it names takes"
            );
        }
        let (small, large) = (counted[0].1, counted[1].1);
        assert!(
            large <= small.saturating_add(1),
            "a full workspace reads one record in {large} documents, a small one in {small}"
        );
    });
}

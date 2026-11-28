//! A job's handler is handed the job as its device recorded it: a `host.Job` argument the API
//! is given as that record reads its id, state and detail.

use serde_json::json;
use tilolang_tilo::publish::PlanRequest;

use crate::fixtures::{Replica, Workspace, action_id, field_id};

const SOURCE: &str = r#"app test.jobs "Jobs"

type Watch {
    seen: Text = ""
    state: Text = ""
    detail: Text = ""
}

root Watch

default view main for Watch

view main(w: Watch) {
    text(w.seen)
}

action ended(w: Watch, j: host.Job) {
    w.seen = j.id
    if j.state == .done {
        w.state = "done"
    }
    w.detail = j.detail
}
"#;

#[test]
fn a_handler_reads_the_job_its_device_recorded() {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(SOURCE, "")]);
    let meta = &packages[0].meta;
    let root = ada.host.root_of("test.jobs").unwrap();
    ada.commit();
    let record = json!({
        "id": "job-7",
        "op": "run",
        "state": "done",
        "detail": {"run": {"exit": 0}},
    });
    let args = json!({ "j": record }).as_object().cloned().unwrap();
    let id = root.id_string();
    let action = action_id(meta, "ended");
    ada.carry_out(&PlanRequest {
        app: "test.jobs",
        kind: "tl.test.jobs.Watch",
        action_id: &action,
        resource: Some(&id),
        args: &args,
        caller: None,
        from: None,
    })
    .unwrap_or_else(|why| panic!("the handler takes the job: {why}"));
    let text = |field: &str| ada.read(&root, &field_id(meta, "Watch", field));
    assert_eq!(text("seen"), tilolang_runtime::Value::text("job-7"));
    assert_eq!(text("state"), tilolang_runtime::Value::text("done"));
    assert_eq!(
        text("detail"),
        tilolang_runtime::Value::text(r#"{"run":{"exit":0}}"#)
    );
}

//! A key the person asks for from a device of theirs (`account/keys#create`), on the device
//! that answers for their account: it waits in their own workspace as their question, they
//! approve it on their own connection there, and the key it answers works. A key of theirs is
//! refused the answer, and a "no" ends the call with a refusal they can read.

use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::ActionProvider as _;

use crate::language::host;

/// What `tool` answers `key`, typed.
async fn structured(key: &host::Key, tool: &str, arguments: Value) -> Value {
    key.tool_structured(tool, &arguments).await["structuredContent"].clone()
}

/// How the invocation `id` stands, as `key` follows it.
async fn status(key: &host::Key, id: &str) -> Value {
    structured(key, "tilo_status", json!({"invocation_id": id})).await
}

/// `key` asks for a key called `name`: its invocation, and the record (`<workspace>/<record>`)
/// it waits in for the person's answer.
async fn asked(key: &host::Key, name: &str) -> (String, String) {
    let started = structured(
        key,
        "tilo_do",
        json!({"path": "account/keys#create", "args": {"name": name}}),
    )
    .await;
    let id = started["invocation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the ask is an invocation to follow: {started}"))
        .to_owned();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = status(key, &id).await;
        if now["status"] == "asked" {
            let record = now["record"]
                .as_str()
                .unwrap_or_else(|| panic!("the ask names where it waits: {now}"));
            return (id, record.to_owned());
        }
        assert_eq!(now["status"], "pending", "it waits for the person: {now}");
        assert!(
            std::time::Instant::now() < deadline,
            "the person was never asked: {now}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The person answers the question in `record` on their own connection to the device, as
/// `tilo do`'s own prompt answers it.
async fn answer(host: &host::Host, record: &str, resolution: &str) {
    let (workspace, record) = record.split_once('/').expect("<workspace>/<record>");
    let (own, person) = host
        .daemon()
        .own_connection_in(workspace)
        .expect("the person's own connection reaches their workspace");
    let receipt = own
        .invoke(
            &person,
            host::call(
                tilo_action_services::workspace_invocations::PROVIDER,
                tilo_actions::ActionAddress::new(record, "resolve"),
                &json!({"resolution": resolution}),
            ),
        )
        .await
        .expect("the dispatch answers")
        .unwrap_or_else(|error| panic!("the person's answer is taken: {error:?}"));
    assert!(
        matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Completed { .. }
        ),
        "the person's answer is done: {receipt:?}"
    );
}

/// How the invocation `id` ended, as `key` follows it.
async fn ended(key: &host::Key, id: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = status(key, id).await;
        if !matches!(now["status"].as_str(), Some("pending" | "asked")) {
            return now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "it never ended: {now}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[test]
fn a_key_asked_for_on_the_persons_device_is_theirs_once_they_approve_it() {
    host::run(|host| async move {
        // The person's own device asks, through the API the daemon serves it.
        let device = host.page_key("Ada's laptop");
        let (id, record) = asked(&device, "Nightly build").await;

        // A key of theirs acts as them, and still answers nothing for them.
        let backend = host.key("Visa backend");
        let (workspace, at) = record.split_once('/').unwrap();
        let (refused, failed) = host::said(
            &backend
                .tool(
                    "tilo_do",
                    &json!({"workspace": workspace,
                            "path": format!("invocations/workspace/{at}#resolve"),
                            "args": {"resolution": "approve"}}),
                )
                .await,
        );
        assert!(failed, "a key answers nothing for its person: {refused}");
        assert!(
            refused.contains(tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM),
            "{refused}"
        );
        assert_eq!(status(&device, &id).await["status"], "asked");

        answer(&host, &record, "approve").await;
        let made = ended(&device, &id).await;
        assert_eq!(made["status"], "ok", "{made}");
        assert_eq!(made["result"]["name"], "Nightly build", "{made}");
        assert_eq!(made["result"]["scope"], "all", "{made}");
        let token = made["result"]["key"]
            .as_str()
            .unwrap_or_else(|| panic!("the key is answered: {made}"));

        // The key works: it is the person's, and reaches their workspaces.
        let nightly = host.key_carried(token);
        let (code, info) = nightly.get("/v1").await;
        assert_eq!(code, 200, "{info}");
        assert_eq!(info["key"]["scope"], "all", "{info}");
        let (code, listed) = nightly
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(code, 200, "{listed}");
        assert!(
            listed.to_string().contains(host.workspace()),
            "the key reaches the person's workspaces: {listed}"
        );

        // A "no" ends the call, and says so.
        let (id, record) = asked(&device, "Unwanted").await;
        answer(&host, &record, "decline").await;
        let declined = ended(&device, &id).await;
        assert_eq!(declined["status"], "failed", "{declined}");
        assert_eq!(declined["error"]["code"], "declined", "{declined}");
        assert!(
            declined["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("declined the key")),
            "{declined}"
        );
    });
}

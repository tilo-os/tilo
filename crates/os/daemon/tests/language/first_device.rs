//! A computer's first start keeps the person's own records: this device is listed in their
//! devices record, and an agent's session opens on it, which certifies the agent's key there.
//!
//! Both are writes to `tilo.devices` from the start itself, so the store a device's service
//! builds has to declare that kind before either lands. A start that declared none refused both
//! with `Undeclared("no type \`tilo.devices\` is declared")`, and the daemon went down with the
//! agent's session.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;

use crate::language::host;

#[test]
fn a_first_start_lists_its_device_and_opens_an_agents_session() {
    host::run(|host| async move {
        let agent = host.agent();
        // The session answers as `tilo mcp` asks it to, so its agent's key is certified in the
        // person's devices record and the session serves.
        let listed = agent.ask("tools/list", json!({})).await;
        assert!(
            listed["result"]["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty()),
            "the agent's session lists its tools: {listed}"
        );
        // The device is in the record as the start keeps it, read back through the API a person's
        // key asks: the write the agent's session made is the one the start made.
        let key = host.key("first start");
        let workspace = host.workspace();
        let asked = format!("/v1/workspaces/{workspace}/resources?path=devices&limit=40");
        let (status, listed) = key.get(&asked).await;
        assert_eq!(status, 200, "the person's devices record reads: {listed}");
        assert!(
            listed["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "the person's devices record lists this device: {listed}"
        );
    });
}

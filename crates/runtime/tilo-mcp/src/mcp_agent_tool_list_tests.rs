//! The tools an agent is offered: few, small, and the same whoever asks.
use super::*;

/// A session is offered what it can use. This server starts no programs, as the one a server
/// takes a person's turn through does not, so `tilo`, the tool for them, is not among its
/// tools: offered it, a model calls it and is refused. A server that starts them offers it
/// (`a_name_read_off_a_listing_walks_back_to_its_resource` asks one).
#[test]
fn an_agent_is_offered_the_small_tools_it_can_use_and_no_classic_ones() {
    let host = Notes::new(true);
    run(async {
        let reply = agent(&host)
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert_eq!(
            names.join(" "),
            "tilo_ls tilo_read tilo_do tilo_link tilo_view tilo_find tilo_watch tilo_ask tilo_status"
        );
        let size = serde_json::to_string(tools).unwrap().len();
        assert!(size < 5_000, "the tool list is {size} bytes");
        // One tool set: a session that names no surface offers the agent's.
        let other = McpServer::new(host.clone(), "workspace".into(), "owner".into())
            .handle(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(tools).unwrap(),
            other["result"]["tools"],
            "one tool set"
        );
        // The old tool is not one of them.
        let refused = agent(&host)
            .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tilo_query","arguments":{}}}))
            .await
            .unwrap();
        assert!(refused["error"].is_object(), "{refused}");
    });
}

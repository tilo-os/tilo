//! Transport-independent MCP projection of the production action broker.
use crate::shell::{
    collab_broker::AppActionBroker,
    collab_broker_capnp::{
        ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse as Reply, BrokerActor,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};

pub struct McpServer {
    broker: AppActionBroker,
    room: String,
    actor: String,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    after_peer: Option<u64>,
    max_results: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    peer_id: u64,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    cursor: Vec<u8>,
    #[serde(default)]
    parent_resource: String,
    #[serde(default)]
    needle: String,
    max_results: Option<u32>,
    max_bytes: Option<u32>,
    max_work: Option<u32>,
    max_depth: Option<u16>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    peer_id: u64,
    revision: String,
    resource: String,
    action: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeArgs {
    peer_id: u64,
    revision: String,
    resource: String,
    action: String,
    arguments: serde_json::Map<String, Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    invocation_id: u64,
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn tool_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => {
            json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value,"isError":false})
        }
        Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
    }
}
impl McpServer {
    #[must_use]
    pub fn new(broker: AppActionBroker, room: String, actor: String) -> Self {
        Self {
            broker,
            room,
            actor,
        }
    }
    pub async fn handle(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let valid_id = id
            .as_ref()
            .is_none_or(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some());
        if !request.is_object()
            || request["jsonrpc"] != "2.0"
            || !request["method"].is_string()
            || !valid_id
        {
            return Some(error(
                if valid_id {
                    id.unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
                -32600,
                "Invalid Request",
            ));
        }
        let Some(id) = id else {
            return None;
        };
        let method = request["method"].as_str().unwrap_or_default();
        let mut params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(error(id, -32602, "params must be an object"));
        }
        // Standard request metadata is protocol context, never action arguments
        // or authority. Progress notifications are not advertised or emitted.
        if let Some(metadata) = params
            .as_object_mut()
            .and_then(|params| params.remove("_meta"))
        {
            if !metadata.is_object() {
                return Some(error(id, -32602, "_meta must be an object"));
            }
            if let Some(token) = metadata.get("progressToken") {
                if !token.is_string() && token.as_i64().is_none() && token.as_u64().is_none() {
                    return Some(error(
                        id,
                        -32602,
                        "progressToken must be a string or integer",
                    ));
                }
            }
        }
        let result = match method {
            "initialize" => {
                let Some(version) = params["protocolVersion"].as_str() else {
                    return Some(error(id, -32602, "protocolVersion is required"));
                };
                let selected = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&version) {
                    version
                } else {
                    "2025-06-18"
                };
                Ok(
                    json!({"protocolVersion":selected,"capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"qos-runtime","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":tool_definitions()})),
            "tools/call" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Call {
                    name: String,
                    #[serde(default = "empty_object")]
                    arguments: Value,
                }
                match parse::<Call>(params) {
                    Ok(call)
                        if tool_definitions().iter().any(|definition| {
                            definition["name"].as_str() == Some(call.name.as_str())
                        }) =>
                    {
                        Ok(tool_result(self.call(&call.name, call.arguments).await))
                    }
                    Ok(_) => return Some(error(id, -32602, "Unknown tool")),
                    Err(e) => Err(e),
                }
            }
            "resources/list" => self.resources(params),
            "resources/read" => self.read_resource(params).await,
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(e) => error(id, -32602, &e),
        })
    }
    fn apps(&self, args: ListArgs) -> Value {
        let page = self.broker.action_directory_root_page(
            args.after_peer,
            args.max_results.unwrap_or(128).clamp(1, 128) as usize,
        );
        let apps=page.roots.into_iter().filter(|r|r.cluster_id==self.room).map(|r|json!({"peer_id":r.peer_id,"app_id":r.app_id,"record_id":r.record_id,"uri":format!("qos://apps/{}",r.peer_id)})).collect::<Vec<_>>();
        json!({"apps":apps,"generation":page.generation,"next_after_peer":page.next_after_peer})
    }
    fn live_peer(&self, peer: u64) -> bool {
        peer > 0
            && self
                .broker
                .action_directory_root_page(peer.checked_sub(1), 1)
                .roots
                .iter()
                .any(|r| r.peer_id == peer && r.cluster_id == self.room)
    }
    async fn query(&self, args: QueryArgs) -> Result<Value, String> {
        if !self.live_peer(args.peer_id) {
            return Err("Unavailable live application instance".into());
        }
        let query = runtime_collab::ActionDirectoryQuery {
            peer_id: args.peer_id,
            revision: args.revision,
            cursor: args.cursor,
            parent_resource: args.parent_resource,
            needle: args.needle,
            max_results: args.max_results.unwrap_or(128).min(128),
            max_bytes: args.max_bytes.unwrap_or(65536).min(65536),
            max_work: args.max_work.unwrap_or(512).min(512),
            max_depth: args.max_depth.unwrap_or(16).min(16),
        };
        let rx = self
            .broker
            .action_directory_request_queue()
            .try_query(BrokerActor::person(&self.actor), &self.room, query)
            .map_err(|e| format!("{e:?}"))?;
        bridge_reply(rx).await
    }
    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "qos_list_apps" => Ok(self.apps(parse(args)?)),
            "qos_query" => self.query(parse(args)?).await,
            "qos_resolve" => {
                let a: TargetArgs = parse(args)?;
                if a.revision.is_empty() {
                    return Err("revision must not be empty".into());
                }
                if !self.live_peer(a.peer_id) {
                    return Err("Unavailable live application instance".into());
                }
                let rx = self
                    .broker
                    .action_directory_request_queue()
                    .try_resolve(
                        BrokerActor::person(&self.actor),
                        &self.room,
                        a.peer_id,
                        a.revision,
                        a.resource,
                        a.action,
                    )
                    .map_err(|e| format!("{e:?}"))?;
                bridge_reply(rx).await
            }
            "qos_invoke" => {
                let a: InvokeArgs = parse(args)?;
                if a.revision.is_empty() {
                    return Err("revision must not be empty".into());
                }
                if !self.live_peer(a.peer_id) {
                    return Err("Unavailable live application instance".into());
                }
                let rx = self
                    .broker
                    .action_directory_request_queue()
                    .try_start_resolved(
                        BrokerActor::person(&self.actor),
                        &self.room,
                        a.peer_id,
                        a.revision,
                        a.resource,
                        a.action,
                        Some(format!(
                            "{}{}",
                            tilo_actions::ARGUMENTS_JSON_PREFIX,
                            Value::Object(a.arguments)
                        )),
                        None,
                    )
                    .map_err(|e| format!("{e:?}"))?;
                bridge_reply(rx).await
            }
            "qos_status" => {
                let a: StatusArgs = parse(args)?;
                self.status(a.invocation_id)
            }
            "qos_cancel" => {
                let a: StatusArgs = parse(args)?;
                self.broker.cancel(a.invocation_id, &self.actor)?;
                self.status(a.invocation_id)
            }
            _ => Err("Unknown tool".into()),
        }
    }
    fn status(&self, id: u64) -> Result<Value, String> {
        self.broker
            .statuses(&self.actor)
            .into_iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, update)| json!({"invocation_id":id,"update":update}))
            .ok_or_else(|| "Unknown or unowned invocation".into())
    }
    fn resources(&self, params: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            cursor: Option<String>,
        }
        let a: Args = parse(params)?;
        let after_peer = a
            .cursor
            .map(|s| {
                s.parse::<u64>()
                    .map_err(|_| "Invalid resource cursor".to_string())
            })
            .transpose()?;
        let apps = self.apps(ListArgs {
            after_peer,
            max_results: None,
        });
        let resources=apps["apps"].as_array().into_iter().flatten().map(|a|json!({"uri":a["uri"],"name":format!("{} ({})",a["app_id"].as_str().unwrap_or_default(),a["peer_id"]),"mimeType":"application/json"})).collect::<Vec<_>>();
        let mut result = json!({"resources":resources,"generation":apps["generation"]});
        if let Some(next) = apps["next_after_peer"].as_u64() {
            result["nextCursor"] = json!(next.to_string());
        }
        Ok(result)
    }
    async fn read_resource(&self, params: Value) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            uri: String,
        }
        let a: Args = parse(params)?;
        let peer = a
            .uri
            .strip_prefix("qos://apps/")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("Invalid application URI")?;
        if a.uri != format!("qos://apps/{peer}") {
            return Err("Invalid application URI".into());
        }
        let value = self.query(parse(json!({"peer_id":peer}))?).await?;
        Ok(
            json!({"contents":[{"uri":a.uri,"mimeType":"application/json","text":value.to_string()}]}),
        )
    }
}
fn empty_object() -> Value {
    json!({})
}
async fn bridge_reply(
    rx: tokio::sync::oneshot::Receiver<ActionDirectoryBridgeReply>,
) -> Result<Value, String> {
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| "TimedOut; admission may be unknown; do not replay mutations".to_string())?
        .map_err(|_| "Unavailable".to_string())?
        .map_err(|e| format!("{e:?}"))?;
    match reply {
        Reply::Page(page) => Ok(
            json!({"revision":page.revision,"rows":page.rows.iter().map(row_json).collect::<Result<Vec<_>,_>>()?,"next_cursor":page.next_cursor,"exhausted":page.exhausted,"stale_cursor":page.stale_cursor,"budget_exhausted":page.budget_exhausted}),
        ),
        Reply::Resolution(runtime_collab::ActionDirectoryResolution::Found(row)) => {
            Ok(json!({"row":row_json(&row)?}))
        }
        Reply::Resolution(other) => Err(format!("{other:?}")),
        Reply::StartedDirectoryAction(start) => {
            Ok(json!({"invocation_id":start.invocation_id,"update":start.initial_update}))
        }
    }
}
fn row_json(row: &runtime_collab::ActionDirectoryRow) -> Result<Value, String> {
    let descriptor=row.descriptor.as_ref().map(|d|Ok::<Value,String>(json!({"resource":d.resource,"action":d.action,"resource_kind":d.resource_kind,"resource_label":d.resource_label,"action_label":d.action_label,"description":d.description,"operation_kind":d.operation_kind,"params":serde_json::from_str::<Value>(&d.params_json).map_err(|e|e.to_string())?,"contract":serde_json::from_str::<Value>(&d.contract_json).map_err(|e|e.to_string())?}))).transpose()?;
    let visibility_contract = if row.visibility_contract_json.is_empty() {
        Value::Null
    } else {
        serde_json::from_str::<Value>(&row.visibility_contract_json).map_err(|e| e.to_string())?
    };
    Ok(
        json!({"parent_resource":row.parent_resource,"resource":row.resource,"resource_kind":row.resource_kind,"resource_label":row.resource_label,"action":row.action,"descriptor":descriptor,"has_children":row.has_children,"visibility_action":row.visibility_action,"visibility_contract":visibility_contract}),
    )
}
fn tool_definitions() -> Vec<Value> {
    let integer = json!({"type":"integer","minimum":0});
    let string = json!({"type":"string"});
    let target = json!({"peer_id":integer,"revision":string,"resource":string,"action":string});
    let mut invoke = target.clone();
    invoke["arguments"] = json!({"type":"object"});
    [
        ("qos_list_apps","List a bounded page of live application instances",json!({"after_peer":integer,"max_results":{"type":"integer","minimum":1,"maximum":128}}),json!([])),
        ("qos_query","Browse a bounded live directory page; preserve revision and cursor",json!({"peer_id":integer,"revision":string,"cursor":{"type":"array","items":{"type":"integer","minimum":0,"maximum":255}},"parent_resource":string,"needle":string,"max_results":integer,"max_bytes":integer,"max_work":integer,"max_depth":integer}),json!(["peer_id"])),
        ("qos_resolve","Resolve an exact current action contract",target,json!(["peer_id","revision","resource","action"])),
        ("qos_invoke","Atomically resolve and start an action; accepted is not succeeded",invoke,json!(["peer_id","revision","resource","action","arguments"])),
        ("qos_status","Read an owned invocation's canonical update",json!({"invocation_id":integer}),json!(["invocation_id"])),
        ("qos_cancel","Request cancellation of an owned invocation and read its update",json!({"invocation_id":integer}),json!(["invocation_id"])),
    ].into_iter().map(|(name,description,properties,required)|json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

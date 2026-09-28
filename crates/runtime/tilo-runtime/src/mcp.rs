//! Transport-independent MCP projection of the production action broker.
use serde::Deserialize;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost, Caller, CatalogueLimits, CataloguePage, CatalogueQuery, CatalogueRow,
    InvokeArgument, InvokeRequest, RemoteActionTarget,
};

pub struct McpServer<H> {
    host: H,
    room: String,
    actor: String,
    /// The agent run every invocation of this connection is attributed to.
    run: Option<tilo_actions::ActionGroup>,
    /// Pixel fallback, when the host runs beside a compositor.
    computer: Option<std::sync::Arc<crate::computer::Computer>>,
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
    peer_id: Option<u64>,
    #[serde(default)]
    workspace: bool,
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
    peer_id: Option<u64>,
    #[serde(default)]
    workspace: bool,
    revision: String,
    resource: String,
    action: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeArgs {
    peer_id: Option<u64>,
    #[serde(default)]
    workspace: bool,
    revision: String,
    resource: String,
    action: String,
    arguments: serde_json::Map<String, Value>,
    /// Caller-chosen id; retrying with the same id never starts twice.
    #[serde(default)]
    invocation_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusArgs {
    invocation_id: String,
}
fn invocation_id(value: &str) -> Result<tilo_actions::InvocationId, String> {
    uuid::Uuid::parse_str(value)
        .map(tilo_actions::InvocationId)
        .map_err(|_| "invocation_id must be a UUID".to_string())
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}
fn error(id: &Value, code: i32, message: &str) -> Value {
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
impl<H: ActionHost + Send + Sync> McpServer<H> {
    #[must_use]
    pub fn new(host: H, room: String, actor: String) -> Self {
        Self {
            host,
            room,
            actor,
            run: None,
            computer: None,
        }
    }
    /// Offer the canonical `computer` tools, driving `computer`.
    #[must_use]
    pub fn with_computer(
        mut self,
        computer: Option<std::sync::Arc<crate::computer::Computer>>,
    ) -> Self {
        self.computer = computer;
        self
    }
    fn tools(&self) -> Vec<Value> {
        let mut tools = tool_definitions();
        if self.computer.is_some() {
            tools.extend(crate::computer::tool_definitions());
        }
        tools
    }
    /// Run a `computer` or `computer_batch` call off the async runtime.
    async fn computer(&self, name: &str, args: Value) -> Value {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Batch {
            actions: Vec<crate::computer::Action>,
        }
        let Some(computer) = self.computer.clone() else {
            return tool_result(Err("Unknown tool".into()));
        };
        let batch = name == "computer_batch";
        let actions = if batch {
            parse::<Batch>(args).map(|batch| batch.actions)
        } else {
            parse::<crate::computer::Action>(args).map(|action| vec![action])
        };
        let actions = match actions {
            Ok(actions) => actions,
            Err(error) => return tool_result(Err(error)),
        };
        match tokio::task::spawn_blocking(move || computer.run(&actions, batch)).await {
            Ok(Ok(outcomes)) => crate::computer::content(&outcomes, batch),
            Ok(Err(error)) => tool_result(Err(error)),
            Err(error) => tool_result(Err(error.to_string())),
        }
    }
    /// Attribute every invocation of this connection to one agent run, so
    /// history, undo and activity tell concurrent agents apart. Provenance
    /// only: authority stays the profile actor's.
    #[must_use]
    pub fn with_agent_run(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.run = Some(tilo_actions::ActionGroup {
            id: id.into(),
            label: label.into(),
            kind: "agent-run".into(),
        });
        self
    }
    pub async fn handle(&mut self, request: Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let valid_id = id
            .as_ref()
            .is_none_or(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some());
        if !request.is_object()
            || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !request.get("method").is_some_and(Value::is_string)
            || !valid_id
        {
            let reply_id = if valid_id {
                id.unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            return Some(error(&reply_id, -32600, "Invalid Request"));
        }
        let id = id?;
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            return Some(error(&id, -32602, "params must be an object"));
        }
        // Standard request metadata is protocol context, never action arguments
        // or authority. Progress notifications are not advertised or emitted.
        if let Some(metadata) = params
            .as_object_mut()
            .and_then(|params| params.remove("_meta"))
        {
            if !metadata.is_object() {
                return Some(error(&id, -32602, "_meta must be an object"));
            }
            if let Some(token) = metadata.get("progressToken")
                && !token.is_string()
                && token.as_i64().is_none()
                && token.as_u64().is_none()
            {
                return Some(error(
                    &id,
                    -32602,
                    "progressToken must be a string or integer",
                ));
            }
        }
        let result = match method {
            "initialize" => {
                let Some(version) = params.get("protocolVersion").and_then(Value::as_str) else {
                    return Some(error(&id, -32602, "protocolVersion is required"));
                };
                let selected = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&version) {
                    version
                } else {
                    "2025-06-18"
                };
                Ok(
                    json!({"protocolVersion":selected,"capabilities":{"tools":{},"resources":{}},"serverInfo":{"name":"tilo-runtime","version":env!("CARGO_PKG_VERSION")}}),
                )
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools":self.tools()})),
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
                        if self.computer.is_some()
                            && matches!(call.name.as_str(), "computer" | "computer_batch") =>
                    {
                        Ok(self.computer(&call.name, call.arguments).await)
                    }
                    Ok(call)
                        if tool_definitions().iter().any(|definition| {
                            definition["name"].as_str() == Some(call.name.as_str())
                        }) =>
                    {
                        Ok(tool_result(self.call(&call.name, call.arguments).await))
                    }
                    Ok(_) => return Some(error(&id, -32602, "Unknown tool")),
                    Err(e) => Err(e),
                }
            }
            "resources/list" => self.resources(params).await,
            "resources/read" => self.read_resource(params).await,
            _ => return Some(error(&id, -32601, "Method not found")),
        };
        Some(match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(e) => error(&id, -32602, &e),
        })
    }
    /// One catalogue page from the action host, as this bridge's caller.
    async fn ask(&self, query: CatalogueQuery) -> Result<CataloguePage, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.host().query(&self.caller(), query),
        )
        .await
        .map_err(|_| "TimedOut".to_string())?
        .ok_or_else(|| "Unavailable".to_string())?
        .map_err(|error| error.to_string())
    }
    /// Live, browsable application instances in this bridge's workspace,
    /// ordered by instance id.
    async fn instances(&self) -> Result<(String, Vec<(u64, Value)>), String> {
        let page = self
            .ask(CatalogueQuery {
                max_results: 1,
                ..CatalogueQuery::default()
            })
            .await?;
        let mut apps = page
            .providers
            .into_iter()
            .filter(|provider| provider.browsable && provider.room == self.room)
            .filter_map(|provider| {
                let peer = provider.provider.parse::<u64>().ok()?;
                Some((
                    peer,
                    json!({"peer_id":peer,"app_id":provider.app_id,"record_id":provider.record_id,"uri":format!("tilo://apps/{peer}")}),
                ))
            })
            .collect::<Vec<_>>();
        apps.sort_by_key(|(peer, _)| *peer);
        Ok((page.revision, apps))
    }
    async fn apps(&self, args: ListArgs) -> Result<Value, String> {
        let (generation, apps) = self.instances().await?;
        let limit = usize::try_from(args.max_results.unwrap_or(128).clamp(1, 128)).unwrap_or(128);
        let mut remaining = apps
            .into_iter()
            .filter(|(peer, _)| args.after_peer.is_none_or(|after| *peer > after));
        let page = remaining.by_ref().take(limit).collect::<Vec<_>>();
        let next_after_peer = remaining
            .next()
            .and_then(|_| page.last().map(|(peer, _)| *peer));
        let apps = page.into_iter().map(|(_, app)| app).collect::<Vec<_>>();
        Ok(json!({"apps":apps,"generation":generation,"next_after_peer":next_after_peer}))
    }
    async fn live_peer(&self, peer: u64) -> Result<(), String> {
        if self
            .instances()
            .await?
            .1
            .iter()
            .any(|(candidate, _)| *candidate == peer)
        {
            Ok(())
        } else {
            Err("Unavailable live application instance".into())
        }
    }
    /// The provider a call addresses: one live application instance, or the
    /// workspace itself (installed applications to open, shortcuts, …).
    async fn provider(&self, peer_id: Option<u64>, workspace: bool) -> Result<String, String> {
        match (peer_id, workspace) {
            (None, true) => Ok(tilo_actions::remote::workspace_provider(&self.room)),
            (Some(peer), false) => {
                self.live_peer(peer).await?;
                Ok(peer.to_string())
            }
            _ => Err("name exactly one of peer_id or workspace: true".into()),
        }
    }
    async fn query(&self, args: QueryArgs) -> Result<Value, String> {
        let provider = self.provider(args.peer_id, args.workspace).await?;
        let page = self
            .ask(CatalogueQuery {
                provider: Some(provider),
                parent_resource: (!args.parent_resource.is_empty()).then_some(args.parent_resource),
                needle: args.needle,
                target: None,
                revision: (!args.revision.is_empty()).then_some(args.revision),
                cursor: args.cursor,
                max_results: args.max_results.unwrap_or(128).min(128),
                limits: Some(CatalogueLimits {
                    max_bytes: Some(args.max_bytes.unwrap_or(65536).min(65536)),
                    max_work: Some(args.max_work.unwrap_or(512).min(512)),
                    max_depth: Some(args.max_depth.unwrap_or(16).min(16)),
                }),
            })
            .await?;
        Ok(page_json(&page))
    }
    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "tilo_list_apps" => self.apps(parse(args)?).await,
            "tilo_query" => self.query(parse(args)?).await,
            "tilo_resolve" => {
                let a: TargetArgs = parse(args)?;
                if a.revision.is_empty() {
                    return Err("revision must not be empty".into());
                }
                let target = RemoteActionTarget {
                    provider: self.provider(a.peer_id, a.workspace).await?,
                    address: tilo_actions::ActionAddress::new(a.resource, a.action),
                };
                let page = self
                    .ask(CatalogueQuery::resolve(&target, a.revision))
                    .await?;
                if page.stale_cursor {
                    return Err("StaleRevision".into());
                }
                page.rows
                    .first()
                    .map(|row| json!({"row":row_json(row)}))
                    .ok_or_else(|| "NotFound".into())
            }
            "tilo_invoke" => {
                let a: InvokeArgs = parse(args)?;
                if a.revision.is_empty() {
                    return Err("revision must not be empty".into());
                }
                let provider = self.provider(a.peer_id, a.workspace).await?;
                let invocation = a
                    .invocation_id
                    .as_deref()
                    .map_or_else(|| Ok(tilo_actions::InvocationId::new()), invocation_id)?;
                let target = RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new(a.resource, a.action),
                };
                let mut request = InvokeRequest {
                    invocation,
                    target,
                    argument: InvokeArgument::Json(a.arguments),
                    context: self.context(),
                    // The MCP client admitted this call under its own
                    // allow/deny policy; that decision is the confirmation.
                    confirmation: Some(tilo_actions::remote::Confirmation::Confirmed),
                    revision: Some(a.revision),
                };
                if let Err(error) = self.start(request.clone()).await {
                    if error
                        != tilo_actions::remote::ActionHostError::ConfirmationRequired.to_string()
                    {
                        return Err(error);
                    }
                    // Only an exact-phrase contract refuses the plain
                    // confirmation; answer with the phrase it names.
                    let revision = request.revision.clone().unwrap_or_default();
                    match self
                        .contract_confirmation(&request.target, &revision)
                        .await?
                    {
                        tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                            request.confirmation =
                                Some(tilo_actions::remote::Confirmation::Phrase(phrase));
                            self.start(request).await?;
                        }
                        _ => return Err(error),
                    }
                }
                self.status(invocation)
            }
            "tilo_status" => {
                let a: StatusArgs = parse(args)?;
                self.status(invocation_id(&a.invocation_id)?)
            }
            "tilo_cancel" => {
                let a: StatusArgs = parse(args)?;
                let invocation = invocation_id(&a.invocation_id)?;
                self.host()
                    .cancel(&self.caller(), &invocation)
                    .map_err(|error| error.to_string())?;
                self.status(invocation)
            }
            _ => Err("Unknown tool".into()),
        }
    }
    /// The action host every caller shares; this bridge is one more caller.
    const fn host(&self) -> &H {
        &self.host
    }
    /// Start one invocation, reporting the host's refusal verbatim.
    async fn start(&self, request: InvokeRequest) -> Result<(), String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.host().invoke(&self.caller(), request),
        )
        .await
        .map_err(|_| {
            "TimedOut; admission may be unknown; retry with the same invocation_id".to_string()
        })?
        .ok_or_else(|| "Unavailable".to_string())?
        .map(|_| ())
        .map_err(|error| error.to_string())
    }
    /// The confirmation the target's current contract asks for.
    async fn contract_confirmation(
        &self,
        target: &RemoteActionTarget,
        revision: &str,
    ) -> Result<tilo_actions::ConfirmationPolicy, String> {
        let page = self
            .ask(CatalogueQuery::resolve(target, revision.to_owned()))
            .await?;
        if page.stale_cursor {
            return Err("StaleRevision".into());
        }
        page.rows
            .first()
            .and_then(|row| row.action.as_ref())
            .map(|action| action.contract.advertised_confirmation())
            .ok_or_else(|| "NotFound".into())
    }
    fn context(&self) -> Option<tilo_actions::ActionContext> {
        self.run.as_ref().map(|run| tilo_actions::ActionContext {
            group: Some(run.clone()),
            invoker: Some(tilo_actions::ActionInvoker::actor(self.actor.clone())),
            ..tilo_actions::ActionContext::default()
        })
    }
    /// The fixed profile actor, confined to the profile's workspace.
    fn caller(&self) -> Caller {
        Caller::local_person(self.actor.clone()).in_room(self.room.clone())
    }
    fn status(&self, id: tilo_actions::InvocationId) -> Result<Value, String> {
        self.host()
            .observe(&self.caller(), &id)
            .map(|state| json!({"invocation_id":id.0.to_string(),"update":state.update}))
            .ok_or_else(|| "Unknown or unowned invocation".into())
    }
    async fn resources(&self, params: Value) -> Result<Value, String> {
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
        let apps = self
            .apps(ListArgs {
                after_peer,
                max_results: None,
            })
            .await?;
        let resources = apps
            .get("apps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|a| {
                json!({
                    "uri": a.get("uri"),
                    "name": format!(
                        "{} ({})",
                        a.get("app_id").and_then(Value::as_str).unwrap_or_default(),
                        a.get("peer_id").unwrap_or(&Value::Null)
                    ),
                    "mimeType": "application/json"
                })
            })
            .collect::<Vec<_>>();
        let mut result = json!({"resources":resources,"generation":apps.get("generation")});
        if let Some(next) = apps.get("next_after_peer").and_then(Value::as_u64)
            && let Some(fields) = result.as_object_mut()
        {
            fields.insert("nextCursor".to_owned(), json!(next.to_string()));
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
            .strip_prefix("tilo://apps/")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or("Invalid application URI")?;
        if a.uri != format!("tilo://apps/{peer}") {
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
fn page_json(page: &CataloguePage) -> Value {
    json!({"revision":page.revision,"rows":page.rows.iter().map(row_json).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"exhausted":page.exhausted,"stale_cursor":page.stale_cursor,"budget_exhausted":page.budget_exhausted})
}
fn row_json(row: &CatalogueRow) -> Value {
    let descriptor = row.action.as_ref().map(|action| {
        json!({"resource":action.address.resource,"action":action.address.action,"resource_kind":action.resource_kind,"resource_label":action.resource_label,"action_label":action.action_label,"description":action.description,"operation_kind":action.kind,"params":action.params,"contract":action.contract})
    });
    json!({
        "parent_resource":row.parent_resource,
        "resource":row.resource.id,
        "resource_kind":row.resource.kind,
        "resource_label":row.resource.label,
        "action":row.action.as_ref().map(|action| &action.address.action),
        "descriptor":descriptor,
        "has_children":row.has_children,
        "visibility_action":row.visibility.as_ref().map(|visibility| &visibility.action),
        "visibility_contract":row.visibility.as_ref().map(|visibility| &visibility.contract),
    })
}
fn tool_definitions() -> Vec<Value> {
    let integer = json!({"type":"integer","minimum":0});
    let string = json!({"type":"string"});
    let workspace = json!({"type":"boolean","description":"Address the workspace itself instead of a live app: installed applications to open (resource app.<id>, action open), shortcuts and workspace settings"});
    let target = json!({"peer_id":integer,"workspace":workspace,"revision":string,"resource":string,"action":string});
    let mut invoke = target.clone();
    if let Some(properties) = invoke.as_object_mut() {
        properties.insert("arguments".into(), json!({"type":"object"}));
        properties.insert(
            "invocation_id".into(),
            json!({"type":"string","format":"uuid"}),
        );
    }
    let read = json!({"readOnlyHint":true,"openWorldHint":false});
    [
        ("tilo_list_apps","List a bounded page of live application instances. To start an app that is not running, tilo_query with workspace: true and invoke its open action",json!({"after_peer":integer,"max_results":{"type":"integer","minimum":1,"maximum":128}}),json!([]),read.clone()),
        ("tilo_query","Browse a bounded live directory page; preserve revision and cursor. Each action's contract carries its risk level",json!({"peer_id":integer,"workspace":workspace,"revision":string,"cursor":{"type":"array","items":{"type":"integer","minimum":0,"maximum":255}},"parent_resource":string,"needle":string,"max_results":integer,"max_bytes":integer,"max_work":integer,"max_depth":integer}),json!([]),read.clone()),
        ("tilo_resolve","Resolve an exact current action contract",target,json!(["revision","resource","action"]),read.clone()),
        ("tilo_invoke","Atomically resolve and start an action; accepted is not succeeded. Retry with the same invocation_id to never start twice. Allowing this call confirms the action, including destructive ones; check the contract's risk first",invoke,json!(["revision","resource","action","arguments"]),json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true})),
        ("tilo_status","Read an owned invocation's canonical update",json!({"invocation_id":string}),json!(["invocation_id"]),read),
        ("tilo_cancel","Request cancellation of an owned invocation and read its update",json!({"invocation_id":string}),json!(["invocation_id"]),json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})),
    ].into_iter().map(|(name,description,properties,required,annotations)|json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":annotations})).collect()
}
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

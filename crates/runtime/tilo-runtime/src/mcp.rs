//! Transport-independent MCP projection of the production action broker.
#[path = "mcp_content.rs"]
mod content;
#[path = "mcp_page.rs"]
mod page;
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
    /// One transport-issued execution scope, independent of history labels.
    caller: Caller,
    /// The agent run every invocation of this connection is attributed to.
    run: Option<tilo_actions::ActionGroup>,
    /// Pixel fallback, when the host runs beside a compositor.
    computer: Option<std::sync::Arc<crate::computer::Computer>>,
    /// The page `tilo_query` answers when a call names no `detail`; a client
    /// that parses v1 rows pins `full` for its session at `initialize`.
    detail: Detail,
    /// The short names compact pages gave long resource ids.
    handles: std::sync::Mutex<page::Handles>,
    open_files: std::sync::Arc<OpenFiles>,
    /// Where answers read the content an action produced, to show it.
    media: Option<std::sync::Arc<dyn crate::MediaReader>>,
    /// The app each of this session's invocations went to, so a later
    /// `tilo_status` reads produced content with that app's scope.
    produced_by: std::sync::Mutex<std::collections::HashMap<tilo_actions::InvocationId, String>>,
    /// The images this session has been shown, by content hash, with the
    /// invocation that showed each: the same picture again is named, not
    /// sent again.
    shown: std::sync::Mutex<std::collections::HashMap<String, tilo_actions::InvocationId>>,
}

/// Where an answer carries the images it shows until [`tool_result`] moves
/// them out of the JSON and into MCP image blocks. Not a valid JSON key an
/// app could produce in an output.
const IMAGES: &str = "\u{1}images";
/// At most this many images answer one action.
const MAX_IMAGES: usize = 4;
/// Larger content is named, not shown.
const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Pictures one session remembers having shown.
const MAX_SHOWN: usize = 256;

/// The MIME type of image bytes an MCP client can show.
fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

/// The images an answer carries, taken out of it: its own, and those of
/// each result of a batch.
fn take_images(value: &mut Value) -> Vec<Value> {
    let mut images = Vec::new();
    if let Some(fields) = value.as_object_mut() {
        if let Some(Value::Array(own)) = fields.remove(IMAGES) {
            images.extend(own);
        }
        if let Some(Value::Array(results)) = fields.get_mut("results") {
            for result in results {
                images.extend(take_images(result));
            }
        }
    }
    images
}

/// The instance each file last went to through an app's own `open` or
/// `import-<extension>`, shared by every session of an endpoint: opening
/// that file again returns that instance instead of starting another that
/// opens it a second time.
#[derive(Default)]
pub struct OpenFiles(std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, u64>>);

impl OpenFiles {
    const MAX: usize = 1024;

    fn key(path: &str) -> std::path::PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.into())
    }

    fn files(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<std::path::PathBuf, u64>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record(&self, path: &str, peer: u64) {
        let mut files = self.files();
        if files.len() >= Self::MAX {
            files.clear();
        }
        files.insert(Self::key(path), peer);
    }

    fn peer(&self, path: &str) -> Option<u64> {
        self.files().get(&Self::key(path)).copied()
    }
}

/// The path an app's own file-opening action was given, by the directory's
/// convention [`McpServer::path_opener`] follows.
fn opened_file<'a>(action: &str, arguments: &'a serde_json::Map<String, Value>) -> Option<&'a str> {
    (action == tilo_actions::remote::APPLICATION_OPEN_ACTION || action.starts_with("import-"))
        .then(|| arguments.get("path").and_then(Value::as_str))
        .flatten()
        .filter(|path| !path.is_empty())
}

/// The action an app declared as reading its open document's first page,
/// on the first resource of `page` that offers one.
fn document_reader(page: &CataloguePage) -> Option<tilo_actions::ActionAddress> {
    page.rows
        .iter()
        .filter_map(|row| row.action.as_ref())
        .find(|action| action.contract.reads_document())
        .map(|action| action.address.clone())
}

/// Whether an invocation's answer leaves open that it did what it asked: it
/// succeeded, or is still running when the call stopped waiting.
fn may_have_opened(value: &Value) -> bool {
    !matches!(
        value.pointer("/update/state").and_then(Value::as_str),
        Some("failed" | "cancelled") | None
    )
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
    app_id: Option<String>,
    #[serde(default)]
    workspace: bool,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    cursor: Cursor,
    #[serde(default)]
    parent_resource: String,
    #[serde(default)]
    needle: String,
    max_results: Option<u32>,
    max_bytes: Option<u32>,
    max_work: Option<u32>,
    max_depth: Option<u16>,
    detail: Option<Detail>,
}
/// A continuation as a page issued it: the compact page's string, or the
/// full page's byte array.
#[derive(Deserialize)]
#[serde(untagged)]
enum Cursor {
    Bytes(Vec<u8>),
    Text(String),
}
impl Default for Cursor {
    fn default() -> Self {
        Self::Bytes(Vec::new())
    }
}
impl Cursor {
    fn into_bytes(self) -> Result<Vec<u8>, String> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Text(text) => page::cursor_bytes(&text),
        }
    }
}
/// How much of each row a page carries.
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Detail {
    /// Rows grouped by resource, each action a one-line signature.
    #[default]
    Compact,
    /// Every row with its descriptor and contracts, as first published.
    Full,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArgs {
    peer_id: Option<u64>,
    app_id: Option<String>,
    #[serde(default)]
    workspace: bool,
    /// Omitted: the directory as it is now.
    revision: Option<String>,
    resource: String,
    action: String,
}
/// One action inside a `calls` batch. It runs on the call's peer.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct InvokeCall {
    resource: String,
    action: String,
    #[serde(default)]
    arguments: serde_json::Map<String, Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeArgs {
    peer_id: Option<u64>,
    app_id: Option<String>,
    #[serde(default)]
    workspace: bool,
    /// Omitted: the directory as it is now.
    revision: Option<String>,
    /// The single-call form. Absent when `calls` names the batch.
    resource: Option<String>,
    action: Option<String>,
    #[serde(default)]
    arguments: serde_json::Map<String, Value>,
    /// Several actions in order, on this same peer. Stops at the first
    /// failure; the single-call form leaves this absent.
    calls: Option<Vec<InvokeCall>>,
    /// Caller-chosen id; retrying with the same id never starts twice.
    #[serde(default)]
    invocation_id: Option<String>,
    /// How long to wait for a terminal update before answering with the
    /// current one.
    wait_ms: Option<u64>,
}
/// One action of either form, after the batch has been split apart.
struct OneCall {
    peer_id: Option<u64>,
    app_id: Option<String>,
    workspace: bool,
    revision: Option<String>,
    resource: String,
    action: String,
    arguments: serde_json::Map<String, Value>,
    invocation_id: Option<String>,
    wait_ms: Option<u64>,
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
        Ok(mut value) => {
            let images = take_images(&mut value);
            let mut content = vec![json!({"type":"text","text":value.to_string()})];
            content.extend(images);
            json!({"content":content,"structuredContent":value,"isError":false})
        }
        Err(message) => json!({"content":[{"type":"text","text":message}],"isError":true}),
    }
}
impl<H: ActionHost + Send + Sync> McpServer<H> {
    #[must_use]
    pub fn new(host: H, room: String, actor: String) -> Self {
        let caller = Caller::local_person(actor.clone())
            .in_room(room.clone())
            .in_new_execution();
        Self {
            host,
            room,
            actor,
            caller,
            run: None,
            computer: None,
            detail: Detail::Compact,
            handles: std::sync::Mutex::default(),
            open_files: std::sync::Arc::default(),
            media: None,
            produced_by: std::sync::Mutex::default(),
            shown: std::sync::Mutex::default(),
        }
    }
    /// Answer an action that produced image content with the image itself,
    /// read from `media` with the producing app's own scope.
    #[must_use]
    pub fn with_media(mut self, media: Option<std::sync::Arc<dyn crate::MediaReader>>) -> Self {
        self.media = media;
        self
    }
    fn produced_by(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<tilo_actions::InvocationId, String>>
    {
        self.produced_by
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Remember that `invocation` went to `provider`'s app.
    async fn record_producer(&self, invocation: tilo_actions::InvocationId, provider: &str) {
        if self.media.is_none() {
            return;
        }
        let Some(app_id) = self.app_of(provider).await else {
            return;
        };
        let mut produced = self.produced_by();
        if produced.len() >= 256 {
            produced.clear();
        }
        produced.insert(invocation, app_id);
    }
    /// The app id of the live instance `provider`.
    async fn app_of(&self, provider: &str) -> Option<String> {
        let peer = provider.parse::<u64>().ok()?;
        self.instances()
            .await
            .ok()?
            .1
            .into_iter()
            .find(|(id, _)| *id == peer)
            .and_then(|(_, app)| app.get("app_id")?.as_str().map(str::to_owned))
    }
    fn shown(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, tilo_actions::InvocationId>>
    {
        self.shown
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// `hashes` split into those to send, now recorded as shown by
    /// `invocation`, and the earlier invocations that showed the rest.
    fn not_yet_shown(
        &self,
        hashes: Vec<String>,
        invocation: tilo_actions::InvocationId,
        again: bool,
    ) -> (Vec<String>, Vec<tilo_actions::InvocationId>) {
        let mut shown = self.shown();
        let mut fresh = Vec::new();
        let mut unchanged = Vec::new();
        for hash in hashes {
            match shown.get(&hash) {
                Some(earlier) if !again && *earlier != invocation => unchanged.push(*earlier),
                _ => {
                    if shown.len() >= MAX_SHOWN {
                        shown.clear();
                    }
                    shown.insert(hash.clone(), invocation);
                    fresh.push(hash);
                }
            }
        }
        drop(shown);
        (fresh, unchanged)
    }
    /// Whether `target` is an effect operation, as its provider describes
    /// it now. Asked only when an answer carries pictures.
    async fn is_effect(&self, target: &RemoteActionTarget) -> bool {
        let Ok(revision) = self.current_revision(&target.provider).await else {
            return false;
        };
        self.ask(CatalogueQuery::resolve(target, revision))
            .await
            .ok()
            .and_then(|page| page.rows.into_iter().next())
            .and_then(|row| row.action)
            .is_some_and(|action| action.kind == tilo_actions::OperationKind::Effect)
    }
    /// Carry the images a succeeded `invocation` produced in `value`, under
    /// [`IMAGES`], for [`tool_result`] to show. With `again` false, a
    /// picture this session was already shown is named in `unchanged`
    /// instead: a model pays for every image it is sent, and a look at an
    /// unchanged artwork is the same picture. `tilo_status` on the earlier
    /// invocation shows it again.
    ///
    /// With `target`, the action that ran: an effect's pictures (an export)
    /// are artifacts it made, not something to look at, so the answer names
    /// them by address and only `tilo_status` shows them.
    async fn attach_images(
        &self,
        invocation: tilo_actions::InvocationId,
        value: &mut Value,
        again: bool,
        target: Option<&RemoteActionTarget>,
    ) {
        let Some(media) = self.media.clone() else {
            return;
        };
        if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
            return;
        }
        let hashes: Vec<String> = value
            .pointer("/update/output/resources")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(tilo_actions::content_hash)
            .take(MAX_IMAGES)
            .map(str::to_owned)
            .collect();
        let Some(app_id) = self.produced_by().get(&invocation).cloned() else {
            return;
        };
        if hashes.is_empty() {
            return;
        }
        if let Some(target) = target
            && self.is_effect(target).await
        {
            return;
        }
        let (hashes, unchanged) = self.not_yet_shown(hashes, invocation, again);
        if let Some(earlier) = unchanged.first()
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert(
                "unchanged".into(),
                format!(
                    "the same picture invocation {} showed, so it is not sent again; tilo_status on that invocation shows it",
                    earlier.0
                )
                .into(),
            );
        }
        if hashes.is_empty() {
            return;
        }
        let read = tokio::task::spawn_blocking(move || {
            hashes
                .iter()
                .filter_map(|hash| media.read(&app_id, hash).ok().flatten())
                .filter(|bytes| bytes.len() <= MAX_IMAGE_BYTES)
                .filter_map(|bytes| {
                    use base64::Engine as _;
                    let mime = image_mime(&bytes)?;
                    Some(json!({"type":"image","mimeType":mime,"data":base64::engine::general_purpose::STANDARD.encode(&bytes)}))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if !read.is_empty()
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert(IMAGES.into(), Value::Array(read));
        }
    }
    /// Share which instance holds which file with the endpoint's other
    /// sessions.
    #[must_use]
    pub fn with_open_files(mut self, open_files: std::sync::Arc<OpenFiles>) -> Self {
        self.open_files = open_files;
        self
    }
    fn handles(&self) -> std::sync::MutexGuard<'_, page::Handles> {
        self.handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The resource id a caller named: a handle a compact page issued, or a
    /// full id.
    fn full_id(&self, name: String) -> String {
        if name.is_empty() {
            return name;
        }
        self.handles().full(&name)
    }
    /// The resource `target` means when its resource names none: a
    /// resource of the provider's whose kind or label it is (`canvas.artwork`,
    /// `Canvas`), or, shaped like a handle another connection issued, what
    /// to name instead. `None` when it is neither.
    async fn named_resource(&self, target: &RemoteActionTarget) -> Option<Result<String, String>> {
        let name = &target.address.resource;
        if name.is_empty()
            || name.len() > RESOURCE_NAME_BYTES
            || tilo_actions::remote::workspace_provider_room(&target.provider).is_some()
        {
            return None;
        }
        let rows = self.top_resources(&target.provider).await.ok()?;
        match page::resource_named(name, &rows) {
            Some(Ok(id)) if id == *name => None,
            Some(found) => Some(found),
            None if page::looks_like_handle(name) => Some(Err(format!(
                "{name} is no handle of this connection (handles last one connection); name the resource by its kind or label instead: {}",
                resource_list(&rows)
            ))),
            None => None,
        }
    }
    /// The resources `provider` lists at its top level.
    async fn top_resources(&self, provider: &str) -> Result<Vec<CatalogueRow>, String> {
        Ok(self
            .ask(CatalogueQuery {
                provider: Some(provider.to_owned()),
                max_results: 128,
                ..CatalogueQuery::default()
            })
            .await?
            .rows)
    }
    /// Why `target` is not offered, in terms the caller can act on: the
    /// actions its resource has, or the resources there are. Only what the
    /// caller's own directory lists is named.
    async fn not_offered(&self, target: &RemoteActionTarget) -> String {
        let address = &target.address;
        let listed = self
            .ask(CatalogueQuery {
                provider: Some(target.provider.clone()),
                parent_resource: Some(address.resource.clone()),
                max_results: 128,
                ..CatalogueQuery::default()
            })
            .await
            .map(|page| page.rows)
            .unwrap_or_default();
        let actions = listed
            .iter()
            .filter(|row| row.resource.id == address.resource)
            .filter_map(|row| {
                row.action
                    .as_ref()
                    .map(|action| action.address.action.as_str())
            })
            .collect::<Vec<_>>();
        let handle = self.handles().short(&address.resource);
        let named = listed
            .iter()
            .find(|row| row.resource.id == address.resource)
            .map_or_else(
                || handle.clone(),
                |row| format!("{} ({handle})", row.resource.label),
            );
        if !actions.is_empty() {
            return format!(
                "{named} has no action {}; its actions: {}",
                address.action,
                actions.join(", ")
            );
        }
        let rows = self
            .top_resources(&target.provider)
            .await
            .unwrap_or_default();
        format!(
            "no resource {named} offers {} here; name one of: {}",
            address.action,
            resource_list(&rows)
        )
    }
    /// `page` compact, describing only its first `described` rows. An app
    /// instance's resource ids are opaque and long, and named by handles;
    /// the workspace's (`app.<app_id>`, its permission roles) are names that
    /// partners' provisioning and prompts spell out.
    fn compact_first_described(
        &self,
        page: &CataloguePage,
        workspace: bool,
        described: usize,
    ) -> Value {
        if workspace {
            page::compact_first_described(page, None, described)
        } else {
            page::compact_first_described(page, Some(&mut self.handles()), described)
        }
    }
    /// `describe` false leaves each action's description out. An `open`
    /// reply is read again on every later turn, and the label with its
    /// parameters is what the next call needs.
    fn compact_described(&self, page: &CataloguePage, workspace: bool, describe: bool) -> Value {
        if workspace {
            page::compact_described(page, None, describe)
        } else {
            page::compact_described(page, Some(&mut self.handles()), describe)
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
            #[serde(default)]
            app_id: Option<String>,
        }
        let Some(computer) = self.computer.clone() else {
            return tool_result(Err("Unknown tool".into()));
        };
        let batch = name == "computer_batch";
        let actions = if batch {
            parse::<Batch>(args).map(|batch| {
                let Batch {
                    mut actions,
                    app_id,
                } = batch;
                for action in &mut actions {
                    if action.app_id.is_none() {
                        action.app_id.clone_from(&app_id);
                    }
                }
                actions
            })
        } else {
            parse::<crate::computer::Action>(args).map(|action| vec![action])
        };
        let actions = match actions {
            Ok(actions) => actions,
            Err(error) => return tool_result(Err(error)),
        };
        let agent = self.agent();
        match tokio::task::spawn_blocking(move || computer.run_as(agent.as_ref(), &actions, batch))
            .await
        {
            Ok(Ok(outcomes)) => crate::computer::content(&outcomes, batch),
            Ok(Err(error)) => tool_result(Err(error)),
            Err(error) => tool_result(Err(error.to_string())),
        }
    }
    /// The agent this connection's pixel actions act for: its agent run, so
    /// concurrent agents get separate seats and strips.
    fn agent(&self) -> Option<crate::computer::Agent> {
        self.run.as_ref().map(|run| crate::computer::Agent {
            id: run.id.clone(),
            label: run.label.clone(),
        })
    }
    /// End this connection's agent on the compositor: it releases whatever
    /// the agent still holds and the agent's claim on its strip. Blocking
    /// socket I/O; for a transport whose connection just closed.
    pub fn leave_computer(&self) {
        let (Some(computer), Some(agent)) = (self.computer.as_ref(), self.agent()) else {
            return;
        };
        if let Err(error) = computer.leave(&agent) {
            tracing::debug!(%error, "agent seat release");
        }
    }
    /// Seat this connection's agent before it opens something through the
    /// workspace, so the window lands in the agent's strip. Best effort: an
    /// app opened without a seat still opens, beside whatever has focus.
    async fn seat_for_launch(&self) {
        let (Some(computer), Some(agent)) = (self.computer.clone(), self.agent()) else {
            return;
        };
        if let Ok(Err(error)) = tokio::task::spawn_blocking(move || computer.seat(&agent)).await {
            tracing::debug!(%error, "agent seat before a workspace action");
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
            "initialize" => match self.initialize(&params) {
                Ok(result) => Ok(result),
                Err(message) => return Some(error(&id, -32602, message)),
            },
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
    /// Negotiate the protocol version and take the client's page detail.
    fn initialize(&mut self, params: &Value) -> Result<Value, &'static str> {
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or("protocolVersion is required")?;
        let selected = if ["2024-11-05", "2025-03-26", "2025-06-18"].contains(&version) {
            version
        } else {
            "2025-06-18"
        };
        if let Some(detail) = params.pointer(SURFACE_DETAIL) {
            self.detail =
                Detail::deserialize(detail).map_err(|_| "detail must be compact or full")?;
        }
        // Every connection is one agent run, so its edits are attributed to
        // it in history and undo, whatever transport it came over. A host
        // that names the run itself (the HTTP endpoint) has done so already.
        // A client that connects again may continue the run it had, so its
        // undo still reaches what it did before.
        let continued = match params.pointer(SURFACE_RUN) {
            None | Some(Value::Null) => None,
            Some(run) => Some(continued_run(run)?),
        };
        if self.run.is_none() || continued.is_some() {
            let label: String = params
                .pointer("/clientInfo/name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("MCP agent")
                .chars()
                .filter(|character| !character.is_control())
                .take(64)
                .collect();
            let label = self.run.take().map_or(label, |run| run.label);
            self.run = Some(tilo_actions::ActionGroup {
                id: continued.unwrap_or_else(|| format!("mcp:{}", uuid::Uuid::new_v4())),
                label,
                kind: "agent-run".into(),
            });
        }
        let run = self.run.as_ref().map(|run| run.id.clone());
        Ok(
            json!({"protocolVersion":selected,"capabilities":{"tools":{},"resources":{},"experimental":{SURFACE:{"version":3,"run":run,"execution_id":self.caller.execution_id()}}},"serverInfo":{"name":"tilo-runtime","version":env!("CARGO_PKG_VERSION")}}),
        )
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
    async fn provider(
        &self,
        peer_id: Option<u64>,
        app_id: Option<&str>,
        workspace: bool,
    ) -> Result<String, String> {
        match (peer_id, app_id, workspace) {
            (None, None, true) => Ok(tilo_actions::remote::workspace_provider(&self.room)),
            (Some(peer), None, false) => {
                self.live_peer(peer).await?;
                Ok(peer.to_string())
            }
            // The newest instance: the one an `open` in the same turn started.
            (None, Some(app_id), false) => {
                let (_, running) = self.instances().await?;
                let ids = running
                    .iter()
                    .filter_map(|(_, app)| app["app_id"].as_str())
                    .collect::<Vec<_>>();
                let app_id = page::app_named(app_id, &ids)?;
                running
                    .iter()
                    .rev()
                    .find(|(_, app)| app["app_id"].as_str() == Some(app_id.as_str()))
                    .map(|(peer, _)| peer.to_string())
                    .ok_or_else(|| page::not_running(&app_id, &ids))
            }
            _ => Err("name exactly one of peer_id, app_id or workspace: true".into()),
        }
    }
    async fn query(&self, args: QueryArgs) -> Result<Value, String> {
        let provider = self
            .provider(args.peer_id, args.app_id.as_deref(), args.workspace)
            .await?;
        let cursor = args.cursor.into_bytes()?;
        let words = page::needle_words(&args.needle);
        let parent = self.full_id(args.parent_resource);
        let workspace = tilo_actions::remote::workspace_provider_room(&provider).is_some();
        let query = CatalogueQuery {
            provider: Some(provider),
            parent_resource: (!parent.is_empty()).then_some(parent),
            // A host matches one substring; the longest word narrows most,
            // and the rest are matched here.
            needle: if words.len() > 1 {
                words
                    .iter()
                    .max_by_key(|word| word.len())
                    .cloned()
                    .unwrap_or_default()
            } else {
                args.needle
            },
            target: None,
            revision: (!args.revision.is_empty()).then_some(args.revision),
            cursor,
            max_results: args.max_results.unwrap_or(DEFAULT_PAGE).min(128),
            limits: Some(CatalogueLimits {
                max_bytes: Some(args.max_bytes.unwrap_or(65536).min(65536)),
                max_work: Some(args.max_work.unwrap_or(512).min(512)),
                max_depth: Some(args.max_depth.unwrap_or(16).min(16)),
            }),
        };
        let detail = args.detail.unwrap_or(self.detail);
        let mut page = self.ask(query.clone()).await?;
        let mut partial = false;
        if words.len() > 1 {
            page.rows.retain(|row| page::matches_words(row, &words));
            // A page every row of which another word ruled out would read
            // as "nothing here"; read on a little before answering.
            let mut pages = 1;
            while page.rows.is_empty()
                && !page.exhausted
                && !page.next_cursor.is_empty()
                && pages < WORD_FILTER_PAGES
            {
                let mut next = query.clone();
                next.revision = Some(page.revision.clone());
                next.cursor = std::mem::take(&mut page.next_cursor);
                page = self.ask(next).await?;
                page.rows.retain(|row| page::matches_words(row, &words));
                pages = pages.saturating_add(1);
            }
            if page.rows.is_empty() && detail == Detail::Compact {
                page.rows = self.some_words(&query, &words).await?;
                partial = !page.rows.is_empty();
                if partial {
                    page.next_cursor.clear();
                    page.exhausted = true;
                }
            }
        }
        if detail == Detail::Full {
            return Ok(page_json(&page));
        }
        // The rows the needle matched are described; the rest of their
        // resources' actions follow as signatures. A caller that set
        // max_results asked for that many rows, not every action.
        let matched = page.rows.len();
        if !query.needle.is_empty() && args.max_results.is_none() {
            self.with_every_action(&query, &mut page).await?;
        }
        if workspace {
            offer_open_path(&mut page);
        }
        let mut value = self.compact_first_described(&page, workspace, matched);
        if partial && let Some(fields) = value.as_object_mut() {
            fields.insert("partial_match".into(), Value::Bool(true));
        }
        Ok(value)
    }
    /// The rows matching the most of `words`, when none matches them all: a
    /// needle naming several actions ("open paragraph style heading") still
    /// finds each of them.
    async fn some_words(
        &self,
        query: &CatalogueQuery,
        words: &[String],
    ) -> Result<Vec<CatalogueRow>, String> {
        let mut found = Vec::<(usize, CatalogueRow)>::new();
        let searched = words
            .iter()
            .filter(|word| word.chars().count() >= SOME_WORDS_MIN_CHARS)
            .take(SOME_WORDS_SEARCHED);
        for word in searched {
            let listed = self
                .ask(CatalogueQuery {
                    needle: word.clone(),
                    revision: None,
                    cursor: Vec::new(),
                    ..query.clone()
                })
                .await?;
            for row in listed.rows {
                let seen = found.iter().any(|(_, seen)| {
                    seen.resource.id == row.resource.id
                        && seen.action.as_ref().map(|a| &a.address.action)
                            == row.action.as_ref().map(|a| &a.address.action)
                });
                let score = page::word_score(&row, words);
                if !seen && score > 0 {
                    found.push((score, row));
                }
            }
        }
        let best = found.iter().map(|(score, _)| *score).max().unwrap_or(0);
        Ok(found
            .into_iter()
            .filter(|(score, _)| *score == best)
            .map(|(_, row)| row)
            .collect())
    }
    /// Add every action of the few resources a needle matched: an agent
    /// that found "paste" on a document also learns its "save-as" without a
    /// second search.
    async fn with_every_action(
        &self,
        query: &CatalogueQuery,
        page: &mut CataloguePage,
    ) -> Result<(), String> {
        let mut matched = Vec::<String>::new();
        for row in page.rows.iter().filter(|row| row.action.is_some()) {
            if !matched.contains(&row.resource.id) {
                matched.push(row.resource.id.clone());
            }
        }
        if matched.len() > EXPANDED_RESOURCES {
            return Ok(());
        }
        for resource in matched {
            let mut listing = CatalogueQuery {
                parent_resource: Some(resource.clone()),
                needle: String::new(),
                revision: None,
                cursor: Vec::new(),
                max_results: 128,
                ..query.clone()
            };
            // The host's byte budget counts whole contracts, so one
            // resource's actions can span pages its compact rows would not.
            for _ in 0..EVERY_ACTION_PAGES {
                let listed = self.ask(listing.clone()).await?;
                for row in listed.rows {
                    let new = row.resource.id == resource
                        && row.action.as_ref().is_some_and(|action| {
                            !page.rows.iter().any(|seen| {
                                seen.resource.id == resource
                                    && seen.action.as_ref().map(|a| &a.address.action)
                                        == Some(&action.address.action)
                            })
                        });
                    if new {
                        page.rows.push(row);
                    }
                }
                if listed.exhausted || listed.next_cursor.is_empty() {
                    break;
                }
                listing.revision = Some(listed.revision);
                listing.cursor = listed.next_cursor;
            }
        }
        Ok(())
    }
    /// The revision `provider`'s directory is at now, for a caller that
    /// named none.
    async fn current_revision(&self, provider: &str) -> Result<String, String> {
        self.ask(CatalogueQuery {
            provider: Some(provider.to_owned()),
            max_results: 1,
            limits: Some(CatalogueLimits {
                max_bytes: Some(4096),
                max_work: Some(1),
                max_depth: Some(1),
            }),
            ..CatalogueQuery::default()
        })
        .await
        .map(|page| page.revision)
    }
    /// The caller's revision, or the current one when it named none. An
    /// explicitly empty revision stays an error.
    async fn revision_for(
        &self,
        provider: &str,
        revision: Option<String>,
    ) -> Result<(String, bool), String> {
        match revision {
            Some(revision) if revision.is_empty() => Err("revision must not be empty".into()),
            Some(revision) => Ok((revision, false)),
            None => Ok((self.current_revision(provider).await?, true)),
        }
    }
    /// Live instances of `app_id`, by peer id.
    async fn peers_of(&self, app_id: &str) -> Result<Vec<u64>, String> {
        Ok(self
            .instances()
            .await?
            .1
            .into_iter()
            .filter(|(_, app)| app["app_id"].as_str() == Some(app_id))
            .map(|(peer, _)| peer)
            .collect())
    }
    /// The instance an `open` of `app_id` started: the first peer not in
    /// `before`, once it is listed and has published its directory. An app
    /// is listed before it publishes, and an empty first page reads as "no
    /// typed actions".
    async fn opened_peer(&self, app_id: &str, before: &[u64]) -> Option<u64> {
        let deadline = tokio::time::Instant::now().checked_add(JUST_LAUNCHED_WAIT)?;
        let peer = loop {
            let peers = self.peers_of(app_id).await.ok()?;
            if let Some(peer) = peers.iter().find(|peer| !before.contains(peer)) {
                break *peer;
            }
            if tokio::time::Instant::now() >= deadline {
                // An app that was already open may have been raised
                // instead of started again.
                return peers.last().copied();
            }
            tokio::time::sleep(JUST_LAUNCHED_POLL).await;
        };
        while tokio::time::Instant::now() < deadline {
            let first = self
                .ask(CatalogueQuery {
                    provider: Some(peer.to_string()),
                    max_results: 1,
                    ..CatalogueQuery::default()
                })
                .await;
            if first.is_ok_and(|page| !page.rows.is_empty()) {
                break;
            }
            tokio::time::sleep(JUST_LAUNCHED_POLL).await;
        }
        Some(peer)
    }
    async fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        match name {
            "tilo_list_apps" => self.apps(parse(args)?).await,
            "tilo_query" => self.query(parse(args)?).await,
            "tilo_resolve" => {
                let a: TargetArgs = parse(args)?;
                if a.revision.as_deref() == Some("") {
                    return Err("revision must not be empty".into());
                }
                let provider = self
                    .provider(a.peer_id, a.app_id.as_deref(), a.workspace)
                    .await?;
                let (revision, _) = self.revision_for(&provider, a.revision).await?;
                let mut target = RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new(self.full_id(a.resource), a.action),
                };
                let mut page = self
                    .ask(CatalogueQuery::resolve(&target, revision.clone()))
                    .await?;
                if page.rows.is_empty()
                    && !page.stale_cursor
                    && let Some(named) = self.named_resource(&target).await
                {
                    target.address.resource = named?;
                    page = self.ask(CatalogueQuery::resolve(&target, revision)).await?;
                }
                if page.stale_cursor {
                    return Err("StaleRevision".into());
                }
                match page.rows.first() {
                    Some(row) => Ok(json!({"row":resolved_row_json(row)})),
                    None => Err(self.not_offered(&target).await),
                }
            }
            "tilo_invoke" => self.invoke(parse(args)?).await,
            "tilo_status" => {
                let a: StatusArgs = parse(args)?;
                let invocation = invocation_id(&a.invocation_id)?;
                let mut value = self.status(invocation)?;
                self.attach_images(invocation, &mut value, true, None).await;
                Ok(value)
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
    /// Start one action, or `calls` in order. A batch answers `results` and,
    /// when one did not succeed, `stopped` at that index; nothing after it
    /// runs. The single-call form answers that action alone, as before.
    async fn invoke(&self, mut a: InvokeArgs) -> Result<Value, String> {
        if a.revision.as_deref() == Some("") {
            return Err("revision must not be empty".into());
        }
        if let Some(calls) = a.calls.take() {
            if a.resource.is_some() || a.action.is_some() {
                return Err("name resource and action, or calls, not both".into());
            }
            if calls.is_empty() || calls.len() > MAX_CALLS {
                return Err(format!("calls must hold 1 to {MAX_CALLS} actions"));
            }
            return self.invoke_calls(a, calls).await;
        }
        let resource = a
            .resource
            .take()
            .filter(|resource| !resource.is_empty())
            .ok_or("name resource and action, or calls")?;
        let action = a
            .action
            .take()
            .filter(|action| !action.is_empty())
            .ok_or("name resource and action, or calls")?;
        // Boxed: an invocation's future is large, and every tool call's
        // future would otherwise carry it.
        Box::pin(self.invoke_one(OneCall {
            peer_id: a.peer_id,
            app_id: a.app_id,
            workspace: a.workspace,
            revision: a.revision,
            resource,
            action,
            arguments: a.arguments,
            invocation_id: a.invocation_id,
            wait_ms: a.wait_ms,
        }))
        .await
    }
    /// `calls` in order on one peer. The first keeps the caller's revision
    /// and invocation id; each later call reads the directory as it is then,
    /// under an id derived from that one, so a retry does not run twice.
    async fn invoke_calls(&self, a: InvokeArgs, calls: Vec<InvokeCall>) -> Result<Value, String> {
        let mut results = Vec::with_capacity(calls.len());
        let mut stopped = None;
        for (index, call) in calls.into_iter().enumerate() {
            let value = Box::pin(self.invoke_one(OneCall {
                peer_id: a.peer_id,
                app_id: a.app_id.clone(),
                workspace: a.workspace,
                revision: if index == 0 { a.revision.clone() } else { None },
                resource: call.resource,
                action: call.action,
                arguments: call.arguments,
                invocation_id: batch_invocation(a.invocation_id.as_deref(), index)?,
                wait_ms: a.wait_ms,
            }))
            .await;
            let failed = match &value {
                Ok(value) => call_failed(value),
                Err(_) => true,
            };
            results.push(match value {
                Ok(value) if !failed => succeeded_step(value),
                Ok(value) => value,
                Err(error) => json!({ "error": error }),
            });
            if failed {
                stopped = Some(index);
                break;
            }
        }
        let mut answer = json!({ "results": results });
        if let Some(index) = stopped
            && let Some(fields) = answer.as_object_mut()
        {
            fields.insert("stopped".into(), json!(index));
            if index > 0 {
                fields.insert(
                    "kept".into(),
                    format!("steps 0..{index} stayed applied; undo takes them back, a group action applies all or nothing").into(),
                );
            }
        }
        Ok(answer)
    }
    /// Start an action and answer with its terminal update when it arrives
    /// within the caller's wait, so a quick action costs one call.
    async fn invoke_one(&self, mut a: OneCall) -> Result<Value, String> {
        if a.revision.as_deref() == Some("") {
            return Err("revision must not be empty".into());
        }
        let provider = self
            .provider(a.peer_id, a.app_id.as_deref(), a.workspace)
            .await?;
        if a.workspace {
            self.seat_for_launch().await;
        }
        // A host application (`app.host:<desktop-id>`) is no Tilo peer.
        let launching = a
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
            .filter(|app_id| {
                a.workspace
                    && a.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
                    && !app_id.starts_with("host:")
            })
            .map(str::to_owned);
        // The workspace launches the app empty; the path goes to the app's
        // own open action once it runs.
        let path = match launching {
            Some(_) => take_path(&mut a.arguments)?,
            None => None,
        };
        let before = match &launching {
            Some(app_id) => self.peers_of(app_id).await?,
            None => Vec::new(),
        };
        if let Some(peer) = path
            .as_deref()
            .and_then(|path| self.open_files.peer(path))
            .filter(|peer| before.contains(peer))
        {
            return Ok(self.already_open(peer).await);
        }
        let invocation = a
            .invocation_id
            .as_deref()
            .map_or_else(|| Ok(tilo_actions::InvocationId::new()), invocation_id)?;
        let wait =
            std::time::Duration::from_millis(a.wait_ms.unwrap_or(DEFAULT_WAIT_MS).min(MAX_WAIT_MS));
        let mut target = RemoteActionTarget {
            provider,
            address: tilo_actions::ActionAddress::new(self.full_id(a.resource), a.action),
        };
        let file = target
            .provider
            .parse::<u64>()
            .ok()
            .zip(opened_file(&target.address.action, &a.arguments).map(str::to_owned));
        self.record_producer(invocation, &target.provider).await;
        let not_offered = tilo_actions::remote::ActionHostError::UnknownAction.to_string();
        let mut value = match self
            .run_with_handles(
                target.clone(),
                a.arguments.clone(),
                invocation,
                a.revision.clone(),
                wait,
            )
            .await
        {
            // Nothing ran: the resource may be named by its kind or label.
            Err(error) if error.contains(&not_offered) => {
                let Some(named) = self.named_resource(&target).await else {
                    return Err(self.not_offered(&target).await);
                };
                target.address.resource = named?;
                match self
                    .run_with_handles(target.clone(), a.arguments, invocation, a.revision, wait)
                    .await
                {
                    Err(error) if error.contains(&not_offered) => {
                        return Err(self.not_offered(&target).await);
                    }
                    ran => ran?,
                }
            }
            ran => ran?,
        };
        self.attach_images(invocation, &mut value, false, Some(&target))
            .await;
        if let Some((peer, path)) = file.filter(|_| may_have_opened(&value)) {
            self.open_files.record(&path, peer);
        }
        let Some(app_id) = launching else {
            return Ok(value);
        };
        if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded") {
            return Ok(value);
        }
        let peer = self.opened_peer(&app_id, &before).await;
        self.answer_open(&mut value, peer, path, wait).await;
        Ok(value)
    }
    /// Complete an `open`'s answer `value` with the instance `peer` it
    /// started: the file at `path` opened in it, its directory and the
    /// file's first page.
    async fn answer_open(
        &self,
        value: &mut Value,
        peer: Option<u64>,
        path: Option<String>,
        wait: std::time::Duration,
    ) {
        let opened = match (peer, path) {
            (Some(peer), Some(path)) => Some(self.open_path(peer, &path, wait).await),
            (None, Some(_)) => Some(json!({"error":OPENED_NO_PEER})),
            (_, None) => None,
        };
        let file_open = opened.as_ref().is_some_and(|opened| {
            opened.get("error").is_none()
                && opened.pointer("/update/state").and_then(Value::as_str) == Some("succeeded")
        });
        // Best effort: the app is open whether or not its directory reads.
        let page = match peer {
            Some(peer) if self.detail == Detail::Compact || file_open => {
                self.first_directory(peer).await.ok()
            }
            _ => None,
        };
        let reader = page
            .as_ref()
            .filter(|_| file_open)
            .and_then(|(page, _)| document_reader(page));
        let directory = page
            .filter(|_| self.detail == Detail::Compact)
            .map(|(page, truncated)| self.directory_value(&page, truncated));
        let content = match (peer, reader) {
            (Some(peer), Some(reader)) => Some(self.document_content(peer, reader, wait).await),
            _ => None,
        };
        if let Some(fields) = value.as_object_mut() {
            if let Some(peer) = peer {
                fields.insert("peer_id".into(), json!(peer));
            }
            if let Some(opened) = opened {
                fields.insert("opened".into(), opened);
            }
            if let Some(directory) = directory {
                fields.insert("directory".into(), directory);
            }
            if let Some(content) = content {
                fields.insert("content".into(), content);
            }
        }
    }
    /// The first page of the document instance `peer` just opened, read
    /// through the action `reader` its app declared for it, and cut to
    /// [`content::CONTENT_BYTES`]: the output's value, the resource and
    /// action that read it, and `"truncated": true` when cut, which the
    /// same action with no arguments answers whole.
    async fn document_content(
        &self,
        peer: u64,
        reader: tilo_actions::ActionAddress,
        wait: std::time::Duration,
    ) -> Value {
        let mut answer = serde_json::Map::new();
        answer.insert(
            "resource".into(),
            self.handles().short(&reader.resource).into(),
        );
        answer.insert("action".into(), reader.action.clone().into());
        let target = RemoteActionTarget {
            provider: peer.to_string(),
            address: reader,
        };
        let read = self
            .run(
                target,
                serde_json::Map::new(),
                tilo_actions::InvocationId::new(),
                None,
                wait,
            )
            .await;
        match read {
            Ok(value) => match value.pointer("/update/output/value") {
                Some(page) if value.pointer("/update/state") == Some(&json!("succeeded")) => {
                    let mut page = page.clone();
                    if content::fit(&mut page, content::CONTENT_BYTES) {
                        answer.insert("truncated".into(), Value::Bool(true));
                    }
                    answer.insert("value".into(), page);
                }
                _ => {
                    answer.insert(
                        "update".into(),
                        value.get("update").cloned().unwrap_or_default(),
                    );
                }
            },
            Err(error) => {
                answer.insert("error".into(), error.into());
            }
        }
        Value::Object(answer)
    }
    /// [`Self::run`], with the session handles `arguments` name expanded
    /// where the action takes a resource. Refused with a handle left as
    /// text, the call runs once more with that handle's resource id, and
    /// the answer says so in `adjusted`.
    async fn run_with_handles(
        &self,
        target: RemoteActionTarget,
        mut arguments: serde_json::Map<String, Value>,
        invocation: tilo_actions::InvocationId,
        revision: Option<String>,
        wait: std::time::Duration,
    ) -> Result<Value, String> {
        let unexpanded = self
            .expand_handles(&target, revision.clone(), &mut arguments)
            .await;
        let retry = (!unexpanded.is_empty()).then(|| (target.clone(), arguments.clone()));
        let value = self
            .run(target, arguments, invocation, revision.clone(), wait)
            .await?;
        let Some((target, mut arguments)) = retry
            .filter(|_| value.pointer("/update/state").and_then(Value::as_str) == Some("failed"))
        else {
            return Ok(value);
        };
        for (name, _, id) in &unexpanded {
            arguments.insert(name.clone(), id.clone().into());
        }
        let mut value = self
            .run(
                target,
                arguments,
                tilo_actions::InvocationId::new(),
                revision,
                wait,
            )
            .await?;
        if let Some(fields) = value.as_object_mut() {
            fields.insert("adjusted".into(), handle_note(&unexpanded).into());
        }
        Ok(value)
    }
    /// Start one invocation of `target` and answer with its update once it
    /// is terminal or `wait` has passed. An omitted revision is the
    /// directory as it is now.
    ///
    /// A number the action refused as outside its range is recoverable: the
    /// call runs once more with the bound it named, and the answer says so
    /// in `adjusted`.
    async fn run(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, Value>,
        invocation: tilo_actions::InvocationId,
        revision: Option<String>,
        wait: std::time::Duration,
    ) -> Result<Value, String> {
        let value = self
            .run_once(
                target.clone(),
                arguments.clone(),
                invocation,
                revision.clone(),
                wait,
            )
            .await?;
        let Some((name, bound)) = out_of_range(&value, &arguments) else {
            return Ok(value);
        };
        let given = arguments.get(&name).cloned().unwrap_or_default();
        let mut arguments = arguments;
        arguments.insert(name.clone(), bound.clone());
        let mut value = self
            .run_once(
                target,
                arguments,
                tilo_actions::InvocationId::new(),
                revision,
                wait,
            )
            .await?;
        if let Some(fields) = value.as_object_mut() {
            fields.insert(
                "adjusted".into(),
                format!("{name} {given} is out of range; used {bound}").into(),
            );
        }
        Ok(value)
    }
    async fn run_once(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, Value>,
        invocation: tilo_actions::InvocationId,
        revision: Option<String>,
        wait: std::time::Duration,
    ) -> Result<Value, String> {
        let (revision, current) = self.revision_for(&target.provider, revision).await?;
        let mut request = InvokeRequest {
            invocation,
            target,
            argument: InvokeArgument::Json(arguments),
            context: self.context(),
            // The MCP client admitted this call under its own
            // allow/deny policy; that decision is the confirmation.
            confirmation: Some(tilo_actions::remote::Confirmation::Confirmed),
            revision: Some(revision),
        };
        let mut started = self.start_confirmed(&mut request).await;
        if current && started.as_ref().is_err_and(|error| error.contains(STALE)) {
            // The caller asked for the directory as it is now, and it moved
            // between reading its revision and starting: read it again.
            request.revision = Some(self.current_revision(&request.target.provider).await?);
            started = self.start_confirmed(&mut request).await;
        }
        started?;
        self.settled(invocation, wait).await
    }
    /// Open `path` in instance `peer` through the app's own action, so the
    /// app reads it inside its sandbox, under the file grants it holds: an
    /// `open` taking a `path`, else an `import-<extension>` taking one.
    async fn open_path(&self, peer: u64, path: &str, wait: std::time::Duration) -> Value {
        let provider = peer.to_string();
        let opener = match self.path_opener(&provider, path).await {
            Ok(Some(opener)) => opener,
            Ok(None) => return json!({"error":OPENED_NO_ACTION}),
            Err(error) => return json!({"error":error}),
        };
        let resource = self.handles().short(&opener.resource);
        let action = opener.action.clone();
        let mut arguments = serde_json::Map::new();
        arguments.insert("path".into(), path.into());
        let target = RemoteActionTarget {
            provider,
            address: opener,
        };
        let outcome = self
            .run(
                target,
                arguments,
                tilo_actions::InvocationId::new(),
                None,
                wait,
            )
            .await;
        let mut value = match outcome {
            Ok(value) => value,
            Err(error) => json!({"error":error}),
        };
        if value.get("error").is_none() && may_have_opened(&value) {
            self.open_files.record(path, peer);
        }
        if let Some(fields) = value.as_object_mut() {
            fields.insert("resource".into(), resource.into());
            fields.insert("action".into(), action.into());
        }
        value
    }
    /// The answer to opening a file instance `peer` already holds: that
    /// instance, as an `open` that started it would have answered. The file
    /// is not opened again, which could drop edits not yet saved.
    async fn already_open(&self, peer: u64) -> Value {
        let mut value = serde_json::Map::new();
        value.insert("update".into(), json!({"state": "succeeded"}));
        value.insert("peer_id".into(), peer.into());
        value.insert("opened".into(), json!({"already_open": true}));
        if self.detail == Detail::Compact
            && let Ok(directory) = self.directory(&peer.to_string()).await
        {
            value.insert("directory".into(), directory);
        }
        Value::Object(value)
    }
    /// Replace each argument naming a session handle with the resource id
    /// it stands for, where the action takes a resource there. Answers the
    /// handles left as they were, as `(argument, handle, resource id)`: the
    /// action takes text there, which may mean the handle literally.
    async fn expand_handles(
        &self,
        target: &RemoteActionTarget,
        revision: Option<String>,
        arguments: &mut serde_json::Map<String, Value>,
    ) -> Vec<(String, String, String)> {
        let named: Vec<(String, String, String)> = {
            let handles = self.handles();
            arguments
                .iter()
                .filter_map(|(name, value)| {
                    let handle = value.as_str()?;
                    let id = handles.issued(handle)?;
                    Some((name.clone(), handle.to_owned(), id.to_owned()))
                })
                .collect()
        };
        if named.is_empty() {
            return named;
        }
        let params = match self.revision_for(&target.provider, revision).await {
            Ok((revision, _)) => self
                .ask(CatalogueQuery::resolve(target, revision))
                .await
                .ok()
                .and_then(|page| page.rows.into_iter().find_map(|row| row.action))
                .map(|action| action.params)
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let mut unexpanded = Vec::new();
        for (name, handle, id) in named {
            let takes_resource = params.iter().any(|param| {
                param.name == name.as_str() && param.ty == tilo_actions::ParamType::Resource
            });
            if takes_resource {
                arguments.insert(name, id.into());
            } else {
                unexpanded.push((name, handle, id));
            }
        }
        unexpanded
    }
    /// The action of `provider` that opens `path`, by the directory's
    /// convention.
    async fn path_opener(
        &self,
        provider: &str,
        path: &str,
    ) -> Result<Option<tilo_actions::ActionAddress>, String> {
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase);
        let names = std::iter::once(tilo_actions::remote::APPLICATION_OPEN_ACTION.to_owned())
            .chain(extension.map(|extension| format!("import-{extension}")));
        for name in names {
            let page = self
                .ask(CatalogueQuery {
                    provider: Some(provider.to_owned()),
                    needle: name.clone(),
                    max_results: 64,
                    ..CatalogueQuery::default()
                })
                .await?;
            let found = page.rows.into_iter().find_map(|row| {
                row.action
                    .filter(|action| {
                        action.address.action == name
                            && action.params.iter().any(|param| param.name == "path")
                    })
                    .map(|action| action.address)
            });
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }
    /// A just-opened instance's [`Self::directory`] once it lists a
    /// resource, or as it is after [`FIRST_DIRECTORY_WAIT`]. An app may
    /// publish again while it starts (binding its document), and a page read
    /// in between is empty, which leaves the caller to query again.
    async fn first_directory(&self, peer: u64) -> Result<(CataloguePage, bool), String> {
        let provider = peer.to_string();
        let deadline = tokio::time::Instant::now().checked_add(FIRST_DIRECTORY_WAIT);
        loop {
            let (page, truncated) = self.directory_page(&provider).await?;
            let listed = !page.rows.is_empty();
            if listed || deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                return Ok((page, truncated));
            }
            tokio::time::sleep(JUST_LAUNCHED_POLL).await;
        }
    }
    async fn directory(&self, provider: &str) -> Result<Value, String> {
        let (page, truncated) = self.directory_page(provider).await?;
        Ok(self.directory_value(&page, truncated))
    }
    /// A directory page as an `open` answers it.
    fn directory_value(&self, page: &CataloguePage, truncated: bool) -> Value {
        let mut value = self.compact_described(page, false, false);
        if let Some(fields) = value.as_object_mut() {
            fields.remove("exhausted");
            if truncated {
                fields.insert("truncated".into(), Value::Bool(true));
            }
        }
        value
    }
    /// A just-opened instance's directory: its top-level resources with
    /// their actions, and the resources beneath them while the page stays
    /// within [`DIRECTORY_BYTES`], so an agent can act without browsing.
    /// Answers whether anything was left out.
    async fn directory_page(&self, provider: &str) -> Result<(CataloguePage, bool), String> {
        let listing = |parent: Option<String>| CatalogueQuery {
            provider: Some(provider.to_owned()),
            parent_resource: parent,
            max_results: 64,
            ..CatalogueQuery::default()
        };
        let root = self.ask(listing(None)).await?;
        let mut page = CataloguePage {
            revision: root.revision,
            exhausted: true,
            ..CataloguePage::default()
        };
        let mut truncated = !root.exhausted;
        let mut queue = std::collections::VecDeque::new();
        for row in root.rows {
            if row.has_children && !queue.contains(&row.resource.id) {
                queue.push_back(row.resource.id.clone());
            }
            page.rows.push(row);
        }
        let mut expanded = Vec::<String>::new();
        while let Some(parent) = queue.pop_front() {
            if expanded.len() >= DIRECTORY_EXPANSIONS {
                truncated = true;
                break;
            }
            expanded.push(parent.clone());
            let listed = self.ask(listing(Some(parent.clone()))).await?;
            truncated |= !listed.exhausted;
            let mut grown = page.clone();
            grown.rows.extend(listed.rows.into_iter().filter(|row| {
                !page.rows.iter().any(|seen| {
                    seen.resource.id == row.resource.id
                        && seen.action.as_ref().map(|a| &a.address.action)
                            == row.action.as_ref().map(|a| &a.address.action)
                })
            }));
            let mut trial = self.handles().clone();
            if page::compact_described(&grown, Some(&mut trial), false)
                .to_string()
                .len()
                > DIRECTORY_BYTES
            {
                truncated = true;
                continue;
            }
            for row in grown.rows.iter().skip(page.rows.len()) {
                let id = &row.resource.id;
                if row.has_children
                    && *id != parent
                    && !expanded.contains(id)
                    && !queue.contains(id)
                {
                    queue.push_back(id.clone());
                }
            }
            page = grown;
        }
        Ok((page, truncated))
    }
    /// Start `request`, answering an exact-phrase confirmation contract with
    /// the phrase it names.
    async fn start_confirmed(&self, request: &mut InvokeRequest) -> Result<(), String> {
        let Err(error) = self.start(request.clone()).await else {
            return Ok(());
        };
        if error != tilo_actions::remote::ActionHostError::ConfirmationRequired.to_string() {
            return Err(error);
        }
        // Only an exact-phrase contract refuses the plain confirmation.
        let revision = request.revision.clone().unwrap_or_default();
        match self
            .contract_confirmation(&request.target, &revision)
            .await?
        {
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                request.confirmation = Some(tilo_actions::remote::Confirmation::Phrase(phrase));
                self.start(request.clone()).await
            }
            _ => Err(error),
        }
    }
    /// The invocation's update once it is terminal, or after `wait`.
    async fn settled(
        &self,
        id: tilo_actions::InvocationId,
        wait: std::time::Duration,
    ) -> Result<Value, String> {
        let deadline = tokio::time::Instant::now().checked_add(wait);
        loop {
            let terminal = self
                .host()
                .observe(&self.caller(), &id)
                .is_some_and(|state| state.update.is_terminal());
            if terminal {
                return self.status(id);
            }
            if deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                let mut value = self.status(id)?;
                if let Some(fields) = value.as_object_mut() {
                    fields.insert(
                        "pending".into(),
                        format!(
                            "not finished within {} ms; tilo_status with this invocation_id answers \
                             how it went, or pass a longer wait_ms",
                            wait.as_millis()
                        )
                        .into(),
                    );
                }
                return Ok(value);
            }
            tokio::time::sleep(SETTLE_POLL).await;
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
        self.caller.clone()
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
        let value = self
            .query(parse(json!({"peer_id":peer,"detail":"full"}))?)
            .await?;
        Ok(
            json!({"contents":[{"uri":a.uri,"mimeType":"application/json","text":value.to_string()}]}),
        )
    }
}
/// How long an `open` waits for its instance to be listed and to publish
/// its directory, and how often it looks.
const JUST_LAUNCHED_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
const JUST_LAUNCHED_POLL: std::time::Duration = std::time::Duration::from_millis(200);
/// How long an `open` waits more for the new instance's directory to list a
/// resource once it has published: two query turns cost more.
const FIRST_DIRECTORY_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
/// The experimental capability naming this surface's version. Version 2
/// answers `tilo_query` with compact pages; version 3 names long resource
/// ids by session handles in them and answers an `open` with the new
/// instance's directory. A client sends `{"so.tilo.mcp": {"detail": "full"}}`
/// to keep version 1's rows.
const SURFACE: &str = "so.tilo.mcp";
const SURFACE_DETAIL: &str = "/capabilities/experimental/so.tilo.mcp/detail";
/// The agent run a client continues: `{"so.tilo.mcp": {"run": "mcp:…"}}`
/// with the id its earlier connection was answered with, or one it chose.
/// Provenance only, like every run: it names whose steps `undo` takes back,
/// never what the connection may do.
const SURFACE_RUN: &str = "/capabilities/experimental/so.tilo.mcp/run";
/// The longest run id a client may name.
const MAX_RUN_ID: usize = 96;

/// The run id a client asks to continue, checked.
fn continued_run(run: &Value) -> Result<String, &'static str> {
    run.as_str()
        .map(str::trim)
        .filter(|id| {
            (1..=MAX_RUN_ID).contains(&id.len()) && id.chars().all(|c| c.is_ascii_graphic())
        })
        .map(str::to_owned)
        .ok_or("run must be 1 to 96 printable characters without spaces: a run id from history or the run an earlier initialize answered")
}
/// Rows per page when the caller names no limit.
const DEFAULT_PAGE: u32 = 64;
/// Resources a compact needle page lists in full; more means the needle was
/// broad, and the matches alone are the useful answer.
const EXPANDED_RESOURCES: usize = 3;
/// Host pages one multi-word needle reads before answering with none.
const WORD_FILTER_PAGES: usize = 8;
/// The most host pages read to list every action of one found resource.
const EVERY_ACTION_PAGES: usize = 4;
/// Words of a needle no row matches whole that are searched one by one,
/// and the shortest searched: "as" or "to" would match nearly everything.
const SOME_WORDS_SEARCHED: usize = 6;
const SOME_WORDS_MIN_CHARS: usize = 3;
/// Largest directory an `open` answers with, in bytes of JSON, and how many
/// resources beneath the top level it lists. A model reads it again on every
/// later turn; beyond this, `tilo_query` browses the rest.
const DIRECTORY_BYTES: usize = 12 * 1024;
const DIRECTORY_EXPANSIONS: usize = 6;
const OPENED_NO_PEER: &str = "the app did not publish its directory in time, so the file is not open; tilo_list_apps shows whether it runs";
const OPENED_NO_ACTION: &str = "the app offers no open action taking a path, so it started empty; its directory shows what it can do";
/// How long `tilo_invoke` waits for a terminal update by default, at most,
/// and how often it looks.
const DEFAULT_WAIT_MS: u64 = 5_000;
const MAX_WAIT_MS: u64 = 30_000;
/// Actions one `calls` batch runs. A document's edits and its save fit;
/// more than this is several turns.
const MAX_CALLS: usize = 32;
const SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(25);
/// What the host says when a target changed since the revision named.
const STALE: &str = "changed since that revision";
/// Longer names are resource ids, never a kind or a label.
const RESOURCE_NAME_BYTES: usize = 64;
/// How many resources an error names before it stops.
const LISTED_RESOURCES: usize = 16;
/// The numeric argument a failed update refused as outside its range, with
/// the bound the refusal named (`argument `pageSize` must be at most 64`).
fn out_of_range(
    value: &Value,
    arguments: &serde_json::Map<String, Value>,
) -> Option<(String, Value)> {
    let error = value.pointer("/update/error")?;
    if error.get("code")?.as_str()? != "invalid_arguments" {
        return None;
    }
    let (name, rest) = error
        .get("message")?
        .as_str()?
        .strip_prefix("argument `")?
        .split_once("` must be at ")?;
    let bound = rest
        .strip_prefix("most ")
        .or_else(|| rest.strip_prefix("least "))?;
    let bound = bound
        .parse::<i64>()
        .map(Value::from)
        .or_else(|_| bound.parse::<f64>().map(Value::from))
        .ok()?;
    arguments.get(name)?.as_f64()?;
    Some((name.to_owned(), bound))
}

fn empty_object() -> Value {
    json!({})
}
/// Whether a finished call should stop a batch: it did not succeed, it is
/// still running, or the file it tried to open did not.
/// A batch step that succeeded, said once: its output (`value`, `resources`)
/// and any note on it, without the invocation id and update wrapping every
/// step repeats. Every step before `stopped` succeeded, so the state needs
/// no saying; a step that did not keeps its whole update.
fn succeeded_step(mut value: Value) -> Value {
    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    fields.remove("invocation_id");
    if let Some(Value::Object(output)) = fields
        .remove("update")
        .and_then(|mut update| update.get_mut("output").map(Value::take))
    {
        for (key, item) in output {
            fields.entry(key).or_insert(item);
        }
    }
    // Content the value already names (`"content": "tilo.content:…"`)
    // need not be listed twice.
    let said = fields
        .get("value")
        .map(Value::to_string)
        .unwrap_or_default();
    if fields
        .get("resources")
        .and_then(Value::as_array)
        .is_some_and(|listed| {
            listed
                .iter()
                .all(|item| item.as_str().is_some_and(|item| said.contains(item)))
        })
    {
        fields.remove("resources");
    }
    value
}
/// `rows`' resources as `label (kind)`, each once.
fn resource_list(rows: &[CatalogueRow]) -> String {
    let mut seen = Vec::<&str>::new();
    let mut listed = Vec::<String>::new();
    for row in rows {
        if seen.contains(&row.resource.id.as_str()) {
            continue;
        }
        seen.push(&row.resource.id);
        if listed.len() == LISTED_RESOURCES {
            listed.push("…".into());
            break;
        }
        listed.push(format!("{} ({})", row.resource.label, row.resource.kind));
    }
    if listed.is_empty() {
        "it lists none".into()
    } else {
        listed.join(", ")
    }
}
fn call_failed(value: &Value) -> bool {
    if value.get("pending").is_some() || value.pointer("/opened/error").is_some() {
        return true;
    }
    value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded")
}
/// The invocation id of batch step `index`. The first is the caller's, so
/// a retry of the batch reports that action instead of starting it again;
/// each later step's id is derived from it and stable across that retry.
fn batch_invocation(parent: Option<&str>, index: usize) -> Result<Option<String>, String> {
    let Some(parent) = parent else {
        return Ok(None);
    };
    let parent = invocation_id(parent)?;
    if index == 0 {
        return Ok(Some(parent.0.to_string()));
    }
    let mut bytes = *parent.0.as_bytes();
    let mix = (index as u128).to_le_bytes();
    for (byte, mix) in bytes.iter_mut().zip(mix) {
        *byte ^= mix;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Some(uuid::Uuid::from_bytes(bytes).to_string()))
}
/// What a call refused with session handles as text ran again with: each
/// handle's resource id in full.
fn handle_note(expanded: &[(String, String, String)]) -> String {
    let named = expanded
        .iter()
        .map(|(name, handle, id)| format!("{name} {handle} was refused; used its id {id}"))
        .collect::<Vec<_>>()
        .join("; ");
    format!("{named} (this argument takes ids, not handles)")
}
/// The `path` an application `open` names, taken out of its arguments: the
/// workspace launches the app empty and this bridge hands the path to the
/// app itself.
fn take_path(arguments: &mut serde_json::Map<String, Value>) -> Result<Option<String>, String> {
    match arguments.remove("path") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(path)) if !path.trim().is_empty() => Ok(Some(path)),
        Some(_) => Err("path must be a non-empty string".into()),
    }
}
/// Offer `path` on the workspace's application `open` actions, which this
/// bridge opens through the launched app's own action.
fn offer_open_path(page: &mut CataloguePage) {
    for action in page.rows.iter_mut().filter_map(|row| row.action.as_mut()) {
        let app = action
            .address
            .resource
            .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX);
        if action.address.action == tilo_actions::remote::APPLICATION_OPEN_ACTION
            && app.is_some_and(|app| !app.starts_with("host:"))
            && !action.params.iter().any(|param| param.name == "path")
        {
            action.params.push(tilo_actions::ActionParam::optional(
                "path",
                tilo_actions::ParamType::Text,
                "A file to open in it; the app reads only files it was granted",
            ));
        }
    }
}
fn page_json(page: &CataloguePage) -> Value {
    json!({"revision":page.revision,"rows":page.rows.iter().map(browsed_row_json).collect::<Vec<_>>(),"next_cursor":page.next_cursor,"exhausted":page.exhausted,"stale_cursor":page.stale_cursor,"budget_exhausted":page.budget_exhausted})
}
/// A parameter schema larger than this is left to `tilo_resolve` in a
/// browsed page: one rich-text `apply` schema is tens of kilobytes, and an
/// agent pays for every byte of every page it reads.
const BROWSED_SCHEMA_BYTES: usize = 1024;
/// [`row_json`] with large parameter schemas replaced by a pointer to
/// `tilo_resolve`, which returns them in full.
fn browsed_row_json(row: &CatalogueRow) -> Value {
    let mut value = row_json(row);
    if let Some(params) = value
        .pointer_mut("/descriptor/params")
        .and_then(Value::as_array_mut)
    {
        for param in params {
            let large = param
                .get("schema")
                .is_some_and(|schema| schema.to_string().len() > BROWSED_SCHEMA_BYTES);
            if large && let Some(param) = param.as_object_mut() {
                param.remove("schema");
                param.insert(
                    "schema_omitted".into(),
                    json!("large schema: tilo_resolve this action to read it"),
                );
            }
        }
    }
    value
}
/// [`row_json`] without what it says twice: the descriptor's resource,
/// kind and label are the row's, a parent that is the resource itself, and
/// a visibility that is the action's own contract. Every resolve pays for
/// them, and the schema is what the agent came for.
fn resolved_row_json(row: &CatalogueRow) -> Value {
    let mut value = row_json(row);
    let own_visibility = value.get("visibility_action") == value.get("action")
        && value.get("visibility_contract") == value.pointer("/descriptor/contract");
    let Some(fields) = value.as_object_mut() else {
        return value;
    };
    if fields.get("parent_resource") == fields.get("resource") {
        fields.remove("parent_resource");
    }
    if own_visibility {
        fields.remove("visibility_action");
        fields.remove("visibility_contract");
    }
    let row_fields = [
        ("resource", fields.get("resource").cloned()),
        ("resource_kind", fields.get("resource_kind").cloned()),
        ("resource_label", fields.get("resource_label").cloned()),
    ];
    if let Some(Value::Object(descriptor)) = fields.get_mut("descriptor") {
        for (key, row_value) in row_fields {
            if descriptor.get(key) == row_value.as_ref() {
                descriptor.remove(key);
            }
        }
        if let Some(Value::Array(params)) = descriptor.get_mut("params") {
            params.iter_mut().for_each(param_said_once);
        }
        descriptor.retain(|_, item| !item.is_null());
    }
    fields.retain(|_, item| !item.is_null());
    value
}
/// A parameter whose schema says it all: its type, nullability and
/// description stand in the schema, so the copies beside it go, and a
/// description that only repeats the name says nothing.
fn param_said_once(param: &mut Value) {
    let Some(fields) = param.as_object_mut() else {
        return;
    };
    let Some(schema) = fields.get("schema").filter(|schema| schema.is_object()) else {
        return;
    };
    let repeated = fields.get("description").is_some_and(|description| {
        Some(description) == schema.get("description") || Some(description) == fields.get("name")
    });
    if repeated {
        fields.remove("description");
    }
    fields.remove("ty");
    fields.remove("nullable");
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
/// The advertised tools. Descriptions are read on every model turn, so they
/// say what an agent needs and no more; tuning knobs a model never needs
/// (`max_bytes`, `max_work`, `max_depth`) are accepted but not advertised.
fn tool_definitions() -> Vec<Value> {
    let integer = json!({"type":"integer"});
    let string = json!({"type":"string"});
    let boolean = json!({"type":"boolean"});
    let app_id = json!({"type":"string","description":"Instead of peer_id: that app's newest instance; its id or last part (canvas)"});
    let target = json!({"peer_id":integer,"app_id":app_id,"workspace":boolean,"revision":string,"resource":string,"action":string});
    let mut invoke = target.clone();
    if let Some(properties) = invoke.as_object_mut() {
        properties.insert("arguments".into(), json!({"type":"object"}));
        properties.insert(
            "calls".into(),
            json!({
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_CALLS,
                "items": {
                    "type": "object",
                    "properties": {"resource": string, "action": string, "arguments": {"type": "object"}},
                    "required": ["resource", "action"],
                    "additionalProperties": false
                }
            }),
        );
        properties.insert("invocation_id".into(), string.clone());
        properties.insert("wait_ms".into(), integer.clone());
    }
    let read = json!({"readOnlyHint":true,"openWorldHint":false});
    let mut tools: Vec<Value> = [
        ("tilo_list_apps","Running apps (peer_id, app_id). workspace: true in the other tools addresses the workspace instead: tilo_query it for installed apps, tilo_invoke {workspace: true, resource: \"app.<app_id>\", action: \"open\", arguments: {path?}} starts one, opens path in it, and returns its peer_id, directory (its resources and all their actions, so no tilo_query is needed) and, for a path, content: the document's first page as the app reads it (so no read is needed); app.host:<desktop-id> opens a host application, such as a browser, driven with the computer tool",json!({"after_peer":integer,"max_results":integer}),json!([]),read.clone()),
        ("tilo_query","Find actions. needle: words in an action or resource name, e.g. \"save as\"; rows matching every word, else those matching most (partial_match). Each resource found lists all its actions as id: \"Label (params)\", described where the needle matched (an explicit max_results lists only the matches) (p? optional, p:type, a|b choices, 1..64 range, p* schema via tilo_resolve). An app's long resource ids appear as handles such as r3 for this connection; a resource's kind or label works too. parent_resource lists a resource's children; pass next_cursor to continue; detail: \"full\" gives whole contracts",json!({"peer_id":integer,"app_id":app_id,"workspace":boolean,"needle":string,"parent_resource":string,"cursor":string,"revision":string,"max_results":integer,"detail":{"enum":["compact","full"]}}),json!([]),read.clone()),
        ("tilo_resolve","An action's full contract and parameter schemas",target,json!(["resource","action"]),read.clone()),
        ("tilo_invoke","One action (resource, action, arguments), or calls: [{resource, action, arguments}, …] in order, stopping at the first failure (results; stopped is that index; earlier steps stay applied: a resource's group action is all-or-nothing): batch the steps you already know, e.g. several edits and then a look, into one call. Waits wait_ms (default 5000); if still running, poll tilo_status. Omit revision. Same invocation_id never runs twice. Calling confirms, destructive ones included. An image this session was already shown is not sent again: unchanged names the invocation that showed it",invoke,json!([]),json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":true})),
        ("tilo_status","An invocation's latest update",json!({"invocation_id":string}),json!(["invocation_id"]),read),
        ("tilo_cancel","Stop an invocation",json!({"invocation_id":string}),json!(["invocation_id"]),json!({"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false})),
    ].into_iter().map(|(name,description,properties,required,annotations)|json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":annotations})).collect();
    if let Some(schema) = tools
        .iter_mut()
        .find(|tool| tool["name"] == "tilo_invoke")
        .and_then(|tool| tool.get_mut("inputSchema"))
    {
        schema["oneOf"] = json!([{"required": ["resource", "action"]}, {"required": ["calls"]}]);
    }
    tools
}
#[cfg(test)]
#[path = "mcp_open_tests.rs"]
mod open_tests;
#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

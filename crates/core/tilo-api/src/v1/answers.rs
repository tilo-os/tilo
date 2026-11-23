//! What the API takes and answers, as types: every answer goes out through
//! one of them ([`typed`]), and the `OpenAPI` description's schemas are made from
//! the same types ([`schemas`]), so the two cannot differ.
//!
//! Fields an app types itself (a resource's fields, an action's result) are
//! JSON as the app gives them; their types are the app's surface, which the
//! description carries per app ([`super::openapi`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

/// `value` as `T` answers it: the fields `T` names, in its shape.
///
/// # Errors
/// `value` is not of `T`'s shape, which is the host's fault, not the caller's.
pub fn typed<T: DeserializeOwned + Serialize>(value: Value) -> Result<Value, String> {
    let typed: T = serde_json::from_value(value).map_err(|error| error.to_string())?;
    serde_json::to_value(typed).map_err(|error| error.to_string())
}

/// Every refusal, whatever route it came from.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Error {
    pub error: ErrorBody,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    /// Stable and typed: what a program branches on.
    pub code: String,
    /// For people.
    pub message: String,
    /// Calls that would work, each a sentence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_steps: Vec<String>,
    /// How long to wait before the same call works.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// When the key was revoked, for `key_revoked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at_ms: Option<u64>,
    /// When the key expired, for `key_expired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_at_ms: Option<u64>,
    /// The action refused, with what it takes, for an action's refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionRow>,
}

/// `GET /v1`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApiInfo {
    /// The API's major version: `"1"`.
    pub api_version: String,
    /// The server's build.
    pub server: String,
    /// What the key sent reaches, when one was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyRow>,
}

pub use crate::server::Workspace;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct KeyRow {
    pub grant: String,
    pub name: String,
    /// `all` or the one workspace it reaches.
    pub scope: String,
    /// `agent`, `key` or `minted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
    /// The person's device that approved it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_ms: Option<u64>,
    /// `null` for a key that lasts until revoked.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub browser_pages: bool,
}

/// One parameter of an action, as its app publishes it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Param {
    pub name: String,
    /// `text`, `integer`, `number`, `boolean`, `object`, `array`, `resource`,
    /// `datetime` or `secret`.
    pub ty: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<Value>>,
    /// The JSON schema of the value, where the app gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
}

/// An action a resource offers.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ActionRow {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub params: Vec<Param>,
    /// `read`, `undoable` or `outside`: what running it may reach.
    pub kind: String,
    /// The JSON schema of what it answers, where it declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// What a row a listing does not speak for is: a resource of its own, read.
const fn a_resource() -> bool {
    true
}

/// One row of a listing: a resource under the path listed, or an app.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Row {
    pub path: String,
    pub name: String,
    /// The resource's own id, which names it whatever else is titled the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The app's stable key for it, the name its actions answer it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub label: String,
    /// The app's type of it, or `app`.
    pub kind: String,
    /// Whether a query with this row as its parent answers rows the workspace
    /// holds now. An app holding no document here says no, though listing it
    /// would start it on a new one; a walk reads what is there and starts
    /// nothing.
    #[serde(default)]
    pub has_children: bool,
    /// Whether a read answers the row as a resource of its own. A row that is
    /// only a place inside it (an option or a choice, like a reading font or a
    /// built-in folder) is part of what lists it and is never read alone; a
    /// listing that does not say is read as it always was, every row read.
    #[serde(default = "a_resource")]
    pub reads: bool,
    /// The ids of the actions it offers, each described once in the
    /// listing's `kinds` under the row's `kind`.
    #[serde(default)]
    pub actions: Vec<String>,
    /// For an app: `running`, `service` or `installed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Listing {
    /// The path listed; `apps` for the apps.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub rows: Vec<Row>,
    /// The actions of the resource listed.
    #[serde(default)]
    pub actions: Vec<ActionRow>,
    /// The actions the rows offer, once for each kind of row: a row names
    /// those it offers by id.
    #[serde(default)]
    pub kinds: std::collections::BTreeMap<String, Vec<ActionRow>>,
    /// Passed back as `cursor` for the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A list of what a resource holds, one entry of it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Child {
    /// The list it is in.
    pub list: String,
    pub name: String,
    /// The key a read takes it by.
    pub item: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Resource {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The app's own fields, with its own types.
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[serde(default)]
    pub children: Vec<Child>,
    /// The app's cursor for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// For an app: its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// For an app: its surface in the form it is kept by
    /// (`collab_types::surface::canonical`), what types are generated from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// For an app: its surface's version, `<app>@<digest>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_version: Option<String>,
}

/// What the person is asked before an action runs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Ask {
    /// Where the person answers it, when a page for it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// One step of several actions run in order.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    pub path: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// An action's answer, and an invocation's.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ActionAnswer {
    /// `ok`, `pending` or `cancelled`. (A failure is the error shape.)
    pub status: String,
    /// The invocation: what `…/invocations/{invocation}` follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    /// The action at the resource it resolved to, `app/resource#action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The action's output, as the app typed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The resources it touched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
    /// What it made, each with its id and the address it is published under:
    /// every item a write of several made, not only the first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made: Vec<Made>,
    /// The link to the record it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Every record it changed, once each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// What actually ran, when the person changed the arguments first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_by_person: Option<Value>,
    /// What the person is being asked, while it waits for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Ask>,
    /// What was undone, for an undo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone: Option<Value>,
}

/// An item a write made: its id, and the address it is published under.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Made {
    pub id: String,
    pub address: String,
}

/// What an app's window shows for a resource: its view's semantic lines, the
/// same ones an MCP client's `tilo_view` prints, and a picture with `picture`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct View {
    /// The lines, in reading order, one node each:
    /// `<role> "<name>" = "<value>" [state]`, indented by depth.
    pub text: String,
    pub path: String,
    /// The app whose window shows it.
    pub app: String,
    /// Whether the lines are of the resource or of the window it could not show.
    pub shown: String,
    /// The same view as a tree: each node's role, name, value, state, bounds and action.
    #[serde(default)]
    pub tree: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<u64>,
    /// The view's size, in logical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<[f64; 2]>,
    /// Where the view stands in the app's window, in logical pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_origin: Option<[f64; 2]>,
    /// The revision of the document the view shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Where the picture is kept, as a content address, when one was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Program {
    /// tilolang: `notes.notebook.notes.filter(.pinned)`.
    pub program: String,
    /// Runs a program that would change much or destroy something; without
    /// it such a program answers `preview`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
    /// The program session it continues, as an earlier answer named it: the
    /// `let`s its programs bound, the references its answers showed (`n3`),
    /// and the steps `undo` and `history task` take back and list. Without
    /// it the program starts a new session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProgramAnswer {
    /// `ok`, `preview` or `failed`.
    pub status: String,
    /// The program's answer, a line per element.
    #[serde(default)]
    pub output: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<u64>,
    /// Why it failed: the trap's words, or what did not compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    /// Every step of the run that changed something, oldest first.
    #[serde(default)]
    pub steps: Vec<Value>,
    /// What a person reads: the answer's lines, each resource by its
    /// reference (`n3`), and what a preview would apply.
    #[serde(default)]
    pub text: String,
    /// The program session it ran in: send it with the next program to
    /// continue it.
    #[serde(default)]
    pub session: String,
}

/// A content put: where an unfinished one stands, and what every byte of it
/// is once the last piece answered.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PutContent {
    /// How many bytes the whole file is.
    pub size: u64,
    /// How many of them are there: the next piece goes at this offset.
    pub offset: u64,
    /// `tilo.content:<hash>`, once every byte is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// One event of a workspace's stream, or of a webhook's delivery.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// The cursor after it: `Last-Event-ID` resumes there.
    pub id: String,
    /// `change`, `invocation` or `presence`.
    #[serde(rename = "type")]
    pub kind: String,
    pub workspace: String,
    /// The event as its kind gives it.
    pub data: Value,
}

/// One turn asked of the person's own model, which the server that keeps
/// their workspace asks with the key their vault keeps there: the
/// conversation as the model service takes it. The answer streams as
/// server-sent events: `text` and `reasoning` each carry `{"text": …}`, what
/// the model wrote since the event before; `answer` ends the stream with the
/// whole turn, `{"Ok": {"message", "stop_reason", "usage"}}` or
/// `{"Err": {"kind", "message"}}`, the kind telling a caller what to do
/// (`no_provider`: none set up yet; `authentication`: no key kept); `error`
/// ends it with a refusal of the API's own shape.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ModelAsk {
    /// The conversation: each `{"role", "content"}`, the role `system`,
    /// `user`, `assistant` or `tool`.
    pub messages: Vec<Value>,
    /// The tools the model may call, each `{"name", "description",
    /// "input_schema"}`.
    #[serde(default)]
    pub tools: Vec<Value>,
    /// `auto`, `required` or `none`.
    pub tool_choice: String,
    /// The most tokens the answer may take.
    pub max_output_tokens: u32,
}

/// Every schema the description carries, by the name operations give it.
#[must_use]
pub fn schemas(generator: &mut schemars::SchemaGenerator) -> Vec<(&'static str, schemars::Schema)> {
    macro_rules! each {
        ($($name:ident),* $(,)?) => {
            vec![$((stringify!($name), generator.subschema_for::<$name>())),*]
        };
    }
    each![
        Error,
        ApiInfo,
        Workspace,
        KeyRow,
        Listing,
        Resource,
        ActionAnswer,
        View,
        Program,
        ProgramAnswer,
        PutContent,
        Event,
        ModelAsk,
    ]
}

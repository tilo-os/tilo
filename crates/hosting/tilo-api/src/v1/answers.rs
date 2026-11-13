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

/// The person's account on their host: their plan, and per workspace what it
/// keeps and the VM time it ran.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Account(pub Map<String, Value>);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    /// What every route names it by.
    pub id: String,
    pub name: String,
    /// The host that keeps it: a relay's name, or the person's device that
    /// hosts it; absent while none of their devices said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `owner` for one of the person's own, else the role they hold there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceList {
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkspace {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeletedWorkspace {
    pub deleted: String,
    /// The keys that reached only it, revoked with it.
    #[serde(default)]
    pub revoked_keys: Vec<String>,
}

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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct KeyList {
    pub keys: Vec<KeyRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewKey {
    /// The one workspace it reaches.
    pub workspace: String,
    /// How History and the grants page name its agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Seconds until it expires; at most this key's own lifetime and 89 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MintedKey {
    pub grant: String,
    pub name: String,
    /// The key itself: shown once.
    pub token: String,
    pub workspace: String,
    /// Where an agent connects with it over MCP.
    pub mcp_url: String,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RevokedKey {
    pub revoked: String,
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
}

/// One row of a listing: a resource under the path listed, or an app.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Row {
    pub path: String,
    pub name: String,
    pub label: String,
    /// The app's type of it, or `app`.
    pub kind: String,
    #[serde(default)]
    pub has_children: bool,
    #[serde(default)]
    pub actions: Vec<ActionRow>,
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
    /// For an app: its surface (`surface.toml`), what types are generated from.
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
    pub run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// An action's answer, and a run's.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ActionAnswer {
    /// `ok`, `pending` or `cancelled`. (A failure is the error shape.)
    pub status: String,
    /// The run: what `…/runs/{run}` follows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The action at the resource it resolved to, `app/resource#action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The action's output, as the app typed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The resources it touched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Hit {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub label: String,
    /// The action the hit offers; `open` when it opens the resource itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchHits {
    pub hits: Vec<Hit>,
    pub query: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    pub path: String,
    /// Opens it in its app for the workspace's members; carries no secret.
    pub link: String,
}

/// The workspace's people, each with their role, and the invitations still
/// open.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Members(pub Map<String, Value>);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewInvitation {
    /// `viewer`, `commenter` or `editor`.
    pub role: String,
    /// The address the invitation names; no mail is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A link that signs the person in and lands them in the workspace.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Invitation(pub Map<String, Value>);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Program {
    /// tilolang: `notes.notebook.notes.filter(.pinned)`.
    pub program: String,
    /// Runs a program that would change much or destroy something; without
    /// it such a program answers `preview`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<bool>,
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
    /// Every step of the run that changed something, oldest first.
    #[serde(default)]
    pub steps: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Uploaded {
    /// `tilo.content:<hash>`: what an action takes the file as.
    pub content: String,
    pub size: u64,
}

/// One event of a workspace's stream, or of a webhook's delivery.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// The cursor after it: `Last-Event-ID` resumes there.
    pub id: String,
    /// `change`, `run` or `presence`.
    #[serde(rename = "type")]
    pub kind: String,
    pub workspace: String,
    /// The event as its kind gives it.
    pub data: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWebhook {
    /// `https://…`, where each event is posted.
    pub url: String,
    /// The kinds to post (`change`, `run`, `presence`); all without it.
    #[serde(default)]
    pub events: Vec<String>,
    /// One resource's changes only, and what is under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Webhook {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// What deliveries are signed with: answered once, when it is made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WebhookList {
    pub webhooks: Vec<Webhook>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeletedWebhook {
    pub deleted: String,
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
        Account,
        Workspace,
        WorkspaceList,
        NewWorkspace,
        DeletedWorkspace,
        KeyRow,
        KeyList,
        NewKey,
        MintedKey,
        RevokedKey,
        Listing,
        Resource,
        ActionAnswer,
        SearchHits,
        Link,
        Members,
        NewInvitation,
        Invitation,
        Program,
        ProgramAnswer,
        Uploaded,
        Event,
        NewWebhook,
        Webhook,
        WebhookList,
        DeletedWebhook,
    ]
}

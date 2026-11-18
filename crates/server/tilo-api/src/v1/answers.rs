//! What the API takes and answers, as types: every answer goes out through
//! one of them ([`typed`]), and the `OpenAPI` description's schemas are made from
//! the same types ([`schemas`]), so the two cannot differ.
//!
//! Fields an app types itself (a resource's fields, an action's result) are
//! JSON as the app gives them; their types are the app's surface, which the
//! description carries per app ([`super::openapi`]).

use std::collections::BTreeMap;

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
    /// The host that keeps it: a server's name, or the person's device that
    /// hosts it; absent while none of their devices said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// `owner` for one of the person's own, else the role they hold there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What the person labelled it with when they made it (`customer_id` →
    /// `cus_8121`), read by their own keys alone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// Keys made for this workspace alone run commands in its Linux machine
    /// without asking the person each time; absent while they are asked, as
    /// every workspace starts. Read by the owner's keys alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
}

/// Whether keys made for a workspace alone run commands in its machine
/// without asking the person each time.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UnattendedChange {
    /// `true` asks the person to let them, on their approving device, and
    /// holds for the branches made from the workspace too; `false` asks again
    /// from the next command on.
    pub on: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceList {
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWorkspace {
    pub name: String,
    /// Labels for your programs to find it by, `customer_id` → `cus_8121`:
    /// at most 16, each name 1 to 63 of a-z, 0-9, `_`, `-` and `.`, each value
    /// 1 to 256 printable characters. Only keys of yours read them; nobody
    /// invited in does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// A branch of a workspace: a new workspace of the person's own that starts
/// with everything the source holds and goes on apart from it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewBranch {
    pub name: String,
    /// Labels for your programs to find it by, `customer_id` → `cus_8121`:
    /// at most 16, each name 1 to 63 of a-z, 0-9, `_`, `-` and `.`, each value
    /// 1 to 256 printable characters. Only keys of yours read them; nobody
    /// invited in does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

/// One resource an export could not read, and why.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Unread {
    pub path: String,
    pub error: String,
}

/// Everything a workspace holds, as its key reads it: every resource of
/// every app, read in full, in the order a walk from its apps finds them.
/// The files they name (`tilo.content:` references) are fetched with
/// `getContent`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceExport {
    pub workspace: Workspace,
    pub exported_at_ms: u64,
    pub resources: Vec<Resource>,
    /// What it holds that did not read, each with why.
    #[serde(default)]
    pub unread: Vec<Unread>,
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
    /// The resource's own id, which names it whatever else is titled the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The app's stable key for it, the name its actions answer it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub label: String,
    /// The app's type of it, or `app`.
    pub kind: String,
    #[serde(default)]
    pub has_children: bool,
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
    /// The listing as a person reads it, one line per row: the same lines an
    /// MCP client's `tilo_ls` prints, for a caller with no surface of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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
    /// The read as a person reads it: the same text an MCP client's
    /// `tilo_read` prints, for a caller with no surface of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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

/// Who made the invocation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InvocationBy {
    /// The name it goes by: a key's, as the person named it, an agent's, or a
    /// person's.
    pub name: String,
    /// `agent` (every key is one, acting for its person), `person`, `bot` or
    /// `automation`.
    pub kind: String,
}

/// A record an invocation changed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InvocationChange {
    /// The app, as a path names it (`word-processor`).
    pub app: String,
    /// The app's id (`so.tilo.word-processor`).
    pub app_id: String,
    /// The record's id there; empty for the app's own record of the workspace.
    #[serde(default)]
    pub record: String,
}

/// One invocation of a workspace, as the listing of them answers it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InvocationInfo {
    /// What `…/invocations/{invocation}`, its `cancel` and its `undo` take:
    /// the invocation of its newest call (the undo of any call of an
    /// invocation takes back the whole of it). Absent for one known only from
    /// the workspace's records, whose calls the host no longer remembers (it
    /// restarted since).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Every call of it the host remembers, oldest first: one, or each call
    /// of an invocation its client named with `Tilo-Invocation`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
    /// The undo step its changes are kept under in each record's history
    /// (`step` of a version there): what tells it apart across the host's
    /// restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub by: InvocationBy,
    /// What its client called it (`Tilo-Invocation-Title`), when it named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `waiting` (for the person), `running`, `ended`, `failed`,
    /// `cancelled` or `undone` (everything it changed was taken back).
    pub status: String,
    /// Why it failed, typed (`confirmation_required` when the person did not
    /// allow it). Its words stay with whoever ran it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_ms: i64,
    /// When it ended; absent while it has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
    /// What its last call asked for, `<app>#<action>`, while the host
    /// remembers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// What its newest change did, without the values it carried: the action
    /// by its published label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// How many changes it made to the workspace's records: its edits, and
    /// what it brought in from outside (what a command printed).
    pub changes: u64,
    /// How many of its edits were taken back since; what came from outside is
    /// no edit, and no undo takes it back.
    pub undone: u64,
    /// The records it changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<InvocationChange>,
    /// Its edits still stand and nobody's edit stands on them: its undo would
    /// take back exactly its own.
    pub undoable: bool,
    /// Why it would not be taken back, when someone's change stands on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_blocked: Option<String>,
    /// The workspace's records keep it: it is listed after the host restarts.
    pub recorded: bool,
    /// The host remembers its calls: `id` reads, stops and takes it back.
    pub remembered: bool,
}

/// `GET /v1/workspaces/{workspace}/invocations`: a page of them in records
/// the key may read, newest first: of each invocation only what lies in those
/// records, and the totals over them alone.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InvocationList {
    pub items: Vec<InvocationInfo>,
    /// Invocations the workspace holds, of which `items` is a page.
    pub total: u64,
    /// Of those, the ones waiting for the person.
    pub waiting: u64,
    /// A record could not be read yet: invocations that changed only it are
    /// missing until it can.
    #[serde(default)]
    pub pending: bool,
    /// Passed back as `cursor` for the next page; absent on the last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// The page as a person reads it, one line per invocation: what
    /// `tilo invocations` and an MCP client's `tilo_read workspace/invocations`
    /// print.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
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

/// The workspace's people, each with their role, and the invitations still
/// open.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Members(pub Map<String, Value>);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewInvitation {
    /// `viewer`, `commenter`, `editor`, `administrator` or `owner`.
    pub role: String,
    /// The address the invitation names; no mail is sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The address on the wait list it is for (its `entry`): mailed to it
    /// where the workspace's entry sends mail, and it leaves the list once
    /// someone joins with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting: Option<String>,
}

/// A person the workspace holds by their key from now on.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewMember {
    /// Their user public key, 64 hex characters.
    pub person: String,
    /// `viewer`, `commenter`, `editor`, `moderator`, `administrator` or
    /// `owner`; never more than yours to give. A member already is moved
    /// there.
    pub role: String,
    /// How the workspace names them until they name themselves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The servers keeping the workspace, the one asked first, each with the
/// name it goes by, whether it reads the workspace and whether its machine is
/// a cloud provider's; after a removal or a move whether the server let go
/// confirmed it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Servers {
    /// The workspace's id.
    pub workspace: String,
    pub servers: Vec<KeptServer>,
    /// After a removal or a move: whether the server let go confirmed it no
    /// longer keeps the workspace. One that was not reached is no longer
    /// named to anyone, and lets the workspace go once it hears so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
    /// How many copies the workspace wants kept and where they stand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copies: Option<Value>,
}

/// One server keeping the workspace.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct KeptServer {
    /// The server's device key, 64 hex characters: what removing or moving
    /// it may name it by.
    pub device: String,
    /// The person the server runs for, by their user public key.
    pub person: String,
    /// Whether this is the server asked.
    pub here: bool,
    /// What it calls itself, as its own signed address names it.
    #[serde(default)]
    pub label: String,
    /// Whether it holds the workspace's keys and reads it; a store keeps its
    /// ciphertext and reads none of it.
    #[serde(default)]
    pub reads: bool,
    /// Whether its machine is rented from a cloud provider, whose operator
    /// can read what it holds too.
    #[serde(default)]
    pub cloud: bool,
}

/// Another server to keep the workspace as well.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewServer {
    /// The server's site, like `https://tilo.sh`. You must be let in there,
    /// and it may refuse by its own rules.
    pub origin: String,
}

/// The addresses left at the workspace's entry while it takes people by
/// invitation, oldest first, and whether the entry mails invitations.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WaitList(pub Map<String, Value>);

/// What the workspace is called from now on, by every list and device of it.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceName {
    /// 1 to 80 characters.
    pub name: String,
}

/// Where a member of the workspace is held from now on.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemberRole {
    /// `viewer`, `commenter`, `editor`, `moderator`, `administrator` or
    /// `owner`; never more than yours to give.
    pub role: String,
}

/// How the key's person goes by in the workspace from now on.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewName {
    /// The name the workspace's member list and the invitations they make
    /// show for them.
    pub name: String,
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Uploaded {
    /// `tilo.content:<hash>`: what an action takes the file as.
    pub content: String,
    pub size: u64,
}

/// Several small files put into the workspace in one request.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PutMany {
    /// Each file's bytes, base64.
    pub files: Vec<String>,
}

/// Each file's `tilo.content:<hash>` reference, in the order asked.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PutManyAnswer {
    pub contents: Vec<String>,
}

/// Content hashes (lowercase BLAKE3 hex) to ask whether the workspace holds.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HaveHashes {
    pub hashes: Vec<String>,
}

/// Whether each asked hash is held, in the order asked.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HeldHashes {
    pub have: Vec<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewUpload {
    /// How many bytes the whole file is.
    pub size: u64,
}

/// One upload in pieces: where it stands, and what every byte of it is once
/// the last piece answered.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Upload {
    /// The id the caller named it by.
    pub upload: String,
    /// How many bytes the whole file is.
    pub size: u64,
    /// How many of them are there: the next piece goes at this offset.
    pub offset: u64,
    /// `tilo.content:<hash>`, in the answer of the piece that completed it.
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NewWebhook {
    /// `https://…`, where each event is posted.
    pub url: String,
    /// The kinds to post (`change`, `invocation`, `presence`); all without it.
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

/// An upload dropped before every byte of it was sent.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeletedUpload {
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
        NewBranch,
        UnattendedChange,
        DeletedWorkspace,
        WorkspaceExport,
        KeyRow,
        KeyList,
        NewKey,
        MintedKey,
        RevokedKey,
        Listing,
        Resource,
        ActionAnswer,
        InvocationList,
        SearchHits,
        Link,
        View,
        Members,
        NewInvitation,
        NewMember,
        WaitList,
        Servers,
        KeptServer,
        NewServer,
        Invitation,
        NewName,
        WorkspaceName,
        MemberRole,
        Program,
        ProgramAnswer,
        Uploaded,
        PutMany,
        PutManyAnswer,
        HaveHashes,
        HeldHashes,
        NewUpload,
        Upload,
        Event,
        NewWebhook,
        Webhook,
        WebhookList,
        DeletedWebhook,
        DeletedUpload,
    ]
}

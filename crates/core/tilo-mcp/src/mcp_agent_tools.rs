//! The tools this server offers an agent, as their definitions: what a
//! listing of its tools answers.

use super::{MAX_CALLS, Value, json};

/// The agent tools, built once.
pub(crate) fn tool_definitions() -> &'static [Value] {
    static TOOLS: std::sync::LazyLock<Vec<Value>> = std::sync::LazyLock::new(tools);
    &TOOLS
}

/// What `tilo_do` says of waiting where a call that names no wait is answered `pending`.
const PENDING_BY_DEFAULT: &str = "past wait_ms (default 5000), `pending <id>` for tilo_status";
/// What it says on a turn's own server, which holds such a call until its action has ended.
const HELD_UNTIL_ENDED: &str = "a call is held until its action has ended, and answers `asked` at once where your person must allow it first; name wait_ms only for what is meant to keep going (a server started in a shell): past it, `pending <id>` for tilo_status";

/// `tools` as a server tells them that holds a call until its action has ended.
pub(crate) fn held_until_ended(tools: &mut [Value]) {
    for tool in tools {
        if let Some(description) = tool.get_mut("description")
            && let Some(told) = description.as_str()
            && told.contains(PENDING_BY_DEFAULT)
        {
            *description = told.replace(PENDING_BY_DEFAULT, HELD_UNTIL_ENDED).into();
        }
    }
}

fn tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer"});
    let read = json!({"readOnlyHint":true,"openWorldHint":false});
    let call = json!({
        "type":"object",
        "properties":{"path":string,"args":{"type":"object"}},
        "required":["path"],
        "additionalProperties":false
    });
    [
        (
            "tilo_ls",
            "List what is in the workspace. No path: the apps. `notes`: an app's resources and actions; `notes/Todo`: a note's children and actions; `notes/Todo#rename`: an action's parameters. A path is app[@window]/resource names (title, kind or key), stable across restarts; or `#12` kind: resources or actions; limit and cursor page.",
            json!({"path":string,"kind":{"enum":["resources","actions"]},"limit":integer,"cursor":string}),
            json!([]),
            read.clone(),
        ),
        (
            "tilo_read",
            "Read a resource as compact text. `notes/Todo`: markdown, each block ending in the id (`^c01e`) block actions take; a long read ends with a cursor to pass back. `notes`: what the app publishes. `record:<app>:<id>`: the record as JSON with its revision; after: a revision to wait past; parts: join its chain (Chat's messages: `record:so.tilo.chat:<id>` of a `chain:<id>` row of `tilo_ls chat`)",
            json!({"path":string,"cursor":string,"max_chars":integer,"parts":{"type":"boolean"},"after":string}),
            json!(["path"]),
            read.clone(),
        ),
        (
            "tilo_do",
            "Run an action: `{path: \"notes/Todo#append-paragraph\", args: {text: \"Buy milk\"}}`; several in order, stopping at a failure: `calls: [{path, args}, …]`. `notes#open` starts an app; `terminal#run` runs a shell `command`. Answers `ok` with the result or `failed: why`; past wait_ms (default 5000), `pending <id>` for tilo_status. An invocation_id never runs twice.",
            json!({"path":string,"args":{"type":"object"},"calls":{"type":"array","minItems":1,"maxItems":MAX_CALLS,"items":call},"wait_ms":integer,"invocation_id":string}),
            json!([]),
            // Every hint is MCP's default: it may change anything, anywhere, and not idempotently.
            json!({}),
        ),
        (
            "tilo_link",
            "A link a person opens to see what a path names (`notes/Todo`); only workspace members can open it. mode: app or desktop.",
            json!({"path":string,"mode":{"enum":["app","desktop"]}}),
            json!(["path"]),
            read.clone(),
        ),
        (
            "tilo_view",
            "What a person's window shows for `path`: its labels, values and controls in reading order, each with its action. picture: the view as a PNG. Starts the app when it is not running.",
            json!({"path":string,"picture":{"type":"boolean"},"scale":{"type":"number"}}),
            json!(["path"]),
            read.clone(),
        ),
        (
            "tilo_find",
            "Search what you may read here, best first: `{query: \"jam\"}`, app: \"notes\". In query: in:here (this conversation), after:2026-11-10, before:…; no words needed then. Not the web or mail servers. Per hit: path, `path#action`, when said.",
            json!({"query":string,"app":string,"limit":integer}),
            json!(["query"]),
            read,
        ),
        (
            "tilo",
            "Run a tilolang program as this task: `notes.notebook.notes.filter(.pinned)`. Lets and refs (n3) persist. A bulk or destructive program waits for your person's yes, and says where. `undo`, `undo task`, `history task` (across sessions).",
            json!({"program":string}),
            json!(["program"]),
            json!({"readOnlyHint":false,"destructiveHint":true,"idempotentHint":false,"openWorldHint":false}),
        ),
        (
            "tilo_watch",
            "What changed since cursor: a resource's changes (`path`), invocations starting and ending, who is in the workspace. Held until something happens, at most wait_ms (default 25000); pass back the cursor it answers.",
            json!({"cursor":string,"path":string,"wait_ms":integer}),
            json!([]),
            json!({"readOnlyHint":true,"openWorldHint":false}),
        ),
        (
            "tilo_ask",
            "Ask your person to pick one of your choices, and wait. Answers `answered: <choice>`, `declined`, or past wait_ms `pending <id>` for tilo_status. For their own words, ask in the conversation. Not to allow an action (tilo_do asks), nor for a secret (vault/vault#ask).",
            json!({"question":string,"choices":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":tilo_actions::remote::MAX_ASK_CHOICES},"wait_ms":integer}),
            json!(["question", "choices"]),
            // Only what differs from MCP's defaults (writes, not idempotently).
            json!({"destructiveHint":false,"openWorldHint":false}),
        ),
        (
            "tilo_status",
            "How an invocation went, or with no id this session's (cancel: stop it, undo: take its run back).",
            json!({"invocation_id":string,"cancel":{"type":"boolean"},"undo":{"type":"boolean"}}),
            json!([]),
            json!({"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}),
        ),
    ]
    .into_iter()
    .map(|(name, description, properties, required, annotations)| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false
            },
            "annotations": annotations
        })
    })
    .collect()
}

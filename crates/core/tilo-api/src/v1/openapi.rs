//! The API's `OpenAPI` 3.1 description, written from what the API is made of:
//! its operations ([`super::operations::OPERATIONS`]), the types its answers go
//! out through ([`super::answers`]), and the apps' surfaces, in the form they
//! are kept by, whose actions' parameters and results are the schemas of
//! `runAction`'s body and answer, per app. Nothing in it is written by hand,
//! so it cannot drift from the API; `tilo sdk gen --lang openapi --check`
//! says whether the committed copy is current.

use super::operations::{Answer, Body, OPERATIONS, Operation};
use collab_types::surface::SurfaceFile;
use serde_json::{Map, Value, json};

/// One app whose actions the description carries.
#[derive(Clone, Debug)]
pub struct App {
    /// Its id (`so.tilo.notes`).
    pub id: String,
    /// What people call it.
    pub title: String,
    /// Its surface, in the form it is kept by
    /// (`collab_types::surface::canonical`).
    pub surface: String,
}

/// The name a caller invokes `verb` by, or `None` for the framework's own content verbs
/// (`type`, `migrate`), which no caller invokes.
fn caller_action(verb: &tilo_actions::surface::VerbSurface) -> Option<&str> {
    let action = verb.offered();
    (!tilo_actions::surface::CONTENT_VERBS.contains(&action)).then_some(action)
}

/// The description of the API, with the actions of `apps`.
///
/// # Errors
/// An app's surface is not one.
pub fn document(apps: &[App]) -> Result<Value, String> {
    let mut generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|settings| settings.definitions_path = "/components/schemas".into())
        .into_generator();
    for (_, schema) in super::answers::schemas(&mut generator) {
        drop(schema);
    }
    let mut schemas: Map<String, Value> = generator.take_definitions(true).into_iter().collect();
    let mut actions = Vec::new();
    for app in apps {
        let surface = SurfaceFile::from_json(&app.surface)
            .map_err(|error| format!("{}'s surface: {error}", app.id))?;
        for resource in &surface.resources {
            for verb in &resource.verbs {
                // The action as a caller names it: the name the API offers it by, which is a
                // tilolang app's own (`new_note`) where a change of it claims the id the
                // declaration keeps.
                let Some(action) = caller_action(verb) else {
                    continue;
                };
                let name = schema_name(&resource.kind, action);
                // A parameter published as a field of another travels nested
                // under that one's name, as the call's arguments do.
                let mut properties = Map::new();
                let mut required = Vec::new();
                for param in &verb.params {
                    let schema = if param.schema.is_null() {
                        json!({})
                    } else {
                        param.schema.clone()
                    };
                    if let Some(within) = &param.within {
                        let nested = properties.entry(within.clone()).or_insert_with(
                            || json!({"type": "object", "properties": {}, "required": []}),
                        );
                        if let Some(fields) =
                            nested.get_mut("properties").and_then(Value::as_object_mut)
                        {
                            fields.insert(param.name.clone(), schema);
                        }
                        if param.required
                            && let Some(needed) =
                                nested.get_mut("required").and_then(Value::as_array_mut)
                        {
                            needed.push(json!(param.name));
                        }
                    } else {
                        properties.insert(param.name.clone(), schema);
                        if param.required {
                            required.push(json!(param.name));
                        }
                    }
                }
                schemas.insert(
                    format!("{name}.Arguments"),
                    json!({"type": "object", "properties": properties, "required": required,
                           "description": verb.description.clone().unwrap_or_else(|| verb.label.clone())}),
                );
                if let Some(returns) = verb
                    .returns
                    .as_ref()
                    .filter(|returns| !returns.schema.is_null())
                {
                    schemas.insert(format!("{name}.Result"), returns.schema.clone());
                }
                actions.push(json!({
                    "app": app.id, "resource": resource.kind, "action": action,
                    "arguments": format!("#/components/schemas/{name}.Arguments"),
                }));
            }
        }
    }
    let mut paths = Map::new();
    for operation in OPERATIONS.iter() {
        let entry = paths
            .entry(operation.path.to_owned())
            .or_insert_with(|| json!({}));
        if let Some(entry) = entry.as_object_mut() {
            entry.insert(
                operation.method.as_str().to_lowercase(),
                operation_of(operation),
            );
        }
    }
    Ok(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Tilo API",
            "version": super::API_VERSION,
            "description": "Every resource of a Tilo workspace, its fields and its actions, over HTTP: \
                            docs/developers/api.md says how. Each app's actions take the arguments \
                            and answer the result its surface declares: `x-tilo-actions` lists them, \
                            each naming its arguments' schema.",
        },
        "servers": [{"url": "https://api.tilo.sh"}],
        "tags": TAGS.iter().map(|(name, description)| json!({"name": name, "description": description})).collect::<Vec<_>>(),
        "security": [{"key": []}],
        "paths": paths,
        "components": {
            "schemas": schemas,
            "securitySchemes": {"key": {"type": "http", "scheme": "bearer",
                                        "description": "A key of the person's: `tilo_sk_…`"}},
            "parameters": parameters(),
        },
        "x-tilo-actions": actions,
    }))
}

/// The schema name of `action` on the resource kind `kind`: `notes.notebook.rename`.
fn schema_name(kind: &str, action: &str) -> String {
    format!("{kind}.{action}")
}

fn parameters() -> Value {
    json!({
        "IdempotencyKey": {"name": "Idempotency-Key", "in": "header", "required": false,
            "description": "Names this attempt: a retry with the same key answers what the first did and runs nothing twice",
            "schema": {"type": "string", "minLength": 1, "maxLength": 255}},
        "Invocation": {"name": "Tilo-Invocation", "in": "header", "required": false,
            "description": "The invocation this call is a step of: every call naming it is one undo step per app",
            "schema": {"type": "string"}},
        "InvocationTitle": {"name": "Tilo-Invocation-Title", "in": "header", "required": false,
            "description": "What History calls the invocation", "schema": {"type": "string"}},
        "ContentRange": {"name": "Content-Range", "in": "header", "required": false,
            "description": "`bytes <from>-<to>/<size>`: where this piece of a file starts and ends, and how big the whole file is. Omitted when the whole file is one request.",
            "schema": {"type": "string"}},
    })
}

/// The groups operations are listed in, in order, each with what it is.
const TAGS: &[(&str, &str)] = &[
    (
        "Account",
        "The API itself, and the key's account: the person's workspaces and keys",
    ),
    (
        "Resources",
        "What is in a workspace, by path: its fields, children and actions",
    ),
    ("Actions", "Running an action on a resource"),
    (
        "Invocations",
        "An action's invocation after it answered: awaiting, cancelling, undoing it",
    ),
    ("Events", "What changes in a workspace, as it changes"),
    ("Models", "The person's own model, asked one turn at a time"),
    ("Webhooks", "A workspace's events posted to a URL"),
    ("Files", "Bytes going into a workspace and coming out of it"),
    ("Programs", "Several calls as one step, written in tilolang"),
];

/// The group `path` is listed in: by its first segment inside a workspace.
fn tag_of(path: &str) -> &'static str {
    let Some(inside) = path.strip_prefix("/v1/workspaces/{workspace}/") else {
        return "Account";
    };
    match inside.split('/').next() {
        Some("resources") if inside.contains("/actions/") => "Actions",
        Some("invocations") => "Invocations",
        Some("events") => "Events",
        Some("model") => "Models",
        Some("webhooks") => "Webhooks",
        Some("contents") => "Files",
        Some("programs") => "Programs",
        _ => "Resources",
    }
}

/// What a path parameter named `name` is. Every name an operation's path uses
/// is here: the test below fails on one that is not.
fn path_parameter(name: &str) -> &'static str {
    match name {
        "workspace" => "The workspace's id",
        "path" => "A resource path, each segment percent-encoded",
        "action" => "The action's id",
        "invocation" => "The invocation, as an action answered it",
        "hash" => {
            "The content's address, lowercase BLAKE3 hex: what a put names the file's bytes by, and a reference's address with a sealed item's key after a `.`"
        }
        "webhook" => "The webhook's id, as its creation answered it",
        _ => "",
    }
}

fn operation_of(operation: &Operation) -> Value {
    let mut parameters: Vec<Value> = operation
        .path
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| {
            json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"},
                   "description": path_parameter(name)})
        })
        .collect();
    parameters.extend(operation.query.iter().map(|(name, description)| {
        json!({"name": name, "in": "query", "required": false, "schema": {"type": "string"},
               "description": description})
    }));
    parameters.push(json!({"$ref": "#/components/parameters/Invocation"}));
    parameters.push(json!({"$ref": "#/components/parameters/InvocationTitle"}));
    if operation.id == "putContent" {
        parameters.push(json!({"$ref": "#/components/parameters/ContentRange"}));
    }
    if operation.mutating {
        parameters.push(json!({"$ref": "#/components/parameters/IdempotencyKey"}));
    }
    let schema = |name: &str| json!({"$ref": format!("#/components/schemas/{name}")});
    let json_content = |schema: Value| json!({"application/json": {"schema": schema}});
    let mut responses = Map::new();
    match operation.answers {
        Answer::Json(status, name) => {
            responses.insert(
                status.to_string(),
                json!({"description": operation.summary,
                "content": json_content(schema(name))}),
            );
        }
        Answer::Action => {
            responses.insert(
                "200".into(),
                json!({"description": "It ran",
                "content": json_content(schema("ActionAnswer"))}),
            );
            responses.insert("202".into(), json!({"description": "It has not finished, or waits for the person: follow `Location`",
                "content": json_content(schema("ActionAnswer"))}));
        }
        Answer::Events => {
            responses.insert(
                "200".into(),
                json!({"description": "Server-sent events, each an `Event`'s `type` and `data`",
                "content": {"text/event-stream": {"schema": {"type": "string"}}}}),
            );
        }
        Answer::Bytes => {
            responses.insert("200".into(), json!({"description": "The bytes",
                "content": {"application/octet-stream": {"schema": {"type": "string", "contentMediaType": "application/octet-stream"}}}}));
        }
        Answer::Content => {
            responses.insert(
                "200".into(),
                json!({"description": "A piece is there: send the next from `offset`",
                "content": json_content(schema("PutContent"))}),
            );
            responses.insert(
                "201".into(),
                json!({"description": "Every byte is there",
                "content": json_content(schema("PutContent"))}),
            );
        }
    }
    responses.insert(
        "default".into(),
        json!({"description": "A refusal",
        "content": json_content(schema("Error"))}),
    );
    let mut described = Map::new();
    described.insert("operationId".into(), json!(operation.id));
    described.insert("summary".into(), json!(operation.summary));
    described.insert("tags".into(), json!([tag_of(operation.path)]));
    described.insert("parameters".into(), json!(parameters));
    described.insert("responses".into(), Value::Object(responses));
    let body = match operation.body {
        Body::None => None,
        Body::Json(name) => Some(json_content(schema(name))),
        Body::Arguments => Some(json_content(json!({"type": "object",
            "description": "The action's arguments: x-tilo-actions names each action's schema"}))),
        Body::Bytes => Some(
            json!({"*/*": {"schema": {"type": "string", "contentMediaType": "application/octet-stream"}}}),
        ),
    };
    if let Some(content) = body {
        described.insert(
            "requestBody".into(),
            json!({"required": true, "content": content}),
        );
    }
    if operation.id == "getApi" {
        described.insert("security".into(), json!([{}, {"key": []}]));
    }
    Value::Object(described)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_and_every_schema_it_names_is_described() {
        // An app's surface travels in the form it is kept by: its JSON, every
        // object's keys in order.
        let surface = r#"{"resources":[{"kind":"notes.notebook","label":"Notebook","version":1,"verbs":[{"id":"rename","kind":"undoable","params":[{"name":"title","required":true,"schema":{"type":"string"},"type":"text"}]}]}]}"#;
        let described = document(&[App {
            id: "so.tilo.notes".into(),
            title: "Notes".into(),
            surface: surface.into(),
        }])
        .unwrap();
        let text = described.to_string();
        for operation in OPERATIONS.iter() {
            assert!(text.contains(&format!("\"operationId\":\"{}\"", operation.id)));
            for name in operation
                .path
                .split('/')
                .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
            {
                assert!(
                    !path_parameter(name).is_empty(),
                    "{}: {{{name}}} is not described",
                    operation.id
                );
            }
        }
        let schemas = described["components"]["schemas"].as_object().unwrap();
        let named: Vec<&str> = text
            .split("#/components/schemas/")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap())
            .collect();
        for name in named {
            assert!(
                schemas.contains_key(name),
                "{name} is named but not described"
            );
        }
        assert_eq!(
            schemas["notes.notebook.rename.Arguments"]["required"],
            json!(["title"])
        );
        // The account's actions are run as any action is, outside every workspace.
        let run = &described["paths"]["/v1/resources/{path}/actions/{action}"]["post"];
        assert_eq!(run["operationId"], "runAccountAction");
        assert_eq!(run["tags"], json!(["Account"]));
    }
}

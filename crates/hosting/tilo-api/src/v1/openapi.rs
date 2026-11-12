//! The API's `OpenAPI` 3.1 description, written from what the API is made of:
//! its operations ([`super::operations::OPERATIONS`]), the types its answers go
//! out through ([`super::answers`]), and the apps' surfaces (`surface.toml`),
//! whose actions' parameters and results are the schemas of
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
    /// Its `surface.toml`.
    pub surface: String,
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
        let surface = SurfaceFile::parse(app.surface.as_bytes())
            .map_err(|error| format!("{}'s surface: {error}", app.id))?;
        for resource in &surface.resources {
            for verb in &resource.verbs {
                let name = schema_name(&resource.kind, &verb.id);
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
                    "app": app.id, "resource": resource.kind, "action": verb.id,
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
        "Run": {"name": "Tilo-Run", "in": "header", "required": false,
            "description": "The run this call is a step of: every call naming it is one undo step per app",
            "schema": {"type": "string"}},
        "RunTitle": {"name": "Tilo-Run-Title", "in": "header", "required": false,
            "description": "What History calls the run", "schema": {"type": "string"}},
    })
}

fn operation_of(operation: &Operation) -> Value {
    let mut parameters: Vec<Value> = operation
        .path
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| {
            json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"},
            "description": match name {
                "workspace" => "The workspace's id",
                "path" => "A resource path, each segment percent-encoded",
                "action" => "The action's id",
                "run" => "The run, as an action answered it",
                "content" => "`tilo.content:<hash>`",
                _ => "",
            }})
        })
        .collect();
    parameters.extend(operation.query.iter().map(|(name, description)| {
        json!({"name": name, "in": "query", "required": false, "schema": {"type": "string"},
               "description": description})
    }));
    parameters.push(json!({"$ref": "#/components/parameters/Run"}));
    parameters.push(json!({"$ref": "#/components/parameters/RunTitle"}));
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
    }
    responses.insert(
        "default".into(),
        json!({"description": "A refusal",
        "content": json_content(schema("Error"))}),
    );
    let mut described = Map::new();
    described.insert("operationId".into(), json!(operation.id));
    described.insert("summary".into(), json!(operation.summary));
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
        let surface = "[[resources]]\nkind = \"notes.notebook\"\nlabel = \"Notebook\"\nversion = 1\n\n\
                       [[resources.verbs]]\nid = \"rename\"\nkind = \"undoable\"\n\
                       params = [{ name = \"title\", type = \"text\", required = true, schema = { type = \"string\" } }]\n";
        let described = document(&[App {
            id: "so.tilo.notes".into(),
            title: "Notes".into(),
            surface: surface.into(),
        }])
        .unwrap();
        let text = described.to_string();
        for operation in OPERATIONS.iter() {
            assert!(text.contains(&format!("\"operationId\":\"{}\"", operation.id)));
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
    }
}

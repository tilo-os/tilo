//! The names an action answers by, and how close a name that runs nothing is
//! to one that would: an answer names a resource whole
//! (`tasks.tracker/issues/7`), and that is a path the next call takes.

use super::{
    ActionDescriptor, ActionProvider, Failure, Level, Levels, McpServer, Node, ROOT_PEEK, Resolved,
    Value, json,
};

/// The close matches an action no resource offers is told with.
pub(super) const CLOSE_MATCHES: usize = 5;

/// `arguments` with each parameter's value written as the label a person reads
/// replaced by the value the app takes, whatever its case or spacing
/// (`In review` for `in-review`, a status or a choice of a kind of thing alike).
fn bind_choices(descriptor: &ActionDescriptor, arguments: &mut serde_json::Map<String, Value>) {
    for param in &descriptor.params {
        let Some(given) = arguments
            .get(param.name.as_ref())
            .and_then(Value::as_str)
            .map(plain)
        else {
            continue;
        };
        let choice = param
            .choices
            .iter()
            .find(|choice| plain(choice) == given)
            .map(|choice| Value::String(choice.to_string()))
            .or_else(|| {
                forms(param.schema.as_deref()?)
                    .into_iter()
                    .find(|(name, _)| plain(name) == given)
                    .map(|(_, value)| value)
            });
        if let Some(choice) = choice {
            arguments.insert(param.name.to_string(), choice);
        }
    }
}

/// The action ids closest to `name`, best first, at most `limit`: those it is
/// part of, one of whose words it is, or whose start it shares.
pub(super) fn close_matches(name: &str, ids: &[String], limit: usize) -> Vec<String> {
    let lower = name.to_lowercase();
    let mut scored: Vec<(u8, &String)> = ids
        .iter()
        .filter_map(|id| {
            let named = id.to_lowercase();
            let rank = if named.contains(&lower) {
                0
            } else if lower.contains(&named) {
                1
            } else if shared_start(&named, &lower) >= 3 {
                2
            } else {
                return None;
            };
            Some((rank, id))
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, id)| id.clone())
        .collect()
}

/// How many bytes the start of `a` and `b` share, to the last `-`.
fn shared_start(a: &str, b: &str) -> usize {
    let common = a
        .bytes()
        .zip(b.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    a.get(..common)
        .and_then(|start| start.rfind('-'))
        .unwrap_or(common)
}

/// The app a name beginning with a kind names (`tasks.tracker` is the Tasks
/// app's tracker), when the name is one kind and no app: a kind is the app's
/// own name and one more word, and an item's kind names its collection after
/// that (`tasks.issues.issues`, `tasks.issues.links`), where a package's id is
/// a domain and its names.
pub(super) fn kind_address(name: &str) -> Option<&str> {
    let (app, kind) = name.split_once('.')?;
    (!app.is_empty() && !kind.is_empty() && !kind.starts_with('.') && !kind.ends_with('.'))
        .then_some(app)
}

impl Node {
    /// Whether `address` names this resource whole, as an action answers it
    /// (`tasks.tracker/issues/7`): the id it is published under, or its key.
    pub(super) fn address_is(&self, address: &str) -> bool {
        self.id == address || self.key == address
    }
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `arguments` with each parameter of `descriptor` that picks a resource of
    /// the workspace (`ParamType::Resource`) bound from the address a caller
    /// wrote: a path (`notes/Launch checklist`, the one `tilo_ls` lists) is
    /// resolved as the tools resolve one, and handed over as the reference an
    /// app takes (its app, kind, id and label). Any other value is the caller's
    /// own reference, passed as given.
    pub(super) async fn bind_resource_arguments(
        &self,
        descriptor: &ActionDescriptor,
        mut arguments: serde_json::Map<String, Value>,
    ) -> Result<serde_json::Map<String, Value>, Failure> {
        for param in &descriptor.params {
            self.bind_resource_argument(param, &mut arguments).await?;
        }
        // What a parameter takes as text is sent as text: a number a person or a model writes
        // for one (`{"issue": 1}`) is the text the app reads.
        for param in &descriptor.params {
            if param.ty == tilo_actions::ParamType::Text
                && let Some(number) = arguments
                    .get(param.name.as_ref())
                    .filter(|value| value.is_number())
                    .cloned()
            {
                arguments.insert(
                    param.name.to_string(),
                    Value::String(match &number {
                        Value::Number(number) => number.to_string(),
                        _ => String::new(),
                    }),
                );
            }
        }
        bind_choices(descriptor, &mut arguments);
        Ok(arguments)
    }

    /// `arguments` with `param`'s value bound, when it picks a resource of the
    /// workspace, from the address a caller wrote: a path is resolved as the
    /// tools resolve one, and handed over as the reference an app takes. Any
    /// other value is the caller's own reference, passed as given.
    async fn bind_resource_argument(
        &self,
        param: &tilo_actions::ActionParam,
        arguments: &mut serde_json::Map<String, Value>,
    ) -> Result<(), Failure> {
        // A resource parameter is typed as one, or declared as a reference
        // to one of a kind (`resource = "notes.pages.pages"`): either way
        // what it picks is a resource of the workspace.
        let picks = param.ty == tilo_actions::ParamType::Resource
            || param
                .schema
                .as_ref()
                .and_then(|schema| schema.get(tilo_actions::PICK_ANNOTATION))
                .and_then(Value::as_str)
                == Some("resource");
        if !picks {
            return Ok(());
        }
        let value = match arguments.get(param.name.as_ref()) {
            Some(value) if !value.is_null() => value.clone(),
            _ => return Ok(()),
        };
        // A reference written whole is the caller's own; one written by its id alone,
        // or as an address the tools list, is resolved as any address is.
        let (address, missing, by_id) = match &value {
            serde_json::Value::Object(fields) => match reference_missing(fields) {
                missing if missing.is_empty() => return Ok(()),
                missing => (
                    fields
                        .get("id")
                        .or_else(|| fields.get("resource"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned),
                    missing,
                    true,
                ),
            },
            serde_json::Value::String(text) => (
                Some(text.trim().to_owned()).filter(|text| !text.is_empty()),
                Vec::new(),
                false,
            ),
            _ => return Ok(()),
        };
        let Some(address) = address else {
            return Err(reference_refusal(param.name.as_ref(), &missing, None).into());
        };
        // An id is the address an item is published under (`notes.pages/pages/<id>`), read
        // as an address whole; a string is a path, as a listing writes one.
        let parsed = if by_id {
            ParsedPath {
                app: address.clone(),
                segments: Vec::new(),
                action: None,
            }
        } else {
            match parse_path(&address) {
                Ok(parsed) => parsed,
                Err(why) => return Err(format!("invalid_arguments: {why}").into()),
            }
        };
        let resolved = match self.resolve(&parsed).await? {
            (Ok(resolved), _) => resolved,
            (Err(note), _) => {
                return Err(reference_refusal(param.name.as_ref(), &missing, Some(&note)).into());
            }
        };
        let Some(node) = resolved.node.clone() else {
            return Err(reference_refusal(
                param.name.as_ref(),
                &missing,
                Some(&format!("{address} names no resource")),
            )
            .into());
        };
        let kept = tilo_actions::remote::resource_parts(&resolved.provider)
            .filter(|(app, _)| *app == resolved.app_id)
            .map(|(_, kept)| kept.to_owned())
            .filter(|kept| !kept.is_empty());
        let mut reference = serde_json::Map::new();
        reference.insert("app".into(), json!(resolved.app_id));
        reference.insert("kind".into(), json!(node.kind));
        reference.insert("id".into(), json!(node.id));
        reference.insert("label".into(), json!(node.label));
        if let Some(kept) = kept {
            reference.insert("record".into(), json!(kept));
        }
        arguments.insert(param.name.to_string(), Value::Object(reference));
        Ok(())
    }

    /// The resource `address` names, written either as its listing publishes
    /// it (`chat.messages/messages/m1`) or in the stable form a link carries
    /// (`chat.messages.messages:m1`): both name one resource, so both are
    /// answered alike.
    pub(super) async fn by_item_address(
        &self,
        app_id: &str,
        provider: &str,
        address: &str,
        levels: &mut Levels,
    ) -> Result<Option<Resolved>, String> {
        if let Some(found) = self.by_address(app_id, provider, address, levels).await? {
            return Ok(Some(found));
        }
        let Some(form) = tilo_actions::item_form_of(address) else {
            return Ok(None);
        };
        self.by_address(app_id, provider, &form, levels).await
    }

    /// The resource the app lists under exactly `address`, with its actions
    /// and what it holds: an answer's own naming of what it made or changed.
    pub(super) async fn by_address(
        &self,
        app_id: &str,
        provider: &str,
        address: &str,
        levels: &mut Levels,
    ) -> Result<Option<Resolved>, String> {
        let root = self.app_children(provider, levels).await?;
        let mut found = root
            .children
            .iter()
            .find(|node| node.address_is(address))
            .cloned();
        // What a folder of a resource holds is a level further down than
        // the app's own listing reaches (`notes.notebook/page/<id>/body`).
        if found.is_none() {
            let containers: Vec<Node> = root
                .children
                .iter()
                .filter(|node| node.container)
                .take(ROOT_PEEK)
                .cloned()
                .collect();
            let addressed = |level: &Level| -> Result<Option<Node>, String> {
                Ok(level
                    .whole_children()
                    .iter()
                    .find(|node| node.address_is(address))
                    .cloned())
            };
            for container in containers {
                // An address that begins with a container's key names one of the
                // container's own, so its level is read on past its bound to find
                // it; any other container is looked through as far as it was read.
                let own = address
                    .strip_prefix(container.key.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
                let node = if own {
                    self.level_holding(provider, &container.id, levels, addressed)
                        .await?
                        .0
                } else {
                    addressed(&self.level(provider, &container.id, levels).await?)?
                };
                if let Some(node) = node {
                    found = Some(node);
                    break;
                }
            }
        }
        let Some(node) = found else {
            return Ok(None);
        };
        let here = self.level(provider, &node.id, levels).await?;
        Ok(Some(Resolved {
            app_id: app_id.to_owned(),
            provider: provider.to_owned(),
            path: address.to_owned(),
            actions: if here.own.is_empty() {
                node.actions.clone()
            } else {
                here.own.clone()
            },
            children: here.children.clone(),
            node: Some(node.clone()),
            chain: vec![node],
            levels: std::mem::take(levels),
            revision: here.revision,
        }))
    }
}

/// A path, taken apart: `app/segment/segment#action`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ParsedPath {
    pub(super) app: String,
    pub(super) segments: Vec<String>,
    pub(super) action: Option<String>,
}

impl ParsedPath {
    /// The path as it names a resource, whole: the app and every segment,
    /// without an action after `#`.
    pub(super) fn address(&self) -> String {
        if self.segments.is_empty() {
            self.app.clone()
        } else {
            format!("{}/{}", self.app, self.segments.join("/"))
        }
    }
}

/// Parse `text`. A name containing `/`, `#` or `\` writes it with a leading
/// backslash.
pub(super) fn parse_path(text: &str) -> Result<ParsedPath, String> {
    let mut parts: Vec<String> = vec![String::new()];
    let mut action = String::new();
    let mut in_action = false;
    let mut escaped = false;
    for character in text.trim().chars() {
        if escaped {
            escaped = false;
            push_char(&mut parts, &mut action, in_action, character);
            continue;
        }
        match character {
            '\\' => escaped = true,
            '#' if !in_action => in_action = true,
            '/' if !in_action => parts.push(String::new()),
            other => push_char(&mut parts, &mut action, in_action, other),
        }
    }
    let action = in_action.then_some(action);
    let mut parts = parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty());
    let app = parts
        .next()
        .ok_or("a path starts with an app: notes/Todo")?;
    let action = action
        .map(|action| action.trim().to_owned())
        .filter(|action| !action.is_empty());
    Ok(ParsedPath {
        app,
        segments: parts.collect(),
        action,
    })
}

fn push_char(parts: &mut [String], action: &mut String, in_action: bool, character: char) {
    if in_action {
        action.push(character);
    } else if let Some(part) = parts.last_mut() {
        part.push(character);
    }
}

/// `text` as a value is compared to a parameter's choices: its letters alone,
/// in one case, so `In review`, `In Review` and `in-review` are one value.
fn plain(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// The values a parameter's schema offers as alternatives, each as the name a
/// person reads it by and the value the app takes (`{"title": "In review",
/// "const": "in-review"}`, or a one-field object with the constant).
fn forms(schema: &Value) -> Vec<(String, Value)> {
    let Some(alternatives) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut forms = Vec::new();
    for alternative in alternatives {
        let Some(fields) = alternative.as_object() else {
            continue;
        };
        let constant = fields.get("const").cloned().or_else(|| {
            let only = fields.get("required")?.as_array()?.first()?.as_str()?;
            fields.get("properties")?.get(only)?.get("const").cloned()
        });
        let name = fields
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| constant.as_ref().and_then(Value::as_str).map(str::to_owned));
        if let (Some(name), Some(constant)) = (name, constant) {
            forms.push((name, constant));
        }
    }
    forms
}

/// What a resource reference an action takes is written with.
const REFERENCE_FIELDS: [&str; 4] = ["app", "kind", "id", "label"];

/// The properties of a resource reference `fields` leaves out or leaves empty.
fn reference_missing(fields: &serde_json::Map<String, Value>) -> Vec<&'static str> {
    REFERENCE_FIELDS
        .iter()
        .copied()
        .filter(|name| {
            fields
                .get(*name)
                .and_then(Value::as_str)
                .is_none_or(|text| text.trim().is_empty())
        })
        .collect()
}

/// Why `name`, a parameter that picks a resource of the workspace, was not answered: what it
/// lacks and where to get it, said whole in one refusal rather than one property at a time.
fn reference_refusal(name: &str, missing: &[&str], why: Option<&str>) -> String {
    let lacks = if missing.is_empty() {
        "its app, kind, id and label".to_owned()
    } else {
        format!("its {}", missing.join(", "))
    };
    let why = why.map(|why| format!(" ({why})")).unwrap_or_default();
    format!(
        "invalid_arguments: `{name}` is a resource of the workspace: name it by the path \
         `tilo_ls` lists it at (`notes/Launch checklist`), or as an object with its app, kind, id \
         and label, as a listing's row carries them; what it lacks: {lacks}{why}"
    )
}

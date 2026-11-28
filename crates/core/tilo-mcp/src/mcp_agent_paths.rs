//! The resource an argument names, bound from the path a caller wrote, and
//! the address an action answers by: an answer names a resource whole
//! (`tasks.tracker/issues/7`), and that is a path the next call takes.

use super::{ActionDescriptor, ActionProvider, Failure, McpServer, Value, json};
use tilo_actions::tree::{Address, Segment, bind_choices};

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
        // A secret named by reference (`vault.item:<id>`) is its person's, not the
        // workspace's, so no path resolves it: the app reads the reference as given.
        if tilo_actions::remote::takes_item(param) {
            return Ok(());
        }
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
        // A secret is taken by the address of an item of its person's vault
        // (`vault.item:<id>`). No listing shows that item and no path of the workspace
        // leads to it, so there is nothing to look it up in: it passes as written, and the
        // action that takes it holds it to its person and to the place the item belongs to.
        // The action's own declaration of the parameter decides that, never what a caller
        // wrote there.
        if tilo_actions::remote::takes_item(param) {
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
            Address {
                segments: vec![Segment::named(address.clone())],
                action: None,
            }
        } else {
            match Address::parse(&address) {
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
        let mut reference = serde_json::Map::new();
        reference.insert("app".into(), json!(resolved.app_id));
        reference.insert("kind".into(), json!(node.resource_type));
        reference.insert("id".into(), json!(node.id));
        reference.insert("label".into(), json!(node.label));
        arguments.insert(param.name.to_string(), Value::Object(reference));
        Ok(())
    }
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

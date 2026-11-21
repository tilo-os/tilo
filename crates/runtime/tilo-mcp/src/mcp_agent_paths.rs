//! The resource an argument names, bound from the path a caller wrote, and
//! the address an action answers by: an answer names a resource whole
//! (`tasks.tracker/issues/7`), and that is a path the next call takes.

use super::{
    ActionDescriptor, ActionProvider, Failure, Level, Levels, McpServer, Node, ROOT_PEEK, Resolved,
    Value, json,
};
use tilo_actions::tree::{Address, Segment, bind_choices};

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
        if tilo_actions::remote::person_vault::takes_item(param) {
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
        if tilo_actions::remote::person_vault::takes_item(param) {
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
        let kept = tilo_actions::remote::resource_parts(&resolved.provider)
            .filter(|(app, _)| *app == resolved.app_id)
            .map(|(_, kept)| kept.to_owned())
            .filter(|kept| !kept.is_empty());
        let mut reference = serde_json::Map::new();
        reference.insert("app".into(), json!(resolved.app_id));
        reference.insert("kind".into(), json!(node.resource_type));
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

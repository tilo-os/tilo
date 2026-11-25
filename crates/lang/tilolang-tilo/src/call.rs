//! How code calls an app whose code runs elsewhere: a task's program, or an app's action calling
//! another app's. [`Callees::call`] builds what the app's action API takes, whatever language the
//! app is written in, and [`Callees::answered`] reads its answer back as the code's value.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::delegate::{Invocation, Invoked};
use tilolang_runtime::meta::{Meta, parse_ty};
use tilolang_runtime::value::{ResourceRef, Value, from_json};

use crate::host::argument_json;
use crate::native::Native;
use crate::publish::{package_resource_id, resource_id};
use crate::task::Answered;

/// The apps a runtime calls without running their code, and how each is reached.
#[derive(Default)]
pub struct Callees {
    /// Every package a call's arguments may name, by app.
    pub apps: HashMap<String, Meta>,
    /// The native apps among them, which a program invokes on their own documents.
    pub native: Rc<Native>,
    /// The platform's apps, whose resources the session publishes under their own ids, and the
    /// packages of the providers the device registered ([`Callees::provide`]).
    pub published: HashSet<String>,
    /// The resource each loose action is published on, by the action's id: the session's (its
    /// own ids, `Shell/place`), and a registered provider's (`<app>#<Type>/<verb>`).
    pub addresses: BTreeMap<String, String>,
    /// The workspace the call is made in: what a dispatch serving more than one routes it by, so a
    /// call reaches a record in another workspace the person belongs to.
    pub workspace: Option<String>,
}

impl Callees {
    /// Call the actions of `packages`, which the providers this device registered publish, at
    /// the addresses each registered them at: the package `tilolang check` read for the code is
    /// the one they publish now.
    pub fn provide(&mut self, packages: &[tilo_actions::remote::ProvidedPackage]) {
        for package in packages {
            let (meta, addresses) = crate::catalogue::provided(package);
            self.published.insert(meta.app.clone());
            self.addresses.extend(addresses);
            self.apps.insert(meta.app.clone(), meta);
        }
    }

    /// The call `invocation` is at its app's action API, as JSON (`{app, resource, action,
    /// arguments}`, and `published` for one the session publishes); refused for an app this
    /// code may not use, failed for an invocation that does not fit what the app declares.
    ///
    /// # Errors
    /// What the code is told instead of a call.
    pub fn call(&self, invocation: &Invocation) -> Result<Json, Answered> {
        // An invocation record's own action: the record's address names the workspace it is in
        // and its id there, and the app that workspace publishes answers the verb there. No
        // package of this host has to know the app.
        if let Some(call) = invocation_record_call(invocation) {
            return call.map_err(Answered::Failed);
        }
        let app = invocation.app();
        let meta = self
            .apps
            .get(app)
            .ok_or_else(|| Answered::Refused(format!("this code may use no app {app}")))?;
        let mut call = if app == tilolang::ACTIONS_APP {
            // What the workspace's action registry offers, read and called by address: its
            // provider is the one the dispatch answers it under, not an app's.
            actions_call(meta, invocation)
        } else if self.published.contains(app) {
            published_call(meta, invocation, &self.addresses, |r| self.published_id(r))
        } else if self.native.is(app) {
            native_call(&self.apps, meta, self.native.record(app), invocation)
        } else {
            call_of(&self.apps, meta, invocation)
        }
        .map_err(Answered::Failed)?;
        if let (Some(workspace), Some(fields)) = (&self.workspace, call.as_object_mut()) {
            fields.insert("workspace".to_owned(), json!(workspace));
        }
        Ok(call)
    }

    /// What `answer`, the app's answer to `invocation`, is to the code that made it.
    #[must_use]
    pub fn answered(&self, invocation: &Invocation, answer: Answered) -> Invoked {
        match answer {
            // One that ran with an answer the code cannot go on with failed, rather than going
            // on with nothing in its place.
            Answered::Done(output) => self
                .apps
                .get(invocation.app())
                .map_or(Ok(Value::Unit), |meta| {
                    result_of(&self.apps, meta, invocation, &output)
                })
                .map_or_else(Invoked::Failed, Invoked::Done),
            Answered::Failed(why) => Invoked::Failed(why),
            Answered::Refused(why) => Invoked::Refused(why),
        }
    }

    /// The id the session publishes the resource `r` under, as a published action takes it: a
    /// resource of one of the platform's apps (a window) by its own id, a tilolang app's by its
    /// app and its id in its document (`tl:so.tilo.notes:<id>`). `r`'s type names the app:
    /// a type of a package held here by its id, any other as `<app>#<Type>`.
    #[must_use]
    pub fn published_id(&self, r: &ResourceRef) -> String {
        let id = r.id_string();
        let app = match r.ty.as_str().split_once('#') {
            Some((app, _)) => Some(app.to_owned()),
            None => self
                .apps
                .values()
                .find(|meta| meta.types.iter().any(|ty| ty.id == r.ty.as_str()))
                .map(|meta| meta.app.clone()),
        };
        match app {
            Some(app) if !self.published.contains(&app) => resource_id(&app, &id),
            _ => id,
        }
    }
}

/// The call an action of the host package makes on an invocation record: the record's own verb,
/// in the app the workspace its address names publishes, with the workspace beside the record's
/// id there. `None` for an invocation on anything else.
fn invocation_record_call(invocation: &Invocation) -> Option<Result<Json, String>> {
    let Invocation::Action {
        app,
        action,
        args,
        target: Some(target),
        ..
    } = invocation
    else {
        return None;
    };
    if target.ty != tilolang::INVOCATION_TYPE {
        return None;
    }
    let mut arguments = serde_json::Map::new();
    for (name, value) in args {
        let value = match value {
            Value::Opt(None) => continue,
            Value::Opt(Some(inner)) => inner.as_ref(),
            other => other,
        };
        let json = match tilolang_runtime::value::to_argument(value) {
            Ok(json) => json,
            Err(error) => return Some(Err(format!("`{name}` does not send: {error}"))),
        };
        arguments.insert(name.clone(), json);
    }
    let record = target.item.clone().unwrap_or_else(|| target.doc.clone());
    let workspace = target.doc.clone();
    Some(Ok(json!({
        "app": app,
        // The workspace's invocations provider answers an invocation record's actions; the
        // record's app names it in the catalogue tree, not the provider that runs it.
        "provider": tilo_actions::remote::workspace_invocations::PROVIDER,
        "workspace": workspace,
        "resource": record,
        "action": action,
        "arguments": arguments,
        "published": true,
    })))
}

/// Whether the parameter `name` of the action `action` of `meta` is a flag (`Bool`, or an
/// optional one).
fn declared_flag(meta: &Meta, action: &str, name: &str) -> bool {
    meta.actions
        .iter()
        .find(|declared| declared.id == action)
        .and_then(|declared| declared.params.iter().find(|param| param.name == name))
        .and_then(|param| {
            tilo_failure::ignored!(
                "a type this cannot read is no flag; the action still checks the value",
                parse_ty(&param.ty, &meta.app).ok()
            )
        })
        .is_some_and(|ty| match ty {
            tilolang_runtime::meta::Ty::Bool => true,
            tilolang_runtime::meta::Ty::Option(inner) => {
                matches!(*inner, tilolang_runtime::meta::Ty::Bool)
            }
            _ => false,
        })
}

/// The API's call for `invocation` of the platform's package `meta`: its declared verb on the
/// resource the session publishes it as (`shell:layout`), with every other argument as the
/// published action takes it, by name, a resource by the id `published_id` gives it. The call
/// says it is the session's (`published`), which the API sends to the session's own provider. A
/// loose action (the session's `place`) goes to the resource `addresses` says it is published on.
fn published_call(
    meta: &Meta,
    invocation: &Invocation,
    addresses: &BTreeMap<String, String>,
    published_id: impl Fn(&ResourceRef) -> String,
) -> Result<Json, String> {
    let Invocation::Action {
        app,
        action,
        name,
        args,
        target,
        from,
        ..
    } = invocation
    else {
        return match invocation {
            // A control bound to a field of one of the package's resources: the resource publishes
            // its own typed field edit, so a field is set as a resource's field is set, and the
            // change is admitted, recorded and undone like any other. A field of a record held in
            // one is set at its path from the resource.
            Invocation::Edit {
                app,
                target,
                path,
                field,
                field_id,
                value,
                ..
            } => {
                let mut at: Vec<Json> = path.iter().map(|key| json!({ "field": key })).collect();
                // The key the record stores the field under, as the field's own edit names it.
                at.push(json!({
                    "field": field_id
                        .rsplit_once('.')
                        .map_or(field.as_str(), |(_, key)| key)
                }));
                // A resource whose fields each have their own setter (a settings section,
                // `set-<field>`) publishes no generic field edit: the edit is its setter of the
                // field it is in, given the value, or the edit inside it from that field.
                let setter = at
                    .first()
                    .and_then(|step| step["field"].as_str())
                    .map(|top| format!("{}/{}", target.ty, tilo_actions::surface::setter_id(top)))
                    .and_then(|id| {
                        meta.actions
                            .iter()
                            .find(|declared| declared.id.ends_with(&id))
                    });
                if let Some(setter) = setter {
                    let arguments = if at.len() == 1 {
                        json!({ "value": edited(value)? })
                    } else {
                        json!({ "edit": { "path": at.get(1..).unwrap_or_default(), "op": "set", "value": edited(value)? } })
                    };
                    return Ok(json!({
                        "app": app,
                        "resource": target.id_string(),
                        "action": setter.verb,
                        "arguments": arguments,
                        "published": true,
                    }));
                }
                Ok(json!({
                    "app": app,
                    "resource": target.id_string(),
                    "action": "field-edit",
                    "arguments": { "edit": { "path": at, "op": "set", "value": edited(value)? } },
                    "published": true,
                }))
            }
            // An action never reaches here: the branch above built it. Named so the match is
            // whole.
            Invocation::Action { .. } => Err(format!(
                "{} publishes its resources' fields read-only; change them through its actions",
                meta.title
            )),
        };
    };
    // A platform package's action is its published verb, by the same id (`show-surface`).
    let resource = match target {
        Some(target) => target.id_string(),
        // The session's loose actions keep their own ids; a provider's are named by its app.
        None => addresses
            .get(action)
            .cloned()
            .filter(|_| {
                app == crate::platform_apps::SESSION_APP
                    || crate::catalogue::published_verb(app, action) != action.as_str()
            })
            .ok_or_else(|| format!("`{action}` of {app} names no resource it is published on"))?,
    };
    let mut arguments = serde_json::Map::new();
    for (name, value) in args {
        if matches!(value, Value::Ref(r) if r.target.id_string() == resource) {
            continue;
        }
        // An optional argument given as `.none` is not sent: the published action's own default
        // holds. One given as `.some(x)` is sent as `x`, as the published action takes it.
        let value = match value {
            Value::Opt(None) => continue,
            Value::Opt(Some(inner)) => inner.as_ref(),
            other => other,
        };
        let json = match value {
            Value::Ref(r) => json!(published_id(&r.target)),
            // A flag travels as the integer the guest boxed it as; the published action takes
            // it as the flag its parameter declares.
            Value::Int(n) if declared_flag(meta, action, name) => json!(*n != 0),
            other => tilolang_runtime::value::to_argument(other)
                .map_err(|error| format!("`{name}` does not send: {error}"))?,
        };
        arguments.insert(name.clone(), json);
    }
    made_from(from.as_ref(), &mut arguments, &published_id);
    Ok(json!({
        "app": app,
        "resource": resource,
        // As the app publishes it: the verb, not the id the package names it by.
        "action": name,
        "arguments": arguments,
        "published": true,
    }))
}

/// The value an edit of a field writes, as the field's own edit takes it: a case by its name (the
/// case a field of an enumeration holds), nothing for an optional field, and any other value as
/// the language gives an argument.
fn edited(value: &Value) -> Result<Json, String> {
    match value {
        Value::Enum(case) if case.fields.is_empty() => Ok(json!(case.variant)),
        Value::Opt(None) => Ok(Json::Null),
        Value::Opt(Some(inner)) => edited(inner),
        other => tilolang_runtime::value::to_argument(other)
            .map_err(|error| format!("a field does not take it: {error}")),
    }
}

/// What the invocation is made from, when it is made from something, among the arguments the
/// api takes: the key [`tilo_actions::params::FROM_ARGUMENT`], as the reference a caller names
/// its source with, and the kind the store keeps it as, which names the app a caller must be
/// able to read it in.
fn made_from(
    from: Option<&ResourceRef>,
    arguments: &mut serde_json::Map<String, Json>,
    published: impl Fn(&ResourceRef) -> String,
) {
    if let Some(from) = from {
        arguments.insert(
            tilo_actions::params::FROM_ARGUMENT.to_owned(),
            tilo_actions::params::MadeFrom {
                resource: published(from),
                kind: crate::resources::kinds::kind_of_type(&from.ty),
            }
            .to_json(),
        );
    }
}

/// The arguments of the action `invocation` makes of `meta`: each of its own (past the first
/// one, when it is the resource the action is on) by its parameter's name, as the API takes it
/// ([`argument_json`]), and what it is made from. `apps` holds every package a type may name.
fn arguments_of(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    invocation: &Invocation,
    target: bool,
) -> Result<Json, String> {
    let Invocation::Action {
        action, args, from, ..
    } = invocation
    else {
        return Ok(json!({}));
    };
    let declared = meta
        .actions
        .iter()
        .find(|declared| declared.id == *action)
        .ok_or_else(|| format!("{} declares no action {action}", meta.app))?;
    let mut arguments = serde_json::Map::new();
    for (name, value) in args.iter().skip(usize::from(target)) {
        let param = declared
            .params
            .iter()
            .find(|param| param.name == *name)
            .ok_or_else(|| format!("{action} takes no argument `{name}`"))?;
        let ty = parse_ty(&param.ty, &meta.app)?;
        let owner = |other: &str| {
            apps.get(other)
                .cloned()
                .ok_or_else(|| format!("this task may use no app {other}"))
        };
        arguments.insert(name.clone(), argument_json(&owner, meta, value, &ty)?);
    }
    made_from(from.as_ref(), &mut arguments, ResourceRef::id_string);
    Ok(Json::Object(arguments))
}

/// The API's call for `invocation` of the native app `meta`: its declared verb, as the app's
/// own api names it, on the resource the program named (the action's own document, or an item of
/// it), with the arguments the app takes. A field assignment is the app's own `field-edit` of
/// its document. Either is the task's invocation of the app, decided and undone as every other
/// call of its actions: the app keeps the step.
fn native_call(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    record: Option<&str>,
    invocation: &Invocation,
) -> Result<Json, String> {
    let app = meta.app.as_str();
    match invocation {
        Invocation::Action { action, target, .. } => {
            let Some(record) = record else {
                return Err(format!(
                    "{app} publishes no document a program names its actions on"
                ));
            };
            // The resource the app's own api takes: the one the program named the action on,
            // else the document the app publishes.
            let resource = target
                .as_ref()
                .map_or_else(|| record.to_owned(), ResourceRef::id_string);
            let arguments = arguments_of(apps, meta, invocation, target.is_some())?;
            Ok(json!({
                "app": app,
                "resource": resource,
                "action": crate::catalogue::published_verb(app, action),
                "arguments": arguments,
            }))
        }
        Invocation::Edit {
            field,
            field_id,
            value,
            target,
            ..
        } => {
            let Some(record) = record else {
                return Err(format!(
                    "{app} publishes no document a program sets the fields of"
                ));
            };
            if target.item.is_some() {
                return Err(format!(
                    "a program sets {app}'s own document fields; what its items hold changes through its actions"
                ));
            }
            let key = field_id
                .split_once('.')
                .map_or(field_id.as_str(), |(_, key)| key);
            // A text field takes the smallest splice of the text it holds; a flag is set whole.
            let op = match value {
                Value::Text(text) => json!({"op": "replace", "text": text}),
                Value::Bool(flag) => json!({"op": "set", "value": flag}),
                _ => {
                    return Err(format!(
                        "a program sets text and flag fields; `{field}` changes through {app}'s actions"
                    ));
                }
            };
            let mut arguments = serde_json::Map::new();
            arguments.insert(
                tilo_resource::EDIT_PARAM.to_owned(),
                json!({"path": [{"field": key}], "op": op}),
            );
            Ok(json!({
                "app": app,
                "resource": record,
                "action": tilo_resource::FIELD_EDIT,
                "arguments": arguments,
            }))
        }
    }
}

/// The API's call for `invocation` of the package `meta`: a declared action on its target
/// (its first parameter, when it is one of the package's types) or on the package, its other
/// arguments by name; a field edit on its resource.
fn call_of(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    invocation: &Invocation,
) -> Result<Json, String> {
    match invocation {
        Invocation::Action {
            app, action, args, ..
        } => {
            let declared = meta
                .actions
                .iter()
                .find(|declared| declared.id == *action)
                .ok_or_else(|| format!("{app} declares no action {action}"))?;
            let target = meta.target_of(&declared.params);
            let target = target.and_then(|_| args.first());
            let resource = match target {
                Some((_, Value::Ref(r))) => address_of(app, &r.target),
                _ => package_resource_id(app),
            };
            let arguments = arguments_of(apps, meta, invocation, target.is_some())?;
            Ok(json!({
                "app": app,
                "resource": resource,
                // As the app's api publishes it: by its name.
                "action": declared.name,
                "arguments": arguments,
            }))
        }
        Invocation::Edit {
            app,
            target,
            field,
            value,
            ..
        } => {
            let (action, value) = match value {
                Value::Text(text) => (crate::action::EDIT_TEXT_ACTION, json!(text)),
                Value::Bool(flag) => (crate::action::EDIT_FLAG_ACTION, json!(flag)),
                _ => {
                    return Err(format!(
                        "a program sets text and flag fields; `{field}` changes through {}'s actions",
                        app.rsplit('.').next().unwrap_or(app)
                    ));
                }
            };
            // The edit verbs take a field by its name, as the surface offers it; the binding
            // refuses an id the verb does not offer, and the program named the field by name.
            Ok(json!({
                "app": app,
                "resource": address_of(app, target),
                "action": action,
                // The field as its type names it, which the API's edit takes.
                "arguments": {"field": field, "value": value},
            }))
        }
    }
}

/// The API's call for an action of the workspace's action registry (`tilo.actions`): its verb on
/// the package's resource, and the provider the dispatch answers those verbs under.
fn actions_call(meta: &Meta, invocation: &Invocation) -> Result<Json, String> {
    let mut call = call_of(&HashMap::new(), meta, invocation)?;
    if let Some(fields) = call.as_object_mut() {
        fields.insert("provider".to_owned(), json!(tilolang::ACTIONS_APP));
    }
    Ok(call)
}

/// The id `app`'s api publishes its resource `r` under: a hosted app's documents carry their
/// app already, a task's do not.
pub(crate) fn address_of(app: &str, r: &ResourceRef) -> String {
    match r.id_string() {
        id if id.starts_with(&resource_id(app, "")) => id,
        id => resource_id(app, &id),
    }
}

/// What an invocation of `meta`'s action answered (`{value, resources}`), as the program gets
/// it: a document result as the resource the app says it returned, any other as its value. A
/// value of a record type another package declares (what `actions.call` answers with the
/// workspace's invocation record) is read as that package declares its fields.
fn result_of(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    invocation: &Invocation,
    output: &Json,
) -> Result<Value, String> {
    let Invocation::Action { action, .. } = invocation else {
        return Ok(Value::Unit);
    };
    let Some(result) = meta
        .actions
        .iter()
        .find(|declared| declared.id == *action)
        .and_then(|declared| declared.result.clone())
    else {
        return Ok(Value::Unit);
    };
    if let Some(declared) = meta.type_named(&result) {
        let prefix = resource_id(&meta.app, "");
        return output["resources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            // A resource is answered by the id its app names it by (`issues/i1`), as its own
            // published id may be (`tl:so.tilo.tasks:issues/i1`).
            .find_map(|id| {
                let id = id.strip_prefix(&prefix).unwrap_or(id);
                ResourceRef::parse_id(id, declared.id.clone())
            })
            .map(|at| Value::resource(&at))
            .ok_or_else(|| {
                format!(
                    "the action ran but did not answer the {} it returns: {output}",
                    declared.name
                )
            });
    }
    let value = match output.get("value") {
        None | Some(Json::Null) => return Ok(Value::Unit),
        Some(value) => value,
    };
    // A value of a declared type is read from its JSON by what the package declares: fields by
    // name, cases by variant. A type no package here declares is read as JSON.
    match parse_ty(&result, &meta.app) {
        Ok(ty) => declared_value(apps, meta, &ty, value),
        Err(_) => from_json(value)
            .map_err(|error| format!("the action ran but its answer does not decode: {error}")),
    }
}

/// A plain JSON value of `ty`, as the package that declares `ty` reads it: its fields by name
/// (or by id), a case by its variant's name.
fn declared_value(
    apps: &HashMap<String, Meta>,
    meta: &Meta,
    ty: &tilolang_runtime::meta::Ty,
    json: &Json,
) -> Result<Value, String> {
    use tilolang_runtime::meta::Ty;
    use tilolang_runtime::value::{EnumValue, RecordInit};
    // A value a host already encoded as the runtime's own (`{record, fields}`, `{list}`): read
    // as it is rather than as plain data of the declared type.
    if json.as_object().is_some_and(|object| {
        ["record", "enum", "list", "tuple", "some", "none", "ref"]
            .iter()
            .any(|tag| object.contains_key(*tag))
    }) {
        return from_json(json);
    }
    let inner = |ty: &Ty, json: &Json| declared_value(apps, meta, ty, json);
    match ty {
        Ty::Json => Ok(Value::Json(Box::new(json.clone()))),
        Ty::Option(item) => match json {
            Json::Null => Ok(Value::none()),
            value => inner(item, value).map(Value::some),
        },
        Ty::List(item) | Ty::Set(item) | Ty::Collection(item) => match json {
            Json::Array(items) => items
                .iter()
                .map(|value| inner(item, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List),
            other => inner(item, other).map(|one| Value::List(vec![one])),
        },
        Ty::Map(item) => match json {
            Json::Object(entries) => entries
                .iter()
                .map(|(key, value)| Ok((Value::text(key), inner(item, value)?)))
                .collect::<Result<Vec<_>, String>>()
                .map(Value::Map),
            _ => from_json(json),
        },
        Ty::Tuple(items) => match json {
            Json::Array(values) => items
                .iter()
                .zip(values)
                .map(|(ty, value)| inner(ty, value))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Tuple),
            _ => from_json(json),
        },
        Ty::Record(key) => {
            let owner = if key.app == meta.app {
                meta
            } else {
                apps.get(key.app.as_str())
                    .ok_or_else(|| format!("no package {} is here", key.app))?
            };
            let declared = owner
                .types
                .iter()
                .find(|declared| declared.id.eq(&key.name) || declared.name.eq(&key.name))
                .ok_or_else(|| format!("{} declares no type {}", key.app, key.name))?;
            let entries = json
                .as_object()
                .ok_or_else(|| format!("{} is not an object", declared.name))?;
            let mut fields = BTreeMap::new();
            for field in &declared.fields {
                let Some(value) = entries.get(&field.name).or_else(|| entries.get(&field.id))
                else {
                    continue;
                };
                let ty = tilolang_runtime::meta::parse_ty(&field.ty, &owner.app)?;
                fields.insert(field.id.clone(), declared_value(apps, owner, &ty, value)?);
            }
            Ok(Value::Rec(RecordInit {
                ty: declared.id.clone(),
                fields,
            }))
        }
        Ty::Enum(key) => {
            let owner = if key.app == meta.app {
                meta
            } else {
                apps.get(key.app.as_str())
                    .ok_or_else(|| format!("no package {} is here", key.app))?
            };
            let declared = owner
                .enums
                .iter()
                .find(|declared| declared.id.eq(&key.name) || declared.name.eq(&key.name))
                .ok_or_else(|| format!("{} declares no enum {}", key.app, key.name))?;
            let case = json
                .as_str()
                .ok_or_else(|| format!("{} is not a case name", declared.name))?;
            // The value carries the case's id, the one the code comparing it with `.{case}`
            // built: the name a reading writes is only how the workspace spells it.
            let variant = declared
                .variants
                .iter()
                .find(|variant| variant.name == case || variant.id == case)
                .ok_or_else(|| format!("`{case}` is not a case of {}", declared.name))?;
            Ok(Value::Enum(EnumValue {
                ty: declared.id.clone(),
                variant: variant.id.clone(),
                fields: BTreeMap::new(),
            }))
        }
        _ => from_json(json),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tilolang_runtime::meta::EffectClass;

    fn callees() -> Callees {
        let actions = tilolang::actions_meta();
        let invocations =
            crate::platform_apps::platform_package("tilo.invocations").expect("its surface reads");
        Callees {
            apps: HashMap::from([
                (actions.app.clone(), actions),
                (invocations.app.clone(), invocations),
            ]),
            ..Callees::default()
        }
    }

    fn action(name: &str, args: Vec<(&str, Value)>) -> Invocation {
        Invocation::Action {
            app: tilolang::ACTIONS_APP.into(),
            action: name.to_owned(),
            name: name.to_owned(),
            args: args
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
            class: EffectClass::Read,
            destructive: false,
            author: None,
            target: None,
            from: None,
            asks: false,
            shows: Vec::new(),
        }
    }

    /// The registry's verbs are called at the dispatch's own provider, on the package's
    /// resource, with the arguments by name.
    #[test]
    fn the_registry_is_called_at_its_own_provider() {
        let call = callees()
            .call(&action("catalogue", vec![("query", Value::none())]))
            .expect("the catalogue is a call");
        assert_eq!(call["provider"], "tilo.actions");
        assert_eq!(call["resource"], "tl:tilo.actions");
        assert_eq!(call["action"], "catalogue");
    }

    /// What the dispatch answers a call with is the invocation record, read as the package
    /// declares it: its status a case by the id the code comparing it with `.{case}` builds,
    /// its output any value.
    #[test]
    fn a_call_answers_the_invocation_record_the_workspace_keeps() {
        let invocation = action(
            "call",
            vec![
                ("address", Value::text("so.tilo.notes/notes.note:1#write")),
                ("arguments", Value::Json(Box::default())),
            ],
        );
        let answered = callees().answered(
            &invocation,
            Answered::Done(json!({
                "value": {
                    "id": "inv-1",
                    "status": "waiting",
                    "action": "notes.note:1#write",
                    "started_ms": 1,
                    "output": {"titles": ["Take the ferry"]},
                },
                "resources": [],
            })),
        );
        let Invoked::Done(Value::Rec(record)) = answered else {
            panic!("the record is a call's answer: {answered:?}");
        };
        assert_eq!(record.ty, "tilo.invocations#Invocation");
        let Some(Value::Enum(status)) = record.fields.get("Invocation.status") else {
            panic!("the status is the record's: {:?}", record.fields);
        };
        assert_eq!(status.variant, "Status.waiting");
        assert!(
            matches!(
                record.fields.get("Invocation.output"),
                Some(Value::Opt(Some(_)))
            ),
            "the output is the action's answer: {:?}",
            record.fields
        );
        // A call the dispatch answered ended reads as the case `answer.status == .ended`
        // builds, so the comparison matches.
        let ended = callees().answered(
            &invocation,
            Answered::Done(json!({
                "value": {
                    "id": "inv-2",
                    "status": "ended",
                    "action": "notes.note:1#write",
                    "started_ms": 1,
                    "ended_ms": 2,
                },
                "resources": [],
            })),
        );
        let Invoked::Done(Value::Rec(record)) = ended else {
            panic!("the record is a call's answer: {ended:?}");
        };
        assert!(
            matches!(
                record.fields.get("Invocation.status"),
                Some(Value::Enum(status)) if status.variant == "Status.ended"
            ),
            "the ended status is the case the code names: {:?}",
            record.fields
        );
    }
}

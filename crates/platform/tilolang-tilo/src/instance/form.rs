//! An action waiting for its arguments: its form (forms-and-diagrams.md §1).
//!
//! A control, a menu item or a shortcut that runs an action whose parameters are neither given
//! nor defaulted opens the action's form instead of running it. The form's fields come from the
//! parameters' types (the one schema's [`FieldType`]): text a field, a flag a switch, a case of
//! an enum a choice, a day or a moment a field that reads natural text ("tomorrow 9"), a person
//! a choice of the workspace's members, a resource a choice of those of its type. Defaults
//! start filled in; what the caller gave is not shown.
//!
//! What the person types is their lens state in the window the form opened from, so closing the
//! form and opening it again keeps it. Each value is read as the action takes it (the door's own
//! reading, [`crate::host::Host::argument`]), and submit stays disabled while one does not
//! read. Submit is one invocation, run as the control would have run it, with undo and history.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilo_actions::surface::{FieldType, TypeSet};
use tilo_render_protocol::patch::{self as ui, Lane};

use crate::host::Host;
use crate::runtime::meta::{ActionMeta, Meta};
use crate::runtime::{ActionRef, NameHost, Value, ViewInstance};

/// The key prefix of a form's nodes in the window: apart from the views' keys.
pub const KEY: &str = "form~";

/// A form open in the window.
pub struct Pending {
    pub id: u64,
    /// The view whose control ran the action, and the control.
    pub lane: Lane,
    pub from: String,
    pub view: Rc<RefCell<ViewInstance>>,
    /// The action, with the arguments the control gave.
    pub aref: ActionRef,
    pub meta: Meta,
    pub action: ActionMeta,
    /// The fields: the parameters after those given.
    pub fields: Vec<Field>,
    /// Why the action refused the last submit.
    pub error: Option<String>,
}

/// A field of a form: one parameter of the action.
pub struct Field {
    /// The parameter's place among the action's.
    pub at: usize,
    pub name: String,
    pub ty: String,
    pub field_type: FieldType,
    /// Its control's key in the form's tree.
    pub key: String,
}

impl Pending {
    /// The form of `aref`, an action of `meta` waiting for its arguments, which the control
    /// `from` of the view of `lane` ran.
    #[must_use]
    pub fn new(
        id: u64,
        (lane, from): (Lane, String),
        view: Rc<RefCell<ViewInstance>>,
        aref: ActionRef,
        meta: Meta,
    ) -> Option<Self> {
        let action = meta.actions.iter().find(|a| a.id == aref.action)?.clone();
        let asked = action.params.get(aref.args.len()..)?;
        if !asked.iter().all(|param| {
            param.default.is_some() || crate::runtime::platform::form_asks(&meta, &param.ty)
        }) {
            return None;
        }
        let mut types = TypeSet::default();
        let fields = action
            .params
            .iter()
            .enumerate()
            .skip(aref.args.len())
            .map(|(at, param)| Field {
                at,
                name: param.name.clone(),
                ty: param.ty.clone(),
                field_type: crate::surface::field_type_of_type(&meta, &param.ty, &mut types),
                key: format!("{KEY}{}", param.name),
            })
            .collect();
        Some(Self {
            id,
            lane,
            from,
            view,
            aref,
            meta,
            action,
            fields,
            error: None,
        })
    }

    /// The lens state what the person typed is kept under: one per action and what the control
    /// gave it, so the form of `share(n)` for another note starts afresh.
    fn state(&self) -> String {
        let given: Vec<Json> = self
            .aref
            .args
            .iter()
            .map(|arg| match arg {
                Value::Ref(r) => Json::String(r.target.id_string()),
                other => crate::runtime::value::to_json(other).unwrap_or(Json::Null),
            })
            .collect();
        format!("$form:{}:{}", self.aref.action, Json::Array(given))
    }

    /// What the person typed into each field so far, by parameter name.
    fn typed(&self) -> BTreeMap<String, String> {
        match self.view.borrow().lens(&self.state()) {
            Some(Value::Map(entries)) => entries
                .into_iter()
                .filter_map(|(name, text)| match (name, text) {
                    (Value::Text(name), Value::Text(text)) => Some((name, text)),
                    _ => None,
                })
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    /// The field `key` now holds `text`: kept as the person's lens state.
    pub fn edit(&mut self, key: &str, text: String) {
        let Some(field) = self.fields.iter().find(|field| field.key == key) else {
            return;
        };
        let mut typed = self.typed();
        typed.insert(field.name.clone(), text);
        let value = Value::Map(
            typed
                .into_iter()
                .map(|(name, text)| (Value::Text(name), Value::Text(text)))
                .collect(),
        );
        self.view.borrow().set_lens(&self.state(), value);
        self.error = None;
    }

    /// The person submitted: forget what they typed.
    pub fn forget(&self) {
        self.view
            .borrow()
            .set_lens(&self.state(), Value::Map(Vec::new()));
    }

    /// Each field's text now: what the person typed, else its default as text, else nothing.
    fn texts(&self, host: &Host) -> Vec<String> {
        let typed = self.typed();
        let mut before: Vec<Value> = self.aref.args.clone();
        let mut out = Vec::with_capacity(self.fields.len());
        for field in &self.fields {
            let text = typed.get(&field.name).cloned().unwrap_or_else(|| {
                if before.len() != field.at {
                    return String::new();
                }
                host.runtime()
                    .default_argument(&self.aref.action, field.at, &before)
                    .ok()
                    .flatten()
                    .map(|value| text_of(&self.meta, &value))
                    .unwrap_or_default()
            });
            match read(host, &self.meta, field, &text) {
                Ok(value) if before.len() == field.at => before.push(value),
                _ => {}
            }
            out.push(text);
        }
        out
    }

    /// The action with every argument read from the form, or why a field does not read, by
    /// its key.
    ///
    /// # Errors
    /// The first field whose text does not read as its type, with why.
    pub fn completed(&self, host: &Host) -> Result<ActionRef, (String, String)> {
        let mut args = self.aref.args.clone();
        for (field, text) in self.fields.iter().zip(self.texts(host)) {
            let value =
                read(host, &self.meta, field, &text).map_err(|why| (field.key.clone(), why))?;
            args.push(value);
        }
        Ok(ActionRef {
            args,
            ..self.aref.clone()
        })
    }

    /// The form as the window shows it now.
    #[must_use]
    pub fn shown(&self, host: &Host, names: &dyn NameHost) -> ui::Form {
        let texts = self.texts(host);
        let mut ready = true;
        let mut short = true;
        let mut children = Vec::with_capacity(self.fields.len());
        for (field, text) in self.fields.iter().zip(&texts) {
            let read = read(host, &self.meta, field, text);
            ready &= read.is_ok();
            short &= !matches!(inner(&field.field_type), FieldType::RichText);
            children.push(self.field_node(host, names, field, text, &read));
        }
        let title = crate::runtime::platform::label_of(&self.action);
        let title = if title.ends_with('…') {
            title
        } else {
            format!("{title}…")
        };
        ui::Form {
            id: self.id,
            submit: title.trim_end_matches('…').to_owned(),
            title,
            destructive: self.action.role.as_deref() == Some("destructive"),
            ready,
            error: self.error.clone(),
            from: Some((self.lane, self.from.clone())),
            fields: self
                .fields
                .iter()
                .zip(texts.iter())
                .map(|(field, text)| (field.key.clone(), text.clone()))
                .collect(),
            short,
            tree: node("form", KEY, BTreeMap::new(), None, children),
        }
    }

    /// The field `field` showing `text`: its label and control, and under it why it does not
    /// read, or for a day or a moment, what it reads as.
    fn field_node(
        &self,
        host: &Host,
        names: &dyn NameHost,
        field: &Field,
        text: &str,
        read: &Result<Value, String>,
    ) -> ui::Node {
        let label = crate::runtime::platform::sentence(&field.name);
        let optional = matches!(field.field_type, FieldType::Option(_));
        let control = match inner(&field.field_type) {
            FieldType::Bool => {
                // A switch carries its own label.
                let mut props = BTreeMap::new();
                props.insert("label".to_owned(), ui::Value::text(&label));
                return node("toggle", &field.key, props, None, Vec::new());
            }
            FieldType::Named(_) | FieldType::Principal | FieldType::Resource(_) => {
                self.choice_node(host, names, field, text, optional)
            }
            other => {
                let mut props = BTreeMap::new();
                let placeholder = match other {
                    FieldType::Date => Some("tomorrow"),
                    FieldType::Time => Some("tomorrow 9"),
                    _ => None,
                };
                if let Some(placeholder) = placeholder {
                    props.insert(
                        "placeholder".to_owned(),
                        ui::Value::some(ui::Value::text(placeholder)),
                    );
                }
                if matches!(other, FieldType::RichText) {
                    props.insert("lines".to_owned(), ui::Value::some(ui::Value::Int(4)));
                }
                node(
                    "ui.TextInput",
                    &field.key,
                    props,
                    Some(ui::Binding {
                        field: None,
                        name: label.clone(),
                    }),
                    Vec::new(),
                )
            }
        };
        let note = match (read, inner(&field.field_type)) {
            (Err(why), _) if !text.is_empty() => Some(why.clone()),
            (Ok(Value::Text(day)), FieldType::Date | FieldType::Time) if !text.is_empty() => {
                Some(day.clone())
            }
            _ => None,
        };
        let mut parts = vec![control];
        if let Some(note) = note {
            let mut props = BTreeMap::new();
            props.insert("value".to_owned(), ui::Value::Text(note));
            props.insert("role".to_owned(), ui::Value::Enum("caption".to_owned()));
            props.insert(
                "value:as".to_owned(),
                ui::Value::text(as_of(&field.field_type)),
            );
            parts.push(node(
                "ui.Text",
                &format!("{}~note", field.key),
                props,
                None,
                Vec::new(),
            ));
        }
        let mut props = BTreeMap::new();
        props.insert("label".to_owned(), ui::Value::text(&label));
        node(
            "field",
            &format!("{}~field", field.key),
            props,
            None,
            vec![node(
                "stack",
                &format!("{}~parts", field.key),
                BTreeMap::new(),
                None,
                parts,
            )],
        )
    }

    /// The choice `field` shows `text` in: an enum's cases, the workspace's members, or the
    /// resources of its type.
    fn choice_node(
        &self,
        host: &Host,
        names: &dyn NameHost,
        field: &Field,
        text: &str,
        optional: bool,
    ) -> ui::Node {
        let cases = self.cases(host, names, field, optional);
        let mut props = BTreeMap::new();
        props.insert(
            "cases".to_owned(),
            ui::Value::List(
                cases
                    .into_iter()
                    .map(|(name, label)| {
                        ui::Value::List(vec![
                            ui::Value::Text(name),
                            ui::Value::Text(label),
                            ui::Value::Opt(None),
                        ])
                    })
                    .collect(),
            ),
        );
        props.insert("selected".to_owned(), ui::Value::text(text));
        let placeholder = match inner(&field.field_type) {
            FieldType::Principal => "Choose someone",
            FieldType::Resource(_) => "Choose one",
            _ => "Choose",
        };
        props.insert("placeholder".to_owned(), ui::Value::text(placeholder));
        if !matches!(inner(&field.field_type), FieldType::Named(_)) {
            // People and resources are picked from a menu, whatever their number.
            props.insert("menu".to_owned(), ui::Value::Bool(true));
        }
        node("choice", &field.key, props, None, Vec::new())
    }

    /// What the choice of `field` offers: an enum's cases, the workspace's members, or the
    /// resources of its type, each by what reads back as the argument and what it says.
    fn cases(
        &self,
        host: &Host,
        names: &dyn NameHost,
        field: &Field,
        optional: bool,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if optional {
            out.push((String::new(), "None".to_owned()));
        }
        match inner(&field.field_type) {
            FieldType::Named(_) => {
                let name = field.ty.trim().trim_start_matches("Option<");
                let name = name.trim_end_matches('>');
                let name = name.rsplit('.').next().unwrap_or(name);
                if let Some(declared) = self.meta.enums.iter().find(|e| e.name == name) {
                    out.extend(declared.variants.iter().map(|case| {
                        let label = if case.label.is_empty() {
                            crate::runtime::platform::sentence(&case.name)
                        } else {
                            case.label.clone()
                        };
                        (case.name.clone(), label)
                    }));
                }
            }
            FieldType::Principal => {
                // The workspace's members; the person at the window is one, also while the
                // host has not told the runtime its membership.
                let viewer = self.view.borrow().viewer().clone();
                let mut members: Vec<_> = host
                    .runtime()
                    .members()
                    .into_iter()
                    .map(|member| member.who)
                    .collect();
                if !members.contains(&viewer) {
                    members.insert(0, viewer);
                }
                out.extend(members.into_iter().map(|who| {
                    let label = names.name(&who);
                    (who.0, label)
                }));
            }
            FieldType::Resource(kind) => {
                // `tl.<app>.<Type>`: every resource of the type, as search finds it.
                let (app, ty) = kind
                    .strip_prefix("tl.")
                    .and_then(|rest| rest.rsplit_once('.'))
                    .unwrap_or_default();
                let prefix = crate::publish::resource_id(app, "");
                out.extend(
                    crate::search::every_hit(host)
                        .into_iter()
                        .filter(|hit| hit.type_name == ty && hit.resource.starts_with(&prefix))
                        .map(|hit| (hit.resource, hit.title)),
                );
            }
            _ => {}
        }
        out
    }
}

/// The type a field's control is for: the value of an option.
fn inner(ty: &FieldType) -> &FieldType {
    match ty {
        FieldType::Option(held) => held,
        other => other,
    }
}

/// How the window reads a field's note: a day or a moment the person's way.
fn as_of(ty: &FieldType) -> &'static str {
    match inner(ty) {
        FieldType::Date => "date",
        FieldType::Time => "time",
        _ => "text",
    }
}

fn node(
    kind: &str,
    key: &str,
    props: BTreeMap<String, ui::Value>,
    binding: Option<ui::Binding>,
    children: Vec<ui::Node>,
) -> ui::Node {
    ui::Node {
        kind: kind.to_owned(),
        key: key.to_owned(),
        props,
        binding,
        events: BTreeMap::new(),
        anchor: None,
        children,
    }
}

/// The text a field shows for `value`, the way the person would type it.
fn text_of(meta: &Meta, value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Opt(Some(held)) => text_of(meta, held),
        Value::Principal(who) => who.0.clone(),
        Value::Ref(r) => crate::publish::resource_id(&meta.app, &r.target.id_string()),
        Value::Enum(case) => meta
            .enums
            .iter()
            .flat_map(|e| &e.variants)
            .find(|variant| variant.id == case.variant)
            .map(|variant| variant.name.clone())
            .unwrap_or_default(),
        Value::Rich(rich) => rich.written_text(),
        _ => String::new(),
    }
}

/// `text` read as the argument of `field`, as the action takes it, or why it does not read.
fn read(host: &Host, meta: &Meta, field: &Field, text: &str) -> Result<Value, String> {
    let optional = matches!(field.field_type, FieldType::Option(_));
    let trimmed = text.trim();
    if trimmed.is_empty() {
        if optional {
            return Ok(Value::none());
        }
        return match inner(&field.field_type) {
            FieldType::Text | FieldType::RichText => host.argument(meta, &json!(""), &field.ty),
            FieldType::Bool => Ok(Value::Bool(false)),
            _ => Err("Required".to_owned()),
        };
    }
    let json = match inner(&field.field_type) {
        FieldType::Text | FieldType::RichText => Json::String(text.to_owned()),
        FieldType::Bool => Json::Bool(trimmed == "true"),
        FieldType::Int => trimmed
            .parse::<i64>()
            .map(Json::from)
            .map_err(|_| "Not a whole number".to_owned())?,
        FieldType::Float => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Json::Number)
            .ok_or_else(|| "Not a number".to_owned())?,
        FieldType::Date => {
            let said = host.runtime().dated(trimmed);
            match said.day {
                Some(day) if said.text.trim().is_empty() => Json::String(day),
                _ if tilolang::calendar::calendar_span(trimmed).is_some() => {
                    Json::String(trimmed.to_owned())
                }
                _ => return Err("Not a day".to_owned()),
            }
        }
        FieldType::Time => {
            let said = host.runtime().dated(trimmed);
            match said.at {
                Some(at) if said.text.trim().is_empty() => Json::String(at),
                _ if tilolang::calendar::instant(trimmed).is_some() => {
                    Json::String(trimmed.to_owned())
                }
                _ => return Err("Not a time".to_owned()),
            }
        }
        FieldType::Principal => match trimmed.strip_prefix("agent:") {
            Some(agent) => json!({ "agent": agent }),
            None => json!({ "person": trimmed }),
        },
        _ => Json::String(trimmed.to_owned()),
    };
    host.argument(meta, &json, &field.ty)
}

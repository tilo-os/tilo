//! An action waiting for its arguments: its form (forms-and-diagrams.md §1).
//!
//! A control, a menu item or a shortcut that runs an action whose parameters are neither given
//! nor defaulted opens the action's form instead of running it. The form's fields come from the
//! parameters' types (the one schema's [`FieldType`]): text a field, a flag a switch, a case of
//! an enum a choice, a day or a moment a field that reads natural text ("tomorrow 9"), a person
//! a choice of the workspace's members, a resource a choice of those of its type. Defaults
//! start filled in; what the caller gave is not shown.
//!
//! What the person types is their view state in the window the form opened from, so closing the
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
    /// Where the form stands: the view and control that ran the action, when one did. A form
    /// the palette ran stands on its own.
    pub from: Option<(Lane, String)>,
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
        from: Option<(Lane, String)>,
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
            from,
            view,
            aref,
            meta,
            action,
            fields,
            error: None,
        })
    }

    /// The view state what the person typed is kept under: one per action and what the control
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
        match self.view.borrow().view_state(&self.state()) {
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

    /// The field `key` now holds `text`: kept as the person's view state.
    pub fn edit(&mut self, key: &str, text: String) {
        // A field's control may be several parts (a day, a time of day, quick choices, free
        // text), each its own key after the field's: what the person did is the field's.
        let name = key.strip_prefix(KEY).unwrap_or(key);
        let name = name.split('~').next().unwrap_or(name);
        let Some(field) = self.fields.iter().find(|field| field.name == name) else {
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
        self.view.borrow().set_view_state(&self.state(), value);
        self.error = None;
    }

    /// The person submitted: forget what they typed.
    pub fn forget(&self) {
        self.view
            .borrow()
            .set_view_state(&self.state(), Value::Map(Vec::new()));
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
                    .map(|value| text_of(&self.meta, &field.field_type, &value))
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
        // A form of one field states what it asks in its title: the field needs no label of
        // its own beside it.
        let labelled = self.fields.len() > 1;
        let mut children = Vec::with_capacity(self.fields.len());
        for (field, text) in self.fields.iter().zip(&texts) {
            let read = read(host, &self.meta, field, text);
            ready &= read.is_ok();
            short &= !matches!(inner(&field.field_type), FieldType::RichText);
            // A form of a field or two stands beside the control that ran it; a day, a moment
            // or a length of time asks in more than a line.
            short &= !matches!(
                inner(&field.field_type),
                FieldType::Date | FieldType::Time | FieldType::Duration
            );
            children.push(self.field_node(host, names, field, text, &read, labelled));
        }
        let title = crate::runtime::platform::label_of(&self.action);
        let title = if title.ends_with('…') {
            title
        } else {
            format!("{title}…")
        };
        let answers = self.quick_choice();
        // A form whose moments answer it on one pick needs no confirm button until the person
        // is on the way in by hand: the moment they type or pick is the one to confirm.
        let confirm = answers.is_empty() || texts.iter().any(|text| !text.trim().is_empty());
        ui::Form {
            id: self.id,
            submit: title.trim_end_matches('…').to_owned(),
            title,
            destructive: self.action.role.as_deref() == Some("destructive"),
            ready,
            confirm,
            error: self.error.clone(),
            from: self.from.clone(),
            fields: self
                .fields
                .iter()
                .zip(texts.iter())
                .flat_map(|(field, text)| field_parts(field, text))
                .collect(),
            short,
            answers,
            tree: node("form", KEY, BTreeMap::new(), None, children),
        }
    }

    /// The control whose choice answers the form at once: the quick choices of a form's one
    /// field, so a person who picks "Tomorrow morning" is reminded without a second step.
    fn quick_choice(&self) -> Vec<String> {
        let [field] = self.fields.as_slice() else {
            return Vec::new();
        };
        matches!(
            inner(&field.field_type),
            FieldType::Time | FieldType::Duration
        )
        .then(|| format!("{}~quick", field.key))
        .into_iter()
        .collect()
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
        labelled: bool,
    ) -> ui::Node {
        let label = crate::runtime::platform::sentence(&field.name);
        let optional = matches!(field.field_type, FieldType::Option(_));
        // The parts of the field's control, in the order a person meets them: the quick
        // choices a type has, the control that picks a day or a moment, the field they type in.
        let mut parts: Vec<ui::Node> = Vec::new();
        match inner(&field.field_type) {
            FieldType::Bool => {
                // A switch carries its own label.
                let mut props = BTreeMap::new();
                props.insert("label".to_owned(), ui::Value::text(&label));
                return node("toggle", &field.key, props, None, Vec::new());
            }
            FieldType::Named(_) | FieldType::Principal | FieldType::Resource(_) => {
                parts.push(self.choice_node(host, names, field, text, optional));
            }
            FieldType::Date => {
                parts.push(moment_row(field, text, optional, false, "Pick a date…"));
            }
            FieldType::Time => {
                // The moments a person names, and one way into the rest: the entry they type
                // a moment into, with the calendar and the hour at its end.
                parts.push(Self::quick_node(
                    field,
                    text,
                    moments(host),
                    optional,
                    Some("time"),
                ));
                parts.push(moment_row(
                    field,
                    text,
                    optional,
                    true,
                    "Pick a date and time…",
                ));
            }
            FieldType::Duration => {
                parts.push(Self::quick_node(field, text, lengths(), optional, None));
                parts.push(text_node(field, "~text", Some("1h 30m"), &label, false));
            }
            other => {
                let placeholder = match other {
                    FieldType::Identifier => Some("jane@example.com"),
                    _ => None,
                };
                let multiline = matches!(other, FieldType::RichText);
                parts.push(text_node(field, "", placeholder, &label, multiline));
            }
        }
        // What the field reads as, or why it does not: a day, a moment or a length of time
        // reads in the person's own words under the control.
        let note = match (read, inner(&field.field_type)) {
            (Err(why), _) if !text.is_empty() => Some(ui::Value::Text(why.clone())),
            (Ok(Value::Text(said)), FieldType::Date | FieldType::Time) if !text.is_empty() => {
                Some(ui::Value::Text(said.clone()))
            }
            (Ok(Value::Int(secs)), FieldType::Duration) if !text.is_empty() => {
                Some(ui::Value::Int(*secs))
            }
            _ => None,
        };
        if let Some(note) = note {
            let mut props = BTreeMap::new();
            props.insert("value".to_owned(), note);
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
        if labelled {
            props.insert("label".to_owned(), ui::Value::text(&label));
        }
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

    /// The quick choices of `field` showing `text`: the few values the type is often given,
    /// one under the next, and none of them when the field may be empty.
    fn quick_node(
        field: &Field,
        text: &str,
        cases: Vec<(String, String)>,
        optional: bool,
        as_kind: Option<&str>,
    ) -> ui::Node {
        let mut all: Vec<(String, String)> = Vec::new();
        if optional {
            all.push((String::new(), "None".to_owned()));
        }
        all.extend(cases);
        let mut props = BTreeMap::new();
        props.insert("list".to_owned(), ui::Value::Bool(true));
        // What each case's value is, so the window tells it at the choice's trailing edge: a
        // moment, in the person's own format ("Tomorrow morning … Sat 09:00").
        if let Some(kind) = as_kind {
            props.insert("case:as".to_owned(), ui::Value::text(kind));
        }
        props.insert(
            "cases".to_owned(),
            ui::Value::List(
                all.into_iter()
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
        node(
            "choice",
            &format!("{}~quick", field.key),
            props,
            None,
            Vec::new(),
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

/// The lengths of time a field is often given, as the segments offer them.
fn lengths() -> Vec<(String, String)> {
    [("15m", "15 minutes"), ("1h", "1 hour"), ("1d", "1 day")]
        .into_iter()
        .map(|(said, label)| (said.to_owned(), label.to_owned()))
        .collect()
}

/// The moments a form offers a moment's field, resolved against the person's clock and zone:
/// the value is the instant, so what the field reads after a choice is the moment itself.
fn moments(host: &Host) -> Vec<(String, String)> {
    [
        ("later today", "Later today"),
        ("this evening", "This evening"),
        ("tomorrow morning", "Tomorrow morning"),
        ("next week 9am", "Next week"),
    ]
    .into_iter()
    .map(|(said, label)| (host.runtime().dated(said).at, label))
    .filter_map(|(at, label)| Some((at?, label.to_owned())))
    .collect()
}

/// The parts of `field`'s control as the window keeps them: each key the app sent the field's
/// text for, so a day, a moment's time of day and the field typed in all show it.
fn field_parts(field: &Field, text: &str) -> Vec<(String, String)> {
    let mut parts = vec![(field.key.clone(), text.to_owned())];
    match inner(&field.field_type) {
        FieldType::Time => {
            parts.push((format!("{}~time", field.key), text.to_owned()));
            parts.push((format!("{}~text", field.key), text.to_owned()));
        }
        FieldType::Date | FieldType::Duration => {
            parts.push((format!("{}~text", field.key), text.to_owned()));
        }
        _ => {}
    }
    parts
}

/// One way into the day or moment a field holds: an entry the person types it into ("tomorrow
/// 9:30"), with the calendar at its end opening the day and, for a moment, the time of day. The
/// entry leads, so the form's first key goes to it.
fn moment_row(
    field: &Field,
    text: &str,
    optional: bool,
    time: bool,
    placeholder: &str,
) -> ui::Node {
    let label = crate::runtime::platform::sentence(&field.name);
    let mut entry = text_node(field, "~text", Some(placeholder), &label, false);
    entry.props.insert("fill".to_owned(), ui::Value::Int(1));
    node(
        "row",
        &format!("{}~pick", field.key),
        BTreeMap::new(),
        None,
        vec![entry, day_node(field, text, optional, time)],
    )
}

/// The field `field` picking a day (a moment, when `time`) as the toolkit's own date field,
/// with the time of day it is due at in its popover when the field is a moment.
fn day_node(field: &Field, text: &str, optional: bool, time: bool) -> ui::Node {
    let mut props = BTreeMap::new();
    props.insert("at".to_owned(), ui::Value::text(text));
    // A calendar alone, beside the entry that tells the day: the entry says what the person
    // picked or typed, and the calendar opens the rest.
    props.insert("compact".to_owned(), ui::Value::Bool(true));
    if optional {
        props.insert("optional".to_owned(), ui::Value::Bool(true));
    }
    let children = if time {
        let mut slot = BTreeMap::new();
        slot.insert("name".to_owned(), ui::Value::text("time"));
        let clock = node(
            "ui.TimeField",
            &format!("{}~time", field.key),
            BTreeMap::new(),
            Some(ui::Binding {
                field: None,
                kind: None,
                name: "value".to_owned(),
            }),
            Vec::new(),
        );
        vec![node(
            "slot",
            &format!("{}~time-slot", field.key),
            slot,
            None,
            vec![clock],
        )]
    } else {
        Vec::new()
    };
    node(
        "ui.DateField",
        &field.key,
        props,
        Some(ui::Binding {
            field: None,
            kind: None,
            name: "value".to_owned(),
        }),
        children,
    )
}

/// The field `field` typed in: `suffix` tells it from the rest of the field's control.
fn text_node(
    field: &Field,
    suffix: &str,
    placeholder: Option<&str>,
    label: &str,
    multiline: bool,
) -> ui::Node {
    let mut props = BTreeMap::new();
    if let Some(placeholder) = placeholder {
        props.insert(
            "placeholder".to_owned(),
            ui::Value::some(ui::Value::text(placeholder)),
        );
    }
    let lines = if multiline { 4 } else { 0 };
    if multiline {
        props.insert("lines".to_owned(), ui::Value::some(ui::Value::Int(lines)));
    }
    node(
        "ui.TextInput",
        &format!("{}{suffix}", field.key),
        props,
        Some(ui::Binding {
            field: None,
            kind: None,
            name: label.to_owned(),
        }),
        Vec::new(),
    )
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
        FieldType::Duration => "duration",
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
fn text_of(meta: &Meta, ty: &FieldType, value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        // A length of time shows as the person would write it (`1h 30m`), not as its seconds.
        Value::Int(number) if matches!(inner(ty), FieldType::Duration) => {
            tilolang::duration::text(*number)
        }
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Opt(Some(held)) => text_of(meta, ty, held),
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
            // What the person typed says when and nothing else ("tomorrow", "Mon 9:30"): the
            // words it leaves are the words it was given.
            match said.day {
                Some(day) if said.text.trim() == trimmed => Json::String(day),
                _ if tilolang::calendar::calendar_span(trimmed).is_some() => {
                    Json::String(trimmed.to_owned())
                }
                _ => return Err("Not a day".to_owned()),
            }
        }
        FieldType::Time => {
            // A moment the person named ("tomorrow 9:30"), a day picked from the calendar
            // (its morning), or the instant a choice or the picker holds.
            let said = host.runtime().dated(trimmed);
            if said.text.trim() == trimmed
                && let Some(at) = said.at
            {
                Json::String(at)
            } else if tilolang::calendar::instant(trimmed).is_some() {
                Json::String(trimmed.to_owned())
            } else {
                // A day alone ("next week") is due at the morning of it.
                let morning = format!("{trimmed} 9am");
                let said = host.runtime().dated(&morning);
                match said.at {
                    Some(at) if said.text.trim() == morning => Json::String(at),
                    _ => return Err("Not a time".to_owned()),
                }
            }
        }
        FieldType::Duration => match tilolang::duration::secs(trimmed) {
            Some(secs) => Json::from(secs),
            None => return Err("Not a length of time".to_owned()),
        },
        FieldType::Principal => match trimmed.strip_prefix("agent:") {
            Some(agent) => json!({ "agent": agent }),
            None => json!({ "person": trimmed }),
        },
        _ => Json::String(trimmed.to_owned()),
    };
    host.argument(meta, &json, &field.ty)
}

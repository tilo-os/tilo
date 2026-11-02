//! The readings of other apps' resources the shell's places show, and what the views composed
//! over them change of those resources: the session publishes each place's record's reading
//! ([`SessionLink::follow_places`]), the shell's runtime reads it ([`SessionOrigin::poll_readings`]),
//! and an edit or an action in a composed view goes back as an ask to the record's door
//! ([`PlaceInvoker`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use tilolang_tilo::origin::OriginCx;
use tilolang_tilo::runtime::{ResourceRef, Value};

use super::{ArrangementReading, Ask, SessionLink, SessionOrigin};

impl SessionLink {
    /// The session published `reading` of the resource `id`, of the type `ty` (`<app>#<Type>`):
    /// the shell's runtime reads it on its next frame, and the views composed over it run
    /// again. Whether it differs from the last.
    pub fn publish_reading(&self, id: &str, ty: &str, reading: serde_json::Value) -> bool {
        let fresh = (ty.to_owned(), reading);
        if self.readings.borrow().get(id) == Some(&fresh) {
            return false;
        }
        self.readings.borrow_mut().insert(id.to_owned(), fresh);
        self.readings_fresh.set(true);
        true
    }

    /// Publish the reading of every resource a place of `arrangement` names, from what the
    /// session publishes: `resources`, its catalogue, says each one's kind and `read` its
    /// reading. A place naming nothing the catalogue holds as a tilolang record keeps its
    /// well. Whether any reading changed.
    pub fn follow_places(
        &self,
        arrangement: &ArrangementReading,
        resources: &[ui::tilo_actions::ResourceSpec],
        read: impl Fn(&str) -> Option<serde_json::Value>,
    ) -> bool {
        let mut changed = false;
        for place in &arrangement.places {
            if place.record.is_empty() {
                continue;
            }
            let Some(ty) = find_resource(resources, &place.record)
                .and_then(|spec| tilolang_tilo::records_service::reading_type(&spec.kind))
            else {
                continue;
            };
            if let Some(reading) = read(&place.record) {
                changed |= self.publish_reading(&place.record, &ty, reading);
            }
        }
        changed
    }
}

/// The resource `id` in `resources` or among their children.
fn find_resource<'a>(
    resources: &'a [ui::tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a ui::tilo_actions::ResourceSpec> {
    resources.iter().find_map(|spec| {
        if spec.id.as_str() == id {
            Some(spec)
        } else {
            find_resource(&spec.children, id)
        }
    })
}

impl SessionOrigin {
    /// Hand the runtime the readings the session published since the last poll: the views
    /// composed over those resources run again. Whether any reading was new.
    pub(super) fn poll_readings(&self, cx: &OriginCx<'_>) -> bool {
        if !self.link.readings_fresh.replace(false) {
            return false;
        }
        let mut moved = false;
        for (id, (ty, reading)) in self.link.readings.borrow().iter() {
            let Some(target) = ResourceRef::parse_id(id, ty.clone()) else {
                tracing::error!(id, "a place names no resource id");
                continue;
            };
            match cx.host.runtime().set_reading(&target, &reading.to_string()) {
                Ok(changed) => moved |= changed,
                Err(error) => tracing::error!(id, %error, "a place's reading was refused"),
            }
        }
        moved
    }
}

/// What a view composed in a place changes of the resource it shows, sent to that resource's
/// door through the session: a press on one of its app's actions, or an edit of a field it
/// binds. The change comes back as the resource's next reading.
pub(super) struct PlaceInvoker {
    pub(super) link: Rc<SessionLink>,
    /// The resource each of the session's loose actions is published on, by the action's id
    /// (`session.show_surface(...)` goes to the desk).
    pub(super) session: BTreeMap<String, String>,
    /// The session's loose actions as its description declares them, by id: a parameter
    /// published as a field of another is sent nested under it.
    pub(super) verbs: BTreeMap<String, ui::tilo_actions::surface::VerbSurface>,
}

impl tilolang_tilo::runtime::delegate::Invoker for PlaceInvoker {
    fn invoke(
        &self,
        invocation: &tilolang_tilo::runtime::delegate::Invocation,
    ) -> tilolang_tilo::runtime::delegate::Invoked {
        use tilolang_tilo::runtime::delegate::{Invocation, Invoked};
        let ask = match invocation {
            Invocation::Action {
                action,
                args,
                subject: Some(subject),
                ..
            } => {
                let subject_id = subject.id_string();
                let mut arguments = BTreeMap::new();
                for (name, value) in args {
                    if matches!(value, Value::Ref(r) if r.target.id_string() == subject_id)
                        || matches!(value, Value::Opt(None))
                    {
                        continue;
                    }
                    arguments.insert(name.clone(), argument_text(value));
                }
                Ask {
                    resource: subject_id,
                    action: action.clone(),
                    arguments,
                }
            }
            Invocation::Action {
                app, action, args, ..
            } if app == super::SESSION_APP && self.session.contains_key(action) => Ask {
                resource: self.session[action].clone(),
                action: action.clone(),
                arguments: session_arguments(self.verbs.get(action), args),
            },
            Invocation::Action { name, .. } => {
                return Invoked::Failed(format!(
                    "`{name}` names no resource of its app to be sent to"
                ));
            }
            Invocation::Edit {
                target,
                field_id,
                value,
                ..
            } => {
                let action = match value {
                    Value::Bool(_) => tilolang_tilo::action::EDIT_FLAG_ACTION,
                    _ => tilolang_tilo::action::EDIT_TEXT_ACTION,
                };
                Ask {
                    resource: target.id_string(),
                    action: action.to_owned(),
                    arguments: BTreeMap::from([
                        ("field".to_owned(), field_id.clone()),
                        ("value".to_owned(), argument_text(value)),
                    ]),
                }
            }
        };
        self.link.ask(ask);
        Invoked::Done(Value::Unit)
    }
}

/// The arguments of a call of one of the session's loose actions, `verb` as its description
/// declares it: each left-out one dropped, and those of a parameter published as a field of
/// another nested back under that parameter, as its JSON.
fn session_arguments(
    verb: Option<&ui::tilo_actions::surface::VerbSurface>,
    args: &[(String, Value)],
) -> BTreeMap<String, String> {
    let given = args
        .iter()
        .filter(|(_, value)| !matches!(value, Value::Opt(None)));
    let Some(verb) = verb.filter(|verb| verb.params.iter().any(|param| param.within.is_some()))
    else {
        return given
            .map(|(name, value)| (name.clone(), argument_text(value)))
            .collect();
    };
    let json = given
        .map(|(name, value)| {
            let text = argument_text(value);
            let json = match value {
                Value::Text(_) => serde_json::Value::String(text),
                Value::Opt(Some(inner)) if matches!(**inner, Value::Text(_)) => {
                    serde_json::Value::String(text)
                }
                _ => serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
            };
            (name.clone(), json)
        })
        .collect();
    ui::tilo_actions::surface::nest_arguments(verb, json)
        .into_iter()
        .map(|(name, value)| {
            let text = match value {
                serde_json::Value::String(text) => text,
                other => other.to_string(),
            };
            (name, text)
        })
        .collect()
}

/// An argument as the session's door takes it from the shell: a text as it is, anything else
/// as its JSON, read back by the parameter's type.
fn argument_text(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Ref(r) => r.target.id_string(),
        Value::Opt(Some(value)) => argument_text(value),
        other => tilolang_tilo::runtime::value::to_json(other)
            .map_or_else(|_| String::new(), |json| json.to_string()),
    }
}

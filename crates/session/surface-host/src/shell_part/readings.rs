//! The readings of other apps' resources the shell's places show, and what the views composed
//! over them change of those resources: the session publishes each place's record's reading
//! ([`SessionLink::follow_places`]), the shell's runtime reads it ([`SessionOrigin::poll_readings`]),
//! and an edit or an action in a composed view goes back as an ask to the record's door
//! ([`PlaceInvoker`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use tilolang_tilo::origin::OriginCx;
use tilolang_tilo::records_service::{app_of, placed_records};
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
        self.readings_fresh.borrow_mut().insert(id.to_owned());
        true
    }

    /// The verbs the session's resources publish that no loaded package declares, by resource
    /// and verb ([`tilolang_tilo::runtime::published`]): the shell's runtime sends a view's action of
    /// one to the app publishing it, as it sends any other app's action to that app's door.
    /// Whether they differ from the last.
    pub fn publish_published_actions(
        &self,
        published: tilolang_tilo::runtime::published::Published,
    ) -> bool {
        if self.published.borrow().as_ref() == Some(&published) {
            return false;
        }
        *self.published.borrow_mut() = Some(published);
        self.published_fresh.set(true);
        true
    }

    /// The packages of the apps whose resources the places show, by app id, each its compiled
    /// module: the shell's runtime loads each it has not, so the places draw those resources
    /// through their app's own views, and unloads every other app's, with the readings of its
    /// resources. Whether they differ from the last.
    pub fn publish_packages(&self, packages: BTreeMap<String, std::sync::Arc<[u8]>>) -> bool {
        let same = {
            let held = self.packages.borrow();
            held.len() == packages.len()
                && held
                    .iter()
                    .zip(&packages)
                    .all(|((a, x), (b, y))| a == b && std::sync::Arc::ptr_eq(x, y))
        };
        if same {
            return false;
        }
        *self.packages.borrow_mut() = packages;
        self.packages_fresh.set(true);
        true
    }

    /// The apps whose packages the shell's runtime loads now, by id.
    #[must_use]
    pub fn package_apps(&self) -> Vec<String> {
        self.packages.borrow().keys().cloned().collect()
    }

    /// The apps whose package the shell's runtime loads here, each with the module it loads:
    /// the ones whose declared actions are their package's, so no published verb of theirs is
    /// counted (`tilolang_tilo::published::from_resources`).
    #[must_use]
    pub fn packages_loaded(&self) -> BTreeMap<String, String> {
        self.packages
            .borrow()
            .keys()
            .map(|app| (app.clone(), app.clone()))
            .collect()
    }

    /// The resources the shell's views read before their readings came in, by id, each with
    /// its type (`<app>#<Type>`): what the session publishes next. Each is handed out once.
    #[must_use]
    pub fn take_wanted(&self) -> BTreeMap<String, String> {
        std::mem::take(&mut *self.wanted.borrow_mut())
    }

    /// Whether the shell's views wait on readings the session has not been asked for yet.
    #[must_use]
    pub fn wants(&self) -> bool {
        !self.wanted.borrow().is_empty()
    }

    /// The shell's views read `wanted` (each an id and its type) before their readings came.
    pub(super) fn want(&self, wanted: Vec<(String, String)>) {
        self.wanted.borrow_mut().extend(wanted);
    }

    /// The apps whose package the shell's views read a resource of before it was loaded, by
    /// app id, each with the type as the view named it (`<app>#<Type>`): what the session
    /// publishes next, as it publishes the packages of the apps whose places show. Each app is
    /// handed out once.
    #[must_use]
    pub fn take_wanted_packages(&self) -> BTreeMap<String, String> {
        std::mem::take(&mut *self.wanted_packages.borrow_mut())
    }

    /// Whether the shell's views wait on packages the session has not been asked for yet.
    #[must_use]
    pub fn wants_packages(&self) -> bool {
        !self.wanted_packages.borrow().is_empty()
    }

    /// The shell's views read a resource of `wanted` (each an app and the type it read) before
    /// that app's package was loaded.
    pub(super) fn want_packages(&self, wanted: BTreeMap<String, String>) {
        self.wanted_packages.borrow_mut().extend(wanted);
    }

    /// The views could not be shown: `failed` says each call's app, view and why. What the
    /// session hears, so it says so where the person's choice is the one that failed
    /// (`shell.md` §6, "Nothing can lock a person out"). Each is handed out once.
    pub(super) fn view_failed(&self, failed: Vec<tilolang_tilo::runtime::foreign::ViewFailure>) {
        self.view_failures.borrow_mut().extend(failed);
    }

    /// The views that could not be shown since the last call, each an app, a view and why: what
    /// the session says the person should hear about.
    #[must_use]
    pub fn take_view_failures(&self) -> Vec<tilolang_tilo::runtime::foreign::ViewFailure> {
        std::mem::take(&mut *self.view_failures.borrow_mut())
    }

    /// The views that could not be shown and now can — the apps whose package failed to load
    /// and then loaded, and the calls that ran again and succeeded: what the session hears so
    /// a style the person chose comes back (`shell.md` §6). Each once.
    pub(super) fn view_recovered(&self, apps: Vec<String>) {
        self.view_recoveries.borrow_mut().extend(apps);
    }

    /// A view call whose failure ended: what the session hears so a style the person chose
    /// comes back (`shell.md` §6).
    pub(super) fn view_recovered_call(&self, view: (String, String)) {
        self.recovered_calls.borrow_mut().push(view);
    }

    /// The apps whose view failure ended since the last call, and the view calls that ran
    /// again and succeeded: a style comes back from either (`shell.md` §6).
    #[must_use]
    pub fn take_view_recoveries(&self) -> (Vec<String>, Vec<(String, String)>) {
        (
            std::mem::take(&mut *self.view_recoveries.borrow_mut()),
            std::mem::take(&mut *self.recovered_calls.borrow_mut()),
        )
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
        for (id, ty) in placed_records(
            arrangement.places.iter().map(|place| place.record.as_str()),
            resources,
        ) {
            if let Some(reading) = read(&id) {
                changed |= self.publish_reading(&id, &ty, reading);
            }
        }
        changed
    }
}

impl SessionOrigin {
    /// Hand the runtime the readings the session published since the last poll: the views
    /// composed over those resources run again. Whether any reading was new.
    pub(super) fn poll_readings(&self, cx: &OriginCx<'_>) -> bool {
        let fresh = std::mem::take(&mut *self.link.readings_fresh.borrow_mut());
        let mut moved = false;
        let readings = self.link.readings.borrow();
        for id in fresh {
            let Some((ty, reading)) = readings.get(&id) else {
                continue;
            };
            let Some(target) = ResourceRef::parse_id(&id, ty.clone()) else {
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

    /// Load the package of each app the places show that the runtime has not loaded, and
    /// unload every other app's, with the readings of its resources: an app nothing shows
    /// costs the shell nothing. Whether a package came or went.
    pub(super) fn poll_packages(&mut self, cx: &OriginCx<'_>) -> bool {
        let mut moved = self.poll_published(cx);
        if !self.link.packages_fresh.replace(false) {
            return moved;
        }
        let wanted = self.link.packages.borrow().clone();
        let gone: Vec<String> = self
            .loaded
            .iter()
            .filter(|(app, held)| {
                !wanted
                    .get(*app)
                    .is_some_and(|now| std::sync::Arc::ptr_eq(now, held))
            })
            .map(|(app, _)| app.clone())
            .collect();
        for app in gone {
            self.loaded.remove(&app);
            moved |= cx.host.unload_foreign(&app);
            self.link
                .readings
                .borrow_mut()
                .retain(|_, (ty, _)| app_of(ty) != Some(app.as_str()));
        }
        for (app, wasm) in wanted {
            // A package this part already runs is loaded again when its module changed: a
            // style reinstalled fixed has the same id and new code (`shell.md` §6).
            if self
                .loaded
                .get(&app)
                .is_some_and(|held| std::sync::Arc::ptr_eq(held, &wasm))
            {
                continue;
            }
            let Some(meta) = tilolang_tilo::runtime::Meta::from_wasm(&wasm) else {
                // A package the places show that carries no description cannot be drawn: its
                // views draw a failed state naming it rather than waiting for ever (`shell.md`
                // §6, "Nothing can lock a person out"), and the session hears of it.
                tracing::error!(app, "a package the places show carries no description");
                cx.host
                    .runtime()
                    .app_failed(&app, "the package carries no description".to_owned());
                moved = true;
                continue;
            };
            match cx.host.load_foreign_package(&wasm, &meta) {
                Ok(()) => {
                    self.loaded.insert(app, wasm);
                    moved = true;
                }
                Err(error) => {
                    // The package does not compile against what this runtime holds, or is not
                    // a package: nothing more is coming for it, so its views draw a failed
                    // state saying so instead of a skeleton that never fills. The failure is
                    // the shell's to say, through the session's notices (`shell.md` §6).
                    tracing::error!(app, %error, "a package the places show did not load");
                    cx.host.runtime().app_failed(&app, error.to_string());
                    moved = true;
                }
            }
        }
        // The verbs of the apps the runtime does not load. A package that loads here declares
        // its own actions, so the table is taken after the packages are, and it names only what
        // a package does not.
        moved
    }

    /// Hand the runtime the verbs the session's resources publish that no loaded package
    /// declares ([`tilolang_tilo::runtime::published`]): a view's action of one is sent to the app
    /// publishing it, the same path another app's declared action takes. Whether it changed.
    fn poll_published(&self, cx: &OriginCx<'_>) -> bool {
        if !self.link.published_fresh.replace(false) {
            return false;
        }
        let published = self.link.published.borrow().clone().unwrap_or_default();
        cx.host.runtime().set_published_actions(published);
        true
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
                from,
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
                    from: from
                        .as_ref()
                        .map(|from| ui::tilo_actions::params::MadeFrom {
                            resource: from.id_string(),
                            kind: from.ty.clone(),
                        }),
                }
            }
            Invocation::Action {
                app, action, args, ..
            } if app == super::SESSION_APP && self.session.contains_key(action) => Ask {
                // The guard says this action's resource is here; read it
                // without indexing so a later change to the guard cannot
                // panic a press.
                resource: self.session.get(action).cloned().unwrap_or_default(),
                action: action.clone(),
                arguments: session_arguments(self.verbs.get(action), args),
                from: None,
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
                    from: None,
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

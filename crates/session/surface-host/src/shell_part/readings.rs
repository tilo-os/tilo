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

use super::{
    ArrangementPlaceReading, ArrangementQueryReading, ArrangementReading, ArrangementRefReading,
    ArrangementRowReading, ArrangementStripReading, ArrangementWorkspaceReading, Ask, SessionLink,
    SessionOrigin,
};

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

    /// The address the session published the resource `reference` under, when it published it:
    /// the shell's runtime names another app's resource as its document and item
    /// (`chat.workspace/c_general`), while the session's catalogue names the same resource by its
    /// own address (`chat.workspace/channels/c_general`). A menu, a sheet or a press the shell
    /// sends back to the session's door must name it as the session published it, or the door
    /// finds no such action.
    #[must_use]
    pub fn published_address(&self, reference: &str) -> Option<String> {
        let reference = tilolang_tilo::runtime::value::flat_id(reference);
        let readings = self.readings.borrow();
        readings
            .iter()
            .find(|(id, _)| tilolang_tilo::runtime::value::flat_id(id) == reference)
            .map(|(id, _)| id.clone())
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

/// Each app of `packages`, the apps a place shows and the session serves, with the apps whose
/// packages they use (`use app.id as x`), closed over uses to any depth: a view of a placed app
/// that calls another app's `fn` (`discussion.said_about(t)`) runs that app's code, so its
/// package has to stand loaded here as the placed app's does. An app `module` answers nothing
/// for is left out, and the place waits for it as it waits for any package the session has not
/// served. So is an app this shell's runtime already holds as a package of its own
/// (`held_here`): every module lists `tilo.std` among the apps it uses (`PackageMeta::uses`),
/// and the session serves its module, but it is no app of the workspace and loading it as one
/// puts a package on the shell no place shows.
#[must_use]
pub fn with_uses(
    packages: impl IntoIterator<Item = (String, std::sync::Arc<[u8]>)>,
    module: impl Fn(&str) -> Option<std::sync::Arc<[u8]>>,
) -> BTreeMap<String, std::sync::Arc<[u8]>> {
    let mut out: BTreeMap<String, std::sync::Arc<[u8]>> = BTreeMap::new();
    let mut queue: Vec<String> = Vec::new();
    for (app, wasm) in packages {
        if out.insert(app.clone(), wasm).is_none() {
            queue.push(app);
        }
    }
    while let Some(app) = queue.pop() {
        let Some(wasm) = out.get(&app).cloned() else {
            continue;
        };
        let Some(meta) = tilolang_tilo::runtime::Meta::from_wasm(&wasm) else {
            continue;
        };
        for used in meta.uses {
            if used == app || out.contains_key(&used) || held_here(&used) {
                continue;
            }
            if let Some(module) = module(&used)
                && out.insert(used.clone(), module).is_none()
            {
                queue.push(used);
            }
        }
    }
    out
}

/// Whether the shell's runtime already holds what `app` is as a package, so a place's apps must
/// not bring it in again as another app's code:
///
/// * the platform's packages (`tilolang_tilo::platform_apps::platform_packages`: the device,
///   Settings and the session), which the runtime loads for itself as descriptions the session's
///   door answers (`ShellPlatform::load`), so a used one stands here already and loading it
///   again would redeclare its ids;
/// * the standard library (`tilo.std`), which every module lists among its uses but which shows
///   nothing itself: its views are templates the compiler expands into their callers, so no
///   call runs its code and the runtime holds no package of it and needs none.
fn held_here(app: &str) -> bool {
    if app == tilolang_tilo::tilolang::stdlib::APP {
        return true;
    }
    match tilolang_tilo::platform_apps::platform_packages() {
        Ok(packages) => packages.iter().any(|meta| meta.app == app),
        Err(why) => {
            tracing::error!(%why, "the platform's packages do not read");
            false
        }
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
                name,
                subject: Some(subject),
                from,
                ..
            } => Ask {
                resource: subject.id_string(),
                // As the resource's app publishes it.
                action: name.clone(),
                arguments: invocation.door_arguments().into_iter().collect(),
                from: from
                    .as_ref()
                    .map(|from| ui::tilo_actions::params::MadeFrom {
                        resource: from.id_string(),
                        kind: from.ty.clone(),
                    }),
            },
            Invocation::Action { app, action, .. }
                if app == super::SESSION_APP && self.session.contains_key(action) =>
            {
                Ask {
                    // The guard says this action's resource is here; read it
                    // without indexing so a later change to the guard cannot
                    // panic a press.
                    resource: self.session.get(action).cloned().unwrap_or_default(),
                    action: action.clone(),
                    arguments: session_arguments(
                        self.verbs.get(action),
                        invocation.door_arguments(),
                    ),
                    from: None,
                }
            }
            Invocation::Action { name, .. } => {
                return Invoked::Failed(format!(
                    "`{name}` names no resource of its app to be sent to"
                ));
            }
            Invocation::Edit {
                target,
                field,
                value,
                ..
            } => {
                let action = match value {
                    Value::Bool(_) => tilolang_tilo::action::EDIT_FLAG_ACTION,
                    _ => tilolang_tilo::action::EDIT_TEXT_ACTION,
                };
                Ask {
                    // As a press: the session's own address, never the flattened reference.
                    resource: self
                        .link
                        .published_address(&target.id_string())
                        .unwrap_or_else(|| target.id_string()),
                    action: action.to_owned(),
                    arguments: BTreeMap::from([
                        ("field".to_owned(), field.clone().into()),
                        (
                            "value".to_owned(),
                            tilolang_tilo::runtime::value::to_argument(value).unwrap_or_default(),
                        ),
                    ]),
                    from: None,
                }
            }
        };
        self.link.ask(ask);
        Invoked::Done(Value::Unit)
    }
}

/// The arguments `given` of a call of one of the session's loose actions, `verb` as its
/// description declares it: those of a parameter published as a field of another nested back
/// under that parameter.
fn session_arguments(
    verb: Option<&ui::tilo_actions::surface::VerbSurface>,
    given: serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    match verb {
        Some(verb) => ui::tilo_actions::surface::nest_arguments(verb, given.into_iter().collect())
            .into_iter()
            .collect(),
        None => given.into_iter().collect(),
    }
}

/// The fields of a canvas query, as it is written into the shell's record. Its refs are the
/// `refs` collection's, each naming its query by key.
pub(super) fn query_fields(q: &ArrangementQueryReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", Value::Text(q.key.clone())),
        ("text", Value::Text(q.text.clone())),
        ("in_app", Value::Text(q.app.clone())),
        ("kind", Value::Text(q.kind.clone())),
        ("state", Value::Text(q.state.clone())),
        ("reason", Value::Text(q.reason.clone())),
    ]
}

/// The fields of a resource a query found, as it is written into the shell's record: its query by
/// key, its address, its title and its kind.
pub(super) fn ref_fields(r: &ArrangementRefReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", Value::Text(r.key.clone())),
        ("query", Value::Text(query_of_ref_key(&r.key))),
        ("address", Value::Text(r.address.clone())),
        ("title", Value::Text(r.title.clone())),
        ("kind", Value::Text(r.kind.clone())),
    ]
}

/// The query a ref belongs to, from its key (`<query>/<number>`).
fn query_of_ref_key(key: &str) -> String {
    key.split('/').next().unwrap_or_default().to_owned()
}

/// The field value a text is, in the shell's record.
fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// The fields of an arrangement workspace, as it is written into the shell's record.
pub(super) fn workspace_fields(w: &ArrangementWorkspaceReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&w.key)),
        ("name", text(&w.name)),
        ("current", Value::Bool(w.current)),
    ]
}

/// The fields of a row, as it is written into the shell's record.
pub(super) fn row_fields(r: &ArrangementRowReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&r.key)),
        ("workspace", text(&r.workspace)),
        ("name", text(&r.name)),
        ("shell", text(&r.shell)),
    ]
}

/// The fields of a strip, as it is written into the shell's record.
pub(super) fn strip_fields(s: &ArrangementStripReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&s.key)),
        ("workspace", text(&s.workspace)),
        ("row", text(&s.row)),
        ("width", Value::Int(s.width)),
        ("maximized", Value::Bool(s.maximized)),
        ("active", text(&s.active)),
        ("placed", Value::Bool(s.placed)),
        ("canvas_x", Value::Int(s.canvas_x)),
        ("canvas_y", Value::Int(s.canvas_y)),
        ("canvas_width", Value::Int(s.canvas_width)),
        ("canvas_height", Value::Int(s.canvas_height)),
        ("shell", text(&s.shell)),
    ]
}

/// The fields of a place, as it is written into the shell's record.
pub(super) fn place_fields(p: &ArrangementPlaceReading) -> Vec<(&'static str, Value)> {
    vec![
        ("key", text(&p.key)),
        ("strip", text(&p.strip)),
        ("shows", text(&p.shows)),
        ("shown_as", text(&p.shown_as)),
        ("record", text(&p.record)),
        ("title", text(&p.title)),
        ("name", text(&p.name)),
        ("pinned", Value::Bool(p.pinned)),
        ("shell", text(&p.shell)),
    ]
}

#[cfg(test)]
mod with_uses_tests {
    use std::sync::Arc;

    use super::with_uses;

    /// A compiled app, its module carrying its metadata as an installed app's does, made against
    /// the packages it uses as a build is: each app here declares one root type and a view of it,
    /// and `uses` names the apps it imports.
    fn package(
        app: &str,
        uses: &[&str],
        imports: &[tilolang_tilo::tilolang::PackageMeta],
    ) -> tilolang_tilo::tilolang::Compiled {
        let mut imports_written = String::new();
        for app in uses {
            imports_written.push_str("use ");
            imports_written.push_str(app);
            imports_written.push_str(" as u");
            imports_written.push_str(&app.replace('.', "_"));
            imports_written.push('\n');
        }
        let source = format!(
            "app {app} \"Test\"\n\n{imports_written}\ntype Bag {{\n    n: Int\n}}\n\nroot Bag\n\ndefault view card for Bag\n\nview card(b: Bag) {{\n    text(\"{app}\")\n}}\n"
        );
        tilolang_tilo::tilolang::compile_package(
            &source,
            &tilolang_tilo::tilolang::IdsLock::default(),
            imports,
        )
        .unwrap_or_else(|diagnostics| panic!("{app} compiles: {diagnostics:?}"))
    }

    fn wasm(of: &tilolang_tilo::tilolang::Compiled) -> Arc<[u8]> {
        Arc::from(of.wasm.clone())
    }

    /// A placed app's own package stands with the packages of the apps it uses, to any depth:
    /// its views call their `fn`s (`discussion.said_about(t)`), so their code runs here, and a
    /// place that drew the app without them drew a failed state in every row.
    #[test]
    fn the_apps_a_placed_app_uses_are_loaded_with_it() {
        let lib = package("me.lib", &[], &[]);
        let mid = package("me.mid", &["me.lib"], std::slice::from_ref(&lib.meta));
        let top = package("me.top", &["me.mid"], &[lib.meta.clone(), mid.meta.clone()]);
        let modules = std::collections::BTreeMap::from([
            ("me.top".to_owned(), wasm(&top)),
            ("me.mid".to_owned(), wasm(&mid)),
            ("me.lib".to_owned(), wasm(&lib)),
        ]);
        let loaded = with_uses([("me.top".to_owned(), wasm(&top))], |app| {
            modules.get(app).cloned()
        });
        assert_eq!(
            loaded.keys().cloned().collect::<Vec<_>>(),
            ["me.lib", "me.mid", "me.top"],
            "every app used, to any depth, is published beside the app that uses it"
        );
    }

    /// An app whose package the session serves none of is left out: the place waits for it as it
    /// waits for any package a view read and the session has not served.
    #[test]
    fn a_used_app_the_session_serves_nothing_of_is_left_out() {
        let mid = package("me.mid", &[], &[]);
        let top = package("me.top", &["me.mid"], std::slice::from_ref(&mid.meta));
        let loaded = with_uses([("me.top".to_owned(), wasm(&top))], |_| None);
        assert_eq!(loaded.len(), 1, "only the app the place shows stands");
    }

    /// Every module lists the standard library among the apps it uses, and the session serves its
    /// module, but the runtime needs no package of it: its views are templates the compiler
    /// expands into their callers, so no call into it runs (its test proves it declares nothing
    /// else). Loaded as one, it stands on the shell as an app of the workspace no place shows.
    #[test]
    fn the_standard_library_an_app_uses_is_not_loaded_as_an_app() {
        let top = package("me.top", &[], &[]);
        assert!(
            top.meta.uses.iter().any(|used| used == "tilo.std"),
            "every module lists the library among its uses: {:?}",
            top.meta.uses
        );
        let library = tilolang_tilo::tilolang::stdlib::compiled();
        let loaded = with_uses([("me.top".to_owned(), wasm(&top))], |app| {
            (app == "tilo.std").then(|| wasm(library))
        });
        assert_eq!(
            loaded.keys().cloned().collect::<Vec<_>>(),
            ["me.top"],
            "the library is no app of the workspace, though the session serves its module"
        );
    }

    /// A platform package the shell's runtime already holds as a description is no app a place
    /// brings in either: the platform loads it for itself, and loading it again as another app's
    /// code would declare its ids twice. The session is asked for no module of it.
    #[test]
    fn a_platform_package_an_app_uses_is_not_loaded_again() {
        let device = tilolang_tilo::platform_apps::platform_packages()
            .expect("the platform's packages read")
            .into_iter()
            .find(|meta| meta.app == tilolang_tilo::platform_apps::DEVICE_APP)
            .expect("the device is one of the platform's packages");
        let top = package("me.top", &["so.tilo.device"], std::slice::from_ref(&device));
        let asked = std::cell::RefCell::new(Vec::new());
        let loaded = with_uses([("me.top".to_owned(), wasm(&top))], |app| {
            asked.borrow_mut().push(app.to_owned());
            None
        });
        assert_eq!(
            loaded.keys().cloned().collect::<Vec<_>>(),
            ["me.top"],
            "the device's package stands on the runtime already"
        );
        assert_eq!(
            *asked.borrow(),
            Vec::<String>::new(),
            "the device's package is not even looked up: the runtime holds it"
        );
    }
}

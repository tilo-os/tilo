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
    /// counted ([`published_actions`]).
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
        for (id, ty) in placed_records(arrangement, resources) {
            if let Some(reading) = read(&id) {
                changed |= self.publish_reading(&id, &ty, reading);
            }
        }
        changed
    }
}

/// Each resource a place of `arrangement` names that `resources`, the session's catalogue,
/// holds as a tilolang record: its id, and its type as the shell's runtime names another
/// app's (`<app>#<Type>`).
#[must_use]
pub fn placed_records(
    arrangement: &ArrangementReading,
    resources: &[ui::tilo_actions::ResourceSpec],
) -> BTreeMap<String, String> {
    arrangement
        .places
        .iter()
        .filter(|place| !place.record.is_empty())
        .filter_map(|place| {
            let ty = find_resource(resources, &place.record)
                .and_then(tilolang_tilo::records_service::reading_type_of)?;
            Some((place.record.clone(), ty))
        })
        .collect()
}

/// The app of a resource whose type the shell's runtime names `<app>#<Type>`.
#[must_use]
pub fn app_of(ty: &str) -> Option<&str> {
    ty.split_once('#').map(|(app, _)| app)
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
        let used = uses
            .iter()
            .map(|used| format!("use {used} as u{}\n", used.replace('.', "_")))
            .collect::<String>();
        let source = format!(
            "app {app} \"Test\"\n\n{used}\ntype Bag {{\n    n: Int\n}}\n\nroot Bag\n\ndefault view card for Bag\n\nview card(b: Bag) {{\n    text(\"{app}\")\n}}\n"
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

/// The verbs each resource of `resources`, the session's catalogue, publishes that no loaded
/// package of `loaded` declares ([`tilolang_tilo::runtime::published`]): the resource's id, the verb's
/// id, the app that runs it, and the arguments it takes after the resource (`channel`, `text`).
///
/// A verb of an app whose package the shell loads here is that package's declared action
/// (`chat.workspace#rename-channel`), and is not counted: the runtime resolves a declared action
/// before it looks a published one up. A verb with no parameter tilolang code can give is left
/// out, as the catalogue leaves it out of a package's actions.
#[must_use]
pub fn published_actions(
    resources: &[ui::tilo_actions::ResourceSpec],
    loaded: &BTreeMap<String, String>,
) -> tilolang_tilo::runtime::published::Published {
    let mut published = tilolang_tilo::runtime::published::Published::default();
    collect_published(resources, loaded, &mut published);
    published
}

fn collect_published(
    resources: &[ui::tilo_actions::ResourceSpec],
    loaded: &BTreeMap<String, String>,
    out: &mut tilolang_tilo::runtime::published::Published,
) {
    for resource in resources {
        let Some(app) = resource.app() else {
            collect_published(&resource.children, loaded, out);
            continue;
        };
        // The app's package loading here declares its own actions already.
        if !loaded.contains_key(app) {
            for action in &resource.actions {
                out.insert(
                    resource.id.as_str(),
                    action.id.as_str(),
                    tilolang_tilo::runtime::published::PublishedAction {
                        app: app.to_owned(),
                        params: action
                            .params()
                            .iter()
                            .map(|param| param.name.to_string())
                            .collect(),
                    },
                );
            }
        }
        collect_published(&resource.children, loaded, out);
    }
}

/// The apps whose views this process links as Rust apps (one-app-model.md §2): a resource of one
/// of their types is drawn by the app's own view, not by a package's, and its reading is followed
/// though the app publishes no module.
#[must_use]
pub fn rust_view_apps() -> Vec<String> {
    tilolang_tilo::views::declared_apps()
        .into_iter()
        .map(str::to_owned)
        .collect()
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
                    // The session's own address for the resource, never the runtime's flattened
                    // reference: the door finds an action by the id its catalogue published.
                    resource: self
                        .link
                        .published_address(&subject_id)
                        .unwrap_or(subject_id),
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
                    // As a press: the session's own address, never the flattened reference.
                    resource: self
                        .link
                        .published_address(&target.id_string())
                        .unwrap_or_else(|| target.id_string()),
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

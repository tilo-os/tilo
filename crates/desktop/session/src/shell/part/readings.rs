//! The readings of other apps' resources the shell's places show, and what the views composed
//! over them change of those resources: the session publishes each place's record's reading
//! ([`SessionLink::follow_places`]), the shell's runtime reads it ([`SessionOrigin::poll_readings`]),
//! and an edit or an action in a composed view goes back as an ask to the record's API
//! ([`PlaceInvoker`]).

use std::collections::BTreeMap;
use std::rc::Rc;

use tilolang_service::placed_documents;
use tilolang_tilo::origin::OriginCx;
use tilolang_tilo::readings::app_of;
use tilolang_tilo::runtime::{ResourceRef, Value};

use super::{Ask, LayoutReading, SessionLink, SessionOrigin};

impl SessionLink {
    /// The session published `reading` of the resource `id`, of the type `ty` (`<app>#<Type>`):
    /// the shell's runtime reads it on its next frame, and the views composed over it run
    /// again. Whether it differs from the last.
    ///
    /// A reference the reading holds to an app whose package no host has served yet is a
    /// package the place's view needs, and the session is asked for it ([`Self::wants_packages`]):
    /// a linked task is a row of the tasks app's own, and the place says it waits for it rather
    /// than drawing nothing until it comes. The library and the platform's own packages are
    /// already here and are no ask ([`held_here`]).
    pub fn publish_reading(&self, id: &str, ty: &str, reading: serde_json::Value) -> bool {
        let fresh = (ty.to_owned(), reading);
        if self.readings.borrow().get(id) == Some(&fresh) {
            return false;
        }
        let wanted = {
            let held = self.packages.borrow();
            let mut wanted = BTreeMap::new();
            wanted_links(&fresh.1, &held, &mut wanted);
            wanted
        };
        if !wanted.is_empty() {
            self.want_packages(wanted);
        }
        self.readings.borrow_mut().insert(id.to_owned(), fresh);
        let at = self
            .readings_generation
            .borrow()
            .get(id)
            .copied()
            .unwrap_or(0);
        self.readings_generation
            .borrow_mut()
            .insert(id.to_owned(), at.wrapping_add(1));
        true
    }

    /// The address the session published the resource `reference` under, when it published it:
    /// the shell's runtime names another app's resource as its document and item
    /// (`chat.workspace/c_general`), while the session's catalogue names the same resource by its
    /// own address (`chat.workspace/channels/c_general`). A menu, a sheet or a press the shell
    /// sends back to the session's API must name it as the session published it, or the API
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
    /// one to the app publishing it, as it sends any other app's action to that app's API.
    /// Whether they differ from the last.
    pub fn publish_published_actions(
        &self,
        published: tilolang_tilo::runtime::published::Published,
    ) -> bool {
        if self.published.borrow().as_ref() == Some(&published) {
            return false;
        }
        *self.published.borrow_mut() = Some(published);
        self.published_generation
            .set(self.published_generation.get().wrapping_add(1));
        true
    }

    /// The actions the session's resources publish as unavailable now, each with the reason
    /// ([`tilolang_tilo::runtime::unavailable`]): what a view's control bound to one draws disabled.
    /// Whether they differ from the last.
    pub fn publish_unavailable(
        &self,
        unavailable: tilolang_tilo::runtime::unavailable::Unavailable,
    ) -> bool {
        if self.unavailable.borrow().as_ref() == Some(&unavailable) {
            return false;
        }
        *self.unavailable.borrow_mut() = Some(unavailable);
        self.unavailable_generation
            .set(self.unavailable_generation.get().wrapping_add(1));
        true
    }

    /// The actions the session's resources publish as unavailable now, each with the reason
    /// ([`tilolang_tilo::runtime::unavailable`]), by the id the shell's runtime binds them by: the
    /// address a resource publishes a verb at (`desktop:chrome#delete-workspace`), and the id
    /// the session's own package names its loose action by (`Desktop/delete-workspace`), which
    /// the shell's code calls. Whether they differ from the last.
    pub fn publish_actions_unavailable(
        &self,
        resources: &[ui::tilo_actions::ResourceSpec],
    ) -> bool {
        self.publish_unavailable(unavailable_of(
            resources,
            self.session_actions(),
            self.session_verbs(),
        ))
    }

    /// The resource each of the session's loose actions is published on, by the action's id, as
    /// the session's description declares them.
    fn session_actions(&self) -> &BTreeMap<String, String> {
        self.session_actions
            .get_or_init(|| tilolang_tilo::platform::session_addresses().unwrap_or_default())
    }

    /// The session's loose actions as its description declares them, by id.
    fn session_verbs(&self) -> &BTreeMap<String, ui::tilo_actions::surface::VerbSurface> {
        self.session_verbs
            .get_or_init(|| tilolang_tilo::platform::session_verbs().unwrap_or_default())
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
        self.packages_generation
            .set(self.packages_generation.get().wrapping_add(1));
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
    pub(super) fn view_failed(&self, failed: Vec<tilolang_tilo::runtime::error::ViewFailure>) {
        self.view_failures.borrow_mut().extend(failed);
    }

    /// The views that could not be shown since the last call, each an app, a view and why: what
    /// the session says the person should hear about.
    #[must_use]
    pub fn take_view_failures(&self) -> Vec<tilolang_tilo::runtime::error::ViewFailure> {
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

    /// Publish the reading of every resource a place of `layout` names, from what the
    /// session publishes: `resources`, its catalogue, says each one's kind and `read` its
    /// reading. A place naming nothing the catalogue holds as a tilolang record keeps its
    /// well. Whether any reading changed.
    pub fn follow_places(
        &self,
        layout: &LayoutReading,
        resources: &[ui::tilo_actions::ResourceSpec],
        read: impl Fn(&str) -> Option<serde_json::Value>,
    ) -> bool {
        let mut changed = false;
        for (id, ty) in placed_documents(
            layout.places.iter().map(|place| place.resource.as_str()),
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

/// Each app `reading` holds a link to (`$links`, each `{"ref": …, "ty": "<app>#<Type>"}`),
/// added to `wanted` when no package here declares it: a place whose view draws the link (`a
/// message made a task`) needs the linked app's package, so the place says it waits for that
/// app rather than drawing nothing until it comes.
fn wanted_links(
    reading: &serde_json::Value,
    held: &BTreeMap<String, std::sync::Arc<[u8]>>,
    wanted: &mut BTreeMap<String, String>,
) {
    let Some(links) = reading
        .get(tilolang_tilo::runtime::store::LINKS)
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for link in links {
        let Some(ty) = link.get("ty").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some((app, _)) = ty.split_once('#') else {
            continue;
        };
        // The library and the platform's own packages are here without the session serving
        // one, and are no ask.
        if held.contains_key(app) || held_here(app) {
            continue;
        }
        wanted.insert(app.to_owned(), ty.to_owned());
    }
}

/// Whether the shell's runtime already holds what `app` is as a package, so a place's apps must
/// not bring it in again as another app's code:
///
/// * the platform's packages (`tilolang_tilo::platform::platform_packages`: the device,
///   Settings and the session), which the runtime loads for itself as descriptions the session's
///   API answers (`ShellPlatform::load`), so a used one stands here already and loading it
///   again would redeclare its ids;
/// * the standard library (`tilo.std`), which every module lists among its uses and which the
///   runtime loads beside every package it runs: its views are templates the compiler expands
///   into their callers, and its `fn`s run in its own package, so a place's apps bring it in no
///   more than they bring the runtime's packages.
fn held_here(app: &str) -> bool {
    if app == tilolang_tilo::tilolang::stdlib::APP {
        return true;
    }
    match tilolang_tilo::platform::is_platform_app(app) {
        Ok(held) => held,
        Err(why) => {
            tracing::error!(%why, "the platform's packages do not read");
            false
        }
    }
}

/// Whether `target` is the workspace's members for a host the session publishes them to, which
/// `reading` then serves: whether the reading moved. None for any other resource, or a host that
/// reads its own workspace's members.
fn serves_members(
    cx: &OriginCx<'_>,
    target: &ResourceRef,
    reading: &serde_json::Value,
) -> Option<bool> {
    (target.doc == tilolang_tilo::platform::WORKSPACE_MEMBERS && cx.host.reads_members_elsewhere())
        .then(|| cx.host.serve_member_reading(reading))
}

impl SessionOrigin {
    /// Hand the runtime the readings the session published since this part last read them: the
    /// views composed over those resources run again. Whether any reading was new.
    pub(super) fn poll_readings(&self, cx: &OriginCx<'_>) -> bool {
        let generations = self.link.readings_generation.borrow().clone();
        let mut seen = self.readings_seen.borrow_mut();
        let mut moved = false;
        let readings = self.link.readings.borrow();
        for (id, generation) in generations {
            if seen.get(&id).copied().unwrap_or(0) >= generation {
                continue;
            }
            // The display a part draws on is that part's own reading: the session publishes it
            // to this runtime, never through the shared layout every part draws. The room its
            // style's chrome leaves is its own the same way.
            let own = match id.as_str() {
                tilo_session_core::shell::SCREEN_RESOURCE => self.screen.borrow().clone(),
                tilo_session_core::shell::ROOM_RESOURCE => self.room.borrow().clone(),
                _ => None,
            };
            let Some((ty, held)) = readings.get(&id) else {
                continue;
            };
            let reading = own.unwrap_or_else(|| held.clone());
            // A root the platform publishes is named by its whole address, as the runtime names
            // the root (`devices/{device}/settings/display`): read as a document and an item, one
            // of the device's settings would be `devices/display`, which no root reads.
            let root = tilolang_tilo::platform::platform_roots()
                .iter()
                .any(|(published, _)| *published == id);
            let target = if root {
                ResourceRef {
                    doc: id.clone(),
                    item: None,
                    ty: ty.clone(),
                }
            } else {
                let Some(target) = ResourceRef::parse_id(&id, ty.clone()) else {
                    tracing::error!(id, "a place names no resource id");
                    continue;
                };
                target
            };
            // A root the part's host reads itself — its own workspace's members, or the workspace
            // in front's the session publishes to it — is never read as the session publishes it.
            let by_members = serves_members(cx, &target, &reading);
            if by_members.is_some() || cx.host.reads_in_workspace(&target) {
                moved |= by_members.unwrap_or(false);
                seen.insert(id, generation);
                continue;
            }
            match cx.host.runtime().set_reading(&target, &reading.to_string()) {
                Ok(changed) => moved |= changed,
                Err(error) => tracing::error!(id, %error, "a place's reading was refused"),
            }
            if id == tilo_session_core::shell::SCREEN_RESOURCE {
                self.screen_seen.set(self.screen_generation.get());
            }
            seen.insert(id, generation);
        }
        // A display told to this part since its last poll: its screen reading again, even while
        // the session's shared reading of the resource has not moved.
        let screen = self.screen_generation.get();
        if screen > self.screen_seen.get() {
            let told = self.screen.borrow().clone();
            if told.is_none() {
                // The display went: there is nothing to tell this part, and its view draws
                // nothing until another display is its.
                self.screen_seen.set(screen);
            }
            if let Some(reading) = told
                && let Some((ty, _)) = readings.get(tilo_session_core::shell::SCREEN_RESOURCE)
                && let Some(target) =
                    ResourceRef::parse_id(tilo_session_core::shell::SCREEN_RESOURCE, ty.clone())
            {
                self.screen_seen.set(screen);
                match cx.host.runtime().set_reading(&target, &reading.to_string()) {
                    Ok(changed) => moved |= changed,
                    Err(error) => tracing::error!(%error, "the part's display was refused"),
                }
                seen.insert(
                    tilo_session_core::shell::SCREEN_RESOURCE.to_owned(),
                    self.link
                        .readings_generation
                        .borrow()
                        .get(tilo_session_core::shell::SCREEN_RESOURCE)
                        .copied()
                        .unwrap_or(0),
                );
            }
        }
        // The room this part's style leaves a widget, measured from the frame it laid out since
        // this part last polled: it is told to this runtime too, though the session's shared
        // reading of the resource has not moved.
        let room = self.room_generation.get();
        if room > self.room_seen.get() {
            self.room_seen.set(room);
            if let Some(reading) = self.room.borrow().clone()
                && let Some((ty, _)) = readings.get(tilo_session_core::shell::ROOM_RESOURCE)
                && let Some(target) =
                    ResourceRef::parse_id(tilo_session_core::shell::ROOM_RESOURCE, ty.clone())
            {
                match cx.host.runtime().set_reading(&target, &reading.to_string()) {
                    Ok(changed) => moved |= changed,
                    Err(error) => tracing::error!(%error, "the part's room was refused"),
                }
                seen.insert(
                    tilo_session_core::shell::ROOM_RESOURCE.to_owned(),
                    self.link
                        .readings_generation
                        .borrow()
                        .get(tilo_session_core::shell::ROOM_RESOURCE)
                        .copied()
                        .unwrap_or(0),
                );
            }
        }
        moved
    }

    /// Load the package of each app the places show that the runtime has not loaded, and
    /// unload every other app's, with the readings of its resources: an app nothing shows
    /// costs the shell nothing. Whether a package came or went.
    pub(super) fn poll_packages(&mut self, cx: &OriginCx<'_>) -> bool {
        let mut moved = self.poll_published(cx);
        let generation = self.link.packages_generation.get();
        if generation <= self.packages_seen.get() {
            return moved;
        }
        self.packages_seen.set(generation);
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
    /// publishing it, the same path another app's declared action takes. And the actions those
    /// resources publish as unavailable now, so a view's control bound to one draws disabled
    /// ([`tilolang_tilo::runtime::unavailable`]). Whether it changed.
    fn poll_published(&self, cx: &OriginCx<'_>) -> bool {
        let mut moved = false;
        let generation = self.link.unavailable_generation.get();
        if generation > self.unavailable_seen.get() {
            self.unavailable_seen.set(generation);
            let unavailable = self.link.unavailable.borrow().clone().unwrap_or_default();
            cx.host.runtime().set_unavailable_actions(unavailable);
            moved = true;
        }
        let generation = self.link.published_generation.get();
        if generation > self.published_seen.get() {
            self.published_seen.set(generation);
            let published = self.link.published.borrow().clone().unwrap_or_default();
            cx.host.runtime().set_published_actions(published);
            moved = true;
        }
        moved
    }
}

/// The actions `resources` publish as unavailable now, each with its reason, by the key the
/// shell's runtime binds a view's control by ([`tilolang_tilo::runtime::unavailable`]): the
/// address of a resource's verb, and — for a verb the session's own package publishes as a loose
/// action — the id its code calls (`Desktop/delete-workspace`), which `session` maps to the
/// resource it is published on and `verbs` to the verb it names there.
fn unavailable_of(
    resources: &[ui::tilo_actions::ResourceSpec],
    session: &BTreeMap<String, String>,
    verbs: &BTreeMap<String, ui::tilo_actions::surface::VerbSurface>,
) -> tilolang_tilo::runtime::unavailable::Unavailable {
    let mut out = tilolang_tilo::runtime::unavailable::from_resources(resources.iter());
    for (id, address) in session {
        let Some(verb) = verbs.get(id) else {
            continue;
        };
        if let Some(reason) = unavailable_reason(resources, address, &verb.id) {
            out.insert(id, reason);
        }
    }
    out
}

/// Why the resource `address` of `resources` publishes its verb `verb` as unable to run now.
fn unavailable_reason(
    resources: &[ui::tilo_actions::ResourceSpec],
    address: &str,
    verb: &str,
) -> Option<String> {
    fn find(
        resource: &ui::tilo_actions::ResourceSpec,
        address: &str,
        verb: &str,
    ) -> Option<String> {
        if resource.id.as_str() == address {
            let said = resource
                .actions
                .iter()
                .find(|action| action.id == verb)
                .and_then(|action| match &action.contract.availability {
                    ui::tilo_actions::Availability::Unavailable { reason, .. } => {
                        Some(reason.clone())
                    }
                    ui::tilo_actions::Availability::Available => None,
                });
            if said.is_some() {
                return said;
            }
        }
        resource
            .children
            .iter()
            .find_map(|child| find(child, address, verb))
    }
    resources.iter().find_map(|r| find(r, address, verb))
}

/// What a view composed in a place changes of the resource it shows, sent to that resource's
/// API through the session: a press on one of its app's actions, or an edit of a field it
/// binds. The change comes back as the resource's next reading.
pub(super) struct PlaceInvoker {
    pub(super) link: Rc<SessionLink>,
    /// The resource each of the session's loose actions is published on, by the action's id
    /// (`session.show_surface(...)`, `Shell/show-surface`, goes to the shell).
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
        // An answer to an asked record is the record's own action, kept by the workspace's
        // invocations where the record is: it never reaches the session's API, which publishes
        // no such record.
        if let Invocation::Action {
            app,
            name,
            target: Some(target),
            ..
        } = invocation
            && tilolang_tilo::tilolang::is_invocation_type(&target.ty)
        {
            let record = target.item.clone().unwrap_or_else(|| target.doc.clone());
            self.link.answer(
                target.doc.clone(),
                record,
                tilolang_tilo::catalogue::published_verb(app.as_str(), name).to_owned(),
                invocation.api_arguments(),
            );
            return Invoked::Done(Value::Unit);
        }
        let ask = match invocation {
            Invocation::Action {
                app,
                name,
                target: Some(target),
                from,
                ..
            } => Ask {
                // As a press: the session's own address, never the flattened reference, and one of
                // the device's settings on this device.
                resource: ui::settings_device::on_this_device(
                    &self
                        .link
                        .published_address(&target.id_string())
                        .unwrap_or_else(|| target.id_string()),
                ),
                // As the resource's app publishes it: the verb, never the id its package names
                // it by among others' ([`tilolang_tilo::catalogue::action_id`]).
                action: tilolang_tilo::catalogue::published_verb(app.as_str(), name).to_owned(),
                arguments: invocation.api_arguments().into_iter().collect(),
                from: from
                    .as_ref()
                    .map(|from| ui::tilo_actions::params::MadeFrom {
                        resource: from.id_string(),
                        kind: tilolang_tilo::resources::kinds::kind_of_type(&from.ty),
                    }),
            },
            Invocation::Action {
                app, action, name, ..
            } if app == super::SESSION_APP && self.session.contains_key(action) => {
                Ask {
                    // The guard says this action's resource is here; read it
                    // without indexing so a later change to the guard cannot
                    // panic a press.
                    resource: self.session.get(action).cloned().unwrap_or_default(),
                    // As the session publishes it: the verb, not the id that names its type.
                    action: name.clone(),
                    arguments: session_arguments(
                        self.verbs.get(action),
                        invocation.api_arguments(),
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
                    // As a press: the session's own address, never the flattened reference, and one
                    // of the device's settings on this device.
                    resource: ui::settings_device::on_this_device(
                        &self
                            .link
                            .published_address(&target.id_string())
                            .unwrap_or_else(|| target.id_string()),
                    ),
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
        let device = tilolang_tilo::platform::platform_packages()
            .expect("the platform's packages read")
            .into_iter()
            .find(|meta| meta.app == tilolang_tilo::platform::DEVICE_APP)
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

#[cfg(test)]
mod unavailable_tests {
    use std::collections::BTreeMap;

    use ui::tilo_actions::{ActionSpec, Availability, EncodedAction, OperationKind, ResourceSpec};

    use super::{SessionLink, unavailable_of};

    /// The chrome resource as the session publishes it, its `delete-workspace` unable to run on
    /// the person's own workspace.
    fn chrome() -> ResourceSpec {
        let mut delete = ActionSpec::typed_operation(
            "delete-workspace",
            "Delete",
            "Delete the workspace",
            OperationKind::Effect,
            "so.tilo.chrome",
            Vec::new(),
            |_| {
                EncodedAction::encode_operation("so.tilo.chrome", &serde_json::json!({}))
                    .expect("an encoded call")
            },
        );
        delete.type_mut().contract.availability = Availability::Unavailable {
            code: "own_workspace".to_owned(),
            reason: "own workspace".to_owned(),
        };
        ResourceSpec::new("desktop:chrome", "desktop", "Desktop").with_action(delete)
    }

    #[test]
    fn a_session_action_published_unavailable_is_held_by_its_address_and_by_the_id_the_shell_calls()
    {
        let link = SessionLink::default();
        let actions: BTreeMap<String, String> = link.session_actions().clone();
        let verbs = link.session_verbs().clone();
        let map = unavailable_of(&[chrome()], &actions, &verbs);
        assert_eq!(
            map.get("desktop:chrome#delete-workspace"),
            Some("own workspace"),
            "the address the resource publishes its verb at"
        );
        assert_eq!(
            map.get("Desktop/delete-workspace"),
            Some("own workspace"),
            "the id the session's own package names it by, which the shell's code calls"
        );
    }

    #[test]
    fn a_resource_no_action_of_is_unavailable_says_nothing() {
        let link = SessionLink::default();
        let map = unavailable_of(
            &[ResourceSpec::new("desktop:shell", "desktop", "Desktop")],
            &link.session_actions().clone(),
            &link.session_verbs().clone(),
        );
        assert!(
            map.is_empty(),
            "nothing is drawn disabled for a resource whose actions can all run"
        );
    }
}

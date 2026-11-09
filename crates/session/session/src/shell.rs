//! The shell on the native session: the tilolang shell package (`apps/tl/shell`) shown by a
//! [`ShellPart`] per display — the whole screen under the windows, which stand in its wells — kept
//! in step with the session's desk (`crate::desk`) and asking the session's door for what the
//! person presses.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_compositor::Compositor;
use tilo_compositor::{CursorIcon, HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_service::Services;
use tilo_surface_host::shell_part::{ArrangementReading, Ask};
use tilo_surface_host::{Doors, SessionLink, ShellPart, ShellWork};

use crate::desk_resource::Desk;
use crate::displays;
use crate::panels::Held;

/// The shell package and the packages it uses, compiled, the shell last: what the shell's
/// instance runs. The installed `so.tilo.shell` when the person's app store holds one, the
/// build's fallback copy when it does not (shell.md §6).
///
/// # Errors
/// Neither the installed shell nor the fallback compiles: the fallback's diagnostics.
pub fn compile() -> Result<Vec<tilolang_tilo::tilolang::Compiled>, String> {
    Ok(shell_packages()?.0)
}

/// [`compile`], and whether the packages are the store's installed shell rather than this
/// build's fallback copy: what a test holds the loading path to.
///
/// # Errors
/// As [`compile`].
pub fn shell_packages() -> Result<(Vec<tilolang_tilo::tilolang::Compiled>, bool), String> {
    match tilo_surface_host::shell_part::installed_shell() {
        Ok(Some(packages)) => Ok((packages, true)),
        Ok(None) => Ok((tilo_surface_host::shell_part::compile_shell()?, false)),
        Err(error) => {
            // A store row that does not read is the person's to see, and the desk still stands:
            // the fallback is what a device with no shell of its own shows.
            tracing::warn!(%error, "the installed shell does not load; the built-in one stands in");
            Ok((tilo_surface_host::shell_part::compile_shell()?, false))
        }
    }
}

/// The device's resources the session publishes, as their surfaces: the sound, the network,
/// Bluetooth, the battery and the displays, each its reading and the actions it offers. The
/// shell's views read them as `so.tilo.device` (`device-surface.json` beside this crate, which
/// the shell compiles against and a test holds to this).
#[must_use]
pub fn device_surfaces() -> Vec<tilo_actions::surface::Surface> {
    vec![
        tilo_audio_service::sound::surface(),
        tilo_connectivity_service::network_surface(),
        tilo_connectivity_service::bluetooth_surface(),
        tilo_session_power::battery_surface(),
        tilo_system_service::power_surface(),
        crate::displays::surface(),
        crate::desk_resource::surface(),
    ]
}

/// The height of the bar's band, logical pixels: what a bar or a dock that shows only at an edge
/// takes, so the screen's views can tell the pointer is at that edge.
fn band() -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a bar's height, a few dozen pixels"
    )]
    let band = f64::from(tilo_desktop::topbar::topbar_height(&ui::Theme::dark())).ceil() as u32;
    band
}

/// The shell on the session, and what feeds it.
pub(crate) struct Shell {
    desk: Desk,
    /// The due timer ids announced already, so an unchanged one is announced once.
    announced: std::cell::RefCell<std::collections::BTreeSet<String>>,
    link: Rc<SessionLink>,
    /// What the desk's reading was last made from.
    seen: Option<crate::desk_resource::DeskKey>,
    /// The services' revision and the arrangement the places' readings were last followed at.
    followed: Option<(u64, ArrangementReading)>,
    /// The resources whose readings the shell holds for its places, by id, each with its type as
    /// the shell's runtime names another app's (`<app>#<Type>`): what a place names, and what the
    /// views composed there read.
    reading: std::collections::BTreeMap<String, String>,
    /// How often each was read for the shell.
    reads: std::collections::BTreeMap<String, u64>,
    /// The apps whose packages a view composed in a place read a type of before it was loaded, by
    /// app id: published with the packages the places show, and kept for the rest of the session (a
    /// view keeps drawing what it read once the card is there).
    needed: std::collections::BTreeSet<String>,
    /// How many of the session's turns read some of them: what the views composed in places waited
    /// on comes in one turn for everything one run of them read, not one per resource.
    rounds: u64,
    /// Lent last to every part, by the workspace in front and the media it was opened over: a
    /// part told the same one again keeps the store it has.
    lent_content: Option<(String, Option<ui::content::ContentStore>)>,
    /// The shell's record, shared by every part: one display draws what another wrote, so a change
    /// any part commits is the whole shell's.
    record: Rc<RefCell<tilo_resource::Doc>>,
    /// One part per output, in the order the outputs are known: each stands on its own display, and
    /// a display added (or taken away) has its part made (or dropped) to follow.
    parts: Vec<(String, Rc<RefCell<ShellPart>>)>,
    /// The packages the shell's parts are made from, kept to make each new display's part.
    packages: Vec<tilolang_tilo::tilolang::Compiled>,
    app: String,
    person: String,
    /// The height of the bar's band, logical pixels: a window reaching into the top band covers a
    /// bar there.
    band: u32,
    /// The wells the screen's views report, which the desktop places the windows at.
    wells: Option<ui::widget::Wells>,
    /// The services' revision the platform readings were last read at: they are read again only
    /// when it moved.
    services_seen: Option<u64>,
    /// The theme the person's appearance settings make: every part draws in it, and a part made for
    /// a display plugged in later starts in it.
    theme: ui::Theme,
}

impl Shell {
    /// The shell over every output of `compositor` , published on `doors` . The screen's views
    /// report their wells to `wells` , the handle the window policy places every window by
    /// ([`tilo_surface_host::WellPolicy`]).
    pub(crate) fn mount(
        compositor: &mut Compositor,
        doors: &mut Doors,
        desk: Desk,
        wells: Option<ui::widget::Wells>,
        theme: ui::Theme,
    ) -> Result<Self, String> {
        let packages = compile()?;
        let app = packages
            .last()
            .map(|compiled| compiled.meta.app.clone())
            .ok_or("the shell package did not compile")?;
        let link = Rc::new(SessionLink::default());
        let mut shell = Self {
            desk,
            announced: std::cell::RefCell::new(std::collections::BTreeSet::new()),
            link,
            seen: None,
            followed: None,
            reading: std::collections::BTreeMap::new(),
            reads: std::collections::BTreeMap::new(),
            needed: std::collections::BTreeSet::new(),
            rounds: 0,
            record: Rc::new(RefCell::new(tilo_resource::Doc::new())),
            parts: Vec::new(),
            lent_content: None,
            packages,
            app,
            person: String::new(),
            band: band(),
            wells,
            services_seen: None,
            theme,
        };
        shell.person = shell.desk.person.clone();
        shell.follow_outputs(compositor, doors);
        // The desk styles every installed package offers, before the first frame draws the
        // switch: a shell that starts already knows what its person may pick (shell.md §6).
        shell.refresh_styles();
        shell.publish();
        Ok(shell)
    }

    /// A timer Clocks says came due that the bar has not announced: its id, and what to say in the
    /// app's own words. Each id is announced once.
    pub(crate) fn new_due_timer(&self) -> Option<(String, String, String)> {
        let (id, title, body) = self.desk.due_timer()?;
        let mut announced = self.announced.borrow_mut();
        announced.insert(id.clone()).then_some((id, title, body))
    }

    /// Make a part for each output `compositor` shows that has none, stand each part on its
    /// output's rectangle, and stand the parts of outputs it no longer shows nowhere: one part per
    /// display, following a display plugged in, moved or taken away. A part whose display went
    /// keeps its place with the compositor and the door, drawing and offering nothing, and stands
    /// again when that display comes back, so unplugging and replugging makes no new part. Cheap
    /// when nothing changed, which is what every frame runs.
    fn follow_outputs(&mut self, compositor: &mut Compositor, doors: &mut Doors) {
        let outputs = displays::placed(&compositor.outputs());
        for (name, part) in &self.parts {
            if !outputs.iter().any(|shown| &shown.connector == name) {
                part.borrow_mut().set_display(None, 1.0);
            }
        }
        let fullscreen = compositor.fullscreen_outputs();
        for output in &outputs {
            if let Some((_, part)) = self
                .parts
                .iter()
                .find(|(shown, _)| *shown == output.connector)
            {
                part.borrow_mut().set_display(
                    Some((output.connector.clone(), output.part_rect())),
                    output.scale,
                );
                part.borrow_mut().set_fullscreen(fullscreen.clone());
                continue;
            }
            self.mount_part(
                compositor,
                doors,
                &output.connector,
                output.part_rect(),
                output.scale,
                &fullscreen,
            );
        }
    }

    /// Make the shell's part on `output` (its connector name and logical rectangle), sharing the
    /// shell's record with every other part, and hand it to `compositor` and its doors.
    fn mount_part(
        &mut self,
        compositor: &mut Compositor,
        doors: &mut Doors,
        output: &str,
        rect: (i32, i32, u32, u32),
        scale: f64,
        fullscreen: &[String],
    ) {
        let wake = compositor.waker();
        let mut part = ShellPart::new(
            tilo_surface_host::shell_part::ShellIdentity {
                app: &self.app,
                packages: self.packages.clone(),
                person: &self.person,
                link: Rc::clone(&self.link),
                record: Rc::clone(&self.record),
                // The first part of the shell makes the shell's record and seeds it; every other
                // display's part draws the one the shell already has.
                seed: self.parts.is_empty(),
            },
            scale,
            std::sync::Arc::new(move || wake.wake_by_ref()),
            self.wells.clone(),
        );
        part.set_theme(self.theme.clone());
        part.set_display(Some((output.to_owned(), rect)), scale);
        part.set_fullscreen(fullscreen.to_vec());
        // The windows the views picture, each display's own: the reading's window keys stand for
        // the tabs of the workspace in front, and a window whose app holds secrets is left out, so
        // its view shows the app's mark instead of its pixels.
        let handle = self.desk.handle.clone();
        part.show_pictures(
            ui::widget::Previews::new(),
            Rc::new(move |key: &str| pictured(&handle, key)),
        );
        let part = Rc::new(RefCell::new(part));
        compositor.add_host(Box::new(Held(Rc::clone(&part))));
        // Each display's part is its own surface on the door: what the person, a script or an agent
        // drives the shell through, and what reads its labels.
        doors.add(Rc::clone(&part) as tilo_surface_host::Shared);
        self.parts.push((output.to_owned(), part));
    }

    /// Hand the shell the desk when what it is made from changed, and what the screen says of its
    /// edges either way: a bar or dock that hides until reached for follows the pointer and the
    /// windows, which move without the desk changing. Whether anything was published.
    pub(crate) fn publish(&mut self) -> bool {
        let key = self.desk.key();
        let was = self.seen.replace(key.clone());
        let desk_changed = was.as_ref() != Some(&key);
        if desk_changed && !self.link.publish(self.desk.reading()) {
            return false;
        }
        // What the desk has to draw at an instant of its own (the switcher's panel coming up while
        // Alt is held) wakes every part then, so nothing draws or polls in between.
        let deadline = self.desk.next_deadline();
        for (_, part) in &self.parts {
            part.borrow_mut().set_deadline(deadline);
        }
        desk_changed
    }

    /// Tell the shell what the screen says of its edges: the pointer at the top or bottom edge,
    /// and a window standing where a bar or a dock there rides. What a bar or dock that shows
    /// only sometimes follows; a screen that says the same as before publishes nothing.
    pub(crate) fn publish_edges(&self, compositor: &Compositor) {
        let Some(outputs) = Self::outputs(compositor) else {
            return;
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a band's height, a few dozen pixels"
        )]
        let band = f64::from(self.band);
        let windows: Vec<tilo_surface_host::shell_part::WindowRect> = compositor
            .windows()
            .into_iter()
            .map(
                |(_, (x, y, w, h))| tilo_surface_host::shell_part::WindowRect {
                    x: f64::from(x),
                    y: f64::from(y),
                    width: f64::from(w),
                    height: f64::from(h),
                },
            )
            .collect();
        let edges = tilo_surface_host::shell_part::screen_edges(
            outputs,
            band,
            compositor.pointer_location(),
            &windows,
        );
        self.link.publish_edges(edges);
    }

    /// Follow the displays as they come and go, and a window taking one for itself: a part is made
    /// or dropped per output, and a part on a display a fullscreen window owns steps aside. Cheap
    /// when nothing changed, which is what every frame runs.
    pub(crate) fn follow_displays(&mut self, compositor: &mut Compositor, doors: &mut Doors) {
        self.follow_outputs(compositor, doors);
    }

    /// The outputs' bounding box `(x, y, w, h)` , as a screen-edge reading names them: every output
    /// that is on.
    fn outputs(compositor: &Compositor) -> Option<(i32, i32, u32, u32)> {
        let (x0, y0, x1, y1) = compositor
            .outputs()
            .iter()
            .filter_map(|output| output.rect)
            .map(|(x, y, w, h)| (x, y, x.saturating_add(w), y.saturating_add(h)))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
        let width = u32::try_from(x1.saturating_sub(x0)).ok()?;
        let height = u32::try_from(y1.saturating_sub(y0)).ok()?;
        Some((x0, y0, width.max(1), height.max(1)))
    }

    /// Hand the shell the readings of the platform's resources its views read (the sound, the
    /// battery, the appearance setting), as `services` publish them, whenever what they publish may
    /// have moved.
    pub(crate) fn publish_readings(&mut self, services: &Services) {
        let revision = services.revision();
        if self.services_seen.replace(revision) == Some(revision) {
            return;
        }
        // A resource no service of this device serves (no Bluetooth, no battery) is published
        // empty: the device has none of it, so a view reads its fields as absent (off, empty) and
        // shows the device without it.
        for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
            let reading = services
                .read(resource)
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
            self.link.publish_reading(resource, &ty, reading);
        }
    }

    /// Hand the shell what its places show, from what `services` publish: the package of each app a
    /// place shows a resource of, so the place draws it through that app's own views (and none of
    /// an app no place shows); the reading of each resource a place names; and the reading of each
    /// resource those views read, as they read it (an item of a collection a reading lists by id).
    /// Each reading is read again only when its service says it moved, so a change costs the shell
    /// only the resources it touched.
    pub(crate) fn follow_places(
        &mut self,
        arrangement: &ArrangementReading,
        services: &mut Services,
    ) {
        let wanted = self.link.take_wanted();
        // A view composed in a place read a resource of a type no loaded package declared: its
        // app's package is published like those a place shows, so the place draws it through that
        // app's own views.
        let mut asked = false;
        for app in self.link.take_wanted_packages().into_keys() {
            asked |= self.needed.insert(app);
        }
        let key = (services.revision(), arrangement.clone());
        let moved_on = asked || self.followed.as_ref() != Some(&key);
        if !moved_on && wanted.is_empty() {
            return;
        }
        let mut fresh: Vec<(String, String)> = Vec::new();
        if asked || self.followed.as_ref().map(|(_, was)| was) != Some(arrangement) {
            // Every app a place shows is published with this pass, however this pass was asked for:
            // a package a view asked for arrives beside them, and none of them goes with it.
            let resources = services.resources();
            let placed =
                tilo_surface_host::shell_part::readings::placed_records(arrangement, &resources);
            let mut packages = std::collections::BTreeMap::new();
            let app_of = tilo_surface_host::shell_part::readings::app_of;
            let mut apps: std::collections::BTreeSet<&str> =
                self.needed.iter().map(String::as_str).collect();
            apps.extend(placed.values().filter_map(|ty| app_of(ty)));
            for app in apps {
                if let Some(module) = services.module(app) {
                    packages.insert(app.to_owned(), module);
                }
            }
            // A resource of an app no package is published of any more is followed no more, and
            // neither is one of an app whose views draw it as a Rust app when none of its views is
            // linked here any more.
            let rust_views = tilo_surface_host::shell_part::readings::rust_view_apps();
            self.reading.retain(|_, ty| {
                app_of(ty).is_some_and(|app| {
                    packages.contains_key(app) || rust_views.iter().any(|held| held == app)
                })
            });
            self.link.publish_packages(packages);
            // The verbs of the apps the shell's runtime does not load — Chat's channel and its
            // `post`, which the session's own service answers — so a place's view sends its
            // action to the app publishing it, as it sends any package's to that package's door.
            self.link.publish_published_actions(
                tilo_surface_host::shell_part::readings::published_actions(
                    &resources,
                    &self.link.packages_loaded(),
                ),
            );
            for (id, ty) in placed {
                if !self.reading.contains_key(&id) {
                    fresh.push((id.clone(), ty.clone()));
                }
                self.reading.insert(id, ty);
            }
        }
        for (id, ty) in wanted {
            let app = tilo_surface_host::shell_part::readings::app_of(&ty);
            let shown = app.is_some_and(|app| {
                self.link.package_apps().iter().any(|held| held == app)
                    || tilo_surface_host::shell_part::readings::rust_view_apps()
                        .iter()
                        .any(|held| held == app)
            });
            if shown && !self.reading.contains_key(&id) {
                self.reading.insert(id.clone(), ty.clone());
                fresh.push((id, ty));
            }
        }
        if moved_on {
            let followed: std::collections::BTreeSet<String> =
                self.reading.keys().cloned().collect();
            for id in services.moved_among(&followed) {
                if let Some(ty) = self.reading.get(&id)
                    && !fresh.iter().any(|(was, _)| *was == id)
                {
                    fresh.push((id.clone(), ty.clone()));
                }
            }
        }
        if !fresh.is_empty() {
            self.rounds = self.rounds.saturating_add(1);
        }
        for (id, ty) in fresh {
            let reads = self.reads.entry(id.clone()).or_default();
            *reads = reads.saturating_add(1);
            if let Some(reading) = services.read(&id) {
                self.link.publish_reading(&id, &ty, reading);
            }
        }
        self.followed = Some(key);
    }

    /// How often the shell read each resource its places show, by id, since it started: what a test
    /// proves a change re-reads only what it touched by.
    pub(crate) fn reads(&self) -> std::collections::BTreeMap<String, u64> {
        self.reads.clone()
    }

    /// How many of the session's turns read readings for the shell's places.
    pub(crate) const fn reading_rounds(&self) -> u64 {
        self.rounds
    }

    /// The apps whose packages the shell's runtime holds loaded now, because a place shows them:
    /// every display's part loads the same.
    pub(crate) fn loaded_apps(&self) -> Vec<String> {
        self.parts
            .first()
            .map(|(_, part)| part.borrow().loaded_apps())
            .unwrap_or_default()
    }

    /// Hand the shell the person's arrangement, which its views draw, whenever it changed.
    pub(crate) fn publish_arrangement(&self, reading: ArrangementReading) {
        self.link.publish_arrangement(reading);
    }

    /// Read the desk styles every installed package offers, when the person's package store
    /// moved: a style the person or their agent installed is offered by the switch without a
    /// restart (`shell.md` §6). The shell's own package's styles — the four that ship — always
    /// stand, from the package the shell runs, so the switch offers them even where the store
    /// holds nothing (the built-in fallback shell). Cheap when nothing was installed, which is
    /// what every frame runs.
    pub(crate) fn refresh_styles(&mut self) {
        // The shell's own styles, from the package it runs: the four that ship are its views.
        let own = crate::desk_styles::of_shell(&self.packages);
        let Ok(catalog) = package_store::Catalog::from_env() else {
            let mut styles = self.desk.styles.borrow_mut();
            styles.list = own;
            styles.revision = styles.revision.saturating_add(1);
            return;
        };
        let moved = crate::desk_styles::fingerprint(&catalog);
        let mut styles = self.desk.styles.borrow_mut();
        if styles.fingerprint == Some(moved.clone()) {
            return;
        }
        styles.fingerprint = Some(moved);
        // The store's styles, the shell's own among them when the store holds the shell: each
        // package's styles are offered once, the shell's own from the package the shell runs.
        let mut list = crate::desk_styles::installed(&catalog);
        for style in own {
            if !list.iter().any(|s| s.style == style.style) {
                list.push(style);
            }
        }
        styles.list = list;
        styles.revision = styles.revision.saturating_add(1);
    }

    /// The link the session publishes the desk on and the shell asks through: what a test stages a
    /// reading on.
    pub(crate) fn link(&self) -> Rc<SessionLink> {
        Rc::clone(&self.link)
    }

    /// The surface the desk shows over its windows, by the name the views read (`d.here`): `none`
    /// at rest.
    pub(crate) fn surface(&self) -> String {
        self.link.surface()
    }

    /// The windows the shell's views picture, each with the rect its picture rests at, logical
    /// `(x, y, width, height)` : what the compositor draws of them, its own buffers scaled into the
    /// place a view gives them. Empty while no view pictures a window.
    pub(crate) fn pictures(&self) -> Vec<(u64, (f64, f64, f64, f64))> {
        self.parts
            .iter()
            .flat_map(|(_, part)| {
                part.borrow()
                    .window_pictures()
                    .into_iter()
                    .map(|picture| (picture.window, picture.rect))
            })
            .collect()
    }

    /// Whether the shell asked something of the door the session has not run yet.
    pub(crate) fn has_asks(&self) -> bool {
        self.link.has_asks()
    }

    /// The shell's own record (`root shell: Shell`, `root viewing: Viewing` ,
    /// `root arrangement: Arrangement` ), as the shell's parts keep it: the document every part
    /// commits into and draws from, shared by them. It exists as soon as the shell does, before any
    /// display's part is made.
    pub(crate) fn records(&self) -> Rc<RefCell<tilo_resource::Doc>> {
        Rc::clone(&self.record)
    }

    /// The shell's package as the session runs it, its compiled module and description: what its
    /// records are published by, the way an app's are.
    pub(crate) fn package(&self) -> Option<tilolang_tilo::tilolang::Compiled> {
        self.packages.last().cloned()
    }

    /// Publish the shell's own records with the apps' records `services` keeps: the package is
    /// loaded over the document its parts draw from, so a REPL or an agent reads and changes the
    /// shell's records by its app, as it does Notes'. The session runs the shell itself, and its
    /// records are the person's, synced with their devices, as an app's are.
    pub(crate) fn share_records(
        &self,
        services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
    ) {
        let Some(compiled) = self.package() else {
            return;
        };
        let records = self.records();
        let taken = services.borrow_mut().share(&tilo_service::Share {
            app: &compiled.meta.app,
            wasm: &compiled.wasm,
            records: &records,
        });
        if let Err(error) = taken {
            tracing::error!(%error, "the shell's records are not published");
        }
    }

    /// Run what the shell asked through `invoke` (the session's door).
    /// The shell's calls of the session's actions (`session.close_place(place: 3)`) reach the door
    /// with every argument as text, so each is typed here by the type the action itself declares
    /// for it: a workspace's key is text even when it reads as digits, and a count is a number.
    /// `types` answers with the declared type of `(resource, action)` 's argument `name` , when the
    /// door publishes it.
    pub(crate) fn serve(
        &self,
        types: impl Fn(&str, &str, &str) -> Option<tilo_actions::params::ParamType>,
        invoke: impl FnMut(&str, &str, &Map<String, Value>) -> Result<(), String>,
    ) {
        let mut invoke = invoke;
        self.link.serve(|ask: &Ask| {
            let mut arguments: Map<String, Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| {
                    let ty = types(&ask.resource, &ask.action, name);
                    (name.clone(), typed(value, ty))
                })
                .collect();
            // What the invocation is made from goes with its arguments: the app that takes it
            // records what it makes as made from it.
            if let Some(from) = &ask.from {
                arguments.insert(
                    tilo_actions::params::FROM_ARGUMENT.to_owned(),
                    from.to_json(),
                );
            }
            invoke(&ask.resource, &ask.action, &arguments)
        });
    }

    /// Run `setting` of the shell's own record, as its settings' controls do.
    pub(crate) fn run_setting(&self, setting: &str, arguments: Vec<Value>) -> Result<(), String> {
        let (_, part) = self
            .parts
            .first()
            .ok_or_else(|| "the shell has no display".to_owned())?;
        part.borrow_mut().run_setting(setting, arguments)
    }

    /// How much work the shell did since the session started, over every display: its view turns
    /// and frames summed, since a part on each display does the same work on each.
    pub(crate) fn work(&self) -> ShellWork {
        let mut work = ShellWork::default();
        let mut seen_event = false;
        for (_, part) in &self.parts {
            let part = part.borrow().work();
            work.turns = work.turns.saturating_add(part.turns);
            work.drawn = work.drawn.saturating_add(part.drawn);
            work.turning = work.turning.saturating_add(part.turning);
            work.drawing = work.drawing.saturating_add(part.drawing);
            work.updating = work.updating.saturating_add(part.updating);
            work.committing = work.committing.saturating_add(part.committing);
            work.receiving = work.receiving.saturating_add(part.receiving);
            work.laying_out = work.laying_out.saturating_add(part.laying_out);
            work.syncing = work.syncing.saturating_add(part.syncing);
            // The effects handed and served are the link's, shared by every part: the last part's
            // reading holds the whole count, so it is not summed.
            if !seen_event {
                work.handed = part.handed;
                work.served = part.served;
                seen_event = true;
            }
        }
        work
    }

    /// Draw every part in `theme` : the person changed their appearance.
    pub(crate) fn set_theme(&mut self, theme: &ui::Theme) {
        self.theme = theme.clone();
        for (_, part) in &self.parts {
            part.borrow_mut().set_theme(theme.clone());
        }
    }

    /// What the shell refused since the session started, in its words: every display's, and what
    /// the door refused of what it asked.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.parts
            .iter()
            .flat_map(|(_, part)| part.borrow().refusals().to_vec())
            .chain(self.link.refused())
            .collect()
    }

    /// Where the shell's control named `name` stands, logical `(x, y, w, h)` : the first part that
    /// shows it, since a name is the shell's one control and every display carries the same.
    pub(crate) fn control(&self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .find_map(|(_, part)| part.borrow_mut().control_bounds(name))
    }

    /// Where every shown part lays out the control `name` : a strip's row is a chip on the bar and
    /// an item of a place's move menu, and both are pressed.
    pub(crate) fn controls(&self, name: &str) -> Vec<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .flat_map(|(_, part)| part.borrow_mut().control_bounds_all(name))
            .collect()
    }

    /// Whether a shown part lays out `text` for assistive technology.
    pub(crate) fn says(&self, text: &str) -> bool {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .any(|(_, part)| part.borrow_mut().says(text))
    }

    /// Where a shown part lays out `text` for assistive technology.
    pub(crate) fn text_bounds(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .find_map(|(_, part)| part.borrow_mut().text_bounds(text))
    }

    /// Where every shown part lays out `text` for assistive technology: every node of every part
    /// whose label, description or value holds it.
    pub(crate) fn text_bounds_all(&self, text: &str) -> Vec<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .flat_map(|(_, part)| part.borrow_mut().text_bounds_all(text))
            .collect()
    }

    /// Where each display's part stands now, by connector name: what a test reads to prove a part
    /// on every display, and none on one a fullscreen window owns.
    pub(crate) fn parts(&self) -> Vec<PartPlace> {
        self.parts
            .iter()
            .filter_map(|(name, part)| {
                let part = part.borrow();
                let (_, rect) = part.placement()?;
                Some(PartPlace {
                    output: name.clone(),
                    rect,
                    scale: part.part_scale(),
                    aside: part.aside(),
                })
            })
            .collect()
    }
}

/// A display's part of the shell, as it stands now: the output it is on, where it stands in the
/// logical layout, the scale it draws at, and whether it stepped aside for a fullscreen window
/// there.
#[derive(Clone, Debug, PartialEq)]
pub struct PartPlace {
    /// The output's connector name (`HEADLESS-1`).
    pub output: String,
    /// Where the part stands, logical `(x, y, w, h)` .
    pub rect: (i32, i32, u32, u32),
    /// The scale the part draws at: its display's, so what it paints is as crisp as the panel.
    pub scale: f64,
    /// A fullscreen window owns the output, so the part draws nothing there.
    pub aside: bool,
}

/// Lend `shell`'s parts the content its places draw from: the document media this session serves
/// its apps (`TILO_FS_BLOB_SOCKET`, `fs serve-media`), read as the session itself, with
/// `workspaces` — the workspaces the desk holds — among the scopes it serves, the one in front
/// first.
///
/// A place shows a record an app's connector kept stills for (`tilo-blob://<hash>`), and the
/// parts run in this process, which the launcher handed no capability of its own: without this
/// every still draws as its placeholder, though the bytes are here
/// (`docs/architecture/apps.md` §1, §8).
///
/// The store is opened again only when the workspace in front changed: every other pass keeps
/// the one the parts have, so a picture drawn every frame is not opened again every frame.
fn lend_content(shell: &mut Shell, shared: &crate::Shared, workspaces: Vec<String>) {
    if workspaces.is_empty() {
        return;
    }
    let active = shared.active_workspace_id();
    let active = if active.is_empty() {
        workspaces.first().cloned().unwrap_or_default()
    } else {
        active
    };
    // One media store, the workspace in front first and every other beside it: a place of any
    // workspace reads its stills through it, and a still is read through the workspace that
    // references it.
    let mut ordered = vec![active.clone()];
    ordered.extend(
        workspaces
            .into_iter()
            .filter(|workspace| *workspace != active),
    );
    ui::content::set_session_workspaces(ordered);
    // The store is opened again only when the workspace in front changed; every part is told it
    // every pass, so a display whose part was made since reads the same content as the others
    // (and a part told the one it has keeps it).
    let content = match shell.lent_content.take() {
        Some((lent, content)) if lent == active => content,
        _ => ui::content::session(&active),
    };
    for (_, part) in &shell.parts {
        part.borrow_mut().lend_content(&active, content.clone());
    }
    shell.lent_content = Some((active, content));
}

/// The compositor's window for the reading's window `key` (its tab's id), when it may be pictured:
/// this device draws the window and its app shows no secrets. A window this device does not draw,
/// or one whose app holds secrets, is pictured by nothing, and its view shows the app's mark
/// instead.
fn pictured(handle: &tilo_desktop_surface::DesktopHandle, key: &str) -> Option<u64> {
    use tilo_desktop_model::TileContent;
    let (app, surface) = handle.read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| tab.id.to_string() == key)
            .and_then(|tab| match &tab.tile {
                TileContent::Foreign { app_id, surface_id } => Some((app_id.clone(), *surface_id)),
                _ => None,
            })
    })?;
    (!handle.withholds(&app)).then_some(surface)
}

/// `value` (the text the shell's effect wrote) as the door's arguments take it, in the type the
/// action declares for that argument. Text stays text however it reads: a workspace's key is
/// `"281474976710657"` , not a number. A flag is written as the integer a tilolang flag travels as
/// (`1`, `0` ), and reads as true and false. An undeclared argument keeps the text it came as, and
/// the door refuses it by name.
fn typed(value: &str, ty: Option<tilo_actions::params::ParamType>) -> Value {
    use tilo_actions::params::ParamType;
    match ty {
        Some(ParamType::Integer) => value
            .trim()
            .parse::<i64>()
            .map_or_else(|_| Value::String(value.to_owned()), Value::from),
        // ignored-failure: a number that does not parse is sent as the text
        // the person typed, and the door refuses it by name
        Some(ParamType::Number) => value
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(value.to_owned()), Value::Number),
        Some(ParamType::Boolean) => match value.trim() {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            _ => Value::String(value.to_owned()),
        },
        Some(ParamType::Object) => serde_json::from_str::<Map<String, Value>>(value)
            .map_or_else(|_| Value::String(value.to_owned()), Value::Object),
        Some(ParamType::Array) => serde_json::from_str::<Vec<Value>>(value)
            .map_or_else(|_| Value::String(value.to_owned()), Value::Array),
        Some(ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret)
        | None => Value::String(value.to_owned()),
    }
}

/// `host` , the desktop's bar or desk, as the desk stands under the shell: the shell draws the bar
/// and the desk itself (its strips, their frames and the places in them, around the wells the
/// windows stand in), so the desktop's own step aside ([`Aside`]) rather than drawing a second set
/// of either. It still keeps the windows' model the wells place them by. Without a shell, the
/// desktop is the shell and stands as itself.
pub(crate) fn under(shell: bool, host: impl SurfaceHost + 'static) -> Box<dyn SurfaceHost> {
    if shell {
        Box::new(Aside(host))
    } else {
        Box::new(host)
    }
}

/// The chrome's published side as the desk stands under the shell: the shell draws the surfaces, so
/// each action of the chrome's that opens one opens the shell's view of it on `surfaces` — the
/// overview, the notifications, the quick settings and the workspaces — while the chrome's own
/// draws nothing. Every other action is the chrome's.
pub(crate) fn chrome_door<T: tilo_surface_host::Published + 'static>(
    shell: bool,
    chrome: &Rc<RefCell<T>>,
    surfaces: crate::desk_resource::Surfaces,
) -> tilo_surface_host::Shared {
    let chrome = Rc::clone(chrome) as tilo_surface_host::Shared;
    if shell {
        Rc::new(RefCell::new(ShellChrome { chrome, surfaces }))
    } else {
        chrome
    }
}

/// The desk's published side as the desk stands under the shell: its `search` opens the shell's
/// launcher (on what the person asked for, when a query comes with it), as the desktop's own search
/// draws nothing under the shell; a search with a query still runs, its rows published for the
/// palette and the agents.
pub(crate) fn desk_door<T: tilo_surface_host::Published + 'static>(
    shell: bool,
    desk: T,
    surfaces: crate::desk_resource::Surfaces,
) -> tilo_surface_host::Shared {
    let desk: tilo_surface_host::Shared = Rc::new(RefCell::new(desk));
    if shell {
        Rc::new(RefCell::new(ShellDesk { desk, surfaces }))
    } else {
        desk
    }
}

/// The chrome's door on the shell ([`chrome_door`]).
struct ShellChrome {
    chrome: tilo_surface_host::Shared,
    surfaces: crate::desk_resource::Surfaces,
}

impl ShellChrome {
    /// The surface `address` opens or closes on the shell, when it is one of the chrome's ways of
    /// showing one: `Ok(None)` for every other action.
    fn show_surface(
        &self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, tilo_actions::DoorError> {
        if address.resource != tilo_desktop::desk_verbs::CHROME_RESOURCE {
            return Ok(None);
        }
        let here = |name: &str| self.surfaces.overlays.here() == name;
        let started = match address.action.as_str() {
            "overview" => {
                let open = here(crate::overlays::Overlay::Overview.name());
                if opens(address, arguments, open)? {
                    self.surfaces
                        .open(crate::overlays::Overlay::Overview.name());
                } else {
                    self.surfaces
                        .close(crate::overlays::Overlay::Overview.name());
                }
                true
            }
            "notifications" => {
                let name = crate::overlays::Overlay::Centre.name();
                if opens(address, arguments, here(name))? {
                    self.surfaces.open(name);
                } else {
                    self.surfaces.close(name);
                }
                true
            }
            "quick-settings" => {
                self.surfaces
                    .toggle(crate::overlays::Overlay::QuickSettings.name());
                true
            }
            // The workspaces are the shell's one workspace surface: the desk's menu, the workspaces
            // a person switches between and the actions on them.
            "workspace-menu" => {
                self.surfaces
                    .toggle(crate::overlays::Overlay::Workspaces.name());
                true
            }
            "shortcut"
                if arguments.get("action").cloned().and_then(|action| {
                    // ignored-failure: an action that does not read back is not this one
                    serde_json::from_value::<ui::shortcut_table::ShortcutAction>(action).ok()
                }) == Some(ui::shortcut_table::ShortcutAction::WorkspaceOverview) =>
            {
                let name = crate::overlays::Overlay::Overview.name();
                self.surfaces.toggle(name);
                true
            }
            _ => false,
        };
        Ok(
            started.then_some(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )),
        )
    }
}

/// Whether the chrome's action at `address` opens its surface, given whether it is up (`here`): an
/// action that names `open` says so, and one that does not toggles.
fn opens(
    address: &ActionAddress,
    arguments: &Map<String, Value>,
    here: bool,
) -> Result<bool, tilo_actions::DoorError> {
    match arguments.get("open") {
        None | Some(Value::Null) => Ok(!here),
        Some(Value::Bool(wanted)) => Ok(*wanted),
        Some(other) => Err(tilo_actions::DoorError::InvalidArguments(format!(
            "`{}` takes `open` as a boolean, got {other}",
            address.action
        ))),
    }
}

impl tilo_surface_host::Published for ShellChrome {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.chrome.borrow_mut().resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if let Some(started) = self.show_surface(address, arguments)? {
            return Ok(started);
        }
        self.chrome.borrow_mut().invoke(address, arguments, look)
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if let Some(started) = self.show_surface(address, arguments)? {
            return Ok(started);
        }
        self.chrome
            .borrow_mut()
            .invoke_as(actor, address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.chrome.borrow_mut().accessible_labels()
    }

    fn revision(&self) -> Option<u64> {
        self.chrome.borrow().revision()
    }

    fn offered(&self) -> bool {
        self.chrome.borrow().offered()
    }

    fn exclusive(&self) -> bool {
        self.chrome.borrow().exclusive()
    }

    fn serves_while_locked(&self) -> bool {
        self.chrome.borrow().serves_while_locked()
    }

    fn locked_resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.chrome.borrow_mut().locked_resources()
    }
}

/// The desk's door on the shell ([`desk_door`]).
struct ShellDesk {
    desk: tilo_surface_host::Shared,
    surfaces: crate::desk_resource::Surfaces,
}

impl tilo_surface_host::Published for ShellDesk {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.desk.borrow_mut().resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if address.resource == tilo_desktop::desk_verbs::DESK_RESOURCE && address.action == "search"
        {
            if let Some(Value::String(query)) = arguments.get("query")
                && !query.is_empty()
            {
                // The desk runs its search as it always does, so its rows are published on its
                // search resource for the palette and the agents; the shell's launcher is what
                // draws, and it opens on what was asked for.
                let started = self.desk.borrow_mut().invoke(address, arguments, look)?;
                self.surfaces.overlays.search(query);
                return Ok(started);
            }
            self.surfaces
                .toggle(crate::overlays::Overlay::Launcher.name());
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // The platform search door (a window's search control, its toolbar's magnifier, Ctrl+K)
        // asks for the desk's search under the shell too: the shell draws that search, so the
        // request opens the launcher, and its query runs the desk's search as the desk's own
        // `search` verb does, its rows published for whoever reads them.
        if ui::search::is_search_address(address) {
            let query = ui::search::SearchRequest::from_arguments(arguments)
                .query
                .filter(|query| !query.is_empty());
            let started = match &query {
                Some(query) => {
                    let asked =
                        Map::from_iter([("query".to_owned(), Value::String(query.clone()))]);
                    self.desk.borrow_mut().invoke(
                        &ActionAddress::new(tilo_desktop::desk_verbs::DESK_RESOURCE, "search"),
                        &asked,
                        look,
                    )?
                }
                None => ui::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            };
            match &query {
                Some(query) => self.surfaces.overlays.search(query),
                None => self
                    .surfaces
                    .toggle(crate::overlays::Overlay::Launcher.name()),
            }
            return Ok(started);
        }
        self.desk.borrow_mut().invoke(address, arguments, look)
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_surface_host::Look<'_>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if ui::search::is_search_address(address)
            || (address.resource == tilo_desktop::desk_verbs::DESK_RESOURCE
                && address.action == "search")
        {
            return self.invoke(address, arguments, look);
        }
        self.desk
            .borrow_mut()
            .invoke_as(actor, address, arguments, look)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        self.desk.borrow_mut().accessible_labels()
    }

    fn revision(&self) -> Option<u64> {
        self.desk.borrow().revision()
    }

    fn offered(&self) -> bool {
        self.desk.borrow().offered()
    }

    fn exclusive(&self) -> bool {
        self.desk.borrow().exclusive()
    }

    fn serves_while_locked(&self) -> bool {
        self.desk.borrow().serves_while_locked()
    }

    fn locked_resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.desk.borrow_mut().locked_resources()
    }
}

/// A surface of the session's own the shell stands in for: the desktop's bar, which the shell's bar
/// replaces, and the desktop's desk, whose strips the shell draws. It draws nothing and takes no
/// pointer and no key: every chord the person presses is the shell's to answer or the session's
/// own. What it holds still moves with what is asked of it ([`SurfaceHost::advance`]), so a
/// workspace switched or a window closed is the shell's to draw, and it still hears the outputs
/// change, so what it holds follows the displays the shell stands on. Left alone it asks for no
/// frame, as none of it shows.
pub(crate) struct Aside<T>(pub(crate) T);

impl<T: SurfaceHost> SurfaceHost for Aside<T> {
    fn layers(&self) -> Vec<HostLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn claim_key(&mut self, _key: &HostKey, _locked: bool, _inhibited: bool) -> KeyClaim {
        KeyClaim::Pass
    }

    fn key(&mut self, _key: &HostKey) {}

    fn outputs_changed(&mut self, outputs: &[tilo_compositor::HostedOutput]) {
        self.0.outputs_changed(outputs);
    }

    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }

    fn prepare_frame(&mut self) -> bool {
        // Nothing of it shows, but what it holds still moves with what the person and the session
        // command of it — a workspace switched, a window closed — so it takes up its own pass
        // without drawing any of it.
        self.0.advance()
    }
}

impl crate::Session {
    /// Hand the shell the desk when it changed, and run what it asked through the door, as any
    /// caller of the action protocol does.
    pub(crate) fn follow_shell(&mut self) -> Result<(), crate::Error> {
        let Some(shell) = self.shell.as_ref() else {
            return Ok(());
        };
        // The shell's views draw the centre over the desk, so while it is up the person is looking
        // at what the notices say: they are read, as they are while the panel's own centre shows
        // them. The bell's count, the centre's header and its rows read the one set, so they never
        // disagree. It is the same surface the views read (`d.here`), and the one place that knows
        // when it closes: however the person left it, the next arrival is a banner on the desk
        // again rather than a row behind a page that is gone.
        let centre = shell.surface() == crate::overlays::Overlay::Centre.name();
        self.shared.notices.set_shell_centre(centre);
        if centre {
            self.shared.notices.mark_read();
        }
        let Some(shell) = self.shell.as_mut() else {
            return Ok(());
        };
        if let Some((_, title, body)) = shell.new_due_timer() {
            self.shared
                .notices
                .post(tilo_desktop_surface::notifications::Notification::new(
                    title, body,
                ));
        }
        // The desk styles every installed package offers: read when the store moved, so a
        // style a person or their agent installed is offered without a restart (shell.md §6).
        // Read before the desk is published, so the reading carries the list.
        shell.refresh_styles();
        shell.publish();
        let arrangement = self.shared.arrangement.borrow().reading();
        shell.publish_readings(&self.shared.services.borrow());
        shell.follow_places(&arrangement, &mut self.shared.services.borrow_mut());
        shell.publish_arrangement(arrangement);
        // What the places draw reads its pictures from the document media of the workspaces
        // those places stand in: the parts draw in the session's own process, which the launcher
        // handed no store of its own, so the session lends them one (`docs/architecture/apps.md`
        // §1, the connector's stills, `fs serve-media`).
        {
            let workspaces: Vec<String> = self.shared.handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .map(|held| held.id().to_string())
                    .collect()
            });
            lend_content(shell, &self.shared, workspaces);
        }
        // What the screen says of its edges is what a bar or dock that shows only sometimes
        // follows; it moves without the desk, so it goes every turn.
        shell.publish_edges(&self.compositor);
        // One part per display: a display plugged in, taken away or taken by a fullscreen window
        // moves its part.
        shell.follow_displays(&mut self.compositor, &mut self.shared.doors);
        if !shell.has_asks() {
            return Ok(());
        }
        let doors = &self.shared.doors;
        let search = &self.shared.search;
        let workspace = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
                .unwrap_or_default()
        });
        let resources = doors.resources();
        shell.serve(
            |resource, action, name| {
                tilo_actions::door::resolve(&resources, &ActionAddress::new(resource, action))
                    .and_then(|spec| spec.params().iter().find(|param| param.name == name))
                    .map(|param| param.ty)
            },
            |resource, action, arguments| {
                // An app's own `open` (`session.open(app)`) starts it in the workspace in front, as
                // the launcher's row of it does.
                if let Some(app) = resource
                    .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                    .filter(|_| action == tilo_actions::remote::APPLICATION_OPEN_ACTION)
                {
                    return search
                        .perform_checked(
                            &workspace,
                            tilo_desktop_search::Target::Launch {
                                app: app.to_owned(),
                                place: tilo_desktop_search::LaunchPlace::Focus,
                            },
                            &mut |own| {
                                doors
                                    .invoke(
                                        &own.address,
                                        &own.arguments,
                                        &mut tilo_surface_host::Look::none(),
                                    )
                                    .map(drop)
                            },
                        )
                        .map_err(|error| error.to_string());
                }
                doors
                    .invoke(
                        &ActionAddress::new(resource, action),
                        arguments,
                        &mut tilo_surface_host::Look::none(),
                    )
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        );
        // What ran may have asked the session in turn (a menu, a sheet).
        self.settle()?;
        if let Some(shell) = self.shell.as_mut() {
            shell.publish();
        }
        // The desk's places are what the outside services keeping its records in step are due
        // for: each workspace with a place of an app's records has that app's connector, and one
        // that no longer shows any ends. Read every pass, so a place made, moved between
        // workspaces or closed is followed on the pass it happened on, in every workspace of
        // the desk, not only the one in front (`docs/architecture/apps.md` §1, §8).
        crate::mount::connectors::reconcile(self);
        Ok(())
    }

    /// What the shell reads the desk through: a test stages a reading on it, as a collaborating
    /// session's presence would arrive.
    #[must_use]
    pub fn shell_link(&self) -> Option<Rc<tilo_surface_host::SessionLink>> {
        self.shell.as_ref().map(Shell::link)
    }

    /// The collaboration plane this session takes part in, when it does: what a test hands it is
    /// who is here.
    #[must_use]
    pub fn sync(&self) -> Option<&Rc<crate::sync::Client>> {
        self.shared.sync.as_ref()
    }

    /// The notifications the desk keeps: what apps sent, and what the centre shows. A test posts
    /// into it as an app's arrival would land.
    #[must_use]
    pub fn notices(&self) -> &tilo_desktop_surface::DesktopNotifications {
        &self.shared.notices
    }

    /// How much work the shell's views did since the session started: `None` when its package did
    /// not start.
    #[must_use]
    pub fn shell_work(&self) -> Option<tilo_surface_host::ShellWork> {
        self.shell.as_ref().map(Shell::work)
    }

    /// What the shell refused since the session started, in its own words: a press whose action
    /// could not run.
    #[must_use]
    pub fn shell_refusals(&self) -> Vec<String> {
        self.shell.as_ref().map(Shell::refusals).unwrap_or_default()
    }

    /// Where the shell's control named `name` (as assistive technology hears it) stands, logical
    /// `(x, y, w, h)` : what a test presses it by.
    #[must_use]
    pub fn shell_control(&self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.control(name)
    }

    /// Where every control the shell shows by `name` stands, logical `(x, y, w, h)` : a menu item
    /// and a chip elsewhere may both be named for the row they name.
    #[must_use]
    pub fn shell_controls(&self, name: &str) -> Vec<(f64, f64, f64, f64)> {
        self.shell
            .as_ref()
            .map(|shell| shell.controls(name))
            .unwrap_or_default()
    }

    /// Whether the shell lays out `text` for assistive technology anywhere on screen: a label, a
    /// description or a value holding it. What a test reads a composed view by.
    #[must_use]
    pub fn shell_says(&self, text: &str) -> bool {
        self.shell.as_ref().is_some_and(|shell| shell.says(text))
    }

    /// Where the shell lays out `text` for assistive technology, logical `(x, y, w, h)` : the first
    /// node whose label, description or value holds it, on any shown part.
    #[must_use]
    pub fn shell_text_bounds(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.text_bounds(text)
    }

    /// Where every node the shell lays out that says `text` stands, logical `(x, y, w, h)`: every
    /// label, description or value holding it, on every shown part. A test asks what the whole
    /// desk says of a name, not what the first one of it says.
    #[must_use]
    pub fn shell_text_bounds_all(&self, text: &str) -> Vec<(f64, f64, f64, f64)> {
        self.shell
            .as_ref()
            .map_or_else(Vec::new, |shell| shell.text_bounds_all(text))
    }

    /// The apps whose packages the shell's runtime holds loaded, because a place shows them.
    #[must_use]
    pub fn shell_apps(&self) -> Vec<String> {
        self.shell
            .as_ref()
            .map(Shell::loaded_apps)
            .unwrap_or_default()
    }

    /// How many of the session's turns read readings for the shell's places: each hands the shell
    /// everything the views composed there read in one run of them.
    #[must_use]
    pub fn shell_reading_rounds(&self) -> u64 {
        self.shell.as_ref().map_or(0, Shell::reading_rounds)
    }

    /// How often the session read each resource the shell's places show, by id, for the shell: a
    /// reading is read again only when its service says it moved.
    #[must_use]
    pub fn shell_reads(&self) -> std::collections::BTreeMap<String, u64> {
        self.shell.as_ref().map(Shell::reads).unwrap_or_default()
    }

    /// The shell's parts, one per display, as each stands now: the output it is on, where it
    /// stands, and whether it stepped aside for a fullscreen window there.
    #[must_use]
    pub fn shell_parts(&self) -> Vec<PartPlace> {
        self.shell.as_ref().map(Shell::parts).unwrap_or_default()
    }

    /// Run `setting` of the shell's own record with `arguments`, as pressing its control in
    /// the shell's settings does: `set_bar_finish` with `["translucent"]` makes the bar the
    /// theme's glass and `["solid"]` its calm ground, and `apply` takes a whole preset.
    ///
    /// # Errors
    /// The shell does not run, it declares no such setting, an argument does not hold the
    /// type the setting declares, or the setting refused.
    pub fn run_shell_setting(&self, setting: &str, arguments: Vec<Value>) -> Result<(), String> {
        self.shell
            .as_ref()
            .ok_or_else(|| "the shell does not run".to_owned())?
            .run_setting(setting, arguments)
    }
}

#[cfg(test)]
mod tests {
    /// What the shell runs — its views and the session's and the platform's descriptions, with no
    /// scenarios, which are fixtures for its shots and tests — compiles, and every id it declares
    /// is in the lock. What it shows.
    #[test]
    fn the_shell_the_session_runs_compiles() {
        super::compile().unwrap_or_else(|why| panic!("{why}"));
    }

    /// `apps/tl/shell/app.ids` is the lock of the *whole* shell package, scenarios included:
    /// `tilolang check` (which layout-check and the shots compile the package with) holds, and a
    /// declaration the session's compile leaves out — a scenario's, its helpers' — keeps its id in
    /// the lock, because a lock names every declaration the package has ever had.
    #[test]
    fn the_shells_ids_lock_holds_every_declaration_the_whole_package_has() {
        use tilolang_tilo::tilolang::ids::IdsLock;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
        // The packages the shell's scenarios reach (`use so.tilo.lang.notes as notes`), each
        // compiled before the shell as `tilolang check` compiles it.
        let published = tilolang_tilo::catalogue::published_packages(&[dir
            .parent()
            .expect("apps/tl")
            .to_path_buf()])
        .unwrap_or_else(|why| panic!("{why}"));
        let built = tilolang_tilo::tilolang::source::compile_package(
            &dir,
            &[],
            &published,
            &mut tilolang_tilo::tilolang::ids::SeededEntropy(0),
        )
        .unwrap_or_else(|why| panic!("{why}"));
        let compiled = built.last().expect("the shell package compiles last");
        let lock = compiled.compiled.ids.to_text();
        let on_disk = std::fs::read_to_string(dir.join("app.ids")).expect("app.ids reads");
        assert_eq!(
            on_disk, lock,
            "apps/tl/shell/app.ids is not the lock the whole package (scenarios included) \
             compiles to; write:\n{lock}"
        );
        // Every scenario the package declares keeps the id the lock gives it, in the lock's live
        // entries and never among the retired: a scenario's id is as real as a type's.
        let parsed = IdsLock::parse(&on_disk).expect("the lock parses");
        let retired: std::collections::BTreeSet<(&str, &str)> = parsed.retired().collect();
        let scenarios = &compiled.compiled.meta.scenarios;
        assert!(
            scenarios.len() > 20,
            "the whole package's scenarios are compiled, as `tilolang check` compiles them: {}",
            scenarios.len()
        );
        for scenario in scenarios {
            let name = tilolang_tilo::tilolang::ids::scenario_key(&scenario.name);
            let held = parsed
                .get(&name)
                .unwrap_or_else(|| panic!("scenario {name:?} has no id in app.ids"));
            assert!(
                !retired.contains(&(name.as_str(), held)),
                "{name} is retired"
            );
        }
    }

    /// A host that counts what is asked of it, to show what the aside never asks.
    #[derive(Default)]
    struct Asked {
        frames: u32,
        keys: u32,
        outputs: u32,
    }

    impl tilo_compositor::SurfaceHost for Asked {
        fn layers(&self) -> Vec<tilo_compositor::HostLayer> {
            Vec::new()
        }

        fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
            true
        }

        fn pointer_motion(&mut self, _x: f64, _y: f64) {}

        fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

        fn claim_key(
            &mut self,
            _key: &tilo_compositor::HostKey,
            _locked: bool,
            _inhibited: bool,
        ) -> tilo_compositor::KeyClaim {
            self.keys = self.keys.saturating_add(1);
            tilo_compositor::KeyClaim::Take
        }

        fn key(&mut self, _key: &tilo_compositor::HostKey) {
            self.keys = self.keys.saturating_add(1);
        }

        fn outputs_changed(&mut self, _outputs: &[tilo_compositor::HostedOutput]) {
            self.outputs = self.outputs.saturating_add(1);
        }

        fn prepare_frame(&mut self) -> bool {
            self.frames = self.frames.saturating_add(1);
            true
        }
    }

    /// The desk the shell stands in for draws nothing, prepares nothing and takes no key or
    /// pointer: what it hears still reaches it, so it follows the displays the shell is on.
    #[test]
    fn the_desktop_aside_prepares_draws_and_takes_nothing() {
        use tilo_compositor::SurfaceHost as _;
        let key = tilo_compositor::HostKey {
            keycode: 0,
            pressed: true,
            repeat: false,
            keysym: 0xff1b,
            unmodified_keysym: 0xff1b,
            latin_keysym: 0xff1b,
            text: None,
            modifiers: tilo_compositor::KeyModifiers::default(),
        };
        let mut aside = super::Aside(Asked::default());
        assert!(!aside.prepare_frame(), "nothing to draw");
        assert_eq!(aside.0.frames, 0, "the desk behind it never prepared");
        assert_eq!(
            aside.claim_key(&key, false, false),
            tilo_compositor::KeyClaim::Pass,
            "the shell answers the keys"
        );
        aside.key(&key);
        assert_eq!(aside.0.keys, 0, "no key reached the desk behind it");
        assert!(aside.layers().is_empty(), "it draws nothing");
        assert!(!aside.accepts_pointer(1.0, 1.0), "it takes no pointer");
        aside.outputs_changed(&[tilo_compositor::HostedOutput {
            rect: (0, 0, 1280, 800),
            scale: 1.0,
        }]);
        assert_eq!(aside.0.outputs, 1, "it follows the displays");
    }

    /// The device's surface the shell compiles against is the one the session publishes.
    #[test]
    fn the_device_surface_file_is_what_the_session_publishes() {
        let published = super::device_surfaces();
        let committed = tilolang_tilo::platform_apps::device_surfaces()
            .unwrap_or_else(|why| panic!("device-surface.json does not read: {why}"));
        if committed != published {
            let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": published }))
                .expect("a surface writes as JSON");
            panic!(
                "crates/session/session/device-surface.json is not what the session publishes; write:\n{text}"
            );
        }
    }
}

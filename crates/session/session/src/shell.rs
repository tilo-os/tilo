//! The shell on the native session: the tilolang shell package
//! (`apps/tl/shell`) shown by a [`ShellPart`] per display — its top bar over
//! the desktop, or the whole screen under the windows, which stand in its
//! wells — kept in step with the session's desk (`crate::desk`) and asking the
//! session's door for what the person presses.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_compositor::Compositor;
use tilo_compositor::{CursorIcon, HostKey, HostLayer, KeyClaim, SurfaceHost};
use tilo_service::Services;
use tilo_surface_host::shell_part::{ArrangementReading, Ask, ShellLayer};
use tilo_surface_host::{Doors, SessionLink, ShellPart, ShellWork};

use crate::desk_resource::Desk;
use crate::panels::Held;

/// The shell package and the packages it uses, compiled, the shell last: what the shell's
/// instance runs.
///
/// # Errors
/// A package does not compile: its diagnostics.
pub fn compile() -> Result<Vec<tilolang_tilo::tilolang::Compiled>, String> {
    tilo_surface_host::shell_part::compile_shell()
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

/// The height of the band the chrome leaves the shell's bar, logical pixels.
fn band() -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a bar's height, a few dozen pixels"
    )]
    let band = f64::from(tilo_desktop::topbar::topbar_height(&ui::Theme::dark())).ceil() as u32;
    band
}

/// The shell record's edge settings, as the session reads them for the part's place.
struct EdgeSettings {
    bar_shown: bool,
    /// `top` or `bottom`; anything else (a leading or trailing bar, which the view does not
    /// place today) leaves the band to the desk.
    bar_edge: String,
    dock_shown: bool,
    dock_edge: String,
}

/// The shell's bar on the session, and what feeds it.
pub(crate) struct Shell {
    desk: Desk,
    /// The due timer ids announced already, so an unchanged one is announced once.
    announced: std::cell::RefCell<std::collections::BTreeSet<String>>,
    link: Rc<SessionLink>,
    /// What the desk's reading was last made from.
    seen: Option<crate::desk_resource::DeskKey>,
    /// The services' revision and the arrangement the places' readings were last followed at.
    followed: Option<(u64, ArrangementReading)>,
    /// The resources whose readings the shell holds for its places, by id, each with its type
    /// as the shell's runtime names another app's (`<app>#<Type>`): what a place names, and
    /// what the views composed there read.
    reading: std::collections::BTreeMap<String, String>,
    /// How often each was read for the shell.
    reads: std::collections::BTreeMap<String, u64>,
    /// How many of the session's turns read some of them: what the views composed in places
    /// waited on comes in one turn for everything one run of them read, not one per resource.
    rounds: u64,
    /// The shell's record, shared by every part: the bar draws it and every other display's
    /// bar draws the same.
    record: Rc<RefCell<tilo_resource::Doc>>,
    /// One part per output, in the order the outputs are known: each stands on its own
    /// display, and a display added (or taken away) has its part made (or dropped) to follow.
    parts: Vec<(String, Rc<RefCell<ShellPart>>)>,
    /// The packages the shell's parts are made from, kept to make each new display's part.
    packages: Vec<tilolang_tilo::tilolang::Compiled>,
    app: String,
    person: String,
    scale: f64,
    /// The band the chrome leaves the bar, logical pixels: a window reaching into it covers a
    /// bar there.
    band: u32,
    /// What each part stands as: the bar's band, or the whole screen.
    layer: ShellLayer,
    /// The wells the screen's views report, which the desktop places the windows at: `None`
    /// for the bar, which has no windows of its own.
    wells: Option<ui::widget::Wells>,
    /// The services' revision the platform readings were last read at: they are read again
    /// only when it moved.
    services_seen: Option<u64>,
    /// The theme the person's appearance settings make: every part draws in it, and a part
    /// made for a display plugged in later starts in it.
    theme: ui::Theme,
}

impl Shell {
    /// The shell over every output of `compositor` as `layer`, published on `doors`. The
    /// whole screen's views report their wells to `wells`, the handle the window policy
    /// places every window by ([`tilo_surface_host::WellPolicy`]).
    pub(crate) fn mount(
        compositor: &mut Compositor,
        doors: &mut Doors,
        desk: Desk,
        scale: f64,
        layer: ShellLayer,
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
            rounds: 0,
            record: Rc::new(RefCell::new(tilo_resource::Doc::new())),
            parts: Vec::new(),
            packages,
            app,
            person: String::new(),
            scale,
            band: band(),
            layer,
            wells,
            services_seen: None,
            theme,
        };
        shell.person = shell.desk.person.clone();
        shell.follow_outputs(compositor, doors);
        shell.publish();
        Ok(shell)
    }

    /// A timer Clocks says came due that the bar has not announced: its id, and what to say
    /// in the app's own words. Each id is announced once.
    pub(crate) fn new_due_timer(&self) -> Option<(String, String, String)> {
        let (id, title, body) = self.desk.due_timer()?;
        let mut announced = self.announced.borrow_mut();
        announced.insert(id.clone()).then_some((id, title, body))
    }

    /// Make a part for each output `compositor` shows that has none, stand each part on its
    /// output's rectangle, and stand the parts of outputs it no longer shows nowhere: one bar
    /// per display, following a display plugged in, moved or taken away. A part whose display
    /// went keeps its place with the compositor and the door, drawing and offering nothing, and
    /// stands again when that display comes back, so unplugging and replugging makes no new
    /// part. Cheap when nothing changed, which is what every frame runs.
    fn follow_outputs(&mut self, compositor: &mut Compositor, doors: &mut Doors) {
        let outputs = outputs_of(compositor);
        for (name, part) in &self.parts {
            if !outputs.iter().any(|(shown, _)| shown == name) {
                part.borrow_mut().set_display(None);
            }
        }
        let fullscreen = compositor.fullscreen_outputs();
        for (name, rect) in &outputs {
            if let Some((_, part)) = self.parts.iter().find(|(shown, _)| shown == name) {
                part.borrow_mut().set_display(Some((name.clone(), *rect)));
                part.borrow_mut().set_fullscreen(fullscreen.clone());
                continue;
            }
            self.mount_part(compositor, doors, name, *rect, &fullscreen);
        }
    }

    /// Make the shell's part on `output` (its connector name and logical rectangle), sharing
    /// the shell's record with every other part, and hand it to `compositor` and its doors.
    fn mount_part(
        &mut self,
        compositor: &mut Compositor,
        doors: &mut Doors,
        output: &str,
        rect: (i32, i32, u32, u32),
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
                // The first part of the shell makes the shell's record and seeds it; every
                // other display's part draws the one the shell already has.
                seed: self.parts.is_empty(),
            },
            self.layer,
            // The bar's band, at rest: what the chrome leaves it.
            self.band,
            self.scale,
            std::sync::Arc::new(move || wake.wake_by_ref()),
            self.wells.clone(),
        );
        part.set_theme(self.theme.clone());
        part.set_display(Some((output.to_owned(), rect)));
        part.set_fullscreen(fullscreen.to_vec());
        part.set_edges(self.edges());
        // The windows the views picture, each display's own: the reading's window keys stand
        // for the tabs of the workspace in front, and a window whose app holds secrets is left
        // out, so its view shows the app's mark instead of its pixels.
        let handle = self.desk.handle.clone();
        part.show_pictures(
            ui::widget::Previews::new(),
            Rc::new(move |key: &str| pictured(&handle, key)),
        );
        let part = Rc::new(RefCell::new(part));
        compositor.add_host(Box::new(Held(Rc::clone(&part))));
        // Each display's bar is its own surface on the door: what the person, a script or an
        // agent drives the shell through, and what reads its labels.
        doors.add(Rc::clone(&part) as tilo_surface_host::Shared);
        self.parts.push((output.to_owned(), part));
    }

    /// Hand the shell the desk when what it is made from changed, and what the screen says of
    /// its edges either way: a bar or dock that hides until reached for follows the pointer and
    /// the windows, which move without the desk changing. Whether anything was published.
    pub(crate) fn publish(&mut self) -> bool {
        let key = self.desk.key();
        let was = self.seen.replace(key.clone());
        let desk_changed = was.as_ref() != Some(&key);
        if desk_changed && !self.link.publish(self.desk.reading()) {
            return false;
        }
        // What the desk has to draw at an instant of its own (the switcher's panel coming up
        // while Alt is held) wakes every part then, so nothing draws or polls in between.
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

    /// Follow the displays as they come and go, and a window taking one for itself: a part is
    /// made or dropped per output, and a part on a display a fullscreen window owns steps
    /// aside. Cheap when nothing changed, which is what every frame runs.
    pub(crate) fn follow_displays(&mut self, compositor: &mut Compositor, doors: &mut Doors) {
        self.follow_outputs(compositor, doors);
    }

    /// The outputs' bounding box `(x, y, w, h)`, as a screen-edge reading names them: every
    /// output that is on.
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
    /// battery, the appearance setting), as `services` publish them, whenever what they
    /// publish may have moved.
    pub(crate) fn publish_readings(&mut self, services: &Services) {
        let revision = services.revision();
        if self.services_seen.replace(revision) == Some(revision) {
            return;
        }
        // A resource no service of this device serves (no Bluetooth, no battery) is published
        // empty: the device has none of it, so a view reads its fields as absent (off, empty)
        // and shows the device without it.
        for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
            let reading = services
                .read(resource)
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
            self.link.publish_reading(resource, &ty, reading);
        }
    }

    /// Hand the shell what its places show, from what `services` publish: the package of each
    /// app a place shows a resource of, so the place draws it through that app's own views
    /// (and none of an app no place shows); the reading of each resource a place names; and
    /// the reading of each resource those views read, as they read it (an item of a
    /// collection a reading lists by id). Each reading is read again only when its service
    /// says it moved, so a change costs the shell only the resources it touched.
    pub(crate) fn follow_places(
        &mut self,
        arrangement: &ArrangementReading,
        services: &mut Services,
    ) {
        let wanted = self.link.take_wanted();
        let key = (services.revision(), arrangement.clone());
        let moved_on = self.followed.as_ref() != Some(&key);
        if !moved_on && wanted.is_empty() {
            return;
        }
        let mut fresh: Vec<(String, String)> = Vec::new();
        if self.followed.as_ref().map(|(_, was)| was) != Some(arrangement) {
            let placed = tilo_surface_host::shell_part::readings::placed_records(
                arrangement,
                &services.resources(),
            );
            let mut packages = std::collections::BTreeMap::new();
            for ty in placed.values() {
                let Some(app) = tilo_surface_host::shell_part::readings::app_of(ty) else {
                    continue;
                };
                if let Some(module) = services.module(app) {
                    packages.insert(app.to_owned(), module);
                }
            }
            // A resource of an app no place shows any more is followed no more.
            self.reading.retain(|_, ty| {
                tilo_surface_host::shell_part::readings::app_of(ty)
                    .is_some_and(|app| packages.contains_key(app))
            });
            self.link.publish_packages(packages);
            for (id, ty) in placed {
                if !self.reading.contains_key(&id) {
                    fresh.push((id.clone(), ty.clone()));
                }
                self.reading.insert(id, ty);
            }
        }
        for (id, ty) in wanted {
            let shown = tilo_surface_host::shell_part::readings::app_of(&ty)
                .is_some_and(|app| self.link.package_apps().iter().any(|held| held == app));
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

    /// How often the shell read each resource its places show, by id, since it started: what
    /// a test proves a change re-reads only what it touched by.
    pub(crate) fn reads(&self) -> std::collections::BTreeMap<String, u64> {
        self.reads.clone()
    }

    /// How many of the session's turns read readings for the shell's places.
    pub(crate) const fn reading_rounds(&self) -> u64 {
        self.rounds
    }

    /// The apps whose packages the shell's runtime holds loaded now, because a place shows
    /// them: every display's part loads the same.
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

    /// The link the session publishes the desk on and the shell asks through: what a test
    /// stages a reading on.
    pub(crate) fn link(&self) -> Rc<SessionLink> {
        Rc::clone(&self.link)
    }

    /// The surface the desk shows over its windows, by the name the views read (`d.here`):
    /// `none` at rest.
    pub(crate) fn surface(&self) -> String {
        self.link.surface()
    }

    /// Whether the shell asked something of the door the session has not run yet.
    pub(crate) fn has_asks(&self) -> bool {
        self.link.has_asks()
    }

    /// The edges the shell's chrome stands on, read from the shell's own record: a bar at the
    /// top or the bottom (`bar_shown`, `bar_edge`) and a dock at the bottom or the top. A
    /// leading or trailing dock, or a bar or dock the person turned off, leaves that edge's
    /// band to the desk. Until the record holds them, the first shell: a bar at the top alone.
    pub(crate) fn edges(&self) -> tilo_surface_host::shell_part::HostEdges {
        use tilo_surface_host::shell_part::HostEdges;
        let Some(fields) = self.shell_edges() else {
            return HostEdges {
                bar: true,
                bar_top: true,
                dock: false,
                dock_bottom: false,
            };
        };
        HostEdges {
            bar: fields.bar_shown,
            bar_top: fields.bar_edge == "top",
            dock: fields.dock_shown && (fields.dock_edge == "top" || fields.dock_edge == "bottom"),
            dock_bottom: fields.dock_edge == "bottom",
        }
    }

    /// The shell record's edge settings as it holds them: each an enum's case id, which the
    /// package names in `chrome.tl`. `None` until the record exists (the bar alone).
    fn shell_edges(&self) -> Option<EdgeSettings> {
        let part = self.parts.first()?.1.borrow();
        let record = part.shell_record()?;
        let package = part.package()?;
        let meta = &package.meta;
        let ty = meta.types.iter().find(|ty| ty.name == "Shell")?;
        let read = |name: &str| {
            ty.fields
                .iter()
                .find(|field| field.name == name)
                .and_then(|field| record.get(&field.id).cloned())
        };
        let flag = |name: &str| {
            matches!(
                read(name),
                Some(tilolang_tilo::record::DynValue::Register(
                    serde_json::Value::Bool(true)
                ))
            )
        };
        // A case of an enum is its variant's id in the record; `Edge.top` and friends are the
        // names the settings use, so the case is read by the name its variant's id stands for.
        let case = |name: &str, ty_name: &str| -> String {
            let Some(field) = read(name) else {
                return String::new();
            };
            let tilolang_tilo::record::DynValue::Register(serde_json::Value::String(id)) = field
            else {
                return String::new();
            };
            meta.enums
                .iter()
                .find(|en| en.name == ty_name)
                .and_then(|en| en.variants.iter().find(|v| v.id == id))
                .map_or_else(String::new, |v| v.name.clone())
        };
        Some(EdgeSettings {
            bar_shown: flag("bar_shown"),
            bar_edge: case("bar_edge", "Edge"),
            dock_shown: flag("dock_shown"),
            dock_edge: case("dock_edge", "Edge"),
        })
    }

    /// Move every part to the bands the shell's chrome stands in: a bar at the top or bottom, a
    /// dock at the bottom or top. A change of the person's settings moves them on the next
    /// frame.
    pub(crate) fn set_edges(&self, edges: tilo_surface_host::shell_part::HostEdges) {
        for (_, part) in &self.parts {
            part.borrow_mut().set_edges(edges);
        }
    }

    /// Run what the shell asked through `invoke` (the session's door).
    ///
    /// The shell's calls of the session's actions (`session.close_place(place: 3)`) reach the
    /// door with every argument as text, so each is typed here by the type the action itself declares for it: a workspace's key
    /// is text even when it reads as digits, and a count is a number. `types` answers with the
    /// declared type of `(resource, action)`'s argument `name`, when the door publishes it.
    pub(crate) fn serve(
        &self,
        types: impl Fn(&str, &str, &str) -> Option<tilo_actions::params::ParamType>,
        invoke: impl FnMut(&str, &str, &Map<String, Value>) -> Result<(), String>,
    ) {
        let mut invoke = invoke;
        self.link.serve(|ask: &Ask| {
            let arguments: Map<String, Value> = ask
                .arguments
                .iter()
                .map(|(name, value)| {
                    let ty = types(&ask.resource, &ask.action, name);
                    (name.clone(), typed(value, ty))
                })
                .collect();
            invoke(&ask.resource, &ask.action, &arguments)
        });
    }

    /// How much work the bar did since the session started, over every display: its view turns
    /// and frames summed, since a bar on each display does the same work on each.
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
            // The effects handed and served are the link's, shared by every part: the last
            // part's reading holds the whole count, so it is not summed.
            if !seen_event {
                work.handed = part.handed;
                work.served = part.served;
                seen_event = true;
            }
        }
        work
    }

    /// Draw every part in `theme`: the person changed their appearance.
    pub(crate) fn set_theme(&mut self, theme: &ui::Theme) {
        self.theme = theme.clone();
        for (_, part) in &self.parts {
            part.borrow_mut().set_theme(theme.clone());
        }
    }

    /// What the bar refused since the session started, in its words: every display's, and
    /// what the door refused of what the bar asked.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.parts
            .iter()
            .flat_map(|(_, part)| part.borrow().refusals().to_vec())
            .chain(self.link.refused())
            .collect()
    }

    /// Where the bar's control named `name` stands, logical `(x, y, w, h)`: the first part
    /// that shows it, since a name is the bar's one control and every bar carries the same.
    pub(crate) fn control(&self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .find_map(|(_, part)| part.borrow_mut().control_bounds(name))
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

    /// Where each display's bar stands now, by connector name: what a test reads to prove a
    /// bar on every display, and none on one a fullscreen window owns.
    pub(crate) fn parts(&self) -> Vec<BarPlace> {
        self.parts
            .iter()
            .filter_map(|(name, part)| {
                let part = part.borrow();
                let (_, rect) = part.placement()?;
                Some(BarPlace {
                    output: name.clone(),
                    rect,
                    aside: part.aside(),
                })
            })
            .collect()
    }
}

/// A display's bar, as it stands now: the output it is on, where it stands in the logical
/// layout, and whether it stepped aside for a fullscreen window there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarPlace {
    /// The output's connector name (`HEADLESS-1`).
    pub output: String,
    /// Where the bar stands, logical `(x, y, w, h)`.
    pub rect: (i32, i32, u32, u32),
    /// A fullscreen window owns the output, so the bar draws nothing there.
    pub aside: bool,
}

/// Every output `compositor` shows, by connector name and logical rectangle `(x, y, w, h)`:
/// one bar stands on each, and none spans them all.
fn outputs_of(compositor: &Compositor) -> Vec<(String, (i32, i32, u32, u32))> {
    compositor
        .outputs()
        .into_iter()
        .filter_map(|output| {
            output.rect.map(|(x, y, w, h)| {
                let side = |side: i32| u32::try_from(side.max(1)).unwrap_or(1);
                (output.name.clone(), (x, y, side(w), side(h)))
            })
        })
        .collect()
}

/// The compositor's window for the reading's window `key` (its tab's id), when it may be
/// pictured: this device draws the window and its app shows no secrets. A window this device
/// does not draw, or one whose app holds secrets, is pictured by nothing, and its view shows
/// the app's mark instead.
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

/// `value` (the text the shell's effect wrote) as the door's arguments take it, in the type
/// the action declares for that argument. Text stays text however it reads: a workspace's
/// key is `"281474976710657"`, not a number. An undeclared argument keeps the text it came as,
/// and the door refuses it by name.
fn typed(value: &str, ty: Option<tilo_actions::params::ParamType>) -> Value {
    use tilo_actions::params::ParamType;
    match ty {
        Some(ParamType::Integer) => value
            .trim()
            .parse::<i64>()
            .map_or_else(|_| Value::String(value.to_owned()), Value::from),
        Some(ParamType::Number) => value
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(value.to_owned()), Value::Number),
        Some(ParamType::Boolean) => match value.trim() {
            "true" | "yes" | "on" => Value::Bool(true),
            "false" | "no" | "off" => Value::Bool(false),
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

/// `host`, the desktop's bar, as the desk stands under `layer`: the shell's whole screen draws
/// its own bar, so the desktop's steps aside ([`Aside`]), and still answers its chords.
pub(crate) fn bar_under(
    layer: Option<ShellLayer>,
    host: impl SurfaceHost + 'static,
) -> Box<dyn SurfaceHost> {
    if layer == Some(ShellLayer::Screen) {
        Box::new(Aside(host))
    } else {
        Box::new(host)
    }
}

/// `host`, the desktop's desk, as the desk stands under `layer`: the shell's whole screen draws
/// the desk itself (its strips, their frames and the places in them, around the wells the windows
/// stand in), so the desktop's own desk steps aside ([`Aside`]) rather than drawing a second set of
/// strips under it. It still keeps the windows' model the wells place them by, and answers its
/// chords.
pub(crate) fn desk_under(
    layer: Option<ShellLayer>,
    host: impl SurfaceHost + 'static,
) -> Box<dyn SurfaceHost> {
    bar_under(layer, host)
}

/// The chrome's published side as the desk stands under `layer`: on the shell's whole screen
/// the chrome draws none of its surfaces, so its `overview` (what the touchpad's pinch, the
/// overview's shortcut, an agent and the REPL run) opens and closes the shell's own overview,
/// the surface the desk shows over its windows (`overlays`), which the screen's views draw.
/// Every other action is the chrome's.
pub(crate) fn chrome_door<T: tilo_surface_host::Published + 'static>(
    layer: Option<ShellLayer>,
    chrome: &Rc<RefCell<T>>,
    overlays: &Rc<crate::overlays::Overlays>,
) -> tilo_surface_host::Shared {
    let chrome = Rc::clone(chrome) as tilo_surface_host::Shared;
    if layer == Some(ShellLayer::Screen) {
        let overlays = Rc::clone(overlays);
        Rc::new(RefCell::new(ScreenChrome { chrome, overlays }))
    } else {
        chrome
    }
}

/// The chrome's door on the shell's whole screen ([`chrome_door`]).
struct ScreenChrome {
    chrome: tilo_surface_host::Shared,
    overlays: Rc<crate::overlays::Overlays>,
}

impl ScreenChrome {
    /// Whether the overview is to be open after `address` with `arguments`, when it is one of
    /// the chrome's ways of opening or closing it: `Ok(None)` for every other action.
    fn overview(
        &self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<Option<bool>, tilo_actions::DoorError> {
        if address.resource != tilo_desktop::desk_verbs::CHROME_RESOURCE {
            return Ok(None);
        }
        let open = self.overlays.here() == crate::overlays::Overlay::Overview.name();
        match address.action.as_str() {
            "overview" => match arguments.get("open") {
                None | Some(Value::Null) => Ok(Some(!open)),
                Some(Value::Bool(wanted)) => Ok(Some(*wanted)),
                Some(other) => Err(tilo_actions::DoorError::InvalidArguments(format!(
                    "argument `open` must be boolean, got {other}"
                ))),
            },
            "shortcut"
                if arguments.get("action").cloned().and_then(|action| {
                    serde_json::from_value::<ui::shortcut_table::ShortcutAction>(action).ok()
                }) == Some(ui::shortcut_table::ShortcutAction::WorkspaceOverview) =>
            {
                Ok(Some(!open))
            }
            _ => Ok(None),
        }
    }

    /// Open or close the shell's overview.
    fn show_overview(&self, open: bool) -> ui::host_operation::ResourceOperationStart {
        let name = crate::overlays::Overlay::Overview.name();
        if open {
            self.overlays.open(name);
        } else {
            self.overlays.close(name);
        }
        ui::host_operation::ResourceOperationStart::Completed(tilo_actions::ActionOutput::default())
    }
}

impl tilo_surface_host::Published for ScreenChrome {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        self.chrome.borrow_mut().resources()
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if let Some(open) = self.overview(address, arguments)? {
            return Ok(self.show_overview(open));
        }
        self.chrome.borrow_mut().invoke(address, arguments)
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if let Some(open) = self.overview(address, arguments)? {
            return Ok(self.show_overview(open));
        }
        self.chrome
            .borrow_mut()
            .invoke_as(actor, address, arguments)
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

/// A surface of the session's own the shell's whole screen stands in for: the desktop's bar,
/// whose band the screen's bar takes, and the desktop's desk, whose strips the screen draws. It
/// draws nothing and takes no pointer, and still answers the chords it answers anywhere, so a key
/// the person presses does what it did.
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

    fn claim_key(&mut self, key: &HostKey, locked: bool, inhibited: bool) -> KeyClaim {
        self.0.claim_key(key, locked, inhibited)
    }

    fn key(&mut self, key: &HostKey) {
        self.0.key(key);
    }

    fn outputs_changed(&mut self, outputs: &[(i32, i32, i32, i32)]) {
        self.0.outputs_changed(outputs);
    }

    fn cursor(&self) -> CursorIcon {
        CursorIcon::Default
    }

    fn prepare_frame(&mut self) -> bool {
        // It keeps up with what it hears, and shows none of it.
        self.0.prepare_frame();
        false
    }
}

impl crate::Session {
    /// Hand the shell's bar the desk when it changed, and run what the bar
    /// asked through the door, as any caller of the action protocol does.
    pub(crate) fn follow_shell(&mut self) -> Result<(), crate::Error> {
        let Some(shell) = self.shell.as_ref() else {
            return Ok(());
        };
        // The shell's views draw the centre over the desk, so while it is up the person is
        // looking at what the notices say: they are read, as they are while the panel's own
        // centre shows them. The bell's count, the centre's header and its rows read the one
        // set, so they never disagree. It is the same surface the views read (`d.here`).
        if shell.surface() == crate::overlays::Overlay::Centre.name() {
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
        shell.publish();
        let arrangement = self.shared.arrangement.borrow().reading();
        shell.publish_readings(&self.shared.services.borrow());
        shell.follow_places(&arrangement, &mut self.shared.services.borrow_mut());
        shell.publish_arrangement(arrangement);
        // What the screen says of its edges is what a bar or dock that shows only sometimes
        // follows; it moves without the desk, so it goes every turn. The part covers the bands
        // the record says the chrome stands in.
        let edges = shell.edges();
        shell.publish_edges(&self.compositor);
        shell.set_edges(edges);
        // One bar per display: a display plugged in, taken away or taken by a fullscreen window
        // moves its part.
        shell.follow_displays(&mut self.compositor, &mut self.shared.doors);
        if !shell.has_asks() {
            return Ok(());
        }
        let doors = &self.shared.doors;
        let search = &self.shared.search;
        let workspace = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
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
                // An app's own `open` (`session.open(app)`) starts it in the workspace in
                // front, as the launcher's row of it does.
                if let Some(app) = resource
                    .strip_prefix(tilo_actions::remote::APPLICATION_RESOURCE_PREFIX)
                    .filter(|_| action == tilo_actions::remote::APPLICATION_OPEN_ACTION)
                {
                    return search
                        .perform_checked(
                            &workspace,
                            tilo_desktop_search::Target::Launch {
                                app: app.to_owned(),
                            },
                            &mut |own| doors.invoke(&own.address, &own.arguments).map(drop),
                        )
                        .map_err(|error| error.to_string());
                }
                doors
                    .invoke(&ActionAddress::new(resource, action), arguments)
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        );
        // What ran may have asked the session in turn (a menu, a sheet).
        self.settle()?;
        if let Some(shell) = self.shell.as_mut() {
            shell.publish();
        }
        Ok(())
    }

    /// What the shell's bar reads the desk through: a test stages a reading on it, as a
    /// collaborating session's presence would arrive.
    #[must_use]
    pub fn shell_link(&self) -> Option<Rc<tilo_surface_host::SessionLink>> {
        self.shell.as_ref().map(Shell::link)
    }

    /// The collaboration plane this session takes part in, when it does: what a test hands it
    /// is who is here.
    #[must_use]
    pub fn sync(&self) -> Option<&Rc<crate::sync::Client>> {
        self.shared.sync.as_ref()
    }

    /// The notifications the desk keeps: what apps sent, and what the centre shows. A test
    /// posts into it as an app's arrival would land.
    #[must_use]
    pub fn notices(&self) -> &tilo_desktop_surface::DesktopNotifications {
        &self.shared.notices
    }

    /// How much work the shell's bar did since the session started:
    /// `None` when its package did not start.
    #[must_use]
    pub fn shell_work(&self) -> Option<tilo_surface_host::ShellWork> {
        self.shell.as_ref().map(Shell::work)
    }

    /// What the shell's bar refused since the session started, in its own
    /// words: a press whose action could not run.
    #[must_use]
    pub fn shell_refusals(&self) -> Vec<String> {
        self.shell.as_ref().map(Shell::refusals).unwrap_or_default()
    }

    /// Where the shell bar's control named `name` (as assistive technology
    /// hears it) stands, logical `(x, y, w, h)`: what a test presses it by.
    #[must_use]
    pub fn shell_control(&self, name: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.control(name)
    }

    /// Whether the shell lays out `text` for assistive technology anywhere on screen: a
    /// label, a description or a value holding it. What a test reads a composed view by.
    #[must_use]
    pub fn shell_says(&self, text: &str) -> bool {
        self.shell.as_ref().is_some_and(|shell| shell.says(text))
    }

    /// Where the shell lays out `text` for assistive technology, logical `(x, y, w, h)`: the
    /// first node whose label, description or value holds it, on any shown part.
    #[must_use]
    pub fn shell_text_bounds(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.text_bounds(text)
    }

    /// The apps whose packages the shell's runtime holds loaded, because a place shows them.
    #[must_use]
    pub fn shell_apps(&self) -> Vec<String> {
        self.shell
            .as_ref()
            .map(Shell::loaded_apps)
            .unwrap_or_default()
    }

    /// How many of the session's turns read readings for the shell's places: each hands the
    /// shell everything the views composed there read in one run of them.
    #[must_use]
    pub fn shell_reading_rounds(&self) -> u64 {
        self.shell.as_ref().map_or(0, Shell::reading_rounds)
    }

    /// How often the session read each resource the shell's places show, by id, for the
    /// shell: a reading is read again only when its service says it moved.
    #[must_use]
    pub fn shell_reads(&self) -> std::collections::BTreeMap<String, u64> {
        self.shell.as_ref().map(Shell::reads).unwrap_or_default()
    }

    /// The shell's bars, one per display, as each stands now: the output it is on, where it
    /// stands, and whether it stepped aside for a fullscreen window there.
    #[must_use]
    pub fn shell_bars(&self) -> Vec<BarPlace> {
        self.shell.as_ref().map(Shell::parts).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_shell_package_compiles_and_its_ids_lock_holds_every_declaration() {
        let packages = super::compile().unwrap_or_else(|why| panic!("{why}"));
        let compiled = packages.last().expect("the shell package compiles last");
        let lock = compiled.ids.to_text();
        assert_eq!(
            lock,
            tilo_surface_host::shell_part::shell_ids(),
            "apps/tl/shell/app.ids is not the lock the package compiles to; write:\n{lock}"
        );
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

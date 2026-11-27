//! The shell on the native session: the tilolang shell package (`apps/tl/shell`) shown by a
//! [`ShellPart`] per display — the whole screen under the windows, which stand in its wells — kept
//! in step with the session's shell (`crate::shell`) and asking the session's API for what the
//! person presses.

use std::cell::RefCell;
use std::rc::Rc;

use crate::platform::surfaces::{SessionLink, ShellPart, ShellWork};

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_compositor::Compositor;
use tilo_session_core::desktop::overlays::{Overlays, Part, Target};

use tilo_service::Services;
use tilo_session_core::shell::{Ask, LayoutReading};
use tilo_session_core::surface::ShellSurface as _;
use tilo_session_core::surfaces::Surfaces;

use crate::device_lists::DeviceLists;
use crate::displays;

#[cfg(target_os = "linux")]
pub mod state;

pub(crate) mod activities;
pub(crate) mod asks;
pub(crate) mod mount;
pub(crate) mod osd;
pub(crate) mod rules;
pub(crate) mod shortcut_guide;

#[cfg(target_os = "linux")]
pub mod part;
#[cfg(target_os = "linux")]
pub mod part_view;

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
    match crate::platform::surfaces::installed_shell() {
        Ok(Some(packages)) => Ok((packages, true)),
        Ok(None) => Ok((crate::platform::surfaces::compile_shell()?, false)),
        Err(error) => {
            // A store row that does not read is the person's to see, and the shell still stands:
            // the fallback is what a device with no shell of its own shows.
            tracing::warn!(%error, "the installed shell does not load; the built-in one stands in");
            Ok((crate::platform::surfaces::compile_shell()?, false))
        }
    }
}

/// The shell on the session, and what feeds it.
pub(crate) struct Shell {
    lists: DeviceLists,
    link: Rc<SessionLink>,
    /// The services' revision and the layout the places' readings were last followed at.
    followed: Option<(u64, LayoutReading)>,
    /// The resources whose readings the shell holds for its places, by id, each with its type as
    /// the shell's runtime names another app's (`<app>#<Type>`): what a place names, and what the
    /// views composed there read.
    reading: std::collections::BTreeMap<String, String>,
    /// The live resources the places' packages and readings were last followed with: one that
    /// goes live or ends is followed as a place that opens or closes is.
    live: Vec<String>,
    /// How often each was read for the shell.
    reads: std::collections::BTreeMap<String, u64>,
    /// The listings the shell's views list records of with a query, by the type they list
    /// (`tilo.invocations#Invocation`), each with the resource the session publishes it as.
    listings: std::collections::BTreeMap<String, &'static str>,
    /// The apps whose packages a view composed in a place read a type of before it was loaded, by
    /// app id: published with the packages the places show, and kept for the rest of the session (a
    /// view keeps drawing what it read once the card is there).
    needed: std::collections::BTreeSet<String>,
    /// Whether the store moved since the last pass and its packages must be published again: a
    /// style that failed to load and was reinstalled fixed has the same package id and needs
    /// its new module sent (`shell.md` §6).
    republish: bool,
    /// The revision of the device's styles the shell last followed ([`Self::follow_styles`]).
    styles_seen: Option<u64>,
    /// How many of the session's turns read some of them: what the views composed in places waited
    /// on comes in one turn for everything one run of them read, not one per resource.
    rounds: u64,
    /// Lent last to every part, by the workspace in front and the media it was opened over: a
    /// part told the same one again keeps the store it has.
    lent_content: Option<(String, Option<ui::content::ContentStore>)>,
    /// The shell's own resources (`resources: .own`, the person's shell choices), shared by every
    /// part: one display draws what another wrote, so a change any part writes is the whole
    /// shell's. In this session's memory until the device's daemon holds the person's own
    /// workspace, then, for a shell the daemon runs ([`Self::kept_elsewhere`]), drawn from there
    /// ([`Self::keep_record_in`]), which carries them to the person's other devices.
    record: tilolang_tilo::resources::ResourceStore,
    /// Whether the record is kept in the person's own workspace yet.
    record_kept: bool,
    /// One part per output, in the order the outputs are known: each stands on its own display, and
    /// a display added (or taken away) has its part made (or dropped) to follow.
    parts: Vec<(String, Rc<RefCell<ShellPart>>)>,
    /// The directions a part's views left over since the session last acted on them: the
    /// desktop's focus moves that way, from the shell's controls to the window it points at.
    steps: Rc<RefCell<Vec<part::FocusStep>>>,
    /// The packages the shell's parts are made from, kept to make each new display's part.
    packages: Vec<tilolang_tilo::tilolang::Compiled>,
    app: String,
    person: String,
    /// How far in from its display's top and bottom edges the chrome the parts laid out stands,
    /// in logical pixels — the bar's band and the dock's — measured from the frame each part laid
    /// out, so the bands follow whatever size the person's shell draws them at. `None` until a
    /// part measured its first frame, when the reference height stands in.
    bands: std::cell::Cell<Option<(f32, f32)>>,
    /// The wells the screen's views report, which the desktop places the windows at.
    wells: Option<ui::widget::Wells>,
    /// The services' revision the platform readings were last read at: they are read again only
    /// when it moved.
    services_seen: Option<u64>,
    /// The workspace whose members the shell last read, and the view's revision it read them at:
    /// its bar's faces and names are the workspace in front's people, read again when they move.
    members_seen: Option<(String, u64)>,
    /// The names the session's roster gave when the shell last read the workspace's members: a
    /// person who comes here names the row the workspace keeps no name for, so the members are
    /// read again when a name arrives.
    members_named_by: Option<Value>,
    /// Whether the shell's records are the person's, kept in their own workspace by the device's
    /// daemon: the installed shell, which the daemon runs too, on a session the daemon started.
    /// A change of them is then the person's call of the shell's action on the device's dispatch
    /// ([`Self::own_calls`]), and the record is drawn from their workspace once the daemon holds
    /// it ([`Self::keep_record_in`]); without one the session keeps them for itself, in memory.
    kept_elsewhere: bool,
    /// What the device's dispatch refused of those calls, as the calls settle off the loop: said
    /// with the shell's other refusals on the next pass.
    dispatch_refusals: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// The theme the person's appearance settings make: every part draws in it, and a part made for
    /// a display plugged in later starts in it.
    theme: tilo_ui_base::Theme,
}

/// Reconcile the desktop's display partition with the outputs the compositor holds: a display
/// added, moved or taken away gives its strips a display even before any window is placed, and a
/// monitor that went hands its windows to the nearest one left. A mirror is one display, as the
/// desktop lays them: an output sharing a rectangle shows the same.
pub(crate) fn reconcile_displays(
    handle: &tilo_session_core::desktop::state::DesktopHandle,
    outputs: &[tilo_compositor::OutputInfo],
    primary: Option<&str>,
) {
    let mut displays: Vec<tilo_desktop_model::Display> = Vec::new();
    for (index, output) in outputs.iter().filter(|output| output.enabled).enumerate() {
        let Some((x, y, width, height)) = output.rect else {
            continue;
        };
        let rect = ui::kurbo::Rect::new(
            f64::from(x),
            f64::from(y),
            f64::from(x) + f64::from(width),
            f64::from(y) + f64::from(height),
        );
        if displays.iter().any(|display| {
            let shared = ui::kurbo::Rect::new(
                display.x,
                display.y,
                display.x + display.width,
                display.y + display.height,
            )
            .intersect(rect);
            shared.width() > 0.0 && shared.height() > 0.0
        }) {
            continue;
        }
        let Ok(id) = u32::try_from(index) else {
            break;
        };
        displays.push(tilo_desktop_model::Display {
            id,
            monitor: output.monitor.clone(),
            x: f64::from(x),
            y: f64::from(y),
            width: f64::from(width),
            height: f64::from(height),
            primary: primary == Some(output.monitor.as_str()),
        });
    }
    // The partition gives every group without a display one in the same pass: the display in use,
    // else the primary, so a strip placed since the last pass learns the display it stands on.
    handle.set_displays(&displays);
}

impl Shell {
    /// The shell over every output of `compositor` , published on `surfaces` . The screen's views
    /// report their wells to `wells` , the handle the window policy places every window by
    /// ([`crate::platform::surfaces::WellPolicy`]).
    pub(crate) fn mount(
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
        lists: DeviceLists,
        wells: Option<ui::widget::Wells>,
        theme: tilo_ui_base::Theme,
        daemon: bool,
        seat: Rc<std::cell::Cell<bool>>,
        sheet: Rc<std::cell::Cell<bool>>,
    ) -> Result<Self, String> {
        // What the person waits for before the shell can draw: its package compiled, its record
        // read and its views made. The frame path after this is each frame's own cost.
        tilo_startup_metrics::mark("shell-build-start");
        let (packages, installed) = shell_packages()?;
        // The daemon runs the shell's actions from the installed package; the build's own copy it
        // has none of.
        let kept_elsewhere = daemon && installed;
        let app = packages
            .last()
            .map(|compiled| compiled.meta.app.clone())
            .ok_or("the shell package did not compile")?;
        let link = Rc::new(SessionLink::with_focus(seat, sheet));
        let mut shell = Self {
            lists,
            link,
            followed: None,
            reading: std::collections::BTreeMap::new(),
            live: Vec::new(),
            reads: std::collections::BTreeMap::new(),
            listings: std::collections::BTreeMap::new(),
            needed: std::collections::BTreeSet::new(),
            republish: false,
            styles_seen: None,
            rounds: 0,
            record: tilolang_tilo::resources::ResourceStore::in_memory(),
            record_kept: false,
            parts: Vec::new(),
            steps: Rc::new(RefCell::new(Vec::new())),
            lent_content: None,
            packages,
            app,
            person: String::new(),
            bands: std::cell::Cell::new(None),
            wells,
            services_seen: None,
            members_seen: None,
            members_named_by: None,
            kept_elsewhere,
            dispatch_refusals: std::sync::Arc::default(),
            theme,
        };
        shell.person = shell.lists.person.clone();
        shell.follow_outputs(compositor, surfaces, &[]);
        // The styles the device offers, as far as the daemon said yet: the switch lists them as
        // they arrive (shell.md §6).
        shell.follow_styles();
        shell.follow_deadline(None);
        tilo_startup_metrics::mark("shell-build-done");
        Ok(shell)
    }

    /// Make a part for each output `compositor` shows that has none, stand each part on its
    /// output's rectangle, and stand the parts of outputs it no longer shows nowhere: one part per
    /// display, following a display plugged in, moved or taken away. A part whose display went
    /// keeps its place with the compositor and the API, drawing and offering nothing, and stands
    /// again when that display comes back, so unplugging and replugging makes no new part. Cheap
    /// when nothing changed, which is what every frame runs.
    fn follow_outputs(
        &mut self,
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
        identify: &[tilo_session_core::desktop::host::chrome::Label],
    ) {
        let info = compositor.outputs();
        reconcile_displays(
            &self.lists.handle,
            &info,
            compositor.primary_monitor().as_deref(),
        );
        let screens = self.screens(&info, identify);
        let outputs = displays::placed(&info);
        for (name, part) in &self.parts {
            if !outputs.iter().any(|shown| &shown.connector == name) {
                part.borrow_mut().set_display(None, 1.0);
                part.borrow_mut().set_screen(None);
            }
        }
        let fullscreen = compositor.fullscreen_outputs();
        for output in &outputs {
            let screen = screens.get(&output.connector).cloned();
            if let Some((_, part)) = self
                .parts
                .iter()
                .find(|(shown, _)| *shown == output.connector)
            {
                part.borrow_mut().set_display(
                    Some((output.connector.clone(), output.part_rect())),
                    output.scale,
                );
                part.borrow_mut().set_screen(screen);
                part.borrow_mut().set_fullscreen(fullscreen.clone());
                continue;
            }
            self.mount_part(
                compositor,
                surfaces,
                &output.connector,
                output.part_rect(),
                output.scale,
                &fullscreen,
                screen,
            );
        }
    }

    /// The display each output draws on, by connector, as that display's part reads it: its id
    /// in this device's layout, what a person calls it, where it stands, its scale and whether
    /// it is primary. The layout's own partition names the id and the primary display, so a
    /// part and its strips agree on which display they are.
    ///
    /// `identify` is what the displays say while the person asked Settings which screen is which:
    /// each one's number and the name Settings gives it, by the monitor the shell follows it by.
    fn screens(
        &self,
        outputs: &[tilo_compositor::OutputInfo],
        identify: &[tilo_session_core::desktop::host::chrome::Label],
    ) -> std::collections::BTreeMap<String, tilo_session_core::shell::LayoutDisplayReading> {
        let (ids, primary) = self.lists.handle.read(|tree| {
            let ids: std::collections::BTreeMap<String, u32> = tree
                .outputs
                .displays
                .iter()
                .map(|display| (display.monitor.clone(), display.id))
                .collect();
            (ids, tree.outputs.primary())
        });
        outputs
            .iter()
            .filter_map(|output| {
                let (x, y, width, height) = output.rect?;
                let id = ids.get(&output.monitor).copied().unwrap_or(0);
                // What Settings says this display is, while the person asked: the same number its
                // tile carries there, and the same name.
                let saying = identify
                    .iter()
                    .find(|label| label.monitor == output.monitor);
                Some((
                    output.name.clone(),
                    tilo_session_core::shell::LayoutDisplayReading {
                        id: i64::from(id),
                        connector: output.name.clone(),
                        name: displays::display_name(output),
                        x: i64::from(x),
                        y: i64::from(y),
                        width: i64::from(width),
                        height: i64::from(height),
                        scale: output.scale,
                        // A device with one display has no partition to name a primary: that
                        // display is the primary.
                        primary: primary.is_none_or(|primary| primary == id),
                        identify: saying.is_some(),
                        identify_number: saying
                            .map_or(0, |label| i64::try_from(label.number).unwrap_or_default()),
                        identify_name: saying.map(|label| label.name.clone()).unwrap_or_default(),
                    },
                ))
            })
            .collect()
    }

    /// A part of the shell over its record at `scale`, which makes and seeds the shell's roots when
    /// `seed`, drawing the shell's theme and the windows' pictures.
    fn part(&self, compositor: &Compositor, scale: f64, seed: bool) -> ShellPart {
        let wake = compositor.waker();
        let mut part = ShellPart::new(
            crate::platform::surfaces::ShellIdentity {
                app: &self.app,
                packages: self.packages.clone(),
                person: &self.person,
                link: Rc::clone(&self.link),
                store: self.record.clone(),
                seed,
                kept_elsewhere: self.kept_elsewhere,
            },
            scale,
            std::sync::Arc::new(move || wake.wake_by_ref()),
            self.wells.clone(),
        );
        // A direction the views have no control left for is the desktop's: the session takes it
        // on the pass and moves the focus to the window it points at.
        let steps = Rc::clone(&self.steps);
        part.on_focus_leave(Rc::new(move |step| steps.borrow_mut().push(step)));
        part.set_theme(self.theme.clone());
        // The windows the views picture, each display's own: the reading's window keys stand for
        // the tabs of the workspace in front, and a window whose app holds secrets is left out, so
        // its view shows the app's mark instead of its pixels.
        let handle = self.lists.handle.clone();
        part.carry_floats(handle.clone());
        part.show_pictures(
            ui::widget::Previews::new(),
            Rc::new(move |key: &str| pictured(&handle, key)),
        );
        part
    }

    /// Keep the shell's record in the person's own workspace, whose store `store` is, from now on,
    /// as an app the session runs itself is kept in a workspace once the device's daemon holds it
    /// ([`tilolang_service::WorkspacePort`]): each part is made again over it, the first seeding
    /// what the workspace holds none of yet, and stands where it stood.
    pub(crate) fn keep_record_in(
        &mut self,
        store: Rc<dyn tilo_service::WorkspaceStore>,
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
    ) {
        let started = std::time::Instant::now();
        self.record = tilolang_tilo::resources::ResourceStore::new(Rc::new(
            tilolang_service::WorkspacePort::new(store, &self.app),
        ));
        self.record_kept = true;
        for (index, (_, part)) in self.parts.iter().enumerate() {
            let scale = part.borrow().part_scale();
            let made = self.part(compositor, scale, index == 0);
            *part.borrow_mut() = made;
        }
        self.follow_outputs(compositor, surfaces, &[]);
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            parts = self.parts.len(),
            "the shell's record is kept in the person's own workspace"
        );
    }

    /// Whether the shell's record is kept in the person's own workspace yet.
    pub(crate) fn record_kept(&self) -> bool {
        self.record_kept
    }

    /// Another of the person's devices wrote the shell's record: what was read of it is read again.
    pub(crate) fn record_moved(&self) {
        self.record.moved();
    }

    /// Make the shell's part on `output` (its connector name and logical rectangle), sharing the
    /// shell's record with every other part, and hand it to `compositor` and its surfaces.
    fn mount_part(
        &mut self,
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
        output: &str,
        rect: (i32, i32, u32, u32),
        scale: f64,
        fullscreen: &[String],
        screen: Option<tilo_session_core::shell::LayoutDisplayReading>,
    ) {
        // The first part of the shell makes the shell's roots and seeds them; every other
        // display's part draws the ones the shell already has.
        let mut part = self.part(compositor, scale, self.parts.is_empty());
        part.set_display(Some((output.to_owned(), rect)), scale);
        part.set_screen(screen);
        part.set_fullscreen(fullscreen.to_vec());
        let part = Rc::new(RefCell::new(part));
        compositor.add_surface(part.clone());
        // Each display's part is its own surface on the API: what the person, a script or an agent
        // drives the shell through, and what reads its labels.
        surfaces.add(Rc::clone(&part) as tilo_session_core::surfaces::Shared);
        self.parts.push((output.to_owned(), part));
    }

    /// Wake every part at the instant the shell has something to draw of its own (the switcher's
    /// panel coming up while Alt is held), so nothing draws or polls in between.
    pub(crate) fn follow_deadline(&self, also: Option<ui::time::Instant>) {
        let deadline = match (self.lists.next_deadline(), also) {
            (Some(listed), Some(also)) => Some(listed.min(also)),
            (listed, also) => listed.or(also),
        };
        for (_, part) in &self.parts {
            part.borrow_mut().set_deadline(deadline);
        }
    }

    /// Tell the displays what the pointer and the windows say of each display's edges: the pointer
    /// at a display's top or bottom edge, and a window where a bar or a dock there would ride.
    /// Each display's own row of the reading says it, so a bar that hides until it is reached for
    /// shows on the display reached and on no other. Whether that moved.
    fn publish_edges(&self, compositor: &Compositor, displays: &displays::Displays) -> bool {
        let (bar_band, dock_band) = self.bands();
        let windows: Vec<tilo_session_core::shell::WindowRect> = compositor
            .windows()
            .into_iter()
            .map(|(_, (x, y, w, h))| tilo_session_core::shell::WindowRect {
                x: f64::from(x),
                y: f64::from(y),
                width: f64::from(w),
                height: f64::from(h),
            })
            .collect();
        let pointer = compositor.pointer_location();
        let mut edges = std::collections::BTreeMap::new();
        for output in compositor.outputs().iter().filter(|output| output.enabled) {
            let Some((x, y, width, height)) = output.rect else {
                continue;
            };
            let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
                continue;
            };
            edges.insert(
                output.name.clone(),
                tilo_session_core::shell::screen_edges(
                    (x, y, width, height),
                    (f64::from(bar_band), f64::from(dock_band)),
                    pointer,
                    &windows,
                ),
            );
        }
        if edges.is_empty() {
            return false;
        }
        displays.set_edges(edges)
    }

    /// Follow the displays as they come and go, and a window taking one for itself: a part is made
    /// or dropped per output, and a part on a display a fullscreen window owns steps aside. Cheap
    /// when nothing changed, which is what every frame runs.
    pub(crate) fn follow_displays(
        &mut self,
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
        identify: &[tilo_session_core::desktop::host::chrome::Label],
    ) {
        self.follow_outputs(compositor, surfaces, identify);
        // The bands the parts laid their chrome out at this frame, so a bar or a dock the person
        // resized is reached for and seen as covered at its own height, and the session's own
        // surfaces stand under it.
        if let Some(measured) = self
            .parts
            .iter()
            .find_map(|(_, part)| part.borrow().bands())
        {
            self.bands.set(Some(measured));
        }
    }

    /// How far in from the display's top and bottom edges the shell's chrome stands, in logical
    /// pixels: the bar's band and the dock's, as the parts measured them, or the reference height
    /// of each before a part measured its first frame.
    pub(crate) fn bands(&self) -> (f32, f32) {
        self.bands.get().unwrap_or_else(|| {
            let bar = tilo_session_core::desktop::overlays::topbar_height(&self.theme);
            (bar, bar)
        })
    }

    /// Hand the shell each listing its views list records of, as the session follows it.
    fn publish_listings(&self, services: &Services) {
        for (ty, resource) in &self.listings {
            if let Some(reading) = services.read(resource) {
                self.link
                    .publish_reading(tilolang_tilo::runtime::foreign::LISTING, ty, reading);
            }
        }
    }

    /// Hand the shell who is in `workspace`, the one in front, as the session's view of it has
    /// them: its members' resource, the same rows its governance answers a read of that resource
    /// with, so a view names a person and draws their face from them. The shell keeps its record
    /// in the person's own workspace, whose members are the person alone, so its host takes these
    /// rows as a reading ([`tilolang_tilo::host::Host::serve_member_reading`]) rather than
    /// reading a store of its own.
    ///
    /// A workspace keeps no name of its own for a member who has not given it one, and a person's
    /// own name is what their devices say of them here (`people`, the session's roster): a row
    /// without one is named by its person's, so the bar names them and draws their face by it.
    pub(crate) fn publish_members(
        &mut self,
        view: Option<&std::sync::Arc<tilo_session_client::workspace_view::WorkspaceView>>,
        people: Option<&Value>,
        workspace: &str,
    ) {
        let Some(view) = view else {
            return;
        };
        if workspace.is_empty() {
            return;
        }
        let revision = view.revision();
        // A name the roster newly gives names a row the workspace keeps none for: the members
        // are read again for it, though the workspace's own view has not moved.
        let named_by = people.cloned();
        let named_moved = named_by != self.members_named_by;
        let seen = self.members_seen.as_ref();
        if !named_moved && seen.is_some_and(|(held, was)| held == workspace && *was == revision) {
            return;
        }
        self.members_named_by = named_by;
        let Ok(members) = view.members(workspace) else {
            return;
        };
        self.members_seen = Some((workspace.to_owned(), revision));
        // The name each person here goes by, by their root and by the device their presence
        // speaks from: whom the workspace keeps no name for is named by theirs. The person at
        // this device is named both by their root and by the device their session speaks from.
        let mine = hex::encode(view.person());
        let this_device = hex::encode(view.device());
        let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        if let Some(people) = people {
            let rows = people
                .get("rows")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(people.get("me"));
            for row in rows {
                let (Some(key), Some(name)) = (
                    row.get("key").and_then(Value::as_str),
                    row.get("name").and_then(Value::as_str),
                ) else {
                    continue;
                };
                if key.is_empty() || name.trim().is_empty() {
                    continue;
                }
                // A row of the person at this device names them `self`, and every other row its
                // person's root or, where their presence named none, the device it speaks from.
                if key == "self" {
                    named.insert(mine.clone(), name.to_owned());
                    named.insert(this_device.clone(), name.to_owned());
                } else {
                    named.insert(key.to_owned(), name.to_owned());
                }
            }
        }
        let owner = view.owner_of(workspace).ok();
        let people: Vec<Value> = members
            .members
            .iter()
            .filter(|member| !member.removed)
            .map(|member| {
                let mut row = member.clone();
                if member.name.trim().is_empty() {
                    let by_person = member.id.person_id().and_then(|person| named.get(person));
                    let by_device = members
                        .devices
                        .get(&member.id)
                        .into_iter()
                        .flatten()
                        .find_map(|device| named.get(&hex::encode(device)));
                    if let Some(name) = by_person.or(by_device) {
                        row.name.clone_from(name);
                    }
                }
                row.members_row(
                    member
                        .id
                        .person_id()
                        .is_some_and(|person| owner.as_deref() == Some(person)),
                )
            })
            .collect();
        let roots = tilolang_tilo::platform::platform_roots();
        let Some((_, ty)) = roots
            .iter()
            .find(|(published, _)| *published == tilolang_tilo::platform::WORKSPACE_MEMBERS)
        else {
            return;
        };
        self.link.publish_reading(
            tilolang_tilo::platform::WORKSPACE_MEMBERS,
            ty,
            serde_json::json!({ "people": people }),
        );
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
        // One of the device's settings is read on this device.
        let device = tilo_app::framework::settings_device();
        for (resource, ty) in tilolang_tilo::platform::platform_roots() {
            // The workspace's members are the host's to read in its workspace, and it hands them
            // to its views itself ([`tilolang_tilo::host::Host::reads_in_workspace`]). No service
            // of this device serves them, so publishing "no service" here would hand the shell an
            // empty roster — every face on the bar drawn as an icon, every follower's mark gone —
            // for as long as nothing published the resource again.
            if resource == tilolang_tilo::platform::WORKSPACE_MEMBERS {
                continue;
            }
            let reading = services
                .read(&tilolang_tilo::platform::on_device(&resource, &device))
                .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
            self.link.publish_reading(&resource, &ty, reading);
        }
        self.publish_listings(services);
    }

    /// Hand the shell what its places show, from what `services` publish: the package of each app a
    /// place shows a resource of, so the place draws it through that app's own views (and none of
    /// an app no place shows); the reading of each resource a place names; and the reading of each
    /// resource those views read, as they read it (an item of a collection a reading lists by id).
    /// Each reading is read again only when its service says it moved, so a change costs the shell
    /// only the resources it touched.
    pub(crate) fn follow_places(&mut self, layout: &LayoutReading, services: &mut Services) {
        let mut wanted = self.link.take_wanted();
        // A view listing an app's records with a query: the session follows that listing from
        // now on, and the shell is handed it each time it moves ([`Self::publish_listings`]).
        let listed = self.listings.len();
        wanted.retain(|id, ty| {
            let listing = (id == tilolang_tilo::runtime::foreign::LISTING)
                .then(|| tilolang_tilo::platform::listing_of(ty))
                .flatten();
            let Some((app, _)) = listing else {
                return true;
            };
            self.listings.insert(ty.clone(), app);
            false
        });
        if self.listings.len() != listed {
            self.publish_listings(services);
        }
        // A view composed in a place read a resource of a type no loaded package declared: its
        // app's package is published like those a place shows, so the place draws it through that
        // app's own views.
        let mut asked = false;
        for app in self.link.take_wanted_packages().into_keys() {
            asked |= self.needed.insert(app);
        }
        // The store moved: every package it holds is sent again, so a reinstalled style's new
        // module reaches the shell even though its package id did not change.
        if std::mem::take(&mut self.republish) {
            asked = true;
        }
        // What is live is followed as what a place shows is: the bar, quick settings and the
        // lock screen show each live resource's own `view live`.
        let live: Vec<String> = self
            .lists
            .live
            .read()
            .rows
            .into_iter()
            .map(|row| row.resource)
            .filter(|resource| !resource.is_empty())
            .collect();
        let live_moved = live != self.live;
        let key = (services.revision(), layout.clone());
        let moved_on = asked || live_moved || self.followed.as_ref() != Some(&key);
        if !moved_on && wanted.is_empty() {
            return;
        }
        // The apps whose views this process links as Rust apps, which draw their resources
        // without a package of their own.
        let rust_views = tilolang_tilo::views::declared_apps();
        let mut fresh = Vec::new();
        if asked || live_moved || self.followed.as_ref().map(|(_, was)| was) != Some(layout) {
            fresh = self.publish_place_packages(layout, services, &live, &rust_views);
        }
        for (id, ty) in wanted {
            let app = tilolang_tilo::readings::app_of(&ty);
            let shown = app.is_some_and(|app| {
                self.link.package_apps().iter().any(|held| held == app) || rust_views.contains(&app)
            });
            // A service answers another app's resource by the address the session publishes it
            // under (`tl:<app>:<id>`), where the shell's views name it as the app's store does
            // (`chat.workspace/<item>`): both parse as the one resource, so it is followed — and
            // published, and read again when it moves — by the address.
            let id =
                tilolang_service::reading_address(&ty, &id, |app| services.module(app).is_some());
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
        let device = tilo_app::framework::settings_device();
        for (id, ty) in fresh {
            let reads = self.reads.entry(id.clone()).or_default();
            *reads = reads.saturating_add(1);
            if let Some(reading) = services.read(&tilolang_tilo::platform::on_device(&id, &device))
            {
                self.link.publish_reading(&id, &ty, reading);
            }
        }
        self.followed = Some(key);
        self.live = live;
    }

    /// Publish the package of every app a place shows a resource of, and follow each resource's
    /// type: what a place draws through its app's own views. What was read here afresh, for the
    /// readings this pass reads.
    fn publish_place_packages(
        &mut self,
        layout: &LayoutReading,
        services: &Services,
        live: &[String],
        rust_views: &[&str],
    ) -> Vec<(String, String)> {
        let mut fresh: Vec<(String, String)> = Vec::new();
        // Every app a place shows is published with this pass, however this pass was asked for:
        // a package a view asked for arrives beside them, and none of them goes with it.
        let resources = services.resources();
        let placed = tilolang_service::placed_documents(
            layout
                .places
                .iter()
                .map(|place| place.resource.as_str())
                .chain(live.iter().map(String::as_str)),
            &resources,
        );
        let mut packages = std::collections::BTreeMap::new();
        let app_of = tilolang_tilo::readings::app_of;
        let mut apps: std::collections::BTreeSet<&str> =
            self.needed.iter().map(String::as_str).collect();
        apps.extend(placed.values().filter_map(|ty| app_of(ty)));
        for app in apps {
            if let Some(module) = services
                .module(app)
                .or_else(|| crate::styles::module_from_store(app))
            {
                packages.insert(app.to_owned(), module);
            }
        }
        // A placed app's views call the `fn`s of the apps it uses (`discussion.said_about`):
        // those run as their own app's code here, so their packages are loaded with it, as
        // an app's own window loads them (`ResourceService::service_document`).
        let packages = crate::shell::part::readings::with_uses(packages, |app| {
            services
                .module(app)
                .or_else(|| crate::styles::module_from_store(app))
        });
        // A resource of an app no package is published of any more is followed no more, and
        // neither is one of an app whose views draw it as a Rust app when none of its views is
        // linked here any more.
        self.reading.retain(|_, ty| {
            app_of(ty).is_some_and(|app| packages.contains_key(app) || rust_views.contains(&app))
        });
        self.link.publish_packages(packages);
        // The verbs of the apps the shell's runtime does not load — Chat's channel and its
        // `send`, which the session's own service answers — so a place's view sends its
        // action to the app publishing it, as it sends any package's to that package's API.
        self.link
            .publish_published_actions(tilolang_tilo::published::from_resources(
                &resources,
                &self.link.packages_loaded(),
            ));
        for (id, ty) in placed {
            if !self.reading.contains_key(&id) {
                fresh.push((id.clone(), ty.clone()));
            }
            self.reading.insert(id, ty);
        }

        fresh
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

    /// Hand the shell the person's layout, which its views draw, whenever it changed.
    pub(crate) fn publish_layout(&self, reading: LayoutReading) {
        self.link.publish_layout(reading);
    }

    /// Follow the styles the device's daemon publishes (`shell.md` §6): when they moved — a
    /// package installed, reinstalled or taken away — every style package is published to the
    /// shell's parts again, so a style that failed and was reinstalled fixed draws again with no
    /// restart, and what failed in this session is cleared: the shell must draw a style again to
    /// learn whether it is fixed, and a store that moved is the only news the session gets of a
    /// reinstall. Cheap when nothing moved, which is what every frame runs.
    pub(crate) fn follow_styles(&mut self) {
        let revision = self.lists.styles.revision();
        if self.styles_seen.replace(revision) == Some(revision) {
            return;
        }
        self.republish = true;
        for style in self.lists.styles.get().offered {
            // The shell's own package is the one the part already runs, never one loaded
            // beside itself.
            if style.package != tilo_session_core::shell::SHELL_APP {
                self.needed.insert(style.package);
            }
        }
        self.lists.failed_styles.borrow_mut().recover(|_| true);
    }

    /// The link the session publishes the shell on and the shell asks through: what a test stages a
    /// reading on.
    pub(crate) fn link(&self) -> Rc<SessionLink> {
        Rc::clone(&self.link)
    }

    /// The directions the parts' views left over since the last call, in the order the person
    /// asked them: the desktop's focus moves that way, to the window beside the shell's controls.
    #[must_use]
    pub(crate) fn take_focus_steps(&self) -> Vec<part::FocusStep> {
        std::mem::take(&mut self.steps.borrow_mut())
    }

    /// Let the parts' own controls give the keyboard up: the focus left the shell for a window it
    /// reached, so the ring goes and the window answers the next press.
    pub(crate) fn release_focus(&self) {
        for (_, part) in &self.parts {
            part.borrow_mut().release_focus();
        }
    }

    /// The views that could not be shown since the last call, each an app, a view and why: what
    /// the parts reported (`shell.md` §6, "Nothing can lock a person out").
    pub(crate) fn take_view_failures(&self) -> Vec<tilolang_tilo::runtime::error::ViewFailure> {
        self.parts
            .iter()
            .flat_map(|(_, part)| part.borrow().take_view_failures())
            .collect()
    }

    /// The apps whose view failure ended since the last call, and the view calls that ran again
    /// and succeeded: a style that failed draws again (`shell.md` §6, "Nothing can lock a
    /// person out").
    pub(crate) fn take_view_recoveries(&self) -> (Vec<String>, Vec<(String, String)>) {
        let mut apps = Vec::new();
        let mut calls = Vec::new();
        for (_, part) in &self.parts {
            let (part_apps, part_calls) = part.borrow().take_view_recoveries();
            apps.extend(part_apps);
            calls.extend(part_calls);
        }
        (apps, calls)
    }

    /// A style whose view failure ended — its package loaded, or its call ran again and
    /// succeeded — loads again: the shell returns to the person's choice with no restart
    /// (`shell.md` §6). `app` is the app whose failure ended, and `style` the call's view when
    /// one names it (`app:view`).
    pub(crate) fn note_style_recovered(&self, app: &str, style: Option<&str>) {
        let named = style.map(|style| format!("{app}:{style}"));
        self.lists.failed_styles.borrow_mut().recover(|failed| {
            failed.split_once(':').map(|(package, _)| package) == Some(app)
                || named.as_deref() == Some(failed)
                || style == Some(failed)
        });
    }

    /// A style the person chose could not be shown: its entry says so with `failure`'s reason,
    /// and the shell draws the built-in strips while the person's choice stays as it was
    /// (`shell.md` §6, "Nothing can lock a person out"). The notice naming the style and why,
    /// once per failure, for the session to raise.
    ///
    /// The entry is marked by the style's own view reference: a failure names the app and the
    /// view it called (`me.styles:timeline`), and the style the switch lists is that pair.
    /// Fixing the package flips it back: the shell asks the runtime again, the call succeeds, and
    /// the next [`Self::take_view_failures`] has nothing to say of it.
    pub(crate) fn note_style_failure(
        &self,
        failure: &tilolang_tilo::runtime::error::ViewFailure,
    ) -> Option<crate::notices::Notice> {
        let style = if failure.view.contains(':') {
            failure.view.clone()
        } else {
            format!("{}:{}", failure.app, failure.view)
        };
        let offered = self.lists.styles.get().offered;
        let Some(found) = offered.iter().find(|s| s.style == style) else {
            // A view that is no style failed some other way (a place's own view): the
            // shell has nothing to say of it.
            return None;
        };
        if !self
            .lists
            .failed_styles
            .borrow_mut()
            .fail(&style, &failure.reason)
        {
            return None;
        }
        let name = if found.line.is_empty() {
            found.name.clone()
        } else {
            found.line.clone()
        };
        Some(crate::notices::style_failed_notice(&name, &failure.reason))
    }

    /// The surface the shell shows over its windows, by the name the views read (`d.here`): `none`
    /// at rest.
    pub(crate) fn surface(&self) -> String {
        self.link.surface()
    }

    /// Three fingers carrying the strip shown, as `scroll` of them: the row the shell draws the
    /// strip in scrolls under the front window's bar, as two fingers over it would scroll it, and
    /// glides on when they lift. The bar is the shell's wherever the window's content comes from —
    /// a well the app draws into, or a view the shell composes — so the one aim moves the row for
    /// every window. The pointer stays at `pointer` for everything else the part shows.
    pub(crate) fn carry_strip(
        &self,
        handle: &tilo_session_core::desktop::state::DesktopHandle,
        scroll: tilo_session_core::surface::Scroll,
        pointer: (f64, f64),
    ) {
        let front = handle.read(|tree| {
            let strip = tree.active_strip_id()?;
            let group = tree.active_group_for(strip)?;
            tree.active_workspace()?
                .strips
                .iter()
                .find(|held| held.id == strip)?
                .window_groups
                .iter()
                .find(|held| held.id == group)
                .map(|held| held.active_tab)
        });
        let Some(name) = front.and_then(|tab| self.link.place_name(&tab.to_string())) else {
            return;
        };
        for (_, part) in &self.parts {
            let mut part = part.borrow_mut();
            if part.placement().is_none() {
                continue;
            }
            // The row scroll stands under the front window's bar, which the frame laid out
            // says its middle without the accessibility tree a reader's walk builds for every
            // widget. A window carried past the part's edge takes its bar with it: the fingers
            // then carry the row filling the part.
            let aim = part
                .bar_bounds(&name)
                .map(|(x, y, width, height)| (x + width / 2.0, y + height / 2.0))
                .filter(|&(x, y)| part.accepts_pointer(x, y))
                .or_else(|| part.row_pointer());
            let Some((x, y)) = aim else {
                continue;
            };
            part.pointer_motion(x, y);
            part.pointer_axis(scroll);
            if part.accepts_pointer(pointer.0, pointer.1) {
                part.pointer_motion(pointer.0, pointer.1);
            } else {
                part.pointer_leave();
            }
            return;
        }
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

    /// Whether the shell asked something of the API the session has not run yet.
    pub(crate) fn has_asks(&self) -> bool {
        self.link.has_asks()
    }

    /// The shell's own field `field` (a field of one of its roots: `dock_edge` and `frames_snap`
    /// of the `Shell` root, `style` of the `Viewing` one) as its store holds it now: what a test
    /// proves a shell choice survived a session by, read the way the shell's views read it.
    pub(crate) fn record_setting(&self, field: &str) -> Option<Value> {
        let compiled = self.packages.last()?;
        let meta = &compiled.meta;
        // The root that declares the field, as the shell's named roots name them.
        let (root, held) = meta.named_roots.iter().find_map(|root| {
            let field = meta
                .types
                .iter()
                .find(|ty| ty.id == root.ty)?
                .fields
                .iter()
                .find(|held| held.name == field)?;
            Some((root, field))
        })?;
        let host = tilolang_tilo::host::Host::open(
            self.record.clone(),
            tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person(
                self.person.as_str(),
            )),
            None,
            || None,
        )
        .ok()?;
        host.load_module(&compiled.wasm, meta.clone(), String::new())
            .ok()?;
        let at = tilolang_tilo::runtime::value::ResourceRef::root(
            format!("root-{}", root.ty),
            root.ty.clone(),
        );
        let value = host.runtime().read(&at, &held.id).ok()?;
        tilolang_tilo::runtime::value::to_json(&value).ok()
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
    ///
    /// A shell whose records the person's workspace keeps publishes nothing here: the device's
    /// daemon answers for them, so a REPL's or an agent's change runs the same action a press
    /// does and reaches their other devices, never a copy only this session reads.
    pub(crate) fn share_records(
        &self,
        services: &std::rc::Rc<std::cell::RefCell<tilo_service::Services>>,
        surfaces: &crate::mcp_server::SessionServices,
    ) {
        if self.kept_elsewhere {
            return;
        }
        let Some(compiled) = self.package() else {
            return;
        };
        let store = self.record.clone();
        let taken = services.borrow_mut().share(&tilo_service::Share {
            app: &compiled.meta.app,
            wasm: &compiled.wasm,
            store: &store,
        });
        match taken {
            Ok(()) => surfaces.keep_records_of(&compiled.meta.app),
            Err(error) => tracing::error!(%error, "the shell's records are not published"),
        }
    }

    /// Run what the shell asked through `invoke` (the session's API).
    /// The shell's calls (`session.close_place(place: 3)`) reach the API as any caller's do, a
    /// list as a list and a map as an object; each is read here as the type the action itself
    /// declares for it: a workspace's key is text even when it was given as digits, and a flag
    /// the shell's runtime keeps as a number is a flag.
    /// `types` answers with the declared type of `(resource, action)` 's argument `name` , when the
    /// API publishes it.
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
                    (name.clone(), typed(value.clone(), ty))
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

    /// Run the shell's action `action` with `arguments`, as pressing its control in the shell's
    /// own views does.
    pub(crate) fn run_action(&self, action: &str, arguments: Vec<Value>) -> Result<(), String> {
        let (_, part) = self
            .parts
            .first()
            .ok_or_else(|| "the shell has no display".to_owned())?;
        part.borrow_mut().run_action(action, arguments)
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
    pub(crate) fn set_theme(&mut self, theme: &tilo_ui_base::Theme) {
        self.theme = theme.clone();
        for (_, part) in &self.parts {
            part.borrow_mut().set_theme(theme.clone());
        }
    }

    /// What the shell refused since the session started, in its words: every display's, and what
    /// the API refused of what it asked.
    pub(crate) fn refusals(&self) -> Vec<String> {
        self.parts
            .iter()
            .flat_map(|(_, part)| part.borrow().refusals().to_vec())
            .chain(self.link.refused())
            .collect()
    }

    /// Run the person's changes of the shell's own records on the device's dispatch, as their own
    /// calls of the shell's actions in `workspace`, the one in front: the call an agent or a
    /// press on their desktop page makes. The daemon runs each on the record in their own
    /// workspace and keeps who made it; the shell draws the change as it takes the record back
    /// in ([`Self::record_moved`]). Nothing waits for the answer; what the dispatch refuses is
    /// said with the shell's refusals.
    pub(crate) fn own_calls(
        &self,
        daemon: &tilo_session_client::daemon_documents::DaemonShell,
        workspace: &str,
    ) {
        let refused = std::mem::take(
            &mut *self
                .dispatch_refusals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for why in refused {
            self.link.refuse(why);
        }
        for (address, arguments) in self.link.take_own_calls() {
            if workspace.is_empty() {
                self.link.refuse(format!(
                    "{} on {}: no workspace is in front to change it in",
                    address.action, address.resource
                ));
                continue;
            }
            let request = tilo_actions::remote::InvokeRequest {
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                // Answered once the change is made where the provider can hold its answer that
                // long; one that answers it only took the change is followed to its end.
                wait_ms: Some(tilo_actions::remote::MAX_REMOTE_WAIT_MS),
                ..tilo_actions::remote::InvokeRequest::plain(
                    tilo_actions::remote::RemoteActionTarget {
                        provider: tilo_actions::remote::application_provider_token(
                            workspace, &self.app,
                        ),
                        address: address.clone(),
                    },
                    serde_json::Map::new(),
                )
            };
            let (daemon_at, workspace) = (daemon.clone(), workspace.to_owned());
            let refusals = std::sync::Arc::clone(&self.dispatch_refusals);
            daemon.runtime().spawn(async move {
                if let Err(why) = own_call_made(&daemon_at, &workspace, request).await {
                    tracing::warn!(
                        action = %address.action,
                        %why,
                        "the person's change of their shell was refused"
                    );
                    refusals
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("{} on {}: {why}", address.action, address.resource));
                }
            });
        }
        // The person's answers to their asked records go to the invocations of the workspace
        // each record is in, whichever workspace is in front.
        for (workspace, address, arguments) in self.link.take_answers() {
            let request = tilo_actions::remote::InvokeRequest {
                argument: tilo_actions::remote::InvokeArgument::Json(arguments),
                ..tilo_actions::remote::InvokeRequest::plain(
                    tilo_actions::remote::RemoteActionTarget {
                        provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
                        address: address.clone(),
                    },
                    serde_json::Map::new(),
                )
            };
            let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
            let daemon_at = daemon.clone();
            let refusals = std::sync::Arc::clone(&self.dispatch_refusals);
            daemon.runtime().spawn(async move {
                if let Err(why) = daemon_at.dispatch(Some(&workspace), &message).await {
                    tracing::warn!(action = %address.action, %why, "the person's answer was refused");
                    refusals
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("{} on {}: {why}", address.action, address.resource));
                }
            });
        }
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

    /// What the parts' layouts said went wrong since this was last asked.
    pub(crate) fn take_layout_reports(&self) -> Vec<String> {
        self.parts
            .iter()
            .filter_map(|(_, part)| part.borrow_mut().take_layout_report())
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

    /// Where a shown part draws `text`: its box cut to the clip of the scroll view it stands in.
    pub(crate) fn drawn_text_bounds(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .find_map(|(_, part)| part.borrow_mut().drawn_text_bounds(text))
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

    /// What the part on `output` shows as a page reads it (its bar, its strips and their
    /// windows as elements): `None` while the shell shows no part there.
    pub(crate) fn composition(&self, output: &str) -> Option<ui::composition::Composition> {
        let (_, part) = self
            .parts
            .iter()
            .find(|(name, part)| name == output && part.borrow().placement().is_some())?;
        Some(part.borrow().composition())
    }

    /// The scene the part on `output` last drew: `None` while the shell shows no part there.
    pub(crate) fn scene(&self, output: &str) -> Option<ui::vello::Scene> {
        let (_, part) = self
            .parts
            .iter()
            .find(|(name, part)| name == output && part.borrow().placement().is_some())?;
        Some(part.borrow().scene().clone())
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
/// `workspaces` — the workspaces the shell holds — among the scopes it serves, the one in front
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
fn pictured(handle: &tilo_session_core::desktop::state::DesktopHandle, key: &str) -> Option<u64> {
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

/// `value` as the type the action declares for it: a number given for a text is its digits, and a
/// flag the shell's runtime keeps as an integer (`1`, `0`) is true or false. Anything else is sent
/// as given, and the API refuses by name what it does not take.
fn typed(value: Value, ty: Option<tilo_actions::params::ParamType>) -> Value {
    use tilo_actions::params::ParamType;
    match (ty, value) {
        (Some(ParamType::Boolean), Value::Number(number)) => {
            Value::Bool(number.as_f64().is_some_and(|number| number != 0.0))
        }
        (
            Some(ParamType::Text | ParamType::Resource | ParamType::DateTime | ParamType::Secret),
            Value::Number(number),
        ) => Value::String(number.to_string()),
        (_, value) => value,
    }
}

/// Move the desktop's focus from the window in front back to the shell's own controls: a
/// direction the window's views had nothing left for is the shell's, and the shell's ring seats
/// the way it points, so the bar, dock and a window's controls are one order under the keyboard
/// and a controller.
pub(crate) fn hand_focus_to_shell(
    shared: &crate::Shared,
    compositor: &mut Compositor,
    direction: part::FocusStep,
) {
    // A sheet over the desktop takes the keys of its own: nothing falls through to the shell
    // behind it.
    if shared.sheet_up.get() {
        return;
    }
    // The shell answers the keys while the desktop's focus is its own; the press that left the
    // window reaches it here, through the same key route a keyboard's press takes.
    shared.shell_seat.set(true);
    // What a key sends, evdev codes: the arrow the press stood for.
    let keycode = match direction {
        part::FocusStep::Up => 103,
        part::FocusStep::Down => 108,
        part::FocusStep::Left => 105,
        part::FocusStep::Right => 106,
    };
    for pressed in [true, false] {
        drop(compositor.input(tilo_compositor::InputEvent::Key { keycode, pressed }));
    }
}

/// `desktop:chrome` as the session publishes it: each verb is carried out on `desktop`, the desktop
/// the session owns, and one that opens a surface opens the shell's view of it on `surfaces` — the
/// overview, the notifications, the quick settings, About and the workspaces. What only the session
/// answers (a person's card, the roster) is asked of it through `asks`.
pub(crate) fn chrome_surface(
    desktop: tilo_session_core::desktop::state::DesktopHandle,
    surfaces: crate::device_lists::Surfaces,
    shortcuts: crate::shell::mount::Shortcuts,
    asks: Rc<RefCell<Vec<tilo_session_core::desktop::host::contract::DesktopAsk>>>,
) -> tilo_session_core::surfaces::Shared {
    Rc::new(RefCell::new(ShellChrome {
        desktop,
        surfaces,
        shortcuts,
        asks,
    }))
}

/// The shell's published side as the shell stands under the shell: its `search` opens the shell's
/// launcher (on what the person asked for, when a query comes with it), as the desktop's own search
/// draws nothing under the shell; a search with a query still runs, its rows published for the
/// palette and the agents.
pub(crate) fn lists_surface(
    desktop: crate::platform::surfaces::ShellDesktop,
    surfaces: crate::device_lists::Surfaces,
) -> tilo_session_core::surfaces::Shared {
    Rc::new(RefCell::new(ShellLists { desktop, surfaces }))
}

/// The chrome's surface on the shell ([`chrome_surface`]).
struct ShellChrome {
    desktop: tilo_session_core::desktop::state::DesktopHandle,
    surfaces: crate::device_lists::Surfaces,
    /// The desktop's one shortcut table, which the chrome's resource names beside its verbs.
    shortcuts: crate::shell::mount::Shortcuts,
    /// What only the session answers: a person's card and the roster.
    asks: Rc<RefCell<Vec<tilo_session_core::desktop::host::contract::DesktopAsk>>>,
}

impl ShellChrome {
    /// The surface `address` opens or closes on the shell, when it is one of the chrome's ways of
    /// showing one: `Ok(None)` for every other action.
    fn show_surface(
        &self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, tilo_actions::ApiError>
    {
        if address.resource != tilo_session_core::desktop::verbs::CHROME_RESOURCE {
            return Ok(None);
        }
        // The strips list is the shell's own view under the shell: `show-strips` opens it and a
        // strip chosen closes it, so nothing of the chrome's strip switcher draws.
        if address.action == "show-strips" {
            self.surfaces.toggle(crate::device_lists::Surfaces::STRIPS);
            return Ok(Some(
                tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            ));
        }
        // Any other action of the chrome's puts the strips list away, as a menu closes once
        // something is chosen: showing a strip, stepping along them, switching workspace.
        self.surfaces.close(crate::device_lists::Surfaces::STRIPS);
        let here = |name: &str| self.surfaces.overlays.here().name() == name;
        let started = match address.action.as_str() {
            "overview" => {
                let open = here(tilo_session_core::desktop::overlays::Overlay::Overview.name());
                if opens(address, arguments, open)? {
                    self.surfaces
                        .open(tilo_session_core::desktop::overlays::Overlay::Overview.name());
                } else {
                    self.surfaces
                        .close(tilo_session_core::desktop::overlays::Overlay::Overview.name());
                }
                true
            }
            "notifications" => {
                let name = tilo_session_core::desktop::overlays::Overlay::Centre.name();
                if opens(address, arguments, here(name))? {
                    self.surfaces.open(name);
                } else {
                    self.surfaces.close(name);
                }
                true
            }
            "quick-settings" => {
                self.surfaces
                    .toggle(tilo_session_core::desktop::overlays::Overlay::QuickSettings.name());
                true
            }
            // The About sheet is the shell's own view where the shell draws the screen: this
            // computer's name, which Tilo it runs, and where to read more.
            "about" => {
                let open = opens(address, arguments, self.surfaces.about_open())?;
                if open {
                    self.surfaces.open(crate::device_lists::ABOUT);
                } else {
                    self.surfaces.close(crate::device_lists::ABOUT);
                }
                true
            }
            // The history, opened on what the person opened lately of an app or a type: the
            // scoped way into the one history surface, as a search button is into search.
            "recents" => {
                let named = |key: &str| match arguments.get(key) {
                    None | Some(Value::Null) => Ok(String::new()),
                    Some(Value::String(text)) => Ok(text.trim().to_owned()),
                    Some(other) => Err(tilo_actions::ApiError::InvalidArguments(format!(
                        "`recents` takes `{key}` as text, got {other}"
                    ))),
                };
                self.surfaces.history(&named("app")?, &named("kind")?);
                true
            }
            // Who is here: the shell draws its own roster over the windows, so the bar's faces
            // open the shell's surface rather than the hidden chrome's panel.
            "toggle-people" => {
                let name = tilo_session_core::desktop::overlays::Overlay::People.name();
                if here(name) {
                    self.surfaces.person.borrow_mut().clear();
                    self.surfaces.close(name);
                } else {
                    self.surfaces.person.borrow_mut().clear();
                    self.surfaces.open(name);
                }
                true
            }
            // A person's face, or a row in the roster: the shell's roster opens on their card.
            "show-person" => {
                let person = arguments
                    .get("person")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                *self.surfaces.person.borrow_mut() = person;
                self.surfaces
                    .open(tilo_session_core::desktop::overlays::Overlay::People.name());
                true
            }
            // The person's own face: the roster opens on their own card.
            "show-self" => {
                crate::people_device::SELF.clone_into(&mut self.surfaces.person.borrow_mut());
                self.surfaces
                    .open(tilo_session_core::desktop::overlays::Overlay::People.name());
                true
            }

            // The workspaces are the shell's one workspace surface: the shell's menu, the workspaces
            // a person switches between and the actions on them.
            "workspace-menu" => {
                self.surfaces
                    .toggle(tilo_session_core::desktop::overlays::Overlay::Workspaces.name());
                true
            }
            _ => false,
        };
        Ok(started.then_some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl ShellChrome {
    /// The chrome's verb at `address`, carried out on the desktop the session owns: `Ok(None)` for
    /// one that is a surface only the chrome draws.
    fn carry(
        &self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<Option<tilo_ui_base::host_operation::ResourceOperationStart>, tilo_actions::ApiError>
    {
        use tilo_session_core::desktop::navigation::{ChromeEffect, Verb};
        use tilo_session_core::desktop::state::chrome_verbs::Carried;
        if address.resource != tilo_session_core::desktop::verbs::CHROME_RESOURCE {
            return Ok(None);
        }
        let published = [tilo_session_core::desktop::verbs::chrome_resource(
            &self.shortcuts.borrow(),
        )];
        let encoded = tilo_actions::api::encode_json(&published, address, arguments)?;
        let verb = match Verb::from_payload(&encoded.payload) {
            Ok(Verb::Chrome(verb)) => verb,
            Ok(Verb::Layout(_)) => return Ok(None),
            Err(error) => return Err(tilo_actions::ApiError::InvalidArguments(error)),
        };
        let value = match self
            .desktop
            .perform_verb(&verb)
            .map_err(tilo_actions::ApiError::Failed)?
        {
            Carried::Done => None,
            Carried::Answer(value) => Some(value),
            Carried::Shortcut(performed) => {
                self.performed(performed);
                None
            }
            // The strips of the workspace shown, each where it stands: the overview.
            Carried::Shell if verb == ChromeEffect::ShowStrips => {
                self.surfaces
                    .open(tilo_session_core::desktop::overlays::Overlay::Overview.name());
                None
            }
            Carried::Shell => return Ok(None),
        };
        Ok(Some(
            tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput {
                    value,
                    ..tilo_actions::ActionOutput::default()
                },
            ),
        ))
    }

    /// Answer what the desktop left of a shortcut to the shell: whether that was all of it. The
    /// overview, the emoji picker and the launcher (the app picker a new window opens) are the
    /// shell's surfaces; the strip switcher shows the strips in the overview, and a report is the
    /// session's to ask for. The session under the shell was asked for its own already, and
    /// performing a shortcut changes nothing until it is done, so asking twice does it once.
    fn performed(&self, performed: tilo_session_core::desktop::state::shortcuts::Performed) {
        use tilo_session_core::desktop::overlays::Overlay;
        use tilo_session_core::desktop::state::shortcuts::{Performed, ShellPart};
        match performed {
            // The session under the shell was asked already.
            Performed::Done | Performed::Session => {}
            Performed::Shell(ShellPart::Overview | ShellPart::StripSwitcher) => {
                self.surfaces.toggle(Overlay::Overview.name());
            }
            Performed::Shell(ShellPart::EmojiPicker) => {
                self.surfaces.toggle(crate::device_lists::EMOJI);
            }
            Performed::AppPicker => self.surfaces.open(Overlay::Launcher.name()),
            Performed::Shell(ShellPart::ReportProblem) => {
                tilo_session_core::desktop::shell_verbs::ask(
                    &self.desktop,
                    &self.asks,
                    tilo_session_core::desktop::host::contract::DesktopAsk::ReportProblem,
                );
            }
        }
    }
}

/// Whether the chrome's action at `address` opens its surface, given whether it is up (`here`): an
/// action that names `open` says so, and one that does not toggles.
fn opens(
    address: &ActionAddress,
    arguments: &Map<String, Value>,
    here: bool,
) -> Result<bool, tilo_actions::ApiError> {
    match arguments.get("open") {
        None | Some(Value::Null) => Ok(!here),
        Some(Value::Bool(wanted)) => Ok(*wanted),
        Some(other) => Err(tilo_actions::ApiError::InvalidArguments(format!(
            "`{}` takes `open` as a boolean, got {other}",
            address.action
        ))),
    }
}

impl tilo_session_core::surfaces::Published for ShellChrome {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        let mut resources = vec![tilo_session_core::desktop::verbs::chrome_resource(
            &self.shortcuts.borrow(),
        )];
        // The shell's own overview answers to what people call it: its words are the action's
        // own search keywords, so the platform's search finds it by any of them.
        for resource in &mut resources {
            if resource.id.as_str() != tilo_session_core::desktop::verbs::CHROME_RESOURCE {
                continue;
            }
            for action in &mut resource.actions {
                if action.id == "overview" {
                    *action =
                        action
                            .clone()
                            .with_keywords(["expose", "mission control", "all windows"]);
                }
            }
        }
        resources
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        _look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        if let Some(started) = self.show_surface(address, arguments)? {
            return Ok(started);
        }
        self.carry(address, arguments)?
            .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// The shell's lists on the shell ([`lists_surface`]).
struct ShellLists {
    /// The desktop whose windows the shell frames: its verbs, its search and what the windows'
    /// own apps describe of their content, which the shell lists beside its controls.
    desktop: crate::platform::surfaces::ShellDesktop,
    surfaces: crate::device_lists::Surfaces,
}

impl tilo_session_core::surfaces::Published for ShellLists {
    fn resources(&mut self) -> Vec<tilo_actions::ResourceSpec> {
        tilo_session_core::surfaces::Published::resources(&mut self.desktop)
    }

    fn invoke(
        &mut self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        if address.resource == tilo_session_core::desktop::verbs::SHELL_RESOURCE
            && address.action == "search"
        {
            if let Some(Value::String(query)) = arguments.get("query")
                && !query.is_empty()
            {
                // The shell runs its search as it always does, so its rows are published on its
                // search resource for the palette and the agents; the shell's launcher is what
                // draws, and it opens on what was asked for.
                let started = tilo_session_core::surfaces::Published::invoke(
                    &mut self.desktop,
                    address,
                    arguments,
                    look,
                )?;
                self.surfaces.overlays.search(query);
                return Ok(started);
            }
            // A scope with nothing typed opens the search over what it reaches (a window's find
            // handed on with its field empty): it never closes the search already up, which is
            // what the key alone does.
            if arguments.get("scope").is_some() {
                let started = tilo_session_core::surfaces::Published::invoke(
                    &mut self.desktop,
                    address,
                    arguments,
                    look,
                )?;
                self.surfaces
                    .open(tilo_session_core::desktop::overlays::Overlay::Launcher.name());
                return Ok(started);
            }
            self.surfaces
                .toggle(tilo_session_core::desktop::overlays::Overlay::Launcher.name());
            return Ok(
                tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            );
        }
        // The platform search API (a window's search control, its toolbar's magnifier, Ctrl+K)
        // asks for the shell's search under the shell too: the shell draws that search, so the
        // request opens the launcher, and its query runs the shell's search as the shell's own
        // `search` verb does, its rows published for whoever reads them.
        if ui::widget::search_request::is_search_address(address) {
            let query = ui::widget::search_request::SearchRequest::from_arguments(arguments)
                .query
                .filter(|query| !query.is_empty());
            let started = match &query {
                Some(query) => {
                    let asked =
                        Map::from_iter([("query".to_owned(), Value::String(query.clone()))]);
                    tilo_session_core::surfaces::Published::invoke(
                        &mut self.desktop,
                        &ActionAddress::new(
                            tilo_session_core::desktop::verbs::SHELL_RESOURCE,
                            "search",
                        ),
                        &asked,
                        look,
                    )?
                }
                None => tilo_ui_base::host_operation::ResourceOperationStart::Completed(
                    tilo_actions::ActionOutput::default(),
                ),
            };
            match &query {
                Some(query) => self.surfaces.overlays.search(query),
                None => self
                    .surfaces
                    .toggle(tilo_session_core::desktop::overlays::Overlay::Launcher.name()),
            }
            return Ok(started);
        }
        tilo_session_core::surfaces::Published::invoke(&mut self.desktop, address, arguments, look)
    }

    fn invoke_as(
        &mut self,
        actor: &tilo_actions::Actor,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        look: &mut tilo_session_core::surfaces::Look<'_>,
    ) -> Result<tilo_ui_base::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        if ui::widget::search_request::is_search_address(address)
            || (address.resource == tilo_session_core::desktop::verbs::SHELL_RESOURCE
                && address.action == "search")
        {
            return self.invoke(address, arguments, look);
        }
        tilo_session_core::surfaces::Published::invoke_as(
            &mut self.desktop,
            actor,
            address,
            arguments,
            look,
        )
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        tilo_session_core::surfaces::Published::accessible_labels(&mut self.desktop)
    }

    /// The controls the windows' own apps describe of their content, each where
    /// it stands on the desktop: the shell replaced what the desktop draws, never
    /// the windows standing in it, so their controls reach the session as in the
    /// Rust shell.
    fn controls(&mut self) -> Vec<ui::window_targets::WindowControl> {
        self.desktop.window_controls()
    }

    fn press(&mut self, name: &str) -> Result<(), String> {
        tilo_session_core::surfaces::Published::press(&mut self.desktop, name)
    }

    fn press_identity(
        &mut self,
        identity: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        tilo_session_core::surfaces::Published::press_identity(&mut self.desktop, identity, action)
    }

    fn press_action(
        &mut self,
        name: &str,
        action: ui::window_targets::TargetAction,
    ) -> Result<(), String> {
        tilo_session_core::surfaces::Published::press_action(&mut self.desktop, name, action)
    }

    fn wants_keyboard(&self) -> bool {
        tilo_session_core::surfaces::Published::wants_keyboard(&self.desktop)
    }

    fn revision(&self) -> Option<u64> {
        tilo_session_core::surfaces::Published::revision(&self.desktop)
    }
}

/// Answer what the shell asks of the session: the actions its views press, over the device's
/// dispatch where one waits for the person, and the apps an app's own `open` starts.
fn serve_shell(shared: &crate::Shared, shell: &Shell, workspace: &str) {
    let (published, search) = (&shared.published, &shared.search);
    let resources = published.resources();
    // Where a press that waits for the person reaches the device's dispatch: it keeps the call
    // as their asked record, which the shell draws.
    let daemon = shared.daemon.clone();
    // The services that answer it over the same link: a press made before they are offered
    // waits for them, and one the dispatch refuses is said on the shell.
    let served = std::sync::Arc::clone(&shared.served_services);
    shell.serve(
        |resource, action, name| {
            tilo_actions::api::resolve(&resources, &ActionAddress::new(resource, action))
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
                    .perform(
                        workspace,
                        tilo_desktop_search::Target::Launch {
                            app: app.to_owned(),
                            place: tilo_desktop_search::LaunchPlace::Focus,
                        },
                        &mut |own| {
                            published
                                .invoke(
                                    &own.address,
                                    &own.arguments,
                                    &mut tilo_session_core::surfaces::Look::none(),
                                )
                                .map(drop)
                        },
                    )
                    .map_err(|error| error.to_string());
            }
            let address = ActionAddress::new(resource, action);
            // A press that waits for the person's yes is their asked record: the call goes to
            // the device's dispatch, which keeps it and says what it asks, and the shell draws
            // the question. Their yes sends the same call back here, which runs it confirmed;
            // anything else runs nothing. It waits where the action's contract asks, which
            // Shift skips only where Undo takes the action back, and where a press of it
            // confirms (`ActionSpec::confirms_for`): a delete Undo takes back unless Shift is
            // held, what Undo cannot take back and what leaves the system always. A question
            // asks a yes; one whose contract wants a typed phrase keeps its own flow.
            if !workspace.is_empty()
                && daemon.link().is_ok()
                && let Some((_, spec)) = tilo_actions::api::locate(&resources, &address)
                && let policy = spec.confirmation_for_json(arguments)
                && !matches!(policy, tilo_actions::ConfirmationPolicy::Phrase { .. })
                && let asks = spec
                    .confirms_for(Some(arguments), None)
                    .on_press(ui::widget::shift_held())
                && (asks
                    || (policy == tilo_actions::ConfirmationPolicy::Confirm
                        && !ui::widget::shift_skips_confirmation(spec.contract.can_be_undone())))
            {
                let request = tilo_actions::remote::InvokeRequest {
                    asks,
                    argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
                    ..tilo_actions::remote::InvokeRequest::plain(
                        tilo_actions::remote::RemoteActionTarget {
                            provider: crate::services::app_wire::SERVICES_PROVIDER.to_owned(),
                            address: address.clone(),
                        },
                        serde_json::Map::new(),
                    )
                };
                let message = tilo_actions::remote::RemoteActionMessage::Invoke(request);
                let workspace = workspace.to_owned();
                let waiting = daemon.clone();
                let served = std::sync::Arc::clone(&served);
                daemon.runtime().spawn(async move {
                    // The dispatch may not serve the session's services yet: the link opens
                    // while the shell is already drawing. The press waits for them rather than
                    // being dropped, and goes through once they are there; what the dispatch
                    // refuses is the person's to see on the shell, not a log line.
                    served
                        .deliver_press(|| async move {
                            waiting.dispatch(Some(&workspace), &message).await.map(drop)
                        })
                        .await;
                });
                return Ok(());
            }
            published
                .invoke(
                    &address,
                    arguments,
                    &mut tilo_session_core::surfaces::Look::none(),
                )
                .map(drop)
                .map_err(|error| error.to_string())
        },
    );
}

impl crate::Session {
    /// What the shell's views could not show, and what came back: a style that failed is marked
    /// not loading, with the notice naming it, and one that came back loads again — the shell
    /// keeps the person's choice either way (`shell.md` §6, "Nothing can lock a person out").
    fn follow_view_failures(
        notices: &Rc<RefCell<crate::notifications::SessionNotifications>>,
        shell: &Shell,
        failures: Vec<tilolang_tilo::runtime::error::ViewFailure>,
        recoveries: (Vec<String>, Vec<(String, String)>),
    ) {
        for failure in failures {
            if let Some(notice) = shell.note_style_failure(&failure) {
                // Raised through the session's own notices, as every notice it tells: the
                // notifications service when one runs, the shell's own otherwise.
                notices.borrow().tell(&notice);
            } else {
                // A view that is no style's failed some other way (a page's own), and the page
                // says so where it stands; a session's log is where whoever looks reads why.
                tracing::warn!(
                    app = %failure.app,
                    view = %failure.view,
                    why = %failure.reason,
                    "a shell view could not be shown"
                );
            }
        }
        let (apps, calls) = recoveries;
        for app in apps {
            shell.note_style_recovered(&app, None);
        }
        for (app, view) in calls {
            shell.note_style_recovered(&app, Some(&view));
        }
    }

    /// Draw the shell's own record from the person's own workspace once the device's daemon holds
    /// it, as the apps the session runs itself keep their resources in the workspace the person
    /// is in ([`crate::Shared::keep_app_stores`]), and draw what another of their devices or the
    /// device's dispatch (an agent's `show-style`, a press on a desktop page) wrote to it since the
    /// last pass. The store to draw it from, once the daemon holds the workspace.
    fn follow_shell_record(
        shell: &Shell,
        shared: &crate::Shared,
    ) -> Option<Rc<dyn tilo_service::WorkspaceStore>> {
        // A shell the daemon does not run (one no daemon started, or this build's own copy) keeps
        // its record in this session's memory.
        if !shell.kept_elsewhere {
            return None;
        }
        let (workspace, writes) = shared.shell_home.as_ref()?;
        if shell.record_kept() {
            use tokio::sync::broadcast::error::TryRecvError;
            let mut moved = false;
            loop {
                match writes.borrow_mut().try_recv() {
                    // A write that names no workspace is the feed falling behind.
                    Ok(write) => {
                        moved |= write.workspace.is_empty()
                            || (write.workspace == *workspace && write.app == shell.app);
                    }
                    Err(TryRecvError::Lagged(_)) => moved = true,
                    Err(TryRecvError::Empty | TryRecvError::Closed) => break,
                }
            }
            if moved {
                shell.record_moved();
            }
            return None;
        }
        // The daemon's own reading of what it holds, followed, rather than an ask on the loop: a
        // store is offered once the workspace is held here, and the pass never waits on it.
        let held = shared.view.as_ref().is_some_and(|view| {
            tilo_desktop_model::WorkspaceId::parse(workspace).is_some_and(|id| view.holds(id))
        });
        if !held {
            return None;
        }
        (shared.held_stores)(workspace.clone(), tilo_rt::app().handle())
    }

    /// What the screen says of its edges, as the displays publish it: it moves without the
    /// services, so a move is taken in at once and the bar follows the pointer this pass.
    fn publish_edges(&self) {
        if let Some(shell) = &self.shell
            && shell.publish_edges(&self.compositor, &self.displays_service)
        {
            self.shared.services.borrow_mut().poll();
        }
    }

    /// Hand the shell the shell when it changed, and run what it asked through the API, as any
    /// caller of the action protocol does.
    pub(crate) fn follow_shell(&mut self) -> Result<(), crate::Error> {
        self.publish_edges();
        let Some(shell) = self.shell.as_ref() else {
            return Ok(());
        };
        // The shell's views draw the centre over the shell, so while it is up the person is looking
        // at what the notices say: they are read, as they are while the panel's own centre shows
        // them. The bell's count, the centre's header and its rows read the one set, so they never
        // disagree. It is the same surface the views read (`d.here`), and the one place that knows
        // when it closes: however the person left it, the next arrival is a banner on the shell
        // again rather than a row behind a page that is gone.
        let centre =
            shell.surface() == tilo_session_core::desktop::overlays::Overlay::Centre.name();
        self.shared.notices.set_shell_centre(centre);
        if centre {
            self.shared.notices.mark_read();
        }
        let Some(shell) = self.shell.as_mut() else {
            return Ok(());
        };
        // The styles the device offers, followed as its daemon publishes them, so a style a
        // person or their agent installed is offered without a restart (shell.md §6).
        shell.follow_styles();
        // What the shell's views could not show — a style package that did not load, a view
        // that failed — so the shell says so and the person's choice is kept: a style that
        // cannot load shows the built-in strips until it is fixed (shell.md §6, "Nothing can
        // lock a person out").
        let failures = shell.take_view_failures();
        let recoveries = shell.take_view_recoveries();
        let notices = Rc::clone(&self.shared.notifications);
        Self::follow_view_failures(&notices, shell, failures, recoveries);
        // What the displays say while the person asked Settings which screen is which: each
        // part's own number and name, and the frame that takes them away.
        let identify = self.numbers.saying();
        shell.follow_deadline(self.numbers.deadline());
        let layout = self.shared.layout_reading();
        // Who is in the workspace in front, for the bar's faces and their names: the workspace's
        // members, as the session's view of the workspace has them.
        let front = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
                .unwrap_or_default()
        });
        let here = self
            .shared
            .services
            .borrow()
            .read(crate::people_device::RESOURCE);
        shell.publish_members(self.shared.view.as_ref(), here.as_ref(), &front);
        shell.publish_readings(&self.shared.services.borrow());
        shell.follow_places(&layout, &mut self.shared.services.borrow_mut());
        shell.publish_layout(layout);
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
        // The shell's own record: what survives the session ending, and what the person's other
        // devices draw.
        if let Some(store) = Self::follow_shell_record(shell, &self.shared) {
            shell.keep_record_in(store, &mut self.compositor, &mut self.shared.published);
        }
        // One part per display: a display plugged in, taken away or taken by a fullscreen window
        // moves its part.
        shell.follow_displays(&mut self.compositor, &mut self.shared.published, &identify);
        // The bar's band, as the chrome the parts laid out measured it: the shell's own views draw
        // the bar, and what the session draws under it — the surfaces anchored to the display's
        // top edge, a banner — stands at the same height.
        let (bar_band, _dock_band) = shell.bands();
        self.shared.overlays.set_bar_height(bar_band);
        // The cards their apps draw themselves stand where the shell's cards hold their place:
        // note the block each card keeps this pass, whether or not the shell asked anything of
        // the session, and one the shell shows none for has its app draw the activity itself.
        self.shared.live.place_drawn(|name| shell.control(name));
        let workspace = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
                .unwrap_or_default()
        });
        // What the person changed of the shell's own records: their calls of its actions where
        // their workspace keeps them. A shell the daemon runs keeps its record in the person's
        // own workspace, whichever is in front, and its instance publishes its actions there.
        let keeps = match &self.shared.shell_home {
            Some((home, _)) if shell.kept_elsewhere => home.as_str(),
            _ => workspace.as_str(),
        };
        shell.own_calls(&self.shared.daemon, keeps);
        let asks = shell.has_asks();
        if asks {
            serve_shell(&self.shared, shell, &workspace);
            // What ran may have asked the session in turn (a menu, a sheet).
            self.settle()?;
            if let Some(shell) = self.shell.as_mut() {
                shell.follow_deadline(self.numbers.deadline());
            }
        }
        // The shell's places are what the outside services keeping its records in step are due
        // for: each workspace with a place of an app's records has that app's connector, and one
        // that no longer shows any ends. Read every pass, whether or not the shell asked anything,
        // so a place made, moved between workspaces or closed, or the workspace in front becoming
        // the one the daemon holds, is followed on the pass it happened on, in every workspace of
        // the shell, not only the one in front (`docs/architecture/apps.md` §1, §8).
        crate::connectors::reconcile(self);
        // A direction the shell's views left over is the desktop's: the focus moves to the
        // window beside the bar or dock, and the window in front answers the next press. The
        // shell and the windows are one order under the keyboard and a controller.
        let steps = self
            .shell
            .as_ref()
            .map(Shell::take_focus_steps)
            .unwrap_or_default();
        if !steps.is_empty() {
            let mut reached = false;
            for step in steps {
                let forward = matches!(step, part::FocusStep::Down | part::FocusStep::Right);
                reached |= crate::windows::focus_beside(&self.shared.handle, forward);
            }
            // A window took the focus: the shell's own ring goes, so the window answers the next
            // press rather than the shell holding the keyboard for it.
            if reached && let Some(shell) = self.shell.as_ref() {
                shell.release_focus();
            }
            self.settle()?;
        }
        Ok(())
    }

    /// What the shell reads the shell through: a test stages a reading on it, as a collaborating
    /// session's presence would arrive.
    #[must_use]
    pub fn shell_link(&self) -> Option<Rc<crate::platform::surfaces::SessionLink>> {
        self.shell.as_ref().map(Shell::link)
    }

    /// Whether the shell's own record is kept in the person's own workspace: what a test waits for
    /// before it proves a shell choice survives the session.
    #[must_use]
    pub fn shell_record_kept(&self) -> bool {
        self.shell.as_ref().is_some_and(Shell::record_kept)
    }

    /// The shell's own record as a setting's value, by the field's name in its type (`dock_edge`,
    /// `frames_snap`), read through the shell's runtime as its own views read it: what a test
    /// proves a shell choice survived a restart by. `None` when the shell does not run or the field
    /// does not read.
    #[must_use]
    pub fn shell_setting(&self, field: &str) -> Option<Value> {
        self.shell.as_ref()?.record_setting(field)
    }

    /// The peer network this session takes part in, when it does: what a test hands it is
    /// who is here.
    #[must_use]
    pub fn sync(&self) -> Option<&Rc<crate::sync::Client>> {
        self.shared.sync.as_ref()
    }

    /// The notifications the shell keeps: what apps sent, and what the centre shows. A test posts
    /// into it as an app's arrival would land.
    #[must_use]
    pub fn notices(&self) -> &tilo_session_core::desktop::state::DesktopNotifications {
        &self.shared.notices
    }

    /// How much work the shell's views did since the session started: `None` when its package did
    /// not start.
    #[must_use]
    pub fn shell_work(&self) -> Option<crate::platform::surfaces::ShellWork> {
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

    /// What the shell's layout said went wrong since this was last asked: each frame it skipped
    /// because its layout failed, and each part that did not settle and was drawn as it last
    /// stood, said once. A test that drives the canvas reads that none was.
    #[must_use]
    pub fn take_shell_layout_reports(&self) -> Vec<String> {
        self.shell
            .as_ref()
            .map(Shell::take_layout_reports)
            .unwrap_or_default()
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

    /// Where the shell *draws* `text`, logical `(x, y, w, h)` : the first node that says it, its
    /// box cut to the clip of the scroll view it stands in. A row a transcript has scrolled out
    /// of its room is drawn nowhere; this is what a person sees of it, never where the layout
    /// put it.
    #[must_use]
    pub fn shell_drawn_text_bounds(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.drawn_text_bounds(text)
    }

    /// Where every node the shell lays out that says `text` stands, logical `(x, y, w, h)`: every
    /// label, description or value holding it, on every shown part. A test asks what the whole
    /// shell says of a name, not what the first one of it says.
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

    /// What the shell shows on `output` as a host that writes it as semantic HTML reads it
    /// (`ui::composition`): the bar, its strips and their windows as elements, each with the
    /// words it shows. `None` when the shell does not run or shows no part on that output.
    #[must_use]
    pub fn shell_composition(&self, output: &str) -> Option<ui::composition::Composition> {
        self.shell.as_ref()?.composition(output)
    }

    /// The scene the shell's part on `output` last drew: its own layer's pixels, which a
    /// display with no working GPU replays on the CPU (`tilo_cpu_raster`). `None` when the
    /// shell does not run or shows no part on that output.
    #[must_use]
    pub fn shell_scene(&self, output: &str) -> Option<ui::vello::Scene> {
        self.shell.as_ref()?.scene(output)
    }

    /// Run the shell's action `action` with `arguments`, as pressing its control in the shell's
    /// own views does: `set_bar_finish` with `["translucent"]` makes the bar the theme's glass
    /// and `["solid"]` its calm ground, `apply` takes a whole preset, and `show_style` shows the
    /// shell through the style it names (`"so.tilo.shell:frames"`).
    ///
    /// # Errors
    /// The shell does not run, it declares no such action, an argument does not hold the type
    /// the action declares, or the action refused.
    pub fn run_shell_action(&self, action: &str, arguments: Vec<Value>) -> Result<(), String> {
        self.shell
            .as_ref()
            .ok_or_else(|| "the shell does not run".to_owned())?
            .run_action(action, arguments)
    }
}

/// How long a person's change of their shell is followed to its end before it is said to have
/// stopped: the change is one write of the shell's record.
const OWN_CALL_PATIENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `request`, the person's change of their shell, in `workspace` and follow it to its end:
/// a provider may answer that it took the change before it is made, and the dispatch's record of
/// the invocation then says once it was made, or why not.
///
/// # Errors
/// Why the change was refused, failed or stopped, or that it did not end in time.
async fn own_call_made(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    workspace: &str,
    request: tilo_actions::remote::InvokeRequest,
) -> Result<(), String> {
    use tilo_actions::InvocationUpdate;
    use tilo_actions::remote::ReceiptStatus;
    let invocation = request.invocation;
    match daemon
        .invoke_receipt(Some(workspace), request)
        .await?
        .status
    {
        ReceiptStatus::Completed { .. } => return Ok(()),
        ReceiptStatus::Accepted => {}
        ReceiptStatus::Rejected { error } => return Err(error.to_string()),
        ReceiptStatus::Failed { error } => return Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            return Err(message.unwrap_or_else(|| "the change was stopped".to_owned()));
        }
    }
    let followed = async {
        loop {
            let Some(state) = daemon
                .observe(
                    Some(workspace),
                    invocation,
                    Some(tilo_actions::remote::MAX_REMOTE_WAIT_MS),
                )
                .await?
            else {
                // The dispatch keeps no record of it: its provider answered it, and the shell
                // draws the change as it takes its record back in.
                return Ok(());
            };
            match state.update {
                InvocationUpdate::Succeeded { .. } => return Ok(()),
                InvocationUpdate::Failed { error, .. } => return Err(error.message),
                InvocationUpdate::Cancelled { message, .. } => {
                    return Err(message.unwrap_or_else(|| "the change was stopped".to_owned()));
                }
                InvocationUpdate::Accepted { .. }
                | InvocationUpdate::Progress { .. }
                | InvocationUpdate::Item { .. } => {}
            }
        }
    };
    tokio::time::timeout(OWN_CALL_PATIENCE, followed)
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "the change did not end within {} s",
                OWN_CALL_PATIENCE.as_secs()
            ))
        })
}

/// What holds the keyboard over the shell: a surface opened over the field the person types in,
/// else the shell's search from the pass it says it is open, and what the overlay stack says
/// otherwise. A key typed between the shell opening its search and the search's surface being
/// published is the search's, not the window's behind it.
pub(crate) fn keyboard_target(
    handle: &tilo_session_core::desktop::state::DesktopHandle,
    overlays: &Overlays,
) -> Target {
    keyboard_target_in(handle, overlays, false)
}

/// [`keyboard_target`] for a shell that draws its own screen: its own launcher takes the keys
/// where the desktop's palette would, so Enter and the arrows reach the shell's view rather than a
/// palette nothing draws.
pub(crate) fn keyboard_target_in(
    handle: &tilo_session_core::desktop::state::DesktopHandle,
    overlays: &Overlays,
    own_shell: bool,
) -> Target {
    if let Some(over) = overlays.over() {
        Target::Surface(over)
    } else if !own_shell && handle.search_open() {
        Target::Surface(Part::Search)
    } else {
        overlays.target()
    }
}

#[cfg(test)]
mod tests {
    tilo_test_env::scenario! {
        fn the_shell_package_compiles_and_is_what_the_session_keeps_of_it;

        phase the_shell_the_session_runs_compiles;
        // What the shell runs — its views and the session's and the platform's descriptions, with no
        // scenarios, which are fixtures for its shots and tests — compiles, and every id it declares
        // is in the lock. What it shows.
        {
            super::compile().unwrap_or_else(|why| panic!("{why}"));
        }

        phase the_shells_ids_lock_holds_every_declaration_the_whole_package_has;
        // `apps/tl/shell/app.ids` is the lock of the *whole* shell package, scenarios included:
        // `tilolang check` (which layout-check and the shots compile the package with) holds, and a
        // declaration the session's compile leaves out — a scenario's, its helpers' — keeps its id in
        // the lock, because a lock names every declaration the package has ever had.
        {
            use tilolang_tilo::tilolang::ids::IdsLock;
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
            // The packages the shell's scenarios reach (`use so.tilo.notes as notes`), each
            // compiled before the shell as `tilolang check` compiles it.
            let published = tilolang_tilo::platform::published_packages(&[dir
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
            // A new declaration's id is drawn here from a fixed seed, so the lock this compile
            // makes is never one to copy into app.ids: every package would draw the same ids and
            // collide when they load together. `tilolang check --write-ids` draws them at random.
            assert_eq!(
                on_disk, lock,
                "apps/tl/shell/app.ids is not the lock the whole package (scenarios included) \
             compiles to; write it with `tilolang check --write-ids apps/tl/shell`"
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
    }

    /// The device's surfaces a shell shows over, as the session starts them.
    fn shell_surfaces() -> crate::device_lists::Surfaces {
        let handle = tilo_session_core::desktop::state::DesktopHandle::new(
            tilo_desktop_model::Layout::default(),
            1,
        );
        crate::device_lists::Surfaces {
            overlays: tilo_session_core::desktop::overlays::Overlays::new(),
            notices: tilo_session_core::desktop::state::DesktopNotifications::default(),
            overview: std::rc::Rc::new(std::cell::Cell::new(false)),
            handle,
            emoji: std::rc::Rc::new(std::cell::Cell::new(false)),
            about: std::rc::Rc::new(std::cell::Cell::new(false)),
            power: std::rc::Rc::new(std::cell::Cell::new(false)),
            person: std::rc::Rc::default(),
        }
    }

    /// The tilolang shell draws what the desktop drew, so the session keeps its offer: every
    /// app window's controls are found here as in the Rust shell, and a press on one reaches
    /// the app through the same route — Files' and Shortcuts' alike, never one app's alone.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_shell_keeps_every_app_windows_controls() {
        use tilo_compositor::policy::{LayoutPoint, LayoutRect};
        use tilo_compositor::{WindowInfo, WindowPolicy as _};
        use ui::window_targets::{TargetAction, WindowTarget};

        // The windows' arrival settles at once rather than over frames that now take no time.
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut desktop = crate::platform::surfaces::desktop(7);
        let mut policy = desktop.well_policy();
        let output = LayoutRect::new(LayoutPoint::from((0, 0)), (1280, 800).into());
        desktop.cover(&[output]);
        let mut map = |id: u64, app: &str| {
            let window = WindowInfo {
                id,
                app_id: Some(app.to_owned()),
                pid: Some(u32::try_from(id).unwrap_or(4242)),
                ..WindowInfo::default()
            };
            let _ = policy.initial_size(&window, &[output]);
            let _ = policy.place(&window, (800, 600), &[output]);
            window.id
        };
        let files = map(1, "so.tilo.file-explorer");
        let shortcuts = map(2, "so.tilo.shortcuts");
        for _ in 0..8 {
            tilo_session_core::surface::ShellSurface::prepare_frame(&mut desktop);
        }
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        for (window, connection, name, identity) in [
            (files, 41, "New folder", "so.tilo.file-explorer.NewFolder"),
            (
                shortcuts,
                42,
                "New shortcut",
                "so.tilo.shortcuts.NewShortcut",
            ),
        ] {
            let tab = desktop.with_policy(|policy| policy.tab_of(window));
            let tab = tab.expect("the window is given a tab");
            handle.declare_targets(
                tab,
                connection,
                vec![WindowTarget {
                    role: "Button".to_owned(),
                    name: name.to_owned(),
                    identity: Some(identity.to_owned()),
                    rect: [10.0, 20.0, 110.0, 40.0],
                    actions: vec![TargetAction::Activate],
                    level: None,
                    selected: false,
                }],
            );
        }
        tilo_session_core::surface::ShellSurface::prepare_frame(&mut desktop);

        let lists = super::lists_surface(desktop, shell_surfaces());
        let mut lists = lists.borrow_mut();
        let controls = lists.controls();
        let mut names: Vec<(&str, Option<&str>)> = controls
            .iter()
            .map(|control| (control.name.as_str(), control.identity.as_deref()))
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                ("New folder", Some("so.tilo.file-explorer.NewFolder")),
                ("New shortcut", Some("so.tilo.shortcuts.NewShortcut")),
            ],
            "each window's own control reaches the shell, and nothing the desktop draws itself"
        );
        for name in ["New folder", "New shortcut"] {
            lists
                .press_action(name, TargetAction::Activate)
                .unwrap_or_else(|why| panic!("a press on {name} reaches its window: {why}"));
        }
        assert_eq!(
            handle.take_target_presses().len(),
            2,
            "each app is told of the press, as a screen reader's activation is"
        );
    }
}

//! The shell on the native session: the tilolang shell package (`apps/tl/shell`) shown by a
//! [`ShellPart`] per display — the whole screen under the windows, which stand in its wells — kept
//! in step with the session's shell (`crate::shell`) and asking the session's API for what the
//! person presses.

use std::cell::RefCell;
use std::rc::Rc;

use crate::platform::surfaces::{SessionLink, ShellPart, ShellWork};

/// Each of the shell's documents with its heads ([`crate::shell_document::ShellRecord::heads`]).
type ShellHeads = Vec<(String, Vec<automerge::ChangeHash>)>;
use serde_json::{Map, Value};
use tilo_actions::ActionAddress;
use tilo_compositor::Compositor;
use tilo_session_core::desktop::overlays::{Overlays, Part, Target};

use tilo_service::Services;
use tilo_session_core::shell::{Ask, LayoutReading};
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

/// The device's resources the session publishes, as their surfaces: the sound, the network,
/// Bluetooth, the modem, VPN, tethering, the battery and the displays, each its reading and
/// the actions it offers, and the rows of a reading that are resources of their own (a Wi-Fi
/// network, a Bluetooth accessory, a VPN profile), each the actions offered on the address its
/// row carries. The
/// shell's views read them as `so.tilo.device` (`device-surface.json` beside this crate, which
/// the shell compiles against and a test holds to this).
#[must_use]
pub fn device_surfaces() -> Vec<tilo_actions::surface::Surface> {
    vec![
        tilo_audio_service::sound::surface(),
        tilo_connectivity_service::network_surface(),
        tilo_connectivity_service::wifi_network_surface(),
        tilo_connectivity_service::saved_network_surface(),
        tilo_connectivity_service::bluetooth_surface(),
        tilo_connectivity_service::bluetooth_accessory_surface(),
        tilo_connectivity_service::bluetooth_nearby_surface(),
        tilo_connectivity_service::modem_surface(),
        tilo_connectivity_service::vpn_surface(),
        tilo_connectivity_service::vpn_profile_surface(),
        tilo_connectivity_service::tethering_surface(),
        tilo_session_power::battery_surface(),
        tilo_system_service::power_surface(),
        tilo_system_service::printers_surface(),
        tilo_system_service::users_surface(),
        tilo_system_service::drives_surface(),
        tilo_system_service::scanners_surface(),
        tilo_system_service::storage_surface(),
        tilo_system_service::disk_unlock_surface(),
        tilo_system_service::host_info_surface(),
        tilo_system_service::codec_pack_surface(),
        tilo_system_service::windows_compat_surface(),
        tilo_system_service::firmware_surface(),
        tilo_system_service::containers_surface(),
        tilo_system_service::processes_surface(),
        tilo_system_service::disks_surface(),
        tilo_system_service::update_surface(),
        tilo_system_service::time_zone_surface(),
        crate::displays::surface(),
        crate::displays::screen_surface(),
        crate::notifications::surface(),
        crate::shell::osd::surface(),
        crate::controls::night_light_surface(),
        crate::notifications::banners_surface(),
        crate::time::surface(),
        crate::controls::apps_surface(),
        crate::device_lists::windows_surface(),
        crate::device_lists::tray_surface(),
        crate::device_lists::agents_surface(),
        crate::device_lists::styles_surface(),
        crate::device_lists::folders_surface(),
        crate::device_lists::search_surface(),
        crate::device_lists::launchpad_surface(),
        crate::device_lists::notice_surface(),
        crate::live_list::surface(),
        crate::shell::state::overlays_surface(),
        crate::shell::state::power_menu_surface(),
        tilo_workspace_service::apps::nearby_send::surface(),
        crate::devices::surface(),
        crate::clipboard::surface(),
        crate::speakers::surface(),
        crate::quick_settings::controls_surface(),
        // This device's server settings, which its daemon changes (`device:server`).
        tilo_runtime::device::server_settings::surface(),
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
    let band = f64::from(tilo_session_core::desktop::overlays::topbar_height(
        &ui::Theme::dark(),
    ))
    .ceil() as u32;
    band
}

/// The shell on the session, and what feeds it.
pub(crate) struct Shell {
    lists: DeviceLists,
    /// The due timer ids announced already, so an unchanged one is announced once.
    announced: std::cell::RefCell<std::collections::BTreeSet<String>>,
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
    /// How many of the session's turns read some of them: what the views composed in places waited
    /// on comes in one turn for everything one run of them read, not one per resource.
    rounds: u64,
    /// Lent last to every part, by the workspace in front and the media it was opened over: a
    /// part told the same one again keeps the store it has.
    lent_content: Option<(String, Option<ui::content::ContentStore>)>,
    /// The shell's own resources, shared by every part: one display draws what another wrote, so
    /// a change any part writes is the whole shell's. Where they live on this device and how they
    /// reach the person's other devices ([`crate::shell_document`]).
    record_kept: crate::shell_document::ShellRecord,
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
    /// Their own workspace's revision the shell's resources were last written at, and their
    /// documents' heads then: an unchanged pair is neither read from their workspace nor
    /// written to it, which is what every pass runs.
    record_synced: std::cell::RefCell<Option<(u64, ShellHeads)>>,
    /// The theme the person's appearance settings make: every part draws in it, and a part made for
    /// a display plugged in later starts in it.
    theme: ui::Theme,
}

/// Reconcile the desktop's display partition with the outputs the compositor holds: a display
/// added, moved or taken away gives its strips a display even before any window is placed, and a
/// monitor that went hands its windows to the nearest one left. A mirror is one display, as the
/// desktop lays them: an output sharing a rectangle shows the same.
pub(crate) fn reconcile_displays(
    handle: &tilo_desktop_surface::DesktopHandle,
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
        theme: ui::Theme,
        state: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        // What the person waits for before the shell can draw: its package compiled, its record
        // read and its views made. The frame path after this is each frame's own cost.
        tilo_startup_metrics::mark("shell-build-start");
        let packages = compile()?;
        let app = packages
            .last()
            .map(|compiled| compiled.meta.app.clone())
            .ok_or("the shell package did not compile")?;
        let link = Rc::new(SessionLink::default());
        // The shell's own record is the person's, kept where every app's own record of theirs is
        // kept and carried to their other devices as one: read back from what an earlier session
        // kept, or the shell's defaults on a device that never showed the shell.
        let record = crate::shell_document::ShellRecord::open(state);
        let mut shell = Self {
            lists,
            announced: std::cell::RefCell::new(std::collections::BTreeSet::new()),
            link,
            followed: None,
            reading: std::collections::BTreeMap::new(),
            live: Vec::new(),
            reads: std::collections::BTreeMap::new(),
            listings: std::collections::BTreeMap::new(),
            needed: std::collections::BTreeSet::new(),
            republish: false,
            rounds: 0,
            record_kept: record,
            parts: Vec::new(),
            lent_content: None,
            packages,
            app,
            person: String::new(),
            band: band(),
            wells,
            services_seen: None,
            record_synced: std::cell::RefCell::new(None),
            theme,
        };
        shell.person = shell.lists.person.clone();
        shell.follow_outputs(compositor, surfaces, &[]);
        // The styles every installed package offers, before the first frame draws the
        // switch: a shell that starts already knows what its person may pick (shell.md §6).
        shell.refresh_styles();
        shell.follow_deadline(None);
        tilo_startup_metrics::mark("shell-build-done");
        Ok(shell)
    }

    /// A timer Clocks says came due that the bar has not announced: its id, and what to say in the
    /// app's own words. Each id is announced once.
    pub(crate) fn new_due_timer(&self) -> Option<(String, String, String)> {
        let (id, title, body) = self.lists.due_timer()?;
        let mut announced = self.announced.borrow_mut();
        announced.insert(id.clone()).then_some((id, title, body))
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
        identify: &[tilo_desktop::display_identify::Label],
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
        identify: &[tilo_desktop::display_identify::Label],
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
        let wake = compositor.waker();
        let mut part = ShellPart::new(
            crate::platform::surfaces::ShellIdentity {
                app: &self.app,
                packages: self.packages.clone(),
                person: &self.person,
                link: Rc::clone(&self.link),
                store: self.record_kept.store(),
                // The first part of the shell makes the shell's roots and seeds them; every other
                // display's part draws the ones the shell already has.
                seed: self.parts.is_empty(),
            },
            scale,
            std::sync::Arc::new(move || wake.wake_by_ref()),
            self.wells.clone(),
        );
        part.set_theme(self.theme.clone());
        part.set_display(Some((output.to_owned(), rect)), scale);
        part.set_screen(screen);
        part.set_fullscreen(fullscreen.to_vec());
        // The windows the views picture, each display's own: the reading's window keys stand for
        // the tabs of the workspace in front, and a window whose app holds secrets is left out, so
        // its view shows the app's mark instead of its pixels.
        let handle = self.lists.handle.clone();
        part.show_pictures(
            ui::widget::Previews::new(),
            Rc::new(move |key: &str| pictured(&handle, key)),
        );
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

    /// Tell the displays what the screen says of its edges: the pointer at the top or bottom edge,
    /// and a window where a bar or a dock there rides. What a bar or dock that shows
    /// only sometimes follows. Whether that moved.
    fn publish_edges(&self, compositor: &Compositor, displays: &displays::Displays) -> bool {
        let Some(outputs) = Self::outputs(compositor) else {
            return false;
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "a band's height, a few dozen pixels"
        )]
        let band = f64::from(self.band);
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
        let edges = tilo_session_core::shell::screen_edges(
            outputs,
            band,
            compositor.pointer_location(),
            &windows,
        );
        displays.set_edges(edges)
    }

    /// Follow the displays as they come and go, and a window taking one for itself: a part is made
    /// or dropped per output, and a part on a display a fullscreen window owns steps aside. Cheap
    /// when nothing changed, which is what every frame runs.
    pub(crate) fn follow_displays(
        &mut self,
        compositor: &mut Compositor,
        surfaces: &mut Surfaces,
        identify: &[tilo_desktop::display_identify::Label],
    ) {
        self.follow_outputs(compositor, surfaces, identify);
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

    /// Hand the shell each listing its views list records of, as the session follows it.
    fn publish_listings(&self, services: &Services) {
        for (ty, resource) in &self.listings {
            if let Some(reading) = services.read(resource) {
                self.link
                    .publish_reading(tilolang_tilo::runtime::foreign::LISTING, ty, reading);
            }
        }
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
                .then(|| tilolang_tilo::platform_apps::listing_of(ty))
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
        let mut fresh: Vec<(String, String)> = Vec::new();
        // The apps whose views this process links as Rust apps, which draw their resources
        // without a package of their own.
        let rust_views = tilolang_tilo::views::declared_apps();
        if asked || live_moved || self.followed.as_ref().map(|(_, was)| was) != Some(layout) {
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
                app_of(ty)
                    .is_some_and(|app| packages.contains_key(app) || rust_views.contains(&app))
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
        }
        for (id, ty) in wanted {
            let app = tilolang_tilo::readings::app_of(&ty);
            let shown = app.is_some_and(|app| {
                self.link.package_apps().iter().any(|held| held == app) || rust_views.contains(&app)
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
        self.live = live;
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

    /// Hand the shell what waits for the person's yes, which its asking view draws, whenever it
    /// changed.
    pub(crate) fn publish_asking(&self, questions: Vec<tilolang_tilo::shell::AskedReading>) {
        self.link.publish_asking(questions);
    }

    /// Read the styles every installed package offers, when the person's package store
    /// moved: a style the person or their agent installed is offered by the switch without a
    /// restart (`shell.md` §6). The shell's own package's styles — the four that ship — always
    /// stand, from the package the shell runs, so the switch offers them even where the store
    /// holds nothing (the built-in fallback shell). Cheap when nothing was installed, which is
    /// what every frame runs.
    pub(crate) fn refresh_styles(&mut self) {
        // The shell's own styles, from the package it runs: the four that ship are its views.
        let Ok(catalog) = package_store::Catalog::from_env() else {
            let own = crate::styles::of_shell(&self.packages);
            let mut styles = self.lists.styles.borrow_mut();
            styles.list = own;
            styles.revision = styles.revision.saturating_add(1);
            return;
        };
        let now = crate::styles::registry(&catalog);
        {
            let mut styles = self.lists.styles.borrow_mut();
            let former = styles.registry.as_ref();
            if former == Some(&now) {
                return;
            }
            // The store is read on every frame, so what the log says of it is said to the frame
            // that found it changed: the warning when it starts, and the line that it reads again
            // when it recovers, rather than the same line thousands of times a second.
            match crate::styles::news(former, &now) {
                Some(crate::styles::News::Unreadable(why)) => {
                    tracing::warn!(
                        error = %why,
                        "the app store's registry does not read; its styles wait"
                    );
                }
                Some(crate::styles::News::ReadsAgain) => {
                    tracing::info!("the app store's registry reads again; its styles are read");
                }
                None => {}
            }
            styles.registry = Some(now);
        }
        // A package installed or taken away is published again, so a style that failed and was
        // reinstalled fixed draws again with no restart (`shell.md` §6). The failure it had is
        // cleared too: the shell must draw it again for its view to run and tell whether it is
        // fixed, and a store that moved is the only news the session gets of a reinstall.
        self.republish = true;
        for style in crate::styles::installed(&catalog, &self.packages) {
            // The shell's own package is the one the part already runs, never one loaded
            // beside itself.
            if style.package != tilo_session_core::shell::SHELL_APP {
                self.needed.insert(style.package.clone());
            }
        }
        for entry in &mut self.lists.styles.borrow_mut().list {
            if !entry.loads {
                entry.loads = true;
                entry.reason.clear();
            }
        }
        // The store's styles, the shell's own among them when the store holds the shell: each
        // package's styles are offered once, the shell's own from the package the shell runs.
        let own = crate::styles::of_shell(&self.packages);
        let mut list = crate::styles::installed(&catalog, &self.packages);
        for style in own {
            if !list.iter().any(|s| s.style == style.style) {
                list.push(style);
            }
        }
        let mut styles = self.lists.styles.borrow_mut();
        styles.list = list;
        styles.revision = styles.revision.saturating_add(1);
    }

    /// The link the session publishes the shell on and the shell asks through: what a test stages a
    /// reading on.
    pub(crate) fn link(&self) -> Rc<SessionLink> {
        Rc::clone(&self.link)
    }

    /// The views that could not be shown since the last call, each an app, a view and why: what
    /// the parts reported (`shell.md` §6, "Nothing can lock a person out").
    pub(crate) fn take_view_failures(&self) -> Vec<tilolang_tilo::runtime::foreign::ViewFailure> {
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
        let mut styles = self.lists.styles.borrow_mut();
        let mut moved = false;
        for entry in &mut styles.list {
            let of_app = entry.package == app;
            let named = style.is_some_and(|style| {
                entry.style == format!("{app}:{style}") || entry.style == style
            });
            if (of_app || named) && !entry.loads {
                entry.loads = true;
                entry.reason.clear();
                moved = true;
            }
        }
        if moved {
            styles.revision = styles.revision.saturating_add(1);
        }
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
        failure: &tilolang_tilo::runtime::foreign::ViewFailure,
    ) -> Option<crate::notices::Notice> {
        let style = if failure.view.contains(':') {
            failure.view.clone()
        } else {
            format!("{}:{}", failure.app, failure.view)
        };
        let mut styles = self.lists.styles.borrow_mut();
        let Some(found) = styles.list.iter_mut().find(|s| s.style == style) else {
            // A view that is no style failed some other way (a place's own view): the
            // shell has nothing to say of it.
            return None;
        };
        if !found.loads && found.reason == failure.reason {
            return None;
        }
        found.loads = false;
        found.reason.clone_from(&failure.reason);
        let name = if found.line.is_empty() {
            found.name.clone()
        } else {
            found.line.clone()
        };
        styles.revision = styles.revision.saturating_add(1);
        Some(crate::notices::style_failed_notice(&name, &failure.reason))
    }

    /// The surface the shell shows over its windows, by the name the views read (`d.here`): `none`
    /// at rest.
    pub(crate) fn surface(&self) -> String {
        self.link.surface()
    }

    /// Three fingers carrying the strip shown, as `scroll` of them: the row the shell draws the
    /// strip in scrolls under the front window, as two fingers over it would scroll it, and glides
    /// on when they lift. The pointer stays at `pointer` for everything else the part shows.
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
        let Some(well) = front.and_then(|tab| self.wells.as_ref()?.well(&tab.to_string())) else {
            return;
        };
        let (x, y) = (well.center().x, well.center().y);
        let Some((_, part)) = self
            .parts
            .iter()
            .find(|(_, part)| part.borrow().accepts_pointer(x, y))
        else {
            return;
        };
        let mut part = part.borrow_mut();
        part.pointer_motion(x, y);
        part.pointer_axis(scroll);
        if part.accepts_pointer(pointer.0, pointer.1) {
            part.pointer_motion(pointer.0, pointer.1);
        } else {
            part.pointer_leave();
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

    /// Why the shell's resources this device kept were not opened, once: the person is told, and
    /// not again.
    pub(crate) fn take_record_refusal(&self) -> Option<String> {
        self.record_kept.take_refusal()
    }

    /// Where the shell's resources are kept on this device: what a test reads back to prove a
    /// shell choice was persisted. `None` on a session that keeps no copy.
    #[must_use]
    pub fn resource_path(&self) -> Option<&std::path::Path> {
        self.record_kept.path()
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
            self.record_kept.store(),
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

    /// Carry the shell's resources to the person's other devices and take in what theirs hold:
    /// they are the person's, in their own workspace (`store`), which carries each of the shell's
    /// documents exactly as it carries every app's own record of theirs. What this device arranged
    /// since the last pass is written there; what another of their devices arranged is merged
    /// here, so the shell this session draws is the one they have everywhere.
    ///
    /// Cheap when nothing moved, which is what every pass runs.
    pub(crate) fn sync_resource(&self, store: &crate::PersonResources) {
        let app = self.app.as_str();
        // The workspace's revision, which moves when it takes a write from any of the person's
        // devices, and the documents' own heads: an unchanged pair means neither this device's
        // shell nor their workspace moved since the last look, so nothing is read or written.
        let revision = *store.changes().borrow();
        let own = self.record_kept.heads();
        if self
            .record_synced
            .borrow()
            .as_ref()
            .is_some_and(|(was, heads)| *was == revision && *heads == own)
        {
            return;
        }
        // The documents their workspace holds that this device has none of yet: a shell arranged
        // on another device, which this one has not drawn.
        let prefix = format!("{}/", self.record_kept.id());
        let mut docs: std::collections::BTreeSet<String> =
            own.iter().map(|(doc, _)| doc.clone()).collect();
        docs.extend(
            store
                .documents(app)
                .into_iter()
                .filter_map(|id| id.strip_prefix(&prefix).map(str::to_owned)),
        );
        for doc in &docs {
            let id = format!("{prefix}{doc}");
            // What another device of theirs wrote since the last look is taken in first, so what
            // this device writes next carries it rather than opening a rival branch.
            let theirs = store.document((app, &id));
            if let Some(bytes) = &theirs {
                self.record_kept.take_in(doc, bytes);
            }
            let Some(whole) = self.record_kept.save(doc) else {
                continue;
            };
            // What this device arranged of the document, merged into what their workspace holds
            // of it: the ops it already has leave nothing pending, and nothing is written.
            store.edit_document((app, &id), move |document| {
                let mut source =
                    automerge::AutoCommit::load(&whole).map_err(|error| error.to_string())?;
                tilo_automerge::ApplyIncoming::merge_incoming(document, &mut source)
                    .map_err(|error| error.to_string())?;
                Ok(())
            });
        }
        // What was read and written is what the next pass finds unchanged: its own heads, with
        // the workspace's revision they were written at.
        let revision = *store.changes().borrow();
        *self.record_synced.borrow_mut() = Some((revision, self.record_kept.heads()));
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
        surfaces: &crate::mcp_server::SessionServices,
    ) {
        let Some(compiled) = self.package() else {
            return;
        };
        let store = self.record_kept.store();
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
    pub(crate) fn set_theme(&mut self, theme: &ui::Theme) {
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

    /// The clip a shown part's scroll view draws the content holding `text` through.
    pub(crate) fn scroll_clip(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.parts
            .iter()
            .filter(|(_, part)| part.borrow().placement().is_some())
            .find_map(|(_, part)| part.borrow_mut().scroll_clip(text))
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
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, tilo_actions::ApiError> {
        if address.resource != tilo_session_core::desktop::verbs::CHROME_RESOURCE {
            return Ok(None);
        }
        // The strips list is the shell's own view under the shell: `show-strips` opens it and a
        // strip chosen closes it, so nothing of the chrome's strip switcher draws.
        if address.action == "show-strips" {
            self.surfaces.toggle(crate::device_lists::Surfaces::STRIPS);
            return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            )));
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
            // The workspaces are the shell's one workspace surface: the shell's menu, the workspaces
            // a person switches between and the actions on them.
            "workspace-menu" => {
                self.surfaces
                    .toggle(tilo_session_core::desktop::overlays::Overlay::Workspaces.name());
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

impl ShellChrome {
    /// The chrome's verb at `address`, carried out on the desktop the session owns: `Ok(None)` for
    /// one that is a surface only the chrome draws.
    fn carry(
        &self,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
    ) -> Result<Option<ui::host_operation::ResourceOperationStart>, tilo_actions::ApiError> {
        use tilo_session_core::desktop::host::chrome::ChromeIntent;
        use tilo_session_core::desktop::host::contract::DesktopAsk;
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
            Carried::Shell => {
                let intent = match verb {
                    ChromeEffect::ShowPerson { person } => ChromeIntent::ShowPerson(person),
                    ChromeEffect::TogglePeople => ChromeIntent::TogglePeople,
                    // The strips of the workspace shown, each where it stands: the overview.
                    ChromeEffect::ShowStrips => {
                        self.surfaces
                            .open(tilo_session_core::desktop::overlays::Overlay::Overview.name());
                        return Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
                            tilo_actions::ActionOutput::default(),
                        )));
                    }
                    _ => return Ok(None),
                };
                tilo_session_core::desktop::shell_verbs::ask(
                    &self.desktop,
                    &self.asks,
                    DesktopAsk::Intent(intent),
                );
                None
            }
        };
        Ok(Some(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput {
                value,
                ..tilo_actions::ActionOutput::default()
            },
        )))
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
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
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
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
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
            self.surfaces
                .toggle(tilo_session_core::desktop::overlays::Overlay::Launcher.name());
            return Ok(ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ));
        }
        // The platform search API (a window's search control, its toolbar's magnifier, Ctrl+K)
        // asks for the shell's search under the shell too: the shell draws that search, so the
        // request opens the launcher, and its query runs the shell's search as the shell's own
        // `search` verb does, its rows published for whoever reads them.
        if ui::search::is_search_address(address) {
            let query = ui::search::SearchRequest::from_arguments(arguments)
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
                None => ui::host_operation::ResourceOperationStart::Completed(
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
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::ApiError> {
        if ui::search::is_search_address(address)
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

impl crate::Session {
    /// What the shell's views could not show, and what came back: a style that failed is marked
    /// not loading, with the notice naming it, and one that came back loads again — the shell
    /// keeps the person's choice either way (`shell.md` §6, "Nothing can lock a person out").
    fn follow_view_failures(
        notices: &Rc<RefCell<crate::notifications::SessionNotifications>>,
        shell: &Shell,
        failures: Vec<tilolang_tilo::runtime::foreign::ViewFailure>,
        recoveries: (Vec<String>, Vec<(String, String)>),
    ) {
        for failure in failures {
            if let Some(notice) = shell.note_style_failure(&failure) {
                // Raised through the session's own notices, as every notice it tells: the
                // notifications service when one runs, the shell's own otherwise.
                notices.borrow().tell(&notice);
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

    /// Keep the shell's own record: say once if the copy this device kept could not be opened,
    /// write what the parts committed into the copy on this device, and carry it to the person's
    /// other devices and take in what theirs holds. Cheap when nothing moved, which every pass runs.
    ///
    /// The person's store is borrowed only for it, so the parts' displays after it borrow the
    /// session freely.
    fn follow_shell_record(shell: &Shell, shared: &crate::Shared) {
        let notices = &shared.notifications;
        let store = shared
            .sync
            .as_ref()
            .and_then(|sync| sync.person_resources());
        // A record this device kept and this build did not open is said, once: the shell drew the
        // shell's defaults, and the person is told where their own shell's choices still are rather
        // than finding them silently reset.
        if let Some(reason) = shell.take_record_refusal() {
            notices
                .borrow()
                .tell(&crate::notices::record_not_opened_notice(&reason));
        }
        // The shell's resources are the person's on every device of theirs: what they arranged here
        // reaches their others through their own workspace, and what they arranged there reaches
        // this session.
        if let Some(store) = store {
            shell.sync_resource(&store);
        }
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
        if let Some((_, title, body)) = shell.new_due_timer() {
            self.shared
                .telling()
                .show(&crate::platform::notice::Notice::new(
                    String::new(),
                    1,
                    title,
                    body,
                ));
        }
        // The styles every installed package offers: read when the store moved, so a
        // style a person or their agent installed is offered without a restart (shell.md §6).
        // Read before the shell is published, so the reading carries the list.
        shell.refresh_styles();
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
        Self::follow_shell_record(shell, &self.shared);
        // One part per display: a display plugged in, taken away or taken by a fullscreen window
        // moves its part.
        shell.follow_displays(&mut self.compositor, &mut self.shared.published, &identify);
        if !shell.has_asks() {
            return Ok(());
        }
        let (published, search) = (&self.shared.published, &self.shared.search);
        let workspace = self.shared.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
                .unwrap_or_default()
        });
        let resources = published.resources();
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
                            &workspace,
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
                published
                    .invoke(
                        &ActionAddress::new(resource, action),
                        arguments,
                        &mut tilo_session_core::surfaces::Look::none(),
                    )
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        );
        // What ran may have asked the session in turn (a menu, a sheet).
        self.settle()?;
        if let Some(shell) = self.shell.as_mut() {
            shell.follow_deadline(self.numbers.deadline());
        }
        // The shell's places are what the outside services keeping its records in step are due
        // for: each workspace with a place of an app's records has that app's connector, and one
        // that no longer shows any ends. Read every pass, so a place made, moved between
        // workspaces or closed is followed on the pass it happened on, in every workspace of
        // the shell, not only the one in front (`docs/architecture/apps.md` §1, §8).
        crate::connectors::reconcile(self);
        Ok(())
    }

    /// What the shell reads the shell through: a test stages a reading on it, as a collaborating
    /// session's presence would arrive.
    #[must_use]
    pub fn shell_link(&self) -> Option<Rc<crate::platform::surfaces::SessionLink>> {
        self.shell.as_ref().map(Shell::link)
    }

    /// Where the shell's own record is kept on this device: the document a person's shell choices
    /// are written to, which a test reads back to prove they survive the session. `None` when the
    /// shell does not run or keeps no copy.
    #[must_use]
    pub fn shell_record_path(&self) -> Option<&std::path::Path> {
        self.shell.as_ref().and_then(Shell::resource_path)
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

    /// The clip the shell's scroll view holding `text` draws its content through, logical
    /// `(x, y, w, h)` : the room a transcript has under the header above it.
    #[must_use]
    pub fn shell_scroll_clip(&self, text: &str) -> Option<(f64, f64, f64, f64)> {
        self.shell.as_ref()?.scroll_clip(text)
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
    }

    /// The device's surface the shell compiles against is the one the session publishes. Its own
    /// test, so `UPDATE_SNAPSHOTS=1` can write the file without first compiling the shell package
    /// that reads it.
    #[test]
    fn the_device_surface_file_is_what_the_session_publishes() {
        let published = super::device_surfaces();
        let committed = tilolang_tilo::platform_apps::device_surfaces()
            .unwrap_or_else(|why| panic!("device-surface.json does not read: {why}"));
        if committed == published {
            return;
        }
        let text = serde_json::to_string_pretty(&serde_json::json!({ "resources": published }))
            .expect("a surface writes as JSON");
        // `UPDATE_SNAPSHOTS=1` writes the file the session publishes, as the session package's own
        // snapshot test does: a resource whose verbs changed is regenerated in place rather than
        // pasted from a failing run.
        if std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("device-surface.json");
            std::fs::write(&path, &text).expect("device-surface.json is written");
            return;
        }
        panic!(
            "crates/desktop/session/device-surface.json is not what the session publishes; \
             regenerate it with UPDATE_SNAPSHOTS=1 cargo nextest run -p tilo-session --lib \
             the_device_surface_file_is_what_the_session_publishes, or write what it \
             publishes:\n{text}"
        );
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
            strips: std::rc::Rc::new(std::cell::Cell::new(false)),
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
        let _settled = ui::motion::testing::settled();
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

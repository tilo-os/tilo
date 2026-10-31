//! The controllers in the room, read off evdev and carried out on the
//! desktop.
//!
//! A controller is one more way to ask for things the session already does,
//! so what it asks for leaves by the doors a keyboard's presses already use:
//! a direction, activate or back is the key it stands for, routed by the
//! compositor exactly as a physical key, and every command is the published
//! shortcut of the same name ([`Invocation::shortcut`]). The direction,
//! repeat and guide-chord policy is [`crate::controller_nav`]'s.
//!
//! A controller leased to another of the person's devices is left unread
//! for the lease's lifetime, so one press never acts in two places, and the
//! fabric's own virtual gamepads are never controllers here: the decoder
//! classifies them as forwarded and does not list them at all.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::OwnedFd;
use std::time::Instant;

use os_actions::{
    ForwardedGamepadEvent, LinuxPhysicalGamepadCatalog, LinuxPhysicalGamepadSource,
    PhysicalGamepadHotplug, PhysicalGamepadId,
};
use ui::ControllerFamily;

use tilo_compositor::InputEvent;

use crate::controller_nav::{ControllerNavMapper, NavDirection, NavIntent};
use crate::{Error, Invocation, Session, ShortcutAction};

/// Raw events read per controller per pass; the rest wait for the next one.
const READ_BUDGET: usize = 64;

/// One controller, as the system names it.
pub type ControllerId = PhysicalGamepadId;

/// A descriptor the session's loop should (stop) polling, so controller
/// input wakes the desktop instead of waiting for its next turn.
#[derive(Debug)]
pub enum ControllerWake {
    Watch { key: u64, fd: OwnedFd },
    Unwatch { key: u64 },
}

/// Where the controllers come from: the machine's evdev devices, or a
/// stand-in for a test.
pub trait GamepadSources {
    /// Whether the controllers attached may have changed since the last
    /// look, without anything else happening.
    fn changed(&mut self) -> bool;

    /// The controllers attached now, by id and printed legend: the ones the
    /// person can drive the desktop with. A leased one is absent until its
    /// lease ends.
    fn attached(&mut self) -> Vec<(ControllerId, ControllerFamily)>;

    /// What `id` sent since the last read, and whether more waits for the
    /// next pass; `None` when it left.
    fn read(
        &mut self,
        id: &ControllerId,
        budget: usize,
    ) -> Option<(Vec<ForwardedGamepadEvent>, bool)>;

    /// The descriptors the loop should (stop) polling, since the last call.
    fn take_wakes(&mut self) -> Vec<ControllerWake> {
        Vec::new()
    }
}

/// Where a session's controllers come from.
pub enum Source {
    /// The controllers attached to this machine, read from evdev: what the
    /// person drives the desktop with.
    Machine,
    /// Controllers lent as they are: a stand-in a test puts in the room.
    Lent(Box<dyn GamepadSources>),
}

/// The controllers driving the desktop: one mapper per controller, which
/// turns what each sends into what the desktop does.
pub struct Controllers {
    sources: Box<dyn GamepadSources>,
    mappers: BTreeMap<ControllerId, ControllerNavMapper>,
    /// One left while the mapper still held a direction: read the set of
    /// controllers again, and open what is attached.
    rescan: bool,
    /// A controller sent more than one pass could read: read again at once.
    backlog: bool,
}

impl Controllers {
    pub fn new(sources: Box<dyn GamepadSources>) -> Self {
        Self {
            sources,
            mappers: BTreeMap::new(),
            rescan: true,
            backlog: false,
        }
    }

    /// The controllers attached to this machine, read from evdev.
    pub fn machine() -> Self {
        Self::new(Box::new(Machine::default()))
    }

    /// The controllers `source` names, read from it.
    #[must_use]
    pub fn mount(source: Source) -> Self {
        match source {
            Source::Machine => Self::machine(),
            Source::Lent(sources) => Self::new(sources),
        }
    }

    /// Descriptor registrations queued since the last call.
    pub fn take_wakes(&mut self) -> Vec<ControllerWake> {
        self.sources.take_wakes()
    }

    /// When [`Self::poll`] next has work without new input.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if self.backlog {
            return Some(now);
        }
        self.mappers
            .values()
            .filter_map(ControllerNavMapper::next_deadline)
            .min()
    }

    /// Read every controller and return what it asked for, with the legend
    /// family of the controller that asked each. `passthrough` is true while
    /// a fullscreen window owns the controller.
    pub fn poll(&mut self, now: Instant, passthrough: bool) -> Vec<(NavIntent, ControllerFamily)> {
        let Self {
            sources,
            mappers,
            rescan,
            backlog,
        } = self;
        if *rescan || sources.changed() {
            reconcile(sources.as_mut(), mappers);
            *rescan = false;
        }
        *backlog = false;
        let mut out = Vec::new();
        let mut lost = Vec::new();
        for (id, mapper) in &mut *mappers {
            mapper.set_passthrough(passthrough);
            let family = mapper.family();
            let Some((events, saturated)) = sources.read(id, READ_BUDGET) else {
                tracing::info!(?id, "controller left desktop navigation");
                lost.push(id.clone());
                continue;
            };
            *backlog |= saturated;
            for event in events {
                out.extend(
                    mapper
                        .feed(event, now)
                        .into_iter()
                        .map(|intent| (intent, family)),
                );
            }
            out.extend(mapper.tick(now).into_iter().map(|intent| (intent, family)));
        }
        if !lost.is_empty() {
            for id in lost {
                mappers.remove(&id);
            }
            *rescan = true;
        }
        out
    }
}

/// Open a mapper for each controller attached that has none, and drop the
/// ones that left.
fn reconcile(
    sources: &mut dyn GamepadSources,
    mappers: &mut BTreeMap<ControllerId, ControllerNavMapper>,
) {
    let attached: BTreeMap<ControllerId, ControllerFamily> =
        sources.attached().into_iter().collect();
    let left: Vec<ControllerId> = mappers
        .keys()
        .filter(|id| !attached.contains_key(*id))
        .cloned()
        .collect();
    for id in left {
        mappers.remove(&id);
    }
    for (id, family) in attached {
        mappers
            .entry(id)
            .or_insert_with(|| ControllerNavMapper::new(family));
    }
}

/// The controllers of `known` that the desktop may read: a controller leased
/// to another device or app is left to its lease, and one that could not be
/// opened is not asked again until something is plugged in.
fn wanted(
    known: &BTreeMap<ControllerId, ControllerFamily>,
    leased: &BTreeSet<ControllerId>,
    refused: &BTreeSet<ControllerId>,
) -> Vec<(ControllerId, ControllerFamily)> {
    known
        .iter()
        .filter(|(id, _)| !leased.contains(*id) && !refused.contains(*id))
        .map(|(id, family)| (id.clone(), *family))
        .collect()
}

impl Session {
    /// Read the controllers in the room and carry out what they asked of the
    /// desktop: a direction, activate or back becomes the key it stands for,
    /// through the same route a physical key takes, and every command is the
    /// desktop's shortcut of the same name.
    ///
    /// What they sent is still read while the screen is locked, so presses
    /// made there never replay after the unlock.
    pub(crate) fn follow_controllers(&mut self) -> Result<(), Error> {
        let Some(controllers) = self.controllers.as_mut() else {
            return Ok(());
        };
        for wake in controllers.take_wakes() {
            match wake {
                ControllerWake::Watch { key, fd } => match self.compositor.watch_input(fd) {
                    Ok(watch) => drop(self.controller_wakes.insert(key, watch)),
                    Err(error) => {
                        tracing::warn!(%error, "controller cannot wake the session");
                    }
                },
                ControllerWake::Unwatch { key } => {
                    drop(self.controller_wakes.remove(&key));
                }
            }
        }
        // A fullscreen window is playing: the controller is its input, and
        // only the guide's chords reach the desktop.
        let fullscreen = self.compositor.fullscreen_window();
        let intents = controllers.poll(Instant::now(), fullscreen);
        if intents.is_empty() || self.compositor.locked() {
            return Ok(());
        }
        for (intent, family) in intents {
            // The controller is the input the guide leads with, until a key
            // of the person's own is pressed: set before its command runs, so
            // a guide it opens explains its buttons.
            self.shared.controller.replace(Some(family));
            if let Some(action) = controller_shortcut(intent) {
                self.shared
                    .invocations
                    .borrow_mut()
                    .push(Invocation::shortcut(action));
                // The desk moves before the next press is routed, so a
                // direction after it acts on whatever has focus by then.
                self.settle()?;
            } else {
                // A direction, activate or back: the key it stands for, which
                // the compositor routes exactly as a press of that key.
                let keys = controller_keys(intent, self.compositor.host_takes_keys());
                self.shared.controller_keys.set(true);
                for (keycode, pressed) in keys {
                    drop(self.compositor.input(InputEvent::Key { keycode, pressed }));
                }
                self.shared.controller_keys.set(false);
            }
        }
        Ok(())
    }
}

/// The desktop command a controller intent stands for, when it is one the
/// person can bind a key to: what a bumper, a trigger, a face button or the
/// Guide chord asks for is a shortcut the desktop already answers.
fn controller_shortcut(intent: NavIntent) -> Option<ShortcutAction> {
    Some(match intent {
        NavIntent::Actions => ShortcutAction::TabMenu,
        NavIntent::Search => ShortcutAction::Search,
        NavIntent::Help => ShortcutAction::ShortcutHelp,
        NavIntent::Overview => ShortcutAction::WorkspaceOverview,
        NavIntent::QuickSettings => ShortcutAction::QuickSettings,
        NavIntent::PreviousTab => ShortcutAction::PreviousTab,
        NavIntent::NextTab => ShortcutAction::NextTab,
        NavIntent::PreviousWorkspace => ShortcutAction::PreviousWorkspace,
        NavIntent::NextWorkspace => ShortcutAction::NextWorkspace,
        NavIntent::FocusPreviousWindow => ShortcutAction::FocusPreviousWindow,
        NavIntent::FocusNextWindow => ShortcutAction::FocusNextWindow,
        NavIntent::PreviousStrip => ShortcutAction::PreviousStrip,
        NavIntent::NextStrip => ShortcutAction::NextStrip,
        NavIntent::MoveGroupLeft => ShortcutAction::MoveGroupLeft,
        NavIntent::MoveGroupRight => ShortcutAction::MoveGroupRight,
        NavIntent::Move(_) | NavIntent::Activate | NavIntent::Back => return None,
    })
}

/// What a key press sends, evdev codes.
const KEY_ESCAPE: u32 = 1;
const KEY_ENTER: u32 = 28;
const KEY_LEFT_ALT: u32 = 56;
const KEY_UP: u32 = 103;
const KEY_LEFT: u32 = 105;
const KEY_RIGHT: u32 = 106;
const KEY_DOWN: u32 = 108;

/// The keystrokes a direction, activate or back stands for, pressed and
/// released. Directional focus is the toolkit's Alt+arrow in an app, so it
/// moves focus rather than the caret, and a bare arrow once a surface of the
/// session's own takes the keyboard (a palette, the overview, the guide),
/// where arrows already navigate.
fn controller_keys(intent: NavIntent, host_takes_keys: bool) -> Vec<(u32, bool)> {
    let (keycode, alt) = match intent {
        NavIntent::Move(direction) => (
            match direction {
                NavDirection::Up => KEY_UP,
                NavDirection::Down => KEY_DOWN,
                NavDirection::Left => KEY_LEFT,
                NavDirection::Right => KEY_RIGHT,
            },
            !host_takes_keys,
        ),
        NavIntent::Activate => (KEY_ENTER, false),
        NavIntent::Back => (KEY_ESCAPE, false),
        _ => return Vec::new(),
    };
    let mut keys = Vec::with_capacity(4);
    if alt {
        keys.push((KEY_LEFT_ALT, true));
    }
    keys.push((keycode, true));
    keys.push((keycode, false));
    if alt {
        keys.push((KEY_LEFT_ALT, false));
    }
    keys
}

/// One controller open for the desktop, and the descriptor registration its
/// input wakes the loop by.
struct Open {
    source: LinuxPhysicalGamepadSource,
    key: u64,
}

/// The controllers attached to this machine, read from evdev.
struct Machine {
    catalog: LinuxPhysicalGamepadCatalog,
    hotplug: Option<PhysicalGamepadHotplug>,
    /// The controllers the machine has now, by id and legend family.
    known: BTreeMap<ControllerId, ControllerFamily>,
    /// The controllers open for the desktop to read, and the descriptor
    /// registration each holds.
    open: BTreeMap<ControllerId, Open>,
    /// The controllers leased to another device or app, which this one
    /// leaves unread until the lease ends.
    leased: BTreeSet<ControllerId>,
    /// The controllers that could not be opened; retried on the next plug-in.
    refused: BTreeSet<ControllerId>,
    /// The catalog has to be read again: at the start, after a plug-in, and
    /// after one was lost.
    rescan: bool,
    next_key: u64,
    wakes: Vec<ControllerWake>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new(LinuxPhysicalGamepadCatalog::default())
    }
}

impl Machine {
    fn new(catalog: LinuxPhysicalGamepadCatalog) -> Self {
        let mut wakes = Vec::new();
        let hotplug = match catalog.hotplug_watch() {
            Ok(hotplug) => {
                match hotplug.wake_fd() {
                    Ok(fd) => wakes.push(ControllerWake::Watch { key: 0, fd }),
                    Err(error) => {
                        tracing::warn!(%error, "controller hotplug cannot wake the session");
                    }
                }
                Some(hotplug)
            }
            Err(error) => {
                tracing::info!(%error, "controller hotplug is not observable here");
                None
            }
        };
        Self {
            catalog,
            hotplug,
            known: BTreeMap::new(),
            open: BTreeMap::new(),
            leased: BTreeSet::new(),
            refused: BTreeSet::new(),
            rescan: true,
            next_key: 1,
            wakes,
        }
    }

    /// The controllers to open now: the ones the machine has, that are
    /// neither leased away nor refused.
    fn wanted(&self) -> Vec<(ControllerId, ControllerFamily)> {
        wanted(&self.known, &self.leased, &self.refused)
    }

    fn refresh_catalog(&mut self) {
        match self.catalog.discover() {
            Ok(found) => {
                self.known = found
                    .into_iter()
                    .map(|info| {
                        let family =
                            crate::controller_nav::controller_family(info.vendor_id, &info.name);
                        (info.id, family)
                    })
                    .collect();
            }
            Err(error) => tracing::warn!(%error, "could not enumerate game controllers"),
        }
        self.rescan = false;
    }

    fn open(&mut self, id: ControllerId) {
        let mut source = match self.catalog.open(&id) {
            Ok(source) => source,
            Err(error) => {
                tracing::info!(%error, "controller is not available for desktop navigation");
                self.refused.insert(id);
                return;
            }
        };
        // Presses from before this controller joined (or before its lease
        // ended) must not replay as desktop actions.
        if source.drain(READ_BUDGET.saturating_mul(4)).is_err() {
            self.refused.insert(id);
            return;
        }
        let key = self.next_key;
        self.next_key = self.next_key.saturating_add(1);
        match source.wake_fd() {
            Ok(fd) => self.wakes.push(ControllerWake::Watch { key, fd }),
            Err(error) => tracing::warn!(%error, "controller cannot wake the session"),
        }
        self.open.insert(id, Open { source, key });
    }

    fn close(&mut self, id: &ControllerId) {
        if let Some(open) = self.open.remove(id) {
            self.wakes.push(ControllerWake::Unwatch { key: open.key });
        }
    }
}

impl GamepadSources for Machine {
    fn changed(&mut self) -> bool {
        let plugged = self
            .hotplug
            .as_mut()
            .is_some_and(PhysicalGamepadHotplug::take_changes);
        if plugged {
            self.refused.clear();
            self.rescan = true;
        }
        std::mem::take(&mut self.rescan)
    }

    fn attached(&mut self) -> Vec<(ControllerId, ControllerFamily)> {
        if self.rescan {
            self.refresh_catalog();
        }
        let wanted = self.wanted();
        let wanted_ids: BTreeSet<ControllerId> = wanted.iter().map(|(id, _)| id.clone()).collect();
        let gone: Vec<ControllerId> = self
            .open
            .keys()
            .filter(|id| !wanted_ids.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            self.close(&id);
        }
        for (id, _) in &wanted {
            if !self.open.contains_key(id) {
                self.open(id.clone());
            }
        }
        wanted
    }

    fn read(
        &mut self,
        id: &ControllerId,
        budget: usize,
    ) -> Option<(Vec<ForwardedGamepadEvent>, bool)> {
        let drained = self.open.get_mut(id)?.source.drain(budget);
        match drained {
            Ok(drained) => Some((drained.events, drained.saturated)),
            Err(error) => {
                tracing::info!(%error, "controller left desktop navigation");
                self.close(id);
                self.rescan = true;
                None
            }
        }
    }

    fn take_wakes(&mut self) -> Vec<ControllerWake> {
        std::mem::take(&mut self.wakes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> ControllerId {
        ControllerId::at(name)
    }

    /// A machine that never touches evdev: what `discover` found, and which
    /// of them are leased away.
    fn machine(known: &[(&str, ControllerFamily)], leased: &[&str]) -> Machine {
        Machine {
            catalog: LinuxPhysicalGamepadCatalog::default(),
            hotplug: None,
            known: known
                .iter()
                .map(|(name, family)| (id(name), *family))
                .collect(),
            open: BTreeMap::new(),
            leased: leased.iter().map(|name| id(name)).collect(),
            refused: BTreeSet::new(),
            rescan: false,
            next_key: 1,
            wakes: Vec::new(),
        }
    }

    fn names(controllers: Vec<(ControllerId, ControllerFamily)>) -> Vec<ControllerId> {
        controllers.into_iter().map(|(id, _)| id).collect()
    }

    #[test]
    fn a_leased_controller_is_left_alone_until_its_lease_ends() {
        let machine = machine(
            &[
                ("room pad", ControllerFamily::Xbox),
                ("headset pad", ControllerFamily::Nintendo),
            ],
            &["headset pad"],
        );
        assert_eq!(names(machine.wanted()), vec![id("room pad")]);
        // The leased one is back once its lease ends.
        let unleased = wanted(&machine.known, &BTreeSet::new(), &machine.refused);
        assert_eq!(
            names(unleased),
            vec![id("headset pad"), id("room pad")],
            "both are the desktop's again"
        );
    }

    #[test]
    fn a_controller_that_could_not_be_opened_is_not_asked_again() {
        let machine = machine(&[("room pad", ControllerFamily::Xbox)], &[]);
        let refused: BTreeSet<ControllerId> = [id("room pad")].into();
        assert!(wanted(&machine.known, &machine.leased, &refused).is_empty());
    }
}

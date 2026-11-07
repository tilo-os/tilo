//! One workspace's presence, merged per publisher: what each device here
//! last said of itself, and what this device says.
//!
//! A frame carries one part of a device ([`Payload`]), so a reader merges the
//! parts it hears into what each member says now, and keeps a revision of
//! when that changed: a surface draws the people it lists when the revision
//! moved, and asks for no frame while it holds still. Who a device is, is its
//! certificate's word, not its own: the board is told what a verified
//! certificate named ([`Board::certify`]), so nobody here can be named as
//! somebody else.

use collab_types::presence::{
    Availability, CallPresence, DESKTOP_APP, Payload, PointerSpace, Presence, PresenceKey,
    ShellPresence,
};

use super::place::{Placed, Tile, tile};
use super::publish::{KEEPALIVE_MS, Schedule};
use super::smooth::Smoother;

/// A device here, as the workspace names it: its endpoint key.
pub type Device = [u8; 32];

/// The name a person gives themselves here: what everyone calls them,
/// whichever of their devices says it.
const NAME_EDGE: usize = 64;

/// A pointer over a tile: the tab it is over and the point in that tile's
/// logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// The tab whose tile the pointer is over.
    pub tab: u64,
    /// The point in that tile's pixels.
    pub x: f32,
    /// The point in that tile's pixels.
    pub y: f32,
}

/// A device's call, as its presence carries it to everyone in the workspace:
/// the channel it is in, and what it sends of the person there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The channel's id and, once someone named it, its name.
    pub call: CallPresence,
    /// This device's camera is on in the call.
    pub camera: bool,
    /// This device shares its screen in the call.
    pub screen: bool,
}

/// A wave: the count it carries, and the member it is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wave {
    /// A new count is a new wave; the same one restated is the same wave.
    pub seq: u32,
    /// The member's publisher, as hex.
    pub target: String,
}

/// What one device says of itself here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Own {
    /// The person's name, as everyone here sees it.
    pub label: String,
    /// What this device calls itself, from its certificate.
    pub device: String,
    /// The person this device belongs to, their root key as hex.
    pub user: Option<String>,
    /// What they say they are doing, in their own words.
    pub status: Option<String>,
    /// How available they say they are.
    pub availability: Availability,
    /// The call this device is in here, if any.
    pub call: Option<Call>,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
    /// The last wave they sent here.
    pub wave: Option<Wave>,
    /// Where their pointer is over a tile here.
    pub pointer: Option<Point>,
}

/// A call in progress in a workspace, as the people in it say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCall {
    /// The workspace the channel is in, as its id travels.
    pub workspace: String,
    /// Chat's id of the voice channel.
    pub channel: String,
    /// What the channel is called, as the people in it say; empty when
    /// nobody named it.
    pub name: String,
    /// Who is in it, in the order they arrived.
    pub people: Vec<LiveCaller>,
}

impl LiveCall {
    /// Whether `person` is in this call, on any of their devices: what a
    /// Join is not offered for.
    #[must_use]
    pub fn joined_by(&self, person: Option<&str>) -> bool {
        let Some(person) = person else {
            return false;
        };
        self.people
            .iter()
            .any(|caller| caller.user.as_deref() == Some(person))
    }
}

/// Someone in a call, as their device's presence names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCaller {
    /// Their device's publisher.
    pub id: String,
    /// The name everyone here calls them: the person's.
    pub label: String,
    /// The person behind the device (user root, hex), once a certificate
    /// named them.
    pub user: Option<String>,
    /// The device's camera is on in the call.
    pub camera: bool,
    /// The device shares its screen in the call.
    pub screen: bool,
}

/// Someone else here, as their frames and their certificate tell it.
#[derive(Clone, Debug)]
pub struct Member {
    /// Their device's publisher.
    pub publisher: Device,
    /// The person behind the device (user root, hex), once its certificate
    /// was heard.
    pub user: Option<String>,
    /// What the device calls itself, from its certificate: what tells two
    /// devices of one person apart.
    pub device: String,
    /// The name everyone here calls them, as their presence says it: the
    /// person's, never the device's.
    pub label: String,
    /// What they say they are doing, in their own words.
    pub status: Option<String>,
    /// How available they say they are.
    pub availability: Availability,
    /// Whose view they follow, by that member's publisher.
    pub following: Option<String>,
    /// The last wave they sent at someone here.
    pub wave: Option<Wave>,
    /// The call they are in, as their presence says.
    pub call: Option<Call>,
    /// Their own view of their desktop, as a shell — or an agent, which says
    /// its face, where it works and what it is doing the same way — said it.
    pub shell: Option<Box<ShellPresence>>,
    /// Every other app's last word under its own key.
    pub apps: Vec<(PresenceKey, Payload)>,
    /// They said they are here and have not left: what makes them a member.
    pub here: bool,
    pointer: Smoother,
}

impl Member {
    fn new(publisher: Device) -> Self {
        Self {
            publisher,
            user: None,
            device: String::new(),
            label: String::new(),
            status: None,
            availability: Availability::Available,
            following: None,
            wave: None,
            call: None,
            shell: None,
            apps: Vec::new(),
            here: false,
            pointer: Smoother::default(),
        }
    }

    /// Whether this member is an agent rather than a person.
    #[must_use]
    pub fn agent(&self) -> bool {
        self.shell.as_deref().is_some_and(|shell| shell.agent)
    }

    /// The name everyone here calls them: the person's own word, and the
    /// face its shell said before that arrived.
    #[must_use]
    pub fn label(&self) -> &str {
        if self.label.trim().is_empty() {
            self.shell
                .as_deref()
                .map_or("", |shell| shell.label.as_str())
        } else {
            &self.label
        }
    }

    /// What they are doing: what their shell says, else their own word.
    #[must_use]
    pub fn activity(&self) -> Option<&str> {
        self.shell
            .as_deref()
            .and_then(|shell| shell.activity.as_deref())
            .or(self.status.as_deref())
            .filter(|activity| !activity.trim().is_empty())
    }

    /// Where within its focus the member works, as its shell says.
    #[must_use]
    pub fn place(&self) -> Option<&str> {
        self.shell
            .as_deref()
            .and_then(|shell| shell.place.as_deref())
    }

    /// The window the member works in and the record it shows, as its shell
    /// says: the app, and the record's digest.
    #[must_use]
    pub fn focus(&self) -> Option<(&str, [u8; 8])> {
        self.shell.as_deref().and_then(ShellPresence::focus)
    }

    /// Their pointer, smoothed: the tab it is over and the point in that
    /// tile's pixels, at `now_ms`.
    #[must_use]
    pub fn pointer(&self, now_ms: f64) -> Option<(u64, f64, f64)> {
        let tab = self.pointer.tab()?;
        let (x, y) = self.pointer.at(now_ms)?;
        Some((tab, x, y))
    }

    /// The app's last word under `key`.
    #[must_use]
    pub fn app(&self, key: &PresenceKey) -> Option<&Payload> {
        self.apps
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, payload)| payload)
    }
}

/// One workspace's presence: this device's own word and the schedule that
/// sends it, and everyone else's merged state.
#[derive(Debug)]
pub struct Board {
    workspace: String,
    publisher: Device,
    own: Own,
    /// What this device said last: only what changed leaves.
    said: Own,
    /// Whether this device said it is here.
    arrived: bool,
    schedule: Schedule,
    keepalive_at: Option<f64>,
    members: Vec<Member>,
    revision: u64,
}

impl Board {
    /// The board of `workspace`, as `publisher` takes part in it.
    #[must_use]
    pub fn new(workspace: impl Into<String>, publisher: Device) -> Self {
        Self {
            workspace: workspace.into(),
            publisher,
            own: Own::default(),
            said: Own::default(),
            arrived: false,
            schedule: Schedule::default(),
            keepalive_at: None,
            members: Vec::new(),
            revision: 0,
        }
    }

    /// How many times what is known here changed: a surface drawing its
    /// people draws again when this moved.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Say `own`: what changed since the last word is queued, to leave under
    /// the one schedule ([`Self::due`]).
    pub fn say(&mut self, own: &Own) {
        self.own.clone_from(own);
        let said = std::mem::replace(&mut self.said, own.clone());
        let first = !std::mem::replace(&mut self.arrived, true);
        let person: String = own.label.trim().chars().take(NAME_EDGE).collect();
        let workspace = self.workspace.clone();
        if first || said.label.trim() != person {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Person { name: person },
            ));
        }
        if first {
            self.schedule
                .push(Presence::desktop(workspace.clone(), Payload::Arrived));
        }
        // How available a device is rides with its arrival: someone who came
        // in after the last change still hears it.
        if first || said.availability != own.availability || said.call != own.call {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Availability {
                    state: own.availability.id().to_owned(),
                    call: own.call.as_ref().map(|call| call.call.encode()),
                    camera: own.call.as_ref().is_some_and(|call| call.camera),
                    screen: own.call.as_ref().is_some_and(|call| call.screen),
                },
            ));
        }
        if said.status != own.status
            && let Some(status) = &own.status
        {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Status {
                    text: status.clone(),
                    automatic: false,
                },
            ));
        }
        if said.following != own.following
            && let Some(target) = &own.following
        {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Following {
                    target: Some(target.clone()),
                },
            ));
        }
        if said.wave != own.wave
            && let Some(wave) = &own.wave
        {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Wave {
                    seq: wave.seq,
                    target: wave.target.clone(),
                },
            ));
        }
        if said.pointer != own.pointer {
            match own.pointer {
                Some(pointer) => self.schedule.push(Presence::pointing(
                    workspace,
                    pointer.tab,
                    pointer.x,
                    pointer.y,
                )),
                None => {
                    if let Some(pointer) = said.pointer {
                        self.schedule
                            .push(Presence::pointer_left(workspace, pointer.tab));
                    }
                }
            }
        }
    }

    /// Say the whole of what this device is again: the frame set a member who
    /// came after the first word needs, and what loss repair and the slow
    /// restatement carry.
    fn restate(&mut self) {
        self.said = Own::default();
        self.arrived = false;
        let own = self.own.clone();
        self.say(&own);
    }

    /// The burst due at `now_ms`: the frames to say, with the object each is
    /// on the presence track. One burst at most per call; nothing before the
    /// rate ceiling has passed.
    pub fn due(&mut self, now_ms: f64) -> Option<Vec<(u32, Presence)>> {
        if self.keepalive_at.is_some_and(|at| now_ms >= at) {
            self.keepalive_at = Some(now_ms + KEEPALIVE_MS);
            self.restate();
        }
        let burst = self.schedule.due(now_ms)?;
        if self.keepalive_at.is_none() {
            self.keepalive_at = Some(now_ms + KEEPALIVE_MS);
        }
        Some(burst)
    }

    /// Every frame this device owes, whatever the ceiling: a join says the
    /// whole of what this device is at once.
    pub fn flush(&mut self, now_ms: f64) -> Vec<(u32, Presence)> {
        self.restate();
        let frames = self.schedule.flush(now_ms);
        if !frames.is_empty() {
            self.keepalive_at = Some(now_ms + KEEPALIVE_MS);
        }
        frames
    }

    /// When this board next has something to say, on the clock the caller
    /// hands [`Self::due`].
    #[must_use]
    pub fn next_due(&self, now_ms: f64) -> Option<f64> {
        let keepalive = self.keepalive_at.filter(|at| *at > now_ms);
        match (self.schedule.next_due(now_ms), keepalive) {
            (Some(said), Some(keepalive)) => Some(said.min(keepalive)),
            (said, keepalive) => said.or(keepalive),
        }
    }

    /// Take one member's frame: theirs, as its key and payload say it.
    /// Whether what is known here changed.
    pub fn hear(&mut self, from: Device, frame: &Presence, now_ms: f64) -> bool {
        if from == self.publisher {
            return false;
        }
        if frame.app != DESKTOP_APP {
            return self.keep(from, &frame.key, &frame.payload);
        }
        match (&frame.key, &frame.payload) {
            (
                PresenceKey::Pointer {
                    space: PointerSpace::Tile { workspace, tab },
                },
                Payload::Pointer { x, y },
            ) if *workspace == self.workspace => {
                self.moved(from, (*tab, f64::from(*x), f64::from(*y)), now_ms)
            }
            (
                PresenceKey::Pointer {
                    space: PointerSpace::Tile { workspace, .. },
                },
                Payload::Left,
            ) if *workspace == self.workspace => {
                let left = self
                    .members
                    .iter_mut()
                    .find(|member| member.publisher == from)
                    .filter(|member| !member.pointer.is_empty());
                left.is_some_and(|member| {
                    member.pointer.left();
                    self.revision = self.revision.wrapping_add(1);
                    true
                })
            }
            (PresenceKey::Workspace { id }, payload) if *id == self.workspace => match payload {
                Payload::Arrived => {
                    self.touch(from, |member| !std::mem::replace(&mut member.here, true))
                }
                Payload::Left => {
                    self.touch(from, |member| std::mem::replace(&mut member.here, false))
                }
                Payload::Status { text, .. } => self.touch(from, |member| {
                    let status = Some(text.trim().to_owned()).filter(|text| !text.is_empty());
                    std::mem::replace(&mut member.status, status) != member.status
                }),
                Payload::Availability {
                    state,
                    call,
                    camera,
                    screen,
                } => self.touch(from, |member| {
                    let call = call
                        .as_deref()
                        .and_then(CallPresence::decode)
                        .map(|call| Call {
                            call,
                            camera: *camera,
                            screen: *screen,
                        });
                    let availability =
                        Availability::from_id(state).unwrap_or(Availability::Available);
                    let changed = member.availability != availability || member.call != call;
                    member.availability = availability;
                    member.call = call;
                    changed
                }),
                Payload::Following { target } => self.touch(from, |member| {
                    std::mem::replace(&mut member.following, target.clone()) != member.following
                }),
                Payload::Wave { seq, target } => self.touch(from, |member| {
                    // A repeat of a wave said before is the same wave, never
                    // a newer one arriving late.
                    if member.wave.as_ref().is_some_and(|held| held.seq >= *seq) {
                        return false;
                    }
                    member.wave = Some(Wave {
                        seq: *seq,
                        target: target.clone(),
                    });
                    true
                }),
                // The person's own name for themselves: what everyone here
                // calls them, on whichever device they say it.
                Payload::Person { name } => self.touch(from, |member| {
                    let label: String = name.trim().chars().take(NAME_EDGE).collect();
                    std::mem::replace(&mut member.label, label) != member.label
                }),
                // A shell — a page's desktop, or an agent's face — says where
                // its person is and, for an agent, that it works here at all.
                Payload::Shell(shell) => self.touch(from, |member| {
                    let changed = !member.here || member.shell.as_deref() != Some(&**shell);
                    member.here = true;
                    member.shell = Some(shell.clone());
                    changed
                }),
                payload => self.keep(from, &frame.key, payload),
            },
            // Another workspace's word says nothing of the people here.
            (PresenceKey::Workspace { .. }, _) => false,
            (key, payload) => self.keep(from, key, payload),
        }
    }

    /// `from`'s pointer moved to `point`, in the pixels of the tile it is
    /// over. Whether it is one to draw: the same place said again is a rest.
    fn moved(&mut self, from: Device, point: (u64, f64, f64), now_ms: f64) -> bool {
        let heard = self
            .members
            .iter()
            .find(|member| member.publisher == from)
            .is_some_and(|member| member.here && member.pointer.newest() == Some(point));
        if heard {
            // The same place said again: the pointer rests.
            return false;
        }
        self.touch(from, |member| {
            // A pointer over a tile is the device saying it is here: it is
            // drawn whether or not the word that first said so reached this
            // device.
            member.here = true;
            member.pointer.heard(point.0, point.1, point.2, now_ms);
            true
        })
    }

    /// Keep an app's or another scope's last word under its key. Whether it
    /// changed.
    fn keep(&mut self, from: Device, key: &PresenceKey, payload: &Payload) -> bool {
        self.touch(from, |member| {
            let at = member.apps.iter().position(|(held, _)| held == key);
            if matches!(payload, Payload::Left) {
                let Some(at) = at else {
                    return false;
                };
                member.apps.remove(at);
                return true;
            }
            if let Some(at) = at {
                let Some((_, said)) = member.apps.get_mut(at) else {
                    return false;
                };
                if said == payload {
                    return false;
                }
                said.clone_from(payload);
                true
            } else {
                member.apps.push((key.clone(), payload.clone()));
                true
            }
        })
    }

    /// Change what is known of `from`, starting their entry the first time
    /// they say anything; `change` says whether it is one to draw. Whether it
    /// was.
    fn touch(&mut self, from: Device, change: impl FnOnce(&mut Member) -> bool) -> bool {
        if !self.members.iter().any(|member| member.publisher == from) {
            self.members.push(Member::new(from));
        }
        let changed = self
            .members
            .iter_mut()
            .find(|member| member.publisher == from)
            .is_some_and(change);
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// The roster's word on `from`: a certificate this device verified named
    /// the person and the device. Whether what is known here changed.
    pub fn certify(&mut self, from: Device, user: String, device: &str) -> bool {
        self.touch(from, |member| {
            let changed = member.user.as_deref() != Some(user.as_str()) || member.device != device;
            member.user = Some(user);
            device.trim().clone_into(&mut member.device);
            changed
        })
    }

    /// They left the workspace.
    pub fn depart(&mut self, publisher: Device) -> bool {
        let before = self.members.len();
        self.members.retain(|member| member.publisher != publisher);
        let changed = self.members.len() != before;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Everyone heard here, as they arrived.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// Who is here: those who said so and have not left.
    pub fn here(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|member| member.here)
    }

    /// The member `publisher` names, if they said anything here.
    #[must_use]
    pub fn member(&self, publisher: Device) -> Option<&Member> {
        self.members
            .iter()
            .find(|member| member.publisher == publisher)
    }

    /// Everyone whose pointer is over `tab` now, by publisher, smoothed at
    /// `now_ms`.
    #[must_use]
    pub fn pointers(&self, tab: u64, now_ms: f64) -> Vec<(Device, f64, f64)> {
        self.here()
            .filter_map(|member| match member.pointer(now_ms) {
                Some((at, x, y)) if at == tab => Some((member.publisher, x, y)),
                _ => None,
            })
            .collect()
    }

    /// Everyone whose pointer is over a tile the surface shows, where that
    /// tile stands on it: what a surface draws.
    #[must_use]
    pub fn placed(&self, tiles: &[Tile], now_ms: f64) -> Vec<Placed> {
        self.here()
            .filter_map(|member| {
                let (tab, x, y) = member.pointer(now_ms)?;
                let over = tile(tiles, tab)?;
                let (x, y) = over.point((x, y));
                Some(Placed {
                    publisher: member.publisher,
                    tab,
                    x,
                    y,
                })
            })
            .collect()
    }

    /// The calls in progress here, as those in them say: the people of one
    /// channel under its id and name, in the order they arrived.
    #[must_use]
    pub fn calls(&self) -> Vec<LiveCall> {
        let mut calls: Vec<LiveCall> = Vec::new();
        for member in self.here() {
            let Some(presence) = &member.call else {
                continue;
            };
            let caller = LiveCaller {
                id: hex::encode(member.publisher),
                label: member.label().to_owned(),
                user: member.user.clone(),
                camera: presence.camera,
                screen: presence.screen,
            };
            match calls
                .iter_mut()
                .find(|known| known.channel == presence.call.channel)
            {
                Some(call) => {
                    if call.name.trim().is_empty() {
                        call.name.clone_from(&presence.call.name);
                    }
                    call.people.push(caller);
                }
                None => calls.push(LiveCall {
                    workspace: self.workspace.clone(),
                    channel: presence.call.channel.clone(),
                    name: presence.call.name.clone(),
                    people: vec![caller],
                }),
            }
        }
        calls
    }
}

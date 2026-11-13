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
    ShellPresence, ShownWave as Wave,
};

use super::publish::{KEEPALIVE_MS, Schedule};

/// How long a member may say nothing before this device forgets them: every
/// device here says itself again each [`KEEPALIVE_MS`], so this is two of
/// those words missed with room for a late timer, which a device still
/// connected does not miss and one that is gone always does.
pub const SILENT_MS: f64 = KEEPALIVE_MS * 2.0 + 2_000.0;
use super::smooth::Smoother;

/// A device here, as the workspace names it: its endpoint key.
pub type Device = [u8; 32];

/// `shell` without its sequence and send time: what a shell says, which two
/// views of it are compared by, never when it was said.
pub fn without_clock(shell: &ShellPresence) -> ShellPresence {
    ShellPresence {
        seq: 0,
        sent_at_ms: 0,
        ..shell.clone()
    }
}

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

/// What someone is doing, as their device says it: their own words, or the
/// line the device read off the steps they took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// One line, e.g. "Editing ‘Launch plan’ in Notes".
    pub text: String,
    /// The device read it off what they did; `false` for their own words.
    pub automatic: bool,
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
    /// When the person last changed their profile, as this device knows it (Unix milliseconds,
    /// `0` while it knows none).
    pub profile_ms: u64,
    /// What they are doing: their own words, or what this device read off
    /// their steps.
    pub status: Option<Status>,
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
    /// This device's own view of its desktop, as a shell: its face, its
    /// screen, the caret in it and what it says beside its pointer.
    pub shell: Option<Box<ShellPresence>>,
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
    /// When the person last changed their profile, as their device says it (Unix milliseconds,
    /// `0` while it said none).
    pub profile_ms: u64,
    /// What they are doing, as their device says it.
    pub status: Option<Status>,
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
    /// The sequence of the view in `shell` for a run the host speaks for: a
    /// view at or below it is one already taken, or one that came late, and
    /// taking it would say what the run was doing before.
    shell_seq: u32,
    /// Every other app's last word under its own key.
    pub apps: Vec<(PresenceKey, Payload)>,
    /// They said they are here and have not left: what makes them a member.
    pub here: bool,
    /// When this stay here began: it moves each time the device comes back
    /// after leaving, so a surface tells a member from the one before it.
    pub epoch: u64,
    /// The device that executes this member and speaks for it
    /// ([`Presence::on_behalf`]): the workspace's host for a run it serves,
    /// or a member's device for an agent its person admitted there. The agent
    /// says nothing itself.
    pub spoken_by: Option<Device>,
    /// The run it is, as the device that speaks for it names it.
    run: Option<String>,
    pointer: Smoother,
    /// When this device last heard them say anything, on the clock the
    /// caller hands [`Board::hear`]: `None` while only the roster named them.
    heard_ms: Option<f64>,
}

impl Member {
    fn new(publisher: Device) -> Self {
        Self {
            publisher,
            user: None,
            device: String::new(),
            label: String::new(),
            profile_ms: 0,
            status: None,
            availability: Availability::Available,
            following: None,
            wave: None,
            call: None,
            shell: None,
            shell_seq: 0,
            apps: Vec::new(),
            here: false,
            epoch: 0,
            spoken_by: None,
            run: None,
            pointer: Smoother::default(),
            heard_ms: None,
        }
    }

    /// They said they are here: their stay begins if it had not, which is
    /// what a surface reads a new member by. Whether it is a change.
    fn arrive(&mut self) -> bool {
        if std::mem::replace(&mut self.here, true) {
            return false;
        }
        self.epoch = self.epoch.wrapping_add(1);
        true
    }

    /// Take `shell`, the view they said of themselves.
    ///
    /// A run says each step it takes with a higher sequence: a view
    /// at or below the one already taken is a repeat of it, sent
    /// again or arrived late, and taking it would say what the run
    /// was doing before the step it is on now. A person's device
    /// stamps each view it says with when it said it: each frame is
    /// a group of its own, which the track hands over as it lands,
    /// so the caret a burst left at can land before one it passed
    /// on the way. A view said earlier than the one taken is passed
    /// over, so the latest always stands. Whether it is a change.
    fn take_shell(&mut self, shell: &ShellPresence) -> bool {
        if self.spoken_by.is_some() && shell.seq.wrapping_sub(self.shell_seq) >= u32::MAX / 2 {
            return false;
        }
        if self.spoken_by.is_none()
            && self
                .shell
                .as_deref()
                .is_some_and(|held| shell.sent_at_ms < held.sent_at_ms)
        {
            return false;
        }
        self.shell_seq = shell.seq;
        let changed = self.arrive() || self.shell.as_deref() != Some(shell);
        self.shell = Some(Box::new(shell.clone()));
        changed
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

    /// What they are doing: what their shell says (an agent's line), else
    /// their status, and whether their device read it off their steps rather
    /// than anyone saying it.
    #[must_use]
    pub fn activity(&self) -> Option<(&str, bool)> {
        self.shell
            .as_deref()
            .and_then(|shell| shell.activity.as_deref())
            .filter(|activity| !activity.trim().is_empty())
            .map(|activity| (activity, false))
            .or_else(|| {
                self.status
                    .as_ref()
                    .map(|status| (status.text.as_str(), status.automatic))
            })
            .filter(|(activity, _)| !activity.trim().is_empty())
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

    /// When they last said where their pointer was, or `None` while it has
    /// left: on the clock presence is heard on, in Unix milliseconds.
    #[must_use]
    pub fn pointer_heard_at(&self) -> Option<f64> {
        self.pointer.heard_at()
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
    /// The sequence of the last shell view this device said: a receiver takes
    /// a view with a higher one.
    shell_seq: u32,
    members: Vec<Member>,
    revision: u64,
    /// The device the workspace's governance pins as its host: a device that
    /// is no member here, whose word for the runs it executes is taken
    /// ([`Self::set_host`]).
    host: Option<Device>,
    /// When this device last took a pass over the board, hearing or saying.
    listened_ms: Option<f64>,
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
            shell_seq: 0,
            members: Vec::new(),
            revision: 0,
            host: None,
            listened_ms: None,
        }
    }

    /// Take `host` as the workspace's host, as the workspace's governance
    /// pins it on this device: what it says for the runs it executes is
    /// heard as one member per run, as a member's word for its own runs is.
    /// Runs a former host spoke for leave. Whether what is known here changed.
    pub fn set_host(&mut self, host: Option<Device>) -> bool {
        if self.host == host {
            return false;
        }
        let former = std::mem::replace(&mut self.host, host);
        let before = self.members.len();
        self.members
            .retain(|member| member.spoken_by.is_none() || member.spoken_by != former);
        let changed = self.members.len() != before;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Who `frame`, heard from `from`, speaks for: `from` itself, or the run
    /// it says the frame for, as that run's own member. A run is the
    /// workspace's host's, or a member's here: its member is keyed by the
    /// device that speaks for it, so nobody speaks for another device's runs.
    /// `None` for a word on a run from a device that is neither.
    fn speaker(&mut self, from: Device, frame: &Presence) -> Option<Device> {
        let Some(run) = &frame.on_behalf else {
            return Some(from);
        };
        let member_here = self
            .member(from)
            .is_some_and(|member| member.here && member.spoken_by.is_none());
        if self.host != Some(from) && !member_here {
            return None;
        }
        let stand_in = collab_types::presence::stand_in(from, run);
        if !self
            .members
            .iter()
            .any(|member| member.publisher == stand_in)
        {
            let mut member = Member::new(stand_in);
            member.spoken_by = Some(from);
            member.run = Some(run.clone());
            // The run is said on its speaker's device: it lists as that device.
            if let Some(speaker) = self.member(from) {
                member.device.clone_from(&speaker.device);
            }
            self.members.push(member);
        }
        Some(stand_in)
    }

    /// Speak for `runs` here: the runs this device executes (an agent its
    /// person admitted at this device's door), each with its face. Each is a
    /// member of its own on this board, as another device's runs are, and is
    /// said on the presence track for everyone here; a run spoken for before
    /// and missing from `runs` leaves. Whether what is known here changed.
    pub fn speak_for(&mut self, runs: &[(String, ShellPresence)]) -> bool {
        let own = Some(self.publisher);
        let gone: Vec<String> = self
            .members
            .iter()
            .filter(|member| member.spoken_by == own)
            .filter_map(|member| member.run.clone())
            .filter(|run| runs.iter().all(|(held, _)| held != run))
            .collect();
        let mut changed = !gone.is_empty();
        for run in gone {
            self.members
                .retain(|member| member.spoken_by != own || member.run.as_ref() != Some(&run));
            self.schedule
                .push(Presence::desktop(self.workspace.clone(), Payload::Left).spoken_for(run));
        }
        for (run, view) in runs {
            let stand_in = collab_types::presence::stand_in(self.publisher, run);
            if !self
                .members
                .iter()
                .any(|member| member.publisher == stand_in)
            {
                let mut member = Member::new(stand_in);
                member.spoken_by = own;
                member.run = Some(run.clone());
                member.device.clone_from(&self.own.device);
                self.members.push(member);
            }
            let Some(member) = self
                .members
                .iter_mut()
                .find(|member| member.publisher == stand_in)
            else {
                continue;
            };
            if member.arrive() {
                self.schedule.push(
                    Presence::desktop(self.workspace.clone(), Payload::Arrived).spoken_for(run),
                );
                changed = true;
            }
            if member.shell.as_deref().map(without_clock) != Some(without_clock(view)) {
                member.shell_seq = member.shell_seq.wrapping_add(1);
                let mut view = Box::new(view.clone());
                view.seq = member.shell_seq;
                member.shell = Some(view.clone());
                self.schedule.push(
                    Presence::desktop(self.workspace.clone(), Payload::Shell(view)).spoken_for(run),
                );
                changed = true;
            }
        }
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// How many times what is known here changed: a surface drawing its
    /// people draws again when this moved.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// What this device last said here: what a surface draws for the seat
    /// the board is driven at, which never comes back from the wire.
    #[must_use]
    pub const fn own(&self) -> &Own {
        &self.own
    }

    /// Take `own` as already known here, without saying it: a device whose
    /// identity it keeps itself says it before its first word.
    pub fn seed(&mut self, own: Own) {
        self.said = own.clone();
        self.own = own;
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
        if own.profile_ms != 0 && (first || said.profile_ms != own.profile_ms) {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Profile {
                    changed_ms: own.profile_ms,
                },
            ));
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
        // A status taken back is said as an empty one, or everyone keeps
        // showing the last.
        if said.status != own.status {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Status {
                    text: own
                        .status
                        .as_ref()
                        .map_or_else(String::new, |status| status.text.clone()),
                    automatic: own.status.as_ref().is_some_and(|status| status.automatic),
                },
            ));
        }
        // Following and stopping are both a change: a member who stops
        // following says so, or everyone else keeps showing it.
        if said.following != own.following {
            self.schedule.push(Presence::desktop(
                workspace.clone(),
                Payload::Following {
                    target: own.following.clone(),
                },
            ));
        }
        // A shell says itself whole: a change of face, screen or caret is
        // one frame, and the same view said again is none. Its sequence
        // moves with what leaves, never with what is said.
        if said.shell.as_deref().map(without_clock) != own.shell.as_deref().map(without_clock)
            && let Some(shell) = &own.shell
        {
            self.shell_seq = self.shell_seq.wrapping_add(1);
            let mut shell = shell.clone();
            shell.seq = self.shell_seq;
            self.schedule
                .push(Presence::desktop(workspace.clone(), Payload::Shell(shell)));
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
        // The runs this device speaks for are said after it, so a reader
        // already hears this device here when their word arrives.
        let own = Some(self.publisher);
        for member in self.members.iter().filter(|member| member.spoken_by == own) {
            let Some(run) = &member.run else {
                continue;
            };
            self.schedule.push(
                Presence::desktop(self.workspace.clone(), Payload::Arrived).spoken_for(run.clone()),
            );
            if let Some(shell) = &member.shell {
                self.schedule.push(
                    Presence::desktop(self.workspace.clone(), Payload::Shell(shell.clone()))
                        .spoken_for(run.clone()),
                );
            }
        }
    }

    /// The burst due at `now_ms`: the frames to say, with the object each is
    /// on the presence track. One burst at most per call; nothing before the
    /// rate ceiling has passed.
    ///
    /// The members heard from no longer are forgotten on the same pass
    /// ([`SILENT_MS`]), so a caller reads [`Self::revision`] after it.
    pub fn due(&mut self, now_ms: f64) -> Option<Vec<(u32, Presence)>> {
        self.forget_silent(now_ms);
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
        self.listen(now_ms);
        if from == self.publisher {
            return false;
        }
        let Some(from) = self.speaker(from, frame) else {
            return false;
        };
        let changed = self.take(from, frame, now_ms);
        if let Some(member) = self
            .members
            .iter_mut()
            .find(|member| member.publisher == from)
        {
            member.heard_ms = Some(now_ms);
        }
        changed
    }

    /// Forget every member nothing was heard from for [`SILENT_MS`]: a
    /// device whose page closed without saying it left, or whose link
    /// dropped, keeps no row here. Whether what is known here changed.
    pub fn forget_silent(&mut self, now_ms: f64) -> bool {
        self.listen(now_ms);
        let before = self.members.len();
        self.members.retain(|member| {
            member
                .heard_ms
                .is_none_or(|heard| now_ms - heard < SILENT_MS)
        });
        let changed = self.members.len() != before;
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// A pass over the board at `now_ms`. A device takes one at least every [`KEEPALIVE_MS`],
    /// when its own word is due; a longer gap is time it could hear nobody, such as a page busy
    /// drawing one slow frame, while what the others said waited for it. That time is not their
    /// silence: a member is forgotten for [`SILENT_MS`] this device listened without hearing them.
    fn listen(&mut self, now_ms: f64) {
        if let Some(last) = self.listened_ms {
            let deaf = now_ms - last - KEEPALIVE_MS;
            if deaf > 0.0 {
                for heard in self
                    .members
                    .iter_mut()
                    .filter_map(|member| member.heard_ms.as_mut())
                {
                    *heard += deaf;
                }
            }
        }
        self.listened_ms = Some(self.listened_ms.map_or(now_ms, |last| last.max(now_ms)));
    }

    /// Take one member's frame into what is known of them.
    fn take(&mut self, from: Device, frame: &Presence, now_ms: f64) -> bool {
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
                Payload::Arrived => self.touch(from, Member::arrive),
                Payload::Left => {
                    self.touch(from, |member| std::mem::replace(&mut member.here, false))
                }
                Payload::Profile { changed_ms } => self.touch(from, |member| {
                    std::mem::replace(&mut member.profile_ms, *changed_ms) != member.profile_ms
                }),
                Payload::Status { text, automatic } => self.touch(from, |member| {
                    let status = Some(text.trim())
                        .filter(|text| !text.is_empty())
                        .map(|text| Status {
                            text: text.to_owned(),
                            automatic: *automatic,
                        });
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
                Payload::Shell(shell) => self.touch(from, |member| member.take_shell(shell)),
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
            // The same place said again: the pointer rests. It is still said,
            // though — a member restates what did not change on every
            // keepalive, and a surface draws only a pointer said lately. A
            // pointer that was moving is drawn where it stopped from now on.
            return self.touch(from, |member| member.pointer.restated(now_ms));
        }
        self.touch(from, |member| {
            // A pointer over a tile is the device saying it is here: it is
            // drawn whether or not the word that first said so reached this
            // device.
            member.arrive();
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

    /// Take one member's frame as the presence track carried it: the body is
    /// opened here, so a host never keeps a second copy of the wire's shape.
    /// An empty body is the plane saying their presence ended with their
    /// session ([`crate::plane::Heard::left`]): they left. Whether what is
    /// known here changed.
    pub fn heard(&mut self, from: Device, body: &[u8], now_ms: f64) -> bool {
        if body.is_empty() {
            return from != self.publisher && self.depart(from);
        }
        super::decode(body).is_some_and(|frame| self.hear(from, &frame, now_ms))
    }

    /// The roster's word on `from`: a certificate this device verified named
    /// the person and the device. Whether what is known here changed.
    pub fn certify(&mut self, from: Device, user: String, device: &str) -> bool {
        // The runs a device speaks for list as that device.
        for run in self
            .members
            .iter_mut()
            .filter(|member| member.spoken_by == Some(from))
        {
            device.trim().clone_into(&mut run.device);
        }
        self.touch(from, |member| {
            let changed = member.user.as_deref() != Some(user.as_str()) || member.device != device;
            member.user = Some(user);
            device.trim().clone_into(&mut member.device);
            changed
        })
    }

    /// They left the workspace, and the runs they spoke for.
    pub fn depart(&mut self, publisher: Device) -> bool {
        let before = self.members.len();
        // The runs a device spoke for leave with it.
        self.members
            .retain(|member| member.publisher != publisher && member.spoken_by != Some(publisher));
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

    /// Everyone whose pointer is over `tab` now, by publisher, smoothed at `now_ms`: the point is
    /// in that tile's own pixels, and a surface places it where that tile stands.
    #[must_use]
    pub fn pointers(&self, tab: u64, now_ms: f64) -> Vec<(Device, f64, f64)> {
        self.here()
            .filter_map(|member| match member.pointer(now_ms) {
                Some((at, x, y)) if at == tab => Some((member.publisher, x, y)),
                _ => None,
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

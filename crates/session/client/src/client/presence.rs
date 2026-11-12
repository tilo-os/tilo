//! Who is here: the workspaces this session takes part in, what each member said of themselves in
//! them, and this device's own word. Every host drives the same board and says what it owes on its
//! own transport; nothing here knows one, so this half of a client is `Send` and a host's
//! thread-bound parts stay behind.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Duration;
use tilo_clock::Instant;

use collab_types::presence::{Payload, Presence as Frame, ShellPresence, Shown};
use iroh_base::EndpointId;
use tilo_desktop_model::WorkspaceId as DeskWorkspaceId;
use tilo_live::presence;
use tilo_live::presence::Board;

use crate::plane::{Device, Event, SayPresence, Topic};
use crate::presence::{OwnPresence, Plane, Workspace, now_ms};

/// Where this person is here: their pointer over a tile, and their shell.
#[derive(Clone, Debug, Default)]
struct Where {
    pointer: Option<presence::Point>,
    shell: Option<Box<ShellPresence>>,
}

impl Where {
    /// Whether `other` says where this person is as `self` does: a shell is the same one while what
    /// it says is the same, never because it was restamped.
    fn same(&self, other: &Self) -> bool {
        self.pointer == other.pointer
            && self.shell.as_deref().map(presence::without_clock)
                == other.shell.as_deref().map(presence::without_clock)
    }
}

/// What this session says of itself here, what each workspace's board heard, and where what it says
/// leaves: the half of a client that any host's own thread can hold.
pub struct Presence {
    plane: RefCell<Plane>,
    own: RefCell<OwnPresence>,
    /// Where this person is on this desk, as the surfaces here read it: the pointer over the tile
    /// it is on, and their screen as a shell. Said with the rest of their word, and said again only
    /// when it moves.
    says: RefCell<Where>,
    /// The count this person's last wave carried here.
    wave_seq: Cell<u32>,
    /// This device's view was asked to follow someone by its own people panel, and the pass that
    /// reads the person's gesture has not heard it yet: the press that asks to follow is not the
    /// person at work taking the view back.
    follow_asked: Cell<bool>,
    /// What this device said since the last call, for what reads it off a session that keeps no
    /// plane of its own.
    said: RefCell<Vec<Frame>>,
    /// The wave this person sent last, for their own row to say sent.
    sent_wave: RefCell<Option<presence::Wave>>,
    /// Where this device's word of itself goes on each workspace's presence track, as the transport
    /// it says on carries it: `None` on a host that says presence nowhere.
    saying: Option<Arc<dyn SayPresence>>,
    /// Why the last word was not said on the live plane, while it was not: told once when saying
    /// starts failing or fails for another reason, and once when it is said again.
    unsaid: RefCell<Option<String>>,
    /// What this device calls itself, from its certificate: the name its person's other devices are
    /// told apart by. Empty on a device that named none.
    device: String,
}

impl std::fmt::Debug for Presence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presence")
            .field("publisher", &self.publisher())
            .finish_non_exhaustive()
    }
}

impl Presence {
    /// A presence saying this device's word of itself on `saying`'s tracks, for the device
    /// `device` names. What the transport heard reaches it through [`Self::heard`].
    #[must_use]
    pub fn over(saying: Option<Arc<dyn SayPresence>>, device: &Device, own: OwnPresence) -> Self {
        Self {
            plane: RefCell::new(Plane::new(device.publisher)),
            own: RefCell::new(own),
            says: RefCell::default(),
            wave_seq: Cell::new(0),
            follow_asked: Cell::new(false),
            said: RefCell::new(Vec::new()),
            sent_wave: RefCell::new(None),
            saying,
            unsaid: RefCell::new(None),
            device: device.name.clone(),
        }
    }

    /// This device's publisher here: the endpoint id every workspace names it by.
    #[must_use]
    pub fn publisher(&self) -> EndpointId {
        self.plane.borrow().publisher
    }

    /// What this session says of itself here now.
    #[must_use]
    pub fn own(&self) -> OwnPresence {
        self.own.borrow().clone()
    }

    /// What this session says of itself: changed here, it is said again in the workspace in front.
    /// Whether it changed.
    pub fn set_own(&self, own: OwnPresence) -> bool {
        if *self.own.borrow() == own {
            return false;
        }
        *self.own.borrow_mut() = own;
        self.say_own();
        true
    }

    /// Follow `target` (or nobody, `None`): this person's own word on whose view is theirs. Answers
    /// whether it changed. An ask made here is not the person working, so the gesture that carried
    /// it does not take back the view it began ([`Self::follow_asked`]).
    pub fn set_following(&self, target: Option<String>) -> bool {
        let mut own = self.own();
        if own.following == target {
            return false;
        }
        own.following = target;
        self.follow_asked.set(true);
        self.set_own(own)
    }

    /// Whether this device's view was asked to follow someone and the gesture carrying the ask is
    /// still in flight: what keeps it from ending the following it began.
    #[must_use]
    pub fn follow_asked(&self) -> bool {
        self.follow_asked.get()
    }

    /// A gesture about the following itself (a press on the banner that names whose view this
    /// is, opening their card) is not the person working either: it leaves the view it follows as
    /// it is, as an ask to follow does ([`Self::follow_asked`]).
    pub fn keep_following_through_gesture(&self) {
        if self.own().following.is_some() {
            self.follow_asked.set(true);
        }
    }

    /// The gesture that asked to follow has ended: the next ask is a new one.
    pub fn forget_follow_asked(&self) {
        self.follow_asked.set(false);
    }

    /// Where this person is on this desk now: their `pointer` over the tile it is on, and their
    /// `shell` as the others' surfaces draw it. Said with the rest of their word, and said again
    /// only when one of them moved. Whether that changed anything.
    pub fn set_where(
        &self,
        pointer: Option<presence::Point>,
        shell: Option<Box<ShellPresence>>,
    ) -> bool {
        let where_ = Where { pointer, shell };
        if self.says.borrow().same(&where_) {
            return false;
        }
        *self.says.borrow_mut() = where_;
        self.say_own();
        true
    }

    /// This device joined `call`, or left it (`None`): said again in the workspace in front, so
    /// everyone here reads which channel this person is in beside their face. Whether that changed
    /// anything.
    pub fn in_call(&self, call: Option<crate::InCall>) -> bool {
        let mut own = self.own.borrow().clone();
        if own.call == call {
            return false;
        }
        own.call = call;
        self.set_own(own)
    }

    /// Whether this person is in the call in `channel`, on any of their devices: what a Join is
    /// not offered for.
    #[must_use]
    pub fn is_in_call(&self, channel: &str) -> bool {
        let own = self.own.borrow();
        if own
            .call
            .as_ref()
            .is_some_and(|call| call.call.channel == channel)
        {
            return true;
        }
        let user = own.user.clone();
        drop(own);
        self.calls()
            .iter()
            .any(|call| call.channel == channel && call.joined_by(user.as_deref()))
    }

    /// Take part in each of `live`, leave the ones gone, and remember `active` as the one in front.
    /// Each workspace joined is handed to `joined` — its transport subscribes, and its host opens
    /// what it carries — and each one left to `left`, in the order the boards take them. Answers
    /// whether anything changed.
    pub fn hold(
        &self,
        live: &[(Topic, String)],
        active: Option<Topic>,
        mut joined: impl FnMut(&str, Topic),
        mut left: impl FnMut(Topic),
    ) -> bool {
        let mut joining = Vec::new();
        let mut leaving: Vec<(Topic, String)> = Vec::new();
        let mut changed = false;
        let mut front_changed = false;
        {
            let mut plane = self.plane.borrow_mut();
            let publisher = *plane.publisher.as_bytes();
            for (ws, id) in live {
                if plane.workspaces.iter().all(|workspace| workspace.ws != *ws) {
                    plane
                        .workspaces
                        .push(Workspace::new(*ws, id.clone(), publisher));
                    joining.push((*ws, id.clone()));
                }
            }
            for workspace in &plane.workspaces {
                if live.iter().all(|(ws, _)| *ws != workspace.ws) {
                    leaving.push((workspace.ws, workspace.id.clone()));
                }
            }
            plane
                .workspaces
                .retain(|held| leaving.iter().all(|(ws, _)| *ws != held.ws));
            if plane.active != active {
                plane.active = active;
                front_changed = true;
                changed = true;
            }
        }
        for (ws, id) in joining {
            joined(&id, ws);
            if DeskWorkspaceId::parse(&id).is_some() {
                self.say_into(&id);
            }
            changed = true;
        }
        for (ws, id) in leaving {
            // The members take this device down where it was.
            if let Some(workspace) = DeskWorkspaceId::parse(&id) {
                self.say_frames(workspace, vec![(0, Frame::desktop(&id, Payload::Left))]);
            }
            left(ws);
            changed = true;
        }
        if front_changed {
            // A workspace coming to the front has not heard this device yet: it says who it is
            // there at once.
            self.say_own();
        }
        changed
    }

    /// Take part in the workspaces this session's transport already put it in, as a host that
    /// joined them itself does, and remember `active` as the one in front. Answers whether
    /// anything changed.
    pub fn taking_part(&self, live: &[(Topic, String)], active: Option<Topic>) -> bool {
        self.hold(live, active, |_, _| {}, |_| {})
    }

    /// When this person next says again where they are, so the workspaces hear they are still here
    /// with no pass in between.
    pub fn next_deadline(&self) -> Option<Instant> {
        let now = now_ms();
        let due = self.plane.borrow().next_due(now)?;
        let wait = (due - now).max(0.0) / 1_000.0;
        tilo_clock::now().checked_add(Duration::from_secs_f64(wait))
    }

    /// Say what `workspace`'s board owes now — the keepalives, the repeats and the loss repairs —
    /// as a host that shows one workspace at a time asks with its own timer. Whether anything left.
    pub fn due_in(&self, workspace: &str) -> bool {
        let now = now_ms();
        let Some((desk, frames)) = self.plane.borrow_mut().due_in(workspace, now) else {
            return false;
        };
        self.say_frames(desk, frames);
        true
    }

    /// When `workspace`'s board next owes a word, in Unix milliseconds: what a host with its own
    /// clock arms its timer with.
    #[must_use]
    pub fn next_due_in(&self, workspace: &str) -> Option<f64> {
        self.plane.borrow().next_due_in(workspace, now_ms())
    }

    /// One member's word about themselves on `workspace`'s live plane: what they said of
    /// themselves, where their pointer went. A session takes these from its planes on every
    /// [`follow`](Self::follow); one with no plane of its own hands them here instead. Answers
    /// whether a surface that draws them draws again.
    pub fn heard_presence(&self, workspace: &str, from: EndpointId, frame: &Frame) -> bool {
        self.heard_word(workspace, from, |board, now| {
            board.hear(*from.as_bytes(), frame, now)
        })
    }

    /// One member's presence frame as its track carried it, opened by the workspace's board: what a
    /// host that carries the frames itself (a page's endpoint) hands its session.
    /// Take `host` as the device `workspace`'s governance pins as its host: its board hears what
    /// that host says for the runs it executes, one member per run, and nobody else's such word.
    /// Whether what is known there changed.
    pub fn set_host(&self, workspace: &str, host: Option<[u8; 32]>) -> bool {
        self.plane
            .borrow_mut()
            .workspaces
            .iter_mut()
            .find(|held| held.id == workspace)
            .is_some_and(|held| held.board.set_host(host))
    }

    pub fn heard_body(&self, workspace: &str, from: EndpointId, body: &[u8]) -> bool {
        self.heard_word(workspace, from, |board, now| {
            board.heard(*from.as_bytes(), body, now)
        })
    }

    /// Take one member's word about themselves into `workspace`'s board. Every surface that lists
    /// people draws what it changed: a pointer landing draws one frame, and one that says the same
    /// place again draws none.
    fn heard_word(
        &self,
        workspace: &str,
        from: EndpointId,
        take: impl FnOnce(&mut Board, f64) -> bool,
    ) -> bool {
        let now = now_ms();
        let mut plane = self.plane.borrow_mut();
        if from == plane.publisher {
            return false;
        }
        plane
            .workspaces
            .iter_mut()
            .find(|held| held.id == workspace)
            .is_some_and(|held| take(&mut held.board, now))
    }

    /// Take one thing the plane heard that the boards keep: who said they are here, what they said
    /// of themselves, who left.
    pub(super) fn heard(&self, event: Event) {
        self.plane.borrow_mut().apply(event);
    }

    /// The workspace on `topic`, by its id, and the person its board heard `peer` as there: what a
    /// workspace's gate holds a message from `peer` to.
    pub(crate) fn heard_member(
        &self,
        topic: Topic,
        peer: EndpointId,
    ) -> Option<(String, Option<String>)> {
        let plane = self.plane.borrow();
        let workspace = plane.workspaces.iter().find(|held| held.ws == topic)?;
        let user = workspace
            .board
            .member(*peer.as_bytes())
            .and_then(|known| known.user.clone());
        Some((workspace.id.clone(), user))
    }

    /// The workspace on `topic`, while this session takes part in it.
    #[must_use]
    pub fn workspace_id(&self, topic: Topic) -> Option<String> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.ws == topic)
            .map(|held| held.id.clone())
    }

    /// The workspaces this session takes part in, by their ids: what a host with a plane of its own
    /// reads them for.
    #[must_use]
    pub fn live_workspaces(&self) -> Vec<String> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .map(|held| held.id.clone())
            .collect()
    }

    /// Whether anyone but `own` is here in a workspace this session takes part in.
    #[must_use]
    pub fn anyone_else_here(&self, own: EndpointId) -> bool {
        self.plane.borrow().workspaces.iter().any(|held| {
            held.board
                .here()
                .any(|member| member.publisher != *own.as_bytes())
        })
    }

    /// Who is here in the workspace in front, as every surface that draws them reads them: the
    /// workspace and its members, each where they point and what they say.
    #[must_use]
    pub fn front_members(&self) -> Option<(String, Vec<Shown>)> {
        let now = now_ms();
        let plane = self.plane.borrow();
        let in_front = plane
            .active
            .and_then(|ws| plane.workspaces.iter().find(|held| held.ws == ws))?;
        Some((in_front.id.clone(), in_front.shown(now)))
    }

    /// The topic of the workspace `workspace`, while this session takes part in it.
    #[must_use]
    pub fn workspace_topic(&self, workspace: &str) -> Option<Topic> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| held.ws)
    }

    /// The devices heard in `workspace` beside this one, by publisher and the name each device goes
    /// by: what shows which device a window of the workspace runs on.
    #[must_use]
    pub fn workspace_devices(&self, workspace: &str) -> Vec<(EndpointId, String)> {
        self.devices_called(workspace, |member| {
            if member.device.trim().is_empty() {
                member.label().to_owned()
            } else {
                member.device.clone()
            }
        })
    }

    /// The devices heard in `workspace` beside this one, by publisher and what each calls itself,
    /// from its certificate: nothing for a device that gave itself no name, or whose certificate
    /// was not heard yet. Never the person's name, which names them and not one of their devices:
    /// what a sentence about the device itself says.
    #[must_use]
    pub fn workspace_device_names(&self, workspace: &str) -> Vec<(EndpointId, String)> {
        self.devices_called(workspace, |member| member.device.trim().to_owned())
    }

    /// The devices heard in `workspace` beside this one, by publisher, each with what `called`
    /// says of it.
    fn devices_called(
        &self,
        workspace: &str,
        called: impl Fn(&presence::Member) -> String,
    ) -> Vec<(EndpointId, String)> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .find(|held| held.id == workspace)
            .map(|held| {
                held.board
                    .members()
                    .iter()
                    .filter_map(|member| {
                        let publisher = EndpointId::from_bytes(&member.publisher).ok()?;
                        Some((publisher, called(member)))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Say what this device is, now, in the workspace in front: only what changed since its last
    /// words leaves, under the board's one schedule.
    fn say_own(&self) {
        let word = self.own_word();
        let now = now_ms();
        let Some((desk, frames)) = self.plane.borrow_mut().say_in_front(&word, now) else {
            return;
        };
        self.say_frames(desk, frames);
    }

    /// Say this device's whole word in `workspace`, on joining it: a device that came into a
    /// workspace says who it is at once, so the people already there read it from the first frame.
    fn say_into(&self, workspace: &str) {
        let word = self.own_word();
        let now = now_ms();
        let (desk, frames) = {
            let mut plane = self.plane.borrow_mut();
            let Some(held) = plane
                .workspaces
                .iter_mut()
                .find(|held| held.id == workspace)
            else {
                return;
            };
            held.board.say(&word);
            (DeskWorkspaceId::parse(&held.id), held.board.flush(now))
        };
        let Some(desk) = desk else {
            return;
        };
        self.say_frames(desk, frames);
    }

    /// Take what the workspace in front's board owes now: what changed since its last burst, the
    /// loss repairs, and the slow restatement that reaches someone who came in after the first
    /// word. Answers whether anything left, and whether the board forgot a member gone quiet on
    /// the way.
    pub(super) fn pump_presence(&self) -> (bool, bool) {
        let now = now_ms();
        let (due, forgot) = {
            let mut plane = self.plane.borrow_mut();
            let before = plane.front_revision();
            let due = plane.due_in_front(now);
            (due, plane.front_revision() != before)
        };
        let Some((desk, frames)) = due else {
            return (false, forgot);
        };
        self.say_frames(desk, frames);
        (true, forgot)
    }

    /// What this device says of itself, as its board carries it.
    fn own_word(&self) -> presence::Own {
        let wave = self.sent_wave.borrow().as_ref().map(|wave| presence::Wave {
            seq: wave.seq,
            target: wave.target.clone(),
        });
        let mut word = self.own.borrow().word(&self.device, wave);
        let says = self.says.borrow();
        word.pointer = says.pointer;
        word.shell.clone_from(&says.shell);
        word
    }

    /// Say `frames` on `ws`'s presence track, each as the object the board numbered: one object per
    /// frame, sealed with the workspace scope's keys. Nothing while this session says presence
    /// nowhere: presence is ephemeral, and the next change says where its sender is.
    fn say_frames(&self, ws: DeskWorkspaceId, frames: Vec<(u32, Frame)>) {
        self.said
            .borrow_mut()
            .extend(frames.iter().map(|(_, frame)| frame.clone()));
        let Some(saying) = &self.saying else {
            return;
        };
        for (sequence, frame) in frames {
            let said = saying.say(ws, sequence, &frame).err();
            let mut unsaid = self.unsaid.borrow_mut();
            if *unsaid == said {
                continue;
            }
            if let Some(error) = &said {
                tracing::warn!(%error, "presence is not said on the live plane");
            } else {
                tracing::info!("presence is said on the live plane again");
            }
            *unsaid = said;
        }
    }

    /// What this device said since the last call, as a session with no plane of its own would have
    /// published it.
    pub fn take_said(&self) -> Vec<Frame> {
        std::mem::take(&mut *self.said.borrow_mut())
    }

    /// Whether `publisher` is this person: this device, or any other device of the person this
    /// session runs for.
    fn is_own_person(&self, publisher: &str) -> bool {
        let Ok(bytes) = <[u8; 32]>::try_from(hex::decode(publisher).unwrap_or_default()) else {
            return false;
        };
        let plane = self.plane.borrow();
        if bytes == *plane.publisher.as_bytes() {
            return true;
        }
        let own_user = self
            .own
            .borrow()
            .user
            .clone()
            .filter(|user| !user.is_empty());
        let Some(own_user) = own_user else {
            return false;
        };
        plane.workspaces.iter().any(|held| {
            held.board
                .member(bytes)
                .and_then(|member| member.user.as_deref())
                .is_some_and(|user| user == own_user)
        })
    }

    /// Wave at `target`, a member here by their live publisher: said in the workspace in front,
    /// where the person it is for hears it once.
    pub fn wave(&self, target: &str) {
        if target.is_empty() || self.is_own_person(target) {
            return;
        }
        let seq = self.wave_seq.get().wrapping_add(1);
        self.wave_seq.set(seq);
        *self.sent_wave.borrow_mut() = Some(presence::Wave {
            seq,
            target: target.to_owned(),
        });
        self.say_own();
    }

    /// How many times what the workspace in front knows of its people changed: a surface that draws
    /// them draws again when this moved, and asks for no frame while it holds still.
    #[must_use]
    pub fn presence_revision(&self) -> u64 {
        self.plane.borrow().revision()
    }

    /// How many times what any of the person's workspaces knows of its people changed, in one
    /// number: who is here, which window they are in and the call they are in are read again when
    /// this moved.
    #[must_use]
    pub fn people_revision(&self) -> u64 {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .fold(0_u64, |seen, held| {
                seen.wrapping_mul(31).wrapping_add(held.board.revision())
            })
    }

    /// Everyone here in `workspace`, as a surface that draws them reads them: where each of them
    /// points, on the tiles the surface shows, and what they say beside their pointer.
    #[must_use]
    pub fn members(&self, workspace: &str) -> Vec<Shown> {
        self.plane.borrow().shown(workspace, now_ms())
    }

    /// How many times what `workspace` knows of its people changed: a surface that draws one
    /// workspace's people draws again when this moved, and asks for no frame while it holds still.
    #[must_use]
    pub fn here_revision(&self, workspace: &str) -> u64 {
        self.plane.borrow().revision_of(workspace)
    }

    /// This device's own word, as the roster draws it beside everyone else's here.
    #[must_use]
    pub fn own_here(&self) -> Shown {
        let publisher = *self.plane.borrow().publisher.as_bytes();
        presence::own_shown(&self.own_word(), publisher)
    }

    /// The calls in progress in every workspace this session takes part in, as those in them say:
    /// who to join, where.
    #[must_use]
    pub fn calls(&self) -> Vec<crate::LiveCall> {
        self.plane
            .borrow()
            .workspaces
            .iter()
            .flat_map(Workspace::calls)
            .collect()
    }

    /// This device's own word as the boards hold it, for a host that draws the roster.
    #[must_use]
    pub fn own_word_here(&self) -> presence::Own {
        self.own_word()
    }
}

// A host's own thread takes a presence: the boards, and the ends it says on, are `Send` where the
// platform has threads. A page has one thread, so there the ends are the browser's own handles.
#[cfg(not(target_family = "wasm"))]
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Presence>();
};

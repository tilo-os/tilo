//! This device's part in the remote uses the record of the person's devices holds, apart from the
//! session around it: what it asks its person, what it starts and stops, what it sends, and what it
//! takes from what the other device sent. The session hands each pass the record, the clock, who
//! is online, the Continuity switches and the app in front ([`World`]), and carries out what the
//! pass returns ([`Out`]): writes to the record, questions for the person, and messages for the
//! live link.

use std::collections::BTreeMap;
use std::time::Instant;

use os_actions::{
    ForwardedPeripheralBroker, ForwardedPeripheralIdentity, PeripheralLeaseId,
    VirtualGamepadFactory,
};
use tilo_resource::Invoke;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, RemoteKind, RemoteStage, RemoteUse,
};
use tilo_workspace_service::workspaces::person_workspace::remote_use::{
    self, Part, Profile, Step, Turn,
};

use super::wire::{Carried, Piece, What, captured, pieces};

/// The app this device has in front: what a camera, microphone or scanner used here is for, and
/// the one app session a controller sent here reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Front {
    pub(crate) app: String,
    pub(crate) workspace: String,
    /// The window it shows in, as the layout names it.
    pub(crate) session: String,
    /// The app's name, as the person reads it.
    pub(crate) name: String,
}

/// What a pass reads of the session.
pub(crate) struct World<'a> {
    pub(crate) record: &'a Devices,
    pub(crate) now_ms: i64,
    /// Whether the device by this key is online.
    pub(crate) online: &'a dyn Fn(&str) -> bool,
    /// Whether this device's person lets this kind cross between their devices (Settings,
    /// Continuity: realtime sessions, game controllers).
    pub(crate) allowed: &'a dyn Fn(RemoteKind) -> bool,
    pub(crate) front: Option<Front>,
}

/// A question for this device's person: whether the use `turn` names goes ahead. A controller's
/// is asked for the app session in front then, which its yes binds to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Question {
    pub(crate) id: String,
    pub(crate) kept: RemoteUse,
    pub(crate) front: Option<Front>,
}

/// What a pass leaves the session to carry out.
#[derive(Default)]
pub(crate) struct Out {
    pub(crate) writes: Vec<Invoke<Devices>>,
    pub(crate) questions: Vec<Question>,
    /// Messages for the device by this key.
    pub(crate) sent: Vec<(String, Vec<u8>)>,
    /// What went wrong at an edge, for the person.
    pub(crate) failed: Vec<(RemoteUse, String)>,
}

/// One unit of what a camera, microphone or scanner captured.
pub(crate) enum Unit {
    /// A frame, or a stretch of sound.
    Frame(Vec<u8>),
    /// One scanned page; `last` ends the document.
    Page { bytes: Vec<u8>, last: bool },
}

/// A camera, microphone or scanner capturing for a remote use.
pub(crate) trait Capture: Send {
    /// What it captured since it was last asked, at most what one pass sends: a live source
    /// drops what the link did not take in time rather than fall behind.
    fn take(&mut self) -> Vec<Unit>;
}

/// The virtual camera, microphone or scanner inbox in place where a remote use is used.
pub(crate) trait VirtualDevice: Send {
    /// One whole frame, or stretch of sound.
    fn frame(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// One whole scanned document, page by page.
    fn document(&mut self, pages: Vec<Vec<u8>>) -> Result<(), String>;
}

/// A game controller plugged into this device, sending.
pub(crate) trait PadSource: Send {
    fn take(&mut self) -> Vec<os_actions::ForwardedGamepadEvent>;
}

/// What stands at the edges: the hardware here and the virtual devices in place for another's.
pub(crate) trait Edge {
    fn capture(&self, profile: &Profile) -> Result<Box<dyn Capture>, String>;
    fn virtual_device(
        &self,
        label: &str,
        profile: &Profile,
    ) -> Result<Box<dyn VirtualDevice>, String>;
    /// The controller called `label` plugged in here.
    fn pad(&self, label: &str) -> Result<Box<dyn PadSource>, String>;
}

/// What this device sends for one use.
struct Sending {
    to: String,
    sequence: u64,
    source: Source,
    /// The next page of a scanned document.
    page: u32,
}

enum Source {
    Capture(Box<dyn Capture>),
    Pad(Box<dyn PadSource>),
}

/// What this device takes for one use.
struct Receiving {
    from: String,
    last: u64,
    sink: Sink,
}

enum Sink {
    Captured {
        profile: Profile,
        device: Box<dyn VirtualDevice>,
        /// The unit being put together, and the pages of the document so far.
        unit: Vec<u8>,
        pages: Vec<Vec<u8>>,
        document_bytes: u64,
    },
    Pad {
        identity: ForwardedPeripheralIdentity,
        device: os_actions::PeripheralDeviceId,
        lease: PeripheralLeaseId,
    },
}

/// This device's part in the remote uses.
pub(crate) struct Engine {
    local: String,
    edge: Box<dyn Edge>,
    part: Part,
    sending: BTreeMap<String, Sending>,
    receiving: BTreeMap<String, Receiving>,
    /// The virtual pads controllers sent here drive, each bound to its app session.
    pads: ForwardedPeripheralBroker<Box<dyn VirtualGamepadFactory>>,
    /// The uses this device wrote an end of, so it writes each once.
    ended: std::collections::BTreeSet<String>,
}

impl Engine {
    pub(crate) fn new(
        local: String,
        edge: Box<dyn Edge>,
        pads: Box<dyn VirtualGamepadFactory>,
    ) -> Self {
        Self {
            local,
            edge,
            part: Part::default(),
            sending: BTreeMap::new(),
            receiving: BTreeMap::new(),
            pads: ForwardedPeripheralBroker::new(pads),
            ended: std::collections::BTreeSet::new(),
        }
    }

    /// One pass: take in what the other devices sent (`heard`, each with its sender), read the
    /// record, and do this device's part.
    pub(crate) fn pass(&mut self, world: &World<'_>, heard: Vec<(String, Vec<u8>)>) -> Out {
        let mut out = Out::default();
        for (from, body) in heard {
            self.take(world, &from, &body, &mut out);
        }
        self.hold(world, &mut out);
        let online = |key: &str| (world.online)(key);
        let (writes, work) = remote_use::turns(
            world.record,
            &self.local,
            online,
            world.now_ms,
            &mut self.part,
        );
        out.writes.extend(writes);
        for turn in work {
            self.begin(world, turn, &mut out);
        }
        self.send(world, &mut out);
        out
    }

    /// The person answered `question`: the write that says so. A controller's yes holds only for
    /// the app session it was asked for, still in front now; a kind their switches turned off
    /// meanwhile is no yes either.
    pub(crate) fn answered(
        &self,
        world: &World<'_>,
        question: &Question,
        yes: bool,
    ) -> Invoke<Devices> {
        let kept = &question.kept;
        let still = world
            .record
            .uses
            .get(&tilo_resource::Id::from_raw(question.id.clone()))
            .is_some_and(|now| now.stage == RemoteStage::Asked && now.decider() == self.local);
        let allowed = (world.allowed)(kept.kind);
        if kept.kind == RemoteKind::Controller {
            let front = question
                .front
                .as_ref()
                .filter(|asked| world.front.as_ref() == Some(*asked));
            match front {
                Some(front) if yes && still && allowed => remote_use::answering(
                    &question.id,
                    true,
                    (&front.app, &front.workspace, &front.session),
                    world.now_ms,
                ),
                _ => remote_use::answering(&question.id, false, ("", "", ""), world.now_ms),
            }
        } else {
            remote_use::answering(
                &question.id,
                yes && still && allowed,
                ("", "", ""),
                world.now_ms,
            )
        }
    }

    /// End every granted use that should not go on: a kind this device's person turned off, and a
    /// use here whose app is no longer in front. An ask of a kind turned off is declined instead.
    fn hold(&mut self, world: &World<'_>, out: &mut Out) {
        for (id, kept) in world.record.uses.iter() {
            let id = id.as_str();
            if !kept.under_way(world.now_ms) || (kept.from != self.local && kept.to != self.local) {
                continue;
            }
            let granted = kept.stage == RemoteStage::Granted;
            let switched_off = granted && !(world.allowed)(kept.kind);
            let focus_lost =
                granted && kept.to == self.local && !in_front(kept, world.front.as_ref());
            if (switched_off || focus_lost) && self.ended.insert(id.to_owned()) {
                out.writes.push(remote_use::ending(id, world.now_ms));
                self.stop(id);
            }
        }
        // A pad reaches only the app session it was granted to, while it is in front.
        let focus = world.front.as_ref().and_then(|front| {
            self.receiving
                .values()
                .find_map(|receiving| match &receiving.sink {
                    Sink::Pad { identity, .. }
                        if identity.app_id() == front.app
                            && identity.workspace_id() == front.workspace
                            && identity.session_id() == front.session =>
                    {
                        Some(identity.clone())
                    }
                    _ => None,
                })
        });
        self.pads.set_focus(focus);
        self.pads.reap_expired(Instant::now());
    }

    /// Begin this device's part of `turn`.
    fn begin(&mut self, world: &World<'_>, turn: Turn, out: &mut Out) {
        let Turn { id, kept, step } = turn;
        match step {
            Step::Decide => {
                // A kind the person turned off here is declined without asking them, and so is a
                // controller with no app in front to reach.
                let front = world.front.clone();
                if !(world.allowed)(kept.kind)
                    || (kept.kind == RemoteKind::Controller && front.is_none())
                {
                    out.writes.push(remote_use::answering(
                        &id,
                        false,
                        ("", "", ""),
                        world.now_ms,
                    ));
                    return;
                }
                out.questions.push(Question { id, kept, front });
            }
            Step::Send => {
                let source = if captured(kept.kind) {
                    Profile::of(&kept)
                        .ok_or_else(|| "the record holds no profile this device sends".to_owned())
                        .and_then(|profile| self.edge.capture(&profile))
                        .map(Source::Capture)
                } else {
                    self.edge.pad(&kept.label).map(Source::Pad)
                };
                match source {
                    Ok(source) => {
                        self.part.running(&id);
                        self.sending.insert(
                            id,
                            Sending {
                                to: kept.to.clone(),
                                sequence: 0,
                                source,
                                page: 0,
                            },
                        );
                    }
                    Err(why) => self.fail(world, &id, kept, why, out),
                }
            }
            Step::Receive => match self.sink(&kept) {
                Ok(sink) => {
                    self.part.running(&id);
                    self.receiving.insert(
                        id,
                        Receiving {
                            from: kept.from.clone(),
                            last: 0,
                            sink,
                        },
                    );
                }
                Err(why) => self.fail(world, &id, kept, why, out),
            },
            Step::Stop => self.stop(&id),
        }
    }

    /// What takes its place here for `kept`: a virtual camera, microphone or scanner inbox, or a
    /// virtual pad bound to the app session the person let it reach.
    fn sink(&mut self, kept: &RemoteUse) -> Result<Sink, String> {
        if captured(kept.kind) {
            let profile =
                Profile::of(kept).ok_or_else(|| "the record holds no profile here".to_owned())?;
            let device = self.edge.virtual_device(&kept.label, &profile)?;
            return Ok(Sink::Captured {
                profile,
                device,
                unit: Vec::new(),
                pages: Vec::new(),
                document_bytes: 0,
            });
        }
        // The binding is the record's, which this device wrote as its person said yes with
        // that app session in front; the sender names none of it.
        let identity = ForwardedPeripheralIdentity::new_after_verification(
            kept.from.clone(),
            kept.to.clone(),
            kept.app.clone(),
            kept.workspace.clone(),
            kept.session.clone(),
        )
        .map_err(|error| error.to_string())?;
        self.pads.set_focus(Some(identity.clone()));
        let device = self
            .pads
            .register_gamepad(kept.label.clone())
            .map_err(|error| error.to_string())?;
        let now = Instant::now();
        let lease = self
            .pads
            .authorize(&identity, device, now)
            .and_then(|proof| self.pads.acquire(proof, &identity, device, now));
        match lease {
            Ok(lease) => Ok(Sink::Pad {
                identity,
                device,
                lease,
            }),
            Err(error) => {
                self.pads.remove_device(device);
                Err(error.to_string())
            }
        }
    }

    /// An edge could not start its part of the use `id`: it ends, and the person hears why.
    fn fail(&mut self, world: &World<'_>, id: &str, kept: RemoteUse, why: String, out: &mut Out) {
        tracing::warn!(%why, kind = ?kept.kind, "a remote use could not start here");
        if self.ended.insert(id.to_owned()) {
            out.writes.push(remote_use::ending(id, world.now_ms));
        }
        out.failed.push((kept, why));
    }

    /// Stop what the use `id` began here.
    fn stop(&mut self, id: &str) {
        self.sending.remove(id);
        if let Some(Receiving {
            sink: Sink::Pad { device, lease, .. },
            ..
        }) = self.receiving.remove(id)
        {
            self.pads.revoke(lease);
            self.pads.remove_device(device);
        }
        self.part.stopped(id);
    }

    /// Send what each use this device sends captured since the last pass.
    fn send(&mut self, world: &World<'_>, out: &mut Out) {
        for (id, sending) in &mut self.sending {
            let granted = world
                .record
                .uses
                .get(&tilo_resource::Id::from_raw(id.clone()))
                .is_some_and(|kept| {
                    kept.stage == RemoteStage::Granted && world.now_ms < kept.until_ms
                });
            if !granted {
                continue;
            }
            let Ok(bytes) = hex::decode(id) else {
                continue;
            };
            let Ok(id) = <[u8; 16]>::try_from(bytes.as_slice()) else {
                continue;
            };
            let mut whats = Vec::new();
            match &mut sending.source {
                Source::Capture(capture) => {
                    for unit in capture.take() {
                        match unit {
                            Unit::Frame(bytes) => {
                                whats.extend(
                                    pieces(&bytes, None, false).into_iter().map(What::Piece),
                                );
                            }
                            Unit::Page { bytes, last } => {
                                let page = sending.page;
                                sending.page = if last { 0 } else { page.saturating_add(1) };
                                whats.extend(
                                    pieces(&bytes, Some(page), last)
                                        .into_iter()
                                        .map(What::Piece),
                                );
                            }
                        }
                    }
                }
                Source::Pad(pad) => whats.extend(pad.take().into_iter().map(What::Pad)),
            }
            for what in whats {
                sending.sequence = sending.sequence.saturating_add(1);
                let carried = Carried {
                    id,
                    sequence: sending.sequence,
                    what,
                };
                if let Some(body) = carried.encode() {
                    out.sent.push((sending.to.clone(), body));
                }
            }
        }
    }

    /// Take one message `from` sent: only for a use this device takes from that very device,
    /// granted and not run out, newer than the last, within what its profile carries.
    fn take(&mut self, world: &World<'_>, from: &str, body: &[u8], out: &mut Out) {
        let Some(carried) = Carried::decode(body) else {
            return;
        };
        let id = hex::encode(carried.id);
        let Some(kept) = world
            .record
            .uses
            .get(&tilo_resource::Id::from_raw(id.clone()))
            .filter(|kept| {
                kept.stage == RemoteStage::Granted
                    && world.now_ms < kept.until_ms
                    && kept.to == self.local
                    && kept.from == from
                    && (world.allowed)(kept.kind)
            })
        else {
            return;
        };
        let Some(receiving) = self.receiving.get_mut(&id) else {
            return;
        };
        if receiving.from != from || carried.sequence <= receiving.last {
            return;
        }
        receiving.last = carried.sequence;
        let refused = match (&mut receiving.sink, carried.what) {
            (sink @ Sink::Captured { .. }, What::Piece(piece)) => put(sink, &piece).err(),
            (
                Sink::Pad {
                    identity, lease, ..
                },
                What::Pad(event),
            ) => self
                .pads
                .submit(*lease, identity, carried.sequence, event, Instant::now())
                .err()
                .and_then(|error| {
                    // A pad's input past its rate, out of order or with its app not in front is
                    // dropped; only a lease that is gone ends the use.
                    matches!(error, os_actions::PeripheralBrokerError::InvalidLease)
                        .then(|| error.to_string())
                }),
            _ => Some("a message of another kind than the use".to_owned()),
        };
        if let Some(why) = refused {
            tracing::debug!(%why, "a remote use's message was refused here");
            if why != DROPPED {
                self.fail(world, &id, kept.clone(), why, out);
                self.stop(&id);
            }
        }
    }
}

/// A piece that does not fit is dropped, and the use goes on.
const DROPPED: &str = "dropped";

/// Put `piece` into `sink`: a whole frame goes to the virtual device, a whole document to the
/// scanner inbox. A piece out of bounds, or out of order, is dropped with what was put together
/// of its unit.
fn put(sink: &mut Sink, piece: &Piece) -> Result<(), String> {
    let Sink::Captured {
        profile,
        device,
        unit,
        pages,
        document_bytes,
    } = sink
    else {
        return Err(DROPPED.to_owned());
    };
    if !piece.fits(profile) {
        unit.clear();
        pages.clear();
        *document_bytes = 0;
        return Err(DROPPED.to_owned());
    }
    if piece.offset == 0 && piece.page == Some(0) {
        // A first page begins a new document.
        pages.clear();
        *document_bytes = 0;
    }
    if piece.offset == 0 {
        unit.clear();
    } else if u64::try_from(unit.len()).ok() != Some(piece.offset) {
        unit.clear();
        return Err(DROPPED.to_owned());
    }
    unit.extend_from_slice(&piece.payload);
    if u64::try_from(unit.len()).ok() != Some(piece.total) {
        return Ok(());
    }
    let whole = std::mem::take(unit);
    let Profile::Scanner {
        max_pages,
        max_document_bytes,
        ..
    } = profile
    else {
        return device.frame(&whole);
    };
    let page = piece.page.unwrap_or(u32::MAX);
    let bytes = document_bytes.saturating_add(u64::try_from(whole.len()).unwrap_or(u64::MAX));
    if u32::try_from(pages.len()).ok() != Some(page)
        || page >= *max_pages
        || bytes > *max_document_bytes
    {
        // A document past its pages or bytes, or with a page missing, is not delivered.
        pages.clear();
        *document_bytes = 0;
        return Err(DROPPED.to_owned());
    }
    *document_bytes = bytes;
    pages.push(whole);
    if piece.last {
        *document_bytes = 0;
        return device.document(std::mem::take(pages));
    }
    Ok(())
}

/// Whether the app `kept` is used by is in front here: the app and workspace for a camera,
/// microphone or scanner, and also the very session for a controller.
fn in_front(kept: &RemoteUse, front: Option<&Front>) -> bool {
    front.is_some_and(|front| {
        front.app == kept.app
            && front.workspace == kept.workspace
            && (kept.kind != RemoteKind::Controller || front.session == kept.session)
    })
}

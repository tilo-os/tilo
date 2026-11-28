//! One of the person's devices' camera, microphone, document scanner or game controller, used on
//! another of theirs for a while, through the record of their devices ([`RemoteUse`]).
//!
//! A camera, microphone or scanner is asked for by the device it is used on, for the app it has in
//! front there, which is the app the person lets use it; the device that has it asks its own
//! person, and on their yes sends what it captures over the live link between the two
//! (`tilo-session`'s own-device link), where a virtual camera, microphone or scanner inbox stands
//! in for it. A game controller is offered the other way: the device it is plugged into asks the
//! device it would play on, whose person decides with the exact app they have in front, and the
//! controller's input reaches only that app's session there, through a virtual pad.
//!
//! A grant lasts [`GRANT_MS`]: a camera, microphone or scanner keeps it while it sends, renewing it
//! as it goes ([`renewal_due`]), so a device that stops sending or goes away lets it run out; a
//! controller's lease is not renewed. Either device ends a use, and each device ends one whose
//! other device left or whose ask nobody answered in [`ASK_WAIT_MS`] ([`turns`]). The person's
//! Continuity switches gate each kind on each device: realtime sessions for a camera, microphone
//! or scanner, game controllers for a controller.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_resource::{Collection, Id, Invoke, Refusal};

use super::devices::{Devices, RemoteAsk, RemoteKind, RemoteStage, RemoteUse};

/// How long a grant lasts: five minutes from the yes, or from the last renewal.
pub const GRANT_MS: i64 = 5 * 60 * 1_000;
/// How long the deciding device's person has to answer before the ask is given up.
pub const ASK_WAIT_MS: i64 = 30_000;
/// The most cameras, microphones and scanners one device uses from the others at once.
pub const MAX_CAPTURES: usize = 8;
/// The most game controllers sent to one device at once.
pub const MAX_CONTROLLERS: usize = 8;
/// The most bytes one piece of what is captured carries over the link.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// The largest scanned document.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// The most pages of one scanned document.
pub const MAX_PAGES: u32 = 1_024;
/// The longest app id, label and workspace a use names.
pub const MAX_APP_BYTES: usize = 256;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_WORKSPACE_BYTES: usize = 512;
/// How many ended uses the record keeps for their story, beside the ones under way.
const ENDED_KEPT: usize = 16;

/// The pixels a remote camera sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoFormat {
    Nv12,
    Rgba8,
    Mjpeg,
}

/// The samples a remote microphone sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioFormat {
    PcmS16Le,
    PcmF32Le,
}

/// The pages a remote scanner sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DocumentFormat {
    Jpeg,
    Png,
    Pdf,
}

/// What one remote use carries: semantic formats only, never a device node, descriptor or
/// serial.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Profile {
    Camera {
        format: VideoFormat,
        width: u16,
        height: u16,
        frames_per_second: u8,
    },
    Microphone {
        format: AudioFormat,
        sample_rate: u32,
        channels: u8,
        frame_millis: u8,
    },
    Scanner {
        format: DocumentFormat,
        max_pages: u32,
        max_document_bytes: u64,
    },
    /// A fixed-shape game pad: six axes, eleven buttons and a hat.
    Controller,
}

impl Profile {
    #[must_use]
    pub const fn kind(&self) -> RemoteKind {
        match self {
            Self::Camera { .. } => RemoteKind::Camera,
            Self::Microphone { .. } => RemoteKind::Microphone,
            Self::Scanner { .. } => RemoteKind::Scanner,
            Self::Controller => RemoteKind::Controller,
        }
    }

    /// Whether it is within what a remote use carries.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Camera {
                width,
                height,
                frames_per_second,
                ..
            } => {
                (160..=4096).contains(width)
                    && (120..=2160).contains(height)
                    && (1..=60).contains(frames_per_second)
            }
            Self::Microphone {
                sample_rate,
                channels,
                frame_millis,
                ..
            } => {
                matches!(*sample_rate, 16_000 | 24_000 | 44_100 | 48_000)
                    && (1..=2).contains(channels)
                    && (5..=40).contains(frame_millis)
            }
            Self::Scanner {
                max_pages,
                max_document_bytes,
                ..
            } => {
                (1..=MAX_PAGES).contains(max_pages)
                    && (1..=MAX_DOCUMENT_BYTES).contains(max_document_bytes)
            }
            Self::Controller => true,
        }
    }

    /// The profile `kept` holds, when it reads and is valid.
    #[must_use]
    pub fn of(kept: &RemoteUse) -> Option<Self> {
        tilo_failure::ignored!(
            "a profile the record cannot read carries nothing: the use it names is refused",
            serde_json::from_str::<Self>(&kept.profile).ok()
        )
        .filter(|profile| profile.valid() && profile.kind() == kept.kind)
    }

    /// As the record keeps it.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The profile a camera, microphone or scanner is asked with when nothing else is said.
    #[must_use]
    pub const fn usual(kind: RemoteKind) -> Self {
        match kind {
            RemoteKind::Camera => Self::Camera {
                format: VideoFormat::Nv12,
                width: 1280,
                height: 720,
                frames_per_second: 30,
            },
            RemoteKind::Microphone => Self::Microphone {
                format: AudioFormat::PcmS16Le,
                sample_rate: 48_000,
                channels: 1,
                frame_millis: 20,
            },
            RemoteKind::Scanner => Self::Scanner {
                format: DocumentFormat::Png,
                max_pages: MAX_PAGES,
                max_document_bytes: MAX_DOCUMENT_BYTES,
            },
            RemoteKind::Controller => Self::Controller,
        }
    }
}

impl RemoteStage {
    /// Whether a use at this stage ended: declined, stopped or given up.
    #[must_use]
    pub const fn ended(self) -> bool {
        !matches!(self, Self::Asked | Self::Granted)
    }
}

impl RemoteUse {
    /// Whether the use is under way at `now_ms`: asked and not yet late, or granted and not run
    /// out.
    #[must_use]
    pub const fn under_way(&self, now_ms: i64) -> bool {
        match self.stage {
            RemoteStage::Asked => now_ms < self.asked_ms.saturating_add(ASK_WAIT_MS),
            RemoteStage::Granted => now_ms < self.until_ms,
            _ => false,
        }
    }

    /// The device whose person decides: the one with the camera, microphone or scanner, or the
    /// one a controller is sent to.
    #[must_use]
    pub fn decider(&self) -> &str {
        if self.kind == RemoteKind::Controller {
            &self.to
        } else {
            &self.from
        }
    }

    /// The device that asked: the other one.
    #[must_use]
    pub fn asker(&self) -> &str {
        if self.kind == RemoteKind::Controller {
            &self.from
        } else {
            &self.to
        }
    }
}

/// The use `id` asked of `from` for `to`, for `app` in `workspace`, called `label`, carrying
/// `profile`, at `asked_ms`.
///
/// # Errors
/// Either device is not one of the person's in `record`, or they are the same; the id, names or
/// profile are out of bounds; or `to` uses as many of that kind as it takes.
pub(super) fn asked(
    record: &Devices,
    id: &str,
    (from, to): (String, String),
    (app, workspace, label): (String, String, String),
    profile: &str,
    asked_ms: i64,
) -> Result<RemoteUse, Refusal> {
    let drawn = hex::decode(id).is_ok_and(|bytes| {
        bytes.len() == 16 && hex::encode(&bytes) == id && bytes.iter().any(|b| *b != 0)
    });
    if !drawn {
        return Err(Refusal::new("a remote use is named by 16 bytes in hex"));
    }
    if from == to
        || !record.devices.contains(&Id::from_raw(from.clone()))
        || !record.devices.contains(&Id::from_raw(to.clone()))
    {
        return Err(Refusal::new(
            "a device's camera, microphone, scanner or controller is used on another of your devices",
        ));
    }
    let profile: Profile = serde_json::from_str(profile)
        .ok()
        .filter(Profile::valid)
        .ok_or_else(|| Refusal::new("that is not something a remote use carries"))?;
    // A camera, microphone or scanner is asked for the app in front where it is used; a
    // controller is offered to whichever app session the person there has in front as they say
    // yes, so it names its app only then.
    let controller = profile.kind() == RemoteKind::Controller;
    let app_named = if controller {
        app.is_empty() && workspace.is_empty()
    } else {
        named(&app, MAX_APP_BYTES) && named(&workspace, MAX_WORKSPACE_BYTES)
    };
    if !app_named || !named(&label, MAX_LABEL_BYTES) {
        return Err(Refusal::new(
            "a remote use names its app, workspace and label",
        ));
    }
    if asked_ms <= 0 {
        return Err(Refusal::new(
            "a remote use is asked at a time since the epoch",
        ));
    }
    let kind = profile.kind();
    let most = if controller {
        MAX_CONTROLLERS
    } else {
        MAX_CAPTURES
    };
    let busy = record
        .uses
        .values()
        .filter(|kept| {
            kept.to == to
                && (kept.kind == RemoteKind::Controller) == controller
                && kept.under_way(asked_ms)
        })
        .count();
    if busy >= most {
        return Err(Refusal::new(if controller {
            "that device plays with as many controllers from your devices as it takes"
        } else {
            "this device uses as many cameras, microphones and scanners of your devices as it takes"
        }));
    }
    Ok(RemoteUse {
        from,
        to,
        kind,
        app,
        workspace,
        session: String::new(),
        label,
        profile: profile.encode(),
        asked_ms,
        stage: RemoteStage::Asked,
        at_ms: asked_ms,
        until_ms: 0,
    })
}

/// Let go of the ended uses past the last [`ENDED_KEPT`], as of `now_ms`.
pub(super) fn forget_ended(uses: &mut Collection<RemoteUse>, now_ms: i64) {
    let mut ended: Vec<(i64, Id)> = uses
        .iter()
        .filter(|(_, kept)| !kept.under_way(now_ms))
        .map(|(id, kept)| (kept.at_ms, id.clone()))
        .collect();
    if ended.len() <= ENDED_KEPT {
        return;
    }
    ended.sort();
    let drop: BTreeSet<Id> = ended
        .into_iter()
        .rev()
        .skip(ENDED_KEPT)
        .map(|(_, id)| id)
        .collect();
    uses.retain(|id, _| !drop.contains(id));
}

/// Whether `value` names something in at most `most` bytes, with no control characters.
fn named(value: &str, most: usize) -> bool {
    !value.is_empty() && value.len() <= most && !value.chars().any(char::is_control)
}

/// `kept` answered at `at_ms`: granted for [`GRANT_MS`], reaching the app session `front` (its
/// app, workspace and session) for a controller, or declined. A use no longer asked stays as it
/// is.
pub(super) fn answer(
    kept: &mut RemoteUse,
    granted: bool,
    (app, workspace, session): (String, String, String),
    at_ms: i64,
) -> Result<(), Refusal> {
    if kept.stage != RemoteStage::Asked {
        return Ok(());
    }
    let controller = kept.kind == RemoteKind::Controller;
    if granted
        && controller
        && !(named(&app, MAX_APP_BYTES)
            && named(&workspace, MAX_WORKSPACE_BYTES)
            && named(&session, MAX_APP_BYTES))
    {
        return Err(Refusal::new(
            "a controller reaches the app session in front",
        ));
    }
    kept.at_ms = at_ms;
    if granted {
        kept.stage = RemoteStage::Granted;
        kept.until_ms = at_ms.saturating_add(GRANT_MS);
        if controller {
            (kept.app, kept.workspace, kept.session) = (app, workspace, session);
        }
    } else {
        kept.stage = RemoteStage::Declined;
    }
    Ok(())
}

/// `kept` renewed at `at_ms`: a granted camera, microphone or scanner that has not run out lasts
/// another [`GRANT_MS`].
pub(super) fn renew(kept: &mut RemoteUse, at_ms: i64) {
    if kept.stage == RemoteStage::Granted
        && kept.kind != RemoteKind::Controller
        && at_ms < kept.until_ms
    {
        kept.until_ms = kept.until_ms.max(at_ms.saturating_add(GRANT_MS));
    }
}

/// The ask the device `local` makes at `now_ms`, under the fresh id `id`: of `other`'s
/// camera, microphone or scanner for the app in front here (`app`, its workspace and its name), or
/// to send its own controller to `other` (named by the controller's name, with no app).
///
/// # Errors
/// Why the record refuses it.
pub fn asking(
    record: &Devices,
    id: [u8; 16],
    local: &str,
    other: &str,
    profile: &Profile,
    (app, workspace, label): (&str, &str, &str),
    now_ms: i64,
) -> Result<Invoke<Devices>, Refusal> {
    let (from, to) = if profile.kind() == RemoteKind::Controller {
        (local.to_owned(), other.to_owned())
    } else {
        (other.to_owned(), local.to_owned())
    };
    let id = hex::encode(id);
    let encoded = profile.encode();
    asked(
        record,
        &id,
        (from.clone(), to.clone()),
        (app.to_owned(), workspace.to_owned(), label.to_owned()),
        &encoded,
        now_ms,
    )?;
    Ok(Devices::ASK_REMOTE_USE.with(RemoteAsk {
        id,
        from,
        to,
        app: app.to_owned(),
        workspace: workspace.to_owned(),
        label: label.to_owned(),
        profile: encoded,
        asked_ms: now_ms,
    }))
}

/// The answer to the use `id`: granted, reaching the app session `front` (app, workspace,
/// session) for a controller, or not.
#[must_use]
pub fn answering(
    id: &str,
    granted: bool,
    front: (&str, &str, &str),
    now_ms: i64,
) -> Invoke<Devices> {
    let (app, workspace, session) = front;
    Devices::ANSWER_REMOTE_USE.with((
        id.to_owned(),
        granted,
        app.to_owned(),
        workspace.to_owned(),
        session.to_owned(),
        now_ms,
    ))
}

/// The end of the use `id` at `now_ms`, as either device stops it.
#[must_use]
pub fn ending(id: &str, now_ms: i64) -> Invoke<Devices> {
    Devices::END_REMOTE_USE.with((id.to_owned(), true, now_ms))
}

/// What one device does for one remote use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// On the deciding device: ask its person.
    Decide,
    /// On the device that has it: start sending.
    Send,
    /// On the device it is used on: stand the virtual device in.
    Receive,
    /// On either: stop what it began.
    Stop,
}

/// One piece of work a device does for the use `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub id: String,
    pub kept: RemoteUse,
    pub step: Step,
}

/// What one device took up of the uses in the record, so it does each piece of work once.
#[derive(Debug, Default)]
pub struct Part {
    /// The work it began and the uses it wrote an end of, by id and step.
    begun: BTreeSet<(String, Option<Step>)>,
    /// The uses it sends or receives now.
    running: BTreeSet<String>,
    /// The grant each use it sends was last renewed up to.
    renewed: BTreeMap<String, i64>,
}

impl Part {
    /// The use `id` is sent or received here now.
    pub fn running(&mut self, id: &str) {
        self.running.insert(id.to_owned());
    }

    /// What the use `id` began here stopped.
    pub fn stopped(&mut self, id: &str) {
        self.running.remove(id);
        self.renewed.remove(id);
    }

    /// Whether the use `id` is sent or received here now.
    #[must_use]
    pub fn is_running(&self, id: &str) -> bool {
        self.running.contains(id)
    }
}

/// Whether the device sending `kept` renews it at `now_ms`: past half its grant.
#[must_use]
pub fn renewal_due(kept: &RemoteUse, now_ms: i64) -> bool {
    kept.stage == RemoteStage::Granted
        && kept.kind != RemoteKind::Controller
        && now_ms < kept.until_ms
        && kept.until_ms.saturating_sub(now_ms) < GRANT_MS / 2
}

/// What the device `own` does for the uses `record` holds at `now_ms`: the writes it makes (the
/// uses it gives up, because nobody answered in time or the other device left, which `online`
/// answers by key; the ones whose grant ran out; and the renewals of the ones it sends) and the
/// work it begins.
pub fn turns(
    record: &Devices,
    own: &str,
    online: impl Fn(&str) -> bool,
    now_ms: i64,
    part: &mut Part,
) -> (Vec<Invoke<Devices>>, Vec<Turn>) {
    let mut writes = Vec::new();
    let mut work = Vec::new();
    for (id, kept) in record.uses.iter() {
        let id = id.as_str();
        let sends = kept.from == own;
        if !sends && kept.to != own {
            continue;
        }
        let other = if sends {
            kept.to.as_str()
        } else {
            kept.from.as_str()
        };
        if !kept.stage.ended() {
            let late = kept.stage == RemoteStage::Asked
                && now_ms >= kept.asked_ms.saturating_add(ASK_WAIT_MS);
            let ran_out = kept.stage == RemoteStage::Granted && now_ms >= kept.until_ms;
            let gone = !online(other);
            if late || ran_out || gone {
                if part.begun.insert((id.to_owned(), None)) {
                    writes.push(Devices::END_REMOTE_USE.with((id.to_owned(), ran_out, now_ms)));
                }
                // What it began is undone on the pass that reads the end.
                continue;
            }
            if sends
                && part.running.contains(id)
                && renewal_due(kept, now_ms)
                && part.renewed.get(id) != Some(&kept.until_ms)
            {
                part.renewed.insert(id.to_owned(), kept.until_ms);
                writes.push(Devices::RENEW_REMOTE_USE.with((id.to_owned(), now_ms)));
            }
        }
        let step = match kept.stage {
            RemoteStage::Asked if kept.decider() == own => Some(Step::Decide),
            RemoteStage::Granted if sends => Some(Step::Send),
            RemoteStage::Granted => Some(Step::Receive),
            stage if stage.ended() && part.running.contains(id) => Some(Step::Stop),
            _ => None,
        };
        if let Some(step) = step
            && part.begun.insert((id.to_owned(), Some(step)))
        {
            work.push(Turn {
                id: id.to_owned(),
                kept: kept.clone(),
                step,
            });
        }
    }
    (writes, work)
}

#[cfg(test)]
#[path = "remote_use_tests.rs"]
mod tests;

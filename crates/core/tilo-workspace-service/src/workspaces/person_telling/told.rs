//! The log of what a person was told, kept where their workspace runs.
//!
//! Everything a person's assistant tells them outside an answer to their
//! own words joins one log in the person's own store on this host: what it
//! is, its words, its mark with the reason, whether it is to reach them
//! now, and its buttons as typed actions. A device of theirs that runs
//! reads it live; whatever tells a person outside Tilo reads the same log
//! later. It is bounded: the newest are kept.
//!
//! A thing has an id that stays, so telling the same thing twice tells it
//! once: whoever feeds the log may hand a thing over again after a
//! restart. Whoever tells never says what kind of thing it is nor how
//! important: the kind comes from what caused it, and the mark from the
//! person's own rule ([`super::judge`]).

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_actions::remote::person_telling::{
    Acknowledged, BY_OPENED, CAUSE_EVENT, Importance, Insists, Kind, ListAfter, MAX_ACTIONS,
    MAX_LIST_WAIT_MS, MAX_TITLE_BYTES, MAX_WORDS_BYTES, Pushed, Reach, Stretch, Tell, Teller, Told,
    ToldList,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::super::Workspaces;
use super::super::model_answers::Spender;
use super::judge::{self, Matter};
use super::{clipped, read_rule};

/// How many things a person's log keeps, newest last.
const TOLD_KEPT: usize = 500;
/// The longest id of a thing told, in bytes.
const MAX_ID_BYTES: usize = 200;
/// The longest kind and key of a cause, in bytes.
const MAX_CAUSE_BYTES: usize = 128;
/// The longest label of a button, in bytes.
const MAX_LABEL_BYTES: usize = 80;
/// The longest name a device gives itself and word for how a thing was
/// acknowledged, in bytes.
const MAX_WORD_BYTES: usize = 80;
/// The most times a thing's log says it was told outside Tilo: the newest
/// are kept.
const PUSHED_KEPT: usize = 32;
/// How many people's last looks are kept before the oldest are let go.
const LOOKS_KEPT: usize = 10_000;
/// How long a look is kept once more than [`LOOKS_KEPT`] people looked.
const LOOK_KEPT: Duration = Duration::from_secs(60 * 60);

/// How long no device of a person's looks before they count as away: a
/// shorter gap is a device that slept or a page that was reloaded.
const AWAY_MS: u64 = 10 * 60 * 1000;
/// How often at most a person's last look is written down, in
/// milliseconds: when they left is known to within this.
const LOOK_KEPT_EVERY_MS: u64 = 60 * 1000;

/// Until when a device of a person's is known to look for what they were
/// told, and when that was last written down.
pub(super) struct Look {
    until: tilo_clock::time::Instant,
    kept_at_ms: u64,
}

/// What changing one thing told did to it.
enum Changed {
    /// Nothing: it stands as it stood.
    Nothing,
    /// Something only this host reads: it keeps its place.
    Noted,
    /// Something a device shows: it takes a new place.
    Moved,
}

/// What the host keeps of what a person was told.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    /// The place the newest thing took; the next takes the one after it.
    #[serde(default)]
    newest: u64,
    #[serde(default)]
    told: VecDeque<Told>,
    /// When a device of the person's last looked, to within
    /// [`LOOK_KEPT_EVERY_MS`]; zero before the first look.
    #[serde(default)]
    looked_at_ms: u64,
    /// When they last left: their last look before an absence.
    #[serde(default)]
    left_at_ms: Option<u64>,
    /// When they came back from it.
    #[serde(default)]
    back_at_ms: Option<u64>,
}

impl Kept {
    /// A device of the person's looks at `now_ms`. A look after an absence
    /// is their coming back: the stretch since they left begins at their
    /// last look before it, and stands until the next absence.
    fn look(&mut self, now_ms: u64) {
        if self.looked_at_ms != 0 && now_ms.saturating_sub(self.looked_at_ms) >= AWAY_MS {
            self.left_at_ms = Some(self.looked_at_ms);
            self.back_at_ms = Some(now_ms);
        }
        self.looked_at_ms = self.looked_at_ms.max(now_ms);
    }

    /// What happened while the person was away, as the one message at the
    /// top of the new stretch says it: what still waits for them, whenever
    /// it was told; what their assistant did alone by an event's lasting
    /// instruction; and whatever else came in.
    fn stretch(self) -> Stretch {
        let (Some(left), Some(back)) = (self.left_at_ms, self.back_at_ms) else {
            return Stretch::default();
        };
        let mut stretch = Stretch {
            left_at_ms: Some(left),
            back_at_ms: Some(back),
            ..Stretch::default()
        };
        for told in self.told {
            // What was told after they came back, they were there for.
            if told.at_ms > back {
                continue;
            }
            let asks = told.kind == Kind::Question || told.insists.is_some();
            if told.waits() && asks {
                stretch.waits.push(told);
            } else if told.at_ms <= left {
                // Told while they were there.
            } else if told.kind == Kind::News
                && told
                    .cause
                    .as_ref()
                    .is_some_and(|cause| cause.kind == CAUSE_EVENT)
            {
                stretch.did_alone.push(told);
            } else {
                stretch.came_in.push(told);
            }
        }
        stretch
    }
}

/// How a thing that must not be missed stands when it is first raised at
/// `now_ms`, by the rhythm `rule` keeps.
fn insisting(rule: &Importance, now_ms: u64) -> Insists {
    let (every_secs, times) = (rule.insists.every_secs, rule.insists.times);
    Insists {
        every_secs,
        times,
        raised: 1,
        next_at_ms: (times > 1).then(|| now_ms.saturating_add(every_secs.saturating_mul(1000))),
    }
}

fn log_at(root: &Path) -> PathBuf {
    root.join("telling").join("told.json")
}

/// What is kept under `root`. A log that does not read starts over, and
/// says so in the host's log.
fn read_log(root: &Path) -> Kept {
    let bytes = match std::fs::read(log_at(root)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Kept::default(),
        Err(error) => {
            tracing::warn!(%error, "telling: a person's log could not be read and starts over");
            return Kept::default();
        }
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "telling: a person's log does not read and starts over");
        Kept::default()
    })
}

/// Keep `kept` under `root`, for its owner alone to read.
fn write_log(root: &Path, kept: &Kept) -> Result<(), String> {
    let bytes = serde_json::to_vec(kept).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&log_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// One thing to tell a person, as whatever feeds their log hands it over.
#[derive(Clone, Debug, Default)]
pub struct ToTell {
    /// The id it keeps: a thing handed over again under the same id is
    /// told once. With none, a thing its cause names by a key takes its id
    /// from that, and any other is a new thing every time.
    pub id: Option<String>,
    /// What it is: a question for whoever tells a message's waiting
    /// question; a reminder or a brief only for what carries out an event
    /// of the person's own, from that event's own cause
    /// ([`Kind::of_cause`]); news for everything else, and for whatever an
    /// agent tells.
    pub kind: Kind,
    /// Its words, what caused it, where it is and its buttons.
    pub tell: Tell,
    /// The agent that told it.
    pub from: Option<Teller>,
    /// The workspace it was told from, by its id spelled out.
    pub workspace: Option<String>,
    /// Whether its words stay on the host whatever the person's rule
    /// says: what asks for a secret is shown only in Tilo.
    pub words_stay: bool,
    /// Whose day a judgement of it is counted on, where its teller knows:
    /// the assistant the person keeps. With none this host asks whoever
    /// knows ([`Spenders`]).
    pub spender: Option<Spender>,
}

/// Whose day the judgements made for a person are counted on: the
/// assistant that person keeps, with the limit they set it. Whatever runs
/// the host's agents knows, and names itself once
/// ([`Workspaces::count_judgements_with`]).
pub trait Spenders: Send + Sync {
    /// The agent whose day a judgement for `person` in `workspace` is
    /// counted on, told by `agent` when an agent told it. None where that
    /// person keeps no assistant there: no judgement is then made.
    fn spender<'a>(
        &'a self,
        person: &'a [u8; 32],
        workspace: &'a str,
        agent: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Option<Spender>> + Send + 'a>>;
}

/// `tell` as it is kept: its words within what a notification shows, its
/// buttons complete.
fn checked(mut tell: Tell) -> Result<Tell, String> {
    tell.words = clipped(tell.words.trim(), MAX_WORDS_BYTES);
    if tell.words.is_empty() {
        return Err("There is nothing to tell".to_owned());
    }
    tell.title = clipped(tell.title.trim(), MAX_TITLE_BYTES);
    if tell.actions.len() > MAX_ACTIONS {
        return Err(format!("A thing told offers at most {MAX_ACTIONS} buttons"));
    }
    for action in &tell.actions {
        // An answer its kind words itself has no label: the device words
        // it from its id.
        let named = !action.id.trim().is_empty() && action.label.len() <= MAX_LABEL_BYTES;
        let aimed = !action.target.app.is_empty()
            && !action.target.resource.is_empty()
            && !action.target.action.is_empty();
        if !named || !aimed {
            return Err("A button has an id, a short label and the action it invokes".to_owned());
        }
    }
    if let Some(cause) = &mut tell.cause {
        if cause.kind.trim().is_empty()
            || cause.kind.len() > MAX_CAUSE_BYTES
            || cause.key.len() > MAX_CAUSE_BYTES
        {
            return Err("A cause names its kind, and a short key of its own".to_owned());
        }
        cause.label = clipped(cause.label.trim(), MAX_TITLE_BYTES);
    }
    Ok(tell)
}

/// The id `tell` keeps: the one it was handed over under, else the one its
/// cause names it by, else none, and it is a new thing every time.
fn id_of(id: Option<String>, tell: &Tell) -> Result<Option<String>, String> {
    let id = id.or_else(|| {
        tell.cause
            .as_ref()
            .filter(|cause| !cause.key.is_empty())
            .map(|cause| format!("cause:{}:{}", cause.kind, cause.key))
    });
    if id.as_ref().is_some_and(|id| id.len() > MAX_ID_BYTES) {
        return Err("That id is longer than a thing told may have".to_owned());
    }
    Ok(id)
}

impl Workspaces {
    /// Name what knows whose day the judgements made for a person are
    /// counted on. Named once; a later call keeps the first.
    pub fn count_judgements_with(&self, spenders: std::sync::Arc<dyn Spenders>) {
        let _ = self.inner.telling.spenders.set(spenders);
    }

    /// Tell `person` one thing: mark it by their own rule, keep it in
    /// their log, and answer it as kept. A thing whose id the log already
    /// keeps is answered as it stands there, and nothing is judged again.
    ///
    /// # Errors
    /// It says nothing, a button of it is incomplete, or the log could not
    /// be written.
    pub async fn tell(&self, person: &[u8; 32], telling: ToTell) -> Result<Told, String> {
        self.tell_offered(person, telling, None).await
    }

    /// [`Self::tell`], with who offered its buttons where an agent's own
    /// telling carried them: `offered_by`, whom a device names beside them.
    ///
    /// # Errors
    /// As [`Self::tell`].
    pub(super) async fn tell_offered(
        &self,
        person: &[u8; 32],
        telling: ToTell,
        offered_by: Option<Teller>,
    ) -> Result<Told, String> {
        let ToTell {
            id,
            kind,
            tell,
            from,
            workspace,
            words_stay,
            spender,
        } = telling;
        let tell = checked(tell)?;
        let id = id_of(id, &tell)?;
        let root = self.person_settings_root(person);
        let known = |kept: &Kept| {
            let id = id.as_ref()?;
            kept.told.iter().find(|told| told.id == *id).cloned()
        };
        let (rule, earlier) = {
            let _writing = self.inner.telling.writing.lock_recover();
            let kept = read_log(&root);
            if let Some(told) = known(&kept) {
                return Ok(told);
            }
            let rule = read_rule(&root);
            let earlier = judge::judged_before(&kept.told, tell.cause.as_ref(), rule.revision);
            (rule, earlier)
        };
        let matter = Matter {
            kind,
            title: &tell.title,
            words: &tell.words,
            cause: tell.cause.as_ref(),
        };
        let mark = if let Some(mark) = judge::settled(&rule, &matter, earlier.as_ref()) {
            mark
        } else {
            let spender = match spender {
                Some(spender) => Some(spender),
                None => {
                    self.spender_for(person, workspace.as_deref(), from.as_ref())
                        .await
                }
            };
            let working_in = workspace.as_deref().and_then(WorkspaceId::parse);
            self.judged(&rule, &matter, spender, working_in).await
        };
        let reach = Reach::of(kind, mark.important);
        let outside = !words_stay
            && workspace
                .as_deref()
                .is_none_or(|workspace| rule.carries_outside(workspace));
        let told = {
            let _writing = self.inner.telling.writing.lock_recover();
            let mut kept = read_log(&root);
            // Handed over twice while it was judged: told once.
            if let Some(told) = known(&kept) {
                return Ok(told);
            }
            kept.newest = kept.newest.saturating_add(1);
            let now_ms = tilo_clock::unix_now_ms();
            let told = Told {
                seq: kept.newest,
                id: id.unwrap_or_else(|| format!("told-{}", kept.newest)),
                at_ms: now_ms,
                kind,
                from,
                title: tell.title,
                words: tell.words,
                about: tell.about,
                task: tell.task,
                cause: tell.cause,
                mark,
                reaches: reach.is_some(),
                through_quiet: reach.is_some_and(|reach| rule.lets_through(reach)),
                outside,
                actions: tell.actions,
                offered_by,
                // Only what reaches its person insists: whoever tells
                // cannot make a thing loud that the rule keeps quiet.
                insists: (tell.insists && reach.is_some()).then(|| insisting(&rule, now_ms)),
                acknowledged: None,
                withdrawn_at_ms: None,
                pushed: Vec::new(),
            };
            kept.told.push_back(told.clone());
            while kept.told.len() > TOLD_KEPT {
                kept.told.pop_front();
            }
            write_log(&root, &kept)?;
            told
        };
        self.told_moved(person);
        Ok(told)
    }

    /// Whose day a judgement of a thing `from` told `person` from
    /// `workspace` is counted on, as whatever runs this host's agents
    /// knows it.
    async fn spender_for(
        &self,
        person: &[u8; 32],
        workspace: Option<&str>,
        from: Option<&Teller>,
    ) -> Option<Spender> {
        let spenders = self.inner.telling.spenders.get()?;
        spenders
            .spender(person, workspace?, from.map(|teller| teller.agent.as_str()))
            .await
    }

    /// What `person` was told after place `after` of their log, oldest
    /// first. A thing that changes takes a new place and keeps its id, so
    /// whoever reads with the place it got to takes each change once.
    #[must_use]
    pub fn told_after(&self, person: &[u8; 32], after: u64) -> ToldList {
        let kept = {
            let _writing = self.inner.telling.writing.lock_recover();
            read_log(&self.person_settings_root(person))
        };
        ToldList {
            told: kept
                .told
                .into_iter()
                .filter(|told| told.seq > after)
                .collect(),
            newest: kept.newest,
        }
    }

    /// Moves whenever anybody's log changed: what a reader of
    /// [`Self::told_after`] wakes on.
    #[must_use]
    pub fn told_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.telling.told.subscribe()
    }

    /// `person`'s log changed in a way a device or a sender acts on.
    fn told_moved(&self, person: &[u8; 32]) {
        self.inner.telling.changed.lock_recover().insert(*person);
        self.inner
            .telling
            .told
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Whether whoever asks is the first to tell this host's people outside
    /// Tilo: one teller tells for a host, so nothing is told there twice.
    #[must_use]
    pub fn claim_telling_outside(&self) -> bool {
        !self
            .inner
            .telling
            .told_outside
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    }

    /// The people whose logs changed since this was last asked: what
    /// whoever tells people outside Tilo looks at when
    /// [`Self::told_changes`] moved.
    #[must_use]
    pub fn told_changed_people(&self) -> Vec<[u8; 32]> {
        std::mem::take(&mut *self.inner.telling.changed.lock_recover())
            .into_iter()
            .collect()
    }

    /// Everybody this host keeps a log of what they were told for: what
    /// whoever tells people outside Tilo looks at once, when it starts.
    #[must_use]
    pub fn people_told(&self) -> Vec<[u8; 32]> {
        let Ok(people) = std::fs::read_dir(self.inner.dir.join("people")) else {
            return Vec::new();
        };
        people
            .filter_map(Result::ok)
            .filter(|entry| log_at(&entry.path()).is_file())
            .filter_map(|entry| {
                hex::decode(entry.file_name().to_str()?)
                    .ok()?
                    .try_into()
                    .ok()
            })
            .collect()
    }

    /// A device of `person`'s looks for what they were told, from now and
    /// for as long as it said it waits.
    fn looked(&self, person: &[u8; 32], waits: Duration) {
        let now = tilo_clock::now();
        let now_ms = tilo_clock::unix_now_ms();
        let until = now.checked_add(waits).unwrap_or(now);
        let keep = {
            let mut looks = self.inner.telling.looks.lock_recover();
            if looks.len() >= LOOKS_KEPT {
                looks.retain(|_, look| now.saturating_duration_since(look.until) < LOOK_KEPT);
            }
            let look = looks.entry(*person).or_insert(Look {
                until,
                kept_at_ms: 0,
            });
            look.until = look.until.max(until);
            // Written down now and then, not at every look: a device
            // that waits for news asks every few seconds.
            let keep = now_ms.saturating_sub(look.kept_at_ms) >= LOOK_KEPT_EVERY_MS;
            if keep {
                look.kept_at_ms = now_ms;
            }
            drop(looks);
            keep
        };
        if keep {
            self.keep_look(person, now_ms);
        }
    }

    /// Write down that a device of `person`'s looked at `now_ms`, which is
    /// their coming back when nothing looked for a while before.
    fn keep_look(&self, person: &[u8; 32], now_ms: u64) {
        let root = self.person_settings_root(person);
        let _writing = self.inner.telling.writing.lock_recover();
        let mut kept = read_log(&root);
        kept.look(now_ms);
        if let Err(error) = write_log(&root, &kept) {
            tracing::warn!(%error, "telling: a person's look is not written down");
        }
    }

    /// A device of `person`'s looked at `now_ms`: what a test of what
    /// happened while they were away states the times of their looks with.
    #[cfg(any(test, feature = "test-support"))]
    pub fn device_looked_at(&self, person: &[u8; 32], now_ms: u64) {
        self.keep_look(person, now_ms);
    }

    /// What happened while `person` was away, for the one message at the
    /// top of the new stretch: nothing for a person who never left.
    #[must_use]
    pub fn since_left(&self, person: &[u8; 32]) -> Stretch {
        let kept = {
            let _writing = self.inner.telling.writing.lock_recover();
            read_log(&self.person_settings_root(person))
        };
        kept.stretch()
    }

    /// [`Self::since_left`] as a person's own device asks it, which is
    /// looking: the device that asks is how they came back.
    pub(super) fn since_left_for_device(&self, person: &[u8; 32]) -> Stretch {
        self.looked(person, Duration::ZERO);
        self.since_left(person)
    }

    /// Say whether the thing `id` told `person` must not be missed. One
    /// that must is raised again by the person's own rhythm until they
    /// acknowledge it. The person may say so of anything they were told,
    /// and it then reaches them; whoever else says so (`theirs` false)
    /// only of what reaches them already: nothing the rule keeps quiet is
    /// made loud by its teller.
    ///
    /// # Errors
    /// Nothing was told that person under that id, or it is not theirs to
    /// make insist.
    pub fn insist_told(
        &self,
        person: &[u8; 32],
        id: &str,
        insists: bool,
        theirs: bool,
    ) -> Result<Told, String> {
        let rule = self.importance_of(person);
        let mut refused = false;
        let told = self.change_told(person, id, |told, now_ms| {
            if !insists {
                return if told.insists.take().is_some() {
                    Changed::Moved
                } else {
                    Changed::Nothing
                };
            }
            if told.acknowledged.is_some() || told.withdrawn_at_ms.is_some() {
                return Changed::Nothing;
            }
            if !told.reaches && !theirs {
                refused = true;
                return Changed::Nothing;
            }
            if told.insists.is_some() {
                return Changed::Nothing;
            }
            told.reaches = true;
            told.insists = Some(insisting(&rule, now_ms));
            Changed::Moved
        })?;
        if refused {
            return Err("Only its person makes a thing insist that does not reach them".to_owned());
        }
        Ok(told)
    }

    /// Raise the thing `id` again for `person` when it insists and its
    /// time has come at `now_ms`: it takes a new place, so a device that
    /// runs raises its notice again, and whoever tells outside Tilo tells
    /// it again. Answers it as it stands, raised or not.
    ///
    /// # Errors
    /// Nothing was told that person under that id.
    pub fn raise_told(&self, person: &[u8; 32], id: &str, now_ms: u64) -> Result<Told, String> {
        self.change_told(person, id, |told, _| {
            if !told.waits() {
                return Changed::Nothing;
            }
            let Some(insists) = &mut told.insists else {
                return Changed::Nothing;
            };
            if insists.next_at_ms.is_none_or(|next| next > now_ms) {
                return Changed::Nothing;
            }
            insists.raised = insists.raised.saturating_add(1);
            insists.next_at_ms = (insists.raised < insists.times)
                .then(|| now_ms.saturating_add(insists.every_secs.saturating_mul(1000)));
            Changed::Moved
        })
    }

    /// Whether a device of `person`'s looked for what they were told
    /// within the last `within`: a device that runs raises its own notice,
    /// so nothing has to tell that person outside Tilo.
    #[must_use]
    pub fn looking(&self, person: &[u8; 32], within: Duration) -> bool {
        let now = tilo_clock::now();
        self.inner
            .telling
            .looks
            .lock_recover()
            .get(person)
            .is_some_and(|look| now.saturating_duration_since(look.until) < within)
    }

    /// Change the thing `id` of `person`'s log with `change`, and answer
    /// it as it stands after. A change a device shows gives it a new place.
    fn change_told(
        &self,
        person: &[u8; 32],
        id: &str,
        change: impl FnOnce(&mut Told, u64) -> Changed,
    ) -> Result<Told, String> {
        let unknown = || "Nothing was told under that id".to_owned();
        let root = self.person_settings_root(person);
        let (told, moved) = {
            let _writing = self.inner.telling.writing.lock_recover();
            let mut kept = read_log(&root);
            let at = kept
                .told
                .iter()
                .position(|told| told.id == id)
                .ok_or_else(unknown)?;
            let mut told = kept.told.remove(at).ok_or_else(unknown)?;
            match change(&mut told, tilo_clock::unix_now_ms()) {
                Changed::Nothing => return Ok(told),
                Changed::Noted => {
                    kept.told.insert(at, told.clone());
                    write_log(&root, &kept)?;
                    (told, false)
                }
                Changed::Moved => {
                    kept.newest = kept.newest.saturating_add(1);
                    told.seq = kept.newest;
                    kept.told.push_back(told.clone());
                    write_log(&root, &kept)?;
                    (told, true)
                }
            }
        };
        if moved {
            self.told_moved(person);
        }
        Ok(told)
    }

    /// `person` saw the thing `id` on `device`, or acted on it: `by` says
    /// how. The first acknowledgement stands, and a thing acknowledged
    /// insists no more.
    ///
    /// # Errors
    /// Nothing was told that person under that id.
    pub fn acknowledge_told(
        &self,
        person: &[u8; 32],
        id: &str,
        device: &str,
        by: &str,
    ) -> Result<Told, String> {
        self.change_told(person, id, |told, now_ms| {
            if told.acknowledged.is_some() {
                return Changed::Nothing;
            }
            let by = by.trim();
            told.acknowledged = Some(Acknowledged {
                at_ms: now_ms,
                device: clipped(device.trim(), MAX_WORD_BYTES),
                by: if by.is_empty() {
                    BY_OPENED.to_owned()
                } else {
                    clipped(by, MAX_WORD_BYTES)
                },
            });
            if let Some(insists) = &mut told.insists {
                insists.next_at_ms = None;
            }
            Changed::Moved
        })
    }

    /// What the thing `id` told `person` was taken back: a question its
    /// asker withdrew. It waits no more.
    ///
    /// # Errors
    /// Nothing was told that person under that id.
    pub fn withdraw_told(&self, person: &[u8; 32], id: &str) -> Result<Told, String> {
        self.change_told(person, id, |told, now_ms| {
            if told.withdrawn_at_ms.is_some() || told.acknowledged.is_some() {
                return Changed::Nothing;
            }
            told.withdrawn_at_ms = Some(now_ms);
            if let Some(insists) = &mut told.insists {
                insists.next_at_ms = None;
            }
            Changed::Moved
        })
    }

    /// The thing `id` was told `person` outside Tilo through `by`, and
    /// `took` of their devices took it: written down so it is told there
    /// once.
    ///
    /// # Errors
    /// Nothing was told that person under that id.
    pub fn told_outside(
        &self,
        person: &[u8; 32],
        id: &str,
        by: &str,
        took: u32,
    ) -> Result<Told, String> {
        self.change_told(person, id, |told, now_ms| {
            told.pushed.push(Pushed {
                at_ms: now_ms,
                by: by.to_owned(),
                took,
            });
            let over = told.pushed.len().saturating_sub(PUSHED_KEPT);
            told.pushed.drain(..over);
            Changed::Noted
        })
    }

    /// [`Self::told_after`] as a person's own device asks it: with a wait,
    /// held until their log holds something newer or the wait is over.
    pub async fn told_listed(&self, person: &[u8; 32], asking: &ListAfter) -> ToldList {
        let wait = Duration::from_millis(asking.wait_ms.unwrap_or(0).min(MAX_LIST_WAIT_MS));
        // Asking is looking, for as long as the ask waits.
        self.looked(person, wait);
        // Following before the first look, so a thing told between the
        // look and the wait still wakes it.
        let mut moved = self.told_changes();
        let now = tilo_clock::now();
        let until = tokio::time::Instant::from_std(now.checked_add(wait).unwrap_or(now));
        loop {
            let list = self.told_after(person, asking.after);
            if !list.told.is_empty() {
                return list;
            }
            match tokio::time::timeout_at(until, moved.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return list,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::person_telling::Cause;

    use super::*;

    const MINUTE: u64 = 60 * 1000;

    fn told(id: &str, at_ms: u64, kind: Kind, cause: Option<&str>) -> Told {
        Told {
            id: id.into(),
            at_ms,
            kind,
            words: id.into(),
            cause: cause.map(|kind| Cause {
                kind: kind.into(),
                ..Cause::default()
            }),
            reaches: kind != Kind::News,
            ..Told::default()
        }
    }

    /// A person is away once nothing of theirs looked for ten minutes, and
    /// the stretch since they left stands until they are away again.
    #[test]
    fn a_look_after_an_absence_is_coming_back() {
        let mut kept = Kept::default();
        // The first look is no coming back: they were never there before.
        kept.look(100 * MINUTE);
        assert_eq!((kept.left_at_ms, kept.back_at_ms), (None, None));
        // A device that slept nine minutes was not away.
        kept.look(109 * MINUTE);
        assert_eq!((kept.left_at_ms, kept.back_at_ms), (None, None));
        // Ten minutes with no look: they left at their last look.
        kept.look(119 * MINUTE);
        assert_eq!(
            (kept.left_at_ms, kept.back_at_ms),
            (Some(109 * MINUTE), Some(119 * MINUTE))
        );
        // While they stay, the stretch stands.
        kept.look(120 * MINUTE);
        kept.look(125 * MINUTE);
        assert_eq!(
            (kept.left_at_ms, kept.back_at_ms),
            (Some(109 * MINUTE), Some(119 * MINUTE))
        );
        // The next absence makes the next stretch.
        kept.look(200 * MINUTE);
        assert_eq!(
            (kept.left_at_ms, kept.back_at_ms),
            (Some(125 * MINUTE), Some(200 * MINUTE))
        );
        // A look written down late moves nothing back.
        kept.look(150 * MINUTE);
        assert_eq!(kept.looked_at_ms, 200 * MINUTE);
    }

    /// The message at the top of the new stretch says what still waits,
    /// what the assistant did alone, and what else came in, and nothing
    /// the person was there for.
    #[test]
    fn the_stretch_holds_what_happened_while_its_person_was_away() {
        let (left, back) = (100 * MINUTE, 160 * MINUTE);
        let mut answered = told("answered", 120 * MINUTE, Kind::Question, None);
        answered.acknowledged = Some(Acknowledged {
            at_ms: 130 * MINUTE,
            device: String::new(),
            by: "answered".into(),
        });
        let kept = Kept {
            told: VecDeque::from(vec![
                // A question from before they left that still waits.
                told("old-question", 90 * MINUTE, Kind::Question, None),
                // News they were there for.
                told("before", 95 * MINUTE, Kind::News, Some("event")),
                told("did", 110 * MINUTE, Kind::News, Some("event")),
                told("found", 115 * MINUTE, Kind::News, Some("competitors")),
                answered,
                told("reminder", 125 * MINUTE, Kind::Reminder, Some("reminder")),
                told("question", 140 * MINUTE, Kind::Question, Some("event")),
                // Told once they were back.
                told("after", 170 * MINUTE, Kind::News, Some("event")),
            ]),
            left_at_ms: Some(left),
            back_at_ms: Some(back),
            ..Kept::default()
        };
        let stretch = kept.stretch();
        let ids = |told: &[Told]| told.iter().map(|told| told.id.clone()).collect::<Vec<_>>();
        assert_eq!(
            (stretch.left_at_ms, stretch.back_at_ms),
            (Some(left), Some(back))
        );
        assert_eq!(ids(&stretch.waits), ["old-question", "question"]);
        assert_eq!(ids(&stretch.did_alone), ["did"]);
        assert_eq!(ids(&stretch.came_in), ["found", "answered", "reminder"]);
        // A person who never left is told nothing happened.
        assert_eq!(Kept::default().stretch(), Stretch::default());
    }
}

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
    Kind, ListAfter, MAX_ACTIONS, MAX_LIST_WAIT_MS, MAX_TITLE_BYTES, MAX_WORDS_BYTES, Reach, Tell,
    Teller, Told, ToldList,
};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

use super::super::Workspaces;
use super::super::person_model::Spender;
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

/// What the host keeps of what a person was told.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kept {
    /// The place the newest thing took; the next takes the one after it.
    #[serde(default)]
    newest: u64,
    #[serde(default)]
    told: VecDeque<Told>,
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
    /// question, and otherwise what its cause says
    /// ([`Kind::of_cause`]).
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
        let named = !action.id.trim().is_empty() && !action.label.trim().is_empty();
        let aimed = !action.target.app.is_empty()
            && !action.target.resource.is_empty()
            && !action.target.action.is_empty();
        if !named || !aimed || action.label.len() > MAX_LABEL_BYTES {
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
        let id = id.or_else(|| {
            tell.cause
                .as_ref()
                .filter(|cause| !cause.key.is_empty())
                .map(|cause| format!("cause:{}:{}", cause.kind, cause.key))
        });
        if id.as_ref().is_some_and(|id| id.len() > MAX_ID_BYTES) {
            return Err("That id is longer than a thing told may have".to_owned());
        }
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
            self.judged(person, &rule, &matter, spender, working_in)
                .await
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
            let told = Told {
                seq: kept.newest,
                id: id.unwrap_or_else(|| format!("told-{}", kept.newest)),
                at_ms: tilo_clock::unix_now_ms(),
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
                insists: None,
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
        self.told_moved();
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

    fn told_moved(&self) {
        self.inner
            .telling
            .told
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// [`Self::told_after`] as a person's own device asks it: with a wait,
    /// held until their log holds something newer or the wait is over.
    pub(super) async fn told_listed(&self, person: &[u8; 32], asking: &ListAfter) -> ToldList {
        let wait = Duration::from_millis(asking.wait_ms.unwrap_or(0).min(MAX_LIST_WAIT_MS));
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

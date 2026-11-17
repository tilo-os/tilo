//! `tilo_watch`: what happened since the caller last asked — changes to a
//! resource, or to every record of the workspace, this session's runs
//! starting and finishing, and who is in the workspace — held open until
//! something did, so a program follows the workspace without polling a
//! second endpoint.
//!
//! The sources are the ones the desktop and the browser read: the resource's
//! own history verb for a change and who made it (the host's feed of every
//! record's changes, [`tilo_actions::remote::ChangeFeed`], for a watch of no
//! path), the host's observation of an invocation for a run, and the host's
//! roster for presence.
use super::*;
use crate::step::DeclaredRun;
use tilo_actions::remote::WatchScope;

/// One invocation of this session, as `tilo_watch` follows it: what it ran,
/// the run it is a step of, and how it stands.
#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub(crate) invocation: String,
    /// The path the call named, when it named one.
    pub(crate) path: String,
    /// The run it is a step of, as people read it, and its id.
    pub(crate) name: String,
    pub(crate) group: String,
    pub(crate) started_ms: u64,
    /// Its phase as last told, and the sequence number it was told at: a
    /// cursor before `seq` is told `phase`.
    pub(crate) phase: String,
    pub(crate) seq: u64,
}

/// The invocations a session keeps for its watches and `tilo_status`, at most: one that runs
/// thousands of actions (a migration does, over one connection) keeps its latest ones rather
/// than every one it ever ran.
const RUNS_KEPT: usize = 1024;

/// How long a watch with no `wait_ms` holds before it answers with what it
/// has.
const DEFAULT_WATCH_MS: u64 = 25_000;
/// The longest a watch holds, so a caller's connection is never held longer
/// than it named.
const MAX_WATCH_MS: u64 = 60_000;
/// How many of a resource's changes one look reads.
const HISTORY_PAGE: u64 = 50;

/// What a watch takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WatchArgs {
    cursor: Option<String>,
    path: Option<String>,
    wait_ms: Option<u64>,
}

/// What a watch has already told its caller: the last run event, the
/// workspace's roster as it last read it, and the last change of the watched
/// path. Opaque to the caller, which hands it back as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Cursor {
    /// The sequence number of the last run event answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run: Option<u64>,
    /// The roster's fingerprint when presence was last answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    here: Option<String>,
    /// The last change of the watched path answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    change: Option<String>,
    /// Where the host's feed of the workspace's changes was read up to, for
    /// a watch of no path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    feed: Option<String>,
    /// Where the workspace and this device's writes stood when last answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<tilo_actions::remote::SessionState>,
}

impl Cursor {
    /// A cursor as the caller hands it back: what an earlier answer said.
    fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|_| "cursor is not one tilo_watch issued".to_owned())
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// How invocation `id` stands, as `tilo_status` answers it.
    pub(crate) async fn status(&self, id: tilo_actions::InvocationId) -> Result<Value, String> {
        self.observed(id)
            .await
            .as_ref()
            .map(|state| crate::state_json(id, state))
            .ok_or_else(|| crate::UNOWNED.into())
    }

    /// The host's last state of invocation `id`, asked off this task's
    /// thread: a host may ask the device running it and wait for the answer
    /// (a session's actions do), and a wait on this thread would stall the
    /// connection that carries that answer until it gave up.
    pub(crate) async fn observed(
        &self,
        id: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        let host = std::sync::Arc::clone(&self.host);
        let caller = self.caller();
        tokio::task::spawn_blocking(move || host.observe(&caller, &id))
            .await
            .ok()
            .flatten()
    }

    /// Note that `invocation` started, as a watch of this session follows it.
    pub(super) fn watch_run_started(&self, invocation: &str, path: &str) {
        let (name, run_id) = self.run_named();
        let mut runs = self.runs.lock_recover();
        let seq = next_seq(&runs);
        runs.push(Run {
            invocation: invocation.to_owned(),
            path: path.to_owned(),
            name,
            group: run_id,
            started_ms: tilo_clock::unix_now_ms(),
            phase: "started".to_owned(),
            seq,
        });
        keep_latest(&mut runs);
        drop(runs);
        self.runs_moved();
    }

    /// Note that `invocation` reached `phase`, which is terminal.
    pub(super) fn watch_run_ended(&self, invocation: &str, phase: &str) {
        ended(&mut self.runs.lock_recover(), invocation, phase);
        self.runs_moved();
    }

    /// Note that `invocation` is held for the person, who allows it or not in their tab.
    pub(super) fn watch_run_held(&self, invocation: &str) {
        held(&mut self.runs.lock_recover(), invocation);
        self.runs_moved();
    }

    /// The run a watch attributes this session's calls to: the title a call
    /// grouped under, else the connection's own run.
    fn run_named(&self) -> (String, String) {
        let run = self.agent_session();
        let id = run.as_ref().map(|run| run.id.clone()).unwrap_or_default();
        let label = match DeclaredRun::current().map(|run| run.title) {
            Some(title) if !title.is_empty() => title,
            _ => run.map_or_else(String::new, |run| run.label),
        };
        (label, id)
    }

    /// [`WatchArgs`] as one answer: what happened since `cursor`, or nothing
    /// with a cursor to ask again from.
    pub(super) async fn agent_watch(&self, args: WatchArgs) -> Result<Reply, Failure> {
        let wait = args.wait_ms.unwrap_or(DEFAULT_WATCH_MS).min(MAX_WATCH_MS);
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(wait))
            .unwrap_or_else(std::time::Instant::now);
        let path = args.path.as_deref();
        let mut cursor = match args.cursor.as_deref() {
            None | Some("") => self.watch_now(path).await?,
            Some(text) => Cursor::parse(text)?,
        };
        // Listened for before anything is read, and again each time it wakes the watch: what is
        // read after an arming is covered by it, so a source that moves between a read and the
        // wait still wakes this watch, and a source that is already armed is not asked again.
        let mut listening = self.watch_changed();
        loop {
            let mut events = Vec::new();
            self.watch_states(&mut cursor, &mut events);
            self.watch_runs(&mut cursor, &mut events).await;
            self.watch_here(&mut cursor, &mut events);
            self.watch_changes(path, &mut cursor, &mut events).await?;
            if !events.is_empty() || std::time::Instant::now() >= deadline {
                let answer = json!({
                    "events": events,
                    "cursor": serde_json::to_string(&cursor)
                        .map_err(|error| error.to_string())?,
                });
                let mut reply = Reply::ok(answer.to_string());
                reply.structured = Some(answer);
                return Ok(reply);
            }
            if self.hold(&mut listening, deadline).await {
                // What woke the watch is spent: listened for again before the next read. A source
                // whose answer was abandoned tells nothing more, and the watch holds to its own
                // deadline instead of reading again on a timer.
                listening = self.watch_changed();
            }
        }
    }

    /// Waits until a source the watch follows may have changed, or until `deadline`: `true` when
    /// a source's own signal woke it, `false` when its wait ran out or nothing signals it.
    async fn hold(
        &self,
        listening: &mut Option<tilo_actions::remote::Pending<()>>,
        deadline: std::time::Instant,
    ) -> bool {
        let until = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
        let Some(wake) = listening.as_mut() else {
            until.await;
            return false;
        };
        tokio::select! {
            woken = wake => woken.is_some(),
            () = until => false,
        }
    }

    /// Every source this watch follows, listened for before anything is read so that a change
    /// between a read and the wait still wakes it: where the workspace and this device's writes
    /// stand, the workspace's records, who is in it, each of this session's runs still going, and
    /// the runs of this session itself. A host that cannot tell of a source leaves it out, and
    /// that source then costs the watch the rest of its wait.
    fn watch_changed(&self) -> Option<tilo_actions::remote::Pending<()>> {
        let caller = self.caller();
        let mut changed: Vec<tilo_actions::remote::Pending<()>> = Vec::new();
        changed.extend(self.host.session_state_changed(&caller));
        changed.extend(self.host.changed(&caller, WatchScope::Records));
        changed.extend(self.host.changed(&caller, WatchScope::Here));
        let going: Vec<String> = self
            .runs
            .lock_recover()
            .iter()
            .filter(|run| !is_terminal(&run.phase))
            .map(|run| run.invocation.clone())
            .collect();
        for invocation in going {
            let Ok(id) = invocation_id(&invocation) else {
                continue;
            };
            // The host's own signal for an invocation, which it answers when the invocation may
            // have moved.
            changed.extend(self.host.next_change(&caller, &id));
        }
        changed.push(self.runs_changed());
        first_wake(changed)
    }

    /// Resolves on the next run event of this session: an invocation of it starting, being held
    /// for its person, or ending ([`Self::runs_moved`]).
    fn runs_changed(&self) -> tilo_actions::remote::Pending<()> {
        let (changed, change) = tilo_actions::remote::pending();
        let mut waiting = self.run_waiters.lock_recover();
        // A wait another source woke the watch from tells nobody: forgotten here rather than kept
        // until the next run.
        waiting.retain(|woken| !woken.is_abandoned());
        waiting.push(changed);
        change
    }

    /// Wake every watch of this session: a run of it started, was held for its person, or ended.
    fn runs_moved(&self) {
        let waiting = std::mem::take(&mut *self.run_waiters.lock_recover());
        for changed in waiting {
            changed.complete(());
        }
    }

    /// Where the workspace and this device's writes stand, when that changed since the cursor:
    /// a `workspace` event when the workspace moved (`admitting`, `open`, `serving`), a `sync`
    /// event when the writes fell behind or caught up.
    fn watch_states(&self, cursor: &mut Cursor, events: &mut Vec<Value>) {
        let Some(now) = self.host.session_state(&self.caller()) else {
            return;
        };
        let told = cursor.state.replace(now);
        if told.is_none_or(|told| told.workspace != now.workspace) {
            events.push(json!({"kind": "workspace", "state": now.workspace}));
        }
        if told.is_none_or(|told| {
            std::mem::discriminant(&told.sync) != std::mem::discriminant(&now.sync)
        }) {
            let mut sync = json!(now.sync);
            if let Some(fields) = sync.as_object_mut() {
                fields.insert("kind".into(), "sync".into());
            }
            events.push(sync);
        }
    }

    /// The cursor a caller with none starts from: nothing that has happened
    /// yet is news to it.
    async fn watch_now(&self, path: Option<&str>) -> Result<Cursor, String> {
        let run = self.runs.lock_recover().iter().map(|run| run.seq).max();
        let feed = match path_named(path) {
            Some(_) => None,
            None => self.feed_page(None).await?.map(|page| page.cursor),
        };
        Ok(Cursor {
            run,
            here: self.here_now().map(|(fingerprint, _)| fingerprint),
            change: last_change(
                self.history_page(path)
                    .await?
                    .as_ref()
                    .map(|(_, entries)| entries.as_slice()),
            ),
            feed,
            state: self.host.session_state(&self.caller()),
        })
    }

    /// The runs this session started since the cursor, as the host says they
    /// stand now.
    async fn watch_runs(&self, cursor: &mut Cursor, events: &mut Vec<Value>) {
        let seen = cursor.run.unwrap_or_default();

        // How the runs still going stand: an invocation that finished after
        // its call answered is reported finished here.
        let going: Vec<String> = self
            .runs
            .lock_recover()
            .iter()
            .filter(|run| !is_terminal(&run.phase))
            .map(|run| run.invocation.clone())
            .collect();
        let mut moved: Vec<(String, String)> = Vec::new();
        for invocation in going {
            let Ok(id) = invocation_id(&invocation) else {
                continue;
            };
            let Ok(value) = self.status(id).await else {
                continue;
            };
            if let Some(state) = value
                .pointer("/update/state")
                .and_then(Value::as_str)
                .filter(|state| is_terminal(state))
            {
                moved.push((invocation, state.to_owned()));
            }
        }
        let announced: Vec<Run> = {
            let mut runs = self.runs.lock_recover();
            for (invocation, phase) in moved {
                ended(&mut runs, &invocation, &phase);
            }
            let last = runs.iter().map(|run| run.seq).max();
            if let Some(last) = last {
                cursor.run = Some(cursor.run.unwrap_or_default().max(last));
            }
            runs.iter().filter(|run| run.seq > seen).cloned().collect()
        };
        for run in announced {
            events.push(json!({
                "kind": "run",
                "phase": run.phase,
                "invocation_id": run.invocation,
                "path": run.path,
                "run": run.name,
                "run_id": run.group,
                "time_ms": run.started_ms,
            }));
        }
    }

    /// Who is in the workspace, when that changed since the cursor.
    fn watch_here(&self, cursor: &mut Cursor, events: &mut Vec<Value>) {
        let Some((fingerprint, here)) = self.here_now() else {
            return;
        };
        if cursor.here.as_deref() == Some(fingerprint.as_str()) {
            return;
        }
        cursor.here = Some(fingerprint);
        events.push(json!({"kind":"presence","here":here}));
    }

    /// The changes of the watched path's history since the cursor.
    async fn watch_changes(
        &self,
        path: Option<&str>,
        cursor: &mut Cursor,
        events: &mut Vec<Value>,
    ) -> Result<(), String> {
        if path_named(path).is_none() {
            return self.watch_feed(cursor, events).await;
        }
        let page = self.history_page(path).await?;
        let Some((app, page)) = page else {
            return Ok(());
        };
        let seen = cursor.change.take();
        let known = seen.as_deref();
        let mut after = known.is_none_or(|seen| {
            // The change the caller last saw may have left the page a busy
            // resource answers: everything on it then is news.
            !page
                .iter()
                .any(|entry| entry.get("change").and_then(Value::as_str) == Some(seen))
        });
        for entry in &page {
            let change = entry
                .get("change")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !after {
                after = known.is_some_and(|seen| seen == change);
                continue;
            }
            let mut event = change_event(path.unwrap_or_default(), Some(&app), None, entry);
            event["change"] = json!(change);
            events.push(event);
        }
        cursor.change = last_change(Some(&page)).or(seen);
        Ok(())
    }

    /// The changes to every record of the workspace since the cursor, as the
    /// host's feed reports them, each named by the app it is a record of.
    async fn watch_feed(&self, cursor: &mut Cursor, events: &mut Vec<Value>) -> Result<(), String> {
        let Some(page) = self.feed_page(cursor.feed.as_deref()).await? else {
            return Ok(());
        };
        for change in page.changes {
            let name = app_names(std::slice::from_ref(&change.app)).join("");
            let event = change_event(
                &name,
                Some(&change.app),
                (!change.record.is_empty()).then_some(change.record.as_str()),
                &change.entry,
            );
            events.push(event);
        }
        cursor.feed = Some(page.cursor);
        Ok(())
    }

    /// One page of the host's feed of the workspace's changes after `after`:
    /// `None` for a host that keeps none, which a watch of no path then
    /// reports nothing of.
    async fn feed_page(
        &self,
        after: Option<&str>,
    ) -> Result<Option<tilo_actions::remote::ChangePage>, String> {
        let Some(feed) = self.host.change_feed() else {
            return Ok(None);
        };
        match feed.since(&self.caller(), after).await {
            Some(Ok(page)) => Ok(Some(page)),
            // A host that keeps no feed says so; a watch still reports runs
            // and presence.
            Some(Err(tilo_actions::remote::ActionHostError::Unavailable { reason }))
                if reason == tilo_actions::remote::CHANGE_FEED_UNAVAILABLE =>
            {
                Ok(None)
            }
            Some(Err(error)) => Err(format!("the workspace's changes are not read: {error}")),
            None => Err("the host dropped the workspace's changes".to_owned()),
        }
    }

    /// The watched path's history, oldest first, as its own `history` verb
    /// answers it: `None` for a watch of no path, or of a resource with no
    /// history to read.
    async fn history_page(
        &self,
        path: Option<&str>,
    ) -> Result<Option<(String, Vec<Value>)>, String> {
        let Some(path) = path_named(path) else {
            return Ok(None);
        };
        let parsed = parse_path(path)?;
        let (Ok(resolved), _) = self.resolve(&parsed).await? else {
            return Ok(None);
        };
        let Some(history) = resolved
            .actions
            .iter()
            .find(|action| action.address.action == "history")
            .cloned()
        else {
            return Ok(None);
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("last".into(), json!(HISTORY_PAGE));
        let value = Box::pin(self.run(
            RemoteActionTarget {
                provider: resolved.provider.clone(),
                address: history.address.clone(),
            },
            arguments,
            tilo_actions::InvocationId::new(),
            None,
            std::time::Duration::from_millis(5_000),
            &mut None,
        ))
        .await?;
        Ok(history_entries(&value).map(|entries| (resolved.app_id, entries)))
    }

    /// Who is in the workspace now, and a fingerprint of it: `None` for a
    /// host with no roster to say.
    fn here_now(&self) -> Option<(String, Vec<Value>)> {
        let here = self.host.here(&self.caller())?;
        let mut listed: Vec<Value> = here
            .iter()
            .map(|here| json!({"who": here.who, "name": here.name, "kind": here.kind}))
            .collect();
        listed.sort_by(|left, right| {
            left["who"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["who"].as_str().unwrap_or_default())
        });
        let mut hasher = blake3::Hasher::new_derive_key("tilo watch here v1");
        for entry in &listed {
            hasher.update(entry.to_string().as_bytes());
        }
        Some((hasher.finalize().to_hex().to_string(), listed))
    }
}

/// The path a watch names, when it names one.
fn path_named(path: Option<&str>) -> Option<&str> {
    path.map(str::trim).filter(|path| !path.is_empty())
}

/// One answer for any of `waits`: resolves when the first of them does, which is a reason to
/// look again. `None` for no waits at all, which leaves a caller only its deadline.
fn first_wake(
    waits: Vec<tilo_actions::remote::Pending<()>>,
) -> Option<tilo_actions::remote::Pending<()>> {
    let mut waits = waits.into_iter();
    let first = waits.next()?;
    let (woken, wake) = tilo_actions::remote::pending();
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(woken)));
    for wait in std::iter::once(first).chain(waits) {
        let slot = std::sync::Arc::clone(&slot);
        wait.then(move |_| {
            let woken = slot.lock().ok().and_then(|mut slot| slot.take());
            if let Some(woken) = woken {
                woken.complete(());
            }
        });
    }
    Some(wake)
}

/// One change, as an agent or an API caller reads it: the resource it
/// changed (the record, the item in it when the change names one), who made
/// it, and the run it is a step of. The run's name is under `run` and the
/// invocation under `invocation`, as the record's own history names them.
fn change_event(path: &str, app: Option<&str>, record: Option<&str>, entry: &Value) -> Value {
    let mut resource = serde_json::Map::new();
    resource.insert("path".into(), json!(path));
    for (key, value) in [("app", app), ("record", record)] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            resource.insert(key.into(), json!(value));
        }
    }
    if let Some(item) = entry.get("item").and_then(Value::as_str) {
        resource.insert("item".into(), json!(item));
    }
    let mut event = json!({
        "kind": "change",
        "path": path,
        "resources": [Value::Object(resource)],
        "change": entry.get("change").cloned().unwrap_or_default(),
        "sentence": entry.get("sentence").cloned().unwrap_or_default(),
        "time_ms": entry.get("time_ms").cloned().unwrap_or_default(),
    });
    if let (Some(fields), Some(entry)) = (event.as_object_mut(), entry.as_object()) {
        for key in ["by", "run", "run_id", "invocation", "undone"] {
            if let Some(value) = entry.get(key) {
                fields.insert(key.to_owned(), value.clone());
            }
        }
        if let Some(actor) = actor_of(entry) {
            fields.insert("actor".into(), actor);
        }
    }
    event
}

/// Who made one change, structured: the record's own record of its actor
/// (a person or an agent, with the name it was shown by), or, for a change
/// recorded by a person, the name the record keeps of them.
fn actor_of(entry: &serde_json::Map<String, Value>) -> Option<Value> {
    if let Some(actor) = entry.get("actor").filter(|actor| actor.is_object()) {
        return Some(actor.clone());
    }
    let name = entry.get("by").and_then(Value::as_str)?.trim();
    if name.is_empty() {
        return None;
    }
    let run = entry
        .get("run")
        .and_then(Value::as_str)
        .is_some_and(|run| !run.is_empty());
    Some(json!({"kind": if run { "agent" } else { "person" }, "name": name}))
}

/// The entries of a `history` verb's answer `value`, oldest first: its
/// `versions`, as the history verbs answer, each whole — what `history`
/// leaves out of an entry as the one before's (`run`, `run_id`, `by`,
/// `time_ms`) put back.
pub(super) fn history_entries(value: &Value) -> Option<Vec<Value>> {
    let output = value.pointer("/update/output/value")?;
    let listed = ["versions", "history"]
        .iter()
        .find_map(|key| output.get(*key))
        .unwrap_or(output)
        .as_array()?;
    let mut carried = serde_json::Map::new();
    Some(
        listed
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                if let Some(fields) = entry.as_object_mut() {
                    for key in ["run", "run_id", "by", "time_ms"] {
                        match fields.get(key) {
                            Some(value) => {
                                carried.insert(key.to_owned(), value.clone());
                            }
                            None => {
                                if let Some(value) = carried.get(key) {
                                    fields.insert(key.to_owned(), value.clone());
                                }
                            }
                        }
                    }
                }
                entry
            })
            .collect(),
    )
}

/// Note that `invocation` reached the terminal `phase`, as one run event: a
/// run already told an end is not told it again, however many sources (its
/// call's answer, a watch's look at the host) say so.
fn ended(runs: &mut [Run], invocation: &str, phase: &str) {
    let seq = next_seq(runs);
    if let Some(run) = runs
        .iter_mut()
        .rev()
        .find(|run| run.invocation == invocation)
        && !is_terminal(&run.phase)
    {
        phase.clone_into(&mut run.phase);
        run.seq = seq;
    }
}

/// Note that `invocation` is held for its person, as one run event: it moves on to how it
/// ended once they answered.
fn held(runs: &mut [Run], invocation: &str) {
    let seq = next_seq(runs);
    if let Some(run) = runs
        .iter_mut()
        .rev()
        .find(|run| run.invocation == invocation)
        && !is_terminal(&run.phase)
        && run.phase != HELD
    {
        HELD.clone_into(&mut run.phase);
        run.seq = seq;
    }
}

/// The phase of a run held for its person.
const HELD: &str = "held_for_person";

/// Let go of the oldest of `runs` past [`RUNS_KEPT`], those that ended first. The one told
/// last stays: the next sequence number counts on from it, so a cursor never sees a number twice.
fn keep_latest(runs: &mut Vec<Run>) {
    while runs.len() > RUNS_KEPT {
        let last = runs.iter().map(|run| run.seq).max();
        let oldest = runs
            .iter()
            .position(|run| is_terminal(&run.phase) && Some(run.seq) != last)
            .or_else(|| runs.iter().position(|run| Some(run.seq) != last));
        let Some(oldest) = oldest else { return };
        runs.remove(oldest);
    }
}

/// The change id a history page ends at.
fn last_change(page: Option<&[Value]>) -> Option<String> {
    page?
        .last()?
        .get("change")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The sequence number the next run event takes.
fn next_seq(runs: &[Run]) -> u64 {
    runs.iter()
        .map(|run| run.seq)
        .max()
        .unwrap_or_default()
        .saturating_add(1)
}

/// Whether `phase` is one an invocation never leaves.
fn is_terminal(phase: &str) -> bool {
    matches!(phase, "succeeded" | "failed" | "cancelled" | "refused")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(invocation: &str, seq: u64) -> Run {
        Run {
            invocation: invocation.to_owned(),
            path: "word-processor#append-paragraph".to_owned(),
            name: "Write the letter".to_owned(),
            group: "run".to_owned(),
            started_ms: 0,
            phase: "started".to_owned(),
            seq,
        }
    }

    /// A session that runs thousands of actions keeps its latest ones: what it holds stays
    /// bounded however long the connection lives, and the sequence a watch reads by still
    /// counts on.
    #[test]
    fn a_long_session_keeps_its_latest_invocations() {
        let mut runs = Vec::new();
        let mut told = 0;
        let mut newest = String::new();
        for n in 0..RUNS_KEPT * 4 {
            newest = n.to_string();
            let seq = next_seq(&runs);
            assert!(seq > told, "sequence {seq} after {told}");
            runs.push(started(&n.to_string(), seq));
            keep_latest(&mut runs);
            told = seq;
            ended(&mut runs, &n.to_string(), "succeeded");
            told = told.max(runs.iter().map(|run| run.seq).max().unwrap_or_default());
        }
        assert_eq!(runs.len(), RUNS_KEPT);
        assert_eq!(runs.last().map(|run| &run.invocation), Some(&newest));
        // One still running outlasts those that ended.
        let mut runs = vec![started("going", 1)];
        for n in 0..RUNS_KEPT * 2 {
            let seq = next_seq(&runs);
            runs.push(started(&n.to_string(), seq));
            ended(&mut runs, &n.to_string(), "succeeded");
            keep_latest(&mut runs);
        }
        assert_eq!(runs.len(), RUNS_KEPT);
        assert_eq!(runs[0].invocation, "going");
    }

    tilo_test_env::scenario! {
        fn a_watch_tells_a_runs_end_once_and_reads_history_whole;

        phase a_run_is_told_its_end_once;
        // A run's end is one event however many sources report it: its call's
        // answer and a watch's look at the host both say it succeeded, and the
        // second moves nothing a cursor already passed.
        {
            let mut runs = vec![started("a", 1)];
            ended(&mut runs, "a", "succeeded");
            assert_eq!((runs[0].phase.as_str(), runs[0].seq), ("succeeded", 2));
            ended(&mut runs, "a", "succeeded");
            ended(&mut runs, "a", "failed");
            assert_eq!((runs[0].phase.as_str(), runs[0].seq), ("succeeded", 2));
        }

        phase a_run_held_for_its_person_is_told_then_how_it_ended;
        // A run the person is asked about is told as held, once; their answer
        // moves it on to how it ended, and a refused call ends it.
        {
            let mut runs = vec![started("a", 1), started("b", 2)];
            held(&mut runs, "a");
            assert_eq!((runs[0].phase.as_str(), runs[0].seq), (HELD, 3));
            held(&mut runs, "a");
            assert_eq!(runs[0].seq, 3, "held is told once");
            assert!(!is_terminal(HELD));
            ended(&mut runs, "a", "succeeded");
            assert_eq!(runs[0].phase, "succeeded");
            ended(&mut runs, "b", "refused");
            assert!(is_terminal(&runs[1].phase));
            held(&mut runs, "b");
            assert_eq!(runs[1].phase, "refused", "an ended run is never held again");
        }

        phase history_entries_are_read_whole;
        // History lists a run's author once for the run of versions it made;
        // a reader of one version reads it whole.
        {
            let answer = json!({"update": {"output": {"value": {"versions": [
                {"change": "a", "run": "Letter", "run_id": "r1", "time_ms": 5},
                {"change": "b", "invocation": "i2"},
                {"change": "c", "by": "Ada", "time_ms": 9},
            ]}}}});
            let entries = history_entries(&answer).unwrap();
            assert_eq!(entries[1]["run_id"], "r1");
            assert_eq!(entries[1]["time_ms"], 5);
            assert_eq!(entries[2]["by"], "Ada");
            // A history answered as a bare list still reads.
            let bare = json!({"update": {"output": {"value": [{"change": "a"}]}}});
            assert_eq!(history_entries(&bare).unwrap().len(), 1);
        }
    }
}

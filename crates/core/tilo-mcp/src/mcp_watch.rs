//! `tilo_watch`: what happened since the caller last asked — changes to a
//! resource, or to every record of the workspace, this session's invocations
//! starting and finishing, and who is in the workspace — held open until
//! something did, so a program follows the workspace without polling a
//! second endpoint.
//!
//! The sources are the ones the desktop and the browser read: the resource's
//! own history verb for a change and who made it (the host's feed of every
//! record's changes, [`tilo_actions::remote::ChangeFeed`], for a watch of no
//! path), the host's observation of an invocation, and the host's roster for
//! presence.
use super::*;
use tilo_actions::remote::WatchScope;

/// A source a watch follows that the host refused to tell of: which, why, and whether a watch
/// said so yet.
#[derive(Debug)]
pub(crate) struct Unfollowed {
    scope: WatchScope,
    reason: String,
    told: bool,
}

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

/// What a watch has already told its caller: the last invocation event, the
/// workspace's roster as it last read it, and the last change of the watched
/// path. Opaque to the caller, which hands it back as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Cursor {
    /// The latest moment, in ms since the epoch, an invocation of the caller's started or
    /// ended at as the invocations record told it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation: Option<i64>,
    /// The caller's invocations still going when last told: their end wakes the watch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    going: Vec<String>,
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

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// How invocation `id` stands, as `tilo_status` answers it.
    pub(crate) async fn status(&self, id: tilo_actions::InvocationId) -> Result<Value, String> {
        let state = self.observed(id).await;
        self.saw(id, state.as_ref());
        state
            .as_ref()
            .map(|state| crate::state_json(id, state))
            .ok_or_else(|| crate::UNOWNED.into())
    }

    /// The host's last state of invocation `id`, asked off this task's
    /// thread: a host may ask the device running it and wait for the answer
    /// (a session's actions do), and a wait on this thread would stall the
    /// connection that carries that answer until it gave up. What the look
    /// cost is told on this thread, where a tool's own accounting is.
    pub(crate) async fn observed(
        &self,
        id: tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        let caller = self.caller();
        let state = tilo_actions::remote::observed_off_the_worker(
            std::sync::Arc::clone(&self.host),
            caller.clone(),
            id,
        )
        .await;
        self.host.observed(&caller, &id, state.as_ref());
        state
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
        let mut listening = self.watch_changed(&cursor);
        loop {
            let mut events = Vec::new();
            self.watch_unfollowed(&mut events);
            self.watch_states(&mut cursor, &mut events);
            self.watch_invocations(&mut cursor, &mut events).await;
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
                listening = self.watch_changed(&cursor);
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
    /// stand, the workspace's records, who is in it, each of the caller's invocations still going
    /// as `cursor` last told them, and the calls of this session itself. A host that cannot tell
    /// of a source leaves it out, and that source then costs the watch the rest of its wait.
    fn watch_changed(&self, cursor: &Cursor) -> Option<tilo_actions::remote::Pending<()>> {
        let caller = self.caller();
        let mut changed: Vec<tilo_actions::remote::Pending<()>> = Vec::new();
        changed.extend(self.host.session_state_changed(&caller));
        for scope in [WatchScope::Records, WatchScope::Here] {
            if self.unfollows(scope) {
                continue;
            }
            if let Some(wait) = self.host.changed(&caller, scope) {
                changed.push(self.followed(scope, wait));
            }
        }
        for invocation in &cursor.going {
            let Ok(id) = invocation_id(invocation) else {
                continue;
            };
            // The host's own signal for an invocation, which it answers when the invocation may
            // have moved.
            changed.extend(self.host.next_change(&caller, &id));
        }
        changed.push(self.invocations_changed());
        first_wake(changed)
    }

    /// `wait`, the host's signal that what `scope` follows may have changed, as a wake of this
    /// watch. A wait the host refuses wakes the watch too: the refusal is told once, as an
    /// `unwatched` event with why, and the source is not asked again, so the watch neither waits
    /// on a signal that never comes nor asks again and again for one the host will not give.
    fn followed(
        &self,
        scope: WatchScope,
        wait: tilo_actions::remote::Pending<Result<(), tilo_actions::remote::ActionCallError>>,
    ) -> tilo_actions::remote::Pending<()> {
        let unfollowed = std::sync::Arc::clone(&self.unfollowed);
        wait.map(move |answer| {
            if let Err(error) = answer {
                unfollowed.lock_recover().push(Unfollowed {
                    scope,
                    reason: error.to_string(),
                    told: false,
                });
            }
        })
    }

    /// Whether the host refused to tell when what `scope` follows changes.
    fn unfollows(&self, scope: WatchScope) -> bool {
        self.unfollowed
            .lock_recover()
            .iter()
            .any(|unfollowed| unfollowed.scope == scope)
    }

    /// The sources the host refused to follow since a watch last said so, each as an
    /// `unwatched` event: `records` or `presence`, and why.
    fn watch_unfollowed(&self, events: &mut Vec<Value>) {
        for unfollowed in self.unfollowed.lock_recover().iter_mut() {
            if std::mem::replace(&mut unfollowed.told, true) {
                continue;
            }
            let source = match unfollowed.scope {
                WatchScope::Records => "records",
                WatchScope::Here => "presence",
            };
            events.push(json!({
                "kind": "unwatched",
                "source": source,
                "reason": unfollowed.reason,
            }));
        }
    }

    /// Resolves on the next call of this session starting, being held for its person, or ending
    /// ([`Self::invocations_moved`]).
    fn invocations_changed(&self) -> tilo_actions::remote::Pending<()> {
        let (changed, change) = tilo_actions::remote::pending();
        let mut waiting = self.invocation_waiters.lock_recover();
        // A wait another source woke the watch from tells nobody: forgotten here rather than kept
        // until the next invocation.
        waiting.retain(|woken| !woken.is_abandoned());
        waiting.push(changed);
        change
    }

    /// Wake every watch of this session: a call of it started, was held for its person, or
    /// ended, which the invocations record now says.
    pub(super) fn invocations_moved(&self) {
        let waiting = std::mem::take(&mut *self.invocation_waiters.lock_recover());
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
    /// yet is news to it. Who is here is not something that happened: the
    /// first read of a watch says who is here now, so a page that opens on a
    /// workspace draws its people and agents at once.
    async fn watch_now(&self, path: Option<&str>) -> Result<Cursor, String> {
        let (invocation, going) = match self.own_invocations().await {
            Ok(own) => (
                own.iter().map(moment).max(),
                own.iter().filter(|item| going(item)).filter_map(|item| item.id.clone()).collect(),
            ),
            Err(_) => (None, Vec::new()),
        };
        let feed = match path_named(path) {
            Some(_) => None,
            None => self.feed_page(None).await?.map(|page| page.cursor),
        };
        Ok(Cursor {
            invocation,
            going,
            here: None,
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

    /// The caller's invocations that started or ended since the cursor, as the workspace's
    /// invocations record tells them: the one list of calls, which every projection reads. A
    /// call that ended well and changed nothing (a read, the watch's own among them) is no news.
    async fn watch_invocations(&self, cursor: &mut Cursor, events: &mut Vec<Value>) {
        use tilo_actions::remote::workspace_invocations::InvocationStatus;
        let Ok(own) = self.own_invocations().await else {
            return;
        };
        let told = cursor.invocation;
        cursor.going = own
            .iter()
            .filter(|item| going(item))
            .filter_map(|item| item.id.clone())
            .collect();
        let mut news: Vec<&tilo_actions::remote::workspace_invocations::Invocation> = own
            .iter()
            .filter(|item| told.is_none_or(|told| moment(item) > told))
            .filter(|item| item.changes > 0 || item.status != InvocationStatus::Ended)
            .collect();
        news.sort_by_key(|item| moment(item));
        if let Some(last) = news.last() {
            cursor.invocation = Some(moment(last));
        }
        for item in news {
            events.push(json!({
                "kind": "invocation",
                "phase": phase(item.status),
                "invocation_id": item.id,
                "path": item.action,
                "title": item.title.clone().unwrap_or_else(|| item.by.name.clone()),
                "time_ms": item.started_ms,
            }));
        }
    }

    /// The invocations the caller made, newest first, as the workspace's invocations record
    /// lists them.
    pub(super) async fn own_invocations(
        &self,
    ) -> Result<Vec<tilo_actions::remote::workspace_invocations::Invocation>, String> {
        use tilo_actions::remote::workspace_invocations::{InvocationsQuery, PAGE};
        let acting = self.caller.acting();
        let by = acting
            .agent_id()
            .or_else(|| acting.member_id().map(collab_types::MemberId::id))
            .map(str::to_owned);
        Ok(self
            .listed_invocations(&InvocationsQuery {
                limit: PAGE,
                by,
                ..InvocationsQuery::default()
            })
            .await?
            .items)
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
            let mut event = change_event(path.unwrap_or_default(), Some(&app), None, entry)?;
            if let Some(event) = event.as_object_mut() {
                event.insert("change".to_owned(), json!(change));
            }
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
            )?;
            events.push(event);
        }
        cursor.feed = Some(page.cursor);
        Ok(())
    }

    /// One page of the host's feed of the workspace's changes after `after`:
    /// `None` for a host that keeps none, or answers this caller none, which a
    /// watch of no path then reports nothing of.
    async fn feed_page(
        &self,
        after: Option<&str>,
    ) -> Result<Option<tilo_actions::remote::ChangePage>, String> {
        let Some(feed) = self.host.change_feed() else {
            return Ok(None);
        };
        match feed.since(&self.caller(), after).await {
            Some(Ok(page)) => Ok(Some(page)),
            // A host that keeps no feed says so; a watch still reports
            // invocations and presence.
            Some(Err(tilo_actions::remote::ActionCallError::Unavailable { reason }))
                if reason == tilo_actions::remote::CHANGE_FEED_UNAVAILABLE =>
            {
                Ok(None)
            }
            // A caller the feed does not answer (an agent, whose rules a change to a whole
            // record does not say it may read) still hears its invocations and who is here.
            Some(Err(tilo_actions::remote::ActionCallError::NotPermitted)) => Ok(None),
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
        let parsed = Address::parse(path)?;
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
            crate::Hold::For(std::time::Duration::from_millis(5_000)),
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
            .map(|here| {
                json!({"who": here.who, "name": here.name, "kind": here.kind,
                    "devices": here.devices})
            })
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
/// it, what it did to the resource (`what`: created, edited or removed, as the
/// store read it from the change), and the group it is a step of. The group's
/// title is under `title`, its id under `group`, and the invocation under
/// `invocation`, as the resource's own history names them.
///
/// # Errors
/// The change names no `what`: a change that does not say what it did to its
/// resource is a fault in what answered it, and is never read as an edit.
fn change_event(
    path: &str,
    app: Option<&str>,
    record: Option<&str>,
    entry: &Value,
) -> Result<Value, String> {
    let mut resource = serde_json::Map::new();
    resource.insert("path".into(), json!(path));
    for (key, value) in [("app", app), ("record", record)] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            resource.insert(key.into(), json!(value));
        }
    }
    if let Some(item) = entry
        .get("item")
        .and_then(Value::as_str)
        .or_else(|| entry.get("resource").and_then(Value::as_str))
    {
        resource.insert("item".into(), json!(item));
    }
    let what = entry
        .get("happened")
        .and_then(Value::as_str)
        .and_then(tilo_actions::remote::What::of_word)
        .ok_or_else(|| {
            format!(
                "the change {} of {path} names no what it did to its resource",
                entry
                    .get("change")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            )
        })?;
    let mut event = json!({
        "kind": "change",
        "path": path,
        "resources": [Value::Object(resource)],
        "change": entry.get("change").cloned().unwrap_or_default(),
        "what": what.word(),
        "sentence": entry.get("sentence").cloned().unwrap_or_default(),
        "time_ms": entry.get("time_ms").cloned().unwrap_or_default(),
    });
    if let Some(fields) = event.as_object_mut() {
        for (key, value) in [("app", app), ("record", record)] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                fields.insert(key.to_owned(), json!(value));
            }
        }
    }
    if let (Some(fields), Some(entry)) = (event.as_object_mut(), entry.as_object()) {
        for key in [
            "by",
            "verb",
            "title",
            "group",
            "group_kind",
            "invocation",
            "undone",
        ] {
            if let Some(value) = entry.get(key) {
                fields.insert(key.to_owned(), value.clone());
            }
        }
        if let Some(actor) = actor_of(entry) {
            fields.insert("actor".into(), actor);
        }
    }
    Ok(event)
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
    let grouped = entry
        .get("group")
        .and_then(Value::as_str)
        .is_some_and(|group| !group.is_empty());
    Some(json!({"kind": if grouped { "agent" } else { "person" }, "name": name}))
}

/// The entries of a `history` verb's answer `value`, oldest first: its
/// `versions`, as the history verbs answer, each whole — what `history`
/// leaves out of an entry as the one before's (`title`, `group`, `by`,
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
                    for key in ["title", "group", "by", "time_ms"] {
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

/// The change id a history page ends at.
fn last_change(page: Option<&[Value]>) -> Option<String> {
    page?
        .last()?
        .get("change")
        .and_then(Value::as_str)
        .map(str::to_owned)
}


/// The latest moment `item` started or ended at, in ms since the epoch.
fn moment(item: &tilo_actions::remote::workspace_invocations::Invocation) -> i64 {
    item.ended_ms.unwrap_or(item.started_ms).max(item.started_ms)
}

/// Whether `item` has not ended yet: it runs, or waits for its person.
const fn going(item: &tilo_actions::remote::workspace_invocations::Invocation) -> bool {
    use tilo_actions::remote::workspace_invocations::InvocationStatus;
    matches!(item.status, InvocationStatus::Running | InvocationStatus::Waiting)
}

/// The phase a watch names `status` by.
pub(super) const fn phase(status: tilo_actions::remote::workspace_invocations::InvocationStatus) -> &'static str {
    use tilo_actions::remote::workspace_invocations::InvocationStatus;
    match status {
        InvocationStatus::Waiting => "held_for_person",
        InvocationStatus::Running => "started",
        InvocationStatus::Ended => "succeeded",
        InvocationStatus::Failed => "failed",
        InvocationStatus::Cancelled => "cancelled",
        InvocationStatus::Undone => "undone",
    }
}

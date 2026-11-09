//! `tilo_watch`: what happened since the caller last asked — changes to a
//! resource, this session's runs starting and finishing, and who is in the
//! workspace — held open until something did, so a program follows the
//! workspace without polling a second endpoint.
//!
//! The sources are the ones the desktop and the browser read: the resource's
//! own history verb for a change and who made it, the host's observation of
//! an invocation for a run, and the host's roster for presence.
use super::*;
use crate::mcp::step::STEP_TITLE;

/// One invocation of this session, as `tilo_watch` follows it: what it ran,
/// the run it is a step of, and how it stands.
#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub(crate) invocation: String,
    /// The path the call named, when it named one.
    pub(crate) path: String,
    /// The run it is a step of, as people read it, and its id.
    pub(crate) name: String,
    pub(crate) run_id: String,
    pub(crate) started_ms: u64,
    /// Its phase as last told, and the sequence number it was told at: a
    /// cursor before `seq` is told `phase`.
    pub(crate) phase: String,
    pub(crate) seq: u64,
}

/// How long a watch with no `wait_ms` holds before it answers with what it
/// has.
const DEFAULT_WATCH_MS: u64 = 25_000;
/// The longest a watch holds, so a caller's connection is never held longer
/// than it named.
const MAX_WATCH_MS: u64 = 60_000;
/// How often a held watch looks again: at the invocation the host may have
/// moved on, the resource's history, and the roster.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(200);
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
}

impl Cursor {
    /// A cursor as the caller hands it back: what an earlier answer said.
    fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|_| "cursor is not one tilo_watch issued".to_owned())
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Note that `invocation` started, as a watch of this session follows it.
    pub(super) fn watch_run_started(&self, invocation: &str, path: &str) {
        let (name, run_id) = self.run_named();
        let mut runs = self.runs.lock_recover();
        let seq = next_seq(&runs);
        runs.push(Run {
            invocation: invocation.to_owned(),
            path: path.to_owned(),
            name,
            run_id,
            started_ms: tilo_clock::unix_now_ms(),
            phase: "started".to_owned(),
            seq,
        });
    }

    /// Note that `invocation` reached `phase`, which is terminal.
    pub(super) fn watch_run_ended(&self, invocation: &str, phase: &str) {
        let mut runs = self.runs.lock_recover();
        let seq = next_seq(&runs);
        if let Some(run) = runs
            .iter_mut()
            .rev()
            .find(|run| run.invocation == invocation)
        {
            run.phase = phase.to_owned();
            run.seq = seq;
        }
    }

    /// The run a watch attributes this session's calls to: the title a call
    /// grouped under, else the connection's own run.
    fn run_named(&self) -> (String, String) {
        let run = self.agent_run();
        let id = run.as_ref().map(|run| run.id.clone()).unwrap_or_default();
        let label = match STEP_TITLE.try_with(Clone::clone).ok().flatten() {
            Some(title) if !title.is_empty() => title,
            _ => run.map(|run| run.label).unwrap_or_default(),
        };
        (label, id)
    }

    /// [`WatchArgs`] as one answer: what happened since `cursor`, or nothing
    /// with a cursor to ask again from.
    pub(super) async fn agent_watch(&self, args: WatchArgs) -> Result<Reply, String> {
        let wait = args.wait_ms.unwrap_or(DEFAULT_WATCH_MS).min(MAX_WATCH_MS);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait);
        let path = args.path.as_deref();
        let mut cursor = match args.cursor.as_deref() {
            None | Some("") => self.watch_now(path).await?,
            Some(text) => Cursor::parse(text)?,
        };
        loop {
            let mut events = Vec::new();
            self.watch_runs(&mut cursor, &mut events);
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
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// The cursor a caller with none starts from: nothing that has happened
    /// yet is news to it.
    async fn watch_now(&self, path: Option<&str>) -> Result<Cursor, String> {
        let run = self.runs.lock_recover().iter().map(|run| run.seq).max();
        Ok(Cursor {
            run,
            here: self.here_now().map(|(fingerprint, _)| fingerprint),
            change: last_change(self.history_page(path).await?.as_deref()),
        })
    }

    /// The runs this session started since the cursor, as the host says they
    /// stand now.
    fn watch_runs(&self, cursor: &mut Cursor, events: &mut Vec<Value>) {
        let seen = cursor.run.unwrap_or_default();

        // How the runs still going stand: an invocation that finished after
        // its call answered is reported finished here.
        let moved: Vec<(String, String)> = self
            .runs
            .lock_recover()
            .iter()
            .filter(|run| !is_terminal(&run.phase))
            .filter_map(|run| {
                let id = invocation_id(&run.invocation).ok()?;
                let value = self.status(id).ok()?;
                let state = value.pointer("/update/state").and_then(Value::as_str)?;
                is_terminal(state).then(|| (run.invocation.clone(), state.to_owned()))
            })
            .collect();
        let announced: Vec<Run> = {
            let mut runs = self.runs.lock_recover();
            for (invocation, phase) in moved {
                let seq = next_seq(&runs);
                if let Some(run) = runs
                    .iter_mut()
                    .rev()
                    .find(|run| run.invocation == invocation)
                {
                    run.phase = phase;
                    run.seq = seq;
                }
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
                "run_id": run.run_id,
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
        let page = self.history_page(path).await?;
        let Some(page) = page else {
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
            let mut event = json!({
                "kind": "change",
                "path": path.unwrap_or_default(),
                "change": change,
                "sentence": entry.get("sentence").cloned().unwrap_or_default(),
                "time_ms": entry.get("time_ms").cloned().unwrap_or_default(),
            });
            for key in ["by", "run", "run_id", "undone"] {
                if let Some(value) = entry.get(key) {
                    event[key] = value.clone();
                }
            }
            events.push(event);
        }
        cursor.change = last_change(Some(&page)).or(seen);
        Ok(())
    }

    /// The watched path's history, oldest first, as its own `history` verb
    /// answers it: `None` for a watch of no path, or of a resource with no
    /// history to read.
    async fn history_page(&self, path: Option<&str>) -> Result<Option<Vec<Value>>, String> {
        let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) else {
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
        Ok(value
            .pointer("/update/output/value/history")
            .or_else(|| value.pointer("/update/output/value"))
            .and_then(Value::as_array)
            .cloned())
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
    runs.iter().map(|run| run.seq).max().unwrap_or_default() + 1
}

/// Whether `phase` is one an invocation never leaves.
fn is_terminal(phase: &str) -> bool {
    matches!(phase, "succeeded" | "failed" | "cancelled")
}

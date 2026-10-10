//! Loading and unloading extensions in the running engine, and reading their
//! toolbar state, over the engine's DevTools bridge.
//!
//! CEF's Chrome runtime has no extension API, but Chromium's `Extensions`
//! DevTools domain loads and unloads an unpacked extension with no restart
//! (`Extensions.loadUnpacked` returns the identifier, `Extensions.uninstall`
//! removes it; both are idempotent for our purposes). Everything is
//! asynchronous: [`ExtensionHost`] queues requests, the caller sends them
//! with [`ExtensionHost::flush`], and each reply comes back through
//! [`ExtensionHost::on_result`]. It holds no engine, so it is testable alone.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use serde_json::{Value, json};
use ui::webview::{SharedEngine, WebViewCommand};

use crate::extension_toolbar as toolbar;

/// How often the toolbar re-reads action badges.
pub const PROBE_INTERVAL: ui::time::Duration = ui::time::Duration::from_secs(4);
/// A reply that never comes (the engine restarted) must not wedge the queue.
const STALE_AFTER: ui::time::Duration = ui::time::Duration::from_secs(20);

/// An extension the person wants running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    /// The identifier Chromium will give it.
    pub id: String,
    pub path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Load(Wanted),
    Unload(String),
    Targets,
    Attach { extension: String, target: String },
    Evaluate { extension: String, session: String },
    Detach,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u32,
    pub method: &'static str,
    pub params: Value,
    pub session: Option<String>,
}

/// What a reply changed that the interface shows.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// The set of running extensions or an action's state changed.
    pub changed: bool,
    /// A failure to tell the person about.
    pub notice: Option<String>,
}

#[derive(Default)]
pub struct ExtensionHost {
    next: u32,
    pending: HashMap<u32, (Step, ui::time::Instant)>,
    outbox: Vec<Request>,
    /// Identifiers Chromium confirmed running.
    loaded: BTreeSet<String>,
    /// What the last `reconcile` asked for, so an unchanged call sends nothing.
    asked: BTreeMap<String, bool>,
    state: HashMap<String, toolbar::ActionState>,
    /// Running extensions the current probe was started for.
    candidates: Vec<String>,
    /// (extension, service-worker target) pairs the probe still has to read.
    probe: VecDeque<(String, String)>,
    probing: bool,
    last_probe: Option<ui::time::Instant>,
}

impl ExtensionHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether Chromium confirmed `id` running.
    pub fn is_loaded(&self, id: &str) -> bool {
        self.loaded.contains(id)
    }

    /// The live action state of a running extension.
    pub fn action_state(&self, id: &str) -> Option<toolbar::ActionState> {
        self.state.get(id).cloned()
    }

    fn request(&mut self, step: Step, now: ui::time::Instant) -> u32 {
        self.next = self.next % (i32::MAX as u32 - 1) + 1;
        let id = self.next;
        let (method, params, session) = match &step {
            Step::Load(wanted) => (
                "Extensions.loadUnpacked",
                json!({"path": wanted.path.to_string_lossy()}),
                None,
            ),
            Step::Unload(id) => ("Extensions.uninstall", json!({"id": id}), None),
            Step::Targets => {
                let (method, params) = toolbar::targets_request();
                (method, params, None)
            }
            Step::Attach { target, .. } => {
                let (method, params) = toolbar::attach_request(target);
                (method, params, None)
            }
            Step::Evaluate { session, .. } => {
                let (method, params) = toolbar::evaluate_request();
                (method, params, Some(session.clone()))
            }
            Step::Detach => unreachable!("detach carries its session in `detach`"),
        };
        self.outbox.push(Request {
            id,
            method,
            params,
            session,
        });
        self.pending.insert(id, (step, now));
        id
    }

    fn detach(&mut self, session: &str, now: ui::time::Instant) {
        self.next = self.next % (i32::MAX as u32 - 1) + 1;
        let (method, params) = toolbar::detach_request(session);
        self.outbox.push(Request {
            id: self.next,
            method,
            params,
            session: None,
        });
        self.pending.insert(self.next, (Step::Detach, now));
    }

    /// Make the engine run exactly `wanted`, and stop `unwanted`: extensions
    /// that are installed but disabled, which Chromium may remember from an
    /// earlier run. Unchanged requests are not repeated.
    pub fn reconcile(&mut self, wanted: &[Wanted], unwanted: &[String], now: ui::time::Instant) {
        for extension in wanted {
            if self.asked.insert(extension.id.clone(), true) != Some(true) {
                self.request(Step::Load(extension.clone()), now);
            }
        }
        for id in unwanted {
            if self.asked.insert(id.clone(), false) != Some(false) {
                self.request(Step::Unload(id.clone()), now);
            }
        }
    }

    /// An extension was removed from disk: forget it.
    pub fn forget(&mut self, id: &str) {
        self.asked.remove(id);
        self.loaded.remove(id);
        self.state.remove(id);
    }

    /// Start a toolbar refresh for `extensions` when one is due.
    pub fn probe(&mut self, extensions: &[String], now: ui::time::Instant) {
        self.pending
            .retain(|_, (_, sent)| now.saturating_duration_since(*sent) < STALE_AFTER);
        if self.probing && self.pending.is_empty() {
            self.probing = false;
        }
        let due = self
            .last_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= PROBE_INTERVAL);
        let loaded: Vec<String> = extensions
            .iter()
            .filter(|id| self.loaded.contains(*id))
            .cloned()
            .collect();
        if self.probing || !due || loaded.is_empty() {
            return;
        }
        self.last_probe = Some(now);
        self.probing = true;
        self.candidates = loaded;
        self.probe.clear();
        self.request(Step::Targets, now);
    }

    /// Handle a reply to a request the host sent.
    pub fn on_result(
        &mut self,
        request: u32,
        ok: bool,
        body: &str,
        now: ui::time::Instant,
    ) -> Update {
        let Some((step, _)) = self.pending.remove(&request) else {
            return Update::default();
        };
        let mut update = Update::default();
        match step {
            Step::Load(wanted) => {
                if let Some(id) = ok.then(|| toolbar::loaded_id(body)).flatten() {
                    update.changed = self.loaded.insert(id);
                } else {
                    // A failed load is not asked again until something changes.
                    self.asked.remove(&wanted.id);
                    update.notice = Some(format!(
                        "{} could not be turned on: {}",
                        wanted.name,
                        failure(body)
                    ));
                }
            }
            Step::Unload(id) => {
                // "Could not find extension" is the normal answer for one
                // that was never running; either way it is not running now.
                update.changed = self.loaded.remove(&id);
                self.state.remove(&id);
            }
            Step::Targets => {
                if ok {
                    self.probe = self
                        .candidates
                        .iter()
                        .filter_map(|id| Some((id.clone(), toolbar::worker_target(body, id)?)))
                        .collect();
                }
                self.next_probe(now);
            }
            Step::Attach { extension, .. } => {
                if let Some(session) = ok.then(|| toolbar::session_id(body)).flatten() {
                    self.request(Step::Evaluate { extension, session }, now);
                } else {
                    self.next_probe(now);
                }
            }
            Step::Evaluate { extension, session } => {
                if let Some(state) = ok.then(|| toolbar::action_state(body)).flatten()
                    && self.state.get(&extension) != Some(&state)
                {
                    self.state.insert(extension, state);
                    update.changed = true;
                }
                self.detach(&session, now);
                self.next_probe(now);
            }
            Step::Detach => {}
        }
        update
    }

    fn next_probe(&mut self, now: ui::time::Instant) {
        if let Some((extension, target)) = self.probe.pop_front() {
            self.request(Step::Attach { extension, target }, now);
        } else {
            self.probing = false;
        }
    }

    /// Send what is queued to `engine`.
    pub fn flush(&mut self, engine: &SharedEngine) {
        for request in self.outbox.drain(..) {
            engine.send(WebViewCommand::DevTools {
                request_id: request.id,
                method: request.method.to_owned(),
                params: request.params,
                session: request.session,
            });
        }
    }

    /// Queued requests, for tests.
    #[cfg(test)]
    pub fn take_outbox(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
    }
}

/// A reply body's message: Chromium's `error.message` or the text itself.
fn failure(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("message")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(id: &str) -> Wanted {
        Wanted {
            id: id.into(),
            path: PathBuf::from(format!("/profile/extensions/{id}")),
            name: format!("Extension {id}"),
        }
    }

    fn id(letter: char) -> String {
        letter.to_string().repeat(32)
    }

    #[test]
    fn reconcile_loads_and_unloads_once_and_tracks_replies() {
        let now = ui::time::now();
        let mut host = ExtensionHost::new();
        host.reconcile(&[wanted(&id('a'))], &[id('b')], now);
        let sent = host.take_outbox();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].method, "Extensions.loadUnpacked");
        assert_eq!(
            sent[0].params["path"],
            format!("/profile/extensions/{}", id('a'))
        );
        assert_eq!(sent[1].method, "Extensions.uninstall");
        assert_eq!(sent[1].params["id"], id('b'));

        // Unchanged intent sends nothing; a flip sends the opposite.
        host.reconcile(&[wanted(&id('a'))], &[id('b')], now);
        assert!(host.take_outbox().is_empty());

        // phase: replies_update_the_running_set
        let loaded = json!({"id": id('a')}).to_string();
        let update = host.on_result(sent[0].id, true, &loaded, now);
        assert!(update.changed && update.notice.is_none());
        assert!(host.is_loaded(&id('a')));
        // An unload of something that never ran is not a failure.
        let update = host.on_result(sent[1].id, false, r#"{"message":"could not find"}"#, now);
        assert_eq!(update, Update::default());

        // phase: disabling_unloads_and_enabling_reloads
        host.reconcile(&[], &[id('a')], now);
        let unload = host.take_outbox();
        assert_eq!(unload[0].method, "Extensions.uninstall");
        assert!(host.on_result(unload[0].id, true, "{}", now).changed);
        assert!(!host.is_loaded(&id('a')));
        host.reconcile(&[wanted(&id('a'))], &[], now);
        assert_eq!(host.take_outbox()[0].method, "Extensions.loadUnpacked");

        // phase: failed_load_reports_and_can_be_retried
        let failed = host.take_outbox();
        assert!(failed.is_empty());
        let mut host = ExtensionHost::new();
        host.reconcile(&[wanted(&id('c'))], &[], now);
        let sent = host.take_outbox();
        let update = host.on_result(
            sent[0].id,
            false,
            r#"{"code":-32000,"message":"Manifest file is missing"}"#,
            now,
        );
        assert!(!update.changed);
        assert!(update.notice.unwrap().contains("Manifest file is missing"));
        host.reconcile(&[wanted(&id('c'))], &[], now);
        assert_eq!(host.take_outbox().len(), 1, "asked again after a failure");

        // phase: unknown_replies_are_ignored
        assert_eq!(host.on_result(999, true, "{}", now), Update::default());
    }

    #[test]
    fn badge_probe_walks_targets_attach_evaluate_detach() {
        let now = ui::time::now();
        let mut host = ExtensionHost::new();
        host.reconcile(&[wanted(&id('a')), wanted(&id('b'))], &[], now);
        for request in host.take_outbox() {
            let loaded =
                json!({"id": request.params["path"].as_str().unwrap().rsplit('/').next().unwrap()});
            host.on_result(request.id, true, &loaded.to_string(), now);
        }
        let ids = vec![id('a'), id('b'), id('c')];

        // phase: probe_starts_once_per_interval
        host.probe(&ids, now);
        let sent = host.take_outbox();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, "Target.getTargets");
        host.probe(&ids, now + ui::time::Duration::from_secs(1));
        assert!(host.take_outbox().is_empty(), "one probe at a time");

        // phase: only_extensions_with_a_running_worker_are_read
        let targets = json!({"targetInfos":[
            {"targetId":"W-A","type":"service_worker","url":format!("chrome-extension://{}/sw.js", id('a'))},
        ]})
        .to_string();
        host.on_result(sent[0].id, true, &targets, now);
        let attach = host.take_outbox();
        assert_eq!(attach[0].method, "Target.attachToTarget");
        assert_eq!(attach[0].params["targetId"], "W-A");
        host.on_result(attach[0].id, true, r#"{"sessionId":"S1"}"#, now);
        let evaluate = host.take_outbox();
        assert_eq!(evaluate[0].method, "Runtime.evaluate");
        assert_eq!(evaluate[0].session.as_deref(), Some("S1"));
        let value =
            json!({"result":{"value": json!({"text":"7","title":"7 blocked"}).to_string()}});
        let update = host.on_result(evaluate[0].id, true, &value.to_string(), now);
        assert!(update.changed);
        let detach = host.take_outbox();
        assert_eq!(detach[0].method, "Target.detachFromTarget");
        assert_eq!(detach[0].params["sessionId"], "S1");
        assert_eq!(host.action_state(&id('a')).unwrap().badge, "7");
        assert_eq!(host.action_state(&id('b')), None);

        // phase: an_unchanged_reading_is_not_a_change_and_the_next_probe_is_due_later
        let later = now + PROBE_INTERVAL;
        host.probe(&ids, later);
        let sent = host.take_outbox();
        assert_eq!(sent[0].method, "Target.getTargets");
        host.on_result(sent[0].id, true, &targets, later);
        let attach = host.take_outbox();
        host.on_result(attach[0].id, true, r#"{"sessionId":"S2"}"#, later);
        let evaluate = host.take_outbox();
        assert!(
            !host
                .on_result(evaluate[0].id, true, &value.to_string(), later)
                .changed
        );

        // phase: a_lost_reply_does_not_wedge_the_probe
        let much_later = later + STALE_AFTER + PROBE_INTERVAL;
        host.take_outbox();
        host.probe(&ids, much_later);
        assert_eq!(host.take_outbox()[0].method, "Target.getTargets");
    }
}

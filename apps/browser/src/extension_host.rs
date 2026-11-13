//! Loading and unloading extensions in the running engine, and reading their toolbar
//! state, over the engine's `DevTools` bridge.
//!
//! Chromium's `Extensions` domain loads and unloads an unpacked extension with no
//! restart. Everything is asynchronous: [`ExtensionHost`] queues requests, the caller
//! sends them with [`ExtensionHost::flush`], and each reply comes back through
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
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
    Detach { session: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u32,
    pub method: &'static str,
    pub params: Value,
    pub session: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Update {
    pub changed: bool,
    pub notice: Option<String>,
}

#[derive(Default)]
pub struct ExtensionHost {
    next: u32,
    pending: HashMap<u32, (Step, ui::time::Instant)>,
    outbox: Vec<Request>,
    loaded: BTreeSet<String>,
    asked: BTreeMap<String, bool>,
    state: HashMap<String, toolbar::ActionState>,
    page: HashMap<String, toolbar::PageValues>,
    candidates: Vec<String>,
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

    pub fn action_state(&self, id: &str) -> Option<toolbar::ActionState> {
        let state = self.state.get(id).cloned();
        match self.page.get(id) {
            Some(page) => Some(state.unwrap_or_default().under(page)),
            None => state,
        }
    }

    /// The extension changed its button, for every tab (`global`) or for this window's page
    /// alone. Whether what the button shows changed.
    pub fn action_changed(
        &mut self,
        id: &str,
        global: bool,
        change: &ui::webview::ExtensionActionChange,
    ) -> bool {
        let before = self.action_state(id);
        if global {
            self.state.entry(id.to_owned()).or_default().apply(change);
        } else {
            self.page.entry(id.to_owned()).or_default().apply(change);
        }
        self.action_state(id) != before
    }

    /// A new document in this window: what extensions set for the page before is gone.
    /// Whether there was any.
    pub fn page_changed(&mut self) -> bool {
        let had = !self.page.is_empty();
        self.page.clear();
        had
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
            Step::Detach { session } => {
                let (method, params) = toolbar::detach_request(session);
                (method, params, None)
            }
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
        self.request(
            Step::Detach {
                session: session.to_owned(),
            },
            now,
        );
    }

    /// Make the engine run exactly `wanted`, and stop `unwanted`: installed but disabled
    /// extensions, which Chromium may remember from an earlier run. Unchanged requests are
    /// not repeated.
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

    pub fn forget(&mut self, id: &str) {
        self.asked.remove(id);
        self.loaded.remove(id);
        self.state.remove(id);
        self.page.remove(id);
    }

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
                    // The engine's reason is for the log; the person is told what to do.
                    tracing::warn!(
                        extension = %wanted.name,
                        reason = %failure(body),
                        "the engine did not load an extension"
                    );
                    update.notice = Some(crate::CATALOG.t_args(
                        "browser-extension-load-failed",
                        &[("name", ui::i18n::ArgValue::Text(wanted.name.clone()))],
                    ));
                }
            }
            Step::Unload(id) => {
                // "Could not find extension" is the normal answer for one that was never
                // running; either way it is not running now.
                update.changed = self.loaded.remove(&id);
                self.state.remove(&id);
                self.page.remove(&id);
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
            Step::Detach { .. } => {}
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

        host.reconcile(&[wanted(&id('a'))], &[id('b')], now);
        assert!(host.take_outbox().is_empty());

        let loaded = json!({"id": id('a')}).to_string();
        let update = host.on_result(sent[0].id, true, &loaded, now);
        assert!(update.changed && update.notice.is_none());
        assert!(host.is_loaded(&id('a')));
        let update = host.on_result(sent[1].id, false, r#"{"message":"could not find"}"#, now);
        assert_eq!(update, Update::default());

        host.reconcile(&[], &[id('a')], now);
        let unload = host.take_outbox();
        assert_eq!(unload[0].method, "Extensions.uninstall");
        assert!(host.on_result(unload[0].id, true, "{}", now).changed);
        assert!(!host.is_loaded(&id('a')));
        host.reconcile(&[wanted(&id('a'))], &[], now);
        assert_eq!(host.take_outbox()[0].method, "Extensions.loadUnpacked");

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
        assert_eq!(
            update.notice.as_deref(),
            Some(
                format!(
                    "Extension {} could not be turned on. Remove it and add it again.",
                    id('c')
                )
                .as_str()
            ),
            "the person is told what to do, not the engine's reason"
        );
        host.reconcile(&[wanted(&id('c'))], &[], now);
        assert_eq!(host.take_outbox().len(), 1, "asked again after a failure");

        assert_eq!(host.on_result(999, true, "{}", now), Update::default());
    }

    #[test]
    fn a_page_shows_its_own_badge_until_its_next_document() {
        use ui::webview::ExtensionActionChange as Change;
        let mut host = ExtensionHost::new();
        let a = id('a');
        let badge = |text: &str| Change {
            badge: Some(Some(text.into())),
            ..Change::default()
        };
        assert!(host.action_changed(&a, true, &badge("on")));
        assert!(host.action_changed(&a, false, &badge("7")));
        assert_eq!(host.action_state(&a).unwrap().badge, "7");
        assert!(
            !host.action_changed(&a, false, &badge("7")),
            "the same again"
        );
        assert!(!host.action_changed(&a, true, &badge("off")));
        assert!(host.page_changed());
        assert_eq!(host.action_state(&a).unwrap().badge, "off");
        assert!(!host.page_changed(), "nothing left to take back");
        host.action_changed(&a, false, &badge("9"));
        host.forget(&a);
        assert_eq!(host.action_state(&a), None);
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

        host.probe(&ids, now);
        let sent = host.take_outbox();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, "Target.getTargets");
        host.probe(&ids, now + ui::time::Duration::from_secs(1));
        assert!(host.take_outbox().is_empty(), "one probe at a time");

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

        let much_later = later + STALE_AFTER + PROBE_INTERVAL;
        host.take_outbox();
        host.probe(&ids, much_later);
        assert_eq!(host.take_outbox()[0].method, "Target.getTargets");
    }
}

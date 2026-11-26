//! A device as the runtime's hosts reach it: its files and its clipboard. Clones share their
//! state, so a test keeps one and hands the runtime another.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};
use tilolang_runtime::clipboard::ClipboardHost;

/// What draws a preview of a file, kept on the device, or `None` for a file with no preview.
type Preview = dyn Fn(&Files, &FileValue, u32) -> Option<FileValue>;

/// The device's files, kept in a session's content store (content at its address, once whatever
/// its name), in the wake scope an engine's calls run in. A chooser answers what [`Self::pick`]
/// staged (nothing: the person dismissed it); a save keeps the bytes under the offered name.
#[derive(Clone)]
pub struct Files {
    pub store: tilo_content::ContentStore,
    /// The window's scope, in which the store answers the engines' reads of a file.
    pub scope: Rc<tilo_wake::WakeScope>,
    picked: Rc<RefCell<Vec<FileValue>>>,
    asked: Rc<RefCell<Vec<FileRequest>>>,
    saved: Rc<RefCell<HashMap<String, Vec<u8>>>>,
    previews: Rc<RefCell<Option<Box<Preview>>>>,
    /// What this device kept, and of it what is on another device for now.
    kept: Rc<RefCell<std::collections::HashSet<String>>>,
    away: Rc<RefCell<std::collections::HashSet<String>>>,
}

impl Default for Files {
    fn default() -> Self {
        let store = tilo_content::ContentStore::memory_in_workspace();
        let scope = Rc::new(tilo_wake::WakeScope::new());
        tilo_resource::content::register(&scope.handle(), store.clone());
        Self {
            store,
            scope,
            picked: Rc::default(),
            asked: Rc::default(),
            saved: Rc::default(),
            previews: Rc::default(),
            kept: Rc::default(),
            away: Rc::default(),
        }
    }
}

impl Files {
    /// `bytes` kept as the file `name`: what a person choosing it would answer.
    pub fn keeping(&self, name: &str, media: &str, bytes: &[u8]) -> FileValue {
        let hash = self.store.put(bytes).unwrap();
        self.kept.borrow_mut().insert(hash.clone());
        FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        )
    }

    /// The bytes kept at `hash`, unless they are away.
    pub fn read(&self, hash: &str) -> Option<Vec<u8>> {
        if self.away.borrow().contains(hash) {
            return None;
        }
        self.store.read(hash)
    }

    /// The bytes at `hash` are on another device only, as on a device the record reached
    /// before them.
    pub fn send_away(&self, hash: &str) {
        self.away.borrow_mut().insert(hash.to_owned());
    }

    /// Every file kept so far is on another device only.
    pub fn send_all_away(&self) {
        let kept = self.kept.borrow().clone();
        self.away.borrow_mut().extend(kept);
    }

    /// The bytes at `hash` arrived.
    pub fn arrive(&self, hash: &str) {
        self.away.borrow_mut().remove(hash);
    }

    /// The bytes at a file's address (`tilo-blob://<hash>`).
    pub fn bytes_at(&self, url: &str) -> Vec<u8> {
        let hash = url
            .strip_prefix(tilolang_runtime::blob::SCHEME)
            .expect("a file's address");
        self.read(hash)
            .unwrap_or_else(|| panic!("nothing kept at {url}"))
    }

    /// The bytes `file` holds.
    pub fn bytes_of(&self, file: &FileValue) -> Vec<u8> {
        self.read(&file.hash)
            .unwrap_or_else(|| panic!("{} has no bytes", file.name))
    }

    /// The files `entries` as one archive, kept under `name`.
    pub fn archive(&self, name: &str, entries: &[(&str, &[u8])]) -> FileValue {
        let entries: Vec<(String, Vec<u8>)> = entries
            .iter()
            .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
            .collect();
        let bytes = bounded_zip::write_zip(&entries, &bounded_zip::ZipLimits::default()).unwrap();
        self.keeping(name, "application/zip", &bytes)
    }

    /// What the next chooser answers.
    pub fn pick(&self, files: Vec<FileValue>) {
        *self.picked.borrow_mut() = files;
    }

    /// The choosers an app opened, in order.
    pub fn asked(&self) -> Vec<FileRequest> {
        self.asked.borrow().clone()
    }

    /// The bytes an app saved or kept under `name`.
    pub fn saved(&self, name: &str) -> Option<Vec<u8>> {
        self.saved.borrow().get(name).cloned()
    }

    /// The text saved under `name`.
    pub fn said(&self, name: &str) -> String {
        let saved = self
            .saved(name)
            .unwrap_or_else(|| panic!("nothing saved as `{name}`"));
        String::from_utf8(saved).unwrap()
    }

    /// Preview each file as `draw` does; without, no file has a preview.
    pub fn previewing(
        self,
        draw: impl Fn(&Files, &FileValue, u32) -> Option<FileValue> + 'static,
    ) -> Self {
        *self.previews.borrow_mut() = Some(Box::new(draw));
        self
    }
}

impl BlobHost for Files {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        Files::read(self, hash)
    }

    fn choose(&self, request: &FileRequest) -> Result<Vec<FileValue>, String> {
        self.asked.borrow_mut().push(request.clone());
        Ok(self.picked.borrow().clone())
    }

    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String> {
        self.saved
            .borrow_mut()
            .insert(request.name.clone(), content.to_vec());
        Ok(())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        self.saved
            .borrow_mut()
            .insert(name.to_owned(), bytes.to_vec());
        Ok(self.keeping(name, media, bytes))
    }

    fn thumbnail(&self, file: &FileValue, size: u32) -> Result<Option<FileValue>, String> {
        Ok(self
            .previews
            .borrow()
            .as_ref()
            .and_then(|draw| draw(self, file, size)))
    }
}

/// What the clipboard holds: the text copied and its media type.
type Held = Rc<RefCell<Option<(String, Vec<u8>)>>>;

/// The device's clipboard: what was copied last, with its media type, and how long a secret
/// stays on it. One that refuses copies nothing.
#[derive(Clone, Default)]
pub struct Clipboard {
    held: Held,
    clears_after: Rc<Cell<Option<Duration>>>,
    refuses: Rc<Cell<bool>>,
}

impl Clipboard {
    /// What the clipboard holds, as text (empty when nothing was copied).
    pub fn text(&self) -> String {
        self.held
            .borrow()
            .as_ref()
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default()
    }

    /// What the clipboard holds, with its media type.
    pub fn held(&self) -> Option<(String, Vec<u8>)> {
        self.held.borrow().clone()
    }

    /// Put `text` on the clipboard, as another app copying it would.
    pub fn put(&self, mime: &str, bytes: &[u8]) {
        *self.held.borrow_mut() = Some((mime.to_owned(), bytes.to_vec()));
    }

    /// Empty the clipboard.
    pub fn clear(&self) {
        *self.held.borrow_mut() = None;
    }

    /// How long the secret last copied stays, when it was one.
    pub fn clears_after(&self) -> Option<Duration> {
        self.clears_after.get()
    }

    /// Refuse every copy from now (`true`) or take them again.
    pub fn refuse(&self, refuses: bool) {
        self.refuses.set(refuses);
    }
}

impl ClipboardHost for Clipboard {
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        if self.refuses.get() {
            return false;
        }
        self.put(mime, bytes);
        self.clears_after.set(None);
        true
    }

    fn copy_secret(&self, text: &str, clear_after: Duration) -> bool {
        if self.refuses.get() {
            return false;
        }
        self.put("text/plain", text.as_bytes());
        self.clears_after.set(Some(clear_after));
        true
    }

    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let held = self.held()?;
        (types.is_empty() || types.iter().any(|wanted| held.0.starts_with(wanted))).then_some(held)
    }
}

/// A provider that answers each call with `F`, and starts no jobs.
struct Calls<F>(F);

impl<F: Fn(&str, &str) -> Result<String, String>> tilolang_tilo::engine::Provider for Calls<F> {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        (self.0)(op, input)
    }
}

/// The device's engine for an app, as a session registers one: `provider` answers inside the
/// window's scope (where the device's files are), its jobs kept as the device keeps them. It
/// writes down what it was asked and the handlers each job runs.
#[derive(Clone)]
pub struct DeviceEngine {
    provider: Rc<dyn tilolang_tilo::engine::Provider>,
    scope: Rc<tilo_wake::WakeScope>,
    pub jobs: super::DeviceJobs,
    app: Option<String>,
    /// The ops this engine starts as jobs it holds itself, whose moves the test makes.
    starts: Vec<String>,
    asked: Rc<RefCell<Vec<(String, String)>>>,
    started: Rc<RefCell<Vec<Started>>>,
    cancelled: Rc<RefCell<Vec<String>>>,
}

/// A job an engine started: its path, op, input, and the handlers it runs as it moves.
#[derive(Clone)]
pub struct Started {
    pub job: String,
    pub app: String,
    pub op: String,
    pub input: String,
    pub handlers: tilolang_runtime::host_work::JobHandlers,
}

impl DeviceEngine {
    /// `provider` as the device's engine, in the scope of `files`.
    pub fn new(provider: impl tilolang_tilo::engine::Provider + 'static, files: &Files) -> Self {
        Self {
            provider: Rc::new(provider),
            scope: Rc::clone(&files.scope),
            jobs: super::DeviceJobs::default(),
            app: None,
            starts: Vec::new(),
            asked: Rc::default(),
            started: Rc::default(),
            cancelled: Rc::default(),
        }
    }

    /// An engine whose calls `answer` answers, which starts no jobs.
    pub fn calls(
        answer: impl Fn(&str, &str) -> Result<String, String> + 'static,
        files: &Files,
    ) -> Self {
        Self::new(Calls(answer), files)
    }

    /// Answer only `app`, refusing every other app's asks.
    pub fn for_app(mut self, app: &str) -> Self {
        self.app = Some(app.to_owned());
        self
    }

    /// Start `ops` as jobs held here (`devices/test/jobs/<op>-<n>`), which move only as the test
    /// runs their handlers.
    pub fn starting(mut self, ops: &[&str]) -> Self {
        self.starts = ops.iter().map(|op| (*op).to_owned()).collect();
        self
    }

    /// Each op asked as a call, with its input, in order.
    pub fn asked(&self) -> Vec<(String, String)> {
        self.asked.borrow().clone()
    }

    /// Whether `op` was asked as a call.
    pub fn was_asked(&self, op: &str) -> bool {
        self.asked.borrow().iter().any(|(asked, _)| asked == op)
    }

    /// Each job started, in order.
    pub fn started(&self) -> Vec<Started> {
        self.started.borrow().clone()
    }

    /// The job started last.
    pub fn last_started(&self) -> Started {
        self.started
            .borrow()
            .last()
            .cloned()
            .expect("a job was started")
    }

    /// The job `job` moves to `state`, saying `detail`, as the device records it for a view to
    /// read; once it ends, the action its `done:` names runs as the person who started it, and
    /// is done.
    pub fn moves(
        &self,
        rt: &tilolang_runtime::Runtime,
        job: &str,
        state: &str,
        detail: &serde_json::Value,
    ) {
        if state == "running" {
            rt.set_job(job, &self.record(job, state, detail));
        } else {
            let outcome = self.ends(rt, job, state, detail);
            assert!(
                matches!(outcome, tilolang_runtime::InvokeOutcome::Done(_)),
                "{outcome:?}"
            );
        }
    }

    /// The job `job` ends as `state`, saying `detail` last: the device records it and runs what
    /// its start named for its end, as the person who started it. What that came to.
    pub fn ends(
        &self,
        rt: &tilolang_runtime::Runtime,
        job: &str,
        state: &str,
        detail: &serde_json::Value,
    ) -> tilolang_runtime::InvokeOutcome {
        let record = self.record(job, state, detail);
        rt.set_job(job, &record);
        let done = self
            .started_as(job)
            .handlers
            .done
            .expect("the job names what runs as it ends");
        let call = rt
            .job_call(&done, &record)
            .expect("`done:` names an action");
        rt.invoke_action(&super::me(), call).expect("`done:` runs")
    }

    /// The job `job` as it was started.
    pub fn started_as(&self, job: &str) -> Started {
        self.started()
            .into_iter()
            .find(|started| started.job == job)
            .unwrap_or_else(|| panic!("`{job}` was not started here"))
    }

    fn record(&self, job: &str, state: &str, detail: &serde_json::Value) -> serde_json::Value {
        let started = self.started_as(job);
        serde_json::json!({
            "id": job, "app": started.app, "op": started.op, "state": state, "detail": detail,
        })
    }

    /// The jobs stopped, in order.
    pub fn cancelled(&self) -> Vec<String> {
        self.cancelled.borrow().clone()
    }

    fn owns(&self, app: &str) -> Result<(), String> {
        match &self.app {
            Some(own) if own != app => {
                Err(format!("`{app}` is not the app this engine answers for"))
            }
            _ => Ok(()),
        }
    }
}

impl tilolang_runtime::host_work::EngineHost for DeviceEngine {
    fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        self.owns(app)?;
        self.asked
            .borrow_mut()
            .push((op.to_owned(), input.to_owned()));
        let _window = self.scope.enter();
        self.provider.call(op, input)
    }

    fn start(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &tilolang_runtime::host_work::JobHandlers,
    ) -> Result<String, String> {
        self.owns(app)?;
        let job = if self.starts.iter().any(|own| own == op) {
            let n = self.started.borrow().len().saturating_add(1);
            format!("devices/test/jobs/{op}-{n}")
        } else {
            let _window = self.scope.enter();
            self.jobs.start(&*self.provider, app, op, input)?
        };
        self.started.borrow_mut().push(Started {
            job: job.clone(),
            app: app.to_owned(),
            op: op.to_owned(),
            input: input.to_owned(),
            handlers: handlers.clone(),
        });
        Ok(job)
    }

    fn stream(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: std::sync::Arc<dyn tilolang_runtime::host_work::Push>,
    ) -> Result<String, String> {
        self.owns(app)?;
        let _window = self.scope.enter();
        self.provider.stream(op, input, push)
    }

    fn job(&self, job: &str) -> Result<Option<serde_json::Value>, String> {
        Ok(self.jobs.job(job))
    }

    fn cancel(&self, job: &str) -> bool {
        self.cancelled.borrow_mut().push(job.to_owned());
        if self
            .started
            .borrow()
            .iter()
            .any(|started| started.job == job && self.starts.contains(&started.op))
        {
            return true;
        }
        let _window = self.scope.enter();
        self.provider.cancel(job)
    }
}

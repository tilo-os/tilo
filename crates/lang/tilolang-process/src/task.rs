//! The host of an agent task: its programs, their author the task
//! (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! The MCP server starts it on the task's first `tilo` call and it lives as long as the task:
//! until the API closes the task's program connection. It holds the task's session (the `let`s
//! its programs bound) and no documents, opens no windows and no broker connection of its own.
//! Every program comes in on the program connection, and everything a program reads or changes
//! goes out on it to the API, which reads and invokes as the task: the API's connection is the
//! task's ownership, so nothing here is trusted to say who it is.
//!
//! The connection carries one JSON object per line. The API sends `{"run": {source, author}}` (a
//! program with its go-ahead: its person approved it, or it is their own), `{"preview": {source,
//! author}}` (run provisionally, nothing sent: [`TaskSession::preview`]) or `{"catalogue": {}}` (answered `{"catalogue": …}`: every installed app's declarations,
//! [`TaskSession::catalogue`], and each app whose package did not load with what a program
//! naming it is told, `can't be reached: <why>`);
//! while the program runs, the host asks `{"store": {app, record, command}}` for what it reads of
//! an app's resources (answered `{"ok": reply}` or `{"err": why}`: the workspace's store's answer
//! to the read command, decided for the task, which the host reads itself with the app's package,
//! [`TaskReads`]), `{"children": {app, resource}}` for the resources a native app lists under
//! one of its own (answered `{"ok": {"rows": [{id, kind, key, name}]}}` or `{"err": why}`: the
//! app's directory, the document at its top and what it holds under it: a row's `id` names it
//! under its parent, which is what an invocation addresses, and its `key` is what the app's
//! `read` takes as its item), `{"reads": {app, resource}}` for whether a resource the app
//! publishes answers a read of itself (answered `{"ok": {"reads": bool}}` or `{"err": why}`: a
//! resource that does not is read by the document, which takes it as its `item`) and
//! `{"invoke": {app, resource, action, arguments}}` (answered `{"ok": output}`, `{"failed": why}`
//! or `{"refused": why}`), and it ends with `{"answer": …}`
//! ([`tilolang_tilo::task::TaskSession::run`]). A native app's document is read by the app itself
//! ([`tilolang_tilo::native`]), so the host's read of one is its `read` action, invoked like any
//! other.

use std::cell::RefCell;
use std::io::{BufRead as _, BufReader, Write as _};

use runtime_portal::packet::Descriptor;
use std::rc::Rc;
use tilo_socket::Stream;

use runtime_portal::process_link::{App, TaskFds, TaskRequest, decode_apps};
use serde_json::{Value as Json, json};
use tilo_resource_store::Port;
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::catalogue::NativeSurface;
use tilolang_tilo::native::{Calls, Native, NativeReads};
use tilolang_tilo::runtime::meta::PackageMeta;
use tilolang_tilo::runtime::value::ResourceRef;
use tilolang_tilo::task::{Answered, TaskApi, TaskSession, Unreachable};
use tilolang_tilo::task_reads::{StoreSource, TaskReads};

/// How a day or a time of day reads in a sentence a person reads (`person_output`): routes
/// through the same shared [`tilolang_tilo::instance::shown_at`] helper that `Platform::shown`
/// uses (one clock read, one zone lookup, no text built or parsed). An agent task session runs
/// outside any window `Platform` (no per-window `Phrases`), so it supplies the toolkit's region
/// formats and date words directly.
fn shown_for_a_person(kind: &str, value: &str) -> String {
    let moment = tilolang_tilo::instance::wall_moment();
    tilolang_tilo::instance::shown_at(
        kind,
        value,
        &moment,
        tilo_ui_base::format::region::current(),
        tilo_ui_base::format::words(),
    )
}

/// How a day or a time of day named in a sentence a person reads (`"{d}"`) reads: routes through
/// the same shared [`tilolang_tilo::instance::sentence_at`] helper that `Platform::sentence` uses.
fn sentence_for_a_person(kind: &str, value: &str) -> String {
    let moment = tilolang_tilo::instance::wall_moment();
    tilolang_tilo::instance::sentence_at(
        kind,
        value,
        &moment,
        tilo_ui_base::format::region::current(),
        tilo_ui_base::format::words(),
    )
}

/// The program connection, one line at a time.
struct Line {
    reader: RefCell<BufReader<Stream>>,
    writer: RefCell<Stream>,
}

impl Line {
    fn send(&self, message: &Json) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.writer
            .borrow_mut()
            .write_all(&line)
            .map_err(|error| format!("the API went away: {error}"))
    }

    /// The next message: `None` once the API closed the connection (or it broke, which is
    /// logged), `Some(Err(why))` for a line that is not a JSON message.
    fn next(&self) -> Option<Result<Json, String>> {
        let mut line = String::new();
        match self.reader.borrow_mut().read_line(&mut line) {
            Ok(0) => None,
            Err(error) => {
                tracing::warn!(%error, "the task's program connection broke");
                None
            }
            Ok(_) => Some(
                serde_json::from_str(&line)
                    .map_err(|error| format!("the API sent a line that is not JSON: {error}")),
            ),
        }
    }

    fn ask(&self, request: &Json) -> Result<Json, String> {
        self.send(request)?;
        self.next()
            .ok_or_else(|| "the API closed the task".to_owned())?
    }
}

/// The program connection, as where a task's reads of the store go out.
struct Lines(Rc<Line>);

impl StoreSource for Lines {
    fn port(&self, app: &str, record: &str) -> Rc<dyn Port> {
        Rc::new(StoreLine {
            line: Rc::clone(&self.0),
            app: app.to_owned(),
            record: record.to_owned(),
        })
    }
}

/// The reads of one app's resources, asked of the store through the API as the task.
struct StoreLine {
    line: Rc<Line>,
    app: String,
    record: String,
}

impl Port for StoreLine {
    fn command(
        &self,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        let command = serde_json::to_value(&command).map_err(|error| error.to_string())?;
        let reply = self.line.ask(&json!({
            "store": {"app": self.app, "record": self.record, "command": command},
        }))?;
        if let Some(answer) = reply.get("ok") {
            return serde_json::from_value(answer.clone()).map_err(|error| {
                format!("the API sent a store answer that does not read: {error}")
            });
        }
        Err(reply.get("err").and_then(Json::as_str).map_or_else(
            || format!("the API answered a store read with neither ok nor err: {reply}"),
            str::to_owned,
        ))
    }
}

/// What the task's programs reach the workspace through: the apps' resources, read here from what
/// the store answers through the API ([`TaskReads`]), a native app's document, read by the app itself
/// ([`NativeReads`]), and invocations, which go to the API.
struct Api {
    line: Rc<Line>,
    reads: TaskReads,
    /// The native apps the task reaches, and the document each publishes.
    native: Rc<Native>,
    /// The author of the program running now, as the actions it plans for a preview see their
    /// caller; `None` until the first program names one.
    caller: RefCell<Option<tilolang_tilo::publish::Caller>>,
    /// The apps whose documents the session changes itself; any other app's change is its own
    /// action, invoked where the app runs.
    changed_at_document: std::collections::BTreeSet<String>,
}

impl TaskApi for Api {
    fn read(&self, app: &str, item: &str, ty: &str) -> Result<Json, String> {
        if self.native.is(app) {
            return NativeReads::new(&self.native, self).read(app, item, ty);
        }
        self.reads.read(app, item)
    }

    fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        if self.native.is(app) {
            return NativeReads::new(&self.native, self).query(app, request);
        }
        self.reads.query(app, request)
    }

    /// The reading the session publishes of `target` (the device's sound, the session's windows,
    /// Settings' sections), asked of the API within the program reading it and taken at once:
    /// `{"ok": {"reading": …}}`, `{"none": true}` for one the session publishes nothing of, or
    /// `{"err": why}`. What a platform root no service of the device serves reads as (an empty
    /// reading, as the shell's own parts are handed it) is the session's to say, so a program and
    /// a view read the device alike.
    fn reading(&self, target: &ResourceRef) -> Result<Option<Json>, String> {
        let (app, ty) = target
            .ty
            .split_once('#')
            .ok_or_else(|| format!("`{}` is no app's resource", target.ty))?;
        let reply = self.line.ask(&json!({
            "reading": {
                "app": app,
                "resource": target.id_string(),
                "type": ty,
            },
        }))?;
        if reply.get("none").and_then(Json::as_bool) == Some(true) {
            return Ok(None);
        }
        if let Some(reading) = reply.pointer("/ok/reading") {
            return Ok(Some(reading.clone()));
        }
        Err(reply.get("err").and_then(Json::as_str).map_or_else(
            || format!("the API answered a reading with neither ok, none nor err: {reply}"),
            str::to_owned,
        ))
    }

    fn begin_program(&self) {
        // Each program reads the documents anew, as the API decides them now.
        self.reads.forget();
    }

    fn resource_id(&self, app: &str, resource: &str) -> Result<String, String> {
        NativeReads::new(&self.native, self).resource_id(app, resource)
    }

    fn invoke(&self, call: &Json) -> Answered {
        // A change of a document the session changes itself is an action of that document's own
        // provider. Any other app's change is its action, invoked at the app.
        let mut call = call.clone();
        if let Some(app) = call.get("app").and_then(Json::as_str).map(str::to_owned)
            && self.changed_at_document.contains(&app)
            && let Some((under, record)) = self.reads.address(&app)
            && let Some(fields) = call.as_object_mut()
        {
            fields.insert("app".to_owned(), json!(under));
            fields.insert("record".to_owned(), json!(record));
        }
        let reply = match self.line.ask(&json!({"invoke": call})) {
            Ok(reply) => reply,
            Err(why) => return Answered::Failed(why),
        };
        // What the program reads of the app next holds what its change made: the store has it,
        // and the app is read again.
        if let Some(app) = call
            .get("app")
            .and_then(Json::as_str)
            .filter(|app| !self.native.is(app))
        {
            self.reads.forget_app(app);
        }
        Line::answered(&reply)
    }
}

impl Calls for Api {
    fn call(&self, call: &Json) -> Answered {
        self.invoke(call)
    }

    fn children(&self, app: &str, resource: &str) -> Result<Vec<Json>, String> {
        let reply = self
            .line
            .ask(&json!({"children": {"app": app, "resource": resource}}))?;
        if let Some(rows) = reply.pointer("/ok/rows").and_then(Json::as_array) {
            return Ok(rows.clone());
        }
        Err(reply.get("err").and_then(Json::as_str).map_or_else(
            || format!("the API answered a listing with neither ok nor err: {reply}"),
            str::to_owned,
        ))
    }

    fn reads(&self, app: &str, resource: &str) -> Result<bool, String> {
        let reply = self
            .line
            .ask(&json!({"reads": {"app": app, "resource": resource}}))?;
        if let Some(reads) = reply.pointer("/ok/reads").and_then(Json::as_bool) {
            return Ok(reads);
        }
        Err(reply.get("err").and_then(Json::as_str).map_or_else(
            || format!("the API answered a read question with neither ok nor err: {reply}"),
            str::to_owned,
        ))
    }
}

impl Line {
    /// How the API's `reply` to an invocation says it ended.
    fn answered(reply: &Json) -> Answered {
        let why = |key: &str| {
            reply
                .get(key)
                .and_then(Json::as_str)
                .map_or_else(|| format!("the API gave no reason ({key})"), str::to_owned)
        };
        if let Some(output) = reply.get("ok") {
            Answered::Done(output.clone())
        } else if reply.get("refused").is_some() {
            Answered::Refused(why("refused"))
        } else if reply.get("failed").is_some() {
            Answered::Failed(why("failed"))
        } else {
            Answered::Failed(format!(
                "the API answered the invocation with neither ok, failed nor refused: {reply}"
            ))
        }
    }
}

/// One other connection into a task's session: the API's own, which started the host, or one
/// the API opened later for the person's lines in the run's conversation (shell.md §8.1).
struct Guest {
    line: Rc<Line>,
    api: Rc<Api>,
    /// Its connection, watched by the host's wait: a line arriving wakes the host.
    watched: Stream,
}

/// A task this host runs the programs of.
pub struct Task {
    line: Rc<Line>,
    /// The program connection, watched for what the API sends.
    watched: Stream,
    /// Further connections into the same session, each given when the host opened: the
    /// person's lines in the run's conversation run here, beside the agent's own calls, so the
    /// two share the session's `let`s. Every program still runs one at a time.
    guests: RefCell<Vec<Guest>>,
    session: TaskSession,
    api: Rc<Api>,
}

impl Task {
    /// The task `request`, its programs coming in on `fds.program` and compiled against the
    /// apps in `fds.apps`.
    pub fn open(request: &TaskRequest, fds: TaskFds) -> Result<Self, String> {
        Self::open_with(request, fds, None)
    }

    /// [`Self::open`] for a task whose session another connection joins from the start,
    /// `person`: the person's lines in its run's conversation run in the same session as the
    /// agent's own calls, each on a connection of its own, so they share its `let`s.
    pub fn open_with(
        request: &TaskRequest,
        fds: TaskFds,
        person: Option<Stream>,
    ) -> Result<Self, String> {
        let apps = apps(&fds.apps, request.apps_len)?;
        #[cfg(unix)]
        let stream = crate::stream_of(fds.program);
        #[cfg(windows)]
        let stream = crate::stream_of(fds.program).map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let watched = stream.try_clone().map_err(|error| error.to_string())?;
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(stream)),
            writer: RefCell::new(writer),
        });
        let reads = TaskReads::new(
            &apps.bundles,
            request.documents_under.clone(),
            request.person.as_deref(),
            Rc::new(Lines(Rc::clone(&line))) as Rc<dyn StoreSource>,
        );
        let native = Rc::new(Native::of(&apps.bundles, &apps.native));
        let api = Rc::new(Api {
            line: Rc::clone(&line),
            reads,
            native: Rc::clone(&native),
            // Each program names its author; this is no one's until the first does.
            caller: RefCell::new(None),
            changed_at_document: request.changed_at_document.clone(),
        });
        let session = TaskSession::new(
            request.person.as_deref(),
            &(Rc::clone(&api) as Rc<dyn TaskApi>),
            &apps.bundles,
            native,
        )?;
        // A reply the person reads takes the words they read: a day or a moment in their
        // formats, not the stored ISO text.
        session.set_shown_by(shown_for_a_person);
        session.set_sentence_by(sentence_for_a_person);
        // An app whose payload did not read is reported as one whose package does not load: a
        // program naming it reads that it can't be reached, and the task runs every other app.
        for gone in &apps.unreachable {
            session.could_not_load(&gone.app, &gone.reason);
        }
        tracing::info!(
            task = request.task,
            apps = apps.bundles.len(),
            native = apps.native.len(),
            unreachable = apps.unreachable.len(),
            "task host started"
        );
        let task = Self {
            line,
            watched,
            guests: RefCell::new(Vec::new()),
            session,
            api,
        };
        if let Some(person) = person {
            task.join(person)?;
        }
        Ok(task)
    }

    /// Let `stream` join this session as a guest: what it sends runs in the same session, one
    /// program at a time, and what the program reads or changes goes back on it.
    pub fn join(&self, stream: Stream) -> Result<(), String> {
        // A pipe always waits; a socket may have been handed over not waiting.
        #[cfg(unix)]
        stream
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let watched = stream.try_clone().map_err(|error| error.to_string())?;
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(stream)),
            writer: RefCell::new(writer),
        });
        let api = Rc::new(Api {
            line: Rc::clone(&line),
            reads: TaskReads::new(
                &[],
                std::collections::BTreeMap::new(),
                None,
                Rc::new(Lines(Rc::clone(&line))) as Rc<dyn StoreSource>,
            ),
            native: Rc::new(Native::of(&[], &[])),
            caller: RefCell::new(None),
            changed_at_document: self.api.changed_at_document.clone(),
        });
        self.guests.borrow_mut().push(Guest { line, api, watched });
        Ok(())
    }

    /// The program connection, to wait on.
    pub fn link(&self) -> &Stream {
        &self.watched
    }

    /// Copies of the guest connections, to wait on: a line arriving on one wakes the host as the
    /// API's own connection would.
    pub fn guests(&self) -> Vec<Stream> {
        self.guests
            .borrow()
            .iter()
            .filter_map(|guest| guest.watched.try_clone().ok())
            .collect()
    }

    /// Answer what the API sent: run each program and say how it went. `false` once the API
    /// closed the task.
    ///
    /// Everything the API sends is answered: a line that is not a program to run is answered
    /// `failed` with why, so the API never waits on a message the host dropped. Every line
    /// already read off the connection is served: a line left in the reader's buffer would wake
    /// nothing, and wait until the API sent more.
    pub fn serve(&self) -> bool {
        loop {
            if !self.serve_one() {
                return false;
            }
            if self.line.reader.borrow().buffer().is_empty() {
                return true;
            }
        }
    }

    /// Answer every connection that has something: the API's own first, then the guests it
    /// opened. Waits on all of them, so a person's line is served as soon as it arrives and
    /// never waits on the agent's next call. `false` once the API's own connection closed.
    pub fn serve_all(&self) -> bool {
        loop {
            let mut ready: Vec<usize> = Vec::new();
            if !self.line.reader.borrow().buffer().is_empty() {
                ready.push(0);
            }
            {
                let guests = self.guests.borrow();
                for (at, guest) in guests.iter().enumerate() {
                    if !guest.line.reader.borrow().buffer().is_empty() {
                        ready.push(at.saturating_add(1));
                    }
                }
            }
            if ready.is_empty() {
                // Nothing is read off the streams yet: block until one of them has something,
                // as a host serving one connection does, then serve it. The wait is on every
                // stream, so a person's line never waits for the agent's next call.
                match self.wait_ready() {
                    None => return false,
                    Some(found) => ready = found,
                }
            }
            if ready.is_empty() {
                continue;
            }
            for connection in ready {
                if connection == 0 {
                    if !self.serve_one() {
                        return false;
                    }
                } else if !self.serve_guest_at(connection.saturating_sub(1)) {
                    self.guests
                        .borrow_mut()
                        .remove(connection.saturating_sub(1));
                }
            }
        }
    }

    /// Block until the API's connection or a guest's has something to read, saying which: `0`
    /// for the API's own, `at + 1` for the guest at `at`. `None` once the API's connection
    /// closed.
    #[cfg(unix)]
    fn wait_ready(&self) -> Option<Vec<usize>> {
        use std::os::fd::AsRawFd as _;
        let api = self.watched.as_raw_fd();
        let guests = self.guests();
        let mut fds = Vec::with_capacity(guests.len().saturating_add(1));
        fds.push(libc::pollfd {
            fd: api,
            events: libc::POLLIN,
            revents: 0,
        });
        for guest in &guests {
            fds.push(libc::pollfd {
                fd: guest.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let count = libc::nfds_t::try_from(fds.len()).unwrap_or(libc::nfds_t::MAX);
        loop {
            // SAFETY: `fds` is a live array of initialized pollfds naming open descriptors.
            let polled = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
            if polled < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            // The API's own connection closing ends the task; a guest's closing is served as
            // a line that ends, which drops it.
            if fds.first().is_some_and(|fd| {
                fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
                    && fd.revents & libc::POLLIN == 0
            }) {
                return None;
            }
            return Some(
                fds.iter()
                    .enumerate()
                    .filter(|(_, fd)| fd.revents != 0)
                    .map(|(at, _)| at)
                    .collect(),
            );
        }
    }

    /// Block until the API's connection or a guest's has something to read, saying which: `0`
    /// for the API's own, `at + 1` for the guest at `at`. `None` once the API's connection
    /// closed.
    #[cfg(windows)]
    fn wait_ready(&self) -> Option<Vec<usize>> {
        let guests = self.guests();
        let mut streams = vec![&self.watched];
        streams.extend(guests.iter());
        tilo_socket::link::wait(&streams, &[], None).ok()?;
        // The API's own connection closing ends the task; a guest's closing is served as a line
        // that ends, which drops it.
        if self.watched.hung_up() {
            return None;
        }
        Some(
            streams
                .iter()
                .enumerate()
                .filter(|(_, stream)| stream.ready().unwrap_or(true))
                .map(|(at, _)| at)
                .collect(),
        )
    }

    /// Serve one line of the guest at `at`: `false` once its connection closed. The guest's own
    /// line and api are taken out of the list's borrow, so a program that serves the API's
    /// connection too is never held under it.
    fn serve_guest_at(&self, at: usize) -> bool {
        let (line, api) = {
            let guests = self.guests.borrow();
            let Some(guest) = guests.get(at) else {
                return false;
            };
            (Rc::clone(&guest.line), Rc::clone(&guest.api))
        };
        self.serve_guest(&line, &api)
    }

    /// Serve one line of a connection: `false` once it closed.
    fn serve_guest(&self, guest: &Rc<Line>, api: &Rc<Api>) -> bool {
        let Some(message) = guest.next() else {
            return false;
        };
        if let Ok(message) = &message
            && message.get("catalogue").is_some()
        {
            return match guest.send(&json!({ "catalogue": self.session.catalogue() })) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%error, "the catalogue did not reach a guest");
                    false
                }
            };
        }
        let answer = match message.and_then(|message| program(&message)) {
            Ok((source, go_ahead, author)) => {
                let actor = author.actor.clone();
                *api.caller.borrow_mut() = Some(author);
                self.run(&source, go_ahead, &actor)
            }
            Err(why) => {
                tracing::warn!(why, "a guest sent the task something that is not a program");
                json!({"kind": "program", "state": "failed", "detail": why})
            }
        };
        if let Err(error) = guest.send(&json!({ "answer": answer })) {
            tracing::warn!(%error, "a program's answer did not reach a guest");
            return false;
        }
        true
    }

    /// Run `source`, written by `actor`: for real with its go-ahead, else provisionally.
    fn run(&self, source: &str, go_ahead: bool, actor: &tilolang_tilo::runtime::Actor) -> Json {
        if go_ahead {
            self.session.run(source, actor)
        } else {
            self.session.preview(source, actor)
        }
    }

    fn serve_one(&self) -> bool {
        let Some(message) = self.line.next() else {
            return false;
        };
        if let Ok(message) = &message
            && message.get("catalogue").is_some()
        {
            return match self
                .line
                .send(&json!({ "catalogue": self.session.catalogue() }))
            {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(%error, "the catalogue did not reach the API");
                    false
                }
            };
        }
        let answer = match message.and_then(|message| program(&message)) {
            Ok((source, go_ahead, author)) => {
                let actor = author.actor.clone();
                *self.api.caller.borrow_mut() = Some(author);
                self.run(&source, go_ahead, &actor)
            }
            Err(why) => {
                tracing::warn!(why, "the API sent the task something that is not a program");
                json!({"kind": "program", "state": "failed", "detail": why})
            }
        };
        if let Err(error) = self.line.send(&json!({ "answer": answer })) {
            tracing::warn!(%error, "a program's answer did not reach the API");
            return false;
        }
        true
    }
}

/// The program `message` asks the task to run, whether it has its go-ahead (`run`) or is
/// previewed (`preview`), and who wrote it: the API names the author of every program, the agent or the
/// person typing.
fn program(message: &Json) -> Result<(String, bool, tilolang_tilo::publish::Caller), String> {
    let (run, go_ahead) = match (message.get("run"), message.get("preview")) {
        (Some(run), None) => (run, true),
        (None, Some(previewed)) => (previewed, false),
        _ => {
            return Err(format!(
                "the API sent the task something other than a program: {message}"
            ));
        }
    };
    let source = run
        .get("source")
        .and_then(Json::as_str)
        .ok_or("the API sent a program without its source")?;
    let author = run
        .get("author")
        .ok_or("the API sent a program without its author")?;
    let actor = author
        .get("actor")
        .and_then(|actor| serde_json::from_value(actor.clone()).ok())
        .ok_or("the API sent a program whose author names no one")?;
    let author = tilolang_tilo::publish::Caller {
        actor,
        agent: author.get("agent").and_then(Json::as_bool).unwrap_or(true),
        on_behalf_of: author
            .get("on_behalf_of")
            .and_then(Json::as_str)
            .map(str::to_owned),
        // Only a device that says it handed the agent its session answers for it; otherwise the
        // agent acts under a key of its own, and its writes must keep its signed call.
        handed: author
            .get("handed")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        app: None,
    };
    Ok((source.to_owned(), go_ahead, author))
}

/// What the session says a task's programs may name: the tilolang apps it runs, whose modules
/// run a program's reads, and the native apps it holds, read as packages from their surfaces.
struct Apps {
    bundles: Vec<Bundle>,
    native: Vec<PackageMeta>,
    /// The apps whose payload did not read, by the app and why: reported with the packages that
    /// do not load, so one broken app leaves only itself unreachable.
    unreachable: Vec<Unreachable>,
}

/// The apps the immutable shared buffer `fd` holds, `len` bytes of them.
fn apps(fd: &Descriptor, len: u64) -> Result<Apps, String> {
    let len = usize::try_from(len).map_err(|error| error.to_string())?;
    let mapping =
        tilo_shm::Mapping::map_sealed(fd, true, len, len).map_err(|error| error.to_string())?;
    let bytes = mapping
        .get(0, len)
        .ok_or("the apps are shorter than they say")?;
    let mut apps = Apps {
        bundles: Vec::new(),
        native: Vec::new(),
        unreachable: Vec::new(),
    };
    for app in decode_apps(bytes).map_err(|error| error.to_string())? {
        match app {
            App::Module(module) => {
                // The package names the app it is: a module without its metadata names none.
                let named = PackageMeta::from_wasm(&module.wasm).map(|meta| meta.app);
                crate::adopt_compiled(&module);
                match Bundle::from_parts(module.wasm, module.ids, &module.imports) {
                    Ok(bundle) => apps.bundles.push(bundle),
                    Err(error) => match named {
                        Some(app) => apps.unreachable.push(Unreachable {
                            app,
                            reason: error.to_string(),
                        }),
                        None => {
                            tracing::warn!(%error, "a package handed to a task names no app");
                        }
                    },
                }
            }
            App::Surface {
                app,
                title,
                surface,
            } => {
                let published = NativeSurface {
                    app: app.clone(),
                    title,
                    surface,
                }
                .package();
                match published {
                    Ok(meta) => apps.native.push(meta),
                    Err(reason) => apps.unreachable.push(Unreachable { app, reason }),
                }
            }
        }
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A task over no packages, its program connection's far end returned as the API's.
    fn task() -> (Task, BufReader<Stream>, Stream) {
        task_with(&[])
    }

    /// A task over the apps `native`, read as packages from their surfaces, its program
    /// connection's far end returned as the API's.
    fn task_with(native: &[PackageMeta]) -> (Task, BufReader<Stream>, Stream) {
        task_over(&[], native)
    }

    /// A task over the packages `bundles` and the apps `native`, its program connection's far
    /// end returned as the API's.
    fn task_over(bundles: &[Bundle], native: &[PackageMeta]) -> (Task, BufReader<Stream>, Stream) {
        let (ours, api) = tilo_socket::pair().expect("a socket pair");
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(ours.try_clone().expect("a clone"))),
            writer: RefCell::new(ours.try_clone().expect("a clone")),
        });
        let apps = Rc::new(Native::of(bundles, native));
        let reached = Rc::new(Api {
            line: Rc::clone(&line),
            reads: TaskReads::new(
                bundles,
                std::collections::BTreeMap::new(),
                None,
                Rc::new(Lines(Rc::clone(&line))) as Rc<dyn StoreSource>,
            ),
            native: Rc::clone(&apps),
            caller: RefCell::new(None),
            changed_at_document: std::collections::BTreeSet::new(),
        });
        let session = TaskSession::new(
            None,
            &(Rc::clone(&reached) as Rc<dyn TaskApi>),
            bundles,
            apps,
        )
        .expect("a session");
        let task = Task {
            line,
            watched: ours,
            guests: RefCell::new(Vec::new()),
            session,
            api: reached,
        };
        (task, BufReader::new(api.try_clone().expect("a clone")), api)
    }

    fn answer(reader: &mut BufReader<Stream>) -> Json {
        let mut line = String::new();
        reader.read_line(&mut line).expect("an answer");
        serde_json::from_str(&line).expect("a JSON answer")
    }

    tilo_test_env::scenario! {
        fn a_line_message_or_reply_the_task_cannot_read_is_failed_with_why;

        phase a_line_that_is_not_json_is_answered_failed;
        {
            let (task, mut reader, mut api) = task();
            api.write_all(b"{not json\n").expect("sent");
            assert!(task.serve(), "the task goes on");
            let answer = answer(&mut reader);
            assert_eq!(answer["answer"]["state"], "failed");
            let detail = answer["answer"]["detail"].as_str().unwrap_or_default();
            assert!(detail.contains("not JSON"), "{detail}");
        }

        phase a_message_that_is_not_a_program_is_answered_failed;
        {
            let (task, mut reader, mut api) = task();
            api.write_all(b"{\"walk\": {}}\n").expect("sent");
            assert!(task.serve());
            assert_eq!(answer(&mut reader)["answer"]["state"], "failed");
            api.write_all(b"{\"run\": {\"author\": {}}}\n")
                .expect("sent");
            assert!(task.serve());
            let answer = answer(&mut reader);
            assert_eq!(answer["answer"]["state"], "failed");
            assert!(
                answer["answer"]["detail"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("without its source")
            );
        }

        phase an_unreadable_reply_fails_the_request_with_why;
        {
            let (task, mut reader, mut api) = task();
            let asking = std::thread::spawn(move || {
                let mut asked = String::new();
                reader.read_line(&mut asked).expect("asked");
                api.write_all(b"garbage\n").expect("sent");
                (reader, api)
            });
            let read = Lines(Rc::clone(&task.line))
                .port("app", "")
                .command(collab_types::store::wire::Command::Get {
                    kind: "tl.so.tilo.lang.counter.Counter".to_owned(),
                    id: "root".to_owned(),
                });
            let _ends = asking.join().expect("the API");
            let why = read.expect_err("an unreadable reply fails the read");
            assert!(why.contains("not JSON"), "{why}");
        }
    }

    /// The native app `app` named `title`, read from `surface` as its committed `surface.toml`
    /// reads.
    fn native(app: &str, title: &str, surface: &str) -> PackageMeta {
        NativeSurface {
            app: app.to_owned(),
            title: title.to_owned(),
            surface: surface.to_owned(),
        }
        .package()
        .expect("the surface reads")
    }

    /// Chat, as the app publishes it.
    fn chat() -> PackageMeta {
        native(
            "so.tilo.chat",
            "Chat",
            include_str!("../../tilolang-tilo/tests/native-chat/surface.json"),
        )
    }

    /// Tasks, as the app publishes it.
    fn tasks() -> PackageMeta {
        native(
            "so.tilo.tasks",
            "Tasks",
            include_str!("../../tilolang-tilo/tests/native-tasks/surface.json"),
        )
    }

    /// Answer the `count` invocations the API is sent with `answer` (one JSON line), and hand
    /// back the invocations it was sent.
    fn answering(
        api: Stream,
        count: usize,
        answer: &'static str,
    ) -> std::thread::JoinHandle<Vec<Json>> {
        std::thread::spawn(move || {
            let mut reader = BufReader::new(api.try_clone().expect("a clone"));
            let mut api = api;
            let mut seen = Vec::new();
            for _ in 0..count {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("a call") == 0 {
                    break;
                }
                let message: Json = serde_json::from_str(&line).expect("a JSON call");
                seen.push(message["invoke"].clone());
                api.write_all(answer.as_bytes()).expect("answered");
                api.write_all(b"\n").expect("answered");
            }
            seen
        })
    }

    /// What a program run answered, and how it ended.
    fn ran(task: &Task, source: &str) -> Json {
        task.session.run(
            source,
            &tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::agent(
                "task", "ada",
            )),
        )
    }

    /// A directory row an app lists for the item `key`, which its `read` takes as its `item`,
    /// under the id the window publishing it binds it by.
    fn row(key: &str, name: &str) -> Json {
        let bound = format!("bound:{key}");
        json!({"id": bound, "kind": "item", "key": key, "name": name})
    }

    /// The row an app's directory lists its document by: the key its `read` takes, under an id the
    /// publishing window binds, which is no name the app itself knows.
    fn document_row(record: &str, name: &str) -> Json {
        json!({"id": format!("bound:{record}"), "kind": record, "key": record, "name": name})
    }

    /// An app's own answers: the resources its directory lists under its document, which of them
    /// answer a read of themselves, and what its `read` answers of each item, its model's JSON.
    struct Reads {
        app: &'static str,
        record: &'static str,
        /// The resources that answer a read of themselves, by the id the directory lists them
        /// as; every other resource is read by the document, which takes it as its `item`.
        reading: &'static [&'static str],
        /// The resource of the kind `.1` the rows are listed under, beside the document (a
        /// tracker's issues, in the parts of a chain); `None` for rows under the document itself.
        beside: Option<(&'static str, &'static str)>,
        rows: Vec<Json>,
        items: Vec<(&'static str, Json)>,
    }

    impl Reads {
        /// The API's answers to what a program asks of the app.
        fn api(self) -> impl FnMut(&Json) -> Json + Send + 'static {
            move |message| self.answer(message)
        }

        /// How the app answers `message`: its directory, whether a resource reads itself, or its
        /// own `read` of an item.
        fn answer(&self, message: &Json) -> Json {
            if let Some(listing) = message.get("children") {
                assert_eq!(listing["app"], self.app, "{message:#}");
                // The document sits at the top of the app's directory, the resources under it.
                let rows = match listing["resource"].as_str() {
                    Some("") => vec![document_row(self.record, self.record)],
                    Some(resource) if resource == format!("bound:{}", self.record) => {
                        match self.beside {
                            Some((id, kind)) => {
                                vec![json!({"id": id, "kind": kind, "key": id, "name": kind})]
                            }
                            None => self.rows.clone(),
                        }
                    }
                    Some(resource) if self.beside.is_some_and(|(id, _)| id == resource) => {
                        self.rows.clone()
                    }
                    other => panic!("{} lists nothing under {other:?}", self.app),
                };
                return json!({"ok": {"rows": rows}});
            }
            if let Some(question) = message.get("reads") {
                assert_eq!(question["app"], self.app, "{message:#}");
                let resource = question["resource"].as_str().unwrap_or_default();
                return json!({"ok": {"reads": self.reading.contains(&resource)}});
            }
            let call = &message["invoke"];
            assert_eq!(call["app"], self.app, "{message:#}");
            assert_eq!(call["action"], "read", "{message:#}");
            // A resource that reads itself is read by its own id; every other by the document the
            // app publishes it as, which takes the resource's key as its `item`.
            let key = if let Some(key) = call["arguments"]["item"].as_str() {
                let under = self
                    .beside
                    .map_or_else(|| format!("bound:{}", self.record), |(id, _)| id.to_owned());
                assert_eq!(call["resource"], under, "{message:#}");
                key
            } else {
                let id = call["resource"].as_str().unwrap_or_default();
                assert!(self.reading.contains(&id), "{message:#}");
                self.rows
                    .iter()
                    .find(|row| row["id"] == id)
                    .and_then(|row| row["key"].as_str())
                    .unwrap_or_else(|| panic!("{} lists no {id}", self.app))
            };
            match self.items.iter().find(|(held, _)| *held == key) {
                Some((_, content)) => json!({"ok": {"value": content, "resources": []}}),
                None => json!({"failed": format!("{} holds no {key}", self.app)}),
            }
        }
    }

    /// Close the test's end of the API once the program ran: the task sends nothing more.
    fn closed(api: &Stream) {
        api.shutdown(std::net::Shutdown::Both)
            .expect("the API closed");
    }

    /// An API that answers every message with `answer` until its far end closes (the test shuts
    /// the returned stream down once the program ran), handing back what the task sent it.
    fn scripted(
        api: &Stream,
        mut answer: impl FnMut(&Json) -> Json + Send + 'static,
    ) -> std::thread::JoinHandle<Vec<Json>> {
        let mut api = api.try_clone().expect("a clone");
        std::thread::spawn(move || {
            let mut reader = BufReader::new(api.try_clone().expect("a clone"));
            let mut seen = Vec::new();
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let message: Json = serde_json::from_str(&line).expect("a JSON message");
                let mut reply = serde_json::to_vec(&answer(&message)).expect("JSON");
                reply.push(b'\n');
                api.write_all(&reply).expect("answered");
                seen.push(message);
            }
            seen
        })
    }

    tilo_test_env::scenario! {
        fn a_program_reads_native_apps_through_what_they_publish;

        phase a_program_reads_a_native_apps_document_fields_through_the_app;
        {
            let (task, _reader, api) = task_with(&[tasks()]);
            let calls = scripted(
                &api,
                Reads {
                    app: "so.tilo.tasks",
                    record: "tasks.tracker",
                    reading: &["bound:tasks.issues/issues/one"],
                    beside: Some(("chain:issues", "tasks.issues")),
                    rows: vec![row("tasks.issues/issues/one", "Ship the reads")],
                    items: vec![(
                        "tasks.issues/issues/one",
                        json!({
                            "number": 7,
                            "title": "Ship the reads",
                            "body": "One read path.",
                            "assignee": "ada",
                            "labels": ["review", "urgent"],
                        }),
                    )],
                }
                .api(),
            );
            let answer = ran(
                &task,
                "for i in find(tasks.Issue) {\n    print(i.title)\n    print(i.assignee)\n    \
             print(i.labels.len())\n    print(i.number)\n}",
            );
            assert_eq!(answer["state"], "done", "{answer:#}");
            // Each field as the app's surface types it: text, the text an option holds, a set's
            // members, a count.
            assert_eq!(
                answer["output"],
                json!(["Ship the reads", "ada", "2", "7"]),
                "{answer:#}"
            );
            closed(&api);
            let calls = calls.join().expect("the API");
            // The app listed its directory, what its document published holds and the issues kept
            // beside it; reading the resource the program asked about, it listed them again to
            // address that resource, asked whether it reads itself, and read through its own
            // `read`, once.
            assert_eq!(
                calls,
                [
                    json!({"children": {"app": "so.tilo.tasks", "resource": ""}}),
                    json!({"children": {"app": "so.tilo.tasks", "resource": "bound:tasks.tracker"}}),
                    json!({"children": {"app": "so.tilo.tasks", "resource": "chain:issues"}}),
                    json!({"children": {"app": "so.tilo.tasks", "resource": ""}}),
                    json!({"children": {"app": "so.tilo.tasks", "resource": "bound:tasks.tracker"}}),
                    json!({"children": {"app": "so.tilo.tasks", "resource": "chain:issues"}}),
                    json!({
                        "reads": {
                            "app": "so.tilo.tasks",
                            "resource": "bound:tasks.issues/issues/one",
                        },
                    }),
                    json!({
                        "invoke": {
                            "app": "so.tilo.tasks",
                            "resource": "bound:tasks.issues/issues/one",
                            "action": "read",
                            "arguments": {"item": null, "version": null},
                        },
                    }),
                ],
                "{calls:#?}"
            );
        }

        phase a_program_filters_a_native_apps_resources_by_their_fields;
        {
            let (task, _reader, api) = task_with(&[tasks()]);
            let calls = scripted(
                &api,
                Reads {
                    app: "so.tilo.tasks",
                    record: "tasks.tracker",
                    reading: &[
                        "bound:tasks.issues/issues/one",
                        "bound:tasks.issues/issues/two",
                    ],
                    beside: Some(("chain:issues", "tasks.issues")),
                    rows: vec![
                        row("tasks.issues/issues/one", "Ship the reads"),
                        row("tasks.issues/issues/two", "Review Ada's portfolio"),
                    ],
                    items: vec![
                        (
                            "tasks.issues/issues/one",
                            json!({"title": "Ship the reads", "assignee": "ada"}),
                        ),
                        (
                            "tasks.issues/issues/two",
                            json!({"title": "Review Ada's portfolio", "assignee": "bob"}),
                        ),
                    ],
                }
                .api(),
            );
            let answer = ran(
                &task,
                "for i in find(tasks.Issue).filter(i -> i.assignee == \"ada\") {\n    \
             print(i.title)\n}",
            );
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(answer["output"], json!(["Ship the reads"]), "{answer:#}");
            closed(&api);
            let calls = calls.join().expect("the API");
            // Its directory, its document's resources and the issues beside it; the two the
            // predicate reads, each by its own `read`; and the one the program prints, addressed
            // again.
            assert_eq!(calls.len(), 13, "{calls:#?}");
        }

        phase a_program_reads_a_native_apps_channels;
        {
            let (task, _reader, api) = task_with(&[chat()]);
            // Chat's document reads itself and takes an item: its channels are read through it.
            let calls = scripted(
                &api,
                Reads {
                    app: "so.tilo.chat",
                    record: "chat.workspace",
                    reading: &[],
                    beside: None,
                    rows: vec![
                        row("chat.workspace/channels/general", "general"),
                        row("chat.workspace/channels/design", "design"),
                    ],
                    items: vec![
                        (
                            "chat.workspace/channels/general",
                            json!({"name": "general", "kind": "text", "archived": false}),
                        ),
                        (
                            "chat.workspace/channels/design",
                            json!({"name": "design", "kind": "voice", "archived": true}),
                        ),
                    ],
                }
                .api(),
            );
            let answer = ran(
                &task,
                "for c in find(chat.Conversation) {\n    print(c.name, c.kind, c.archived)\n}",
            );
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(
                answer["output"],
                json!(["design .voice 1", "general .text 0"]),
                "{answer:#}"
            );
            closed(&api);
            let calls = calls.join().expect("the API");
            // The app listed its directory, its document's resources, and for each channel the
            // program read, the directory once more and the question of what reads it: its document
            // does, taking the channel as its `item`. The listing read no channel's fields.
            assert_eq!(calls.len(), 10, "{calls:#?}");
        }

        phase a_program_reads_the_reading_the_session_publishes;
        {
            // A resource the session publishes itself (the styles it shows) is read within the
            // program: the host asks its API for the newest reading, and goes on with it, so one
            // call reads the style ids and may switch the style itself.
            let (task, _reader, api) = task();
            let calls = scripted(&api, |message| match message.get("reading") {
                Some(reading) => {
                    assert_eq!(reading["app"], "tilo.styles", "{message:#}");
                    assert_eq!(reading["resource"], "tilo.styles", "{message:#}");
                    json!({"ok": {"reading": {"offered": [
                        {"style": "so.tilo.shell:strips", "package": "so.tilo.shell",
                         "name": "strips", "line": "The strips", "loads": true, "reason": ""},
                    ]}}})
                }
                None => json!({"err": format!("the API is asked only for a reading: {message}")}),
            });
            let answer = ran(
                &task,
                "for s in styles.installed.offered {\n    print(s.style)\n}",
            );
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(
                answer["output"],
                json!(["so.tilo.shell:strips"]),
                "{answer:#}"
            );
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(
                calls,
                [json!({"reading": {
                    "app": "tilo.styles",
                    "resource": "tilo.styles",
                    "type": "Styles",
                }})],
                "the reading asked for once, and nothing else: {calls:#?}"
            );
        }

        phase a_read_of_a_reading_shows_its_fields;
        {
            // `read(styles.installed)` of a resource the session publishes itself is the reading's
            // fields, as a view of it reads them: no app is asked, and no record is read.
            let (task, _reader, api) = task();
            let calls = scripted(&api, |message| match message.get("reading") {
                Some(_) => json!({"ok": {"reading": {"offered": [
                    {"style": "so.tilo.shell:strips", "package": "so.tilo.shell",
                     "name": "strips", "line": "The strips", "loads": true, "reason": ""},
                ]}}}),
                None => json!({"err": format!("the API is asked only for a reading: {message}")}),
            });
            let answer = ran(&task, "read(styles.installed)");
            assert_eq!(answer["state"], "done", "{answer:#}");
            let text = answer["output"][0].as_str().unwrap_or_default().to_owned();
            assert!(
                text.contains(".offered") && text.contains("tilo.styles.offered.0"),
                "the reading's field is read as the view reads it: {text}"
            );
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(
                calls.len(),
                1,
                "the reading asked once, no app asked: {calls:#?}"
            );
        }

        phase a_platform_root_with_no_service_reads_as_it_starts;
        {
            // The session publishes a platform root no service of this device serves (no battery,
            // no Bluetooth) empty, as the shell's own parts are handed it: a program reads its
            // fields as absent and goes on, exactly as a view of it draws the device without it.
            let (task, _reader, api) = task();
            let calls = scripted(&api, |message| match message.get("reading") {
                Some(_) => json!({"ok": {"reading": {}}}),
                None => json!({"err": format!("the API is asked only for a reading: {message}")}),
            });
            let answer = ran(&task, "print(device.time.clock, device.time.year)");
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(answer["output"], json!([" 0"]), "{answer:#}");
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(calls.len(), 1, "the one resource asked once: {calls:#?}");
        }

        phase a_reading_the_api_has_none_of_reads_as_it_starts;
        {
            // A resource the session publishes nothing of answers `none`, and reads as its type
            // starts, never as a fault of the program: a device without Wi-Fi publishes no
            // networks, and a view draws it so.
            let (task, _reader, api) = task();
            let calls = scripted(&api, |message| match message.get("reading") {
                Some(_) => json!({"none": true}),
                None => json!({"err": format!("the API is asked only for a reading: {message}")}),
            });
            let answer = ran(&task, "print(device.time.clock, device.time.year)");
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(answer["output"], json!([" 0"]), "{answer:#}");
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(calls.len(), 1, "asked once: {calls:#?}");
        }

        phase a_native_read_the_policy_refuses_fails_the_program_with_why;
        {
            let (task, _reader, api) = task_with(&[tasks()]);
            let calls = scripted(&api, |message| {
                if let Some(listing) = message.get("children") {
                    let rows = match listing["resource"].as_str() {
                        Some("") => vec![document_row("tasks.tracker", "Tasks")],
                        Some("bound:tasks.tracker") => vec![json!({
                            "id": "chain:issues", "kind": "tasks.issues",
                            "key": "chain:issues", "name": "Issues",
                        })],
                        _ => vec![row("tasks.issues/issues/one", "Ship it")],
                    };
                    return json!({"ok": {"rows": rows}});
                }
                if message.get("reads").is_some() {
                    return json!({"ok": {"reads": true}});
                }
                json!({"refused": "these are a person's private tasks"})
            });
            let answer = ran(&task, "for i in find(tasks.Issue) {\n    print(i.title)\n}");
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(calls.len(), 8, "{calls:#?}");
            assert_eq!(answer["state"], "failed", "{answer:#}");
            let why = answer["detail"].as_str().unwrap_or_default();
            assert!(
                why.contains("so.tilo.tasks refused the read: these are a person's private tasks"),
                "{why}"
            );
        }

        phase a_program_names_a_native_apps_fields_and_reads_no_other;
        {
            let (task, _reader, _api) = task_with(&[chat()]);
            // A field the app does not publish is an error in the program, as in `tilolang check`.
            let wrong = ran(
                &task,
                "for c in find(chat.Conversation) {\n    print(c.colour)\n}",
            );
            assert_eq!(wrong["state"], "rejected", "{wrong:#}");
        }

        phase a_native_apps_document_is_a_root_a_program_names;
        {
            let (task, _reader, _api) = task_with(&[chat(), tasks()]);
            let catalogue = task.session.catalogue();
            let roots = |app: &str| -> Vec<(String, String)> {
                catalogue[app]["signatures"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|signature| signature["kind"] == "root")
                    .map(|signature| {
                        (
                            signature["name"].as_str().unwrap_or_default().to_owned(),
                            signature["call"].as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            };
            let named = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
                pairs
                    .iter()
                    .map(|(name, call)| ((*name).to_owned(), (*call).to_owned()))
                    .collect()
            };
            // The app the program invokes the document of, and how it writes it; then each
            // resource the app keeps a collection in beside it (chat's messages and skills, the
            // tracker's issues), whose verbs make and change what it holds. What Chat learned is
            // the person's own, kept in their session, and the workspace publishes none of it.
            assert_eq!(
                roots("chat"),
                named(&[
                    ("workspace", "chat.workspace"),
                    ("messages", "chat.messages"),
                    ("skills", "chat.skills"),
                ]),
                "{catalogue:#}"
            );
            assert_eq!(
                roots("tasks"),
                named(&[("tracker", "tasks.tracker"), ("issues", "tasks.issues")]),
                "{catalogue:#}"
            );
        }

        phase a_program_reads_what_a_native_app_keeps_beside_its_document;
        {
            // The tracker keeps its issues in parts beside it, one resource the app's directory
            // lists under the tracker: the root `tasks.issues` lists them, and each is read by the
            // id it is listed by.
            let (task, _reader, api) = task_with(&[tasks()]);
            let calls = scripted(&api, |message| {
                if let Some(listing) = message.get("children") {
                    let rows = match listing["resource"].as_str() {
                        Some("") => vec![document_row("tasks.tracker", "Tasks")],
                        Some("bound:tasks.tracker") => vec![json!({
                            "id": "chain:issues", "kind": "tasks.issues",
                            "key": "chain:issues", "name": "Issues",
                        })],
                        _ => vec![
                            row("tasks.issues/issues/one", "Ship it"),
                            row("tasks.issues/issues/two", "Review it"),
                        ],
                    };
                    return json!({"ok": {"rows": rows}});
                }
                if message.get("reads").is_some() {
                    return json!({"ok": {"reads": false}});
                }
                let item = message["invoke"]["arguments"]["item"].as_str();
                let title = match item {
                    Some("tasks.issues/issues/one") => "Ship it",
                    Some("tasks.issues/issues/two") => "Review it",
                    _ => return json!({"ok": {"value": {}, "resources": []}}),
                };
                json!({"ok": {"value": {"title": title}, "resources": []}})
            });
            let answer = ran(
                &task,
                "for i in tasks.issues.issues {\n    print(i.title)\n}",
            );
            closed(&api);
            calls.join().expect("the API");
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(
                answer["output"],
                json!(["Ship it", "Review it"]),
                "{answer:#}"
            );
        }
    }

    tilo_test_env::scenario! {
        fn a_program_changes_native_apps_only_through_actions_they_take;

        phase a_program_changes_native_apps_through_their_actions;
        {
            let (task, _reader, api) = task_with(&[chat(), tasks()]);
            let calls = answering(api, 2, r#"{"ok": {}}"#);
            let answer = ran(
                &task,
                "chat.rename_channel(chat.workspace, \"general\", \"town\")\ntasks.create_area(tasks.tracker, \"Launch\")",
            );
            let calls = calls.join().expect("the API");
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(
                calls,
                [
                    json!({
                        "app": "so.tilo.chat",
                        "resource": "chat.workspace",
                        "action": "rename-channel",
                        "arguments": {"channel": "general", "name": "town"},
                    }),
                    json!({
                        "app": "so.tilo.tasks",
                        "resource": "tasks.tracker",
                        "action": "create-area",
                        "arguments": {"name": "Launch", "kind": null},
                    }),
                ]
            );
            assert_eq!(answer["changed"], 2, "{answer:#}");
            // Both steps reach the task's history on the documents the apps publish, so `undo task`
            // takes them back like any tl app's.
            assert_eq!(
                answer["history"],
                json!([
                    {"app": "so.tilo.chat", "resource": "chat.workspace", "published": false, "invocation": null},
                    {"app": "so.tilo.tasks", "resource": "tasks.tracker", "published": false, "invocation": null},
                ])
            );
        }

        phase a_program_invokes_what_a_native_app_keeps_beside_its_document;
        {
            // An issue is made on the issues the tracker keeps beside it, invoked by the id its
            // directory lists them by; the step is taken back through the tracker.
            let (task, _reader, api) = task_with(&[tasks()]);
            let calls = scripted(&api, |message| {
                if let Some(listing) = message.get("children") {
                    let rows = match listing["resource"].as_str() {
                        Some("") => vec![document_row("tasks.tracker", "Tasks")],
                        _ => vec![json!({
                            "id": "chain:issues", "kind": "tasks.issues",
                            "key": "chain:issues", "name": "Issues",
                        })],
                    };
                    return json!({"ok": {"rows": rows}});
                }
                json!({"ok": {}})
            });
            let answer = ran(&task, "tasks.create(tasks.issues, \"Ship it\")");
            closed(&api);
            let calls = calls.join().expect("the API");
            assert_eq!(answer["state"], "done", "{answer:#}");
            let invoked: Vec<&Json> = calls.iter().filter_map(|call| call.get("invoke")).collect();
            assert_eq!(invoked.len(), 1, "{calls:#?}");
            assert_eq!(invoked[0]["resource"], "chain:issues", "{calls:#?}");
            assert_eq!(invoked[0]["action"], "create", "{calls:#?}");
            assert_eq!(
                answer["history"],
                json!([{"app": "so.tilo.tasks", "resource": "chain:issues", "published": false, "invocation": null}]),
                "{answer:#}"
            );
        }

        phase a_native_action_the_app_refuses_fails_the_program_and_changes_nothing;
        {
            let (task, _reader, api) = task_with(&[chat()]);
            let calls = answering(
                api,
                1,
                r#"{"refused": "the workspace asks its members before renaming"}"#,
            );
            let answer = ran(
                &task,
                "chat.rename_channel(chat.workspace, \"general\", \"town\")",
            );
            let calls = calls.join().expect("the API");
            assert_eq!(calls.len(), 1, "{calls:#?}");
            assert_eq!(answer["state"], "failed", "{answer:#}");
            let why = answer["detail"].as_str().unwrap_or_default();
            assert!(why.contains("asks its members before renaming"), "{why}");
            assert_eq!(answer["changed"], 0, "{answer:#}");
            assert_eq!(answer["history"], json!([]), "{answer:#}");
        }
    }

    tilo_test_env::scenario! {
        fn a_task_reaches_the_apps_its_session_and_catalogue_name;

        phase a_task_names_only_the_apps_its_session_hands_it;
        {
            let (task, _reader, _api) = task_with(&[]);
            let answer = ran(
                &task,
                "for c in find(chat.Conversation) {\n    print(c.name)\n}",
            );
            assert_eq!(answer["state"], "rejected", "{answer:#}");
            let why = format!("{:#}", answer["detail"]);
            assert!(
                why.contains("chat") || why.contains("chat.Conversation"),
                "{why}"
            );
        }

        phase a_package_that_does_not_load_leaves_only_its_app_unreachable;
        {
            // One installed app among the rest whose package does not load here, as a module built
            // for another runtime, or a payload damaged on its way to the device, hands one over.
            let mut broken = counter();
            let gone = broken.meta.app.clone();
            broken.wasm = b"not a tilolang module".to_vec();
            let (task, _reader, api) = task_over(&[discussion(), broken], &[]);
            // The task starts: the catalogue lists the rest of the apps, and says of the broken one
            // what a program naming it reads.
            let catalogue = task.session.catalogue();
            let alias = listed(&catalogue, &gone);
            let said = catalogue[&alias]["unreachable"]
                .as_str()
                .unwrap_or_else(|| panic!("{gone} is not reported: {catalogue:#}"))
                .to_owned();
            assert!(said.starts_with("can't be reached: "), "{said}");
            let discussion = listed(&catalogue, "so.tilo.lang.discussion");
            let signatures = catalogue[&discussion]["signatures"]
                .as_array()
                .into_iter()
                .flatten()
                .collect::<Vec<&Json>>();
            let root = signatures
                .iter()
                .find(|signature| signature["kind"] == "root")
                .and_then(|signature| signature["call"].as_str())
                .expect("discussion names its root");
            let comment = signatures
                .iter()
                .find(|signature| signature["kind"] == "action" && signature["name"] == "comment")
                .unwrap_or_else(|| panic!("discussion lists no comment action: {catalogue:#}"));
            // The API takes it by the name it is published under.
            let comment = comment["name"].as_str().unwrap_or_default().to_owned();
            // Its actions resolve, and invoking one runs: the call goes to the app through the API,
            // which answers the comment it made.
            let called = scripted(&api, |message| {
                if message.get("record").is_some() {
                    return json!({"absent": true});
                }
                json!({
                    "ok": {
                        "value": null,
                        "resources": ["tl:so.tilo.lang.discussion:hello"],
                    },
                })
            });
            let answer = ran(
                &task,
                &format!("{discussion}.comment({root}, RichText(\"hello\"))"),
            );
            assert_eq!(answer["state"], "done", "{answer:#}");
            assert_eq!(answer["changed"], 1, "{answer:#}");
            closed(&api);
            let calls = called.join().expect("the API");
            let invoked = calls
                .iter()
                .find(|call| call.get("invoke").is_some())
                .unwrap_or_else(|| panic!("discussion's action was not invoked: {calls:#?}"));
            assert_eq!(
                invoked["invoke"]["app"], "so.tilo.lang.discussion",
                "{calls:#?}"
            );
            assert_eq!(invoked["invoke"]["action"], comment, "{calls:#?}");
            // A program naming the app that did not load is rejected, as one naming no app is, and
            // the task goes on with the apps it reaches.
            let unknown = ran(&task, &format!("{alias}.messages"));
            assert_eq!(unknown["state"], "rejected", "{unknown:#}");
            let nothing = ran(&task, "print(1)");
            assert_eq!(nothing["state"], "done", "{nothing:#}");
        }

        phase check_and_a_task_compile_against_the_same_catalogue;
        {
            let checked = checked();
            let program =
                "for c in find(chat.Conversation) {\n    print(c.name)\n    print(c.colour)\n}";
            let Err(diagnostics) = tilolang::compile_program_reading(program, &checked, &[], &[])
            else {
                panic!("`colour` is no field of a chat conversation");
            };
            let expected: Vec<String> = diagnostics
                .iter()
                .map(|d| format!("{}:{}: {} {}", d.line, d.col, d.code, d.message))
                .collect();
            let (task, _reader, _api) = task_with(&[chat()]);
            let answer = ran(&task, program);
            assert_eq!(answer["state"], "rejected", "{answer:#}");
            assert_eq!(answer["detail"], serde_json::json!(expected), "{answer:#}");
        }
    }

    /// The bundle of the package `source` declares, its ids lock `ids`.
    fn bundle(source: &str, ids: &str) -> Bundle {
        let lock = tilolang::IdsLock::parse(ids).expect("the ids lock reads");
        // Against the platform's packages, as the host compiles an app: the workspace's
        // package an app's person pickers read its members through among them.
        let platform =
            tilolang_tilo::platform_apps::platform_packages().expect("the platform reads");
        let compiled =
            tilolang::compile_package(source, &lock, &platform).expect("the package compiles");
        Bundle::new(&compiled, [])
    }

    /// Discussion, as its directory publishes it.
    fn discussion() -> Bundle {
        bundle(
            include_str!("../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../apps/tl/discussion/app.ids"),
        )
    }

    /// Counter, as its directory publishes it.
    fn counter() -> Bundle {
        bundle(
            include_str!("../../../../apps/tl/counter/app.tl"),
            include_str!("../../../../apps/tl/counter/app.ids"),
        )
    }

    /// The app the catalogue lists `app` as, by the name a program names it with.
    fn listed(catalogue: &Json, app: &str) -> String {
        let found = catalogue
            .as_object()
            .into_iter()
            .flatten()
            .find(|(_, listed)| listed["app"] == app);
        match found {
            Some((alias, _)) => alias.clone(),
            None => panic!("{app} is not listed: {catalogue:#}"),
        }
    }

    /// `tilolang check`'s packages: what the apps' directories publish.
    fn checked() -> Vec<PackageMeta> {
        let chat = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tilolang-tilo/tests/native-chat");
        tilolang_tilo::catalogue::published_packages(&[chat]).expect("the surfaces read")
    }
}

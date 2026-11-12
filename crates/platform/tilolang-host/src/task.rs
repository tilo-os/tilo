//! The host of an agent task: its programs, their author the task
//! (`docs/designs/app-language/agents.md`, "How a program runs").
//!
//! The agent door starts it on the task's first `tilo` call and it lives as long as the task:
//! until the door closes the task's program connection. It holds the task's session (the `let`s
//! its programs bound) and no records, opens no windows and no broker connection of its own.
//! Every program comes in on the program connection, and everything a program reads or changes
//! goes out on it to the door, which reads and invokes as the task: the door's connection is the
//! task's custody, so nothing here is trusted to say who it is.
//!
//! The connection carries one JSON object per line. The door sends `{"run": {source, confirm}}`
//! or `{"catalogue": {}}` (answered `{"catalogue": …}`: every installed app's declarations,
//! [`TaskSession::catalogue`], and each app whose package did not load with what a program
//! naming it is told, `can't be reached: <why>`);
//! while the program runs, the host asks `{"record": {app, record}}` for a record it reads
//! (answered `{"ok": {"document": base64}}`, `{"absent": true}` or `{"err": why}`: the record's
//! committed content, read-only, which the host reads itself with the app's package,
//! [`RecordReads`]), `{"children": {app, resource}}` for the resources a native app lists under
//! one of its own (answered `{"ok": {"rows": [{id, kind, key, name}]}}` or `{"err": why}`: the
//! app's directory, the record at its top and what it holds under it: a row's `id` names it
//! under its parent, which is what an invocation addresses, and its `key` is what the app's
//! `read` takes as its item), `{"reads": {app, resource}}` for whether a resource the app
//! publishes answers a read of itself (answered `{"ok": {"reads": bool}}` or `{"err": why}`: a
//! resource that does not is read by the record, which takes it as its `item`) and
//! `{"invoke": {app, resource, action, arguments}}` (answered `{"ok": output}`, `{"failed": why}`,
//! `{"refused": why}` or `{"asks": why}`), and it ends with `{"answer": …}`
//! ([`tilolang_tilo::task::TaskSession::run`]). A native app's record is read by the app itself
//! ([`tilolang_tilo::native`]), so the host's read of one is its `read` action, invoked like any
//! other.

use std::cell::RefCell;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use base64::Engine as _;
use runtime_portal::tilolang_host::{App, TaskFds, TaskRequest, decode_apps};
use serde_json::{Value as Json, json};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::catalogue::NativeSurface;
use tilolang_tilo::native::{Calls, Native, NativeReads};
use tilolang_tilo::runtime::meta::PackageMeta;
use tilolang_tilo::task::{Answered, TaskDoor, TaskSession, Unreachable};
use tilolang_tilo::task_records::{RecordReads, RecordSource};

/// The program connection, one line at a time.
struct Line {
    reader: RefCell<BufReader<UnixStream>>,
    writer: RefCell<UnixStream>,
}

impl Line {
    fn send(&self, message: &Json) -> Result<(), String> {
        let mut line = serde_json::to_vec(message).map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.writer
            .borrow_mut()
            .write_all(&line)
            .map_err(|error| format!("the door went away: {error}"))
    }

    /// The next message: `None` once the door closed the connection (or it broke, which is
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
                    .map_err(|error| format!("the door sent a line that is not JSON: {error}")),
            ),
        }
    }

    fn ask(&self, request: &Json) -> Result<Json, String> {
        self.send(request)?;
        self.next()
            .ok_or_else(|| "the door closed the task".to_owned())?
    }
}

impl RecordSource for Line {
    fn record(&self, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        let reply = self.ask(&json!({"record": {"app": app, "record": record}}))?;
        if reply.get("absent").and_then(Json::as_bool) == Some(true) {
            return Ok(None);
        }
        if let Some(document) = reply.pointer("/ok/document").and_then(Json::as_str) {
            return base64::engine::general_purpose::STANDARD
                .decode(document)
                .map(Some)
                .map_err(|error| format!("the door sent a record that is not base64: {error}"));
        }
        Err(reply.get("err").and_then(Json::as_str).map_or_else(
            || format!("the door answered a record read with neither ok, absent nor err: {reply}"),
            str::to_owned,
        ))
    }
}

/// What the task's programs reach the workspace through: the records, read here from what the
/// door hands over ([`RecordReads`]), a native app's record, read by the app itself
/// ([`NativeReads`]), and invocations, which go to the door.
struct Door {
    line: Rc<Line>,
    reads: RecordReads,
    /// The native apps the task reaches, and the record each publishes.
    native: Rc<Native>,
    /// The author of the program running now, as the actions it plans for a preview see their
    /// caller.
    caller: RefCell<tilolang_tilo::publish::Caller>,
    /// The apps whose records the session changes itself; any other app's change is its own
    /// action, invoked where the app runs.
    changed_at_record: std::collections::BTreeSet<String>,
}

impl TaskDoor for Door {
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

    fn begin_program(&self) {
        // Each program reads the records anew, as the door decides them now.
        self.reads.forget();
    }

    fn invoke(&self, call: &Json) -> Answered {
        // A change of a record the session changes itself goes there with the record it changes,
        // and what the task's read of it would write, which the session compares its run with.
        // Any other app's change is its action, invoked at the app.
        let mut call = call.clone();
        if let Some(app) = call.get("app").and_then(Json::as_str).map(str::to_owned)
            && self.changed_at_record.contains(&app)
            && let Some((under, record)) = self.reads.address(&app)
        {
            let preview = self
                .reads
                .plan(&call, &self.caller.borrow())
                .unwrap_or(Json::Null);
            if let Some(fields) = call.as_object_mut() {
                fields.insert("app".to_owned(), json!(under));
                fields.insert("record".to_owned(), json!(record));
                fields.insert("preview".to_owned(), preview);
            }
        }
        let reply = match self.line.ask(&json!({"invoke": call})) {
            Ok(reply) => reply,
            Err(why) => return Answered::Failed(why),
        };
        // What the program reads of the app next holds what its change made: the origin says
        // what it wrote, which goes into the record this program read.
        if let Some(app) = call
            .get("app")
            .and_then(Json::as_str)
            .filter(|app| !self.native.is(app))
        {
            let wrote = reply.get("change").and_then(Line::change_bytes);
            let taken = match wrote {
                Some(Ok(change)) => self.reads.take_in(app, &change),
                Some(Err(error)) => Err(format!("the origin's change does not read: {error}")),
                None => Err("no change said".to_owned()),
            };
            if let Err(why) = taken {
                tracing::debug!(app, why, "the program reads the record again");
                self.reads.forget_app(app);
            }
        }
        Line::answered(&reply)
    }
}

impl Calls for Door {
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
            || format!("the door answered a listing with neither ok nor err: {reply}"),
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
            || format!("the door answered a read question with neither ok nor err: {reply}"),
            str::to_owned,
        ))
    }
}

impl Line {
    /// The bytes a door's `change` field holds: the record's whole document, as base64 text or,
    /// for the record a package the session runs itself keeps, as its bytes in JSON.
    fn change_bytes(change: &Json) -> Option<Result<Vec<u8>, String>> {
        match change {
            Json::String(text) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|error| error.to_string()),
            ),
            Json::Array(bytes) => Some(
                bytes
                    .iter()
                    .map(|byte| {
                        byte.as_u64()
                            .and_then(|byte| u8::try_from(byte).ok())
                            .ok_or_else(|| "a change is bytes".to_owned())
                    })
                    .collect::<Result<Vec<u8>, String>>(),
            ),
            _ => None,
        }
    }

    /// How the door's `reply` to an invocation says it ended.
    fn answered(reply: &Json) -> Answered {
        let why = |key: &str| {
            reply
                .get(key)
                .and_then(Json::as_str)
                .map_or_else(|| format!("the door gave no reason ({key})"), str::to_owned)
        };
        if let Some(output) = reply.get("ok") {
            Answered::Done(output.clone())
        } else if reply.get("refused").is_some() {
            Answered::Refused(why("refused"))
        } else if reply.get("asks").is_some() {
            Answered::Asks(why("asks"))
        } else if reply.get("failed").is_some() {
            Answered::Failed(why("failed"))
        } else {
            Answered::Failed(format!(
                "the door answered the invocation with neither ok, failed, refused nor asks: \
                 {reply}"
            ))
        }
    }
}

/// One other connection into a task's session: the door's own, which started the host, or one
/// the door opened later for the person's lines in the run's conversation (shell.md §8.1).
struct Guest {
    line: Rc<Line>,
    door: Rc<Door>,
    /// Its connection, watched by the host's wait: a line arriving wakes the host.
    watched: OwnedFd,
}

/// A task this host runs the programs of.
pub struct Task {
    line: Rc<Line>,
    /// The program connection, watched for what the door sends.
    watched: OwnedFd,
    /// Further connections into the same session, each given when the host opened: the
    /// person's lines in the run's conversation run here, beside the agent's own calls, so the
    /// two share the session's `let`s. Every program still runs one at a time.
    guests: RefCell<Vec<Guest>>,
    session: TaskSession,
    door: Rc<Door>,
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
        person: Option<UnixStream>,
    ) -> Result<Self, String> {
        let apps = apps(&fds.apps, request.apps_len)?;
        let stream = UnixStream::from(fds.program);
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let watched = OwnedFd::from(stream.try_clone().map_err(|error| error.to_string())?);
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(stream)),
            writer: RefCell::new(writer),
        });
        let reads = RecordReads::new(
            &apps.bundles,
            request.records_under.clone(),
            request.person.as_deref(),
            Rc::clone(&line) as Rc<dyn RecordSource>,
        );
        let native = Rc::new(Native::of(&apps.bundles, &apps.native));
        let door = Rc::new(Door {
            line: Rc::clone(&line),
            reads,
            native: Rc::clone(&native),
            // Each program names its author; this is no one's until the first does.
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: String::new(),
                agent: true,
                on_behalf_of: request.person.clone(),
            }),
            changed_at_record: request.changed_at_record.clone(),
        });
        let session = TaskSession::new(
            request.person.as_deref(),
            &(Rc::clone(&door) as Rc<dyn TaskDoor>),
            &apps.bundles,
            native,
        )?;
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
            door,
        };
        if let Some(person) = person {
            task.join(person)?;
        }
        Ok(task)
    }

    /// Let `stream` join this session as a guest: what it sends runs in the same session, one
    /// program at a time, and what the program reads or changes goes back on it.
    pub fn join(&self, stream: UnixStream) -> Result<(), String> {
        stream
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let watched = OwnedFd::from(stream.try_clone().map_err(|error| error.to_string())?);
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(stream)),
            writer: RefCell::new(writer),
        });
        let door = Rc::new(Door {
            line: Rc::clone(&line),
            reads: RecordReads::new(
                &[],
                std::collections::BTreeMap::new(),
                None,
                Rc::clone(&line) as Rc<dyn RecordSource>,
            ),
            native: Rc::new(Native::of(&[], &[])),
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: String::new(),
                agent: true,
                on_behalf_of: None,
            }),
            changed_at_record: self.door.changed_at_record.clone(),
        });
        self.guests.borrow_mut().push(Guest {
            line,
            door,
            watched,
        });
        Ok(())
    }

    /// The program connection, to wait on.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.watched.as_fd()
    }

    /// Copies of the guest connections' descriptors, to wait on: a line arriving on one wakes
    /// the host as the door's own connection would.
    pub fn guest_fds(&self) -> Vec<OwnedFd> {
        self.guests
            .borrow()
            .iter()
            .filter_map(|guest| guest.watched.try_clone().ok())
            .collect()
    }

    /// Answer what the door sent: run each program and say how it went. `false` once the door
    /// closed the task.
    ///
    /// Everything the door sends is answered: a line that is not a program to run is answered
    /// `failed` with why, so the door never waits on a message the host dropped. Every line
    /// already read off the connection is served: a line left in the reader's buffer would wake
    /// nothing, and wait until the door sent more.
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

    /// Answer every connection that has something: the door's own first, then the guests it
    /// opened. Waits on all of them, so a person's line is served as soon as it arrives and
    /// never waits on the agent's next call. `false` once the door's own connection closed.
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

    /// Block until the door's connection or a guest's has something to read, saying which: `0`
    /// for the door's own, `at + 1` for the guest at `at`. `None` once the door's connection
    /// closed.
    fn wait_ready(&self) -> Option<Vec<usize>> {
        use std::os::fd::AsRawFd as _;
        let door = self.watched.as_raw_fd();
        let guests = self.guest_fds();
        let mut fds = Vec::with_capacity(guests.len().saturating_add(1));
        fds.push(libc::pollfd {
            fd: door,
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
            // The door's own connection closing ends the task; a guest's closing is served as
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

    /// Serve one line of the guest at `at`: `false` once its connection closed. The guest's own
    /// line and door are taken out of the list's borrow, so a program that serves the door's
    /// connection too is never held under it.
    fn serve_guest_at(&self, at: usize) -> bool {
        let (line, door) = {
            let guests = self.guests.borrow();
            let Some(guest) = guests.get(at) else {
                return false;
            };
            (Rc::clone(&guest.line), Rc::clone(&guest.door))
        };
        self.serve_guest(&line, &door)
    }

    /// Serve one line of a connection: `false` once it closed.
    fn serve_guest(&self, guest: &Rc<Line>, door: &Rc<Door>) -> bool {
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
            Ok((source, confirm, author)) => {
                let principal = tilolang_tilo::runtime::Principal::new(author.actor.clone());
                *door.caller.borrow_mut() = author;
                self.session.run(&source, confirm, &principal)
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
                    tracing::warn!(%error, "the catalogue did not reach the door");
                    false
                }
            };
        }
        let answer = match message.and_then(|message| program(&message)) {
            Ok((source, confirm, author)) => {
                let principal = tilolang_tilo::runtime::Principal::new(author.actor.clone());
                *self.door.caller.borrow_mut() = author;
                self.session.run(&source, confirm, &principal)
            }
            Err(why) => {
                tracing::warn!(
                    why,
                    "the door sent the task something that is not a program"
                );
                json!({"kind": "program", "state": "failed", "detail": why})
            }
        };
        if let Err(error) = self.line.send(&json!({ "answer": answer })) {
            tracing::warn!(%error, "a program's answer did not reach the door");
            return false;
        }
        true
    }
}

/// The program `message` asks the task to run, whether its changes are confirmed, and who wrote
/// it: the door names the author of every program, the agent or the person typing.
fn program(message: &Json) -> Result<(String, bool, tilolang_tilo::publish::Caller), String> {
    let run = message.get("run").ok_or_else(|| {
        format!("the door sent the task something other than a program: {message}")
    })?;
    let source = run
        .get("source")
        .and_then(Json::as_str)
        .ok_or("the door sent a program without its source")?;
    // An absent `confirm` is a run not confirmed yet: its changes are previewed first.
    let confirm = run.get("confirm").and_then(Json::as_bool).unwrap_or(false);
    let author = run
        .get("author")
        .ok_or("the door sent a program without its author")?;
    let actor = author
        .get("actor")
        .and_then(Json::as_str)
        .filter(|actor| !actor.is_empty())
        .ok_or("the door sent a program whose author names no one")?;
    let author = tilolang_tilo::publish::Caller {
        actor: actor.to_owned(),
        agent: author.get("agent").and_then(Json::as_bool).unwrap_or(true),
        on_behalf_of: author
            .get("on_behalf_of")
            .and_then(Json::as_str)
            .map(str::to_owned),
    };
    Ok((source.to_owned(), confirm, author))
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

/// The apps the sealed memfd `fd` holds, `len` bytes of them.
fn apps(fd: &OwnedFd, len: u64) -> Result<Apps, String> {
    let len = usize::try_from(len).map_err(|error| error.to_string())?;
    let mapping = tilo_render_protocol::shm::Mapping::map_sealed(fd, true, len, len)
        .map_err(|error| error.to_string())?;
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

    /// A task over no packages, its program connection's far end returned as the door's.
    fn task() -> (Task, BufReader<UnixStream>, UnixStream) {
        task_with(&[])
    }

    /// A task over the apps `native`, read as packages from their surfaces, its program
    /// connection's far end returned as the door's.
    fn task_with(native: &[PackageMeta]) -> (Task, BufReader<UnixStream>, UnixStream) {
        task_over(&[], native)
    }

    /// A task over the packages `bundles` and the apps `native`, its program connection's far
    /// end returned as the door's.
    fn task_over(
        bundles: &[Bundle],
        native: &[PackageMeta],
    ) -> (Task, BufReader<UnixStream>, UnixStream) {
        let (ours, door) = UnixStream::pair().expect("a socket pair");
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(ours.try_clone().expect("a clone"))),
            writer: RefCell::new(ours.try_clone().expect("a clone")),
        });
        let apps = Rc::new(Native::of(bundles, native));
        let reached = Rc::new(Door {
            line: Rc::clone(&line),
            reads: RecordReads::new(
                bundles,
                std::collections::BTreeMap::new(),
                None,
                Rc::clone(&line) as Rc<dyn RecordSource>,
            ),
            native: Rc::clone(&apps),
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: "agent:task".to_owned(),
                agent: true,
                on_behalf_of: None,
            }),
            changed_at_record: std::collections::BTreeSet::new(),
        });
        let session = TaskSession::new(
            None,
            &(Rc::clone(&reached) as Rc<dyn TaskDoor>),
            bundles,
            apps,
        )
        .expect("a session");
        let task = Task {
            line,
            watched: OwnedFd::from(ours),
            guests: RefCell::new(Vec::new()),
            session,
            door: reached,
        };
        (
            task,
            BufReader::new(door.try_clone().expect("a clone")),
            door,
        )
    }

    fn answer(reader: &mut BufReader<UnixStream>) -> Json {
        let mut line = String::new();
        reader.read_line(&mut line).expect("an answer");
        serde_json::from_str(&line).expect("a JSON answer")
    }

    #[test]
    fn a_line_that_is_not_json_is_answered_failed() {
        let (task, mut reader, mut door) = task();
        door.write_all(b"{not json\n").expect("sent");
        assert!(task.serve(), "the task goes on");
        let answer = answer(&mut reader);
        assert_eq!(answer["answer"]["state"], "failed");
        let detail = answer["answer"]["detail"].as_str().unwrap_or_default();
        assert!(detail.contains("not JSON"), "{detail}");
    }

    #[test]
    fn a_message_that_is_not_a_program_is_answered_failed() {
        let (task, mut reader, mut door) = task();
        door.write_all(b"{\"walk\": {}}\n").expect("sent");
        assert!(task.serve());
        assert_eq!(answer(&mut reader)["answer"]["state"], "failed");
        door.write_all(b"{\"run\": {\"confirm\": true}}\n")
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

    #[test]
    fn an_unreadable_reply_fails_the_request_with_why() {
        let (task, mut reader, mut door) = task();
        let asking = std::thread::spawn(move || {
            let mut asked = String::new();
            reader.read_line(&mut asked).expect("asked");
            door.write_all(b"garbage\n").expect("sent");
            (reader, door)
        });
        let read = task.line.record("app", "");
        let _ends = asking.join().expect("the door");
        let why = read.expect_err("an unreadable reply fails the read");
        assert!(why.contains("not JSON"), "{why}");
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
            include_str!("../../../../apps/chat/surface.toml"),
        )
    }

    /// Tasks, as the app publishes it.
    fn tasks() -> PackageMeta {
        native(
            "so.tilo.tasks",
            "Tasks",
            include_str!("../../../../apps/tasks/surface.toml"),
        )
    }

    /// Answer the `count` invocations the door is sent with `answer` (one JSON line), and hand
    /// back the invocations it was sent.
    fn answering(
        door: UnixStream,
        count: usize,
        answer: &'static str,
    ) -> std::thread::JoinHandle<Vec<Json>> {
        std::thread::spawn(move || {
            let mut reader = BufReader::new(door.try_clone().expect("a clone"));
            let mut door = door;
            let mut seen = Vec::new();
            for _ in 0..count {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("a call") == 0 {
                    break;
                }
                let message: Json = serde_json::from_str(&line).expect("a JSON call");
                seen.push(message["invoke"].clone());
                door.write_all(answer.as_bytes()).expect("answered");
                door.write_all(b"\n").expect("answered");
            }
            seen
        })
    }

    /// What a program run answered, and how it ended.
    fn ran(task: &Task, source: &str) -> Json {
        task.session.run(
            source,
            false,
            &tilolang_tilo::runtime::Principal::new("agent:task"),
        )
    }

    /// A directory row an app lists for the item `key`, which its `read` takes as its `item`,
    /// under the id the window publishing it binds it by.
    fn row(key: &str, name: &str) -> Json {
        let bound = format!("bound:{key}");
        json!({"id": bound, "kind": "item", "key": key, "name": name})
    }

    /// The row an app's directory lists its record by: the key its `read` takes, under an id the
    /// publishing window binds, which is no name the app itself knows.
    fn record_row(record: &str, name: &str) -> Json {
        json!({"id": format!("bound:{record}"), "kind": record, "key": record, "name": name})
    }

    /// An app's own answers: the resources its directory lists under its record, which of them
    /// answer a read of themselves, and what its `read` answers of each item, its model's JSON.
    struct Reads {
        app: &'static str,
        record: &'static str,
        /// The resources that answer a read of themselves, by the id the directory lists them
        /// as; every other resource is read by the record, which takes it as its `item`.
        reading: &'static [&'static str],
        rows: Vec<Json>,
        items: Vec<(&'static str, Json)>,
    }

    impl Reads {
        /// The door's answers to what a program asks of the app.
        fn door(self) -> impl FnMut(&Json) -> Json + Send + 'static {
            move |message| self.answer(message)
        }

        /// How the app answers `message`: its directory, whether a resource reads itself, or its
        /// own `read` of an item.
        fn answer(&self, message: &Json) -> Json {
            if let Some(listing) = message.get("children") {
                assert_eq!(listing["app"], self.app, "{message:#}");
                // The record sits at the top of the app's directory, the resources under it.
                let rows = match listing["resource"].as_str() {
                    Some("") => vec![record_row(self.record, self.record)],
                    Some(resource) if resource == format!("bound:{}", self.record) => {
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
            // A resource that reads itself is read by its own id; every other by the record the
            // app publishes it as, which takes the resource's key as its `item`.
            let key = if let Some(key) = call["arguments"]["item"].as_str() {
                assert_eq!(
                    call["resource"],
                    format!("bound:{}", self.record),
                    "{message:#}"
                );
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

    /// Close the test's end of the door once the program ran: the task sends nothing more.
    fn closed(door: &UnixStream) {
        door.shutdown(std::net::Shutdown::Both)
            .expect("the door closed");
    }

    /// A door that answers every message with `answer` until its far end closes (the test shuts
    /// the returned stream down once the program ran), handing back what the task sent it.
    fn scripted(
        door: &UnixStream,
        mut answer: impl FnMut(&Json) -> Json + Send + 'static,
    ) -> std::thread::JoinHandle<Vec<Json>> {
        let mut door = door.try_clone().expect("a clone");
        std::thread::spawn(move || {
            let mut reader = BufReader::new(door.try_clone().expect("a clone"));
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
                door.write_all(&reply).expect("answered");
                seen.push(message);
            }
            seen
        })
    }

    #[test]
    fn a_program_reads_a_native_apps_record_fields_through_the_app() {
        let (task, _reader, door) = task_with(&[tasks()]);
        let calls = scripted(
            &door,
            Reads {
                app: "so.tilo.tasks",
                record: "tasks.tracker",
                reading: &["bound:tasks.tracker/issues/one"],
                rows: vec![row("tasks.tracker/issues/one", "Ship the reads")],
                items: vec![(
                    "tasks.tracker/issues/one",
                    json!({
                        "number": 7,
                        "title": "Ship the reads",
                        "body": "One read path.",
                        "assignee": "person:ada",
                        "labels": ["review", "urgent"],
                    }),
                )],
            }
            .door(),
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
            json!(["Ship the reads", "person:ada", "2", "7"]),
            "{answer:#}"
        );
        closed(&door);
        let calls = calls.join().expect("the door");
        // The app listed its directory and what its record published holds; reading the
        // resource the program asked about, it listed the directory again to address that
        // resource, asked whether it reads itself, and read through its own `read`, once.
        assert_eq!(
            calls,
            [
                json!({"children": {"app": "so.tilo.tasks", "resource": ""}}),
                json!({"children": {"app": "so.tilo.tasks", "resource": "bound:tasks.tracker"}}),
                json!({"children": {"app": "so.tilo.tasks", "resource": ""}}),
                json!({"children": {"app": "so.tilo.tasks", "resource": "bound:tasks.tracker"}}),
                json!({
                    "reads": {
                        "app": "so.tilo.tasks",
                        "resource": "bound:tasks.tracker/issues/one",
                    },
                }),
                json!({
                    "invoke": {
                        "app": "so.tilo.tasks",
                        "resource": "bound:tasks.tracker/issues/one",
                        "action": "read",
                        "arguments": {"item": null, "version": null},
                    },
                }),
            ],
            "{calls:#?}"
        );
    }

    #[test]
    fn a_program_filters_a_native_apps_resources_by_their_fields() {
        let (task, _reader, door) = task_with(&[tasks()]);
        let calls = scripted(
            &door,
            Reads {
                app: "so.tilo.tasks",
                record: "tasks.tracker",
                reading: &[
                    "bound:tasks.tracker/issues/one",
                    "bound:tasks.tracker/issues/two",
                ],
                rows: vec![
                    row("tasks.tracker/issues/one", "Ship the reads"),
                    row("tasks.tracker/issues/two", "Review Ada's portfolio"),
                ],
                items: vec![
                    (
                        "tasks.tracker/issues/one",
                        json!({"title": "Ship the reads", "assignee": "person:ada"}),
                    ),
                    (
                        "tasks.tracker/issues/two",
                        json!({"title": "Review Ada's portfolio", "assignee": "person:bob"}),
                    ),
                ],
            }
            .door(),
        );
        let answer = ran(
            &task,
            "for i in find(tasks.Issue).filter(i -> i.assignee == \"person:ada\") {\n    \
             print(i.title)\n}",
        );
        assert_eq!(answer["state"], "done", "{answer:#}");
        assert_eq!(answer["output"], json!(["Ship the reads"]), "{answer:#}");
        closed(&door);
        let calls = calls.join().expect("the door");
        // Its directory and its record's resources; the two the predicate reads, each by its
        // own `read`; and the one the program prints, addressed again.
        assert_eq!(calls.len(), 10, "{calls:#?}");
    }

    #[test]
    fn a_program_reads_a_native_apps_channels() {
        let (task, _reader, door) = task_with(&[chat()]);
        // Chat's record reads itself and takes an item: its channels are read through it.
        let calls = scripted(
            &door,
            Reads {
                app: "so.tilo.chat",
                record: "chat.workspace",
                reading: &[],
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
            .door(),
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
        closed(&door);
        let calls = calls.join().expect("the door");
        // The app listed its directory, its record's resources, and for each channel the
        // program read, the directory once more and the question of what reads it: its record
        // does, taking the channel as its `item`. The listing read no channel's fields.
        assert_eq!(calls.len(), 10, "{calls:#?}");
    }

    #[test]
    fn a_native_read_the_policy_refuses_fails_the_program_with_why() {
        let (task, _reader, door) = task_with(&[tasks()]);
        let calls = scripted(&door, |message| {
            if let Some(listing) = message.get("children") {
                let rows = match listing["resource"].as_str() {
                    Some("") => vec![record_row("tasks.tracker", "Tasks")],
                    _ => vec![row("tasks.tracker/issues/one", "Ship it")],
                };
                return json!({"ok": {"rows": rows}});
            }
            if message.get("reads").is_some() {
                return json!({"ok": {"reads": true}});
            }
            json!({"refused": "these are a person's private tasks"})
        });
        let answer = ran(&task, "for i in find(tasks.Issue) {\n    print(i.title)\n}");
        closed(&door);
        let calls = calls.join().expect("the door");
        assert_eq!(calls.len(), 6, "{calls:#?}");
        assert_eq!(answer["state"], "failed", "{answer:#}");
        let why = answer["detail"].as_str().unwrap_or_default();
        assert!(
            why.contains("so.tilo.tasks refused the read: these are a person's private tasks"),
            "{why}"
        );
    }

    #[test]
    fn a_program_names_a_native_apps_fields_and_reads_no_other() {
        let (task, _reader, _door) = task_with(&[chat()]);
        // A field the app does not publish is an error in the program, as in `tilolang check`.
        let wrong = ran(
            &task,
            "for c in find(chat.Conversation) {\n    print(c.colour)\n}",
        );
        assert_eq!(wrong["state"], "rejected", "{wrong:#}");
    }

    #[test]
    fn a_native_apps_record_is_a_root_a_program_names() {
        let (task, _reader, _door) = task_with(&[chat(), tasks()]);
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
        // The app the program invokes the record of, and how it writes it.
        assert_eq!(
            roots("chat"),
            [("workspace".to_owned(), "chat.workspace".to_owned())],
            "{catalogue:#}"
        );
        assert_eq!(
            roots("tasks"),
            [("tracker".to_owned(), "tasks.tracker".to_owned())],
            "{catalogue:#}"
        );
    }

    #[test]
    fn a_program_changes_native_apps_through_their_actions() {
        let (task, _reader, door) = task_with(&[chat(), tasks()]);
        let calls = answering(door, 2, r#"{"ok": {}}"#);
        let answer = ran(
            &task,
            "chat.rename_channel(chat.workspace, \"general\", \"town\")\ntasks.create(tasks.tracker, \"Ship it\")",
        );
        let calls = calls.join().expect("the door");
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
                    "action": "create",
                    "arguments": {
                        "title": "Ship it",
                        "parent": null,
                        "body": null,
                        "status": null,
                        "assignee": null,
                    },
                }),
            ]
        );
        assert_eq!(answer["changed"], 2, "{answer:#}");
        // Both steps reach the task's history on the records the apps publish, so `undo task`
        // takes them back like any tl app's.
        assert_eq!(
            answer["history"],
            json!([
                {"app": "so.tilo.chat", "resource": "chat.workspace", "published": false},
                {"app": "so.tilo.tasks", "resource": "tasks.tracker", "published": false},
            ])
        );
    }

    #[test]
    fn a_native_action_the_app_refuses_fails_the_program_and_changes_nothing() {
        let (task, _reader, door) = task_with(&[chat()]);
        let calls = answering(
            door,
            1,
            r#"{"refused": "the workspace asks its members before renaming"}"#,
        );
        let answer = ran(
            &task,
            "chat.rename_channel(chat.workspace, \"general\", \"town\")",
        );
        let calls = calls.join().expect("the door");
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(answer["state"], "failed", "{answer:#}");
        let why = answer["detail"].as_str().unwrap_or_default();
        assert!(why.contains("asks its members before renaming"), "{why}");
        assert_eq!(answer["changed"], 0, "{answer:#}");
        assert_eq!(answer["history"], json!([]), "{answer:#}");
    }

    #[test]
    fn a_task_names_only_the_apps_its_session_hands_it() {
        let (task, _reader, _door) = task_with(&[]);
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

    /// The bundle of the package `source` declares, its ids lock `ids`.
    fn bundle(source: &str, ids: &str) -> Bundle {
        let lock = tilolang::IdsLock::parse(ids).expect("the ids lock reads");
        let compiled = tilolang::compile_package(source, &lock, &[]).expect("the package compiles");
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

    #[test]
    fn a_package_that_does_not_load_leaves_only_its_app_unreachable() {
        // One installed app among the rest whose package does not load here, as a module built
        // for another runtime, or a payload damaged on its way to the device, hands one over.
        let mut broken = counter();
        let gone = broken.meta.app.clone();
        broken.wasm = b"not a tilolang module".to_vec();
        let (task, _reader, door) = task_over(&[discussion(), broken], &[]);
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
        let comment = comment["id"].as_str().unwrap_or_default().to_owned();
        // Its actions resolve, and invoking one runs: the call goes to the app through the door,
        // which answers the comment it made.
        let called = scripted(&door, |message| {
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
        closed(&door);
        let calls = called.join().expect("the door");
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

    /// `tilolang check`'s packages: what the apps' directories publish.
    fn checked() -> Vec<PackageMeta> {
        let chat = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/chat");
        tilolang_tilo::catalogue::published_packages(&[chat]).expect("the surfaces read")
    }

    #[test]
    fn check_and_a_task_compile_against_the_same_catalogue() {
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
        let (task, _reader, _door) = task_with(&[chat()]);
        let answer = ran(&task, program);
        assert_eq!(answer["state"], "rejected", "{answer:#}");
        assert_eq!(answer["detail"], serde_json::json!(expected), "{answer:#}");
    }
}

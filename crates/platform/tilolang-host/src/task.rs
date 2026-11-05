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
//! [`TaskSession::catalogue`]);
//! while the program runs, the host asks `{"record": {app, record}}` for a record it reads
//! (answered `{"ok": {"document": base64}}`, `{"absent": true}` or `{"err": why}`: the record's
//! committed content, read-only, which the host reads itself with the app's package,
//! [`RecordReads`]) and `{"invoke": {app, resource, action, arguments}}` (answered `{"ok":
//! output}`, `{"failed": why}`, `{"refused": why}` or `{"asks": why}`), and it ends with
//! `{"answer": …}` ([`tilolang_tilo::task::TaskSession::run`]).

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
use tilolang_tilo::runtime::meta::PackageMeta;
use tilolang_tilo::task::{Answered, TaskDoor, TaskSession};
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
/// door hands over ([`RecordReads`]), and invocations, which go to the door.
struct Door {
    line: Rc<Line>,
    reads: RecordReads,
    /// The author of the program running now, as the actions it plans for a preview see their
    /// caller.
    caller: RefCell<tilolang_tilo::publish::Caller>,
}

impl TaskDoor for Door {
    fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        self.reads.read(app, item)
    }

    fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        self.reads.query(app, request)
    }

    fn begin_program(&self) {
        // Each program reads the records anew, as the door decides them now.
        self.reads.forget();
    }

    fn invoke(&self, call: &Json) -> Answered {
        // A change of an app's record goes to the record's origin with the record it changes,
        // and what the task's read of it would write, which the origin compares its run with.
        let mut call = call.clone();
        if let Some(app) = call.get("app").and_then(Json::as_str).map(str::to_owned)
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
        if let Some(app) = call.get("app").and_then(Json::as_str) {
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
    ///
    /// # Errors
    /// The apps cannot be read or loaded, or the connection is unusable.
    pub fn open(request: &TaskRequest, fds: TaskFds) -> Result<Self, String> {
        Self::open_with(request, fds, None)
    }

    /// [`Self::open`] for a task whose session another connection joins from the start,
    /// `person`: the person's lines in its run's conversation run in the same session as the
    /// agent's own calls, each on a connection of its own, so they share its `let`s.
    ///
    /// # Errors
    /// The apps cannot be read or loaded, or a connection is unusable.
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
        let door = Rc::new(Door {
            line: Rc::clone(&line),
            reads,
            // Each program names its author; this is no one's until the first does.
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: String::new(),
                agent: true,
                on_behalf_of: request.person.clone(),
            }),
        });
        let session = TaskSession::new(
            request.person.as_deref(),
            &(Rc::clone(&door) as Rc<dyn TaskDoor>),
            &apps.bundles,
            &apps.native,
        )?;
        tracing::info!(
            task = request.task,
            apps = apps.bundles.len(),
            native = apps.native.len(),
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
    ///
    /// # Errors
    /// The stream cannot be taken apart.
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
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: String::new(),
                agent: true,
                on_behalf_of: None,
            }),
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
    };
    for app in decode_apps(bytes).map_err(|error| error.to_string())? {
        match app {
            App::Module(module) => apps.bundles.push(
                Bundle::from_parts(module.wasm, module.ids, &module.imports)
                    .map_err(|error| error.to_string())?,
            ),
            App::Surface {
                app,
                title,
                surface,
            } => apps.native.push(
                NativeSurface {
                    app,
                    title,
                    surface,
                }
                .package()?,
            ),
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
        let (ours, door) = UnixStream::pair().expect("a socket pair");
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(ours.try_clone().expect("a clone"))),
            writer: RefCell::new(ours.try_clone().expect("a clone")),
        });
        let reached = Rc::new(Door {
            line: Rc::clone(&line),
            reads: RecordReads::new(
                &[],
                std::collections::BTreeMap::new(),
                None,
                Rc::clone(&line) as Rc<dyn RecordSource>,
            ),
            caller: RefCell::new(tilolang_tilo::publish::Caller {
                actor: "agent:task".to_owned(),
                agent: true,
                on_behalf_of: None,
            }),
        });
        let session = TaskSession::new(
            None,
            &(Rc::clone(&reached) as Rc<dyn TaskDoor>),
            &[],
            native,
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

    #[test]
    fn a_program_names_a_native_apps_types_and_reads_its_fields() {
        let (task, _reader, _door) = task_with(&[chat()]);
        // Field reads against the app's published surface: a type-check is all a program does
        // with them while the app's record stays in the app.
        let answer = ran(
            &task,
            "for c in find(chat.Channel) {\n    print(c.name, c.archived)\n}",
        );
        assert_eq!(answer["state"], "failed", "{answer:#}");
        let why = answer["detail"].as_str().unwrap_or_default();
        assert!(why.contains("so.tilo.chat"), "{why}");
        assert!(why.contains("is not yet readable from a program"), "{why}");
        // A field the app does not publish is an error in the program, as in `tilolang check`.
        let wrong = ran(
            &task,
            "for c in find(chat.Channel) {\n    print(c.colour)\n}",
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
        let answer = ran(&task, "for c in find(chat.Channel) {\n    print(c.name)\n}");
        assert_eq!(answer["state"], "rejected", "{answer:#}");
        let why = format!("{:#}", answer["detail"]);
        assert!(
            why.contains("chat") || why.contains("chat.Channel"),
            "{why}"
        );
    }

    /// `tilolang check`'s packages: what the apps' directories publish.
    fn checked() -> Vec<PackageMeta> {
        let chat = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/chat");
        tilolang_tilo::catalogue::published_packages(&[chat]).expect("the surfaces read")
    }

    #[test]
    fn check_and_a_task_compile_against_the_same_catalogue() {
        let checked = checked();
        let program = "for c in find(chat.Channel) {\n    print(c.name)\n    print(c.colour)\n}";
        let Err(diagnostics) = tilolang::compile_program_reading(program, &checked, &[], &[])
        else {
            panic!("`colour` is no field of Chat's Channel");
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

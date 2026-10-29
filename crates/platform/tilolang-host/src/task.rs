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
//! The connection carries one JSON object per line. The door sends `{"run": {source, confirm}}`;
//! while the program runs, the host asks `{"read": {app, item}}` (answered `{"ok": fields}` or
//! `{"err": why}`), `{"query": {app, request}}` (a `grep`, `find`, `read` or `describe` of an
//! app, answered the same way) and `{"invoke": {app, resource, action, arguments}}` (answered `{"ok":
//! output}`, `{"failed": why}`, `{"refused": why}` or `{"asks": why}`), and it ends with
//! `{"answer": …}` ([`tilolang_tilo::task::TaskSession::run`]).

use std::cell::RefCell;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;

use runtime_portal::tilolang_host::{TaskFds, TaskRequest, decode_modules};
use serde_json::{Value as Json, json};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::task::{Answered, TaskDoor, TaskSession};

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

    /// The next message, or `None` once the door closed the connection.
    fn next(&self) -> Option<Json> {
        let mut line = String::new();
        match self.reader.borrow_mut().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => serde_json::from_str(&line).ok().or(Some(Json::Null)),
        }
    }

    fn ask(&self, request: &Json) -> Result<Json, String> {
        self.send(request)?;
        self.next()
            .ok_or_else(|| "the door closed the task".to_owned())
    }
}

impl TaskDoor for Line {
    fn read(&self, app: &str, item: &str) -> Result<Json, String> {
        let reply = self.ask(&json!({"read": {"app": app, "item": item}}))?;
        match reply.get("ok") {
            Some(fields) => Ok(fields.clone()),
            None => Err(reply
                .get("err")
                .and_then(Json::as_str)
                .unwrap_or("the door did not read it")
                .to_owned()),
        }
    }

    fn query(&self, app: &str, request: &Json) -> Result<Json, String> {
        let reply = self.ask(&json!({"query": {"app": app, "request": request}}))?;
        match reply.get("ok") {
            Some(answer) => Ok(answer.clone()),
            None => Err(reply
                .get("err")
                .and_then(Json::as_str)
                .unwrap_or("the app did not answer")
                .to_owned()),
        }
    }

    fn invoke(&self, call: &Json) -> Answered {
        let reply = match self.ask(&json!({"invoke": call})) {
            Ok(reply) => reply,
            Err(why) => return Answered::Failed(why),
        };
        let why = |key: &str| {
            reply
                .get(key)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        if let Some(output) = reply.get("ok") {
            Answered::Done(output.clone())
        } else if reply.get("refused").is_some() {
            Answered::Refused(why("refused"))
        } else if reply.get("asks").is_some() {
            Answered::Asks(why("asks"))
        } else {
            Answered::Failed(why("failed"))
        }
    }
}

/// A task this host runs the programs of.
pub struct Task {
    line: Rc<Line>,
    /// The program connection, watched for what the door sends.
    watched: OwnedFd,
    session: TaskSession,
}

impl Task {
    /// The task `request`, its programs coming in on `fds.program` and compiled against the
    /// modules in `fds.modules`.
    ///
    /// # Errors
    /// The modules cannot be read or loaded, or the connection is unusable.
    pub fn open(request: &TaskRequest, fds: TaskFds) -> Result<Self, String> {
        let bundles = bundles(&fds.modules, request.modules_len)?;
        let stream = UnixStream::from(fds.program);
        let writer = stream.try_clone().map_err(|error| error.to_string())?;
        let watched = OwnedFd::from(stream.try_clone().map_err(|error| error.to_string())?);
        let line = Rc::new(Line {
            reader: RefCell::new(BufReader::new(stream)),
            writer: RefCell::new(writer),
        });
        let session = TaskSession::new(
            &request.task,
            request.person.as_deref(),
            &(Rc::clone(&line) as Rc<dyn TaskDoor>),
            &bundles,
        )?;
        tracing::info!(
            task = request.task,
            apps = bundles.len(),
            "task host started"
        );
        Ok(Self {
            line,
            watched,
            session,
        })
    }

    /// The program connection, to wait on.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.watched.as_fd()
    }

    /// Answer what the door sent: run a program and say how it went. `false` once the door
    /// closed the task.
    pub fn serve(&self) -> bool {
        let Some(message) = self.line.next() else {
            return false;
        };
        let Some(run) = message.get("run") else {
            tracing::warn!("the door sent the task something other than a program");
            return true;
        };
        let source = run["source"].as_str().unwrap_or_default();
        let confirm = run["confirm"].as_bool().unwrap_or(false);
        let answer = self.session.run(source, confirm);
        if let Err(error) = self.line.send(&json!({ "answer": answer })) {
            tracing::warn!(%error, "a program's answer did not reach the door");
            return false;
        }
        true
    }
}

/// The bundles the sealed memfd `fd` holds, `len` bytes of them.
fn bundles(fd: &OwnedFd, len: u64) -> Result<Vec<Bundle>, String> {
    let len = usize::try_from(len).map_err(|error| error.to_string())?;
    let mapping = tilo_render_protocol::shm::Mapping::map_sealed(fd, true, len, len)
        .map_err(|error| error.to_string())?;
    let bytes = mapping
        .get(0, len)
        .ok_or("the modules are shorter than they say")?;
    decode_modules(bytes)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|module| {
            Bundle::from_parts(module.wasm, module.ids, &module.imports)
                .map_err(|error| error.to_string())
        })
        .collect()
}

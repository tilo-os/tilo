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
use runtime_portal::tilolang_host::{TaskFds, TaskRequest, decode_modules};
use serde_json::{Value as Json, json};
use tilolang_tilo::bundle::Bundle;
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
    /// The task as the actions it plans for a preview see their caller.
    caller: tilolang_tilo::publish::Caller,
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
            let preview = self.reads.plan(&call, &self.caller).unwrap_or(Json::Null);
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
            let wrote = reply
                .get("change")
                .and_then(Json::as_str)
                .map(|change| base64::engine::general_purpose::STANDARD.decode(change));
            let taken = match wrote {
                Some(Ok(change)) => self.reads.take_in(app, &change),
                Some(Err(error)) => Err(format!("the origin's change is not base64: {error}")),
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
        let reads = RecordReads::new(
            &bundles,
            request.records_under.clone(),
            request.person.as_deref(),
            Rc::clone(&line) as Rc<dyn RecordSource>,
        );
        let door = Rc::new(Door {
            line: Rc::clone(&line),
            reads,
            caller: tilolang_tilo::publish::Caller {
                actor: format!("agent:{}", request.task),
                agent: true,
                on_behalf_of: request.person.clone(),
            },
        });
        let session = TaskSession::new(
            &request.task,
            request.person.as_deref(),
            &(Rc::clone(&door) as Rc<dyn TaskDoor>),
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
            Ok((source, confirm)) => self.session.run(&source, confirm),
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

/// The program `message` asks the task to run, and whether its changes are confirmed.
fn program(message: &Json) -> Result<(String, bool), String> {
    let run = message.get("run").ok_or_else(|| {
        format!("the door sent the task something other than a program: {message}")
    })?;
    let source = run
        .get("source")
        .and_then(Json::as_str)
        .ok_or("the door sent a program without its source")?;
    // An absent `confirm` is a run not confirmed yet: its changes are previewed first.
    let confirm = run.get("confirm").and_then(Json::as_bool).unwrap_or(false);
    Ok((source.to_owned(), confirm))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A task over no packages, its program connection's far end returned as the door's.
    fn task() -> (Task, BufReader<UnixStream>, UnixStream) {
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
            caller: tilolang_tilo::publish::Caller {
                actor: "agent:task".to_owned(),
                agent: true,
                on_behalf_of: None,
            },
        });
        let session = TaskSession::new(
            "task",
            None,
            &(Rc::clone(&reached) as Rc<dyn TaskDoor>),
            &[],
        )
        .expect("a session");
        let task = Task {
            line,
            watched: OwnedFd::from(ours),
            session,
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
}

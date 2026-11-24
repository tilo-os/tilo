//! The `tilo` command, run as a program of its own, which the REPL reaches the person's workspace
//! through: `tilo mcp --as-me` answers MCP's JSON-RPC, a line each way on its standard input and
//! output, as the person (`crates/core/tilo-cli/src/mcp/person.rs`). How `tilo` signs in (this
//! computer's login, a desktop, a key file) is `tilo`'s; the REPL only speaks the lines.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};

/// What a person reads when no `tilo` command is installed beside `tilolang` or on `PATH`.
pub const TILO_MISSING: &str = "The REPL works in your workspace through the `tilo` command, \
     which isn't installed here: install it with `curl -fsSL https://tilo.sh | sh`.";

/// The most of `tilo`'s own error output kept, to say why it ended.
const SAID_KEPT: usize = 4096;

/// Why `tilo` serves no workspace.
#[derive(Debug)]
pub enum Refused {
    /// Nothing on this computer signs the person in.
    NotSignedIn,
    /// Anything else, in words a person reads.
    Said(String),
}

/// A running `tilo mcp --as-me`, and the workspace each call names.
pub struct Tilo {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    /// What `tilo` wrote to its standard error, its last [`SAID_KEPT`] bytes.
    said: Arc<Mutex<String>>,
    /// What reads it, done once `tilo` has ended.
    reading_said: Option<std::thread::JoinHandle<()>>,
    next_id: u64,
    /// The workspace calls name, when `tilo` acts in none of its own.
    pub workspace: Option<String>,
}

/// The `tilo` command: the one on `PATH`, else the one beside this program (a release installs
/// both commands together).
#[must_use]
pub fn find() -> Option<PathBuf> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join("tilo"))
            .find(|candidate| runnable(candidate))
    });
    on_path.or_else(|| {
        let beside = std::env::current_exe().ok()?.parent()?.join("tilo");
        runnable(&beside).then_some(beside)
    })
}

fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

impl Tilo {
    /// Start `program` with `args` (its own flags, then `mcp --as-me`), `envs` added to its
    /// environment, and open its session: the server, and the workspace it acts in when it names
    /// one.
    ///
    /// # Errors
    /// `tilo` did not start, or serves nothing.
    pub fn start(
        program: &Path,
        args: &[std::ffi::OsString],
        envs: &[(&str, &Path)],
    ) -> Result<(Self, Option<(String, String)>), Refused> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in envs {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|error| {
            Refused::Said(format!("`{}` did not start: {error}", program.display()))
        })?;
        let (Some(input), Some(output), Some(mut errors)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Refused::Said(format!(
                "`{}` started without its standard streams",
                program.display()
            )));
        };
        let said = Arc::new(Mutex::new(String::new()));
        let kept = Arc::clone(&said);
        let reading_said = std::thread::spawn(move || {
            let mut chunk = [0_u8; 1024];
            while let Ok(read @ 1..) = errors.read(&mut chunk) {
                let mut said = kept
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                said.push_str(&String::from_utf8_lossy(
                    chunk.get(..read).unwrap_or_default(),
                ));
                if said.len() > SAID_KEPT {
                    let cut = said.len().saturating_sub(SAID_KEPT);
                    let cut = (cut..said.len())
                        .find(|at| said.is_char_boundary(*at))
                        .unwrap_or(0);
                    said.drain(..cut);
                }
            }
        });
        let mut tilo = Self {
            child,
            input: Some(input),
            output: BufReader::new(output),
            said,
            reading_said: Some(reading_said),
            next_id: 0,
            workspace: None,
        };
        let opened = tilo.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "tilolang repl", "version": env!("CARGO_PKG_VERSION")},
            }),
        );
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) if error.reason.as_deref() == Some("not_signed_in") => {
                return Err(Refused::NotSignedIn);
            }
            Err(error) => return Err(Refused::Said(error.message)),
        };
        tilo.notify("notifications/initialized")
            .map_err(Refused::Said)?;
        let workspace = opened
            .pointer("/_meta/so.tilo.mcp/workspace")
            .and_then(|workspace| {
                let id = workspace.get("id")?.as_str()?.to_owned();
                let name = workspace
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or(&id)
                    .to_owned();
                Some((id, name))
            });
        Ok((tilo, workspace))
    }

    /// Call `tool` with `arguments`, in [`Self::workspace`] when it names one: the typed answer,
    /// or the refusal in the words a person reads. A program that failed is an answer (it says
    /// how); a refused call is not.
    ///
    /// # Errors
    /// The call was refused, or `tilo` ended.
    pub fn call(&mut self, tool: &str, mut arguments: Json) -> Result<Json, String> {
        if let (Some(fields), Some(workspace)) = (arguments.as_object_mut(), &self.workspace) {
            fields.insert("workspace".into(), json!(workspace));
        }
        let result = self
            .request(
                "tools/call",
                &json!({
                    "name": tool,
                    "arguments": arguments,
                    "_meta": {"so.tilo.mcp": {"structured": true}},
                }),
            )
            .map_err(|error| error.message)?;
        let failed = result.get("isError").and_then(Json::as_bool) == Some(true);
        let text = || {
            result
                .pointer("/content/0/text")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        match result.get("structuredContent") {
            Some(typed) if !failed || tool == "tilo" => Ok(typed.clone()),
            Some(typed) => Err(typed
                .pointer("/error/message")
                .and_then(Json::as_str)
                .map_or_else(text, str::to_owned)),
            None if failed => Err(text()),
            None => Ok(json!({"text": text()})),
        }
    }

    /// Send `method` with `params`: its result, or its error.
    fn request(&mut self, method: &str, params: &Json) -> Result<Json, RpcError> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .map_err(RpcError::said)?;
        loop {
            let mut line = String::new();
            let read = self.output.read_line(&mut line);
            if !matches!(read, Ok(1..)) {
                return Err(RpcError::said(self.ended()));
            }
            let Ok(answer) = serde_json::from_str::<Json>(&line) else {
                continue;
            };
            // What the server tells unasked (a notification) is no answer.
            if answer.get("id").and_then(Json::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = answer.get("error") {
                return Err(RpcError {
                    message: error
                        .get("message")
                        .and_then(Json::as_str)
                        .unwrap_or("`tilo` refused it")
                        .to_owned(),
                    reason: error
                        .pointer("/data/reason")
                        .and_then(Json::as_str)
                        .map(str::to_owned),
                });
            }
            return Ok(answer.get("result").cloned().unwrap_or_default());
        }
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    fn send(&mut self, message: &Json) -> Result<(), String> {
        let sent = self
            .input
            .as_mut()
            .map(|input| writeln!(input, "{message}").and_then(|()| input.flush()));
        if matches!(sent, Some(Ok(()))) {
            Ok(())
        } else {
            Err(self.ended())
        }
    }

    /// Why `tilo` stopped answering: what it said last, or that it ended.
    fn ended(&mut self) -> String {
        let status = self.child.wait().ok();
        if let Some(reading) = self.reading_said.take() {
            let _ = reading.join();
        }
        let said = self
            .said
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .trim()
            .to_owned();
        match (said.is_empty(), status) {
            (false, _) => said,
            (true, Some(status)) => format!("`tilo` ended ({status})"),
            (true, None) => "`tilo` ended".to_owned(),
        }
    }
}

impl Drop for Tilo {
    fn drop(&mut self) {
        // Its standard input ending is its end: it answered every call already.
        drop(self.input.take());
        let _ = self.child.wait();
    }
}

struct RpcError {
    message: String,
    reason: Option<String>,
}

impl RpcError {
    const fn said(message: String) -> Self {
        Self {
            message,
            reason: None,
        }
    }
}

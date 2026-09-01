use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};

use thiserror::Error;

/// A command invocation represented without a shell.
///
/// `stdin` may contain credentials and is intentionally omitted from `Debug`.
#[derive(Clone, Eq, PartialEq)]
pub struct CommandRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub stdin: Vec<u8>,
}

impl CommandRequest {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn stdin_secret(mut self, secret: &[u8]) -> Self {
        self.stdin.extend_from_slice(secret);
        self
    }
}

impl fmt::Debug for CommandRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandRequest")
            .field("program", &self.program)
            .field("args", &self.args)
            .field(
                "stdin",
                &format_args!("<redacted: {} bytes>", self.stdin.len()),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    #[must_use]
    pub fn successful(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            success: true,
            code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("could not spawn {program:?}: {source}")]
    Spawn {
        program: OsString,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write private input to {program:?}: {source}")]
    Stdin {
        program: OsString,
        #[source]
        source: std::io::Error,
    },
    #[error("could not wait for {program:?}: {source}")]
    Wait {
        program: OsString,
        #[source]
        source: std::io::Error,
    },
}

pub trait CommandRunner: Send + Sync {
    /// Executes exactly `program` and `args`; implementations must never pass
    /// this request through a shell.
    fn run(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StdCommandRunner;

impl CommandRunner for StdCommandRunner {
    fn run(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
        let mut command = Command::new(&request.program);
        command
            .args(request.args.iter().map(OsStr::new))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if request.stdin.is_empty() {
            command.stdin(Stdio::null());
        } else {
            command.stdin(Stdio::piped());
        }

        let mut child = command.spawn().map_err(|source| CommandError::Spawn {
            program: request.program.clone(),
            source,
        })?;
        if !request.stdin.is_empty() {
            let mut stdin = child.stdin.take().ok_or_else(|| CommandError::Stdin {
                program: request.program.clone(),
                source: std::io::Error::other("child did not expose its piped stdin"),
            })?;
            stdin
                .write_all(&request.stdin)
                .map_err(|source| CommandError::Stdin {
                    program: request.program.clone(),
                    source,
                })?;
        }
        let output = child
            .wait_with_output()
            .map_err(|source| CommandError::Wait {
                program: request.program.clone(),
                source,
            })?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_contains_stdin_secret() {
        let request = CommandRequest::new("program").stdin_secret(b"do-not-log-me");
        let debug = format!("{request:?}");
        assert!(!debug.contains("do-not-log-me"));
        assert!(debug.contains("redacted"));
    }
}

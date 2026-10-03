use std::ffi::{OsStr, OsString};
use std::fmt;
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

/// A command invocation represented without a shell.
///
/// `stdin` may contain credentials and is intentionally omitted from `Debug`.
#[derive(Clone, Eq, PartialEq)]
pub struct CommandRequest {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub stdin: Vec<u8>,
    /// Host-selected environment overrides. Values are omitted from `Debug`
    /// because future adapters may carry private endpoint information here.
    pub environment: Vec<(OsString, OsString)>,
    /// Maximum bytes captured independently from stdout and stderr.
    pub output_limit: usize,
    /// Hard lifetime for this invocation, including inherited pipe holders.
    pub timeout: Duration,
}

impl CommandRequest {
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: Vec::new(),
            environment: Vec::new(),
            output_limit: 8 * 1024 * 1024,
            timeout: Duration::from_secs(86_400),
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

    #[must_use]
    pub fn env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
            .field("output_limit", &self.output_limit)
            .field("timeout", &self.timeout)
            .field(
                "environment",
                &self
                    .environment
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
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
    #[error("bounded execution of {program:?} failed: {source}")]
    Execution {
        program: OsString,
        #[source]
        source: bounded_command::Error,
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
        command.args(request.args.iter().map(OsStr::new)).envs(
            request
                .environment
                .iter()
                .map(|(name, value)| (name, value)),
        );
        let input = (!request.stdin.is_empty()).then_some(request.stdin.as_slice());
        let output =
            bounded_command::run_prepared(command, input, request.output_limit, request.timeout)
                .map_err(|source| CommandError::Execution {
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
    fn the_command_runner_hides_stdin_secrets_and_bounds_output_and_time() {
        // Phase `debug_never_contains_stdin_secret`.
        {
            let request = CommandRequest::new("program")
                .stdin_secret(b"do-not-log-me")
                .env("SECRET_ENV", "do-not-log-env");
            let debug = format!("{request:?}");
            assert!(!debug.contains("do-not-log-me"));
            assert!(!debug.contains("do-not-log-env"));
            assert!(debug.contains("SECRET_ENV"));
            assert!(debug.contains("redacted"));
        }
        // Phase `runner_enforces_output_and_time_bounds`.
        {
            let runner = StdCommandRunner;
            let mut oversized = CommandRequest::new("sh").args(["-c", "head -c 4096 /dev/zero"]);
            oversized.output_limit = 32;
            assert!(matches!(
                runner.run(&oversized),
                Err(CommandError::Execution {
                    source: bounded_command::Error::OutputTooLarge { .. },
                    ..
                })
            ));

            let timeout = CommandRequest::new("sh")
                .args(["-c", "sleep 5"])
                .timeout(Duration::from_millis(25));
            let started = std::time::Instant::now();
            assert!(matches!(
                runner.run(&timeout),
                Err(CommandError::Execution {
                    source: bounded_command::Error::Timeout(_),
                    ..
                })
            ));
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
}

//! Programs a workspace's Terminal starts on the server's own machine, as the server's own user.
//!
//! A server that serves strangers starts their programs in each workspace's VM
//! ([`crate::hosted_vm`]). A server that is its operator's own computer, such as a development
//! VM that one person and their agents work in, can start them where it is instead: `cargo`,
//! `git` and every other program on the machine's `PATH`, with the machine's network and every
//! file its user may touch. The operator turns this on with [`MACHINE_ENV`]; nothing confines
//! the programs, so it is for a machine whose every workspace member may already do anything
//! there.
//!
//! A program's output reaches the shell as it prints it, its input is the shell's (a pipe, or
//! nothing), and a stop ends the program and every process it started: it is started in a
//! process group of its own, which is sent `SIGTERM`, and `SIGKILL` once [`STOP_GRACE`] passed.
//! A process the program left in the background with the output still open does not hold the
//! command: once the program ended, its output is read for at most [`DRAIN_GRACE`] more.

use std::io::{Read, Write};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::{Child, Command, Stdio as Pipes};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ush_shell::{Input, Output, Program, Stdio};
use ush_tilo::Programs;

/// Set to `1`, a server without a hosted-VM service starts its workspaces' programs on its
/// own machine ([`MachinePrograms`]).
pub const MACHINE_ENV: &str = "TILO_TERMINAL_ON_MACHINE";

/// How long a stopped program is given to end before it is killed.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// How long the output of a program that ended is still read.
pub const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// How often a program is looked at for its end and for a stop.
const POLL: Duration = Duration::from_millis(25);

/// Whether the environment asks this server to start its workspaces' programs on its machine.
#[must_use]
pub fn on_machine_from_env() -> bool {
    std::env::var(MACHINE_ENV).is_ok_and(|value| value == "1")
}

/// Starts each program as a child process of the server.
#[derive(Debug, Clone, Copy, Default)]
pub struct MachinePrograms;

impl Programs for MachinePrograms {
    fn run(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
        stop: Option<&dyn Fn() -> bool>,
    ) -> Program {
        let Some(name) = argv.first() else {
            return Program::Exited(0);
        };
        let mut command = Command::new(name);
        command
            .args(argv.iter().skip(1))
            .env_clear()
            .envs(env.iter().map(|(key, value)| (key, value)))
            .current_dir(cwd)
            .stdin(Pipes::piped())
            .stdout(Pipes::piped())
            .stderr(Pipes::piped())
            .process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Program::NotFound,
            Err(error) => {
                let _ = io.stderr.write_str(&format!("ush: {name}: {error}\n"));
                return Program::Exited(126);
            }
        };
        feed(child.stdin.take(), io.stdin.clone());
        let reading = Arc::new(AtomicUsize::new(0));
        if let Some(stdout) = child.stdout.take() {
            copy(stdout, io.stdout.clone(), &reading);
        }
        if let Some(stderr) = child.stderr.take() {
            copy(stderr, io.stderr.clone(), &reading);
        }
        let status = wait(&mut child, stop);
        let ended = Instant::now();
        while reading.load(Ordering::SeqCst) > 0 && ended.elapsed() < DRAIN_GRACE {
            std::thread::sleep(POLL);
        }
        Program::Exited(status)
    }

    fn names(&self, path: &str) -> Vec<String> {
        let mut names: Vec<String> = std::env::split_paths(path)
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flat_map(|entries| entries.filter_map(Result::ok))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Hand the shell's input to the program until either ends.
fn feed(stdin: Option<std::process::ChildStdin>, input: Input) {
    let Some(mut stdin) = stdin else { return };
    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match input.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let read = buf.get(..read).unwrap_or_default();
                    if stdin.write_all(read).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Pass what the program prints to `output` as it prints it; `reading` counts the streams
/// still open.
fn copy(mut from: impl Read + Send + 'static, output: Output, reading: &Arc<AtomicUsize>) {
    reading.fetch_add(1, Ordering::SeqCst);
    let reading = Arc::clone(reading);
    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let read = buf.get(..read).unwrap_or_default();
                    if output.write_all(read).is_err() {
                        break;
                    }
                }
            }
        }
        reading.fetch_sub(1, Ordering::SeqCst);
    });
}

/// Wait for the program to end, ending its process group when `stop` says so; its shell status.
fn wait(child: &mut Child, stop: Option<&dyn Fn() -> bool>) -> i32 {
    let group = child.id().cast_signed();
    let mut stopped_at: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status
                    .code()
                    .or_else(|| status.signal().map(|signal| signal.saturating_add(128)))
                    .unwrap_or(1);
            }
            Ok(None) => {}
            Err(_) => return 1,
        }
        match stopped_at {
            None if stop.is_some_and(|stop| stop()) => {
                signal_group(group, libc::SIGTERM);
                stopped_at = Some(Instant::now());
            }
            Some(at) if at.elapsed() >= STOP_GRACE => {
                signal_group(group, libc::SIGKILL);
            }
            _ => {}
        }
        std::thread::sleep(POLL);
    }
}

fn signal_group(group: i32, signal: i32) {
    // SAFETY: kill only sends a signal; a negative pid names the program's own
    // process group, which it was started in.
    unsafe {
        libc::kill(group.saturating_neg(), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(argv: &[&str], input: &str, stop: Option<&dyn Fn() -> bool>) -> (Program, String) {
        let (stdout, captured) = Output::buffer();
        let io = Stdio {
            stdin: Input::bytes(input.as_bytes().to_vec()),
            stdout: stdout.clone(),
            stderr: stdout,
        };
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
        let ended = MachinePrograms.run(&argv, &env, Path::new("/"), &io, stop);
        (
            ended,
            String::from_utf8_lossy(&captured.take()).into_owned(),
        )
    }

    #[test]
    fn a_program_starts_here_with_the_shells_input_and_status() {
        let (ended, out) = start(&["sh", "-c", "tr a-z A-Z; exit 3"], "hello\n", None);
        assert_eq!(ended, Program::Exited(3));
        assert_eq!(out, "HELLO\n");
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let (ended, _) = start(&["no-such-program-here"], "", None);
        assert_eq!(ended, Program::NotFound);
    }

    #[test]
    fn a_stop_ends_the_program_and_what_it_started() {
        let began = Instant::now();
        let stop = || began.elapsed() > Duration::from_millis(200);
        let (ended, _) = start(&["sh", "-c", "sleep 30 & sleep 30"], "", Some(&stop));
        assert_eq!(ended, Program::Exited(libc::SIGTERM.saturating_add(128)));
        assert!(began.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_background_process_holding_the_output_does_not_hold_the_command() {
        let began = Instant::now();
        let (ended, out) = start(&["sh", "-c", "echo started; sleep 30 &"], "", None);
        assert_eq!(ended, Program::Exited(0));
        assert_eq!(out, "started\n");
        assert!(began.elapsed() < Duration::from_secs(5));
    }
}

//! Serving a run: the agent program's mode that starts a program in the
//! machine and has its host answer what it asks
//! ([`legacy_agent_protocol::served`]).
//!
//! It runs as the guest's ordinary user, launched like any program, so the
//! guest counts it as running for as long as the program lives and the
//! machine is not put to sleep under it. Its standard output carries frames
//! to the host and its standard input the host's frames back; nothing else
//! is written there. It opens the endpoints it was asked for on the
//! machine's loopback, answers only the processes it started
//! ([`peer`]), and writes each endpoint's address into the program's
//! arguments and variables in its placeholder's place.
//!
//! The run ends with the program, and the program with its host: when the
//! host asks ([`ToRun::Stop`]) the run's processes are interrupted, then
//! asked to end, then ended, each step given its time; when the host goes
//! away (its frames end, or the launcher is signalled) they are asked to end
//! at once. Nothing of a run outlives it: what the program left running is
//! ended when it ends.

mod endpoint;
mod http;
pub mod peer;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use legacy_agent_protocol::served::{
    ENDPOINT_FLAG, FromRun, MAX_ENDPOINTS, STOP_GRACE, STOP_GRACE_FLAG, ToRun, VERSION,
    endpoint_name, placeholder,
};
use legacy_agent_protocol::{ExitStatus, recv, send};
use tilo_lock::MutexRecover as _;

use endpoint::Endpoint;
use peer::Processes;

/// How long the program's last output is waited for once it ended.
const DRAIN: Duration = Duration::from_secs(2);
/// How often the launcher looks whether it was asked to stop, or the
/// program ended.
const TICK: Duration = Duration::from_millis(25);
/// The longest a program that was asked to end is given before it is ended.
const END_GRACE: Duration = Duration::from_secs(5);
/// The most of the program's output one frame carries.
const OUTPUT_CHUNK: usize = 32 * 1024;
/// The launcher's exit status when it could not do what it was asked.
const UNUSABLE: u8 = 70;
/// The launcher's exit status when the program could not be started.
const NOT_STARTED: u8 = 127;

/// Set when the launcher itself is signalled to end: its host is going.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn signalled(_signal: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// What an endpoint's asker is waiting for.
enum Answered {
    Head {
        status: u16,
        headers: Vec<(String, String)>,
    },
    Body {
        bytes: Vec<u8>,
        last: bool,
    },
}

/// What the launcher's threads share.
struct Shared {
    /// The frames to the host, one writer at a time.
    out: Mutex<Box<dyn Write + Send>>,
    /// The requests waiting for their answers.
    waiting: Mutex<HashMap<u64, Sender<Answered>>>,
    next: AtomicU64,
    /// How many requests are open now.
    open: AtomicUsize,
    /// The launcher's own process: the run is what descends from it.
    launcher: u32,
    processes: Processes,
    /// The host stopped reading: no frame reaches it any more.
    host_gone: AtomicBool,
    /// The host's own frames ended: it is going, or gone.
    host_left: AtomicBool,
    /// The host asked the program to end.
    stop_asked: AtomicBool,
}

impl Shared {
    /// Send `frame` to the host; `false` once the host is gone.
    fn tell(&self, frame: &FromRun) -> bool {
        if self.host_gone.load(Ordering::SeqCst) {
            return false;
        }
        let mut out = self.out.lock_recover();
        let sent = send(&mut **out, frame).is_ok() && out.flush().is_ok();
        drop(out);
        if !sent {
            self.host_gone.store(true, Ordering::SeqCst);
        }
        sent
    }

    /// Whether the run has no host any more.
    fn alone(&self) -> bool {
        self.host_gone.load(Ordering::SeqCst)
            || self.host_left.load(Ordering::SeqCst)
            || SIGNALLED.load(Ordering::SeqCst)
    }

    /// A number for a new request, and where its answer will arrive.
    fn asking(&self) -> (u64, Receiver<Answered>) {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (answering, answers) = mpsc::channel();
        self.waiting.lock_recover().insert(id, answering);
        (id, answers)
    }

    fn forget(&self, id: u64) {
        self.waiting.lock_recover().remove(&id);
    }

    /// Hand part of the host's answer to the asker of request `id`, if it
    /// still waits.
    fn answer(&self, id: u64, answered: Answered) {
        if let Some(asker) = self.waiting.lock_recover().get(&id) {
            let _ = asker.send(answered);
        }
    }
}

/// What the launcher was asked to serve.
#[derive(Debug, PartialEq, Eq)]
struct Config {
    endpoints: Vec<String>,
    stop_grace: Duration,
    argv: Vec<String>,
}

impl Config {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut config = Self {
            endpoints: Vec::new(),
            stop_grace: STOP_GRACE,
            argv: Vec::new(),
        };
        let mut arguments = arguments.iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--" => {
                    config.argv = arguments.cloned().collect();
                    break;
                }
                ENDPOINT_FLAG => {
                    let name = arguments.next().ok_or("--endpoint takes a name")?;
                    if !endpoint_name(name) || config.endpoints.contains(name) {
                        return Err(format!("{name:?} does not name one more endpoint"));
                    }
                    if config.endpoints.len() >= MAX_ENDPOINTS {
                        return Err(format!("at most {MAX_ENDPOINTS} endpoints"));
                    }
                    config.endpoints.push(name.clone());
                }
                STOP_GRACE_FLAG => {
                    let millis = arguments
                        .next()
                        .and_then(|millis| millis.parse().ok())
                        .ok_or("--stop-grace-ms takes milliseconds")?;
                    config.stop_grace = Duration::from_millis(millis);
                }
                other => return Err(format!("{other:?} is no argument of serve-run")),
            }
        }
        if config.argv.is_empty() {
            return Err("no program after --".into());
        }
        Ok(config)
    }
}

/// `tilo-legacy-agent serve-run …`: serve the run `arguments` describe on
/// this process's standard input and output, and answer with the program's
/// own exit status.
#[must_use]
pub fn main(arguments: &[String]) -> ExitCode {
    let config = match Config::parse(arguments) {
        Ok(config) => config,
        Err(why) => {
            eprintln!("tilo-legacy-agent serve-run: {why}");
            return ExitCode::from(2);
        }
    };
    // SAFETY: the handler only stores to an atomic, which a signal handler
    // may do; `prctl` with these arguments changes how the kernel treats
    // this process's orphans and takes no memory. Orphans of the run become
    // the launcher's children, so they stay of the run and end with it.
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::signal(
                signal,
                signalled as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, libc::c_ulong::from(1_u8));
    }
    ExitCode::from(run(&config, std::io::stdin(), std::io::stdout()))
}

/// The program `config` names, each endpoint's address written where its
/// placeholder stood in an argument or a variable's value.
fn program(config: &Config, endpoints: &[Endpoint]) -> Option<Command> {
    let placed = |text: &str| {
        endpoints.iter().fold(text.to_owned(), |text, endpoint| {
            text.replace(&placeholder(&endpoint.name), &endpoint.address())
        })
    };
    let (name, arguments) = config.argv.split_first()?;
    let mut command = Command::new(placed(name));
    command.args(arguments.iter().map(|argument| placed(argument)));
    for (key, value) in std::env::vars_os() {
        if let Some(value) = value.to_str()
            && value.contains("{endpoint:")
        {
            command.env(key, placed(value));
        }
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the closure runs in the child between fork and exec and only
    // calls `prctl`, which is safe there: the program is ended by the kernel
    // if the launcher dies without ending it.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        command.pre_exec(|| {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::c_ulong::from(libc::SIGKILL.unsigned_abs()),
            );
            Ok(())
        });
    }
    Some(command)
}

/// Serve the run until its program ended; the launcher's exit status.
fn run(
    config: &Config,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> u8 {
    let shared = Arc::new(Shared {
        out: Mutex::new(Box::new(output)),
        waiting: Mutex::new(HashMap::new()),
        next: AtomicU64::new(1),
        open: AtomicUsize::new(0),
        launcher: std::process::id(),
        processes: Processes::of_this_machine(),
        host_gone: AtomicBool::new(false),
        host_left: AtomicBool::new(false),
        stop_asked: AtomicBool::new(false),
    });
    let fail = |message: String, status: u8| {
        shared.tell(&FromRun::Failed { message });
        status
    };
    let mut endpoints = Vec::new();
    for name in &config.endpoints {
        match Endpoint::open(name) {
            Ok(endpoint) => endpoints.push(endpoint),
            Err(error) => {
                return fail(
                    format!("the endpoint {name} did not open: {error}"),
                    UNUSABLE,
                );
            }
        }
    }
    let Some(mut command) = program(config, &endpoints) else {
        return fail("no program to start".into(), UNUSABLE);
    };
    shared.tell(&FromRun::Ready {
        version: VERSION,
        endpoints: config.endpoints.clone(),
    });
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let name = config.argv.first().map_or("", String::as_str);
            return fail(format!("{name}: {error}"), NOT_STARTED);
        }
    };
    shared.tell(&FromRun::Started { pid: child.id() });
    for endpoint in endpoints {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || endpoint.serve(&shared));
    }
    let (drained, pumps_done) = mpsc::channel();
    let pumps = [
        child
            .stdout
            .take()
            .map(|out| pump(out, &shared, FromRun::Out, drained.clone())),
        child
            .stderr
            .take()
            .map(|err| pump(err, &shared, FromRun::Err, drained.clone())),
    ]
    .into_iter()
    .flatten()
    .count();
    drop(drained);
    let _input_open = listen(&shared, input, child.stdin.take());
    let ended = Arc::new(AtomicBool::new(false));
    let stopper = {
        let (shared, ended, grace) = (Arc::clone(&shared), Arc::clone(&ended), config.stop_grace);
        std::thread::spawn(move || stop_when_asked(&shared, &ended, grace))
    };
    let status = wait(&mut child);
    ended.store(true, Ordering::SeqCst);
    let _ = stopper.join();
    // What the program left running is not the run any more; what it had
    // printed until then is still in its pipes, and read to their end.
    signal_run(&shared, libc::SIGKILL);
    for _ in 0..pumps {
        if pumps_done.recv_timeout(DRAIN).is_err() {
            break;
        }
    }
    shared.tell(&FromRun::Exited(status));
    u8::try_from(status.shell_code()).unwrap_or(1)
}

fn wait(child: &mut Child) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt as _;
    child.wait().map_or(
        ExitStatus {
            code: None,
            signal: None,
        },
        |status| ExitStatus {
            code: status.code(),
            signal: status.signal(),
        },
    )
}

/// Carry what `source` yields to the host as `frame`s, until it ends.
fn pump<R: Read + Send + 'static>(
    mut source: R,
    shared: &Arc<Shared>,
    frame: fn(Vec<u8>) -> FromRun,
    done: Sender<()>,
) {
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        let mut buffer = vec![0_u8; OUTPUT_CHUNK];
        loop {
            let read = match source.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            if !shared.tell(&frame(buffer.get(..read).unwrap_or_default().to_vec())) {
                break;
            }
        }
        let _ = done.send(());
    });
}

/// Read the host's frames for as long as it sends them, on a thread of its
/// own: the program's input (written by another thread, so a program that
/// does not read never holds an answer up), the answers to what was asked,
/// and the ask to stop. The frames ending is the host going away.
///
/// The program's input stays open for as long as the caller keeps what this
/// returns: a host that goes away does not end the program's input, it has
/// the program asked to end, so every program ends the same way then.
fn listen(
    shared: &Arc<Shared>,
    mut input: impl Read + Send + 'static,
    stdin: Option<ChildStdin>,
) -> Sender<Option<Vec<u8>>> {
    let (typing, typed) = mpsc::channel::<Option<Vec<u8>>>();
    let kept = typing.clone();
    std::thread::spawn(move || {
        let mut stdin = stdin;
        while let Ok(typed) = typed.recv() {
            match (typed, stdin.as_mut()) {
                (Some(bytes), Some(pipe)) => {
                    if pipe.write_all(&bytes).and_then(|()| pipe.flush()).is_err() {
                        stdin = None;
                    }
                }
                (None, _) => stdin = None,
                (Some(_), None) => {}
            }
        }
    });
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        loop {
            match recv::<_, ToRun>(&mut input) {
                Ok(Some(ToRun::In(bytes))) => {
                    let _ = typing.send(Some(bytes));
                }
                Ok(Some(ToRun::CloseIn)) => {
                    let _ = typing.send(None);
                }
                Ok(Some(ToRun::Answer {
                    id,
                    status,
                    headers,
                })) => shared.answer(id, Answered::Head { status, headers }),
                Ok(Some(ToRun::AnswerBody { id, bytes, last })) => {
                    shared.answer(id, Answered::Body { bytes, last });
                }
                Ok(Some(ToRun::Stop)) => shared.stop_asked.store(true, Ordering::SeqCst),
                Ok(None) | Err(_) => {
                    shared.host_left.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    });
    kept
}

/// Send `signal` to every process of the run.
fn signal_run(shared: &Shared, signal: libc::c_int) {
    for process in shared.processes.descendants(shared.launcher) {
        if let Ok(process) = libc::pid_t::try_from(process) {
            // SAFETY: `kill` has no memory preconditions; the process is
            // one this launcher's own tree holds.
            unsafe { libc::kill(process, signal) };
        }
    }
}

/// Wait until the program `ended`, or `grace` passed; whether it ended.
fn ended_within(ended: &AtomicBool, grace: Duration) -> bool {
    let until = Instant::now().checked_add(grace);
    while !ended.load(Ordering::SeqCst) {
        if until.is_none_or(|until| Instant::now() >= until) {
            return false;
        }
        std::thread::sleep(TICK);
    }
    true
}

/// Once the host asks the program to end, or goes away, end the run: the
/// host's ask interrupts it first, as a person's Ctrl+C would, and gives it
/// `grace`; then, and at once for a host that is gone, it is asked to end,
/// and ended when it has not within its last grace.
fn stop_when_asked(shared: &Shared, ended: &AtomicBool, grace: Duration) {
    let asked = || shared.stop_asked.load(Ordering::SeqCst);
    while !asked() && !shared.alone() {
        if ended.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(TICK);
    }
    if !shared.alone() {
        signal_run(shared, libc::SIGINT);
        if ended_within(ended, grace) {
            return;
        }
    }
    signal_run(shared, libc::SIGTERM);
    if ended_within(ended, grace.min(END_GRACE)) {
        return;
    }
    signal_run(shared, libc::SIGKILL);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(arguments: &[&str]) -> Result<Config, String> {
        let arguments: Vec<String> = arguments
            .iter()
            .map(|&argument| argument.to_owned())
            .collect();
        Config::parse(&arguments)
    }

    tilo_test_env::scenario! {
        fn the_launcher_reads_what_the_host_writes_for_it;

        phase the_hosts_own_arguments_are_read_back;
        {
            let program = vec!["claude".to_owned(), "--print".to_owned()];
            let written = legacy_agent_protocol::served::arguments(
                &["model", "tilo"],
                Duration::from_millis(1500),
                &program,
            );
            // The mode's own name is the program's first argument, not the mode's.
            assert_eq!(
                Config::parse(written.get(1..).unwrap()).unwrap(),
                Config {
                    endpoints: vec!["model".into(), "tilo".into()],
                    stop_grace: Duration::from_millis(1500),
                    argv: program,
                }
            );
        }

        phase a_program_keeps_its_own_arguments_after_the_separator;
        {
            let config = parsed(&["--", "sh", "--endpoint", "--"]).unwrap();
            assert_eq!(config.argv, ["sh", "--endpoint", "--"]);
            assert!(config.endpoints.is_empty());
            assert_eq!(config.stop_grace, STOP_GRACE);
        }

        phase what_cannot_be_served_is_said;
        {
            assert_eq!(parsed(&["--endpoint", "model"]).unwrap_err(), "no program after --");
            assert!(parsed(&["--endpoint", "Mo del", "--", "sh"]).is_err());
            assert!(parsed(&["--endpoint", "a", "--endpoint", "a", "--", "sh"]).is_err());
            assert!(parsed(&["--stop-grace-ms", "soon", "--", "sh"]).is_err());
            assert!(parsed(&["--verbose", "--", "sh"]).is_err());
        }
    }
}

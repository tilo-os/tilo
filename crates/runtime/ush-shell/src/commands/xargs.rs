//! `xargs`: build command lines from standard input and run them, as GNU
//! xargs does, each one dispatched back to the shell (never a process).
//!
//! Items are read as GNU reads them: separated by blanks and newlines, with
//! `'…'`, `"…"` and `\` quoting (`-0` and `-d` take items between
//! delimiters, unquoted), and as many go on one command as `-n`, `-L` and
//! `-s` allow. `-I` runs one command per input line.
//!
//! Under `-P N`, up to N commands run at once, on threads where the shell
//! has them (sequentially in WebAssembly). Each command's output is
//! collected and written whole when it ends, so the output of two commands
//! never interleaves within a line; the order of commands is whichever
//! finished first, as GNU's is. `-P 0` runs as many at once as the host has
//! cores: in-process commands share them.
//!
//! The exit status is GNU's: 123 if any command failed with 1 to 125 (or a
//! status above 128 that is no stop), 124 if one exited with 255 (no more
//! are started), 126 or 127 if one could not be run or found.

use std::sync::{Mutex, MutexGuard, PoisonError};

use super::Ctx;
use crate::io::{Input, Output, Stdio};

/// The longest command line built when `-s` does not say, in bytes: GNU
/// xargs' own default, so commands split where GNU's would.
pub const DEFAULT_SIZE: usize = 128 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Split {
    /// Blanks and newlines, with quotes.
    Words,
    /// Between this byte, unquoted.
    Delimiter(u8),
}

#[expect(clippy::struct_excessive_bools, reason = "one switch per GNU flag")]
struct Opts {
    split: Split,
    max_args: Option<usize>,
    max_lines: Option<usize>,
    replace: Option<String>,
    no_run_if_empty: bool,
    verbose: bool,
    interactive: bool,
    exit_if_too_long: bool,
    procs: usize,
    size: usize,
    eof: Option<String>,
    arg_file: Option<String>,
}

/// What stopped reading or running, already reported, with this status.
struct Fail(i32);

/// Input items, read a chunk at a time so a long input (`yes | xargs`) is
/// never read whole.
struct Items {
    input: Input,
    buf: Vec<u8>,
    at: usize,
    eof: bool,
    /// The `-E` string was read: nothing after it is an item.
    ended: bool,
}

impl Items {
    fn byte(&mut self) -> Result<Option<u8>, std::io::Error> {
        if self.at >= self.buf.len() {
            if self.eof {
                return Ok(None);
            }
            self.buf.resize(8192, 0);
            let n = self.input.read(&mut self.buf)?;
            self.buf.truncate(n);
            self.at = 0;
            if n == 0 {
                self.eof = true;
                return Ok(None);
            }
        }
        let b = self.buf.get(self.at).copied();
        self.at = self.at.saturating_add(1);
        Ok(b)
    }

    /// The next item, and whether it ended its input line (for `-L`). With
    /// `lines`, blanks do not end an item (`-I`).
    fn next(&mut self, ctx: &Ctx<'_>, o: &Opts) -> Result<Option<(String, bool)>, Fail> {
        if self.ended {
            return Ok(None);
        }
        let read_error = |e: std::io::Error| {
            ctx.error(format!("read error: {}", super::describe(&e)));
            Fail(1)
        };
        let lines = o.replace.is_some();
        let mut item = Vec::new();
        let found = match o.split {
            Split::Delimiter(d) => loop {
                match self.byte().map_err(read_error)? {
                    None if item.is_empty() => return Ok(None),
                    None => break (item, true),
                    Some(b) if b == d => break (item, true),
                    Some(b) => item.push(b),
                }
            },
            Split::Words => {
                let mut have = false;
                let mut quote: Option<u8> = None;
                loop {
                    let Some(b) = self.byte().map_err(read_error)? else {
                        if let Some(q) = quote {
                            let kind = if q == b'"' { "double" } else { "single" };
                            ctx.error(format!("unmatched {kind} quote; by default quotes are special to xargs unless you use the -0 option"));
                            return Err(Fail(1));
                        }
                        if have {
                            break (item, true);
                        }
                        return Ok(None);
                    };
                    if let Some(q) = quote {
                        if b == q {
                            quote = None;
                        } else if b == b'\n' {
                            let kind = if q == b'"' { "double" } else { "single" };
                            ctx.error(format!("unmatched {kind} quote; by default quotes are special to xargs unless you use the -0 option"));
                            return Err(Fail(1));
                        } else {
                            item.push(b);
                        }
                        continue;
                    }
                    match b {
                        b'\\' => {
                            if let Some(next) = self.byte().map_err(read_error)? {
                                item.push(next);
                            }
                            have = true;
                        }
                        b'\'' | b'"' => {
                            quote = Some(b);
                            have = true;
                        }
                        b'\n' if have => break (item, true),
                        b'\n' => {}
                        b' ' | b'\t' if !have => {}
                        b' ' | b'\t' if !lines => break (item, false),
                        other => {
                            item.push(other);
                            have = true;
                        }
                    }
                }
            }
        };
        let (bytes, ends_line) = found;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if o.split == Split::Words && o.eof.as_deref() == Some(text.as_str()) {
            self.ended = true;
            return Ok(None);
        }
        Ok(Some((text, ends_line)))
    }
}

/// Command lines, built from the items as they are read.
struct Batcher {
    items: Items,
    command: Vec<String>,
    /// An item read but not fitted on the command line before it.
    carry: Option<(String, bool)>,
    built_any: bool,
    done: bool,
}

fn line_len(args: &[String]) -> usize {
    args.iter()
        .map(|a| a.len().saturating_add(1))
        .fold(0, usize::saturating_add)
}

impl Batcher {
    fn next(&mut self, ctx: &Ctx<'_>, o: &Opts) -> Result<Option<Vec<String>>, Fail> {
        if self.done {
            return Ok(None);
        }
        if let Some(repl) = &o.replace {
            let Some((item, _)) = self.items.next(ctx, o)? else {
                self.done = true;
                return Ok(None);
            };
            self.built_any = true;
            let argv: Vec<String> = self
                .command
                .iter()
                .map(|a| a.replace(repl.as_str(), &item))
                .collect();
            if line_len(&argv) > o.size {
                ctx.error("argument line too long");
                return Err(Fail(1));
            }
            return Ok(Some(argv));
        }
        let mut argv = self.command.clone();
        let base = argv.len();
        let mut lines = 0_usize;
        loop {
            let next = match self.carry.take() {
                Some(item) => Some(item),
                None => self.items.next(ctx, o)?,
            };
            let Some((item, ends_line)) = next else {
                self.done = true;
                break;
            };
            let would = line_len(&argv).saturating_add(item.len()).saturating_add(1);
            if would > o.size {
                if argv.len() == base {
                    ctx.error("argument line too long");
                    return Err(Fail(1));
                }
                if o.exit_if_too_long && (o.max_args.is_some() || o.max_lines.is_some()) {
                    ctx.error("argument list too long");
                    return Err(Fail(1));
                }
                self.carry = Some((item, ends_line));
                break;
            }
            argv.push(item);
            if ends_line {
                lines = lines.saturating_add(1);
            }
            let taken = argv.len().saturating_sub(base);
            if o.max_args.is_some_and(|n| taken >= n) || o.max_lines.is_some_and(|n| lines >= n) {
                break;
            }
        }
        if argv.len() == base && (self.built_any || o.no_run_if_empty) {
            return Ok(None);
        }
        self.built_any = true;
        Ok(Some(argv))
    }
}

/// How the commands run so far went.
#[derive(Default)]
struct State {
    /// Set when no more commands are to start, with xargs' status.
    stop: Option<i32>,
    failed: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One digit-only option value.
fn count(ctx: &Ctx<'_>, flag: &str, value: &str, least: usize) -> Result<usize, i32> {
    match value.parse::<usize>() {
        Ok(n) if n >= least => Ok(n),
        Ok(_) => {
            ctx.error(format!(
                "value {value} for {flag} option should be >= {least}"
            ));
            Err(1)
        }
        Err(_) => {
            ctx.error(format!("invalid number \"{value}\" for {flag} option"));
            Err(1)
        }
    }
}

/// The byte `-d` names: one character, or an escape (`\n`, `\t`, `\0`,
/// `\\`, `\xHH`, `\NNN`).
fn delimiter(spec: &str) -> Option<u8> {
    let bytes = spec.as_bytes();
    match bytes {
        [one] => Some(*one),
        [b'\\', rest @ ..] => match rest {
            [b'n'] => Some(b'\n'),
            [b't'] => Some(b'\t'),
            [b'r'] => Some(b'\r'),
            [b'v'] => Some(0x0b),
            [b'f'] => Some(0x0c),
            [b'a'] => Some(0x07),
            [b'b'] => Some(0x08),
            [b'\\'] => Some(b'\\'),
            [b'x', hex @ ..] => u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok(),
            octal if !octal.is_empty() && octal.iter().all(|b| (b'0'..=b'7').contains(b)) => {
                u8::from_str_radix(std::str::from_utf8(octal).ok()?, 8).ok()
            }
            _ => None,
        },
        _ => None,
    }
}

fn options(ctx: &Ctx<'_>, args: &[String]) -> Result<(Opts, Vec<String>), i32> {
    let mut o = Opts {
        split: Split::Words,
        max_args: None,
        max_lines: None,
        replace: None,
        no_run_if_empty: false,
        verbose: false,
        interactive: false,
        exit_if_too_long: false,
        procs: 1,
        size: DEFAULT_SIZE,
        eof: None,
        arg_file: None,
    };
    let mut i = 0_usize;
    while let Some(arg) = args.get(i) {
        i = i.saturating_add(1);
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') || arg.len() < 2 {
            i = i.saturating_sub(1);
            break;
        }
        // Long options: `--name=value` or `--name value`.
        let (flag, mut attached): (String, Option<String>) =
            if let Some(long) = arg.strip_prefix("--") {
                match long.split_once('=') {
                    Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                    None => (arg.clone(), None),
                }
            } else {
                // A cluster of short options: each that takes a value takes the
                // rest of the cluster, or the next argument.
                let mut chars = arg.chars().skip(1).peekable();
                let mut last: Option<(String, Option<String>)> = None;
                while let Some(c) = chars.next() {
                    let takes = matches!(c, 'd' | 'n' | 'L' | 'I' | 'P' | 's' | 'a' | 'E');
                    let optional = matches!(c, 'i' | 'e' | 'l');
                    if takes || optional {
                        let rest: String = chars.by_ref().collect();
                        last = Some((format!("-{c}"), (!rest.is_empty()).then_some(rest)));
                        break;
                    }
                    if chars.peek().is_some() {
                        apply(ctx, &mut o, &format!("-{c}"), None)?;
                    } else {
                        last = Some((format!("-{c}"), None));
                    }
                }
                match last {
                    Some(l) => l,
                    None => continue,
                }
            };
        let needs = matches!(
            flag.as_str(),
            "-d" | "-n"
                | "-L"
                | "-I"
                | "-P"
                | "-s"
                | "-a"
                | "-E"
                | "--delimiter"
                | "--max-args"
                | "--max-lines"
                | "--max-procs"
                | "--max-chars"
                | "--arg-file"
        );
        if needs && attached.is_none() {
            let Some(v) = args.get(i) else {
                ctx.error(format!(
                    "option requires an argument -- '{}'",
                    flag.trim_start_matches('-')
                ));
                return Err(1);
            };
            attached = Some(v.clone());
            i = i.saturating_add(1);
        }
        apply(ctx, &mut o, &flag, attached)?;
    }
    let command: Vec<String> = args.get(i..).unwrap_or_default().to_vec();
    Ok((o, command))
}

fn apply(ctx: &Ctx<'_>, o: &mut Opts, flag: &str, value: Option<String>) -> Result<(), i32> {
    let v = value.unwrap_or_default();
    match flag {
        "-0" | "--null" => o.split = Split::Delimiter(0),
        "-d" | "--delimiter" => {
            let Some(d) = delimiter(&v) else {
                ctx.error(format!("invalid input delimiter specification {v}: the delimiter must be either a single character or an escape sequence starting with \\."));
                return Err(1);
            };
            o.split = Split::Delimiter(d);
        }
        "-n" | "--max-args" => {
            o.max_args = Some(count(ctx, flag, &v, 1)?);
            o.max_lines = None;
            o.replace = None;
        }
        "-L" | "--max-lines" | "-l" => {
            let n = if v.is_empty() {
                1
            } else {
                count(ctx, flag, &v, 1)?
            };
            o.max_lines = Some(n);
            o.max_args = None;
            o.replace = None;
        }
        "-I" | "-i" | "--replace" => {
            o.replace = Some(if v.is_empty() && flag != "-I" {
                "{}".into()
            } else {
                v
            });
            o.max_args = None;
            o.max_lines = None;
            o.exit_if_too_long = true;
        }
        "-r" | "--no-run-if-empty" => o.no_run_if_empty = true,
        "-t" | "--verbose" => o.verbose = true,
        "-p" | "--interactive" => {
            o.interactive = true;
            o.verbose = true;
        }
        "-x" | "--exit" => o.exit_if_too_long = true,
        "-P" | "--max-procs" => o.procs = count(ctx, flag, &v, 0)?,
        "-s" | "--max-chars" => o.size = count(ctx, flag, &v, 1)?,
        "-E" | "-e" | "--eof" => o.eof = (!v.is_empty()).then_some(v),
        "-a" | "--arg-file" => o.arg_file = Some(v),
        other => return Err(ctx.unsupported(other)),
    }
    Ok(())
}

pub fn xargs(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let (o, mut command) = match options(ctx, args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    if command.is_empty() {
        command.push("echo".into());
    }
    // The commands read xargs' input only when the items come from a file.
    let (input, child_stdin) = match &o.arg_file {
        Some(file) => match ctx.read_operand(file) {
            Ok(bytes) => (Input::bytes(bytes), ctx.io.stdin.clone()),
            Err(e) => {
                ctx.error(format!(
                    "Cannot open input file '{file}': {}",
                    super::describe(&e)
                ));
                return 1;
            }
        },
        None => (ctx.io.stdin.clone(), Input::null()),
    };
    let batcher = Mutex::new(Batcher {
        items: Items {
            input,
            buf: Vec::new(),
            at: 0,
            eof: false,
            ended: false,
        },
        command,
        carry: None,
        built_any: false,
        done: false,
    });
    let state = Mutex::new(State::default());
    let procs = workers(o.procs);
    run_all(ctx, &o, &batcher, &state, &child_stdin, procs);
    let state = lock(&state);
    match state.stop {
        Some(code) => code,
        None if state.failed => 123,
        None => 0,
    }
}

/// How many commands run at once.
fn workers(asked: usize) -> usize {
    #[cfg(not(target_family = "wasm"))]
    {
        if asked == 0 {
            return std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        }
        asked
    }
    #[cfg(target_family = "wasm")]
    {
        let _ = asked;
        1
    }
}

#[cfg(not(target_family = "wasm"))]
fn run_all(
    ctx: &Ctx<'_>,
    o: &Opts,
    batcher: &Mutex<Batcher>,
    state: &Mutex<State>,
    stdin: &Input,
    procs: usize,
) {
    if procs <= 1 {
        worker(ctx, o, batcher, state, stdin, false);
        return;
    }
    std::thread::scope(|scope| {
        for _ in 0..procs {
            scope.spawn(|| worker(ctx, o, batcher, state, stdin, true));
        }
    });
}

#[cfg(target_family = "wasm")]
fn run_all(
    ctx: &Ctx<'_>,
    o: &Opts,
    batcher: &Mutex<Batcher>,
    state: &Mutex<State>,
    stdin: &Input,
    _: usize,
) {
    worker(ctx, o, batcher, state, stdin, false);
}

/// Take command lines and run them until there are none or one says stop.
/// `buffered`: collect each command's output and write it whole.
fn worker(
    ctx: &Ctx<'_>,
    o: &Opts,
    batcher: &Mutex<Batcher>,
    state: &Mutex<State>,
    stdin: &Input,
    buffered: bool,
) {
    loop {
        if lock(state).stop.is_some() {
            return;
        }
        if ctx.host.interrupted() {
            lock(state).stop.get_or_insert(130);
            return;
        }
        let next = lock(batcher).next(ctx, o);
        let argv = match next {
            Ok(Some(argv)) => argv,
            Ok(None) => return,
            Err(Fail(code)) => {
                lock(state).stop.get_or_insert(code);
                return;
            }
        };
        let line = argv.join(" ");
        if o.interactive {
            if !ctx.dispatch.confirm(&format!("{line} ?...")) {
                continue;
            }
        } else if o.verbose {
            let _ = ctx.io.stderr.write_str(&format!("{line}\n"));
        }
        let status = if buffered {
            let (stdout, out) = Output::buffer();
            let (stderr, err) = Output::buffer();
            let io = Stdio {
                stdin: stdin.clone(),
                stdout,
                stderr,
            };
            let status = ctx.dispatch.run(&argv, ctx.cwd, &io);
            drop(io);
            let _ = ctx.io.stderr.write_all(&err.take());
            if let Err(e) = ctx.io.stdout.write_all(&out.take()) {
                lock(state)
                    .stop
                    .get_or_insert_with(|| super::Stop::from(e).0);
                return;
            }
            status
        } else {
            let io = Stdio {
                stdin: stdin.clone(),
                stdout: ctx.io.stdout.clone(),
                stderr: ctx.io.stderr.clone(),
            };
            ctx.dispatch.run(&argv, ctx.cwd, &io)
        };
        let name = argv.first().map_or("", String::as_str);
        let mut st = lock(state);
        match status {
            0 => {}
            255 => {
                ctx.error(format!("{name}: exited with status 255; aborting"));
                st.stop.get_or_insert(124);
            }
            126 | 127 | 130 | 141 => {
                st.stop.get_or_insert(status);
            }
            _ => st.failed = true,
        }
    }
}

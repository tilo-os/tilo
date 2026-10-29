//! The tier-1 commands that run another command: `timeout`, `nice` and
//! `nohup` here, `xargs` in its own module (and `env` with the text
//! commands, `find -exec` with `find`).
//!
//! None of them starts a process. Each hands its command back to the shell
//! ([`Ctx::dispatch`]), which resolves it as it resolves any utility, so the
//! command is one of Tilo's own, a file action, or a program the host runs,
//! and whatever it changes belongs to the same run.
//!
//! What a process would have and an in-process command does not:
//!
//! - **Signals.** `timeout` stops its command when the time is up the one way
//!   an in-process command can be stopped: it is interrupted, and stops at
//!   its next step. `-s SIGNAL` is accepted and names the signal
//!   `--preserve-status` and `-v` report; `-k DURATION` is accepted and has
//!   nothing further to do, as there is no harder stop than that one.
//! - **Priority.** `nice` runs its command: there is no scheduler priority
//!   for a command that shares the shell's thread. `nice` alone prints `0`.
//! - **Hangups.** `nohup` runs its command with no terminal input. It writes
//!   no `nohup.out`: output stays where it was going, and when that is the
//!   terminal it says so.

use super::{Command, Ctx};
use crate::io::{Input, Std, Stdio};

pub const NAMES: &[&str] = &["nice", "nohup", "timeout", "xargs"];

pub fn lookup(name: &str) -> Option<Command> {
    Some(match name {
        "timeout" => timeout,
        "nice" => nice,
        "nohup" => nohup,
        "xargs" => super::xargs::xargs,
        _ => return None,
    })
}

/// The signals `timeout -s` and `--preserve-status` know, by name and
/// number, as Linux numbers them.
const SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("KILL", 9),
    ("USR1", 10),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("CONT", 18),
    ("STOP", 19),
];

fn signal(text: &str) -> Option<(&'static str, i32)> {
    if let Ok(n) = text.parse::<i32>() {
        return SIGNALS.iter().find(|(_, k)| *k == n).copied();
    }
    let upper = text.to_ascii_uppercase();
    let name = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|(s, _)| *s == name).copied()
}

/// A duration as `sleep` and `timeout` read it: a number of seconds, with
/// `s`, `m`, `h` or `d` after it for another unit. In milliseconds.
fn duration(text: &str) -> Option<u64> {
    let (number, factor) = match text.char_indices().last() {
        Some((at, c)) if at > 0 && "smhd".contains(c) => (
            text.get(..at)?,
            match c {
                'm' => 60.0,
                'h' => 3600.0,
                'd' => 86_400.0,
                _ => 1.0,
            },
        ),
        _ => (text, 1.0),
    };
    let seconds: f64 = number.parse().ok()?;
    if seconds.is_nan() || seconds < 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative, and `as` saturates: infinity is the longest wait there is"
    )]
    let millis = (seconds * factor * 1000.0).ceil() as u64;
    Some(millis)
}

/// `timeout [-s SIG] [-k DURATION] [--preserve-status] [--foreground] [-v]
/// DURATION COMMAND [ARG]...`
fn timeout(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut sig = ("TERM", 15);
    let (mut preserve, mut verbose) = (false, false);
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        let (flag, attached) = match first.as_str() {
            "--" => {
                rest = tail;
                break;
            }
            "--preserve-status" => {
                preserve = true;
                rest = tail;
                continue;
            }
            "--foreground" => {
                rest = tail;
                continue;
            }
            "-v" | "--verbose" => {
                verbose = true;
                rest = tail;
                continue;
            }
            f if f.starts_with("--signal=") => ("-s", f.get("--signal=".len()..)),
            f if f.starts_with("--kill-after=") => ("-k", f.get("--kill-after=".len()..)),
            "--signal" => ("-s", None),
            "--kill-after" => ("-k", None),
            f if f.len() > 1 && f.starts_with('-') && f.parse::<f64>().is_err() => {
                match f.get(..2) {
                    Some(short @ ("-s" | "-k")) => (short, f.get(2..).filter(|v| !v.is_empty())),
                    _ => return ctx.unsupported(f),
                }
            }
            _ => break,
        };
        let (value, after) = if let Some(v) = attached {
            (v.to_string(), tail)
        } else {
            let Some((v, after)) = tail.split_first() else {
                ctx.error(format!(
                    "option requires an argument -- '{}'",
                    flag.trim_start_matches('-')
                ));
                return 125;
            };
            (v.clone(), after)
        };
        rest = after;
        if flag == "-s" {
            let Some(found) = signal(&value) else {
                ctx.error(format!("'{value}': invalid signal"));
                return 125;
            };
            sig = found;
        } else if duration(&value).is_none() {
            ctx.error(format!("invalid time interval '{value}'"));
            return 125;
        }
    }
    let Some((limit, command)) = rest.split_first() else {
        ctx.error("missing operand");
        return 125;
    };
    let Some(millis) = duration(limit) else {
        ctx.error(format!("invalid time interval '{limit}'"));
        return 125;
    };
    if command.is_empty() {
        ctx.error(format!("missing operand after '{limit}'"));
        return 125;
    }
    if millis == 0 {
        return ctx.dispatch.run(command, ctx.cwd, ctx.io);
    }
    if let Some(status) = ctx.dispatch.run_for(command, ctx.cwd, ctx.io, millis) {
        return status;
    }
    if verbose {
        let name = command.first().map_or("", String::as_str);
        ctx.error(format!("sending signal {} to command '{name}'", sig.0));
    }
    if preserve {
        128_i32.saturating_add(sig.1)
    } else {
        124
    }
}

/// `nice [-n N] [COMMAND [ARG]...]`
fn nice(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        let adjustment = match first.as_str() {
            "--" => {
                rest = tail;
                break;
            }
            "-n" | "--adjustment" => {
                let Some((n, after)) = tail.split_first() else {
                    ctx.error("option requires an argument -- 'n'");
                    return 125;
                };
                rest = after;
                n.clone()
            }
            f if f.starts_with("--adjustment=") => {
                rest = tail;
                f.get("--adjustment=".len()..).unwrap_or("").to_string()
            }
            f if f.starts_with("-n") && f.len() > 2 => {
                rest = tail;
                f.get(2..).unwrap_or("").to_string()
            }
            // The old spelling: `nice -5 cmd`, `nice --5 cmd`.
            f if f.len() > 1
                && f.starts_with('-')
                && f.get(1..)
                    .is_some_and(|n| n.trim_start_matches('-').parse::<i64>().is_ok()) =>
            {
                rest = tail;
                f.get(1..).unwrap_or("").to_string()
            }
            f if f.len() > 1 && f.starts_with('-') => return ctx.unsupported(f),
            _ => break,
        };
        if adjustment.parse::<i64>().is_err() {
            ctx.error(format!("invalid adjustment '{adjustment}'"));
            return 125;
        }
    }
    if rest.is_empty() {
        return match ctx.out_str("0\n") {
            Ok(()) => 0,
            Err(stop) => stop.0,
        };
    }
    ctx.dispatch.run(rest, ctx.cwd, ctx.io)
}

/// `nohup COMMAND [ARG]...`
fn nohup(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let command = match args.split_first() {
        Some((first, tail)) if first == "--" => tail,
        Some((first, _)) if first.len() > 1 && first.starts_with('-') => {
            return ctx.unsupported(first);
        }
        _ => args,
    };
    if command.is_empty() {
        ctx.error("missing operand");
        return 125;
    }
    let terminal_in = ctx.io.stdin.host_stream() == Some(Std::In);
    let terminal_out = ctx.io.stdout.host_stream() == Some(Std::Out);
    if terminal_out {
        ctx.error(if terminal_in {
            "ignoring input; output stays on this terminal (no nohup.out here)"
        } else {
            "output stays on this terminal (no nohup.out here)"
        });
    }
    let io = Stdio {
        stdin: if terminal_in {
            Input::null()
        } else {
            ctx.io.stdin.clone()
        },
        stdout: ctx.io.stdout.clone(),
        stderr: ctx.io.stderr.clone(),
    };
    ctx.dispatch.run(command, ctx.cwd, &io)
}

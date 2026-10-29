//! The shell's own built-ins: POSIX's special built-ins, the regular ones a
//! shell must have to work at all (`cd`, `read`, `test`), and the bash ones
//! people use (`local`, `declare`, `mapfile`, `shopt`).

use brush_parser::ast;

use std::fmt::Write as _;

use crate::shell::{Exec, Flow, Shell};
use crate::vars::{Value, valid_name};

pub(crate) type Builtin = fn(&mut Shell, &[String]) -> Exec;

/// POSIX's special built-ins: an error in one ends a script, and their
/// assignments stay.
pub(crate) fn special(name: &str) -> Option<Builtin> {
    Some(match name {
        ":" => |_, _| Ok(0),
        "." | "source" => source,
        "break" => |sh, args| loop_control(sh, args, true),
        "continue" => |sh, args| loop_control(sh, args, false),
        "eval" => eval,
        "exec" => exec,
        "exit" => exit,
        "return" => return_builtin,
        "set" => set,
        "shift" => shift,
        "trap" => trap,
        "unset" => unset,
        "times" => |sh, _| Err(sh.unsupported("`times`")),
        _ => return None,
    })
}

/// Built-ins that are not special.
pub(crate) fn regular(name: &str) -> Option<Builtin> {
    Some(match name {
        "true" => |_, _| Ok(0),
        "false" => |_, _| Ok(1),
        "echo" => echo,
        "printf" => crate::printf::printf,
        "pwd" => pwd,
        "cd" => cd,
        "test" => crate::test::test,
        "[" => crate::test::bracket,
        "read" => read,
        "type" => type_builtin,
        "command" => command,
        "builtin" => builtin,
        "let" => let_builtin,
        "getopts" => getopts,
        "alias" => alias,
        "unalias" => unalias,
        "shopt" => shopt,
        "hash" => |_, _| Ok(0),
        "wait" => crate::jobs::wait_builtin,
        "jobs" => crate::jobs::jobs_builtin,
        "fg" => crate::jobs::fg_builtin,
        "bg" => crate::jobs::bg_builtin,
        "kill" => crate::jobs::kill_builtin,
        "umask" => umask,
        "mapfile" | "readarray" => mapfile,
        "history" => history,
        "disown" | "suspend" => |sh, _| Err(sh.unsupported("this job control built-in")),
        "ulimit" => |sh, _| Err(sh.unsupported("`ulimit`")),
        "enable" | "complete" | "compgen" | "bind" => {
            |sh, _| Err(sh.unsupported("this bash built-in"))
        }
        _ => return None,
    })
}

/// Every built-in's name, for Tab completion.
pub(crate) const NAMES: &[&str] = &[
    ":",
    ".",
    "source",
    "break",
    "continue",
    "eval",
    "exec",
    "exit",
    "return",
    "set",
    "shift",
    "trap",
    "unset",
    "true",
    "false",
    "echo",
    "printf",
    "pwd",
    "cd",
    "test",
    "[",
    "read",
    "type",
    "command",
    "builtin",
    "let",
    "getopts",
    "alias",
    "unalias",
    "shopt",
    "wait",
    "hash",
    "umask",
    "mapfile",
    "readarray",
    "jobs",
    "fg",
    "bg",
    "kill",
    "history",
    "declare",
    "typeset",
    "local",
    "export",
    "readonly",
];

/// The shell's reserved words that start a command, for Tab completion.
pub(crate) const KEYWORDS: &[&str] = &[
    "if", "case", "for", "while", "until", "function", "select", "[[",
];

/// The built-ins whose `name=value` arguments are assignments.
pub(crate) fn is_declaration(name: &str) -> bool {
    matches!(
        name,
        "declare" | "typeset" | "local" | "export" | "readonly"
    )
}

/// The shell's reserved words, for `type`.
pub(crate) fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "case"
            | "esac"
            | "for"
            | "while"
            | "until"
            | "do"
            | "done"
            | "in"
            | "function"
            | "select"
            | "time"
            | "{"
            | "}"
            | "!"
            | "[["
            | "]]"
    )
}

fn usage(sh: &Shell, message: impl std::fmt::Display) -> i32 {
    sh.error(message);
    2
}

fn eval(sh: &mut Shell, args: &[String]) -> Exec {
    let text = args.join(" ");
    if text.trim().is_empty() {
        return Ok(0);
    }
    sh.eval_text(&text)
}

fn source(sh: &mut Shell, args: &[String]) -> Exec {
    let Some((file, rest)) = args.split_first() else {
        return Ok(usage(sh, ".: filename argument required"));
    };
    let path = crate::paths::join(&sh.cwd, file);
    let text = match sh.host.open_read(&path).and_then(|mut r| {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut r, &mut bytes).map(|_| bytes)
    }) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            sh.error(format!("{file}: {}", crate::commands::describe(&e)));
            return Ok(1);
        }
    };
    let saved = (!rest.is_empty()).then(|| std::mem::replace(&mut sh.positional, rest.to_vec()));
    sh.call_depth = sh.call_depth.saturating_add(1);
    let result = sh.eval_text(&text);
    sh.call_depth = sh.call_depth.saturating_sub(1);
    if let Some(saved) = saved {
        sh.positional = saved;
    }
    match result {
        Err(Flow::Return(status)) => Ok(status),
        other => other,
    }
}

fn exec(sh: &mut Shell, args: &[String]) -> Exec {
    if args.is_empty() {
        // The redirections stay: the caller does not restore them.
        return Ok(0);
    }
    let words: Vec<String> = args.iter().map(|a| crate::expand::quote(a)).collect();
    let status = sh.eval_text(&words.join(" "))?;
    Err(Flow::Exit(status))
}

fn exit(sh: &mut Shell, args: &[String]) -> Exec {
    let status = match args.first() {
        Some(n) => {
            if let Ok(n) = n.parse::<i64>() {
                i32::try_from(n.rem_euclid(256)).unwrap_or(0)
            } else {
                sh.error(format!("exit: {n}: numeric argument required"));
                2
            }
        }
        None => sh.last_status,
    };
    Err(Flow::Exit(status))
}

fn return_builtin(sh: &mut Shell, args: &[String]) -> Exec {
    if !sh.vars.in_function() && sh.call_depth == 0 {
        sh.error("return: can only `return' from a function or sourced script");
        return Ok(1);
    }
    let status = match args.first() {
        Some(n) => {
            if let Ok(n) = n.parse::<i64>() {
                i32::try_from(n.rem_euclid(256)).unwrap_or(0)
            } else {
                sh.error(format!("return: {n}: numeric argument required"));
                2
            }
        }
        None => sh.last_status,
    };
    Err(Flow::Return(status))
}

fn loop_control(sh: &Shell, args: &[String], brk: bool) -> Exec {
    let n = match args.first().map(|a| a.parse::<u32>()) {
        None => 1,
        Some(Ok(n)) if n >= 1 => n,
        Some(_) => {
            sh.error("loop count out of range");
            return Ok(1);
        }
    };
    if sh.loop_depth == 0 {
        sh.error(format!(
            "{}: only meaningful in a `for', `while', or `until' loop",
            if brk { "break" } else { "continue" }
        ));
        return Ok(0);
    }
    let n = n.min(sh.loop_depth);
    Err(if brk {
        Flow::Break(n)
    } else {
        Flow::Continue(n)
    })
}

fn set_option(sh: &mut Shell, name: &str, on: bool) -> Result<(), String> {
    let o = &mut sh.options;
    match name {
        "errexit" | "e" => o.errexit = on,
        "nounset" | "u" => o.nounset = on,
        "xtrace" | "x" => o.xtrace = on,
        "noglob" | "f" => o.noglob = on,
        "noclobber" | "C" => o.noclobber = on,
        "allexport" | "a" => o.allexport = on,
        "pipefail" => o.pipefail = on,
        // The line editor has emacs keys only.
        "vi" if on => return Err("set -o vi".into()),
        // Accepted and meaningless here: job control is always on for an
        // interactive shell, there is no history file, and the editor has
        // emacs keys only.
        "vi"
        | "notify"
        | "b"
        | "monitor"
        | "m"
        | "hashall"
        | "h"
        | "emacs"
        | "history"
        | "B"
        | "braceexpand"
        | "interactive-comments"
        | "posix" => {}
        "verbose" | "v" | "noexec" | "n" | "physical" | "P" | "privileged" | "p" | "T" | "E"
        | "k" | "keyword" | "functrace" | "errtrace" | "onecmd" | "t" | "ignoreeof" => {
            return Err(format!("set -o {name}"));
        }
        other => return Err(format!("set: {other}: invalid option name")),
    }
    Ok(())
}

fn set(sh: &mut Shell, args: &[String]) -> Exec {
    if args.is_empty() {
        let mut out = String::new();
        for (name, var) in sh.vars.all() {
            if let Some(value) = var.value.as_ref().and_then(Value::scalar) {
                let _ = writeln!(out, "{name}={}", crate::expand::quote(&value));
            }
        }
        sh.out(&out);
        return Ok(0);
    }
    let mut i = 0_usize;
    while let Some(arg) = args.get(i) {
        let on = arg.starts_with('-');
        if arg == "--" {
            sh.positional = args.get(i.saturating_add(1)..).unwrap_or(&[]).to_vec();
            return Ok(0);
        }
        if arg == "-" {
            sh.options.xtrace = false;
            sh.positional = args.get(i.saturating_add(1)..).unwrap_or(&[]).to_vec();
            return Ok(0);
        }
        if !(on || arg.starts_with('+')) {
            sh.positional = args.get(i..).unwrap_or(&[]).to_vec();
            return Ok(0);
        }
        let flags: String = arg.chars().skip(1).collect();
        if flags == "o" {
            i = i.saturating_add(1);
            if let Some(name) = args.get(i) {
                if let Err(what) = set_option(sh, name, on) {
                    return Err(sh.unsupported(&what));
                }
            } else {
                let o = &sh.options;
                let mut out = String::new();
                for (name, value) in [
                    ("allexport", o.allexport),
                    ("errexit", o.errexit),
                    ("noclobber", o.noclobber),
                    ("noglob", o.noglob),
                    ("nounset", o.nounset),
                    ("pipefail", o.pipefail),
                    ("xtrace", o.xtrace),
                ] {
                    let _ = writeln!(out, "{name:<15}\t{}", if value { "on" } else { "off" });
                }
                sh.out(&out);
            }
        } else {
            for c in flags.chars() {
                if let Err(what) = set_option(sh, &c.to_string(), on) {
                    return Err(sh.unsupported(&what));
                }
            }
        }
        i = i.saturating_add(1);
    }
    Ok(0)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn shift(sh: &mut Shell, args: &[String]) -> Exec {
    let n = match args.first().map(|a| a.parse::<usize>()) {
        None => 1,
        Some(Ok(n)) => n,
        Some(Err(_)) => return Ok(usage(sh, "shift: numeric argument required")),
    };
    if n > sh.positional.len() {
        return Ok(1);
    }
    sh.positional.drain(..n);
    Ok(0)
}

fn trap(sh: &mut Shell, args: &[String]) -> Exec {
    let print = |sh: &Shell, only: &[String]| {
        let mut out = String::new();
        for (signal, action) in &sh.traps {
            if only.is_empty() || only.contains(signal) {
                let _ = writeln!(out, "trap -- {} {signal}", crate::expand::quote(action));
            }
        }
        sh.out(&out);
        Ok(0)
    };
    match args {
        [] => return print(sh, &[]),
        [p, rest @ ..] if p == "-p" => {
            let names: Vec<String> = rest.iter().map(|s| signal_name(s)).collect();
            return print(sh, &names);
        }
        [l] if l == "-l" => {
            sh.out("EXIT HUP INT QUIT TERM\n");
            return Ok(0);
        }
        _ => {}
    }
    let (action, signals) = match args.split_first() {
        // `trap 0 2`: every word a signal, each reset.
        Some((first, _)) if first.parse::<u32>().is_ok() => (None, args.to_vec()),
        Some((first, rest)) if first == "-" => (None, rest.to_vec()),
        Some((first, rest)) => (Some(first.clone()), rest.to_vec()),
        None => return Ok(0),
    };
    for signal in signals {
        let name = signal_name(&signal);
        match name.as_str() {
            "EXIT" | "INT" | "TERM" | "HUP" | "QUIT" => {}
            "ERR" | "DEBUG" | "RETURN" => return Err(sh.unsupported(&format!("`trap … {name}`"))),
            _ => {
                sh.error(format!("trap: {signal}: invalid signal specification"));
                return Ok(1);
            }
        }
        match &action {
            Some(action) => {
                sh.traps.insert(name, action.clone());
            }
            None => {
                sh.traps.remove(&name);
            }
        }
    }
    Ok(0)
}

fn signal_name(signal: &str) -> String {
    let upper = signal.to_uppercase();
    let bare = upper.strip_prefix("SIG").unwrap_or(&upper);
    match bare {
        "0" => "EXIT".into(),
        "1" => "HUP".into(),
        "2" => "INT".into(),
        "3" => "QUIT".into(),
        "15" => "TERM".into(),
        other => other.into(),
    }
}

fn unset(sh: &mut Shell, args: &[String]) -> Exec {
    let mut functions = false;
    let mut status = 0;
    for arg in args {
        match arg.as_str() {
            "-f" => functions = true,
            "-v" => functions = false,
            "-n" => return Err(sh.unsupported("`unset -n` (name references)")),
            name if functions => {
                sh.functions.remove(name);
            }
            name => {
                let result = match name.split_once('[') {
                    Some((base, rest)) if rest.ends_with(']') => {
                        let key = rest.trim_end_matches(']');
                        let key = if matches!(
                            sh.vars.get(base).and_then(|v| v.value.as_ref()),
                            Some(Value::Indexed(_))
                        ) {
                            let i = sh.arith_text(key)?;
                            sh.index_from_end(base, i).to_string()
                        } else {
                            key.to_string()
                        };
                        sh.vars.unset_element(base, &key)
                    }
                    _ => {
                        if sh.vars.get(name).is_none() && sh.functions.contains_key(name) {
                            sh.functions.remove(name);
                            Ok(())
                        } else {
                            sh.vars.unset(name)
                        }
                    }
                };
                if let Err(e) = result {
                    sh.error(format!("unset: {e}"));
                    status = 1;
                }
            }
        }
    }
    Ok(status)
}

/// An argument of a declaration built-in: a word, or an assignment it
/// carries out itself.
pub(crate) enum DeclArg {
    Word(String),
    Assign(ast::Assignment),
}

/// `name=value`, `name+=value` or `name[i]=value` as an assignment of the
/// literal `value`, if the word is one.
pub(crate) fn literal_assignment(word: &str) -> Option<ast::Assignment> {
    let (lhs, value) = word.split_once('=')?;
    let (lhs, append) = match lhs.strip_suffix('+') {
        Some(l) => (l, true),
        None => (lhs, false),
    };
    let name = match lhs.split_once('[') {
        Some((base, index)) if index.ends_with(']') && valid_name(base) => {
            ast::AssignmentName::ArrayElementName(
                base.to_string(),
                index.trim_end_matches(']').to_string(),
            )
        }
        _ if valid_name(lhs) => ast::AssignmentName::VariableName(lhs.to_string()),
        _ => return None,
    };
    Some(ast::Assignment {
        name,
        value: ast::AssignmentValue::Scalar(ast::Word::new(&crate::expand::quote(value))),
        append,
        loc: brush_parser::SourceSpan::default(),
    })
}

fn assignment_name(a: &ast::Assignment) -> &str {
    match &a.name {
        ast::AssignmentName::VariableName(n) | ast::AssignmentName::ArrayElementName(n, _) => n,
    }
}

/// `declare`, `typeset`, `local`, `export` and `readonly`.
pub(crate) fn declaration(sh: &mut Shell, name: &str, args: Vec<DeclArg>) -> Exec {
    let mut export = name == "export";
    let mut readonly = name == "readonly";
    let local =
        name == "local" || ((name == "declare" || name == "typeset") && sh.vars.in_function());
    let mut indexed = false;
    let mut assoc = false;
    let mut integer = false;
    let mut print = false;
    let mut remove_export = false;
    let mut functions = false;
    let mut global = false;
    let mut operands = Vec::new();
    for arg in args {
        match arg {
            DeclArg::Word(word) if word.starts_with('-') || word.starts_with('+') => {
                let on = word.starts_with('-');
                for c in word.chars().skip(1) {
                    match c {
                        'x' => {
                            export = on;
                            remove_export = !on;
                        }
                        'r' => readonly = on,
                        'a' => indexed = on,
                        'A' => assoc = on,
                        'i' => integer = on,
                        'p' => print = true,
                        'f' | 'F' => functions = true,
                        'g' => global = true,
                        'n' if name == "export" => remove_export = true,
                        other => {
                            return Err(sh.unsupported(&format!("`{name} -{other}`")));
                        }
                    }
                }
            }
            DeclArg::Word(word) => match literal_assignment(&word) {
                // `local "$1"` where `$1` is `x=y z`: an assignment of the
                // literal text, not split again.
                Some(a) => operands.push(DeclArg::Assign(a)),
                None => operands.push(DeclArg::Word(word)),
            },
            other @ DeclArg::Assign(_) => operands.push(other),
        }
    }
    if name == "local" && !sh.vars.in_function() {
        sh.error("local: can only be used in a function");
        return Ok(1);
    }
    if functions {
        print_functions(sh);
        return Ok(0);
    }
    if operands.is_empty() || print {
        print_declarations(sh, name, &operands, export, readonly);
        return Ok(0);
    }
    let mut status = 0;
    for operand in operands {
        let var_name = match &operand {
            DeclArg::Word(w) => w.clone(),
            DeclArg::Assign(a) => assignment_name(a).to_string(),
        };
        if !valid_name(&var_name) {
            sh.error(format!("{name}: `{var_name}': not a valid identifier"));
            status = 1;
            continue;
        }
        if local && !global {
            sh.vars.make_local(&var_name);
        }
        {
            let var = sh.vars.declare(&var_name);
            if indexed && var.value.is_none() {
                var.value = Some(Value::Indexed(std::collections::BTreeMap::new()));
            }
            if assoc && !matches!(var.value, Some(Value::Assoc(_))) {
                var.value = Some(Value::Assoc(std::collections::BTreeMap::new()));
            }
            if integer {
                var.integer = true;
            }
        }
        if let DeclArg::Assign(a) = &operand {
            sh.assign(a)?;
        }
        let var = sh.vars.declare(&var_name);
        if export {
            var.exported = true;
        }
        if remove_export {
            var.exported = false;
        }
        if readonly {
            var.readonly = true;
        }
    }
    Ok(status)
}

/// `declare -f`: the name of every function.
fn print_functions(sh: &Shell) {
    let mut out = String::new();
    let mut names: Vec<&String> = sh.functions.keys().collect();
    names.sort();
    for n in names {
        let _ = writeln!(out, "declare -f {n}");
    }
    sh.out(&out);
}

fn print_declarations(
    sh: &Shell,
    builtin: &str,
    operands: &[DeclArg],
    export: bool,
    readonly: bool,
) {
    let wanted: Vec<String> = operands
        .iter()
        .filter_map(|o| match o {
            DeclArg::Word(w) => Some(w.clone()),
            DeclArg::Assign(_) => None,
        })
        .collect();
    let mut out = String::new();
    for (name, var) in sh.vars.all() {
        if !wanted.is_empty() && !wanted.contains(name) {
            continue;
        }
        if wanted.is_empty() && ((export && !var.exported) || (readonly && !var.readonly)) {
            continue;
        }
        let mut flags = String::new();
        match &var.value {
            Some(Value::Indexed(_)) => flags.push('a'),
            Some(Value::Assoc(_)) => flags.push('A'),
            _ => {}
        }
        if var.integer {
            flags.push('i');
        }
        if var.readonly {
            flags.push('r');
        }
        if var.exported {
            flags.push('x');
        }
        let flags = if flags.is_empty() {
            "--".to_string()
        } else {
            format!("-{flags}")
        };
        let prefix = match builtin {
            "export" => "declare -x".to_string(),
            "readonly" => "declare -r".to_string(),
            _ => format!("declare {flags}"),
        };
        let value = match &var.value {
            None => String::new(),
            Some(Value::Scalar(s)) => {
                format!("=\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
            }
            Some(Value::Indexed(map)) => {
                let items: Vec<String> =
                    map.iter().map(|(k, v)| format!("[{k}]=\"{v}\"")).collect();
                format!("=({})", items.join(" "))
            }
            Some(Value::Assoc(map)) => {
                let items: Vec<String> =
                    map.iter().map(|(k, v)| format!("[{k}]=\"{v}\"")).collect();
                format!("=({} )", items.join(" "))
            }
        };
        let _ = writeln!(out, "{prefix} {name}{value}");
    }
    sh.out(&out);
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn echo(sh: &mut Shell, args: &[String]) -> Exec {
    let mut newline = true;
    let mut escapes = false;
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        if first.len() < 2
            || !first.starts_with('-')
            || !first.chars().skip(1).all(|c| "neE".contains(c))
        {
            break;
        }
        for c in first.chars().skip(1) {
            match c {
                'n' => newline = false,
                'e' => escapes = true,
                _ => escapes = false,
            }
        }
        rest = tail;
    }
    let mut text = rest.join(" ");
    let mut bytes = if escapes {
        if let Some(cut) = text.find("\\c") {
            text.truncate(cut);
            newline = false;
        }
        crate::expand::ansi_c_bytes(&text)
    } else {
        text.into_bytes()
    };
    if newline {
        bytes.push(b'\n');
    }
    Ok(i32::from(!sh.out_bytes(&bytes)))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn pwd(sh: &mut Shell, args: &[String]) -> Exec {
    for arg in args {
        if arg != "-L" && arg != "-P" {
            return Ok(usage(sh, format!("pwd: {arg}: invalid option")));
        }
    }
    let text = format!("{}\n", sh.cwd.display());
    Ok(i32::from(!sh.out(&text)))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn cd(sh: &mut Shell, args: &[String]) -> Exec {
    let operands: Vec<&String> = args.iter().filter(|a| *a != "-L" && *a != "-P").collect();
    if operands.len() > 1 {
        sh.error("cd: too many arguments");
        return Ok(1);
    }
    let mut print = false;
    let target = match operands.first() {
        None => {
            let Some(home) = sh.vars.scalar("HOME") else {
                sh.error("cd: HOME not set");
                return Ok(1);
            };
            home
        }
        Some(dir) if dir.as_str() == "-" => {
            let Some(old) = sh.vars.scalar("OLDPWD") else {
                sh.error("cd: OLDPWD not set");
                return Ok(1);
            };
            print = true;
            old
        }
        Some(dir) => (*dir).clone(),
    };
    let path = crate::paths::join(&sh.cwd, &target);
    match sh.host.metadata(&path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            sh.error(format!("cd: {target}: Not a directory"));
            return Ok(1);
        }
        Err(e) => {
            sh.error(format!("cd: {target}: {}", crate::commands::describe(&e)));
            return Ok(1);
        }
    }
    let old = sh.cwd.to_string_lossy().into_owned();
    sh.cwd = path;
    let new = sh.cwd.to_string_lossy().into_owned();
    let _ = sh.vars.set("OLDPWD", old);
    let _ = sh.vars.set("PWD", new.clone());
    if print {
        sh.out(&format!("{new}\n"));
    }
    Ok(0)
}

fn read(sh: &mut Shell, args: &[String]) -> Exec {
    let mut raw = false;
    let mut prompt = None;
    let mut array = None;
    let mut delimiter = b'\n';
    let mut count: Option<usize> = None;
    let mut fd = 0_u32;
    let mut names = Vec::new();
    let mut i = 0_usize;
    while let Some(arg) = args.get(i) {
        i = i.saturating_add(1);
        if !arg.starts_with('-') || arg == "-" || !names.is_empty() {
            names.push(arg.clone());
            continue;
        }
        if arg == "--" {
            names.extend(args.get(i..).unwrap_or(&[]).iter().cloned());
            break;
        }
        let mut chars = arg.chars().skip(1);
        if let Some(c) = chars.next() {
            let value = |chars: &mut std::iter::Skip<std::str::Chars<'_>>, i: &mut usize| {
                let rest: String = chars.collect();
                if rest.is_empty() {
                    let v = args.get(*i).cloned();
                    *i = i.saturating_add(1);
                    v
                } else {
                    Some(rest)
                }
            };
            match c {
                'r' => raw = true,
                'p' => prompt = value(&mut chars, &mut i),
                'a' => array = value(&mut chars, &mut i),
                'd' => {
                    delimiter = value(&mut chars, &mut i)
                        .and_then(|d| d.bytes().next())
                        .unwrap_or(0);
                }
                'n' | 'N' => count = value(&mut chars, &mut i).and_then(|n| n.parse().ok()),
                'u' => {
                    fd = value(&mut chars, &mut i)
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                }
                's' => return Err(sh.unsupported("`read -s`")),
                't' => return Err(sh.unsupported("`read -t`")),
                'e' | 'i' => return Err(sh.unsupported(&format!("`read -{c}`"))),
                other => return Ok(usage(sh, format!("read: -{other}: invalid option"))),
            }
        }
    }
    if let Some(prompt) = prompt
        && let Some(err) = sh.output(2)
    {
        let _ = err.write_str(&prompt);
    }
    let Some(input) = sh.input(fd) else {
        sh.error(format!("read: {fd}: invalid file descriptor"));
        return Ok(1);
    };
    let Some((line, eof)) = read_record(&input, count, delimiter, raw)? else {
        return Ok(1);
    };
    let text = String::from_utf8_lossy(&line).into_owned();
    let text = if raw { text } else { unescape_read(&text) };
    let ifs = sh.ifs();
    if let Some(array) = array {
        let fields = split_read(&text, &ifs, usize::MAX);
        let map = fields
            .into_iter()
            .enumerate()
            .map(|(i, f)| (i64::try_from(i).unwrap_or(0), f))
            .collect();
        if let Err(e) = sh.vars.set_value(&array, Value::Indexed(map)) {
            sh.error(e);
            return Ok(1);
        }
    } else if names.is_empty() {
        let _ = sh.vars.set("REPLY", text);
    } else {
        let fields = split_read(&text, &ifs, names.len());
        for (i, name) in names.iter().enumerate() {
            let value = fields.get(i).cloned().unwrap_or_default();
            if let Err(e) = sh.vars.set(name, value) {
                sh.error(e);
                return Ok(1);
            }
        }
    }
    Ok(i32::from(eof))
}

/// The record `read` reads: up to `count` bytes or the delimiter, joining
/// backslash-newline continuations unless `raw`, and whether the input ended
/// first. `None` when reading failed.
fn read_record(
    input: &crate::io::Input,
    count: Option<usize>,
    delimiter: u8,
    raw: bool,
) -> Result<Option<(Vec<u8>, bool)>, Flow> {
    let mut line: Vec<u8> = Vec::new();
    let mut eof = false;
    loop {
        if let Some(n) = count {
            let mut buf = vec![0_u8; n.saturating_sub(line.len())];
            if buf.is_empty() {
                break;
            }
            match input.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(got) => {
                    let got_bytes = buf.get(..got).unwrap_or(&[]);
                    if let Some(pos) = got_bytes.iter().position(|b| *b == delimiter) {
                        line.extend_from_slice(got_bytes.get(..pos).unwrap_or(&[]));
                        break;
                    }
                    line.extend_from_slice(got_bytes);
                    continue;
                }
                Err(_) => return Ok(None),
            }
        }
        match input.read_line_until(delimiter) {
            Ok(Some(part)) => {
                if !raw && part.last() == Some(&b'\\') {
                    line.extend_from_slice(part.get(..part.len().saturating_sub(1)).unwrap_or(&[]));
                    continue;
                }
                line.extend_from_slice(&part);
                break;
            }
            Ok(None) => {
                eof = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(Flow::Fatal(130)),
            Err(_) => return Ok(None),
        }
    }
    Ok(Some((line, eof)))
}

fn unescape_read(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split a line read by `read` into at most `max` fields: the last takes
/// the rest of the line, less trailing IFS white space.
fn split_read(text: &str, ifs: &str, max: usize) -> Vec<String> {
    let is_white = |c: char| ifs.contains(c) && c.is_whitespace();
    let mut fields = Vec::new();
    let mut rest = text.trim_start_matches(is_white);
    while !rest.is_empty() {
        if fields.len().saturating_add(1) == max {
            fields.push(rest.trim_end_matches(is_white).to_string());
            return fields;
        }
        if let Some(pos) = rest.find(|c: char| ifs.contains(c)) {
            fields.push(rest.get(..pos).unwrap_or("").to_string());
            let after = rest.get(pos..).unwrap_or("");
            let mut chars = after.chars();
            let sep = chars.next().unwrap_or(' ');
            let mut tail = chars.as_str().trim_start_matches(is_white);
            if is_white(sep)
                && let Some(next) = tail.chars().next()
                && ifs.contains(next)
                && !next.is_whitespace()
            {
                tail = tail
                    .get(next.len_utf8()..)
                    .unwrap_or("")
                    .trim_start_matches(is_white);
            }
            rest = tail;
        } else {
            fields.push(rest.to_string());
            break;
        }
    }
    fields
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn type_builtin(sh: &mut Shell, args: &[String]) -> Exec {
    let terse = args.first().is_some_and(|a| a == "-t");
    let mut status = 0;
    for name in args.iter().filter(|a| !a.starts_with('-')) {
        let line = match sh.describe(name) {
            Some("alias") => {
                let v = sh.aliases.get(name).cloned().unwrap_or_default();
                if terse {
                    "alias".to_string()
                } else {
                    format!("{name} is aliased to `{v}'")
                }
            }
            Some("keyword") => {
                if terse {
                    "keyword".into()
                } else {
                    format!("{name} is a shell keyword")
                }
            }
            Some("function") => {
                if terse {
                    "function".into()
                } else {
                    format!("{name} is a function")
                }
            }
            Some("builtin") => {
                if terse {
                    "builtin".into()
                } else {
                    format!("{name} is a shell builtin")
                }
            }
            Some(_) => {
                if terse {
                    "builtin".into()
                } else {
                    format!("{name} is one of Tilo's own commands")
                }
            }
            None => {
                if !terse {
                    sh.error(format!("type: {name}: not found"));
                }
                status = 1;
                continue;
            }
        };
        sh.out(&format!("{line}\n"));
    }
    Ok(status)
}

fn command(sh: &mut Shell, args: &[String]) -> Exec {
    let mut rest = args;
    let mut verbose = None;
    while let Some((first, tail)) = rest.split_first() {
        match first.as_str() {
            "-v" => verbose = Some(false),
            "-V" => verbose = Some(true),
            "-p" => {}
            "--" => {
                rest = tail;
                break;
            }
            _ => break,
        }
        rest = tail;
    }
    if let Some(long) = verbose {
        let mut status = 0;
        for name in rest {
            match sh.describe(name) {
                Some(kind) if long => {
                    sh.out(&format!("{name} is a {kind}\n"));
                }
                Some("alias") => {
                    let v = sh.aliases.get(name).cloned().unwrap_or_default();
                    sh.out(&format!("alias {name}={}\n", crate::expand::quote(&v)));
                }
                Some(_) => {
                    sh.out(&format!("{name}\n"));
                }
                None => status = 1,
            }
        }
        return Ok(status);
    }
    let Some(name) = rest.first() else {
        return Ok(0);
    };
    // Run it skipping functions.
    let saved = std::mem::take(&mut sh.functions);
    let words: Vec<String> = rest.iter().map(|a| crate::expand::quote(a)).collect();
    let result = sh.eval_text(&words.join(" "));
    let added = std::mem::replace(&mut sh.functions, saved);
    sh.functions.extend(added);
    let _ = name;
    result
}

fn builtin(sh: &mut Shell, args: &[String]) -> Exec {
    let Some((name, rest)) = args.split_first() else {
        return Ok(0);
    };
    if let Some(b) = special(name).or_else(|| regular(name)) {
        b(sh, rest)
    } else {
        sh.error(format!("builtin: {name}: not a shell builtin"));
        Ok(1)
    }
}

fn let_builtin(sh: &mut Shell, args: &[String]) -> Exec {
    if args.is_empty() {
        return Ok(usage(sh, "let: expression expected"));
    }
    let mut last = 0;
    for expr in args {
        last = sh.arith_text(expr)?;
    }
    Ok(i32::from(last == 0))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn getopts(sh: &mut Shell, args: &[String]) -> Exec {
    let [spec, name, given @ ..] = args else {
        let message = "getopts: usage: getopts optstring name [arg ...]";
        return Ok(usage(sh, message));
    };
    let params: Vec<String> = if given.is_empty() {
        sh.positional.clone()
    } else {
        given.to_vec()
    };
    let silent = spec.starts_with(':');
    let spec = spec.trim_start_matches(':');
    let mut index: usize = sh
        .vars
        .scalar("OPTIND")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    // Where in a clustered word (`-abc`) the last call stopped.
    let mut offset: usize = sh
        .vars
        .scalar("__USH_OPTPOS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let set = |sh: &mut Shell, var: &str, value: String| {
        let _ = sh.vars.set(var, value);
    };
    let Some(word) = params.get(index.saturating_sub(1)) else {
        set(sh, name, "?".into());
        return Ok(1);
    };
    if word == "--" {
        set(sh, "OPTIND", index.saturating_add(1).to_string());
        set(sh, name, "?".into());
        return Ok(1);
    }
    if !word.starts_with('-') || word == "-" {
        set(sh, name, "?".into());
        return Ok(1);
    }
    let chars: Vec<char> = word.chars().collect();
    let Some(&opt) = chars.get(offset) else {
        set(sh, name, "?".into());
        return Ok(1);
    };
    offset = offset.saturating_add(1);
    let at_end = offset >= chars.len();
    let takes_arg = spec
        .find(opt)
        .and_then(|p| spec.get(p.saturating_add(opt.len_utf8())..))
        .is_some_and(|after| after.starts_with(':'));
    let _ = sh.vars.unset("OPTARG");
    if !spec.contains(opt) || opt == ':' {
        if silent {
            set(sh, "OPTARG", opt.to_string());
        } else {
            sh.error(format!("illegal option -- {opt}"));
        }
        set(sh, name, "?".into());
    } else if takes_arg {
        let arg = if at_end {
            index = index.saturating_add(1);
            params.get(index.saturating_sub(1)).cloned()
        } else {
            let a: String = chars.get(offset..).unwrap_or(&[]).iter().collect();
            Some(a)
        };
        match arg {
            Some(arg) => {
                set(sh, "OPTARG", arg);
                set(sh, name, opt.to_string());
            }
            None => {
                if silent {
                    set(sh, "OPTARG", opt.to_string());
                    set(sh, name, ":".into());
                } else {
                    sh.error(format!("option requires an argument -- {opt}"));
                    set(sh, name, "?".into());
                }
            }
        }
        index = index.saturating_add(1);
        offset = 1;
        set(sh, "OPTIND", index.to_string());
        set(sh, "__USH_OPTPOS", offset.to_string());
        return Ok(0);
    } else {
        set(sh, name, opt.to_string());
    }
    if at_end {
        index = index.saturating_add(1);
        offset = 1;
    }
    set(sh, "OPTIND", index.to_string());
    set(sh, "__USH_OPTPOS", offset.to_string());
    Ok(0)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn alias(sh: &mut Shell, args: &[String]) -> Exec {
    let args: Vec<&String> = args.iter().filter(|a| *a != "-p").collect();
    if args.is_empty() {
        let mut out = String::new();
        for (name, value) in &sh.aliases {
            let _ = writeln!(out, "alias {name}={}", crate::expand::quote(value));
        }
        sh.out(&out);
        return Ok(0);
    }
    let mut status = 0;
    for arg in args {
        match arg.split_once('=') {
            Some((name, value)) => {
                sh.aliases.insert(name.to_string(), value.to_string());
            }
            None => {
                if let Some(value) = sh.aliases.get(arg.as_str()) {
                    let line = format!("alias {arg}={}\n", crate::expand::quote(value));
                    sh.out(&line);
                } else {
                    sh.error(format!("alias: {arg}: not found"));
                    status = 1;
                }
            }
        }
    }
    Ok(status)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn unalias(sh: &mut Shell, args: &[String]) -> Exec {
    let mut status = 0;
    for arg in args {
        if arg == "-a" {
            sh.aliases.clear();
        } else if sh.aliases.remove(arg).is_none() {
            sh.error(format!("unalias: {arg}: not found"));
            status = 1;
        }
    }
    Ok(status)
}

fn shopt(sh: &mut Shell, args: &[String]) -> Exec {
    let mut on = None;
    let mut quiet = false;
    let mut names = Vec::new();
    for arg in args {
        match arg.as_str() {
            "-s" => on = Some(true),
            "-u" => on = Some(false),
            "-q" => quiet = true,
            "-o" | "-p" => {}
            name => names.push(name),
        }
    }
    let mut status = 0;
    for name in names {
        let slot = match name {
            "nullglob" => Some(&mut sh.options.nullglob),
            "dotglob" => Some(&mut sh.options.dotglob),
            "failglob" => Some(&mut sh.options.failglob),
            // What this shell always does, or has nothing to do.
            "expand_aliases"
            | "checkwinsize"
            | "histappend"
            | "cmdhist"
            | "progcomp"
            | "promptvars"
            | "sourcepath"
            | "interactive_comments"
            | "hostcomplete"
            | "complete_fullquote" => None,
            other => {
                if on == Some(true) || on.is_none() {
                    return Err(sh.unsupported(&format!("`shopt {other}`")));
                }
                None
            }
        };
        match (slot, on) {
            (Some(slot), Some(value)) => *slot = value,
            (Some(slot), None) => {
                let value = *slot;
                if !quiet {
                    sh.out(&format!(
                        "{name:<15}\t{}\n",
                        if value { "on" } else { "off" }
                    ));
                }
                if !value {
                    status = 1;
                }
            }
            (None, _) => {}
        }
    }
    Ok(status)
}

fn umask(sh: &mut Shell, args: &[String]) -> Exec {
    if args.is_empty() {
        sh.out("0022\n");
        return Ok(0);
    }
    Err(sh.unsupported("changing the umask"))
}

fn mapfile(sh: &mut Shell, args: &[String]) -> Exec {
    let mut trim = false;
    let mut delimiter = b'\n';
    let mut count: Option<usize> = None;
    let mut skip = 0_usize;
    let mut name = String::from("MAPFILE");
    let mut i = 0_usize;
    while let Some(arg) = args.get(i) {
        i = i.saturating_add(1);
        match arg.as_str() {
            "-t" => trim = true,
            "-d" => {
                delimiter = args.get(i).and_then(|d| d.bytes().next()).unwrap_or(0);
                i = i.saturating_add(1);
            }
            "-n" => {
                count = args.get(i).and_then(|n| n.parse().ok()).filter(|n| *n > 0);
                i = i.saturating_add(1);
            }
            "-s" => {
                skip = args.get(i).and_then(|n| n.parse().ok()).unwrap_or(0);
                i = i.saturating_add(1);
            }
            flag if flag.starts_with('-') => {
                return Err(sh.unsupported(&format!("`mapfile {flag}`")));
            }
            other => name = other.to_string(),
        }
    }
    let input = sh.input(0).unwrap_or_else(crate::io::Input::null);
    let mut map = std::collections::BTreeMap::new();
    let mut seen = 0_usize;
    let mut index = 0_i64;
    while count.is_none_or(|c| map.len() < c) {
        match input.read_line_until(delimiter) {
            Ok(Some(line)) => {
                seen = seen.saturating_add(1);
                if seen <= skip {
                    continue;
                }
                let mut text = String::from_utf8_lossy(&line).into_owned();
                if !trim {
                    text.push(char::from(delimiter));
                }
                map.insert(index, text);
                index = index.saturating_add(1);
            }
            Ok(None) | Err(_) => break,
        }
    }
    if let Err(e) = sh.vars.set_value(&name, Value::Indexed(map)) {
        sh.error(e);
        return Ok(1);
    }
    Ok(0)
}

/// `history [N]`: the person's command history, oldest first and numbered,
/// or its newest `N` lines. It is their own record (`terminal.history`),
/// shared by every session and device; the shell changes nothing in it.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a `Builtin`: every built-in returns `Exec`"
)]
fn history(sh: &mut Shell, args: &[String]) -> Exec {
    let mut newest = None;
    for arg in args {
        match arg.as_str() {
            "--" => {}
            flag if flag.starts_with('-') => {
                sh.error(format!(
                    "history: {flag}: the history is the person's own record, kept by \
                     Terminal; this shell only reads it"
                ));
                return Ok(2);
            }
            n => {
                let Ok(n) = n.parse::<usize>() else {
                    sh.error(format!("history: {n}: numeric argument required"));
                    return Ok(2);
                };
                newest = Some(n);
            }
        }
    }
    let lines = sh.whole_history();
    let total = lines.len();
    let first = total.saturating_sub(newest.unwrap_or(total));
    let mut out = String::new();
    for (i, line) in lines.iter().rev().enumerate().skip(first) {
        let _ = writeln!(out, "{:>5}  {line}", i.saturating_add(1));
    }
    sh.out(&out);
    Ok(0)
}

//! The tier-1 commands that look at files and paths: `ls`, `stat`, `find`,
//! `tree` (each in its own module), and the path commands `basename`,
//! `dirname`, `realpath` and `readlink`.

use std::io;
use std::path::{Component, Path, PathBuf};

use super::util::{Arg, Spec, parse, value};
use super::{Command, Ctx};
use crate::host::Kind;

pub const NAMES: &[&str] = &[
    "basename", "dirname", "find", "ls", "readlink", "realpath", "stat", "tree",
];

pub fn lookup(name: &str) -> Option<Command> {
    Some(match name {
        "ls" => super::ls::ls,
        "stat" => super::stat::stat,
        "find" => super::find::find,
        "tree" => super::tree::tree,
        "basename" => basename,
        "dirname" => dirname,
        "realpath" => realpath,
        "readlink" => readlink,
        _ => return None,
    })
}

/// `basename NAME [SUFFIX]`, `basename -a NAME...`, `-s SUFFIX`, `-z`.
fn basename(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "s",
        &[("multiple", false), ("suffix", true), ("zero", false)],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut multiple, mut suffix, mut end) = (false, None, "\n");
    let mut names = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(o) => names.push(o),
            Arg::Flag(f, v) => match f.as_str() {
                "-a" | "--multiple" => multiple = true,
                "-s" | "--suffix" => {
                    multiple = true;
                    suffix = Some(value(v.as_ref()).to_string());
                }
                "-z" | "--zero" => end = "\0",
                other => return ctx.unsupported(other),
            },
        }
    }
    if names.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    if !multiple {
        if names.len() > 2 {
            ctx.error(format!(
                "extra operand '{}'",
                names.get(2).map_or("", String::as_str)
            ));
            return 2;
        }
        suffix = names.get(1).cloned();
        names.truncate(1);
    }
    for name in &names {
        let mut base = base_name(name);
        if let Some(s) = suffix.as_deref()
            && !s.is_empty()
            && base.len() > s.len()
            && let Some(stripped) = base.strip_suffix(s)
        {
            base = stripped.to_string();
        }
        if let Err(stop) = ctx.out_str(&format!("{base}{end}")) {
            return stop.0;
        }
    }
    0
}

/// The last component of `name` as `basename` takes it: trailing slashes
/// ignored, `/` for the root.
pub fn base_name(name: &str) -> String {
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        return if name.is_empty() {
            String::new()
        } else {
            "/".into()
        };
    }
    trimmed
        .rsplit_once('/')
        .map_or(trimmed, |(_, last)| last)
        .to_string()
}

/// The directory part of `name` as `dirname` takes it.
pub fn dir_name(name: &str) -> String {
    let trimmed = name.trim_end_matches('/');
    if trimmed.is_empty() {
        return if name.starts_with('/') { "/" } else { "." }.into();
    }
    match trimmed.rsplit_once('/') {
        None => ".".into(),
        Some((dir, _)) => {
            let dir = dir.trim_end_matches('/');
            if dir.is_empty() { "/" } else { dir }.into()
        }
    }
}

fn dirname(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new("", &[("zero", false)]);
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut end = "\n";
    let mut names = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(o) => names.push(o),
            Arg::Flag(f, _) if f == "-z" || f == "--zero" => end = "\0",
            Arg::Flag(f, _) => return ctx.unsupported(&f),
        }
    }
    if names.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    for name in &names {
        if let Err(stop) = ctx.out_str(&format!("{}{end}", dir_name(name))) {
            return stop.0;
        }
    }
    0
}

/// How much of a path must exist for [`canonicalize`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Must {
    /// Every component (`-e`).
    All,
    /// All but the last (`readlink -f`, `realpath`'s default).
    AllButLast,
    /// None of it (`-m`).
    Nothing,
}

/// `path` absolute, with every symlink followed and `.` and `..` resolved,
/// asking the host about each component.
pub fn canonicalize(ctx: &Ctx<'_>, path: &str, must: Must) -> io::Result<PathBuf> {
    let mut pending: Vec<String> = Vec::new();
    push_components(&mut pending, &ctx.resolve(path));
    let mut out = PathBuf::from("/");
    let mut links = 0_u32;
    while let Some(part) = pending.pop() {
        if part == ".." {
            out.pop();
            continue;
        }
        let candidate = out.join(&part);
        let last = pending.is_empty();
        match ctx.host.symlink_metadata(&candidate) {
            Ok(meta) if meta.kind == Kind::Symlink => {
                links = links.saturating_add(1);
                if links > 40 {
                    return Err(io::Error::other("Too many levels of symbolic links"));
                }
                let target = ctx.host.read_link(&candidate)?;
                if target.is_absolute() {
                    out = PathBuf::from("/");
                }
                push_components(&mut pending, &target);
            }
            Ok(meta) => {
                if !last && !meta.is_dir() && must != Must::Nothing {
                    return Err(io::ErrorKind::NotADirectory.into());
                }
                out = candidate;
            }
            Err(e) => {
                let allowed = match must {
                    Must::All => false,
                    Must::AllButLast => last,
                    Must::Nothing => true,
                };
                if !allowed {
                    return Err(e);
                }
                out = candidate;
            }
        }
    }
    Ok(out)
}

/// Push `path`'s components onto `stack` so the first pops first.
fn push_components(stack: &mut Vec<String>, path: &Path) {
    let parts = path.components().filter_map(|c| match c {
        Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
        Component::ParentDir => Some("..".into()),
        _ => None,
    });
    let start = stack.len();
    stack.extend(parts);
    if let Some(added) = stack.get_mut(start..) {
        added.reverse();
    }
}

fn realpath(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("canonicalize-existing", false),
            ("canonicalize-missing", false),
            ("no-symlinks", false),
            ("strip", false),
            ("quiet", false),
            ("zero", false),
            ("relative-to", true),
            ("relative-base", true),
            ("logical", false),
            ("physical", false),
        ],
    );
    resolve_paths(ctx, args, &spec, Must::AllButLast, true)
}

fn readlink(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "",
        &[
            ("canonicalize", false),
            ("canonicalize-existing", false),
            ("canonicalize-missing", false),
            ("no-newline", false),
            ("quiet", false),
            ("silent", false),
            ("verbose", false),
            ("zero", false),
        ],
    );
    resolve_paths(ctx, args, &spec, Must::AllButLast, false)
}

/// `realpath` and `readlink`: the same resolution, different defaults.
/// `readlink` without `-f`/`-e`/`-m` prints a symlink's target as stored.
fn resolve_paths(
    ctx: &Ctx<'_>,
    args: &[String],
    spec: &Spec<'_>,
    mut must: Must,
    real: bool,
) -> i32 {
    let parsed = match parse(ctx, args, spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (mut canon, mut follow, mut quiet, mut newline) = (real, true, !real, true);
    let mut end = "\n";
    let mut names = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(o) => names.push(o),
            Arg::Flag(f, _) => match (f.as_str(), real) {
                ("-f" | "--canonicalize", false) => {
                    canon = true;
                    must = Must::AllButLast;
                }
                ("-e" | "--canonicalize-existing", _) => {
                    canon = true;
                    must = Must::All;
                }
                ("-m" | "--canonicalize-missing", _) => {
                    canon = true;
                    must = Must::Nothing;
                }
                ("-s" | "--no-symlinks" | "--strip", true) => follow = false,
                ("-q" | "--quiet", _) | ("--silent", false) => quiet = true,
                ("-v" | "--verbose", false) => quiet = false,
                ("-n" | "--no-newline", false) => newline = false,
                ("-z" | "--zero", _) => end = "\0",
                ("-P" | "--physical", true) => follow = true,
                (other, _) => return ctx.unsupported(other),
            },
        }
    }
    if names.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    if !newline && names.len() == 1 {
        end = "";
    }
    let mut status = 0;
    for name in &names {
        let answer = if !canon {
            ctx.host
                .read_link(&ctx.resolve(name))
                .map(|t| t.to_string_lossy().into_owned())
        } else if follow {
            canonicalize(ctx, name, must).map(|p| p.to_string_lossy().into_owned())
        } else {
            Ok(ctx.resolve(name).to_string_lossy().into_owned())
        };
        match answer {
            Ok(text) => {
                if let Err(stop) = ctx.out_str(&format!("{text}{end}")) {
                    return stop.0;
                }
            }
            Err(e) => {
                status = 1;
                if !quiet {
                    ctx.io_error(name, &e);
                }
            }
        }
    }
    status
}

//! `tree`: a directory's contents as an indented tree, in the layout of the
//! `tree` program, with its summary line.

use std::path::Path;

use super::util::{Arg, Spec, parse, value};
use super::{Ctx, Stop};
use crate::host::Kind;

struct Opts {
    all: bool,
    dirs_only: bool,
    levels: Option<usize>,
    report: bool,
}

#[derive(Default)]
struct Counts {
    dirs: u64,
    files: u64,
}

pub fn tree(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new("L", &[("noreport", false)]);
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut o = Opts {
        all: false,
        dirs_only: false,
        levels: None,
        report: true,
    };
    let mut roots = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(op) => roots.push(op),
            Arg::Flag(f, v) => match f.as_str() {
                "-a" => o.all = true,
                "-d" => o.dirs_only = true,
                "-L" => match value(v.as_ref()).parse::<usize>() {
                    Ok(n) if n > 0 => o.levels = Some(n),
                    _ => {
                        ctx.error("Invalid level, must be greater than 0.");
                        return 2;
                    }
                },
                "--noreport" => o.report = false,
                other => return ctx.unsupported(other),
            },
        }
    }
    if roots.is_empty() {
        roots.push(".".into());
    }
    let mut counts = Counts::default();
    let mut status = 0;
    let result = (|| -> Result<(), Stop> {
        for root in &roots {
            let path = ctx.resolve(root);
            if ctx.host.metadata(&path).is_ok_and(|m| m.is_dir()) {
                ctx.out_str(&format!("{root}\n"))?;
                walk(ctx, &o, &path, "", 1, &mut counts)?;
            } else {
                ctx.out_str(&format!("{root}  [error opening dir]\n"))?;
                status = 2;
            }
        }
        if o.report {
            let dirs = plural(counts.dirs, "directory", "directories");
            if o.dirs_only {
                ctx.out_str(&format!("\n{dirs}\n"))?;
            } else {
                let files = plural(counts.files, "file", "files");
                ctx.out_str(&format!("\n{dirs}, {files}\n"))?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => status,
        Err(Stop(code)) => code,
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn walk(
    ctx: &Ctx<'_>,
    o: &Opts,
    dir: &Path,
    prefix: &str,
    level: usize,
    counts: &mut Counts,
) -> Result<(), Stop> {
    let Ok(mut entries) = ctx.host.read_dir(dir) else {
        return Ok(());
    };
    entries.retain(|e| o.all || !e.name.starts_with('.'));
    entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let shown: Vec<_> = entries
        .into_iter()
        .filter_map(|e| {
            let path = dir.join(&e.name);
            let meta = ctx.host.symlink_metadata(&path).ok()?;
            let target = (meta.kind == Kind::Symlink)
                .then(|| ctx.host.read_link(&path).ok())
                .flatten();
            let is_dir = match meta.kind {
                Kind::Dir => true,
                Kind::Symlink => ctx.host.metadata(&path).is_ok_and(|m| m.is_dir()),
                _ => false,
            };
            (!o.dirs_only || is_dir).then_some((e.name, path, meta.kind, target, is_dir))
        })
        .collect();
    let count = shown.len();
    for (i, (name, path, kind, target, is_dir)) in shown.into_iter().enumerate() {
        let last = i.saturating_add(1) == count;
        let branch = if last { "└── " } else { "├── " };
        let label = match &target {
            Some(t) => format!("{name} -> {}", t.display()),
            None => name,
        };
        ctx.out_str(&format!("{prefix}{branch}{label}\n"))?;
        if is_dir {
            counts.dirs = counts.dirs.saturating_add(1);
        } else {
            counts.files = counts.files.saturating_add(1);
        }
        let deeper = o.levels.is_none_or(|l| level < l);
        if kind == Kind::Dir && deeper {
            let inner = format!("{prefix}{}", if last { "    " } else { "│   " });
            walk(ctx, o, &path, &inner, level.saturating_add(1), counts)?;
        }
    }
    Ok(())
}

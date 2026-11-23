//! `stat`: what the host knows about a path, in GNU's default layout or a
//! `-c`/`--printf` format.
//!
//! Hosts report one time, the last modification, so access and change
//! times print it too, and the birth time is unknown (`-`).

use super::ls::{blocks, group, mode_string, user};
use super::util::{Arg, Spec, full_time, parse, value};
use super::{Ctx, describe};
use crate::host::{Kind, Metadata};

pub fn stat(ctx: &Ctx<'_>, args: &[String]) -> i32 {
    let spec = Spec::new(
        "c",
        &[
            ("format", true),
            ("printf", true),
            ("dereference", false),
            ("terse", false),
            ("file-system", false),
        ],
    );
    let parsed = match parse(ctx, args, &spec) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let mut format: Option<(String, bool)> = None;
    let mut follow = false;
    let mut names = Vec::new();
    for arg in parsed {
        match arg {
            Arg::Operand(o) => names.push(o),
            Arg::Flag(f, v) => match f.as_str() {
                "-c" | "--format" => format = Some((value(v.as_ref()).to_string(), false)),
                "--printf" => format = Some((value(v.as_ref()).to_string(), true)),
                "-L" | "--dereference" => follow = true,
                other => return ctx.unsupported(other),
            },
        }
    }
    if names.is_empty() {
        ctx.error("missing operand");
        return 2;
    }
    let mut status = 0;
    for name in &names {
        let path = ctx.resolve(name);
        let meta = if follow {
            ctx.host.metadata(&path)
        } else {
            ctx.host.symlink_metadata(&path)
        };
        let meta = match meta {
            Ok(m) => m,
            Err(e) => {
                ctx.error(format!("cannot statx '{name}': {}", describe(&e)));
                status = 1;
                continue;
            }
        };
        let target = (meta.kind == Kind::Symlink)
            .then(|| ctx.host.read_link(&path).ok())
            .flatten()
            .map(|t| t.to_string_lossy().into_owned());
        let file = File {
            name,
            meta: &meta,
            target: target.as_deref(),
        };
        let text = match &format {
            Some((f, true)) => expand(ctx, &file, &escapes(f)),
            Some((f, false)) => format!("{}\n", expand(ctx, &file, f)),
            None => default(ctx, &file),
        };
        if let Err(stop) = ctx.out_str(&text) {
            return stop.0;
        }
    }
    status
}

struct File<'a> {
    name: &'a str,
    meta: &'a Metadata,
    target: Option<&'a str>,
}

fn kind_name(meta: &Metadata) -> &'static str {
    match meta.kind {
        Kind::File if meta.len == 0 => "regular empty file",
        Kind::File => "regular file",
        Kind::Dir => "directory",
        Kind::Symlink => "symbolic link",
        Kind::Other => "unknown",
    }
}

/// A device number's major and minor halves, as glibc splits them.
fn major_minor(dev: u64) -> (u64, u64) {
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
    (major, minor)
}

fn default(ctx: &Ctx<'_>, f: &File<'_>) -> String {
    let m = f.meta;
    let shown = match f.target {
        Some(t) => format!("{} -> {t}", f.name),
        None => f.name.to_string(),
    };
    let (major, minor) = major_minor(m.dev);
    let time = full_time(ctx, m.modified);
    format!(
        "  File: {shown}\n  Size: {:<10}\tBlocks: {:<10} IO Block: {:<6} {}\n\
         Device: {major},{minor}\tInode: {:<11} Links: {}\n\
         Access: ({:04o}/{})  Uid: ({:>5}/{:>8})   Gid: ({:>5}/{:>8})\n\
         Access: {time}\nModify: {time}\nChange: {time}\n Birth: -\n",
        m.len,
        blocks(m),
        4096,
        kind_name(m),
        m.ino,
        m.nlink,
        m.mode & 0o7777,
        mode_string(m.kind, m.mode),
        m.uid,
        user(ctx, m.uid),
        m.gid,
        group(ctx, m.gid),
    )
}

/// `--printf`'s backslash escapes.
fn escapes(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('v') => out.push('\x0b'),
            Some(c @ ('\\' | '"')) => out.push(c),
            Some(d @ '0'..='7') => {
                let mut code = d.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(next) => {
                            code = code.saturating_mul(8).saturating_add(next);
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(code).unwrap_or('?'));
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Expand `%` directives, with GNU's `-` and `0` flags and widths.
fn expand(ctx: &Ctx<'_>, f: &File<'_>, format: &str) -> String {
    let m = f.meta;
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut left = false;
        let mut zero = false;
        while let Some(&flag) = chars.peek() {
            match flag {
                '-' => left = true,
                '0' => zero = true,
                '#' | '+' | ' ' | '\'' => {}
                _ => break,
            }
            chars.next();
        }
        let mut width = 0_usize;
        while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
            width = width.saturating_mul(10).saturating_add(d as usize);
            chars.next();
        }
        let Some(directive) = chars.next() else {
            out.push('%');
            break;
        };
        let time = || full_time(ctx, m.modified);
        let text = match directive {
            '%' => "%".to_string(),
            'n' => f.name.to_string(),
            'N' => match f.target {
                Some(t) => format!("'{}' -> '{t}'", f.name),
                None => format!("'{}'", f.name),
            },
            's' => m.len.to_string(),
            'b' => blocks(m).to_string(),
            'B' => "512".into(),
            'o' => "4096".into(),
            'F' => kind_name(m).into(),
            'a' => format!("{:o}", m.mode & 0o7777),
            'A' => mode_string(m.kind, m.mode),
            'f' => format!("{:x}", m.mode | kind_bits(m.kind)),
            'U' => user(ctx, m.uid),
            'G' => group(ctx, m.gid),
            'u' => m.uid.to_string(),
            'g' => m.gid.to_string(),
            'Y' | 'X' | 'Z' => m.modified.to_string(),
            'y' | 'x' | 'z' => time(),
            'W' => "0".into(),
            'w' => "-".into(),
            'i' => m.ino.to_string(),
            'h' => m.nlink.to_string(),
            'd' => m.dev.to_string(),
            'D' => format!("{:x}", m.dev),
            other => format!("?{other}"),
        };
        let len = text.chars().count();
        let pad = width.saturating_sub(len);
        if left {
            out.push_str(&text);
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            let fill = if zero && text.chars().all(|c| c.is_ascii_digit()) {
                '0'
            } else {
                ' '
            };
            out.extend(std::iter::repeat_n(fill, pad));
            out.push_str(&text);
        }
    }
    out
}

/// The file-type bits of a raw mode, for `%f`.
fn kind_bits(kind: Kind) -> u32 {
    match kind {
        Kind::File => 0o100_000,
        Kind::Dir => 0o040_000,
        Kind::Symlink => 0o120_000,
        Kind::Other => 0,
    }
}

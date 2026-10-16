//! Apply one caller's staged plan of changes to the read-only ESP.
//!
//! ```text
//! tilo-esp-write CALLER
//! ```
//!
//! /boot (the ESP, or the separate /boot beside it) stays mounted read-only.
//! The units that maintain it decide what to change without privileges and
//! stage a plan; this helper, started as `tilo-esp-write@CALLER.service`, is
//! the only step that writes. It remounts the ESP read-write inside its own
//! private mount namespace (`PrivateMounts=yes`), so the global mount never
//! becomes writable.
//!
//! A plan is read only from the caller's own directory,
//! `/var/lib/tilo/esp-plan/CALLER/plan.json` (root-owned, mode 0700, a
//! `StateDirectory=` of the caller's unit). It lists operations relative to
//! the ESP; each caller may use only the operations and paths it needs:
//!
//! ```json
//! {"ops": [
//!   {"op": "mkdir",   "path": "EFI/tilo"},
//!   {"op": "rename",  "src": "loader/entries/x.conf", "dst": "loader/entries/x+3.conf"},
//!   {"op": "install", "file": "repair.efi", "sha256": "...", "dst": "EFI/Linux/tilo-repair.efi"},
//!   {"op": "remove",  "path": "EFI/tilo/repair.efi"}
//! ]}
//! ```
//!
//! Every path is walked one component at a time with `O_NOFOLLOW`, so a
//! symlink anywhere on the ESP is refused rather than followed. An install
//! opens its staged file `O_NOFOLLOW`, checks its SHA-256 against the plan,
//! writes it to a temporary name beside its destination, fsyncs it, renames
//! it into place and fsyncs the directory: each operation is atomic, and an
//! interruption between operations leaves every file either old or new. The
//! plan and staged files are removed once applied, or once refused.
//!
//! Exit status: 0 applied, 1 refused or failed, 2 usage. The test hook
//! `TILO_ESP_WRITE_CRASH_AFTER=N` stops with 99 before operation N, leaving
//! the plan in place as an interruption would.

use std::ffi::{CString, OsStr, OsString};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use regex::Regex;
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, SeekFrom, StatVfsMountFlags};
use rustix::io::Errno;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const DEFAULT_PLAN_ROOT: &str = "/var/lib/tilo/esp-plan";
const TMP_PREFIX: &str = ".tilo-esp-write.";
const CHUNK: usize = 1 << 20;

/// What one caller may do. Patterns match whole ESP-relative paths.
struct Policy {
    caller: &'static str,
    mkdir: &'static [&'static str],
    rename: &'static [(&'static str, &'static str)],
    install: &'static [&'static str],
    remove: &'static [&'static str],
    /// Installs may only replace a file that is already there: a signer
    /// re-signs artefacts and must never add a boot menu entry.
    replace_only: bool,
}

const POLICIES: &[Policy] = &[
    Policy {
        caller: "boot-entries",
        mkdir: &[r"EFI/tilo", r"EFI/Linux"],
        // Boot counters: a bootc entry gains, or keeps counting down, its +N tries.
        rename: &[
            (
                r"loader/entries/bootc[A-Za-z0-9._-]*(\+[0-9]+(-[0-9]+)?)?\.conf",
                r"loader/entries/bootc[A-Za-z0-9._-]*\+[0-9]+(-[0-9]+)?\.conf",
            ),
            (
                r"loader/entries\.staged/bootc[A-Za-z0-9._-]*(\+[0-9]+(-[0-9]+)?)?\.conf",
                r"loader/entries\.staged/bootc[A-Za-z0-9._-]*\+[0-9]+(-[0-9]+)?\.conf",
            ),
        ],
        install: &[r"EFI/Linux/tilo-repair\.efi", r"EFI/tilo/repair\.source"],
        remove: &[
            r"EFI/Linux/tilo-repair\.efi",
            r"loader/entries/tilo-repair\.conf",
            r"EFI/tilo/repair\.efi",
        ],
        replace_only: false,
    },
    // Signing replaces the managed boot artefacts with signed copies, and
    // installs nothing else (tilo-secureboot's artefacts()).
    Policy {
        caller: "secureboot-sign",
        mkdir: &[],
        rename: &[],
        install: &[
            r"EFI/systemd/systemd-boot(x64|aa64|ia32)\.efi",
            r"EFI/BOOT/BOOT(X64|AA64)\.EFI",
            r"EFI/Linux/[A-Za-z0-9][A-Za-z0-9._+-]*\.efi",
            r"EFI/Linux/bootc/[A-Za-z0-9][A-Za-z0-9._+-]*\.efi",
        ],
        remove: &[],
        replace_only: true,
    },
];

enum Failure {
    /// The plan asked for something its caller may not do.
    Refused(String),
    /// A system call failed.
    Os { what: String, errno: Errno },
    /// Anything else: a malformed plan, a failed remount.
    Other(String),
    /// The test hook's interruption between operations.
    Interrupted,
}

impl Failure {
    fn is_not_found(&self) -> bool {
        matches!(self, Self::Os { errno, .. } if *errno == Errno::NOENT)
    }
}

type Result<T> = std::result::Result<T, Failure>;

fn os(what: impl Into<String>) -> impl FnOnce(Errno) -> Failure {
    let what = what.into();
    move |errno| Failure::Os { what, errno }
}

/// The C library's message for `errno`, as Python's `OSError.strerror`.
fn strerror(errno: Errno) -> String {
    let text = io::Error::from(errno).to_string();
    match text.rfind(" (os error ") {
        Some(end) => text.get(..end).unwrap_or(&text).to_owned(),
        None => text,
    }
}

/// A string quoted the way Python's `repr` quotes it, for messages.
fn repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// One operation as Python's `json.dumps(op, sort_keys=True)` prints it, so
/// the journal reads as it always has.
fn py_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            let _ = write!(out, "{n}");
        }
        Value::String(s) => py_json_str(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_str(key, out);
                out.push_str(": ");
                if let Some(item) = map.get(key) {
                    py_json(item, out);
                }
            }
            out.push('}');
        }
    }
}

fn py_json_str(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn full_match(pattern: &str, text: &str) -> Result<bool> {
    let re = Regex::new(&format!("^(?:{pattern})$"))
        .map_err(|e| Failure::Other(format!("bad policy pattern {pattern}: {e}")))?;
    Ok(re.is_match(text))
}

fn allowed_one(caller: &str, op: &str, rules: &[&str], path: &str) -> Result<()> {
    for rule in rules {
        if full_match(rule, path)? {
            return Ok(());
        }
    }
    Err(Failure::Refused(format!("{caller} may not {op} {path}")))
}

fn allowed_pair(policy: &Policy, src: &str, dst: &str) -> Result<()> {
    for (from, to) in policy.rename {
        if full_match(from, src)? && full_match(to, dst)? {
            return Ok(());
        }
    }
    Err(Failure::Refused(format!(
        "{} may not rename {src} -> {dst}",
        policy.caller
    )))
}

/// `[A-Za-z0-9][A-Za-z0-9._+-]*`: no `.`, `..`, empty or hidden component.
fn is_name(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn components(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').collect();
    if path.is_empty() || path.starts_with('/') || !parts.iter().all(|p| is_name(p)) {
        return Err(Failure::Refused(format!(
            "not a plain ESP-relative path: {}",
            repr(path)
        )));
    }
    Ok(parts)
}

/// The parent components and the final name of a path from `components`.
fn split_parent<'a>(parts: &'a [&'a str]) -> Result<(&'a [&'a str], &'a str)> {
    match parts.split_last() {
        Some((last, parent)) => Ok((parent, last)),
        None => Err(Failure::Other("an empty path".to_owned())),
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// The directory holding the last component, never following a symlink.
fn open_dir(root: BorrowedFd<'_>, parts: &[&str], create: bool) -> Result<OwnedFd> {
    let mut fd = rustix::io::fcntl_dupfd_cloexec(root, 0).map_err(os("dup the ESP root"))?;
    for part in parts {
        let next = match rustix::fs::openat(&fd, *part, dir_flags(), Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOENT) if create => {
                rustix::fs::mkdirat(&fd, *part, Mode::from_raw_mode(0o755))
                    .map_err(os(format!("mkdir {part}")))?;
                rustix::fs::fsync(&fd).map_err(os("fsync"))?;
                rustix::fs::openat(&fd, *part, dir_flags(), Mode::empty())
                    .map_err(os(format!("open {part}")))?
            }
            Err(Errno::NOENT) => {
                return Err(Failure::Os {
                    what: format!("open {part}"),
                    errno: Errno::NOENT,
                });
            }
            Err(e) => {
                return Err(Failure::Refused(format!(
                    "{} is not a directory, or is a symlink: {}",
                    repr(part),
                    strerror(e)
                )));
            }
        };
        fd = next;
    }
    Ok(fd)
}

/// Whether `name` exists in `dir`; a symlink there is refused.
fn refuse_symlink<N: rustix::path::Arg + Copy>(
    dir: BorrowedFd<'_>,
    name: N,
    shown: &str,
) -> Result<bool> {
    match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => Ok(false),
        Err(errno) => Err(Failure::Os {
            what: format!("stat {shown}"),
            errno,
        }),
        Ok(st) if FileType::from_raw_mode(st.st_mode).is_symlink() => Err(Failure::Refused(
            format!("{} on the ESP is a symlink", repr(shown)),
        )),
        Ok(_) => Ok(true),
    }
}

fn staged_file(plan_dir: BorrowedFd<'_>, name: &str, want: &Value) -> Result<File> {
    if !is_name(name) {
        return Err(Failure::Refused(format!(
            "not a staged file name: {}",
            repr(name)
        )));
    }
    let fd = rustix::fs::openat(
        plan_dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(os(format!("open staged {name}")))?;
    let st = rustix::fs::fstat(&fd).map_err(os(format!("stat staged {name}")))?;
    if !FileType::from_raw_mode(st.st_mode).is_file() {
        return Err(Failure::Refused(format!(
            "staged {} is not a regular file",
            repr(name)
        )));
    }
    let mut file = File::from(fd);
    let mut digest = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| io_failure(&format!("read staged {name}"), &e))?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
            break;
        };
        digest.update(chunk);
    }
    if want.as_str() != Some(hex::encode(digest.finalize()).as_str()) {
        return Err(Failure::Refused(format!(
            "staged {} does not match its SHA-256",
            repr(name)
        )));
    }
    rustix::fs::seek(&file, SeekFrom::Start(0)).map_err(os(format!("seek staged {name}")))?;
    Ok(file)
}

fn io_failure(what: &str, e: &io::Error) -> Failure {
    match e.raw_os_error() {
        Some(raw) => Failure::Os {
            what: what.to_owned(),
            errno: Errno::from_raw_os_error(raw),
        },
        None => Failure::Other(format!("{what}: {e}")),
    }
}

fn field<'a>(op: &'a Map<String, Value>, key: &str) -> Result<&'a Value> {
    op.get(key)
        .ok_or_else(|| Failure::Other(format!("operation has no {}", repr(key))))
}

fn str_field<'a>(op: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    field(op, key)?
        .as_str()
        .ok_or_else(|| Failure::Other(format!("operation's {} is not a string", repr(key))))
}

/// Names in `dir`, without `.` and `..`.
fn list_dir(dir: BorrowedFd<'_>) -> Result<Vec<CString>> {
    let entries = Dir::read_from(dir).map_err(os("list a directory"))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(os("list a directory"))?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

fn apply(
    policy: &Policy,
    op: &Value,
    root: BorrowedFd<'_>,
    plan_dir: BorrowedFd<'_>,
) -> Result<()> {
    let Some(op) = op.as_object() else {
        return Err(Failure::Other("an operation is not an object".to_owned()));
    };
    let caller = policy.caller;
    match op.get("op").and_then(Value::as_str) {
        Some("mkdir") => {
            let path = str_field(op, "path")?;
            allowed_one(caller, "mkdir", policy.mkdir, path)?;
            open_dir(root, &components(path)?, true).map(drop)
        }
        Some("rename") => {
            let (src_path, dst_path) = (str_field(op, "src")?, str_field(op, "dst")?);
            allowed_pair(policy, src_path, dst_path)?;
            rename(root, src_path, dst_path)
        }
        Some("install") => {
            let dst_path = str_field(op, "dst")?;
            allowed_one(caller, "install", policy.install, dst_path)?;
            let dst = components(dst_path)?;
            let staged = staged_file(plan_dir, str_field(op, "file")?, field(op, "sha256")?)?;
            install(policy, root, staged, &dst, dst_path)
        }
        Some("remove") => {
            let path = str_field(op, "path")?;
            allowed_one(caller, "remove", policy.remove, path)?;
            let parts = components(path)?;
            let (parent, name) = split_parent(&parts)?;
            let dir = match open_dir(root, parent, false) {
                Ok(dir) => dir,
                Err(e) if e.is_not_found() => return Ok(()),
                Err(e) => return Err(e),
            };
            if refuse_symlink(dir.as_fd(), name, name)? {
                rustix::fs::unlinkat(&dir, name, AtFlags::empty())
                    .map_err(os(format!("remove {path}")))?;
                rustix::fs::fsync(&dir).map_err(os("fsync"))?;
            }
            Ok(())
        }
        _ => {
            let kind = match op.get("op") {
                Some(Value::String(s)) => repr(s),
                None | Some(Value::Null) => "None".to_owned(),
                Some(other) => other.to_string(),
            };
            Err(Failure::Refused(format!("unknown operation {kind}")))
        }
    }
}

fn rename(root: BorrowedFd<'_>, src_path: &str, dst_path: &str) -> Result<()> {
    let (src, dst) = (components(src_path)?, components(dst_path)?);
    let (src_parent, src_name) = split_parent(&src)?;
    let (dst_parent, dst_name) = split_parent(&dst)?;
    if src_parent != dst_parent {
        return Err(Failure::Refused(
            "a rename stays inside its directory".to_owned(),
        ));
    }
    let dir = open_dir(root, src_parent, false)?;
    if !refuse_symlink(dir.as_fd(), src_name, src_name)? {
        return Err(Failure::Refused(format!("no {src_path} to rename")));
    }
    refuse_symlink(dir.as_fd(), dst_name, dst_name)?;
    rustix::fs::renameat(&dir, src_name, &dir, dst_name)
        .map_err(os(format!("rename {src_path}")))?;
    rustix::fs::fsync(&dir).map_err(os("fsync"))
}

fn install(
    policy: &Policy,
    root: BorrowedFd<'_>,
    staged: File,
    dst: &[&str],
    dst_path: &str,
) -> Result<()> {
    let (parent, name) = split_parent(dst)?;
    let dir = open_dir(root, parent, false)?;
    // No .efi suffix: an interrupted write in EFI/Linux must not become a
    // boot menu entry. Leftovers of an earlier interruption go first.
    let tmp = format!("{TMP_PREFIX}{}.tmp", std::process::id());
    let written = write_beside(policy, dir.as_fd(), staged, (name, dst_path), &tmp);
    if written.is_err() {
        match rustix::fs::unlinkat(&dir, tmp.as_str(), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(errno) => {
                return Err(Failure::Os {
                    what: format!("remove {tmp}"),
                    errno,
                });
            }
        }
    }
    written
}

fn write_beside(
    policy: &Policy,
    dir: BorrowedFd<'_>,
    mut staged: File,
    (name, dst_path): (&str, &str),
    tmp: &str,
) -> Result<()> {
    for leftover in list_dir(dir)? {
        if leftover.to_bytes().starts_with(TMP_PREFIX.as_bytes()) {
            let shown = leftover.to_string_lossy().into_owned();
            if refuse_symlink(dir, leftover.as_c_str(), &shown)? {
                rustix::fs::unlinkat(dir, leftover.as_c_str(), AtFlags::empty())
                    .map_err(os(format!("remove {shown}")))?;
            }
        }
    }
    if !refuse_symlink(dir, name, name)? && policy.replace_only {
        return Err(Failure::Refused(format!(
            "{} may only replace {dst_path}, which does not exist",
            policy.caller
        )));
    }
    let out = rustix::fs::openat(
        dir,
        tmp,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(os(format!("create {tmp}")))?;
    let mut out = File::from(out);
    // Plain reads and writes, as the unit's system call filter expects; no
    // copy offload between the staging filesystem and the ESP.
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = staged
            .read(&mut buf)
            .map_err(|e| io_failure("read the staged file", &e))?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
            break;
        };
        out.write_all(chunk)
            .map_err(|e| io_failure(&format!("write {tmp}"), &e))?;
    }
    rustix::fs::fsync(&out).map_err(os(format!("fsync {tmp}")))?;
    drop(out);
    rustix::fs::renameat(dir, tmp, dir, name).map_err(os(format!("rename {tmp} to {dst_path}")))?;
    rustix::fs::fsync(dir).map_err(os("fsync"))
}

/// The directory the caller's paths are relative to, found as the caller
/// finds it (tilo-deployment's and tilo-secureboot's `esp_path`).
fn esp_root(caller: &str) -> Result<PathBuf> {
    if let Some(forced) = std::env::var_os("TILO_ESP").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(forced));
    }
    let candidates: [(&str, &str); 2] = if caller == "boot-entries" {
        [("/boot", "loader/entries"), ("/boot/efi", "loader/entries")]
    } else {
        [("/boot/efi", "EFI"), ("/boot", "EFI")]
    };
    candidates
        .into_iter()
        .find(|(root, marker)| Path::new(root).join(marker).is_dir())
        .map(|(root, _)| PathBuf::from(root))
        .ok_or_else(|| Failure::Refused("no ESP at /boot or /boot/efi".to_owned()))
}

/// Remount the ESP's mount read-write in this (private) namespace.
fn writable(root: &Path) -> Result<()> {
    let shown = root.display();
    let st = rustix::fs::statvfs(root).map_err(os(format!("statvfs {shown}")))?;
    if !st.f_flag.contains(StatVfsMountFlags::RDONLY) {
        return Ok(());
    }
    if std::env::var_os("TILO_ESP_WRITE_NO_REMOUNT").as_deref() == Some(OsStr::new("1")) {
        return Err(Failure::Refused(format!("{shown} is read-only")));
    }
    let found = Command::new("findmnt")
        .args([
            OsStr::new("-no"),
            OsStr::new("TARGET"),
            OsStr::new("-T"),
            root.as_os_str(),
        ])
        .output()
        .map_err(|e| Failure::Other(format!("findmnt: {e}")))?;
    if !found.status.success() {
        return Err(Failure::Other(format!(
            "findmnt -T {shown} failed ({}): {}",
            found.status,
            String::from_utf8_lossy(&found.stderr).trim()
        )));
    }
    let target = String::from_utf8_lossy(&found.stdout).trim().to_owned();
    let status = Command::new("mount")
        .args(["-o", "remount,rw", &target])
        .status()
        .map_err(|e| Failure::Other(format!("mount: {e}")))?;
    if !status.success() {
        return Err(Failure::Other(format!(
            "mount -o remount,rw {target} failed ({status})"
        )));
    }
    Ok(())
}

fn env_int(key: &str, default: i64) -> Result<i64> {
    match std::env::var_os(key) {
        None => Ok(default),
        Some(value) => value
            .to_str()
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| Failure::Other(format!("{key} is not an integer"))),
    }
}

fn read_ops(plan_dir: BorrowedFd<'_>) -> Result<Vec<Value>> {
    let fd = rustix::fs::openat(
        plan_dir,
        "plan.json",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(os("open plan.json"))?;
    let mut text = Vec::new();
    File::from(fd)
        .read_to_end(&mut text)
        .map_err(|e| io_failure("read plan.json", &e))?;
    let plan: Value = serde_json::from_slice(&text)
        .map_err(|e| Failure::Other(format!("plan.json is not JSON: {e}")))?;
    let Value::Object(mut plan) = plan else {
        return Err(Failure::Other("plan.json is not an object".to_owned()));
    };
    match plan.remove("ops") {
        Some(Value::Array(ops)) => Ok(ops),
        _ => Err(Failure::Refused(
            "plan.json has no operation list".to_owned(),
        )),
    }
}

fn run_plan(policy: &Policy, plan_dir: BorrowedFd<'_>, crash_after: i64) -> Result<()> {
    let ops = read_ops(plan_dir)?;
    let root = esp_root(policy.caller)?;
    writable(&root)?;
    let root_fd = rustix::fs::openat(CWD, &root, dir_flags(), Mode::empty())
        .map_err(os(format!("open {}", root.display())))?;
    let mut stdout = io::stdout().lock();
    for (i, op) in ops.iter().enumerate() {
        if i64::try_from(i).is_ok_and(|i| i == crash_after) {
            return Err(Failure::Interrupted);
        }
        apply(policy, op, root_fd.as_fd(), plan_dir)?;
        let mut line = format!("tilo-esp-write: caller={} ", policy.caller);
        py_json(op, &mut line);
        writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .map_err(|e| io_failure("log", &e))?;
    }
    Ok(())
}

fn clear_plan(plan_dir: BorrowedFd<'_>) -> Result<()> {
    for name in list_dir(plan_dir)? {
        rustix::fs::unlinkat(plan_dir, name.as_c_str(), AtFlags::empty())
            .map_err(os(format!("remove staged {}", name.to_string_lossy())))?;
    }
    Ok(())
}

fn run(policy: &Policy) -> Result<()> {
    let plan_root =
        std::env::var_os("TILO_ESP_PLAN_ROOT").unwrap_or_else(|| OsString::from(DEFAULT_PLAN_ROOT));
    let plan_path = Path::new(&plan_root).join(policy.caller);
    let plan_dir = rustix::fs::openat(CWD, &plan_path, dir_flags(), Mode::empty())
        .map_err(os(format!("open {}", plan_path.display())))?;
    let st = rustix::fs::fstat(&plan_dir).map_err(os(format!("stat {}", plan_path.display())))?;
    let owner = env_int("TILO_ESP_PLAN_OWNER", 0)?;
    if i64::from(st.st_uid) != owner || st.st_mode & 0o077 != 0 {
        return Err(Failure::Refused(format!(
            "{} must be root's alone (0700)",
            plan_path.display()
        )));
    }
    let crash_after = env_int("TILO_ESP_WRITE_CRASH_AFTER", -1)?;
    // The plan and its staged files go whether it applied or was refused;
    // an interruption leaves them, as a crash would.
    let applied = run_plan(policy, plan_dir.as_fd(), crash_after);
    if matches!(applied, Err(Failure::Interrupted)) {
        return applied;
    }
    let cleared = clear_plan(plan_dir.as_fd());
    applied.and(cleared)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let policy = match args.as_slice() {
        [_, caller] => POLICIES
            .iter()
            .find(|p| OsStr::new(p.caller).as_bytes() == caller.as_bytes()),
        _ => None,
    };
    let Some(policy) = policy else {
        let callers: Vec<&str> = POLICIES.iter().map(|p| p.caller).collect();
        eprintln!("usage: tilo-esp-write {{{}}}", callers.join("|"));
        return ExitCode::from(2);
    };
    match run(policy) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Interrupted) => ExitCode::from(99),
        Err(Failure::Refused(why)) => {
            eprintln!("tilo-esp-write: refused: {why}");
            ExitCode::FAILURE
        }
        Err(Failure::Os { what, errno }) => {
            eprintln!("tilo-esp-write: {what}: {}", strerror(errno));
            ExitCode::FAILURE
        }
        Err(Failure::Other(why)) => {
            eprintln!("tilo-esp-write: {why}");
            ExitCode::FAILURE
        }
    }
}

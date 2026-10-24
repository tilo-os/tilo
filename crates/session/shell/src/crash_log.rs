//! Panic reports the user can send back.
//!
//! [`install`] writes `~/.cache/tilo/crashes/<timestamp>-<pid>.log` for every
//! panic and echoes the message and log path to stderr. A session launched
//! outside a terminal (greetd, a .desktop double-click) discards stderr, so
//! without the file "the shell just disappeared" is unactionable.
//!
//! The same directory keeps the record of a desktop that stopped responding
//! ([`render_freeze`], `<timestamp>-<pid>-freeze.log`, written by the main
//! loop's watchdog), so everything that ended a session is in one place on
//! this computer, and nothing leaves it unless the person sends it.
//! `shell::crash_notice` tells the person once, at the next start.
//!
//! The backtrace is symbolized in the field because release binaries keep
//! their symbol table (see `[profile.release]` in the workspace `Cargo.toml`):
//! each frame names its function, and a build with debug info adds
//! `file:line`. The header names the exact binary by its GNU build id, which
//! matches the build id of the CI artifact the package was made from.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The identity of the running binary, recorded once at install time so the
/// panic path does no file parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildInfo {
    /// The crate version the binary was built from.
    pub version: &'static str,
    /// The executable that is running, if the kernel names it.
    pub executable: Option<PathBuf>,
    /// The GNU build id (hex) of that executable, if it carries one.
    pub build_id: Option<String>,
}

impl BuildInfo {
    /// Describe the running process.
    #[must_use]
    pub fn current() -> Self {
        let executable = std::env::current_exe().ok();
        let build_id = executable.as_deref().and_then(read_build_id);
        Self {
            version: env!("CARGO_PKG_VERSION"),
            executable,
            build_id,
        }
    }
}

/// What a crash report says about one panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panic<'a> {
    /// Local time of the panic, already formatted.
    pub timestamp: &'a str,
    /// Name of the panicking thread.
    pub thread: &'a str,
    /// Whether a panic boundary contains the panic, and what follows from it.
    pub contained: &'a str,
    /// `file:line:column` of the panic site.
    pub location: &'a str,
    /// The panic payload, as text.
    pub message: &'a str,
    /// The rendered backtrace.
    pub backtrace: &'a str,
}

/// The binary a record names and its build id, or what stands in for them.
fn identity(build: &BuildInfo) -> (String, &str) {
    let executable = build
        .executable
        .as_deref()
        .map_or_else(|| "<unknown>".to_owned(), |path| path.display().to_string());
    (executable, build.build_id.as_deref().unwrap_or("<none>"))
}

/// Render the report written for a panic.
#[must_use]
pub fn render(title: &str, build: &BuildInfo, panic: &Panic<'_>) -> String {
    let mut body = String::new();
    let rule = "=".repeat(title.chars().count());
    let (executable, build_id) = identity(build);
    // Writing to a String cannot fail.
    let _ = write!(
        body,
        "{title}\n\
         {rule}\n\
         timestamp: {}\n\
         version:   {}\n\
         binary:    {executable}\n\
         build-id:  {build_id}\n\
         thread:    {}\n\
         contained: {}\n\
         location:  {}\n\
         message:   {}\n\n\
         backtrace:\n{}\n",
        panic.timestamp,
        build.version,
        panic.thread,
        panic.contained,
        panic.location,
        panic.message,
        panic.backtrace
    );
    body
}

/// Where the records live: the private `tilo/crashes` directory of the
/// person's cache, created if missing. `None` without a home cache directory,
/// or when the path is not a private directory of this user's: a shared
/// fallback such as `/tmp` would let another user plant the path.
#[must_use]
pub fn directory() -> Option<PathBuf> {
    tilo_paths::cache_dir()
        .map(|cache| cache.join("tilo").join("crashes"))
        .filter(|dir| tilo_files::native::private::directory(dir).is_ok())
}

/// The local time now, as a record's file name starts with it. Names sort in
/// the order the records were written.
#[must_use]
pub fn file_timestamp() -> String {
    // format: machine (the crash report's file name)
    chrono::Local::now().format(FILE_TIMESTAMP).to_string()
}

/// How a record's file name writes its local time.
pub const FILE_TIMESTAMP: &str = "%Y-%m-%d-%H%M%S";

/// The end of a freeze record's file name; a panic's ends in `.log` alone.
pub const FREEZE_SUFFIX: &str = "-freeze.log";

/// The heading of a freeze record.
pub const FREEZE_TITLE: &str = "tilo desktop freeze";

/// What a freeze record says about a main loop that stopped for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freeze<'a> {
    /// Local time the watchdog judged the loop hung, already formatted.
    pub timestamp: &'a str,
    /// The compositor thread's id.
    pub tid: i32,
    /// How long it had slept without running when judged hung.
    pub blocked_for: std::time::Duration,
    /// The watchdog's report: what the thread waited in, then every thread
    /// with its stack.
    pub report: &'a str,
}

/// Render the record written for a desktop that stopped responding.
#[must_use]
pub fn render_freeze(build: &BuildInfo, freeze: &Freeze<'_>) -> String {
    let mut body = String::new();
    let rule = "=".repeat(FREEZE_TITLE.chars().count());
    let (executable, build_id) = identity(build);
    // Writing to a String cannot fail.
    let _ = write!(
        body,
        "{FREEZE_TITLE}\n\
         {rule}\n\
         timestamp: {}\n\
         version:   {}\n\
         binary:    {executable}\n\
         build-id:  {build_id}\n\
         thread:    {}\n\
         stalled:   {:.1} s\n\
         outcome:   the compositor ended and the session returned to the login screen\n\n\
         {}\n",
        freeze.timestamp,
        build.version,
        freeze.tid,
        freeze.blocked_for.as_secs_f64(),
        freeze.report,
    );
    body
}

/// Write `body` as the new record `name` in `dir`: a private file, created
/// only if it does not exist, never through a link.
///
/// # Errors
///
/// The file exists already or could not be created or written.
pub fn write_record(dir: &Path, name: &str, body: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    let path = dir.join(name);
    tilo_files::native::private::create_new(&path)?.write_all(body.as_bytes())?;
    Ok(path)
}

/// Install a panic hook that writes a crash report and then chains to the
/// previous hook, so a developer running from a terminal still sees the
/// usual panic output.
///
/// Reports carry backtraces and payloads, so they are private (0700 directory,
/// 0600 file, never following a link). Without a home cache directory only the
/// stderr copy is made: a shared fallback such as `/tmp` would let another user
/// plant the path.
pub fn install(title: &'static str) {
    let dir = directory();
    let build = BuildInfo::current();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let timestamp = file_timestamp();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<unknown payload>".to_owned());
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let contained = if tilo_rt::panic_boundary::is_isolated() {
            "yes: the failing helper restarts and the session continues"
        } else {
            "no: the session ends"
        };
        let location = info.location().map_or_else(
            || "<unknown location>".to_owned(),
            |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
        );
        let body = render(
            title,
            &build,
            &Panic {
                timestamp: &timestamp,
                thread,
                contained,
                location: &location,
                message: &message,
                backtrace: &backtrace,
            },
        );
        if let Some(dir) = &dir {
            let name = format!("{timestamp}-{}.log", std::process::id());
            let path = dir.join(&name);
            if write_record(dir, &name, &body).is_ok() {
                eprintln!("\n[tilo] crash log written to {}", path.display());
            } else {
                eprintln!("\n[tilo] FAILED to write crash log to {}", path.display());
            }
        } else {
            eprintln!("\n[tilo] no private cache directory; crash log not written");
        }
        default_hook(info);
    }));
}

const PT_NOTE: u32 = 4;
const NT_GNU_BUILD_ID: u32 = 3;
/// Notes are a few hundred bytes; anything larger is not a note segment we
/// want to read in full.
const MAX_NOTE_SEGMENT: u64 = 64 * 1024;

/// The GNU build id of a 64-bit little-endian ELF file (`x86_64` and `aarch64`,
/// the architectures Tilo ships), read from its note segments.
#[must_use]
pub fn read_build_id(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header).ok()?;
    // Magic, ELFCLASS64, ELFDATA2LSB.
    if header.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let phoff = le_u64(&header, 0x20)?;
    let phentsize = u64::from(le_u16(&header, 0x36)?);
    let phnum = u64::from(le_u16(&header, 0x38)?);
    if phentsize < 56 {
        return None;
    }
    for index in 0..phnum {
        let mut phdr = [0_u8; 56];
        file.seek(SeekFrom::Start(
            phoff.checked_add(index.checked_mul(phentsize)?)?,
        ))
        .ok()?;
        file.read_exact(&mut phdr).ok()?;
        if le_u32(&phdr, 0)? != PT_NOTE {
            continue;
        }
        let offset = le_u64(&phdr, 8)?;
        let size = le_u64(&phdr, 32)?;
        if size > MAX_NOTE_SEGMENT {
            continue;
        }
        let mut notes = vec![0_u8; usize::try_from(size).ok()?];
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut notes).ok()?;
        if let Some(id) = gnu_build_id(&notes) {
            return Some(id);
        }
    }
    None
}

/// Find the `NT_GNU_BUILD_ID` note in a note segment and render it as hex.
fn gnu_build_id(mut notes: &[u8]) -> Option<String> {
    while notes.len() >= 12 {
        let namesz = usize::try_from(le_u32(notes, 0)?).ok()?;
        let descsz = usize::try_from(le_u32(notes, 4)?).ok()?;
        let kind = le_u32(notes, 8)?;
        let name_end = 12_usize.checked_add(namesz)?;
        let desc_start = name_end.checked_next_multiple_of(4)?;
        let desc_end = desc_start.checked_add(descsz)?;
        let name = notes.get(12..name_end)?;
        let desc = notes.get(desc_start..desc_end)?;
        if kind == NT_GNU_BUILD_ID && name == b"GNU\0" {
            return Some(hex::encode(desc));
        }
        notes = notes.get(desc_end.checked_next_multiple_of(4)?..)?;
    }
    None
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(kind: u32, name: &[u8], desc: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&u32::try_from(desc.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(name);
        out.resize(out.len().next_multiple_of(4), 0);
        out.extend_from_slice(desc);
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    #[test]
    fn the_build_id_is_found_after_other_notes() {
        let mut notes = note(1, b"GNU\0", &[0; 16]);
        notes.extend(note(
            NT_GNU_BUILD_ID,
            b"GNU\0",
            &[0xde, 0xad, 0xbe, 0xef, 0x01],
        ));
        assert_eq!(gnu_build_id(&notes).as_deref(), Some("deadbeef01"));
    }

    #[test]
    fn a_truncated_note_is_no_build_id() {
        let notes = note(NT_GNU_BUILD_ID, b"GNU\0", &[1, 2, 3, 4]);
        assert_eq!(gnu_build_id(&notes[..notes.len() - 2]), None);
        assert_eq!(gnu_build_id(&note(NT_GNU_BUILD_ID, b"XYZ\0", &[1])), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_running_test_binary_has_a_build_id() {
        // Every Linux link here passes --build-id (the distribution
        // compiler's default); the crash log relies on it to name the binary.
        let id = read_build_id(&std::env::current_exe().unwrap()).unwrap();
        assert!(
            id.len() >= 16 && id.chars().all(|c| c.is_ascii_hexdigit()),
            "{id}"
        );
    }

    #[test]
    fn a_report_names_the_binary_and_keeps_the_backtrace() {
        let build = BuildInfo {
            version: "0.1.0",
            executable: Some(PathBuf::from("/usr/bin/tilo-shell")),
            build_id: Some("abc123".to_owned()),
        };
        let report = render(
            "tilo shell panic",
            &build,
            &Panic {
                timestamp: "2026-10-03-120000",
                thread: "main",
                contained: "no: the session ends",
                location: "crates/session/shell/src/x.rs:1:2",
                message: "boom",
                backtrace: "   0: shell::x::f\n             at ./crates/session/shell/src/x.rs:1:2",
            },
        );
        assert_eq!(
            report,
            "tilo shell panic\n\
             ================\n\
             timestamp: 2026-10-03-120000\n\
             version:   0.1.0\n\
             binary:    /usr/bin/tilo-shell\n\
             build-id:  abc123\n\
             thread:    main\n\
             contained: no: the session ends\n\
             location:  crates/session/shell/src/x.rs:1:2\n\
             message:   boom\n\n\
             backtrace:\n   0: shell::x::f\n             at ./crates/session/shell/src/x.rs:1:2\n"
        );
    }

    #[test]
    fn a_freeze_record_names_the_binary_and_keeps_the_stacks() {
        let build = BuildInfo {
            version: "0.1.0",
            executable: Some(PathBuf::from("/usr/bin/tilo-shell")),
            build_id: Some("abc123".to_owned()),
        };
        let record = render_freeze(
            &build,
            &Freeze {
                timestamp: "2026-10-24-225201",
                tid: 412,
                blocked_for: std::time::Duration::from_millis(26_400),
                report: "main loop hung\nthread 412 (shell)\n   0: shell::x::wait",
            },
        );
        assert!(record.starts_with(FREEZE_TITLE), "{record}");
        for line in [
            "timestamp: 2026-10-24-225201\n",
            "binary:    /usr/bin/tilo-shell\n",
            "build-id:  abc123\n",
            "thread:    412\n",
            "stalled:   26.4 s\n",
            "   0: shell::x::wait\n",
        ] {
            assert!(record.contains(line), "{line:?} in:\n{record}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_record_is_private_and_never_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        let path = write_record(dir.path(), "2026-10-24-225201-1-freeze.log", "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_record(dir.path(), "2026-10-24-225201-1-freeze.log", "second").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
    }

    #[test]
    fn a_report_without_an_identity_says_so() {
        let build = BuildInfo {
            version: "0.1.0",
            executable: None,
            build_id: None,
        };
        let report = render(
            "t",
            &build,
            &Panic {
                timestamp: "",
                thread: "",
                contained: "",
                location: "",
                message: "",
                backtrace: "",
            },
        );
        assert!(report.contains("binary:    <unknown>\n"), "{report}");
        assert!(report.contains("build-id:  <none>\n"), "{report}");
    }
}

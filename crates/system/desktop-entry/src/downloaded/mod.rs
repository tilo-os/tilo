//! Programs the person downloaded: an `AppImage`, a program from an extracted
//! tarball, or a loose binary.
//!
//! None of these come with an entry in an `applications` directory, so none
//! of them is in the person's apps. This module is how one gets there, as one
//! more desktop entry in the same applications model everything else reads
//! (search, the launcher, `tilo app list`): a `.desktop` file Tilo writes
//! into `$XDG_DATA_HOME/applications/tilo-downloaded/`, whose desktop-file
//! id is therefore `tilo-downloaded-<identity>`. It is launched exactly as
//! any other host application is ([`crate::launch`]): nothing here confines
//! or unconfines anything.
//!
//! - [`classify`] decides whether a running process is a downloaded program
//!   at all, from what the kernel says about it ([`ProcessFacts`]).
//! - [`identify`] finds its name and icon, best source first: an
//!   `AppImage`'s embedded `.desktop` file and icon ([`appimage`]), a
//!   `.desktop` file or icon in the folder the program came from, then its
//!   window's app id and icon ([`WindowFacts`]).
//! - [`Store`] writes, updates and removes the entries. An entry is keyed by
//!   the app's identity, never its path: the same app run from a new path
//!   updates its entry's location instead of adding a second one. It also
//!   records the file's stable identity (device and inode), so a later step
//!   can follow the file when it is moved or renamed.
//!
//! Every function here touches the disk; none may run on the compositor's
//! main loop.

pub mod appimage;
#[cfg(feature = "appimage")]
mod squashfs;

use std::io;
use std::path::{Path, PathBuf};

use crate::icon_theme::IconFormat;

/// The subdirectory of `applications` the entries live in; through the
/// spec's id rule it is also every entry's desktop-id prefix.
pub const ENTRY_DIR: &str = "tilo-downloaded";
/// The program the entry starts: the `AppImage` or binary itself.
pub const KEY_PROGRAM: &str = "X-Tilo-Downloaded-Program";
/// The program file's stable identity when the entry was written
/// ([`file_identity`]), for following it across moves and renames.
pub const KEY_FILE: &str = "X-Tilo-Downloaded-File";
/// The other copies of the same app the person has run or added, newest
/// first: kept when a newer copy became the one the entry starts, so
/// Uninstall can trash them all and a gone copy falls back to one still
/// there.
pub const KEY_LOCATIONS: &str = "X-Tilo-Downloaded-Locations";
/// The identity the entry is keyed by.
pub const KEY_IDENTITY: &str = "X-Tilo-Downloaded-Identity";
/// `AppImage` or `Program`.
pub const KEY_KIND: &str = "X-Tilo-Downloaded-Kind";

/// The icon an entry names when the program has no artwork of its own:
/// the system's generic application icon, from the icon theme.
pub const GENERIC_ICON: &str = "application-x-executable";

/// Largest icon taken from a program's folder or image.
const MAX_ICON_BYTES: usize = 4 * 1024 * 1024;
/// Largest `.desktop` file read from a program's folder.
const MAX_DESKTOP_BYTES: u64 = 256 * 1024;
/// Folders above the program's own that are searched for its `.desktop`
/// file or icon: `foo-1.2/bin/foo` finds `foo-1.2/foo.desktop`.
const FOLDER_LEVELS: usize = 2;

/// What kind of download a program is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramKind {
    /// One self-contained file with its own name and icon inside.
    AppImage,
    /// A binary, alone or inside the folder a tarball unpacked to.
    Program,
}

impl ProgramKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AppImage => "AppImage",
            Self::Program => "Program",
        }
    }

    fn parse(raw: &str) -> Self {
        if raw.trim() == "AppImage" {
            Self::AppImage
        } else {
            Self::Program
        }
    }
}

/// A downloaded program, by the file that starts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub path: PathBuf,
    pub kind: ProgramKind,
}

/// What the kernel says about a running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFacts {
    /// `/proc/<pid>/exe`: the file the process runs.
    pub exe: PathBuf,
    /// `APPIMAGE` from its environment: an `AppImage`'s runtime names the
    /// image it mounted there for the program it starts.
    pub appimage: Option<PathBuf>,
    /// The script its parent process runs, when the parent is a shell
    /// running one: how a launcher script (`idea.sh`) starts a bundled
    /// runtime (`jbr/bin/java`).
    pub parent_script: Option<PathBuf>,
}

/// Programs that run another program's code: when one of these is what a
/// window's process runs, the program the person started is the script
/// that started it, or the one the folder's desktop entry names.
const RUNTIMES: &[&str] = &[
    "java",
    "javaw",
    "python",
    "python3",
    "node",
    "nodejs",
    "electron",
    "wine",
    "wine64",
    "wine-preloader",
    "wine64-preloader",
    "mono",
    "dotnet",
];

/// Shells a launcher script runs in.
const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

fn is_runtime(exe: &Path) -> bool {
    exe.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        RUNTIMES.contains(&name.as_ref())
            || name
                .strip_prefix("python3.")
                .is_some_and(|minor| minor.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Read [`ProcessFacts`] for `pid`. `None` when the process is gone, its
/// file was deleted, or it isn't readable (another user's).
#[cfg(target_os = "linux")]
pub fn process_facts(pid: u32) -> Option<ProcessFacts> {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    let exe = std::fs::read_link(proc.join("exe")).ok()?;
    if exe.to_string_lossy().ends_with(" (deleted)") {
        return None;
    }
    let appimage = std::fs::read(proc.join("environ"))
        .ok()
        .and_then(|environ| {
            environ.split(|byte| *byte == 0).find_map(|pair| {
                let value = pair.strip_prefix(b"APPIMAGE=")?;
                let value = std::str::from_utf8(value).ok()?;
                (!value.is_empty()).then(|| PathBuf::from(value))
            })
        });
    Some(ProcessFacts {
        exe,
        appimage,
        parent_script: parent_script(&proc),
    })
}

/// The script `proc`'s parent runs, when the parent is a shell running a
/// file: `bash ./idea.sh` or a `#!/bin/sh` script started directly, which
/// the kernel starts as `/bin/sh ./idea.sh`. A relative path is resolved
/// against the parent's working folder.
#[cfg(target_os = "linux")]
fn parent_script(proc: &Path) -> Option<PathBuf> {
    let stat = std::fs::read_to_string(proc.join("stat")).ok()?;
    let ppid: u32 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let parent = PathBuf::from(format!("/proc/{ppid}"));
    let shell = std::fs::read_link(parent.join("exe")).ok()?;
    if !shell
        .file_name()
        .is_some_and(|name| SHELLS.contains(&name.to_string_lossy().as_ref()))
    {
        return None;
    }
    let cmdline = std::fs::read(parent.join("cmdline")).ok()?;
    let script = cmdline
        .split(|byte| *byte == 0)
        .skip(1)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .find(|arg| !arg.is_empty() && !arg.starts_with('-'))?;
    let script = PathBuf::from(script);
    let script = if script.is_absolute() {
        script
    } else {
        std::fs::read_link(parent.join("cwd")).ok()?.join(script)
    };
    std::fs::metadata(&script)
        .is_ok_and(|meta| meta.is_file())
        .then_some(script)
}

/// Whether a running process is a downloaded program, and which file
/// starts it.
///
/// An `AppImage` names itself through its runtime's `APPIMAGE` variable,
/// checked against the file's own `AppImage` magic. Anything else counts
/// when its file is in the person's home, and not:
/// - under a hidden folder (`~/.cargo/bin`, `~/.local`): tools installed
///   there were put there by a package manager or an installer;
/// - inside a source checkout (a `.git` in any folder between it and home):
///   a program the person is building is not an app they downloaded.
///
/// Everything outside home (the image's `/usr`, Flatpak's runtimes, a
/// Tilo package) has an entry of its own or is not a downloaded program.
pub fn classify(facts: &ProcessFacts, home: &Path) -> Option<Program> {
    if let Some(image) = &facts.appimage
        && image.is_absolute()
        && appimage::is_appimage(image)
    {
        return Some(Program {
            path: image.clone(),
            kind: ProgramKind::AppImage,
        });
    }
    let launcher;
    let exe = if is_runtime(&facts.exe) {
        // The runtime is not the program: the script that started it is,
        // or the program the folder's desktop entry names.
        launcher = facts
            .parent_script
            .clone()
            .filter(|script| script.starts_with(home))
            .or_else(|| entry_program(&facts.exe, home))?;
        &launcher
    } else {
        &facts.exe
    };
    let relative = exe.strip_prefix(home).ok()?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
    {
        return None;
    }
    let mut folder = exe.parent();
    while let Some(dir) = folder {
        if dir == home || !dir.starts_with(home) {
            break;
        }
        if dir.join(".git").exists() {
            return None;
        }
        folder = dir.parent();
    }
    if !std::fs::metadata(exe).is_ok_and(|meta| meta.is_file()) {
        return None;
    }
    // A loose AppImage started some other way (extracted and run) still
    // names its image through the magic of the file itself.
    let kind = if appimage::is_appimage(exe) {
        ProgramKind::AppImage
    } else {
        ProgramKind::Program
    };
    Some(Program {
        path: exe.clone(),
        kind,
    })
}

/// The program a desktop entry beside `exe` (in its folder or up to
/// [`FOLDER_LEVELS`] above, never above `home`) starts, when that program is
/// a file in the same tree: how a bundled runtime finds the launcher its
/// app ships.
fn entry_program(exe: &Path, home: &Path) -> Option<PathBuf> {
    let mut folder = exe.parent();
    let mut level = 0_usize;
    while let Some(dir) = folder {
        if dir == home || !dir.starts_with(home) || level > FOLDER_LEVELS {
            return None;
        }
        let mut desktops: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        desktops.sort();
        for path in desktops {
            if std::fs::metadata(&path).map_or(true, |meta| meta.len() > MAX_DESKTOP_BYTES) {
                continue;
            }
            let Some(first) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|contents| crate::entry::parse(&contents, "", &path, &[]))
                .and_then(|entry| entry.exec)
                .and_then(|exec| crate::exec::split_exec(&exec).ok())
                .and_then(|argv| argv.into_iter().next())
            else {
                continue;
            };
            let program = Path::new(&first);
            let program = if program.is_absolute() {
                program.to_path_buf()
            } else {
                dir.join(program)
            };
            if program.starts_with(dir)
                && !is_runtime(&program)
                && std::fs::metadata(&program).is_ok_and(|meta| meta.is_file())
            {
                return Some(program);
            }
        }
        folder = dir.parent();
        level = level.saturating_add(1);
    }
    None
}

/// Icon bytes and how to decode them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub format: IconFormat,
    pub bytes: Vec<u8>,
}

impl Icon {
    /// `bytes` as an icon when they are a PNG or an SVG.
    pub fn sniff(bytes: Vec<u8>) -> Option<Self> {
        let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            IconFormat::Png
        } else {
            let head = bytes.get(..bytes.len().min(1024)).unwrap_or_default();
            let head = String::from_utf8_lossy(head);
            if head.contains("<svg") {
                IconFormat::Svg
            } else {
                return None;
            }
        };
        Some(Self { format, bytes })
    }
}

/// What the program's first window said about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowFacts {
    /// The Wayland app id (for an X11 program, its class).
    pub app_id: String,
    /// The window's own icon, already encoded.
    pub icon: Option<Icon>,
}

/// A downloaded program's name and icon, and the identity its entry is
/// keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The embedded or neighbouring desktop id, else the window's app id.
    pub key: String,
    pub name: String,
    pub icon: Option<Icon>,
    pub comment: Option<String>,
    pub categories: Vec<String>,
    pub keywords: Vec<String>,
    pub mime_types: Vec<String>,
    /// The window identity the program's windows announce.
    pub startup_wm_class: Option<String>,
    /// The field code (`%U`, `%F`, …) the program's own entry passes files
    /// with; `None` when it takes none.
    pub field_code: Option<String>,
}

/// Find `program`'s identity, best source first. Never fails: the window
/// always names the program.
pub fn identify(program: &Program, window: &WindowFacts) -> Identity {
    identify_with_source(program, window).0
}

/// [`identify`], and whether only the window named the program.
fn identify_with_source(program: &Program, window: &WindowFacts) -> (Identity, bool) {
    #[cfg(feature = "appimage")]
    let embedded = match program.kind {
        ProgramKind::AppImage => appimage::read_identity(&program.path).ok().flatten(),
        ProgramKind::Program => None,
    };
    #[cfg(not(feature = "appimage"))]
    let embedded: Option<Found> = None;
    let found = embedded.or_else(|| folder_identity(&program.path, &window.app_id));
    let window_key = window_key(&window.app_id, &program.path);
    // Only the window names it: no entry anywhere, or just an icon under
    // the program's own name, keyed by the window.
    let from_window = found.as_ref().is_none_or(|found| found.key == window_key);
    let identity = match found {
        Some(found) => Identity {
            key: found.key,
            name: found.entry.name.clone(),
            icon: found.icon.or_else(|| window.icon.clone()),
            comment: found.entry.comment.clone(),
            categories: found.entry.categories.clone(),
            keywords: found.entry.keywords.clone(),
            mime_types: found.entry.mime_types.clone(),
            startup_wm_class: found
                .entry
                .startup_wm_class
                .clone()
                .or_else(|| (!window.app_id.trim().is_empty()).then(|| window.app_id.clone())),
            field_code: found.entry.exec.as_deref().and_then(field_code),
        },
        None => Identity {
            name: name_from_app_id(&window_key),
            key: window_key,
            icon: window.icon.clone(),
            comment: None,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            startup_wm_class: (!window.app_id.trim().is_empty()).then(|| window.app_id.clone()),
            field_code: None,
        },
    };
    (identity, from_window)
}

/// A desktop entry found for a program, with its icon.
pub(crate) struct Found {
    pub key: String,
    pub entry: crate::DesktopEntry,
    pub icon: Option<Icon>,
}

fn window_key(app_id: &str, program: &Path) -> String {
    let app_id = app_id.trim();
    if !app_id.is_empty() && !app_id.starts_with("anon-") {
        return app_id.to_owned();
    }
    program
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `org.example.MyTool` reads as `MyTool`, `mytool` as `Mytool`.
fn name_from_app_id(app_id: &str) -> String {
    let last = app_id
        .trim_end_matches(".desktop")
        .rsplit('.')
        .next()
        .unwrap_or(app_id);
    let mut chars = last.chars();
    match chars.next() {
        Some(first) if first.is_lowercase() => first.to_uppercase().chain(chars).collect(),
        Some(first) => std::iter::once(first).chain(chars).collect(),
        None => app_id.to_owned(),
    }
}

/// The first file-taking field code in an `Exec=` value.
fn field_code(exec: &str) -> Option<String> {
    crate::exec::split_exec(exec)
        .ok()?
        .into_iter()
        .find_map(|token| {
            ["%U", "%u", "%F", "%f"]
                .into_iter()
                .find(|code| token == *code)
                .map(str::to_owned)
        })
}

/// The folders a person keeps things in, which are never one program's
/// folder: the search for a program's `.desktop` file stops there.
fn boundaries() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = [
        tilo_paths::home(),
        tilo_paths::download_dir(),
        tilo_paths::desktop_dir(),
        tilo_paths::document_dir(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some(home) = tilo_paths::home() {
        out.push(home.join("Applications"));
    }
    out
}

/// A `.desktop` file or icon in the folder `program` came from.
///
/// A `.desktop` file counts when its `Exec=` runs this program, or its
/// `StartupWMClass` or id is the window's app id, or it is the only one
/// beside the program. With none, an icon named like the program
/// (`foo.png` beside `foo`) or in the folder's `hicolor` tree still gives
/// the program its picture.
fn folder_identity(program: &Path, app_id: &str) -> Option<Found> {
    let stops = boundaries();
    let program_name = program.file_name()?.to_string_lossy().into_owned();
    let mut folders = Vec::new();
    let mut folder = program.parent();
    while let Some(dir) = folder {
        if folders.len() > FOLDER_LEVELS || stops.iter().any(|stop| stop == dir) {
            break;
        }
        folders.push(dir.to_path_buf());
        folder = dir.parent();
    }
    let locales = crate::locale::locale_candidates();
    for (depth, dir) in folders.iter().enumerate() {
        let mut desktops: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        desktops.sort();
        let only_one = desktops.len() == 1 && depth == 0;
        for path in desktops {
            if std::fs::metadata(&path).map_or(true, |meta| meta.len() > MAX_DESKTOP_BYTES) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(entry) = crate::entry::parse(&contents, &id, &path, &locales) else {
                continue;
            };
            let runs_it = entry
                .exec
                .as_deref()
                .and_then(|exec| crate::exec::split_exec(exec).ok())
                .and_then(|argv| argv.into_iter().next())
                .is_some_and(|first| {
                    Path::new(&first)
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy() == program_name)
                });
            let names_window = !app_id.is_empty()
                && (entry.startup_wm_class.as_deref() == Some(app_id) || entry.id == app_id);
            if runs_it || names_window || only_one {
                let icon = entry
                    .icon
                    .as_deref()
                    .and_then(|name| folder_icon(&folders, dir, name));
                return Some(Found {
                    key: id,
                    entry,
                    icon: icon.or_else(|| folder_icon(&folders, dir, &program_name)),
                });
            }
        }
    }
    // No entry: an icon alone still names the picture, under the program's
    // own name, so the window's identity keys the entry.
    let icon = folders
        .first()
        .and_then(|dir| folder_icon(&folders, dir, &program_name))?;
    let key = window_key(app_id, program);
    let mut entry = crate::entry::parse(
        "[Desktop Entry]\nType=Application\n",
        &key,
        program,
        &locales,
    )?;
    entry.name = name_from_app_id(&key);
    Some(Found {
        key,
        entry,
        icon: Some(icon),
    })
}

/// The icon called `name` for a program in `folders`: an absolute path, a
/// file beside the entry, or one in a `hicolor` tree of the folder.
fn folder_icon(folders: &[PathBuf], beside: &Path, name: &str) -> Option<Icon> {
    let read = |path: &Path| -> Option<Icon> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() > MAX_ICON_BYTES as u64 {
            return None;
        }
        Icon::sniff(std::fs::read(path).ok()?)
    };
    let name_path = Path::new(name);
    if name_path.is_absolute() {
        return folders
            .iter()
            .any(|folder| name_path.starts_with(folder))
            .then(|| read(name_path))
            .flatten();
    }
    if name.contains('/') {
        return read(&beside.join(name));
    }
    let mut candidates = Vec::new();
    for folder in std::iter::once(beside).chain(folders.iter().map(PathBuf::as_path)) {
        for ext in ["png", "svg"] {
            candidates.push(folder.join(format!("{name}.{ext}")));
        }
        for root in [
            "share/icons/hicolor",
            "icons/hicolor",
            "usr/share/icons/hicolor",
        ] {
            for size in [
                "scalable", "512x512", "256x256", "128x128", "64x64", "48x48",
            ] {
                for ext in ["svg", "png"] {
                    candidates.push(folder.join(format!("{root}/{size}/apps/{name}.{ext}")));
                }
            }
        }
    }
    candidates.iter().find_map(|path| read(path))
}

/// The entry recorded for one identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub desktop_id: String,
    pub path: PathBuf,
    pub program: PathBuf,
    pub kind: ProgramKind,
    /// The other copies ([`KEY_LOCATIONS`]).
    pub locations: Vec<PathBuf>,
}

/// The program a desktop entry was written for, as the scan reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadedProgram {
    pub program: PathBuf,
    pub kind: ProgramKind,
    /// `<device>:<inode>` when the entry was written.
    pub file: Option<String>,
    /// Whether the program's file is there. Set by the scan, which runs on
    /// a worker; a freshly parsed entry assumes it is.
    pub present: bool,
    /// The other copies of the app, newest first ([`KEY_LOCATIONS`]).
    pub locations: Vec<PathBuf>,
}

impl DownloadedProgram {
    pub(crate) fn from_keys(
        program: Option<&str>,
        kind: Option<&str>,
        file: Option<&str>,
        locations: Option<&str>,
    ) -> Option<Self> {
        let program = program.map(str::trim).filter(|p| p.starts_with('/'))?;
        Some(Self {
            program: PathBuf::from(program),
            kind: ProgramKind::parse(kind.unwrap_or_default()),
            file: file
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map(str::to_owned),
            present: true,
            locations: crate::entry::string_list(locations)
                .into_iter()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .collect(),
        })
    }

    /// Every copy of the app: the one the entry starts, then the others.
    #[must_use]
    pub fn copies(&self) -> Vec<PathBuf> {
        std::iter::once(self.program.clone())
            .chain(self.locations.iter().cloned())
            .collect()
    }

    /// The file record id of Tilo's filesystem the program had when its
    /// entry was written: what finds it again after a move or rename.
    #[must_use]
    pub fn record(&self) -> Option<[u8; 16]> {
        parse_record(self.file.as_deref()?.strip_prefix(RECORD_PREFIX)?)
    }
}

/// The [`file_identity`] of a file that has a record id.
const RECORD_PREFIX: &str = "record:";
/// The extended attribute Tilo's filesystem serves each file's record id
/// on (16 bytes), the same through every move and rename.
const RECORD_ID_XATTR: &str = "user.tilo.record-id";

fn parse_record(hex: &str) -> Option<[u8; 16]> {
    let digits: Vec<u8> = hex.bytes().filter(|byte| *byte != b'-').collect();
    if digits.len() != 32 {
        return None;
    }
    let mut out = [0_u8; 16];
    for (byte, pair) in out.iter_mut().zip(digits.chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

fn format_record(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(36);
    for (index, byte) in id.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The record id Tilo's filesystem gives the file at `path`, if it is on
/// it.
#[cfg(target_os = "linux")]
fn record_id(path: &Path) -> Option<[u8; 16]> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let name = std::ffi::CString::new(RECORD_ID_XATTR).ok()?;
    let mut id = [0_u8; 16];
    // SAFETY: both strings are NUL-terminated and outlive the call, and the
    // buffer is writable for the length passed.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            id.as_mut_ptr().cast(),
            id.len(),
        )
    };
    (usize::try_from(read).ok() == Some(id.len())).then_some(id)
}

#[cfg(not(target_os = "linux"))]
fn record_id(_path: &Path) -> Option<[u8; 16]> {
    None
}

/// A file the person declined to add.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Declined {
    /// [`file_identity`], when the file has one.
    file: Option<String>,
    path: PathBuf,
}

/// The entries of downloaded programs, as files in an `applications`
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// `…/applications/tilo-downloaded`.
    entries: PathBuf,
    /// Where their icons are kept, outside every icon-theme root.
    icons: PathBuf,
    /// The files the person was asked about and said no to, one per line.
    declined: PathBuf,
}

impl Store {
    /// The session's store, under `$XDG_DATA_HOME`.
    pub fn for_session() -> Option<Self> {
        tilo_paths::xdg::data_home().map(|data| Self::in_data_home(&data))
    }

    /// The store of the person whose `$XDG_DATA_HOME` is `data`.
    pub fn in_data_home(data: &Path) -> Self {
        Self::at(
            &data.join("applications"),
            &data.join("tilo").join("downloaded-apps"),
        )
    }

    /// A store writing into `applications_dir` (an XDG `applications`
    /// directory) and keeping what else it knows (icons, the files the
    /// person declined) in `state_dir`.
    pub fn at(applications_dir: &Path, state_dir: &Path) -> Self {
        Self {
            entries: applications_dir.join(ENTRY_DIR),
            icons: state_dir.join("icons"),
            declined: state_dir.join("declined"),
        }
    }

    /// Remember that the person declined to add the program at `path`, so
    /// that file is not asked about again. Another file (a new version, a
    /// copy) is. Files that are gone are forgotten on the way.
    ///
    /// # Errors
    ///
    /// The list could not be written.
    pub fn decline(&self, path: &Path) -> io::Result<()> {
        let mut declined: Vec<Declined> = self
            .declined_files()
            .into_iter()
            .filter(|declined| declined.path.exists() && declined.path != path)
            .collect();
        declined.push(Declined {
            file: file_identity(path),
            path: path.to_path_buf(),
        });
        let text = serde_json::to_vec(&declined).map_err(io::Error::other)?;
        if let Some(parent) = self.declined.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomically(&self.declined, &text)
    }

    /// Whether the person declined the program at `path`: the same file,
    /// wherever it is now, or this path when the file has no identity.
    pub fn declined(&self, path: &Path) -> bool {
        let file = file_identity(path);
        self.declined_files()
            .iter()
            .any(|declined| match (&file, &declined.file) {
                (Some(file), Some(recorded)) => file == recorded,
                _ => declined.path == path,
            })
    }

    fn declined_files(&self) -> Vec<Declined> {
        std::fs::read(&self.declined)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The desktop-file id an entry keyed by `key` has.
    pub fn desktop_id(key: &str) -> String {
        format!("{ENTRY_DIR}-{}", slug(key))
    }

    /// Whether `desktop_id` names an entry of this store.
    pub fn owns(desktop_id: &str) -> bool {
        desktop_id
            .strip_prefix(ENTRY_DIR)
            .is_some_and(|rest| rest.starts_with('-'))
    }

    fn entry_path(&self, desktop_id: &str) -> Option<PathBuf> {
        let file = desktop_id.strip_prefix(ENTRY_DIR)?.strip_prefix('-')?;
        if file.is_empty() || file.contains('/') || file.starts_with('.') {
            return None;
        }
        Some(self.entries.join(format!("{file}.desktop")))
    }

    /// The entry keyed by `key`, if there is one.
    pub fn lookup(&self, key: &str) -> Option<Recorded> {
        self.read(&Self::desktop_id(key))
    }

    fn read(&self, desktop_id: &str) -> Option<Recorded> {
        let path = self.entry_path(desktop_id)?;
        let contents = std::fs::read_to_string(&path).ok()?;
        let groups = crate::entry::unlocalised_groups(&contents);
        let main = groups.get("Desktop Entry")?;
        let downloaded = DownloadedProgram::from_keys(
            main.get(KEY_PROGRAM).map(String::as_str),
            main.get(KEY_KIND).map(String::as_str),
            None,
            main.get(KEY_LOCATIONS).map(String::as_str),
        )?;
        Some(Recorded {
            desktop_id: desktop_id.to_owned(),
            path,
            program: downloaded.program,
            kind: downloaded.kind,
            locations: downloaded.locations,
        })
    }

    /// Add `program` to the person's apps as `identity`, or point the existing
    /// entry for `identity` at it. Returns the entry's desktop id.
    ///
    /// # Errors
    ///
    /// The entry or its icon could not be written.
    pub fn add(&self, identity: &Identity, program: &Program) -> io::Result<String> {
        let desktop_id = Self::desktop_id(&identity.key);
        let path = self
            .entry_path(&desktop_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no usable identity"))?;
        std::fs::create_dir_all(&self.entries)?;
        let icon = match &identity.icon {
            Some(icon) => Some(self.write_icon(&slug(&identity.key), icon)?),
            None => self
                .read(&desktop_id)
                .and_then(|_| existing_icon(&path))
                .or_else(|| Some(GENERIC_ICON.to_owned())),
        };
        // The newest copy is the one the entry starts; the copy it replaces
        // and the ones before it stay known while they are there.
        let locations = self
            .read(&desktop_id)
            .map(|recorded| {
                other_copies(
                    std::iter::once(recorded.program).chain(recorded.locations),
                    &program.path,
                )
            })
            .unwrap_or_default();
        let contents = render(identity, program, icon.as_deref(), &locations);
        write_atomically(&path, contents.as_bytes())?;
        Ok(desktop_id)
    }

    /// Point the entry for `identity` at `program` when there is one and it
    /// names another file. Running an app from a new path (a new version, a
    /// second download) updates its entry rather than adding a second one.
    /// Returns the desktop id when it changed something.
    ///
    /// # Errors
    ///
    /// The entry could not be rewritten.
    pub fn update_location(
        &self,
        identity: &Identity,
        program: &Program,
    ) -> io::Result<Option<String>> {
        let Some(recorded) = self.lookup(&identity.key) else {
            return Ok(None);
        };
        if recorded.program == program.path
            && file_identity(&program.path).is_some_and(|now| {
                std::fs::read_to_string(&recorded.path)
                    .is_ok_and(|contents| contents.contains(&format!("{KEY_FILE}={now}")))
            })
        {
            return Ok(None);
        }
        self.add(identity, program).map(Some)
    }

    /// Remove the entry `desktop_id` from the person's apps. The program's file
    /// is never touched; only the entry and the icon Tilo kept for it go.
    ///
    /// # Errors
    ///
    /// `desktop_id` is not an entry of this store, or it could not be
    /// removed.
    pub fn remove(&self, desktop_id: &str) -> io::Result<()> {
        let path = self
            .entry_path(desktop_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a downloaded app"))?;
        // The icon goes first: once the entry is gone, so is everything of
        // it, for whoever watches the entry.
        if let Some(icon) =
            existing_icon(&path).filter(|icon| Path::new(icon).starts_with(&self.icons))
        {
            let _ = std::fs::remove_file(icon);
        }
        std::fs::remove_file(&path)
    }

    /// Point the entry `desktop_id` at `program`, the file the person found
    /// when it went missing.
    ///
    /// # Errors
    ///
    /// No such entry, `program` is not a runnable file, or the entry could
    /// not be rewritten.
    pub fn locate(&self, desktop_id: &str, program: &Path) -> io::Result<()> {
        let path = self
            .entry_path(desktop_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a downloaded app"))?;
        let contents = std::fs::read_to_string(&path)?;
        if !crate::scan::program_exists(&program.to_string_lossy()) || !program.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "that file is not a program",
            ));
        }
        let kind = if appimage::is_appimage(program) {
            ProgramKind::AppImage
        } else {
            ProgramKind::Program
        };
        let locales = crate::locale::locale_candidates();
        let entry = crate::entry::parse(&contents, desktop_id, &path, &locales)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unreadable entry"))?;
        let groups = crate::entry::unlocalised_groups(&contents);
        let key = groups
            .get("Desktop Entry")
            .and_then(|main| main.get(KEY_IDENTITY).cloned())
            .unwrap_or_else(|| desktop_id.to_owned());
        let identity = Identity {
            key,
            name: entry.name.clone(),
            icon: None,
            comment: entry.comment.clone(),
            categories: entry.categories.clone(),
            keywords: entry.keywords.clone(),
            mime_types: entry.mime_types.clone(),
            startup_wm_class: entry.startup_wm_class.clone(),
            field_code: entry.exec.as_deref().and_then(field_code),
        };
        let locations = entry
            .downloaded
            .map(|downloaded| other_copies(downloaded.copies(), program))
            .unwrap_or_default();
        let rendered = render(
            &identity,
            &Program {
                path: program.to_path_buf(),
                kind,
            },
            entry.icon.as_deref(),
            &locations,
        );
        write_atomically(&path, rendered.as_bytes())
    }

    /// When the file the entry `desktop_id` starts is gone, make the newest
    /// other copy still there the one it starts. Returns that copy, or
    /// `None` when the file is there or no other copy is.
    ///
    /// # Errors
    ///
    /// The entry could not be rewritten.
    pub fn fall_back(&self, desktop_id: &str) -> io::Result<Option<PathBuf>> {
        let Some(recorded) = self.read(desktop_id) else {
            return Ok(None);
        };
        if std::fs::metadata(&recorded.program).is_ok() {
            return Ok(None);
        }
        let Some(copy) = recorded
            .locations
            .into_iter()
            .find(|copy| std::fs::metadata(copy).is_ok_and(|meta| meta.is_file()))
        else {
            return Ok(None);
        };
        self.locate(desktop_id, &copy)?;
        Ok(Some(copy))
    }

    /// The identity of an entry this store has for a program whose window
    /// is `app_id`: the entry's `StartupWMClass`, or the desktop file name it
    /// was keyed by, is that app id. How a copy that names itself only
    /// through its window (a bare binary) joins the entry another copy of
    /// the same program made, rather than adding a second one.
    #[must_use]
    pub fn key_for_window(&self, app_id: &str) -> Option<String> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return None;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.entries)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "desktop"))
            .collect();
        files.sort();
        files.into_iter().find_map(|path| {
            let contents = std::fs::read_to_string(&path).ok()?;
            let groups = crate::entry::unlocalised_groups(&contents);
            let main = groups.get("Desktop Entry")?;
            let key = main.get(KEY_IDENTITY)?;
            let class = main
                .get("StartupWMClass")
                .map(|class| crate::entry::unescape(class));
            let same = |name: &str| name.trim().eq_ignore_ascii_case(app_id);
            (same(key) || class.as_deref().is_some_and(same)).then(|| key.clone())
        })
    }

    /// [`identify`] `program`, joining an entry this store already has
    /// for the same window ([`Self::key_for_window`]) when nothing but the
    /// window names the program.
    #[must_use]
    pub fn identify(&self, program: &Program, window: &WindowFacts) -> Identity {
        let (mut identity, from_window) = identify_with_source(program, window);
        if from_window
            && let Some(recorded) = self
                .key_for_window(&window.app_id)
                .and_then(|key| self.lookup(&key))
        {
            let contents = std::fs::read_to_string(&recorded.path).unwrap_or_default();
            let locales = crate::locale::locale_candidates();
            if let Some(entry) =
                crate::entry::parse(&contents, &recorded.desktop_id, &recorded.path, &locales)
            {
                identity.name = entry.name;
                identity.comment = entry.comment;
                identity.categories = entry.categories;
                identity.keywords = entry.keywords;
                identity.mime_types = entry.mime_types;
                identity.field_code = entry.exec.as_deref().and_then(field_code);
                identity.startup_wm_class = entry.startup_wm_class.or(identity.startup_wm_class);
            }
            let groups = crate::entry::unlocalised_groups(&contents);
            if let Some(key) = groups
                .get("Desktop Entry")
                .and_then(|main| main.get(KEY_IDENTITY))
            {
                identity.key.clone_from(key);
            }
        }
        identity
    }

    fn write_icon(&self, slug: &str, icon: &Icon) -> io::Result<String> {
        std::fs::create_dir_all(&self.icons)?;
        for format in [IconFormat::Png, IconFormat::Svg] {
            if format != icon.format {
                let _ =
                    std::fs::remove_file(self.icons.join(format!("{slug}.{}", format.extension())));
            }
        }
        let path = self
            .icons
            .join(format!("{slug}.{}", icon.format.extension()));
        write_atomically(&path, &icon.bytes)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

fn existing_icon(entry: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(entry).ok()?;
    let groups = crate::entry::unlocalised_groups(&contents);
    groups
        .get("Desktop Entry")?
        .get("Icon")
        .map(|icon| crate::entry::unescape(icon))
        .filter(|icon| icon.starts_with('/'))
}

/// The stable identity of the file at `path`: `record:<id>` with its record
/// id when it is on Tilo's filesystem, which a move or rename keeps and
/// which finds the file again; otherwise `<device>:<inode>`, which tells a
/// replaced file from the same one.
#[cfg(unix)]
pub fn file_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    if let Some(id) = record_id(path) {
        return Some(format!("{RECORD_PREFIX}{}", format_record(&id)));
    }
    Some(format!("{}:{}", meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub fn file_identity(_path: &Path) -> Option<String> {
    None
}

/// Whether `path` is a compiled program that runs by itself (an ELF
/// executable; every `AppImage` is one), as opposed to a script, a
/// document, an object file or a shared library. Reads its first bytes.
pub fn is_program(path: &Path) -> bool {
    use std::io::Read as _;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let extension = Path::new(&name)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned());
    let library = matches!(extension.as_deref(), Some("so" | "ko")) || name.contains(".so.");
    let mut header = [0_u8; 18];
    let read = !library
        && std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
        && std::fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .is_ok();
    // `e_type`: 2 is an executable, 3 a position-independent one (or a
    // library, which the name told apart); 1, an object file, never runs.
    let kind = match header.get(5) {
        Some(1) => header.get(16).copied(),
        Some(2) => header.get(17).copied(),
        _ => None,
    };
    read && header.get(..4) == Some(b"\x7fELF".as_slice()) && matches!(kind, Some(2 | 3))
}

/// Move the file at `program` into `folder` (the person's Applications
/// folder), keeping its name, or `<name> (2)` and on when that is taken.
/// Within one filesystem it is a rename; across filesystems the copy is
/// written and synced before the original goes. Returns where it is now.
///
/// Only for a self-contained file (an `AppImage`): a program that sits
/// among files it needs must stay where it is.
///
/// # Errors
///
/// The folder could not be made or the file could not be moved; the file
/// is then where it was.
pub fn move_into(program: &Path, folder: &Path) -> io::Result<PathBuf> {
    let name = program
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file"))?;
    if program.parent() == Some(folder) {
        return Ok(program.to_path_buf());
    }
    std::fs::create_dir_all(folder)?;
    let target = free_name(folder, Path::new(name));
    match std::fs::rename(program, &target) {
        Ok(()) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_then_remove(program, &target)?;
            Ok(target)
        }
        Err(error) => Err(error),
    }
}

/// `name` in `folder`, or the first `<stem> (n).<ext>` nothing has taken.
fn free_name(folder: &Path, name: &Path) -> PathBuf {
    let first = folder.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let stem = name
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = name
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy()))
        .unwrap_or_default();
    (2_u32..=u32::MAX)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|candidate| std::fs::symlink_metadata(candidate).is_err())
        .unwrap_or(first)
}

/// Copy `from` to `to` with its permissions, sync it, then remove `from`.
fn copy_then_remove(from: &Path, to: &Path) -> io::Result<()> {
    let written = std::fs::copy(from, to).and_then(|_| {
        let file = std::fs::File::open(to)?;
        file.sync_all()
    });
    if let Err(error) = written {
        let _ = std::fs::remove_file(to);
        return Err(error);
    }
    std::fs::remove_file(from)
}

/// `key` as a file name: letters, digits, `.`, `_` and `-` kept, anything
/// else `_`, never empty and never hidden.
fn slug(key: &str) -> String {
    let mut out: String = key
        .trim()
        .trim_end_matches(".desktop")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

/// One value escaped for a desktop file (no newlines, backslashes doubled).
fn value(raw: &str) -> String {
    raw.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn list(items: &[String]) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str(&value(&item.replace(';', "\\;")));
        out.push(';');
    }
    out
}

/// `path` as one `Exec=` argument, quoted when it needs to be.
fn exec_argument(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('%', "%%");
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '%'))
    {
        return raw;
    }
    let mut out = String::from("\"");
    for c in raw.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The copies among `copies` that are still files and are not `current`,
/// in order, each once.
fn other_copies(copies: impl IntoIterator<Item = PathBuf>, current: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for copy in copies {
        if copy != current
            && !out.contains(&copy)
            && std::fs::metadata(&copy).is_ok_and(|meta| meta.is_file())
        {
            out.push(copy);
        }
    }
    out
}

fn render(
    identity: &Identity,
    program: &Program,
    icon: Option<&str>,
    locations: &[PathBuf],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("[Desktop Entry]\nType=Application\n");
    let _ = writeln!(out, "Name={}", value(&identity.name));
    if let Some(comment) = &identity.comment {
        let _ = writeln!(out, "Comment={}", value(comment));
    }
    let mut exec = exec_argument(&program.path);
    if let Some(code) = &identity.field_code {
        exec.push(' ');
        exec.push_str(code);
    }
    let _ = writeln!(out, "Exec={}", value(&exec));
    if let Some(icon) = icon {
        let _ = writeln!(out, "Icon={}", value(icon));
    }
    if let Some(class) = &identity.startup_wm_class {
        let _ = writeln!(out, "StartupWMClass={}", value(class));
    }
    if !identity.categories.is_empty() {
        let _ = writeln!(out, "Categories={}", list(&identity.categories));
    }
    if !identity.keywords.is_empty() {
        let _ = writeln!(out, "Keywords={}", list(&identity.keywords));
    }
    if !identity.mime_types.is_empty() {
        let _ = writeln!(out, "MimeType={}", list(&identity.mime_types));
    }
    let _ = writeln!(out, "{KEY_IDENTITY}={}", value(&identity.key));
    let _ = writeln!(out, "{KEY_KIND}={}", program.kind.as_str());
    let _ = writeln!(
        out,
        "{KEY_PROGRAM}={}",
        value(&program.path.to_string_lossy())
    );
    if let Some(file) = file_identity(&program.path) {
        let _ = writeln!(out, "{KEY_FILE}={file}");
    }
    if !locations.is_empty() {
        let paths: Vec<String> = locations
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let _ = writeln!(out, "{KEY_LOCATIONS}={}", list(&paths));
    }
    out
}

/// Write `bytes` to `path` through a temporary file beside it, so a reader
/// (the scan's worker) never sees half an entry.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let temporary = dir.join(format!(".{}.partial", name.to_string_lossy()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests;

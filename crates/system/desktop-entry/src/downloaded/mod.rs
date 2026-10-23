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
/// (`<device>:<inode>`), for following it across moves and renames.
pub const KEY_FILE: &str = "X-Tilo-Downloaded-File";
/// The identity the entry is keyed by.
pub const KEY_IDENTITY: &str = "X-Tilo-Downloaded-Identity";
/// `AppImage` or `Program`.
pub const KEY_KIND: &str = "X-Tilo-Downloaded-Kind";

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
    Some(ProcessFacts { exe, appimage })
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
    let exe = &facts.exe;
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
    #[cfg(feature = "appimage")]
    let embedded = match program.kind {
        ProgramKind::AppImage => appimage::read_identity(&program.path).ok().flatten(),
        ProgramKind::Program => None,
    };
    #[cfg(not(feature = "appimage"))]
    let embedded: Option<Found> = None;
    let found = embedded.or_else(|| folder_identity(&program.path, &window.app_id));
    let window_key = window_key(&window.app_id, &program.path);
    match found {
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
    }
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
}

impl DownloadedProgram {
    pub(crate) fn from_keys(
        program: Option<&str>,
        kind: Option<&str>,
        file: Option<&str>,
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
        })
    }
}

/// The entries of downloaded programs, as files in an `applications`
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// `…/applications/tilo-downloaded`.
    entries: PathBuf,
    /// Where their icons are kept, outside every icon-theme root.
    icons: PathBuf,
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
            &data.join("tilo").join("downloaded-apps").join("icons"),
        )
    }

    /// A store writing into `applications_dir` (an XDG `applications`
    /// directory) and keeping icons in `icons_dir`.
    pub fn at(applications_dir: &Path, icons_dir: &Path) -> Self {
        Self {
            entries: applications_dir.join(ENTRY_DIR),
            icons: icons_dir.to_path_buf(),
        }
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
        )?;
        Some(Recorded {
            desktop_id: desktop_id.to_owned(),
            path,
            program: downloaded.program,
            kind: downloaded.kind,
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
            None => self.read(&desktop_id).and_then(|_| existing_icon(&path)),
        };
        let contents = render(identity, program, icon.as_deref());
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
        let icon = existing_icon(&path);
        std::fs::remove_file(&path)?;
        if let Some(icon) = icon.filter(|icon| Path::new(icon).starts_with(&self.icons)) {
            let _ = std::fs::remove_file(icon);
        }
        Ok(())
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
        let rendered = render(
            &identity,
            &Program {
                path: program.to_path_buf(),
                kind,
            },
            entry.icon.as_deref(),
        );
        write_atomically(&path, rendered.as_bytes())
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

/// `<device>:<inode>` of the file at `path`.
#[cfg(unix)]
pub fn file_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    Some(format!("{}:{}", meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
pub fn file_identity(_path: &Path) -> Option<String> {
    None
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

fn render(identity: &Identity, program: &Program, icon: Option<&str>) -> String {
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

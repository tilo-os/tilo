//! The familiar folders of a home (Documents, Desktop, Pictures, Downloads,
//! Music, Videos), as the system itself maps them.
//!
//! The installer suggests them and `tilo migrate review home` lists them.
//! Results are suggestions only: nothing turns a discovered path into a
//! folder that comes over until the person chooses it.

#[cfg(target_os = "linux")]
use std::io::Read;
use std::path::PathBuf;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownFolderKind {
    Documents,
    Desktop,
    Pictures,
    Downloads,
    Music,
    Videos,
}

impl KnownFolderKind {
    pub const ALL: [Self; 6] = [
        Self::Documents,
        Self::Desktop,
        Self::Pictures,
        Self::Downloads,
        Self::Music,
        Self::Videos,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Desktop => "Desktop",
            Self::Pictures => "Pictures",
            Self::Downloads => "Downloads",
            Self::Music => "Music",
            Self::Videos => "Videos",
        }
    }

    #[cfg(target_os = "linux")]
    const fn xdg_name(self) -> &'static str {
        match self {
            Self::Documents => "DOCUMENTS",
            Self::Desktop => "DESKTOP",
            Self::Pictures => "PICTURES",
            Self::Downloads => "DOWNLOAD",
            Self::Music => "MUSIC",
            Self::Videos => "VIDEOS",
        }
    }

    #[cfg(any(target_os = "linux", test))]
    const fn xdg_config_key(self) -> &'static str {
        match self {
            Self::Documents => "XDG_DOCUMENTS_DIR",
            Self::Desktop => "XDG_DESKTOP_DIR",
            Self::Pictures => "XDG_PICTURES_DIR",
            Self::Downloads => "XDG_DOWNLOAD_DIR",
            Self::Music => "XDG_MUSIC_DIR",
            Self::Videos => "XDG_VIDEOS_DIR",
        }
    }

    #[cfg(target_os = "windows")]
    const fn windows_shell_name(self) -> &'static str {
        match self {
            Self::Documents => "shell:Personal",
            Self::Desktop => "shell:Desktop",
            Self::Pictures => "shell:My Pictures",
            Self::Downloads => "shell:Downloads",
            Self::Music => "shell:My Music",
            Self::Videos => "shell:My Video",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownFolder {
    pub kind: KnownFolderKind,
    pub path: Option<PathBuf>,
    pub unavailable_reason: Option<String>,
}

impl KnownFolder {
    fn unavailable(kind: KnownFolderKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            path: None,
            unavailable_reason: Some(reason.into()),
        }
    }
}

/// Look up the familiar user folders through the system's own mapping.
///
/// This is intentionally blocking; callers call it from a worker thread.
#[must_use]
pub fn discover() -> Vec<KnownFolder> {
    KnownFolderKind::ALL.into_iter().map(discover_one).collect()
}

#[cfg(target_os = "linux")]
fn discover_one(kind: KnownFolderKind) -> KnownFolder {
    let config_path = tilo_paths::xdg::config_home()
        .unwrap_or_default()
        .join("user-dirs.dirs");
    let Ok(file) = std::fs::File::open(&config_path) else {
        return KnownFolder::unavailable(kind, "this folder is not configured");
    };
    let mut config = String::new();
    if file
        .take(128 * 1024 + 1)
        .read_to_string(&mut config)
        .is_err()
        || config.len() > 128 * 1024
    {
        return KnownFolder::unavailable(kind, "folder configuration is invalid or too large");
    }
    if !xdg_entry_is_configured(&config, kind) {
        return KnownFolder::unavailable(kind, "this folder is not configured");
    }
    let mut command = Command::new("xdg-user-dir");
    command.arg(kind.xdg_name());
    let output = match bounded_command::run_prepared(
        command,
        None,
        128 * 1024,
        std::time::Duration::from_secs(10),
    ) {
        Ok(output) => output,
        Err(error) => {
            return KnownFolder::unavailable(
                kind,
                format!("native folder lookup could not start: {error}"),
            );
        }
    };
    if !output.status.success() {
        return KnownFolder::unavailable(kind, "native folder lookup did not succeed");
    }
    let resolved = resolved_folder(kind, parse_lookup_path(&output.stdout));
    if let (Some(path), Some(home)) = (resolved.path.as_ref(), std::env::var_os("HOME"))
        && path == &PathBuf::from(home)
    {
        return KnownFolder::unavailable(
            kind,
            "the configured folder resolves to the home directory",
        );
    }
    resolved
}

#[cfg(target_os = "windows")]
fn discover_one(kind: KnownFolderKind) -> KnownFolder {
    let script = format!(
        "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false); \
         $folder = (New-Object -ComObject Shell.Application).NameSpace('{}'); \
         if ($null -ne $folder) {{ [Console]::Out.Write($folder.Self.Path) }}",
        kind.windows_shell_name()
    );
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script]);
    let output = match bounded_command::run_prepared(
        command,
        None,
        128 * 1024,
        std::time::Duration::from_secs(10),
    ) {
        Ok(output) => output,
        Err(error) => {
            return KnownFolder::unavailable(
                kind,
                format!("native folder lookup could not start: {error}"),
            );
        }
    };
    if !output.status.success() {
        return KnownFolder::unavailable(kind, "native folder lookup did not succeed");
    }
    resolved_folder(kind, parse_lookup_path(&output.stdout))
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn discover_one(kind: KnownFolderKind) -> KnownFolder {
    KnownFolder::unavailable(
        kind,
        "known-folder suggestions are available on Windows and Linux",
    )
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn resolved_folder(kind: KnownFolderKind, parsed: Result<Option<PathBuf>, String>) -> KnownFolder {
    let path = match parsed {
        Ok(Some(path)) => path,
        Ok(None) => return KnownFolder::unavailable(kind, "this folder is not configured"),
        Err(error) => return KnownFolder::unavailable(kind, error),
    };
    if !path.is_dir() {
        return KnownFolder::unavailable(kind, "the configured folder does not exist locally");
    }
    KnownFolder {
        kind,
        path: Some(path),
        unavailable_reason: None,
    }
}

#[cfg(any(target_os = "linux", target_os = "windows", test))]
fn parse_lookup_path(stdout: &[u8]) -> Result<Option<PathBuf>, String> {
    let text = std::str::from_utf8(stdout)
        .map_err(|_| "native folder lookup returned invalid UTF-8".to_string())?;
    let text = text.trim_end_matches(['\r', '\n']);
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

#[cfg(any(target_os = "linux", test))]
fn xdg_entry_is_configured(config: &str, kind: KnownFolderKind) -> bool {
    let key = kind.xdg_config_key();
    config.lines().any(|line| {
        let line = line.trim_start();
        !line.starts_with('#')
            && line
                .split_once('=')
                .is_some_and(|(found, value)| found.trim() == key && !value.trim().is_empty())
    })
}

//! The files that register the runtime's search providers with GNOME Shell
//! and `KRunner`: each host finds a provider through a file naming its bus
//! name and object path, and GNOME Shell also needs a desktop entry to show.
//!
//! `KRunner` and D-Bus read `$XDG_DATA_HOME`, but GNOME Shell reads providers
//! only from `$XDG_DATA_DIRS`: its file goes into a Tilo export directory that
//! a systemd `environment.d` entry adds at the next login.

use std::io;
use std::path::{Path, PathBuf};

use crate::dbus::{BUS_NAME, GNOME_PATH, KRUNNER_PATH};

const NAME: &str = "Tilo";

const EXPORT: &str = "tilo/export/share";

/// Each file's path and contents, for the data and config homes given and a
/// runtime executable at `runtime`.
///
/// # Errors
///
/// A path these formats cannot quote: relative, not UTF-8, or holding quotes,
/// escapes, `$`, `%` or control characters.
pub fn files(
    data_home: &Path,
    config_home: &Path,
    runtime: &Path,
) -> io::Result<Vec<(PathBuf, String)>> {
    let exec = quoted(runtime)?;
    let export = data_home.join(EXPORT);
    let export_dirs = quotable(&export)?;
    Ok(vec![
        (
            data_home.join(format!("applications/{BUS_NAME}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName={NAME}\nExec={exec}\nTerminal=false\n"
            ),
        ),
        (
            export.join(format!(
                "gnome-shell/search-providers/{BUS_NAME}.search-provider.ini"
            )),
            format!(
                "[Shell Search Provider]\nDesktopId={BUS_NAME}.desktop\nBusName={BUS_NAME}\n\
                 ObjectPath={GNOME_PATH}\nVersion=2\n"
            ),
        ),
        (
            // The systemd defaults stand in for an unset list, which would
            // otherwise lose the system's own data directories.
            config_home.join("environment.d/60-tilo-search.conf"),
            format!(
                "XDG_DATA_DIRS=${{XDG_DATA_DIRS:-/usr/local/share/:/usr/share/}}:{export_dirs}\n"
            ),
        ),
        (
            data_home.join(format!("krunner/dbusplugins/{BUS_NAME}.desktop")),
            format!(
                "[Desktop Entry]\nName={NAME}\nType=Service\nX-KDE-ServiceTypes=Plasma/Runner\n\
                 X-KDE-PluginInfo-Name={BUS_NAME}\nX-KDE-PluginInfo-EnabledByDefault=true\n\
                 X-Plasma-API=DBus\nX-Plasma-API-Minimum-Version=2.0\n\
                 X-Plasma-DBusRunner-Service={BUS_NAME}\nX-Plasma-DBusRunner-Path={KRUNNER_PATH}\n"
            ),
        ),
        (
            data_home.join(format!("dbus-1/services/{BUS_NAME}.service")),
            format!("[D-BUS Service]\nName={BUS_NAME}\nExec={exec}\n"),
        ),
    ])
}

/// Write [`files`], leaving any already up to date alone.
pub fn write(data_home: &Path, config_home: &Path, runtime: &Path) -> io::Result<()> {
    for (path, contents) in files(data_home, config_home, runtime)? {
        if std::fs::read_to_string(&path).is_ok_and(|held| held == contents) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    Ok(())
}

fn quotable(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|text| {
            path.is_absolute()
                && !text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\' | '`' | '$' | '%'))
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the path {} cannot be written into a desktop entry",
                    path.display()
                ),
            )
        })
}

fn quoted(runtime: &Path) -> io::Result<String> {
    let text = quotable(runtime)?;
    Ok(if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    })
}

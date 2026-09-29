//! The files that register the runtime's search providers with GNOME Shell
//! and KRunner, under a user's `$XDG_DATA_HOME`.
//!
//! Both hosts find a provider through a file naming its bus name and object
//! path, and D-Bus starts the runtime for either when it is not running.
//! GNOME Shell also requires a desktop entry it shows, whose name labels the
//! results and lists the provider in Settings → Search.

use std::io;
use std::path::{Path, PathBuf};

use crate::dbus::{BUS_NAME, GNOME_PATH, KRUNNER_PATH};

/// The name every file carries.
const NAME: &str = "Tilo";

/// Each file's path under the data home, and its contents, for a runtime
/// executable at `runtime`.
///
/// # Errors
///
/// Refuses a `runtime` path that desktop entries and D-Bus service files
/// cannot quote: relative, not UTF-8, or holding quotes, escapes, `$`, `%`
/// or control characters.
pub fn files(runtime: &Path) -> io::Result<Vec<(PathBuf, String)>> {
    let exec = format!("{} --daemon", quoted(runtime)?);
    Ok(vec![
        (
            PathBuf::from(format!("applications/{BUS_NAME}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName={NAME}\nExec={exec}\nTerminal=false\n"
            ),
        ),
        (
            PathBuf::from(format!(
                "gnome-shell/search-providers/{BUS_NAME}.search-provider.ini"
            )),
            format!(
                "[Shell Search Provider]\nDesktopId={BUS_NAME}.desktop\nBusName={BUS_NAME}\n\
                 ObjectPath={GNOME_PATH}\nVersion=2\n"
            ),
        ),
        (
            PathBuf::from(format!("krunner/dbusplugins/{BUS_NAME}.desktop")),
            format!(
                "[Desktop Entry]\nName={NAME}\nType=Service\nX-KDE-ServiceTypes=Plasma/Runner\n\
                 X-KDE-PluginInfo-Name={BUS_NAME}\nX-KDE-PluginInfo-EnabledByDefault=true\n\
                 X-Plasma-API=DBus\nX-Plasma-API-Minimum-Version=2.0\n\
                 X-Plasma-DBusRunner-Service={BUS_NAME}\nX-Plasma-DBusRunner-Path={KRUNNER_PATH}\n"
            ),
        ),
        (
            PathBuf::from(format!("dbus-1/services/{BUS_NAME}.service")),
            format!("[D-BUS Service]\nName={BUS_NAME}\nExec={exec}\n"),
        ),
    ])
}

/// Write [`files`] under `data_home`, leaving any already up to date alone.
///
/// # Errors
///
/// As [`files`], and I/O errors creating directories or writing.
pub fn write(data_home: &Path, runtime: &Path) -> io::Result<()> {
    for (path, contents) in files(runtime)? {
        let path = data_home.join(path);
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

/// `runtime` as one argument both formats read the same way.
fn quoted(runtime: &Path) -> io::Result<String> {
    let text = runtime
        .to_str()
        .filter(|text| {
            runtime.is_absolute()
                && !text
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '"' | '\'' | '\\' | '`' | '$' | '%'))
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "the runtime path {} cannot be written into a desktop entry",
                    runtime.display()
                ),
            )
        })?;
    Ok(if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_names_the_bus_the_runtime_serves() {
        let data = tempfile::tempdir().unwrap();
        let runtime = Path::new("/opt/Tilo Apps/bin/tilo-runtime");
        write(data.path(), runtime).unwrap();
        let read = |path: &str| std::fs::read_to_string(data.path().join(path)).unwrap();

        let provider = read("gnome-shell/search-providers/so.tilo.Runtime.search-provider.ini");
        assert!(
            provider.contains("\nBusName=so.tilo.Runtime\n"),
            "{provider}"
        );
        assert!(provider.contains("\nObjectPath=/so/tilo/Runtime/SearchProvider\n"));
        assert!(provider.contains("\nDesktopId=so.tilo.Runtime.desktop\n"));
        assert!(provider.contains("\nVersion=2\n"));

        let entry = read("applications/so.tilo.Runtime.desktop");
        // GNOME Shell ignores a provider whose desktop entry it would not show.
        assert!(!entry.contains("NoDisplay"), "{entry}");
        assert!(
            entry.contains("\nExec=\"/opt/Tilo Apps/bin/tilo-runtime\" --daemon\n"),
            "{entry}"
        );

        let runner = read("krunner/dbusplugins/so.tilo.Runtime.desktop");
        assert!(runner.contains("\nX-Plasma-API=DBus\n"), "{runner}");
        assert!(runner.contains("\nX-Plasma-DBusRunner-Service=so.tilo.Runtime\n"));
        assert!(runner.contains("\nX-Plasma-DBusRunner-Path=/so/tilo/Runtime/KRunner\n"));

        let service = read("dbus-1/services/so.tilo.Runtime.service");
        assert_eq!(
            service,
            "[D-BUS Service]\nName=so.tilo.Runtime\nExec=\"/opt/Tilo Apps/bin/tilo-runtime\" --daemon\n"
        );

        // Idempotent: an up-to-date file is not rewritten.
        let before = std::fs::metadata(data.path().join("dbus-1/services/so.tilo.Runtime.service"))
            .unwrap()
            .modified()
            .unwrap();
        write(data.path(), runtime).unwrap();
        let after = std::fs::metadata(data.path().join("dbus-1/services/so.tilo.Runtime.service"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_path_the_formats_cannot_quote_is_refused() {
        for path in [
            "tilo-runtime",
            "/opt/$HOME/tilo-runtime",
            "/opt/a\"b",
            "/opt/100%",
            "/opt/a\nb",
        ] {
            assert!(files(Path::new(path)).is_err(), "{path:?}");
        }
    }
}

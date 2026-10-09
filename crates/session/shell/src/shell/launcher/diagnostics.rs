//! Content-free diagnostics for future children of one desktop session.
#[cfg(unix)]
use std::io::Read;
use std::process::Command;
use std::{io, path::PathBuf};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "versioned persistence mirrors the existing four independent boolean diagnostic action parameters"
)]
struct ManagedOptions {
    schema: u32,
    presentation: bool,
    gpu_stages: bool,
    gpu_timestamps: bool,
    gpu_dispatch_timestamps: bool,
}

fn managed_path() -> io::Result<PathBuf> {
    tilo_paths::config_dir()
        .filter(|root| root.is_absolute())
        .map(|root| root.join("tilo/diagnostics/session.json"))
        .ok_or_else(|| {
            io::Error::other("session diagnostics require an absolute user config directory")
        })
}

#[cfg(unix)]
fn load_managed() -> io::Result<Option<Options>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = managed_path()?;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions and does not mutate memory.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "session diagnostics must be an owner-only regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(io::Error::other("session diagnostics exceed 4096 bytes"));
    }
    let saved: ManagedOptions = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if saved.schema != 1 || saved.gpu_dispatch_timestamps && !saved.gpu_timestamps {
        return Err(io::Error::other(
            "unsupported or inconsistent session diagnostics schema",
        ));
    }
    Ok(Some(Options([
        saved.presentation,
        saved.gpu_stages,
        saved.gpu_timestamps,
        saved.gpu_dispatch_timestamps,
    ])))
}

#[cfg(not(unix))]
fn load_managed() -> io::Result<Option<Options>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "managed session diagnostics require Unix ownership",
    ))
}

/// Load managed profiling before any desktop threads or GPU constructors start.
/// Missing configuration preserves explicit developer startup options.
///
/// # Safety
/// The caller must still be single-threaded; no concurrent environment readers
/// may exist. Never call this from a running desktop or its configure action.
///
/// # Errors
/// Refuses unreadable, oversized, non-private or invalid configuration.
#[expect(
    clippy::disallowed_methods,
    reason = "only main before shader/service/GPU threads applies the four fixed loader diagnostics; live configure never mutates process environment"
)]
pub unsafe fn apply_managed_startup() -> io::Result<()> {
    if let Some(options) = load_managed()? {
        for (enabled, (_, name)) in options.0.into_iter().zip(FIELDS) {
            // SAFETY: the caller guarantees single-threaded startup.
            unsafe {
                if enabled {
                    std::env::set_var(name, "1");
                } else {
                    std::env::remove_var(name);
                }
            }
        }
    }
    Ok(())
}

pub(crate) const FIELDS: [(&str, &str); 4] = [
    ("presentation", "TILO_PROFILE_PRESENTATION"),
    ("gpu_stages", "TILO_PROFILE_GPU_STAGES"),
    ("gpu_timestamps", "VELLO_GPU_TIMESTAMPS"),
    ("gpu_dispatch_timestamps", "VELLO_GPU_DISPATCH_TIMESTAMPS"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Options(pub(crate) [bool; 4]);

impl Options {
    pub(crate) fn persist(self) -> io::Result<()> {
        let [
            presentation,
            gpu_stages,
            gpu_timestamps,
            gpu_dispatch_timestamps,
        ] = self.0;
        let saved = ManagedOptions {
            schema: 1,
            presentation,
            gpu_stages,
            gpu_timestamps,
            gpu_dispatch_timestamps,
        };
        let bytes = serde_json::to_vec(&saved).map_err(io::Error::other)?;
        tilo_files::native::atomic::write_private(&managed_path()?, &bytes)
    }
    pub(crate) fn inherited() -> Self {
        Self(FIELDS.map(|(_, name)| std::env::var(name).is_ok_and(|v| v == "1")))
    }

    pub(crate) fn apply(self, command: &mut Command) {
        for (enabled, (_, name)) in self.0.into_iter().zip(FIELDS) {
            if enabled {
                command.env(name, "1");
            } else {
                command.env_remove(name);
            }
        }
    }

    pub(crate) fn value(self) -> serde_json::Value {
        FIELDS
            .into_iter()
            .zip(self.0)
            .map(|((field, _), enabled)| (field.to_owned(), serde_json::Value::Bool(enabled)))
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn managed_session_diagnostics_startup_refuses_invalid_files_without_partial_flags() {
        use std::os::unix::fs::PermissionsExt;
        let config = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        env.set("VELLO_GPU_TIMESTAMPS", "0");
        env.set("TILO_PROFILE_GPU_STAGES", "0");
        // Missing managed policy retains explicit developer controls.
        unsafe { apply_managed_startup() }.unwrap();
        assert_eq!(std::env::var("VELLO_GPU_TIMESTAMPS").unwrap(), "0");
        let path = managed_path().unwrap();
        for bytes in [
            br#"{"schema":2,"presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false}"#.to_vec(),
            br#"{"schema":1,"presentation":true,"gpu_stages":true,"gpu_timestamps":false,"gpu_dispatch_timestamps":true}"#.to_vec(),
            br#"{"schema":1,"presentation":true,"gpu_stages":true,"gpu_timestamps":true,"gpu_dispatch_timestamps":false,"environment":{}}"#.to_vec(),
            br#"{"schema":1,"presentation":true}"#.to_vec(),
            vec![b' ';4097],
        ] {
            tilo_files::native::atomic::write_private(&path, &bytes).unwrap();
            // SAFETY: ScopedEnv serialises environment access.
            assert!(unsafe { apply_managed_startup() }.is_err());
            assert_eq!(std::env::var("VELLO_GPU_TIMESTAMPS").unwrap(), "0");
            assert_eq!(std::env::var("TILO_PROFILE_GPU_STAGES").unwrap(), "0");
        }
        Options([false; 4]).persist().unwrap();
        // SAFETY: ScopedEnv serialises environment access.
        unsafe { apply_managed_startup() }.unwrap();
        for (_, name) in FIELDS {
            assert!(
                std::env::var_os(name).is_none(),
                "persisted disable removes {name}"
            );
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(unsafe { apply_managed_startup() }.is_err());
        std::fs::remove_file(&path).unwrap();
        let other = config.path().join("other");
        tilo_files::native::atomic::write_private(&other, b"{}").unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(unsafe { apply_managed_startup() }.is_err());
    }

    #[test]
    fn session_diagnostics_control_live_delivery_without_mutating_startup_environment() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("TILO_PROFILE_PRESENTATION");
        let session = super::super::LaunchSession::default();
        assert!(!session.presentation_observer_enabled());
        env.set("TILO_PROFILE_PRESENTATION", "1");
        assert!(session.presentation_observer_enabled());
        session.configure_diagnostics(Options([false; 4]));
        assert!(!session.presentation_observer_enabled());
        assert_eq!(std::env::var("TILO_PROFILE_PRESENTATION").unwrap(), "1");
        session.configure_diagnostics(Options([true, false, false, false]));
        env.remove("TILO_PROFILE_PRESENTATION");
        assert!(session.presentation_observer_enabled());
        assert!(std::env::var_os("TILO_PROFILE_PRESENTATION").is_none());
    }

    #[test]
    fn session_diagnostics_apply_only_fixed_future_child_environment() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s:%s:%s:%s:%s' \"$TILO_PROFILE_PRESENTATION\" \"$TILO_PROFILE_GPU_STAGES\" \"$VELLO_GPU_TIMESTAMPS\" \"$VELLO_GPU_DISPATCH_TIMESTAMPS\" \"$ORDINARY_LAUNCH_VALUE\""]);
        command.env("ORDINARY_LAUNCH_VALUE", "retained");
        for (_, field) in FIELDS {
            command.env(field, "old");
        }
        Options([true, false, true, false]).apply(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"1::1::retained");
        Options([false; 4]).apply(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"::::retained");
    }
}

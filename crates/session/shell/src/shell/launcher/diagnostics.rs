//! Content-free diagnostics for future children of one desktop session.
use std::process::Command;

pub(crate) const FIELDS: [(&str, &str); 4] = [
    ("presentation", "TILO_PROFILE_PRESENTATION"),
    ("gpu_stages", "TILO_PROFILE_GPU_STAGES"),
    ("gpu_timestamps", "VELLO_GPU_TIMESTAMPS"),
    ("gpu_dispatch_timestamps", "VELLO_GPU_DISPATCH_TIMESTAMPS"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Options(pub(crate) [bool; 4]);

impl Options {
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

/// Freeze effective diagnostic values before the retained command is signed.
/// Configured options override every command value just as normal apply does;
/// unconfigured launches preserve explicit values, then inherited values.
pub(crate) fn freeze_retained(
    env: &mut Vec<(String, String)>,
    options: Option<Options>,
) -> std::io::Result<()> {
    if let Some(options) = options {
        env.retain(|(key, _)| !FIELDS.iter().any(|(_, name)| key == name));
        for (enabled, (_, name)) in options.0.into_iter().zip(FIELDS) {
            if enabled {
                env.push((name.into(), "1".into()));
            }
        }
    } else {
        for (_, name) in FIELDS {
            if env.iter().any(|(key, _)| key == name) {
                continue;
            }
            if let Some(value) = std::env::var_os(name) {
                let value = value
                    .into_string()
                    .map_err(|_| std::io::ErrorKind::InvalidData)?;
                env.push((name.into(), value));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "test fixtures transitively own long-lived permits and lock guards by design"
    )]
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
    #[test]
    fn retained_diagnostics_freeze_configured_absence_and_preserve_unconfigured_overrides() {
        let mut ambient = tilo_test_env::ScopedEnv::new();
        for (_, name) in FIELDS {
            ambient.set(name, "ambient");
        }
        let mut env = vec![
            (FIELDS[0].1.into(), "explicit".into()),
            ("ORDINARY".into(), "keep".into()),
        ];
        freeze_retained(&mut env, Some(Options([false, true, false, false]))).unwrap();
        assert_eq!(
            env,
            vec![
                ("ORDINARY".into(), "keep".into()),
                (FIELDS[1].1.into(), "1".into())
            ]
        );
        for (_, name) in FIELDS {
            assert_eq!(std::env::var(name).unwrap(), "ambient");
        }
        let mut env = vec![(FIELDS[0].1.into(), "explicit".into())];
        freeze_retained(&mut env, None).unwrap();
        assert_eq!(env[0].1, "explicit");
        for (_, name) in FIELDS.into_iter().skip(1) {
            assert!(
                env.iter()
                    .any(|(key, value)| key == name && value == "ambient")
            );
        }
    }
}

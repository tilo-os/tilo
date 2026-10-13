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

#[cfg(test)]
mod tests {
    use super::*;

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

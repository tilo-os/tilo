//! The opt-in additional codecs pack, as Settings sees it.
//!
//! Tilo ships no patented codecs. When the owner asks, the root helper
//! `/usr/libexec/tilo-codec-pack` downloads them on this device from RPM Fusion
//! and from Cisco, verifies every package against pinned keys and merges
//! them as local system extensions. Settings never runs that helper itself: it
//! starts one of two fixed, polkit-allowed unit instances and reads the
//! helper's world-readable status file. See
//! `/usr/share/doc/tilo/codec-pack.md`.

use std::time::Duration;

/// Root-owned, world-readable status the helper rewrites atomically.
pub const STATUS_PATH: &str = "/var/lib/tilo/codec-pack/status.json";
/// Resolving, downloading and unpacking the pack takes minutes on a slow link.
const OPERATION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_OUTPUT: usize = 64 * 1024;

/// What the owner asked for. Each maps to exactly one unit instance, the only
/// two the codec-pack polkit rule lets a desktop session start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecAction {
    Install,
    Remove,
}

impl CodecAction {
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Install => "tilo-codec-pack@install.service",
            Self::Remove => "tilo-codec-pack@remove.service",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PackState {
    #[default]
    NotInstalled,
    Installing,
    Installed,
    Removing,
    Failed,
}

/// The freeworld Mesa part, which must match the base Mesa exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MesaState {
    #[default]
    Off,
    Enabled,
    /// No RPM Fusion build of the base's Mesa version exists yet.
    Waiting,
    Unsupported,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecPackStatus {
    pub state: PackState,
    pub mesa: MesaState,
    pub last_error: String,
    /// Increases after every merge or unmerge.
    pub generation: u64,
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

impl CodecPackStatus {
    /// Parse the helper's status JSON. Anything unreadable is "not installed":
    /// the controls then offer Install, and the helper decides what is true.
    #[must_use]
    pub fn parse(json: &str) -> Self {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let state = match text(&value, "state").as_str() {
            "installing" => PackState::Installing,
            "installed" => PackState::Installed,
            "removing" => PackState::Removing,
            "failed" => PackState::Failed,
            _ => PackState::NotInstalled,
        };
        let mesa = match text(&value, "mesa").as_str() {
            "enabled" => MesaState::Enabled,
            "waiting" => MesaState::Waiting,
            "unsupported" => MesaState::Unsupported,
            _ => MesaState::Off,
        };
        Self {
            state,
            mesa,
            last_error: text(&value, "last_error"),
            generation: value
                .get("generation")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        }
    }

    /// The status on this machine, or "not installed" when there is none.
    #[must_use]
    pub fn read() -> Self {
        std::fs::read_to_string(STATUS_PATH)
            .map(|json| Self::parse(&json))
            .unwrap_or_default()
    }

    /// The one status line the card shows.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.state, self.mesa) {
            (PackState::NotInstalled, _) => "Not installed".into(),
            (PackState::Installing, _) => "Downloading and installing\u{2026}".into(),
            (PackState::Removing, _) => "Removing\u{2026}".into(),
            (PackState::Failed, _) if self.last_error.is_empty() => "Installation failed".into(),
            (PackState::Failed, _) => format!("Installation failed: {}", self.last_error),
            (PackState::Installed, MesaState::Enabled) => {
                "Installed, including hardware H.264/HEVC".into()
            }
            (PackState::Installed, MesaState::Waiting) => {
                "Installed. Hardware H.264/HEVC is waiting for a matching Mesa build from RPM Fusion."
                    .into()
            }
            (PackState::Installed, MesaState::Off | MesaState::Unsupported) => {
                "Installed (software codecs)".into()
            }
        }
    }

    /// A background refresh that failed while the pack kept working.
    #[must_use]
    pub fn refresh_problem(&self) -> Option<String> {
        (self.state == PackState::Installed && !self.last_error.is_empty()).then(|| {
            format!(
                "The last update of the codec pack failed: {}",
                self.last_error
            )
        })
    }

    /// The action the card offers, if any.
    #[must_use]
    pub const fn offered_action(&self) -> Option<CodecAction> {
        match self.state {
            PackState::NotInstalled | PackState::Failed => Some(CodecAction::Install),
            PackState::Installed => Some(CodecAction::Remove),
            PackState::Installing | PackState::Removing => None,
        }
    }
}

/// Run one fixed unit instance to completion.
///
/// # Errors
/// Returns the unit's failure reason when systemd, polkit or the helper refuse
/// or fail.
pub fn run(action: CodecAction) -> Result<(), String> {
    let unit = action.unit();
    let output = bounded_command::run(
        "systemctl",
        &["start", "--wait", unit],
        None,
        MAX_OUTPUT,
        OPERATION_TIMEOUT,
    )
    .map_err(|error| format!("systemctl command failed: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let status = CodecPackStatus::read();
    if !status.last_error.is_empty() {
        return Err(status.last_error);
    }
    let stderr: String = String::from_utf8_lossy(&output.stderr)
        .chars()
        .filter(|character| !character.is_control() || *character == ' ')
        .take(300)
        .collect();
    Err(if stderr.trim().is_empty() {
        format!("{unit} failed; see journalctl -u {unit}")
    } else {
        stderr.trim().to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_garbled_status_is_not_installed() {
        assert_eq!(CodecPackStatus::parse(""), CodecPackStatus::default());
        assert_eq!(
            CodecPackStatus::parse("[1,2]").state,
            PackState::NotInstalled
        );
        assert_eq!(
            CodecPackStatus::parse(r#"{"state":"exploded"}"#).offered_action(),
            Some(CodecAction::Install)
        );
    }

    #[test]
    fn installed_with_mesa_waiting() {
        let status = CodecPackStatus::parse(
            r#"{"schema":1,"state":"installed","mesa":"waiting","generation":4,"last_error":""}"#,
        );
        assert_eq!(status.state, PackState::Installed);
        assert_eq!(status.mesa, MesaState::Waiting);
        assert_eq!(status.generation, 4);
        assert!(
            status
                .summary()
                .contains("waiting for a matching Mesa build from RPM Fusion")
        );
        assert_eq!(status.offered_action(), Some(CodecAction::Remove));
        assert_eq!(status.refresh_problem(), None);
    }

    #[test]
    fn every_state_has_its_line_and_action() {
        let cases = [
            ("not-installed", "Not installed", Some(CodecAction::Install)),
            ("installing", "Downloading and installing\u{2026}", None),
            ("removing", "Removing\u{2026}", None),
            (
                "failed",
                "Installation failed: offline",
                Some(CodecAction::Install),
            ),
        ];
        for (state, line, action) in cases {
            let status =
                CodecPackStatus::parse(&format!(r#"{{"state":"{state}","last_error":"offline"}}"#));
            assert_eq!(status.summary(), line, "{state}");
            assert_eq!(status.offered_action(), action, "{state}");
        }
        let enabled = CodecPackStatus::parse(r#"{"state":"installed","mesa":"enabled"}"#);
        assert_eq!(
            enabled.summary(),
            "Installed, including hardware H.264/HEVC"
        );
    }

    #[test]
    fn failed_refresh_of_a_working_pack_is_reported_separately() {
        let status = CodecPackStatus::parse(
            r#"{"state":"installed","mesa":"enabled","last_error":"dnf5 failed: offline"}"#,
        );
        assert_eq!(status.offered_action(), Some(CodecAction::Remove));
        assert_eq!(
            status.refresh_problem().as_deref(),
            Some("The last update of the codec pack failed: dnf5 failed: offline")
        );
    }

    #[test]
    fn error_text_is_bounded_and_printable() {
        let long = "x".repeat(1000);
        let status = CodecPackStatus::parse(&format!(
            r#"{{"state":"failed","last_error":"a\u001b[31m{long}"}}"#
        ));
        assert!(status.last_error.len() <= 300);
        assert!(!status.last_error.contains('\u{1b}'));
    }

    #[test]
    fn only_the_two_polkit_allowed_units() {
        assert_eq!(
            CodecAction::Install.unit(),
            "tilo-codec-pack@install.service"
        );
        assert_eq!(CodecAction::Remove.unit(), "tilo-codec-pack@remove.service");
    }
}

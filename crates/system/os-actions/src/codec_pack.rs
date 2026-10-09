//! The media codecs extension, as the desktop sees it.
//!
//! A codec-specific view over the generic downloaded system extension
//! ([`crate::extension`]) with the catalogue id [`ID`]: its software part
//! plays and records more video and audio formats, and its hardware part
//! ([`MESA_PART`]) adds hardware H.264/HEVC. First-login setup, Settings, the
//! shell's notices and the installer hand-off share this module.

use std::path::Path;

use crate::extension::{self, ExtensionId, ExtensionStatus};
pub use crate::extension::{
    ExtensionAction as CodecAction, ExtensionState as PackState, PartState as MesaState, Progress,
};

/// The catalogue id of the media codecs extension.
pub const ID: &str = "media-codecs";
/// The hardware H.264/HEVC part (freeworld Mesa VA-API drivers).
pub const MESA_PART: &str = "tilo-codecs-mesa";

/// The media codecs extension's id.
#[must_use]
pub fn id() -> ExtensionId {
    ExtensionId::known(ID)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecPackStatus {
    pub state: PackState,
    /// The hardware part, which must match the base Mesa exactly.
    pub mesa: MesaState,
    /// Whether the owner's opt-in is recorded (it survives failures and
    /// offline periods until they remove the codecs).
    pub desired: bool,
    pub last_error: String,
    pub progress: Option<Progress>,
    /// Increases after every merge or unmerge.
    pub generation: u64,
}

impl From<ExtensionStatus> for CodecPackStatus {
    fn from(status: ExtensionStatus) -> Self {
        Self {
            state: status.state,
            mesa: status.part(MESA_PART),
            desired: status.desired,
            last_error: status.last_error,
            progress: status.progress,
            generation: status.generation,
        }
    }
}

impl CodecPackStatus {
    /// Parse the helper's status JSON for the codecs extension.
    #[must_use]
    pub fn parse(json: &str) -> Self {
        ExtensionStatus::parse(json).into()
    }

    /// The status on this machine, or "not installed" when there is none.
    #[must_use]
    pub fn read() -> Self {
        ExtensionStatus::read(&id()).into()
    }

    #[must_use]
    pub fn read_from(path: &Path) -> Self {
        ExtensionStatus::read_from(path).into()
    }

    /// Whether the owner has already made the choice to install: first-login
    /// setup does not ask again (the installer may have recorded it).
    #[must_use]
    pub fn opted_in(&self) -> bool {
        self.desired
            || matches!(
                self.state,
                PackState::WaitingForNetwork | PackState::Installing | PackState::Installed
            )
    }

    /// The one status line the controls show.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.state, self.mesa) {
            (PackState::NotInstalled, _) => "Off".into(),
            (PackState::WaitingForNetwork, _) => {
                "Waiting for an internet connection. It turns on by itself once this device is online."
                    .into()
            }
            (PackState::Installing, _) => match &self.progress {
                Some(progress) if !progress.label.is_empty() => match progress.megabytes() {
                    Some((done, total)) => format!(
                        "{} {done} of {total} MB (step {} of {})\u{2026}",
                        progress.label, progress.step, progress.steps
                    ),
                    None => format!(
                        "{} (step {} of {})\u{2026}",
                        progress.label, progress.step, progress.steps
                    ),
                },
                _ => "Downloading\u{2026}".into(),
            },
            (PackState::Removing, _) => "Removing\u{2026}".into(),
            (PackState::Failed, _) if self.last_error.is_empty() => "Could not install".into(),
            (PackState::Failed, _) => format!("Could not install: {}", self.last_error),
            (PackState::Installed, MesaState::Enabled) => {
                "On, including hardware H.264/HEVC".into()
            }
            (PackState::Installed, MesaState::Waiting) => {
                "On. Hardware H.264/HEVC follows once a matching driver is published.".into()
            }
            (PackState::Installed, MesaState::Off | MesaState::Unsupported) => "On".into(),
        }
    }

    /// A background refresh that failed while the codecs kept working.
    #[must_use]
    pub fn refresh_problem(&self) -> Option<String> {
        (self.state == PackState::Installed && !self.last_error.is_empty())
            .then(|| format!("The last update of the formats failed: {}", self.last_error))
    }

    /// When the new formats take effect, stated exactly. `None` while nothing
    /// is installed.
    #[must_use]
    pub fn when_it_applies(&self) -> Option<&'static str> {
        if self.state != PackState::Installed {
            return None;
        }
        Some(match self.mesa {
            MesaState::Enabled => {
                "Apps you open from now on play and record H.264 and HEVC with hardware \
                 acceleration. Sign out and back in to use it for screen sharing and recording, \
                 and reopen apps that were already open."
            }
            MesaState::Waiting | MesaState::Off | MesaState::Unsupported => {
                "Works right away: calls, screen sharing and videos pick it up the next time they \
                 start. No restart is needed."
            }
        })
    }

    /// The actions the controls offer, primary first.
    #[must_use]
    pub fn offered_actions(&self) -> Vec<CodecAction> {
        match self.state {
            PackState::Installing | PackState::Removing => Vec::new(),
            PackState::Installed if self.mesa == MesaState::Waiting => {
                vec![CodecAction::Refresh, CodecAction::Remove]
            }
            PackState::WaitingForNetwork | PackState::Installed => vec![CodecAction::Remove],
            PackState::Failed if self.desired => vec![CodecAction::Install, CodecAction::Remove],
            PackState::NotInstalled | PackState::Failed => vec![CodecAction::Install],
        }
    }

    /// The button label for `action` in this state.
    #[must_use]
    pub fn action_label(&self, action: CodecAction) -> &'static str {
        match (action, self.state) {
            (CodecAction::Install, PackState::Failed) => "Try again",
            (CodecAction::Install, _) => "Install",
            (CodecAction::Remove, PackState::WaitingForNetwork) => "Cancel",
            (CodecAction::Remove, _) => "Remove",
            (CodecAction::Refresh, _) => "Check for hardware support again",
        }
    }
}

/// Whether this system can install the codecs at all.
#[must_use]
pub fn available() -> bool {
    extension::available(&id())
}

/// Start the codecs extension's unit for `action` and wait until it finishes.
///
/// # Errors
/// As [`extension::run`].
pub fn run(action: CodecAction) -> Result<(), String> {
    extension::run(action, &id())
}

/// Call `on_change` whenever the helper rewrites the codecs status.
///
/// # Errors
/// As [`extension::watch`].
pub fn watch(on_change: impl FnMut() -> bool + Send + 'static) -> Result<(), String> {
    extension::watch(&id(), on_change)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesa_state_derives_from_the_hardware_part() {
        let status = CodecPackStatus::parse(
            r#"{"schema":1,"id":"media-codecs","state":"installed","desired":true,"generation":4,
                "parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"}}"#,
        );
        assert_eq!(status.state, PackState::Installed);
        assert_eq!(status.mesa, MesaState::Waiting);
        assert_eq!(status.generation, 4);
        assert_eq!(
            CodecPackStatus::parse(r#"{"state":"installed","mesa":"enabled"}"#).mesa,
            MesaState::Off,
            "only the parts map decides"
        );
        assert_eq!(
            CodecPackStatus::parse(
                r#"{"state":"installed","parts":{"tilo-codecs-mesa":"unsupported"}}"#
            )
            .mesa,
            MesaState::Unsupported
        );
    }

    #[test]
    fn installed_with_mesa_waiting_offers_a_recheck() {
        let status = CodecPackStatus::parse(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
        );
        assert!(status.summary().contains("follows once a matching driver"));
        assert_eq!(
            status.offered_actions(),
            vec![CodecAction::Refresh, CodecAction::Remove]
        );
        assert_eq!(status.refresh_problem(), None);
        assert!(status.when_it_applies().unwrap().contains("No restart"));
    }

    #[test]
    fn every_state_has_its_line_and_actions() {
        let cases = [
            ("not-installed", false, "Off", vec![CodecAction::Install]),
            (
                "waiting-for-network",
                true,
                "Waiting for an internet connection. It turns on by itself once this device is online.",
                vec![CodecAction::Remove],
            ),
            ("installing", true, "Downloading\u{2026}", vec![]),
            ("removing", false, "Removing\u{2026}", vec![]),
            (
                "failed",
                true,
                "Could not install: offline",
                vec![CodecAction::Install, CodecAction::Remove],
            ),
            (
                "failed",
                false,
                "Could not install: offline",
                vec![CodecAction::Install],
            ),
        ];
        for (state, desired, line, actions) in cases {
            let status = CodecPackStatus::parse(&format!(
                r#"{{"state":"{state}","last_error":"offline","desired":{desired}}}"#
            ));
            assert_eq!(status.summary(), line, "{state}");
            assert_eq!(status.offered_actions(), actions, "{state}");
        }
        let enabled = CodecPackStatus::parse(
            r#"{"state":"installed","parts":{"tilo-codecs-mesa":"enabled"}}"#,
        );
        assert_eq!(enabled.summary(), "On, including hardware H.264/HEVC");
        assert!(
            enabled
                .when_it_applies()
                .unwrap()
                .contains("Sign out and back in")
        );
        assert_eq!(
            CodecPackStatus::parse(r#"{"state":"failed"}"#).action_label(CodecAction::Install),
            "Try again"
        );
    }

    #[test]
    fn progress_label_leads_the_line_while_installing() {
        let installing = CodecPackStatus::parse(
            r#"{"state":"installing","progress":{"step":3,"steps":6,"label":"Downloading"}}"#,
        );
        assert_eq!(installing.summary(), "Downloading (step 3 of 6)\u{2026}");
        let bytes = CodecPackStatus::parse(
            r#"{"state":"installing","progress":{"step":3,"steps":5,"label":"Downloading","bytes":12500000,"total":40000000}}"#,
        );
        assert_eq!(
            bytes.summary(),
            "Downloading 12 of 40 MB (step 3 of 5)\u{2026}"
        );
    }

    #[test]
    fn failed_refresh_of_working_codecs_is_reported_separately() {
        let status = CodecPackStatus::parse(
            r#"{"state":"installed","parts":{"tilo-codecs-mesa":"enabled"},"last_error":"offline"}"#,
        );
        assert_eq!(status.offered_actions(), vec![CodecAction::Remove]);
        assert_eq!(
            status.refresh_problem().as_deref(),
            Some("The last update of the formats failed: offline")
        );
    }

    #[test]
    fn the_codecs_use_their_own_units_and_status() {
        assert_eq!(
            CodecAction::Install.unit(&id()),
            "tilo-extension@install:media-codecs.service"
        );
        assert_eq!(
            id().status_path(),
            Path::new("/var/lib/tilo/extensions/media-codecs.json")
        );
    }
}

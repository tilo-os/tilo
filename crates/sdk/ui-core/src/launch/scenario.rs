//! Which scenario a launch is built from, and the refusal of a capture of
//! what the machine it runs on holds.
//!
//! An app that shows the machine's own state (its journal, its processes,
//! its drives, the person's real files) declares so ([`LaunchSpec::reads_host`]).
//! A headless drive that captures such an app must name one of its
//! scenarios: the app is then built from that scenario's fixtures, never
//! from the machine, so no picture of it shows the journal, the home folder
//! or the source tree of whoever took it. `--shots` is always built from
//! fixtures (`None`, the plain capture, is the app's own default fixture).

use super::{LaunchArgs, LaunchError, LaunchSpec};

/// What of the machine it runs on an app shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSource {
    /// The system journal.
    Journal,
    /// The person's files, and the folder the app was started in.
    Files,
    /// The running processes and programs.
    Processes,
    /// Drives, partitions and filesystems.
    Disks,
    /// The container engine.
    Containers,
    /// The network: connections, tunnels and the firewall.
    Network,
}

impl HostSource {
    /// What the app shows, as the refusal names it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Journal => "the system journal",
            Self::Files => "the files",
            Self::Processes => "the processes",
            Self::Disks => "the drives",
            Self::Containers => "the containers",
            Self::Network => "the network",
        }
    }
}

impl LaunchSpec {
    /// The app shows `sources` of the machine it runs on: a headless drive
    /// that captures it must name a scenario, and is then built from that
    /// scenario's fixtures.
    #[must_use]
    pub fn reads_host(mut self, sources: &'static [HostSource]) -> Self {
        self.host_sources = sources;
        self
    }
}

/// Refuse an undeclared scenario, and a capture of a host-reading app that
/// names none.
pub(super) fn check(args: &LaunchArgs, spec: &LaunchSpec) -> Result<(), LaunchError> {
    if let Some(scenario) = args.scenario.as_deref()
        && !spec.scenarios.contains(&scenario)
    {
        return Err(LaunchError::Scenario(unknown_scenario(
            scenario,
            spec.scenarios,
        )));
    }
    let captures = args
        .headless
        .as_ref()
        .is_some_and(|drive| !drive.captures.is_empty());
    if captures && args.scenario.is_none() && !spec.host_sources.is_empty() {
        return Err(LaunchError::Scenario(live_capture(spec)));
    }
    Ok(())
}

/// The refusal of a drive capturing a host-reading app with no scenario,
/// naming the app, what it would show and the scenarios to choose from.
fn live_capture(spec: &LaunchSpec) -> String {
    let shows: Vec<&str> = spec
        .host_sources
        .iter()
        .map(|source| source.describe())
        .collect();
    let choose = if spec.scenarios.is_empty() {
        "it declares none yet".to_owned()
    } else {
        format!("one of {}", quoted(spec.scenarios))
    };
    format!(
        "{} shows {} of this machine, so a drive that captures it must name a scenario \
         (\"scenario\" in the drive, or --scenario): {choose}",
        spec.app_id,
        shows.join(", ")
    )
}

/// The refusal of an undeclared `--scenario`, listing the declared ones.
fn unknown_scenario(name: &str, declared: &[&str]) -> String {
    if declared.is_empty() {
        format!("unknown screenshot scenario {name:?}; this app declares no scenarios")
    } else {
        format!(
            "unknown screenshot scenario {name:?}; expected one of {}",
            quoted(declared)
        )
    }
}

fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::super::prepared_launch_tests::prepare;
    use super::super::{DefinitionDispatch, dispatch_definition, parse_args_extended};
    use super::*;
    use crate::application_test_support::*;

    fn journal_app() -> LaunchSpec {
        LaunchSpec::from_definition(runner_probe_definition())
            .component_host(prepare)
            .allowed_live_options(&["--fixture-root"])
            .scenarios(&["sample"])
            .reads_host(&[HostSource::Journal])
    }

    fn args(
        spec: &LaunchSpec,
        root: &std::path::Path,
        drive: &serde_json::Value,
    ) -> Result<LaunchArgs, LaunchError> {
        let values: Vec<String> = vec![
            "app".into(),
            "--fixture-root".into(),
            root.join("state").to_string_lossy().into_owned(),
            "--headless-drive".into(),
            drive.to_string(),
        ];
        parse_args_extended(
            &values,
            spec.allowed_flags,
            spec.allowed_options,
            spec.allowed_live_flags,
            spec.allowed_live_options,
            spec.max_positionals,
        )
    }

    /// A drive that would picture an app showing the machine's journal with
    /// no scenario is refused before the app is built, naming the app and
    /// the scenarios to choose from; naming one builds it from that
    /// scenario's fixtures, and the capture is written.
    #[test]
    fn a_host_reading_app_renders_only_from_a_scenario() {
        let temp = tempfile::tempdir().unwrap();
        let spec = journal_app();
        let shot = temp.path().join("shot.png");
        let shot_path = shot.to_string_lossy().into_owned();

        let live = args(
            &spec,
            temp.path(),
            &serde_json::json!({ "capture": { "path": shot_path } }),
        )
        .expect("the arguments parse");
        let before = calls();
        let refusal = check(&live, &spec).expect_err("a live capture is refused");
        let message = refusal.to_string();
        assert!(message.contains(spec.app_id), "names the app: {message}");
        assert!(
            message.contains("the system journal") && message.contains("scenario"),
            "says what it would show and what is missing: {message}"
        );
        assert!(
            message.contains("\"sample\""),
            "lists the scenarios: {message}"
        );
        assert_eq!(calls(), before, "nothing of the app was built");

        let uncaptured =
            args(&spec, temp.path(), &serde_json::json!({})).expect("the arguments parse");
        assert!(
            check(&uncaptured, &spec).is_ok(),
            "a drive that pictures nothing is not a render"
        );

        let staged = args(
            &spec,
            temp.path(),
            &serde_json::json!({ "scenario": "sample", "capture": { "path": shot_path } }),
        )
        .expect("the arguments parse");
        check(&staged, &spec).expect("a drive of a scenario is allowed");
        assert_eq!(staged.scenario.as_deref(), Some("sample"));
        let dispatched = dispatch_definition(&spec, &staged).expect("the drive runs");
        assert!(
            matches!(dispatched, DefinitionDispatch::Headless(0)),
            "the drive of the scenario succeeds"
        );
        assert!(shot.is_file(), "and its capture is written");
    }
}

//! Which launches render at the capture instant
//! ([`crate::clock::freeze_for_capture`]).

use super::LaunchArgs;

/// Whether this launch renders at the capture instant: a capture, and a
/// drive that names a scenario, whose app is built and staged as that
/// capture.
pub(super) fn pins_the_clock(args: &LaunchArgs) -> bool {
    args.shots.is_some()
        || args.responsive_matrix.is_some()
        || (args.headless.is_some() && args.scenario.is_some())
}

#[cfg(test)]
mod tests {
    use super::pins_the_clock;
    use crate::launch::parse_args;

    fn launch(args: &[&str]) -> crate::launch::LaunchArgs {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        parse_args(&args, &[], &[]).expect("the arguments parse")
    }

    /// A scenario drive renders at the capture instant, as its capture
    /// does; a drive of the live app reads the real clock.
    #[test]
    fn a_scenario_drive_renders_at_the_capture_instant() {
        assert!(pins_the_clock(&launch(&[
            "app",
            "--headless-drive",
            r#"{"scenario":"wide"}"#
        ])));
        assert!(pins_the_clock(&launch(&["app", "--shots", "/tmp/a.png"])));
        assert!(!pins_the_clock(&launch(&[
            "app",
            "--headless-drive",
            r#"{"actions":[]}"#
        ])));
    }
}

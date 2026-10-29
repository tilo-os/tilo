//! The native fallback: what a launcher passes through the process
//! environment. It is the one place host inputs are read from the process.

/// The host inputs a native launcher passes through the process environment.
pub fn process_environment() -> super::HostEnvironment {
    /// Empty is unset — an explicit `""` is the shape an unset forwarded
    /// shell variable produces, and taking it literally would put every such
    /// app in the same (empty-id) room.
    fn text(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }
    fn path(name: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from)
    }
    /// Non-empty, and not `"0"`/`"false"` (case-insensitive).
    fn flag(names: &[&str]) -> bool {
        names.iter().any(|name| {
            std::env::var(name).is_ok_and(|value| {
                let value = value.trim();
                !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
            })
        })
    }
    /// A finite, positive factor; a bad value is ignored rather than
    /// crashing the app at startup.
    fn factor(names: &[&str]) -> Option<f32> {
        names.iter().find_map(|name| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.trim().parse::<f32>().ok())
                .filter(|factor| factor.is_finite() && *factor > 0.0)
        })
    }
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let ui_set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    let continuity_resume = text("TILO_CONTINUITY_RESUME_HEX")
        .and_then(|hex| super::decode_continuation(&hex, &text("TILO_CONTINUITY_RESUME_SCHEMA")?));
    super::HostEnvironment {
        identity: super::HostIdentity {
            package_id: text("TILO_PACKAGE_ID"),
            development_app_id: text("QUICK_APP_ID"),
            app_name: text("TILO_APP_NAME"),
            workspace_id: text("TILO_WORKSPACE_ID"),
            user_id: text("TILO_USER_ID"),
            presence_handle: text("TILO_PRESENCE_HANDLE"),
            account_name: text("USER"),
            continuity_resume,
            continuity_instance: text("TILO_CONTINUITY_INSTANCE"),
            collab_service_configured: std::env::var_os("TILO_SVC_COLLAB_FD").is_some(),
            managed_agent_context: std::env::var_os("TILO_MANAGED_AGENT_CONTEXT").is_some(),
        },
        paths: super::HostPaths {
            user_home: path("TILO_USER_HOME"),
            home: path("HOME"),
            config_home: path("XDG_CONFIG_HOME"),
            cache_home: path("XDG_CACHE_HOME"),
            pipeline_seed: path("TILO_PIPELINE_SEED_DIR"),
            font_index: path("TILO_FONT_INDEX"),
            font_catalog: path("TILO_FONT_CATALOG"),
            font_artifact_cache: path("TILO_FONT_ASSET_CACHE"),
            public_identity_record: path("TILO_USER_IDENTITY"),
        },
        accessibility: super::AccessibilityInputs {
            screen_reader: flag(&["TILO_A11Y_SCREEN_READER", "TILO_SCREEN_READER"]),
            high_contrast: flag(&["TILO_A11Y_HIGH_CONTRAST", "TILO_HIGH_CONTRAST"]),
            reduce_motion: flag(&["TILO_A11Y_REDUCE_MOTION", "TILO_REDUCE_MOTION"]),
            text_scale: factor(&["TILO_A11Y_TEXT_SCALE", "TILO_TEXT_SCALE"]),
            // The shell mirrors the user's UI-scale preference into spawned
            // sandboxes so an app comes up already scaled.
            ui_scale: factor(&["TILO_UI_SCALE", "TILO_A11Y_UI_SCALE"]),
        },
        locale: super::locale_from_vars(|name| std::env::var(name).ok()),
        desktop: text("XDG_CURRENT_DESKTOP"),
        diagnostics: super::RunnerDiagnostics {
            debug_overlay: std::env::var_os("TILO_UI_DEBUG").is_some(),
            perf_log: ui_set("TILO_UI_PERF"),
            disable_scroll_fast_path: ui_set("TILO_UI_NO_SCROLL_OPT"),
            gpu_timestamps: std::env::var("VELLO_GPU_TIMESTAMPS").is_ok_and(|v| v == "1"),
            gpu_dispatch_timestamps: std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS")
                .is_ok_and(|v| v == "1"),
            profile_hud: set("TILO_PROFILE"),
            profile_out: path("TILO_PROFILE_OUT"),
            presentation_profile: presentation_profile_requested(
                std::env::var("TILO_PROFILE_PRESENTATION").ok().as_deref(),
                std::env::var("TILO_PROFILE_LOCAL_INPUT")
                    .ok()
                    .as_deref()
                    .is_some_and(|value| value == "1"),
                std::env::var_os("TILO_NATIVE_SCENARIO").as_deref(),
            ),
            presentation_timing: presentation_timing_requested(
                std::env::var("TILO_PROFILE_PRESENTATION").ok().as_deref(),
            ),
            window_no_activate: std::env::var("TILO_PROFILE_WINDOW_NO_ACTIVATE").as_deref()
                == Ok("1"),
            host_perf_baseline: ["TILO_HOST_PERF_BASELINE", "TILO_HOST_PERF2_BASELINE"]
                .iter()
                .any(|name| std::env::var_os(name).is_some_and(|v| v == "1")),
            hosted_pump_timing: std::env::var_os("TILO_HOSTED_PUMP_TIMING")
                .is_some_and(|v| v == "1"),
        },
        dev_session: path("TILO_DEV_SESSION"),
        ui_graph_dir: path("TILO_UI_GRAPH_DIR"),
    }
}

/// Ordinary timing diagnostics require explicit opt-in without a synthetic scenario.
fn presentation_timing_requested(profile: Option<&str>) -> bool {
    profile == Some("1")
}

/// Presentation profiling needs an explicit `profile == "1"` and a record to
/// profile: a native scenario, or a local-input run that has no synthetic
/// input at all and measures the person's own typing. The two are exclusive —
/// a local-input run rejects a scenario — so a run that asks for both gets
/// nothing rather than a measurement of the wrong thing. Whether this platform
/// can profile presentation at all is the renderer's decision.
fn presentation_profile_requested(
    profile: Option<&str>,
    local_input: bool,
    scenario: Option<&std::ffi::OsStr>,
) -> bool {
    let scenario_present = scenario.is_some_and(|path| !path.is_empty());
    // Any scenario value at all, including an empty one, is a conflict with
    // local input: the native scenario loader takes an empty one literally and
    // refuses the launch.
    profile == Some("1")
        && !(local_input && scenario.is_some())
        && (scenario_present || local_input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_presentation_timing_requires_explicit_host_diagnostics() {
        assert!(presentation_timing_requested(Some("1")));
        for value in [None, Some(""), Some("0"), Some("true")] {
            assert!(!presentation_timing_requested(value));
        }
    }

    #[test]
    fn presentation_profile_requires_a_native_scenario_and_explicit_feedback() {
        use std::ffi::OsStr;
        assert!(presentation_profile_requested(
            Some("1"),
            false,
            Some(OsStr::new("scenario.json"))
        ));
        for (profile, scenario) in [
            (None, Some(OsStr::new("scenario.json"))),
            (Some("0"), Some(OsStr::new("scenario.json"))),
            (Some("1"), None),
            (Some("1"), Some(OsStr::new(""))),
        ] {
            assert!(!presentation_profile_requested(profile, false, scenario));
        }
    }

    #[test]
    fn local_input_run_profiles_presentation_without_a_scenario() {
        use std::ffi::OsStr;
        // A launch that measures the person's own typing has no scenario to
        // name, and its feedback must still be enabled.
        assert!(presentation_profile_requested(Some("1"), true, None));
        // Neither opt-in alone enables the profile.
        assert!(!presentation_profile_requested(Some("1"), false, None));
        assert!(!presentation_profile_requested(None, true, None));
        assert!(!presentation_profile_requested(Some("0"), true, None));
        // A named scenario alongside local input is refused, as the local-input
        // observer refuses it: rather than profile the wrong input, enable
        // nothing.
        assert!(!presentation_profile_requested(
            Some("1"),
            true,
            Some(OsStr::new("scenario.json"))
        ));
    }
}
